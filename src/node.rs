//! Node.js backend (JavaScript and TypeScript): V8 CPU profile, call recording through a temporary
//! block appended to the module, a napi-rs crate, and a reversible shim that rebinds the function.

use crate::backend::{self, Profile, Recording};
use crate::hotspot::Hotspot;
use crate::python::DISABLE_ENV;
use crate::state::{Ctx, Port, Verify};
use crate::util::{combined, forward_slashes, output, relative, slug, tail};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const RUNNER: &str = include_str!("node/runner.mjs");
const RECORDER: &str = include_str!("node/recorder.cjs");
const STOP: &str = include_str!("node/stop.cjs");

pub fn is_node(program: &str) -> bool {
    Runtime::of(program).is_some()
}

/// The JavaScript runtime the app runs on. Bun and Deno run the same ports: they load Node-API
/// addons, and write the same .cpuprofile files as Node with --cpu-prof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Runtime {
    Node,
    Bun,
    Deno,
}

impl Runtime {
    pub fn of(program: &str) -> Option<Runtime> {
        match crate::util::program_stem(program).as_str() {
            "node" | "nodejs" => Some(Runtime::Node),
            "bun" => Some(Runtime::Bun),
            "deno" => Some(Runtime::Deno),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Runtime::Node => "node",
            Runtime::Bun => "bun",
            Runtime::Deno => "deno",
        }
    }

    /// The app's own arguments: without the "run" of  bun run app.ts  or  deno run -A app.ts.
    fn app_args(self, args: &[String]) -> &[String] {
        match args.first() {
            Some(a) if a == "run" && self != Runtime::Node => &args[1..],
            _ => args,
        }
    }

    /// Arguments that run the app with fast-anything's options first.
    fn with(self, options: &[&str], args: &[String]) -> Vec<String> {
        let mut out: Vec<String> = if self == Runtime::Deno { vec!["run".into()] } else { vec![] };
        out.extend(options.iter().map(|o| o.to_string()));
        out.extend(self.app_args(args).iter().cloned());
        out
    }

    /// Arguments that run a script of fast-anything's (it needs files, env vars and the addon).
    fn script(self, script: &Path) -> Vec<String> {
        let mut out: Vec<String> = if self == Runtime::Deno { vec!["run".into(), "-A".into()] } else { vec![] };
        out.push(script.to_string_lossy().into_owned());
        out
    }
}

/// The runtime behind a resolved executable (node unless it's bun or deno).
fn runtime_of(exe: &str) -> Runtime {
    Runtime::of(exe).unwrap_or(Runtime::Node)
}

pub fn resolve(program: &str, root: &Path) -> Result<String> {
    let prog = if program.contains(['/', '\\']) { root.join(program) } else { PathBuf::from(program) };
    let runtime = Runtime::of(program).unwrap_or(Runtime::Node);
    let (args, minimum): (&[&str], u32) = match runtime {
        Runtime::Node => (&["-p", "process.execPath + '\\n' + process.versions.node"], 22),
        Runtime::Bun => (&["-p", "process.execPath + '\\n' + Bun.version"], 1),
        Runtime::Deno => (&["eval", "-p", "Deno.execPath() + '\\n' + Deno.version.deno"], 2),
    };
    let out = output(Command::new(&prog).args(args).current_dir(root), runtime.name())?;
    ensure!(out.status.success(), "{program} failed:\n{}", tail(&combined(&out), 5));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut lines = text.lines();
    let exe = lines.next().unwrap_or_default().trim().to_string();
    let major: u32 = lines.next().unwrap_or_default().split('.').next().and_then(|m| m.parse().ok()).unwrap_or(0);
    let label = match runtime {
        Runtime::Node => "Node.js",
        Runtime::Bun => "Bun",
        Runtime::Deno => "Deno",
    };
    ensure!(major >= minimum, "{label} {minimum} or newer is needed (found {major})");
    Ok(exe)
}

/// The script in "node [flags] script.js args" (or "bun run ...", "deno run ...").
fn entry(args: &[String]) -> Option<&String> {
    let args = if args.first().is_some_and(|a| a == "run") { &args[1..] } else { args };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if matches!(
            a.as_str(),
            "-r" | "--require" | "--import" | "--loader" | "--experimental-loader" | "-C" | "--conditions"
        ) {
            it.next();
        } else if !a.starts_with('-') {
            return Some(a);
        }
    }
    None
}

fn runner(ctx: &Ctx, node: &str, mode: &str, job: Value) -> Result<Output> {
    let script = ctx.fast.join("runner.mjs");
    fs::write(&script, RUNNER)?;
    let job_path = ctx.fast.join(format!("job-{mode}.json"));
    fs::write(&job_path, serde_json::to_vec_pretty(&job)?)?;
    let runtime = runtime_of(node);
    let out = output(
        Command::new(node)
            .args(runtime.script(&script))
            .arg(mode)
            .arg(&job_path)
            .current_dir(&ctx.root)
            .stdin(Stdio::null()),
        runtime.name(),
    )?;
    if !out.status.success() {
        bail!("the {} {mode} step failed ({}):\n{}", runtime.name(), out.status, tail(&combined(&out), 30));
    }
    Ok(out)
}

/// `duration`: end the app after that many seconds (servers and GUIs that run until closed).
pub fn profile(ctx: &Ctx, node: &str, args: &[String], duration: Option<f64>) -> Result<Profile> {
    let script = entry(args).context("name the script to run, e.g.  fast-anything scan -- node app.js")?;
    let dir = ctx.fast.join("cpuprof");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir)?;
    let runtime = runtime_of(node);
    let dir_text = dir.to_string_lossy().into_owned();
    let stop = ctx.fast.join("stop.cjs");
    let stop_text = stop.to_string_lossy().into_owned();
    let mut options = vec!["--cpu-prof", "--cpu-prof-interval", "250", "--cpu-prof-dir", &dir_text];
    let mut cmd = Command::new(node);
    if let Some(d) = duration {
        // the app ends itself (process.exit), so the profile is written as on a normal exit
        fs::write(&stop, STOP)?;
        options.extend([if runtime == Runtime::Node { "--require" } else { "--preload" }, &stop_text]);
        cmd.env("FAST_ANYTHING_STOP_AFTER_MS", ((d * 1000.0) as u64).to_string());
    }
    let (out, killed) = crate::util::output_for(
        cmd.args(runtime.with(&options, args)).current_dir(&ctx.root).stdin(Stdio::null()),
        runtime.name(),
        duration.map(|d| Duration::from_secs_f64(d + 15.0)),
        Duration::from_secs(10),
    )?;
    ensure!(!killed, "the app was still running 15s after --duration: does it call process.exit?");
    ensure!(out.status.success(), "the app failed ({}):\n{}", out.status, tail(&combined(&out), 30));
    let result = ctx.fast.join("profile.json");
    let job = json!({ "root": ctx.root, "dir": dir, "entry": ctx.root.join(script), "out": result });
    runner(ctx, node, "profile", job)?;
    serde_json::from_str(&fs::read_to_string(&result)?).context("reading the profile")
}

/// The entry script's main block, if its main code sits under one.
const MAIN_GUARD: &[&str] = &["if(import.meta.main)", "if(require.main===module)", "if(module===require.main)"];

/// Add a marked block to a module: at the end, or in the entry script just before its main block,
/// so the app's main code already sees it. Function declarations are hoisted, so they exist there.
fn add_marked(text: &str, id: &str, body: &str, entry: bool) -> String {
    if entry {
        backend::insert_marked(text, "//", id, body, |t| backend::line_starting(t, MAIN_GUARD))
    } else {
        backend::append_marked(text, "//", id, body)
    }
}

/// Re-run the app with a recorder wrapped around the function. The module gets a temporary
/// block appended (a copy waits in .fast/backup) and is restored byte for byte afterwards.
pub fn record(ctx: &Ctx, node: &str, args: &[String], hs: &Hotspot, out: &Path, limit: usize) -> Result<Recording> {
    let file = ctx.root.join(&hs.file);
    let original = fs::read_to_string(&file).with_context(|| format!("reading {}", hs.file))?;
    let backup_dir = ctx.fast.join("backup");
    fs::create_dir_all(&backup_dir)?;
    let backup = backup_dir.join(slug(&hs.file));
    fs::write(&backup, &original)?;

    let runtime = runtime_of(node);
    let f = &target(&hs.name, hs.owner.as_deref());
    let wrap = record_wrap(f, is_esm_file(&ctx.root, &file, runtime));
    let entry = hs.module.as_deref() == Some("__main__");
    let reassignable = redeclare(&original, "const", "let", f).unwrap_or_else(|| original.clone());
    fs::write(&file, add_marked(&reassignable, &format!("recording {}", hs.id), &wrap, entry))?;
    let recorder = ctx.fast.join("recorder.cjs");
    fs::write(&recorder, RECORDER)?;
    let meta = out.with_extension("meta.json");
    let recorder_text = recorder.to_string_lossy().into_owned();
    let mut options: Vec<&str> = match runtime {
        Runtime::Node => vec!["--require", &recorder_text],
        Runtime::Bun | Runtime::Deno => vec!["--preload", &recorder_text],
    };
    if runtime == Runtime::Deno {
        // the recorder reads its settings from env vars and writes its results to files; Deno
        // rejects a permission given twice, or next to -A
        let given = |flag: &str| args.iter().any(|a| a == "-A" || a == "--allow-all" || a.starts_with(flag));
        options.extend(["--allow-env", "--allow-read", "--allow-write"].into_iter().filter(|f| !given(f)));
    }
    let run = output(
        Command::new(node)
            .args(runtime.with(&options, args))
            .current_dir(&ctx.root)
            .env("FAST_ANYTHING_RECORD_OUT", out)
            .env("FAST_ANYTHING_RECORD_META", &meta)
            .env("FAST_ANYTHING_RECORD_LIMIT", limit.to_string())
            .env(DISABLE_ENV, "1")
            .stdin(Stdio::null()),
        runtime.name(),
    );
    fs::write(&file, &original)
        .with_context(|| format!("restoring {} (a copy is in {})", hs.file, backup.display()))?;
    let _ = fs::remove_file(&backup);
    let run = run?;
    ensure!(run.status.success(), "the app failed while recording ({}):\n{}", run.status, tail(&combined(&run), 30));
    serde_json::from_str(&fs::read_to_string(&meta)?).context("reading the recording summary")
}

// ------------------------------------------------------------------ the napi-rs crate

fn split_union(s: &str) -> Vec<String> {
    let (mut depth, mut cur, mut out) = (0i32, String::new(), vec![]);
    for c in s.chars() {
        match c {
            '<' | '[' | '(' | '{' => depth += 1,
            '>' | ']' | ')' | '}' => depth -= 1,
            '|' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur.trim().to_string());
    out.into_iter().filter(|p| !p.is_empty()).collect()
}

const TYPED_ARRAYS: &[&str] = &[
    "Float64Array",
    "Float32Array",
    "Int32Array",
    "Uint32Array",
    "Int16Array",
    "Uint16Array",
    "Int8Array",
    "Uint8Array",
    "Uint8ClampedArray",
];

/// Rust type for a TypeScript annotation, if there's an obvious one.
pub fn rust_type(annotation: &str) -> Option<String> {
    let a = annotation.split_whitespace().collect::<Vec<_>>().join(" ");
    let parts = split_union(&a);
    let solid: Vec<&String> = parts.iter().filter(|p| *p != "null" && *p != "undefined").collect();
    if solid.len() < parts.len() {
        return match solid.as_slice() {
            [one] => rust_type(one).map(|t| format!("Option<{t}>")),
            _ => None,
        };
    }
    if parts.len() != 1 {
        return None;
    }
    let a = a.trim();
    Some(match a {
        "number" => "f64".into(),
        "string" => "String".into(),
        "boolean" => "bool".into(),
        "void" | "undefined" => "()".into(),
        "Buffer" => "Buffer".into(),
        t if TYPED_ARRAYS.contains(&t) => t.into(),
        _ => {
            let inner = a
                .strip_suffix("[]")
                .map(|x| x.trim_start_matches("readonly ").to_string())
                .or_else(|| a.strip_prefix("Array<").and_then(|x| x.strip_suffix('>')).map(String::from))
                .or_else(|| a.strip_prefix("ReadonlyArray<").and_then(|x| x.strip_suffix('>')).map(String::from));
            let set_inner = a.strip_prefix("Set<").and_then(|x| x.strip_suffix('>'));
            let tuple = a
                .strip_prefix('[')
                .and_then(|x| x.strip_suffix(']'))
                .map(|x| x.split(',').map(|p| rust_type(p.trim())).collect::<Option<Vec<String>>>());
            if let Some(Some(parts)) = tuple {
                // A JS tuple is an array; a same-typed one maps to a Vec.
                if parts.is_empty() || parts.iter().any(|p| p != &parts[0]) {
                    return None;
                }
                format!("Vec<{}>", parts[0])
            } else if let Some(inner) = inner {
                format!("Vec<{}>", rust_type(&inner)?)
            } else if let Some(inner) = set_inner {
                // napi converts a JS Set into a HashSet, whose items must hash: strings yes, f64 no
                let inner = rust_type(inner.trim()).filter(|t| t == "String")?;
                format!("std::collections::HashSet<{inner}>")
            } else {
                // a JS Map would arrive empty (napi's HashMap reads a plain object's own keys), so
                // only Record<string, T> maps to one
                let v = a.strip_prefix("Record<string,").and_then(|x| x.strip_suffix('>'))?;
                format!("std::collections::HashMap<String, {}>", rust_type(v.trim())?)
            }
        }
    })
}

fn snake(name: &str) -> String {
    let mut s = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                s.push('_');
            }
            s.push(c.to_ascii_lowercase());
        } else if c == '$' {
            s.push('_');
        } else {
            s.push(c);
        }
    }
    match s.as_str() {
        "type" | "match" | "fn" | "ref" | "move" | "loop" | "mod" | "use" | "impl" | "trait" | "struct" | "enum"
        | "where" | "mut" | "self" | "super" | "crate" | "box" | "gen" | "dyn" | "unsafe" | "extern" | "pub" | "as"
        | "in" => {
            format!("r#{s}")
        }
        _ => s,
    }
}

pub fn lib_rs(hs: &Hotspot, port: &Port) -> String {
    let params: Vec<String> = hs
        .params
        .iter()
        .map(|p| {
            let ty = p.annotation.as_deref().and_then(rust_type).unwrap_or_else(|| "serde_json::Value".into());
            format!("{}: {ty}", snake(&p.name))
        })
        .collect();
    let ret = hs.returns.as_deref().and_then(rust_type).unwrap_or_else(|| "serde_json::Value".into());
    format!(
        "//! Rust port of {id} ({file}:{line}), generated by fast-anything.\n\
         //! Replace the todo!() with a faithful, fast port. TASK.md has the original and real calls.\n\
         //! Check it with:  fast-anything verify {id}\n\n\
         use napi::bindgen_prelude::*;\n\
         use napi_derive::napi;\n\n\
         #[napi(js_name = \"{js}\", catch_unwind)]\n\
         #[allow(unused_variables)]\n\
         pub fn {rust}({params}) -> Result<{ret}> {{\n    todo!(\"port {id}\")\n}}\n",
        id = port.id,
        file = hs.file,
        line = hs.line,
        js = hs.name,
        rust = snake(&hs.name),
        params = params.join(", "),
    )
}

fn cargo_toml(port: &Port) -> String {
    format!(
        "# Rust port of {id}, generated by fast-anything.\n\
         [package]\nname = \"{ext}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = false\n\n\
         [lib]\nname = \"{ext}\"\ncrate-type = [\"cdylib\"]\n\n\
         [dependencies]\nnapi = {{ version = \"3\", features = [\"serde-json\"] }}\nnapi-derive = \"3\"\nserde_json = \"1\"\n\n\
         [build-dependencies]\nnapi-build = \"2\"\n\n{profile}",
        id = port.id,
        ext = port.ext,
        profile = backend::RELEASE_PROFILE,
    )
}

/// A recorded value the way JavaScript would print it.
fn js_value(v: &Value) -> String {
    match v {
        Value::Array(items) => format!("[{}]", items.iter().map(js_value).collect::<Vec<_>>().join(", ")),
        Value::Object(map) => match map.iter().next() {
            Some((tag, Value::Array(pairs))) if tag == "$object" => {
                let inner: Vec<String> = pairs
                    .iter()
                    .filter_map(|p| p.as_array())
                    .filter(|p| p.len() == 2)
                    .map(|p| format!("{}: {}", p[0].as_str().unwrap_or("?"), js_value(&p[1])))
                    .collect();
                format!("{{ {} }}", inner.join(", "))
            }
            Some((tag, x)) if tag == "$typed" => format!("{}{}", x[0].as_str().unwrap_or("TypedArray"), x[1]),
            Some((tag, x)) if tag == "$bigint" => format!("{}n", x.as_str().unwrap_or("0")),
            Some((tag, _)) if tag == "$undefined" => "undefined".into(),
            Some((tag, x)) if tag == "$float" => x.as_str().unwrap_or("NaN").to_string(),
            Some((tag, Value::Array(items))) if tag == "$set" => {
                format!("Set {{{}}}", items.iter().map(js_value).collect::<Vec<_>>().join(", "))
            }
            Some((tag, Value::Array(pairs))) if tag == "$map" => {
                let inner: Vec<String> = pairs
                    .iter()
                    .filter_map(|p| p.as_array())
                    .filter(|p| p.len() == 2)
                    .map(|p| format!("{} => {}", js_value(&p[0]), js_value(&p[1])))
                    .collect();
                format!("Map {{ {} }}", inner.join(", "))
            }
            _ => v.to_string(),
        },
        _ => v.to_string(),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n - 1).collect::<String>()) }
}

fn sample_calls(path: &Path, func: &str, n: usize) -> Vec<String> {
    let Ok(text) = fs::read_to_string(path) else { return vec![] };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .take(n)
        .map(|case| {
            let args: Vec<String> =
                case["args"].as_array().map(|a| a.iter().map(js_value).collect()).unwrap_or_default();
            let outcome = match case.get("raises") {
                Some(e) => format!("throws {}", e.as_str().unwrap_or("an error")),
                None => js_value(&case["result"]),
            };
            format!("{}  ->  {}", clip(&format!("{func}({})", args.join(", ")), 110), clip(&outcome, 60))
        })
        .collect()
}

pub fn task_md(hs: &Hotspot, helpers: &[Hotspot], port: &Port, crate_dir: &Path, held_out: usize) -> String {
    let fence = "```";
    let lang =
        if hs.file.ends_with(".ts") || hs.file.ends_with(".mts") || hs.file.ends_with(".cts") { "ts" } else { "js" };
    let samples = sample_calls(&crate_dir.join("recordings.jsonl"), &hs.name, 8).join("\n");
    let notes: String = hs.warnings.iter().map(|w| format!("- Note: {w}.\n")).collect();
    format!(
        "# Port `{id}` to Rust\n\n\
You're in a Rust crate (napi-rs) fast-anything generated for this: `{dir}`.\n\
Write `{func}` in `src/lib.rs` so it behaves **exactly** like the JavaScript below, and runs fast.\n\
Only edit files in this folder. Don't touch the JavaScript project.\n\n\
## The original ({file}:{line})\n\n{fence}{lang}\n{source}\n{fence}\n\n\
{helpers}## Real calls\n\n\
fast-anything recorded {recorded} real calls (out of {seen} while the app ran). A few:\n\n{fence}\n{samples}\n{fence}\n\n{held}\
## Rules\n\n\
- Same result for every input, edge cases included: empty input, zero, negatives, NaN and infinity, non-ASCII text, very long input. verify tests these too.\n\
- JavaScript strings are UTF-16: `.length`, indexing and `charCodeAt` count UTF-16 code units. \
Use `s.encode_utf16()` for anything index- or length-based (bytes are fine after checking `is_ascii()`).\n\
- JavaScript numbers are f64. Integer math is exact only up to 2^53; use `f64` unless the values are clearly small integers.\n\
- Numbers must match within 1e-9 of their size (or of the largest value in the same array), and zeros keep their sign: -0 is not 0. Keep the same order of float operations.\n\
- `.sum()` of an empty float iterator is -0.0 in Rust: start float sums with `fold(0.0, |a, x| a + x)`.\n\
- Where JavaScript throws, return `Err(Error::from_reason(...))`.\n\
- Each call crosses from JavaScript into Rust: keep conversions cheap, don't clone big inputs.\n\
- Make it fast: no needless allocation, borrow, `with_capacity`, fast paths for common cases.\n\
- Keep `#[napi(js_name = \"{func}\", catch_unwind)]` as it is.\n\
{notes}\n\
## Check\n\n\
Run `cargo check` here. Then fast-anything builds the crate in release mode and replays all {total} recorded calls against it\n\
(`fast-anything verify {id}`).\n",
        id = port.id,
        dir = crate_dir.display(),
        func = hs.name,
        file = hs.file,
        line = hs.line,
        source = hs.source,
        recorded = port.recorded.saturating_sub(held_out),
        held = backend::held_out_note(held_out),
        total = port.recorded,
        seen = crate::util::thousands(port.calls_seen),
        helpers = helpers_md(hs, helpers),
    )
}

pub fn scaffold(ctx: &Ctx, hs: &Hotspot, helpers: &[Hotspot], port: &Port) -> Result<String> {
    let dir = ctx.crate_dir(port);
    fs::create_dir_all(dir.join("src"))?;
    fs::create_dir_all(dir.join(".cargo"))?;
    fs::write(dir.join("Cargo.toml"), cargo_toml(port))?;
    fs::write(dir.join("build.rs"), "fn main() {\n    napi_build::setup();\n}\n")?;
    fs::write(dir.join(".cargo/config.toml"), backend::CARGO_CONFIG)?;
    let lib = dir.join("src/lib.rs");
    if !lib.exists() {
        fs::write(&lib, lib_rs(hs, port))?;
    }
    let task = task_md(hs, helpers, port, &dir, ctx.held_out(port));
    fs::write(dir.join("TASK.md"), &task)?;
    Ok(task)
}

/// TASK.md section listing the same-file functions the target calls, to port alongside it.
pub fn helpers_md(hs: &Hotspot, helpers: &[Hotspot]) -> String {
    if hs.calls.is_empty() {
        return String::new();
    }
    let mut out = "## Helpers it calls\n\nPort these as private Rust functions in the same crate (they stay in JavaScript for everyone else).\n\n".to_string();
    for name in &hs.calls {
        match helpers.iter().find(|h| &h.name == name) {
            Some(h) => out.push_str(&format!("```js\n{}\n```\n\n", h.source)),
            None => out.push_str(&format!("- `{name}`: read it in {}\n", hs.file)),
        }
    }
    out
}

pub fn verify(ctx: &Ctx, port: &Port) -> Result<Verify> {
    let node = ctx.runtime()?;
    let replay = backend::replay(ctx, port)?;
    let out = replay.out;
    let job = json!({
        "addon": ctx.lib_dir().join(format!("{}.node", port.ext)), "js_name": port.func, "func": port.func,
        "module_file": ctx.root.join(&port.file), "cases": replay.cases, "held_out": replay.held_out, "out": out, "budget": 0.5,
    });
    // The original may not be exported: a block appended for this run hands it to the runner from
    // inside the module, then the file goes back byte for byte.
    let file = ctx.root.join(&port.file);
    let text = fs::read_to_string(&file).with_context(|| format!("reading {}", port.file))?;
    let hand_over = format!(
        "if (globalThis.__fastAnythingOriginal) globalThis.__fastAnythingOriginal({});",
        target(&port.func, port.owner.as_deref())
    );
    let mut patches = crate::native::Patches::new(ctx)?;
    patches.write(&file, &backend::append_marked(&text, "//", &format!("verifying {}", port.id), &hand_over))?;
    let run = runner(ctx, node, "verify", job);
    patches.restore()?;
    run?;
    let mut v: Verify = serde_json::from_str(&fs::read_to_string(&out)?).context("reading verify results")?;
    v.built = true;
    Ok(v)
}

// ------------------------------------------------------------------ the shim

/// ES module or CommonJS, the way Node decides it.
/// Explicit extensions (.mjs/.mts, .cjs/.cts) and the nearest package.json's "type" win; when
/// package.json is absent or omits "type", fall back to detecting top-level ESM syntax the way
/// Node 22.12+ / 24 does.
fn is_esm(root: &Path, file: &Path) -> bool {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "mjs" | "mts" => return true,
        "cjs" | "cts" => return false,
        _ => {}
    }
    let mut dir = file.parent();
    while let Some(d) = dir {
        if let Ok(text) = fs::read_to_string(d.join("package.json")) {
            let pkg_type = serde_json::from_str::<Value>(text.strip_prefix('\u{feff}').unwrap_or(&text))
                .ok()
                .and_then(|v| v["type"].as_str().map(str::to_string));
            match pkg_type.as_deref() {
                Some("module") => return true,
                Some("commonjs") => return false,
                _ => break,
            }
        }
        if d == root {
            break;
        }
        dir = d.parent();
    }
    fs::read_to_string(file).is_ok_and(|text| has_esm_syntax(&text))
}

/// Whether JS/TS source uses ES module syntax that triggers Node's module syntax detection:
/// top-level `import` / `export` runtime declarations or `import.meta`. Skips comments,
/// strings/templates, regex literals, dynamic `import(...)`, and TypeScript type-only imports/exports.
/// Top-level `await` without `import`/`export`/`import.meta` is intentionally left out so CJS files
/// with expression-bodied `async` arrows are never misclassified as ESM.
fn has_esm_syntax(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let text = if text.starts_with("#!") { text.find('\n').map_or("", |i| &text[i + 1..]) } else { text };
    let bytes = text.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut brace_depth = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut stmt_start = true;
    let mut after_dot = false;
    let mut regex_allowed = true;
    let mut block_brace_next = true;
    let mut cond_paren_next = false;
    let mut saw_newline = true;
    let mut no_regex_before = 0usize;
    // Stack of template-literal interpolation brace depths (`${ ... }`).
    let mut template_braces: Vec<usize> = Vec::new();
    // Whether each open `{` (non-template) started a statement block rather than an object literal.
    let mut brace_stack: Vec<bool> = Vec::new();
    // Whether each open `(` is a control-statement header (`if`/`while`/`for`/`with`) whose `)` allows regex.
    let mut paren_stack: Vec<bool> = Vec::new();

    let unicode_ws_at = |pos: usize| -> Option<(usize, bool)> {
        let ch = text[pos..].chars().next()?;
        if ch == '\u{2028}' || ch == '\u{2029}' {
            Some((ch.len_utf8(), true))
        } else if ch.is_whitespace() || ch == '\u{feff}' {
            Some((ch.len_utf8(), false))
        } else {
            None
        }
    };

    let skip_ws_and_comments = |mut pos: usize| -> usize {
        while pos < len {
            let b = bytes[pos];
            if b.is_ascii_whitespace() {
                pos += 1;
            } else if b >= 0x80
                && let Some((step, _)) = unicode_ws_at(pos)
            {
                pos += step;
            } else if b == b'/' && pos + 1 < len && bytes[pos + 1] == b'/' {
                pos += 2;
                while pos < len && bytes[pos] != b'\n' {
                    pos += 1;
                }
            } else if b == b'/' && pos + 1 < len && bytes[pos + 1] == b'*' {
                pos += 2;
                while pos + 1 < len && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                    pos += 1;
                }
                pos = (pos + 2).min(len);
            } else {
                break;
            }
        }
        pos
    };

    let read_ident = |pos: usize| -> Option<(&str, usize)> {
        if pos >= len {
            return None;
        }
        let b = bytes[pos];
        let mut end = if b.is_ascii_alphabetic() || b == b'_' || b == b'$' {
            pos + 1
        } else if b >= 0x80 {
            let ch = text[pos..].chars().next()?;
            if ch.is_whitespace() || ch == '\u{feff}' {
                return None;
            }
            pos + ch.len_utf8()
        } else {
            return None;
        };
        while end < len {
            let nb = bytes[end];
            if nb.is_ascii_alphanumeric() || nb == b'_' || nb == b'$' {
                end += 1;
            } else if nb >= 0x80 {
                let Some(ch) = text[end..].chars().next() else { break };
                if ch.is_whitespace() || ch == '\u{feff}' {
                    break;
                }
                end += ch.len_utf8();
            } else {
                break;
            }
        }
        Some((&text[pos..end], end))
    };

    while i < len {
        let b = bytes[i];
        if b == b' ' || b == b'\t' || b == b'\x0b' || b == b'\x0c' {
            i += 1;
            continue;
        }
        if b == b'\n' || b == b'\r' {
            saw_newline = true;
            if !after_dot && brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                stmt_start = true;
            }
            i += 1;
            continue;
        }
        if b >= 0x80
            && let Some((step, is_line_sep)) = unicode_ws_at(i)
        {
            if is_line_sep {
                saw_newline = true;
                if !after_dot && brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    stmt_start = true;
                }
            }
            i += step;
            continue;
        }
        // Comments
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'/' {
            i += 2;
            while i < len && bytes[i] != b'\n' && bytes[i] != b'\r' {
                i += 1;
            }
            continue;
        }
        if b == b'/' && i + 1 < len && bytes[i + 1] == b'*' {
            i += 2;
            let mut has_nl = false;
            while i + 1 < len && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' || bytes[i] == b'\r' {
                    has_nl = true;
                }
                i += 1;
            }
            i = (i + 2).min(len);
            if has_nl {
                saw_newline = true;
                if !after_dot && brace_depth == 0 && paren_depth == 0 && bracket_depth == 0 {
                    stmt_start = true;
                }
            }
            continue;
        }
        // Quoted strings
        if b == b'\'' || b == b'"' {
            let q = b;
            i += 1;
            while i < len && bytes[i] != q {
                if bytes[i] == b'\\' {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            i = (i + 1).min(len);
            stmt_start = false;
            after_dot = false;
            regex_allowed = false;
            block_brace_next = false;
            cond_paren_next = false;
            saw_newline = false;
            continue;
        }
        // Template literal start
        if b == b'`' {
            i += 1;
            let mut entered_expr = false;
            while i < len {
                if bytes[i] == b'\\' {
                    i += 2;
                } else if bytes[i] == b'`' {
                    i += 1;
                    break;
                } else if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                    template_braces.push(brace_depth);
                    brace_depth += 1;
                    i += 2;
                    entered_expr = true;
                    break;
                } else {
                    i += 1;
                }
            }
            stmt_start = false;
            after_dot = false;
            regex_allowed = entered_expr;
            block_brace_next = false;
            cond_paren_next = false;
            saw_newline = false;
            continue;
        }
        // Single-line regex literal (bounded by `no_regex_before` so unterminated `/` stays O(N))
        if b == b'/' && regex_allowed && i >= no_regex_before {
            let mut j = i + 1;
            let mut in_class = false;
            let mut closed = false;
            while j < len && bytes[j] != b'\n' && bytes[j] != b'\r' {
                if bytes[j] == b'\\' {
                    if j + 1 >= len || bytes[j + 1] == b'\n' || bytes[j + 1] == b'\r' {
                        break;
                    }
                    j += 2;
                    continue;
                }
                if bytes[j] == b'[' {
                    in_class = true;
                } else if bytes[j] == b']' {
                    in_class = false;
                } else if bytes[j] == b'/' && !in_class {
                    closed = true;
                    break;
                }
                j += 1;
            }
            if closed {
                j += 1;
                while j < len && bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                i = j;
                stmt_start = false;
                after_dot = false;
                regex_allowed = false;
                block_brace_next = false;
                cond_paren_next = false;
                saw_newline = false;
                continue;
            }
            no_regex_before = j;
        }
        // Identifiers / keywords
        if let Some((word, end)) = read_ident(i) {
            if !after_dot {
                if word == "import" {
                    let next = skip_ws_and_comments(end);
                    // `import.meta` triggers ESM anywhere in the file.
                    if next < len && bytes[next] == b'.' {
                        let after = skip_ws_and_comments(next + 1);
                        if let Some(("meta", _)) = read_ident(after) {
                            return true;
                        }
                    } else if brace_depth == 0 && stmt_start && next < len && bytes[next] != b'(' {
                        // Top-level `import` declaration, excluding TS `import type ...` and `import x = require(...)`.
                        match bytes[next] {
                            b'\'' | b'"' | b'{' | b'*' => return true,
                            _ => {
                                if let Some((w2, end2)) = read_ident(next) {
                                    let after_w2 = skip_ws_and_comments(end2);
                                    if w2 == "type" {
                                        // `import type from "mod"` / `import type, { a } from "mod"` imports a default named `type`,
                                        // whereas `import type from from "mod"` is a TS type-only import of a type named `from`.
                                        let is_default_named_type = read_ident(after_w2).is_some_and(|(w3, end3)| {
                                            if w3 != "from" {
                                                return false;
                                            }
                                            let after_from = skip_ws_and_comments(end3);
                                            after_from < len && matches!(bytes[after_from], b'\'' | b'"')
                                        }) || (after_w2 < len && bytes[after_w2] == b',');
                                        if is_default_named_type {
                                            return true;
                                        }
                                    } else if after_w2 < len && bytes[after_w2] == b'=' {
                                        // TypeScript CJS `import x = require(...)`
                                    } else {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                } else if word == "export" && brace_depth == 0 && stmt_start {
                    let next = skip_ws_and_comments(end);
                    if next < len {
                        match bytes[next] {
                            b'{' | b'*' => return true,
                            _ => {
                                if let Some((w2, _)) = read_ident(next)
                                    && matches!(
                                        w2,
                                        "default"
                                            | "function"
                                            | "async"
                                            | "class"
                                            | "const"
                                            | "let"
                                            | "var"
                                            | "enum"
                                            | "abstract"
                                    )
                                {
                                    return true;
                                }
                            }
                        }
                    }
                }
            }
            regex_allowed = !after_dot
                && matches!(
                    word,
                    "return"
                        | "case"
                        | "throw"
                        | "in"
                        | "typeof"
                        | "void"
                        | "delete"
                        | "new"
                        | "yield"
                        | "await"
                        | "else"
                        | "do"
                );
            block_brace_next = after_dot
                || !matches!(
                    word,
                    "return" | "throw" | "yield" | "await" | "typeof" | "void" | "delete" | "in" | "new" | "default"
                );
            cond_paren_next =
                !after_dot && (matches!(word, "if" | "while" | "for" | "with") || (word == "await" && cond_paren_next));
            stmt_start = false;
            after_dot = false;
            saw_newline = false;
            i = end;
            continue;
        }
        // Numbers
        if b.is_ascii_digit() {
            i += 1;
            while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.') {
                i += 1;
            }
            stmt_start = false;
            after_dot = false;
            regex_allowed = false;
            block_brace_next = false;
            cond_paren_next = false;
            saw_newline = false;
            continue;
        }
        // Multi-character operators that affect `/` and `{` disambiguation
        if (b == b'+' && i + 1 < len && bytes[i + 1] == b'+') || (b == b'-' && i + 1 < len && bytes[i + 1] == b'-') {
            stmt_start = false;
            after_dot = false;
            regex_allowed = false;
            block_brace_next = false;
            cond_paren_next = false;
            saw_newline = false;
            i += 2;
            continue;
        }
        if b == b'=' && i + 1 < len && bytes[i + 1] == b'>' {
            stmt_start = false;
            after_dot = false;
            regex_allowed = true;
            block_brace_next = true;
            cond_paren_next = false;
            saw_newline = false;
            i += 2;
            continue;
        }
        // Punctuation
        match b {
            b'{' => {
                brace_stack.push(block_brace_next);
                brace_depth += 1;
                stmt_start = true;
                after_dot = false;
                regex_allowed = true;
                block_brace_next = true;
                cond_paren_next = false;
                saw_newline = false;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if template_braces.last().copied() == Some(brace_depth) {
                    template_braces.pop();
                    // Resume scanning the enclosing template literal.
                    i += 1;
                    let mut entered_expr = false;
                    while i < len {
                        if bytes[i] == b'\\' {
                            i += 2;
                        } else if bytes[i] == b'`' {
                            i += 1;
                            break;
                        } else if bytes[i] == b'$' && i + 1 < len && bytes[i + 1] == b'{' {
                            template_braces.push(brace_depth);
                            brace_depth += 1;
                            i += 2;
                            entered_expr = true;
                            break;
                        } else {
                            i += 1;
                        }
                    }
                    stmt_start = false;
                    after_dot = false;
                    regex_allowed = entered_expr;
                    block_brace_next = false;
                    cond_paren_next = false;
                    saw_newline = false;
                    continue;
                }
                let is_block = brace_stack.pop().unwrap_or(true);
                stmt_start = is_block && brace_depth == 0 && paren_depth == 0 && bracket_depth == 0;
                after_dot = false;
                regex_allowed = is_block;
                block_brace_next = is_block;
                cond_paren_next = false;
                saw_newline = false;
            }
            b'(' => {
                paren_stack.push(cond_paren_next);
                paren_depth += 1;
                stmt_start = false;
                after_dot = false;
                regex_allowed = true;
                block_brace_next = false;
                cond_paren_next = false;
                saw_newline = false;
            }
            b'[' => {
                bracket_depth += 1;
                stmt_start = false;
                after_dot = false;
                regex_allowed = true;
                block_brace_next = false;
                cond_paren_next = false;
                saw_newline = false;
            }
            b')' => {
                paren_depth = paren_depth.saturating_sub(1);
                let after_cond = paren_stack.pop().unwrap_or(false);
                stmt_start = false;
                after_dot = false;
                regex_allowed = after_cond;
                block_brace_next = true;
                cond_paren_next = false;
                saw_newline = false;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                stmt_start = false;
                after_dot = false;
                regex_allowed = false;
                block_brace_next = false;
                cond_paren_next = false;
                saw_newline = false;
            }
            b'!' => {
                // TypeScript postfix non-null assertion (`expr!`) keeps `/` as division; prefix `!` and `!=` allow regex.
                regex_allowed = (i + 1 < len && bytes[i + 1] == b'=') || regex_allowed || saw_newline;
                stmt_start = false;
                after_dot = false;
                block_brace_next = false;
                cond_paren_next = false;
                saw_newline = false;
            }
            b';' => {
                stmt_start = brace_depth == 0 && paren_depth == 0 && bracket_depth == 0;
                after_dot = false;
                regex_allowed = true;
                block_brace_next = true;
                cond_paren_next = false;
                saw_newline = false;
            }
            b'.' => {
                stmt_start = false;
                after_dot = true;
                regex_allowed = false;
                block_brace_next = false;
                cond_paren_next = false;
                saw_newline = false;
            }
            _ => {
                stmt_start = false;
                after_dot = false;
                regex_allowed = true;
                block_brace_next = false;
                cond_paren_next = false;
                saw_newline = false;
            }
        }
        i += 1;
    }
    false
}

/// Whether `file` runs as an ES module under `runtime`. Deno treats everything except .cjs/.cts as ESM.
fn is_esm_file(root: &Path, file: &Path, runtime: Runtime) -> bool {
    let cjs = matches!(file.extension().and_then(|e| e.to_str()), Some("cjs" | "cts"));
    is_esm(root, file) || (runtime == Runtime::Deno && !cjs)
}

/// How the module refers to the function: its name, or Class.name for a static method.
fn target(name: &str, owner: Option<&str>) -> String {
    owner.map_or_else(|| name.to_string(), |class| format!("{class}.{name}"))
}

/// CommonJS copies function references when `module.exports` / `exports` is assigned, so rebinding
/// the local name at the end of the file alone does not update what callers got from `require()`.
/// Sync any slot on `module.exports` still holding `orig` to `target` without invoking getters or
/// throwing on read-only/frozen properties, preserving own properties attached to default exports.
fn cjs_sync_exports(target: &str, orig: &str, indent: &str) -> String {
    format!(
        "{indent}if (typeof module === \"object\" && module !== null) {{\n\
         {indent}  const _faMod = Object.getOwnPropertyDescriptor(module, \"exports\");\n\
         {indent}  const _faExp = _faMod && \"value\" in _faMod ? _faMod.value : undefined;\n\
         {indent}  if (_faExp === {orig}) {{\n\
         {indent}    if (_faMod.writable) {{\n\
         {indent}      if (typeof {target} === \"function\" && Object.isExtensible({target})) {{\n\
         {indent}        for (const _faKey of Object.keys({orig})) {{\n\
         {indent}          if (Object.hasOwn({target}, _faKey)) continue;\n\
         {indent}          const _faDesc = Object.getOwnPropertyDescriptor({orig}, _faKey);\n\
         {indent}          if (!_faDesc) continue;\n\
         {indent}          if (\"value\" in _faDesc && _faDesc.value === {orig}) {{\n\
         {indent}            Object.defineProperty({target}, _faKey, {{ ..._faDesc, value: {target} }});\n\
         {indent}          }} else {{\n\
         {indent}            Object.defineProperty({target}, _faKey, _faDesc);\n\
         {indent}          }}\n\
         {indent}        }}\n\
         {indent}      }}\n\
         {indent}      module.exports = {target};\n\
         {indent}    }}\n\
         {indent}  }} else if (_faExp && (typeof _faExp === \"object\" || typeof _faExp === \"function\")) {{\n\
         {indent}    for (const _faKey of Object.keys(_faExp)) {{\n\
         {indent}      const _faDesc = Object.getOwnPropertyDescriptor(_faExp, _faKey);\n\
         {indent}      if (_faDesc && _faDesc.writable && _faDesc.value === {orig}) _faExp[_faKey] = {target};\n\
         {indent}    }}\n\
         {indent}  }}\n\
         {indent}}}"
    )
}

fn record_wrap(target: &str, esm: bool) -> String {
    if esm {
        return format!(
            "{target} = globalThis.__fastAnythingWrap ? globalThis.__fastAnythingWrap({target}) : {target};"
        );
    }
    let sync = cjs_sync_exports(target, "_faOrig", "  ");
    format!(
        "if (globalThis.__fastAnythingWrap) {{\n\
         \x20 const _faOrig = {target};\n\
         \x20 {target} = globalThis.__fastAnythingWrap(_faOrig);\n\
         {sync}\n\
         }}"
    )
}

pub fn shim(port: &Port, addon_rel: &str, esm: bool) -> String {
    let f = target(&port.func, port.owner.as_deref());
    let load = if esm {
        format!("process.getBuiltinModule(\"node:module\").createRequire(import.meta.url)(\"{addon_rel}\")")
    } else {
        format!("require(\"{addon_rel}\")")
    };
    let mut lines = vec![
        "// Uses the verified Rust port. FAST_ANYTHING_DISABLE=1 runs the JavaScript above instead.".to_string(),
        format!("// Undo with:  fast-anything disable {}", port.id),
        "if (!process.env.FAST_ANYTHING_DISABLE) {".to_string(),
        "  try {".to_string(),
    ];
    if esm {
        lines.push(format!("    {f} = {load}.{};", port.func));
    } else {
        lines.push(format!("    const _faOrig = {f};"));
        lines.push(format!("    {f} = {load}.{};", port.func));
        lines.push(cjs_sync_exports(&f, "_faOrig", "    "));
    }
    lines.push("  } catch {}".to_string());
    lines.push("}".to_string());
    lines.join("\n")
}

pub fn enable(ctx: &Ctx, port: &Port) -> Result<()> {
    let file = ctx.root.join(&port.file);
    let text = fs::read_to_string(&file).with_context(|| format!("reading {}", port.file))?;
    let from = file.parent().context("module has no folder")?;
    let mut rel = forward_slashes(&relative(from, &ctx.lib_dir().join(format!("{}.node", port.ext))));
    if !rel.starts_with("../") {
        rel = format!("./{rel}");
    }
    let runtime = ctx.state.runtime.as_deref().map_or(Runtime::Node, runtime_of);
    let block = shim(port, &rel, is_esm_file(&ctx.root, &file, runtime));
    fs::write(&file, with_shim(&text, port, &block)).with_context(|| format!("writing {}", port.file))
}

pub fn disable(ctx: &Ctx, port: &Port) -> Result<()> {
    let file = ctx.root.join(&port.file);
    let text = fs::read_to_string(&file).with_context(|| format!("reading {}", port.file))?;
    fs::write(&file, without_shim(&text, port)).with_context(|| format!("writing {}", port.file))
}

/// Byte offset of the keyword in the top-level `[export] <keyword> <name>` declaration, if any.
/// A module can declare a name only once at the top level, so there's at most one.
fn declaration(text: &str, keyword: &str, name: &str) -> Option<usize> {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let mut rest = if offset == 0 { line.strip_prefix('\u{feff}').unwrap_or(line) } else { line };
        if let Some(r) = rest.strip_prefix("export").filter(|r| r.starts_with([' ', '\t'])) {
            rest = r.trim_start();
        }
        let at = offset + line.len() - rest.len();
        if let Some(r) = rest.strip_prefix(keyword).filter(|r| r.starts_with([' ', '\t']))
            && r.trim_start().strip_prefix(name).is_some_and(|r| !r.starts_with(ident))
        {
            return Some(at);
        }
        offset += line.len();
    }
    None
}

/// The text with the declaration of name switched from one keyword to another (const <-> let).
fn redeclare(text: &str, from: &str, to: &str, name: &str) -> Option<String> {
    let at = declaration(text, from, name)?;
    Some(format!("{}{to}{}", &text[..at], &text[at + from.len()..]))
}

/// A const can't be reassigned, so a function declared with const is declared with let while
/// fast-anything swaps it; this note in the block tells disable to put the const back.
fn let_note(name: &str) -> String {
    format!("// fast-anything declared {name} with let instead of const above; disable puts the const back.")
}

fn with_shim(text: &str, port: &Port, block: &str) -> String {
    let note = let_note(&port.func);
    let (text, swapped) = match redeclare(text, "const", "let", &port.func).filter(|_| port.owner.is_none()) {
        Some(t) => (t, true),
        None => (text.to_string(), text.contains(&note)), // already swapped by an earlier enable
    };
    let block = if swapped { format!("{block}\n{note}") } else { block.to_string() };
    add_marked(&text, &port.id, &block, port.module == "__main__")
}

fn without_shim(text: &str, port: &Port) -> String {
    let swapped = text.contains(&let_note(&port.func));
    let text = backend::strip_marked(text, "//", &port.id);
    if swapped { redeclare(&text, "let", "const", &port.func).unwrap_or(text) } else { text }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_bom_node_file_scans_enables_and_restores_byte_for_byte() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let cases = [
            (
                "func",
                b"\xef\xbb\xbfexport function crunch(n) {\r\n  let t = 0;\r\n  for (let i = 0; i < n; i++) t += (i * i) % 97;\r\n  return t;\r\n}\r\n"
                    .as_slice(),
            ),
            (
                "const",
                b"\xef\xbb\xbfexport const crunch = (n) => {\r\n  let t = 0;\r\n  for (let i = 0; i < n; i++) t += (i * i) % 97;\r\n  return t;\r\n};\r\n"
                    .as_slice(),
            ),
        ];
        let tmp = std::env::temp_dir();
        let tmp = if cfg!(windows) { tmp } else { tmp.canonicalize().unwrap() };
        let base = tmp.join(format!("fa-node-bom-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        // Bare `const` after line-1 BOM matches, while non-line-1 U+FEFF is not skipped.
        assert_eq!(declaration("\u{feff}const crunch = (n) => n;\n", "const", "crunch"), Some("\u{feff}".len()));
        assert_eq!(declaration("const a = 1;\n\u{feff}const crunch = (n) => n;\n", "const", "crunch"), None);
        // package.json with UTF-8 BOM is still recognized as ESM.
        let pkg_dir = base.join("bom-pkg");
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("package.json"), b"\xef\xbb\xbf{\"type\": \"module\"}\r\n").unwrap();
        assert!(is_esm(&pkg_dir, &pkg_dir.join("work.js")));
        for (name, work_bytes) in cases {
            let dir = base.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("work.mjs"), work_bytes).unwrap();
            fs::write(
                dir.join("app.mjs"),
                b"\xef\xbb\xbfimport { crunch } from './work.mjs';\r\nlet s = 0;\r\nfor (let r = 0; r < 200; r++) s += crunch(100000);\r\nconsole.log(s);\r\n",
            )
            .unwrap();
            let scanned = crate::pipeline::scan(&dir, vec!["node".into(), "app.mjs".into()], None).unwrap();
            let h =
                scanned.hotspots.iter().find(|h| h.name == "crunch").unwrap_or_else(|| {
                    panic!("{name}: BOM node file should appear in scan, got {:?}", scanned.hotspots)
                });
            assert!(h.portable(), "{name}: crunch should be portable, got {:?}", h.blockers);
            crate::pipeline::port(&dir, Some("work.crunch"), 10).unwrap();
            let ctx = Ctx::open(&dir).unwrap();
            let port = &ctx.state.ports["work.crunch"];
            assert!(!port.sources.is_empty(), "{name}: verbatim source should be captured for BOM file");
            assert!(!ctx.stale(port), "{name}: fresh BOM port must not be reported stale");
            crate::pipeline::enable(&dir, Some("work.crunch"), true).unwrap();
            let enabled_bytes = fs::read(dir.join("work.mjs")).unwrap();
            assert!(enabled_bytes.starts_with(b"\xef\xbb\xbf"), "{name}: enable must preserve UTF-8 BOM");
            if name == "const" {
                let text = String::from_utf8_lossy(&enabled_bytes);
                assert!(
                    text.starts_with("\u{feff}export let crunch"),
                    "{name}: const must switch to let after BOM: {text}"
                );
            }
            assert!(!Ctx::open(&dir).unwrap().stale(port), "{name}: enabled BOM port must not be reported stale");
            crate::pipeline::disable(&dir, Some("work.crunch")).unwrap();
            let restored_bytes = fs::read(dir.join("work.mjs")).unwrap();
            assert_eq!(restored_bytes, work_bytes, "{name}: disable must restore BOM file byte for byte");
            let edited = String::from_utf8_lossy(work_bytes).replace("let t = 0;", "let t = 1;");
            fs::write(dir.join("work.mjs"), edited.as_bytes()).unwrap();
            let stale_after_edit = Ctx::open(&dir).unwrap().stale(port);
            let _ = fs::remove_dir_all(&dir);
            assert!(stale_after_edit, "{name}: editing first-line BOM function must trigger stale detection");
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn scan_handles_regex_literals_and_division_ambiguity() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let tmp = std::env::temp_dir();
        let tmp = if cfg!(windows) { tmp } else { tmp.canonicalize().unwrap() };
        let dir = tmp.join(format!("fa-node-regex-lexer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Put regexes with quotes, character-class slashes, and return-regex above a single hot
        // function so V8 sampling reliably captures it even on fast CI runners.
        fs::write(
            dir.join("work.mjs"),
            "export function stripQuotes(s) {\n\
             \x20 return s.replace(/['\"]/g, \"\").replace(/[/']/g, \"-\");\n\
             }\n\n\
             export function hasQuote(s) {\n\
             \x20 return /'/.test(s);\n\
             }\n\n\
             export function afterRegex(n) {\n\
             \x20 let t = 0;\n\
             \x20 for (let i = 0; i < n; i++) t += (i * i) % 97;\n\
             \x20 return t;\n\
             }\n",
        )
        .unwrap();
        fs::write(
            dir.join("app.mjs"),
            "import { stripQuotes, hasQuote, afterRegex } from './work.mjs';\n\
             let s = stripQuotes(\"a/b'c\") + hasQuote(\"'\");\n\
             let total = 0;\n\
             for (let r = 0; r < 200; r++) total += afterRegex(100000);\n\
             console.log(s, total);\n",
        )
        .unwrap();
        let scanned = crate::pipeline::scan(&dir, vec!["node".into(), "app.mjs".into()], None);
        let _ = fs::remove_dir_all(&dir);
        let scanned = scanned.unwrap();
        let h = scanned.hotspots.iter().find(|h| h.name == "afterRegex").unwrap_or_else(|| {
            panic!("missing hotspot afterRegex; got {:?}", scanned.hotspots.iter().map(|h| &h.name).collect::<Vec<_>>())
        });
        assert!(h.portable(), "afterRegex should be portable, got blockers {:?}", h.blockers);
    }

    #[test]
    fn code_only_handles_regex_and_division_deterministically() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let tmp = std::env::temp_dir();
        let tmp = if cfg!(windows) { tmp } else { tmp.canonicalize().unwrap() };
        let dir = tmp.join(format!("fa-node-codeonly-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // Export codeOnly/describe from a temporary copy of runner.mjs to assert exact blanking on tricky tokens
        // (and verify linear time on adversarial unterminated-slash lines).
        let stripped = RUNNER.split("const [mode, jobPath]").next().unwrap();
        fs::write(dir.join("lib.mjs"), format!("{stripped}\nexport {{ codeOnly, describe }};\n")).unwrap();
        fs::write(
            dir.join("check.mjs"),
            r#"import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { codeOnly, describe } from "./lib.mjs";

const root = path.dirname(fileURLToPath(import.meta.url));

// Division preserved intact (two slashes on the same line pin every guard against regex misreads)
for (const line of [
  "const r = a / b / c;",
  "const r = (a + b) / 2 / arr[0];",
  "const r = arr[0] / 2 / 3;",
  "const r = obj.return / 2 / 3;",
  "const r = i++ / 2 / 3;",
  "const r = i-- / 2 / 3;",
  "const r = 10 / 2 / 3;",
  "const r = 1.5 / 2 / 3;",
  'const r = "10" / 2 / 3;',
  "const r = of / 2 / 3;",
  "const r = { a: 1 } / 2 / 3;",
  "return { a: 1 } / 2 / 3;",
  "const r = { a: () => { return 1; } } / 2 / 3;",
  "const r = π / 2 / 3;",
  "const r = 数量 / 2 / 3;",
  "const r = a! / console.log(1) / 2;",
  "const r = a!! / 2 / 3;",
]) {
  assert.equal(codeOnly(line), line.replace('"10"', '"  "'), `division misread: ${line}`);
}
assert.equal(codeOnly("const r = /a/ / 2 / 3;"), "const r = / / / 2 / 3;");

// Division followed by a quoted line comment (must not pair `/` with `//`)
for (const prefix of [
  "const x = y / 2",
  "const x = obj.return / 2",
  "const x = i++ / 2",
  "const x = of / 2",
  "const x = { a: 1 } / 2",
  "const x = π / 2",
  "const x = m.get('a')! / 2",
]) {
  const src = `${prefix}; // don't "break"\nconst z = 1;`;
  const res = codeOnly(src);
  assert.equal(res.length, src.length, `length drift for ${prefix}`);
  assert.ok(res.startsWith(`${prefix.replace("'a'", "' '")}; `), `prefix damaged: ${res}`);
  assert.ok(res.endsWith("\nconst z = 1;"), `next line swallowed for ${prefix}: ${res}`);
}

// Regex literals: start-of-file, quotes, char-class slashes, escapes, flags, keywords, blocks, prefix `!`
assert.equal(codeOnly("/'/.test(s);"), "/ /.test(s);");
assert.equal(codeOnly("/>/.test(s);"), "/ /.test(s);");
assert.equal(codeOnly("a < /'/.source.length;"), "a < / /.source.length;");
assert.equal(codeOnly("s.replace(/['\"]/g, \"\")"), "s.replace(/    /g, \"\")");
assert.equal(codeOnly("s.replace(/[/]/g, \"-\")"), "s.replace(/   /g, \" \")");
assert.equal(codeOnly("return /https?:\\/\\//.test(s);"), "return /           /.test(s);");
assert.equal(codeOnly("const t = `a / b / 'c'`;\nconst ok = 1;"), "const t = `           `;\nconst ok = 1;");

for (const [src, expected] of [
  ["return /'/.test(s);", "return / /.test(s);"],
  ["case /'/: break;", "case / /: break;"],
  ["throw /'/;", "throw / /;"],
  ["typeof /'/", "typeof / /"],
  ["void /'/", "void / /"],
  ["delete /'/", "delete / /"],
  ["new /'/", "new / /"],
  ["yield /'/", "yield / /"],
  ["await /'/", "await / /"],
  ["if (a) b(); else /'/.test(s);", "if (a) b(); else / /.test(s);"],
  ["do /'/.test(s); while (0);", "do / /.test(s); while (0);"],
  ["x in /'/", "x in / /"],
  ["function foo() {} /'/.test(s);", "function foo() {} / /.test(s);"],
  ["if (ok) { a(); } /'/.test(s);", "if (ok) { a(); } / /.test(s);"],
  ["const f = () => { return 1; } /'/.test(s);", "const f = () => { return 1; } / /.test(s);"],
  ["class C {} /'/.test(s);", "class C {} / /.test(s);"],
  ["class C extends ns.Base {} /'/.test(s);", "class C extends ns.Base {} / /.test(s);"],
  ["if (!/'/.test(s)) {}", "if (!/ /.test(s)) {}"],
  ["foo()\n!/'/.test(s);", "foo()\n!/ /.test(s);"],
  ["foo() /*\n*/ !/'/.test(s);", "foo()   \n   !/ /.test(s);"],
  ["a != /'/.source;", "a != / /.source;"],
  ["a !== /'/.source;", "a !== / /.source;"],
]) {
  assert.equal(codeOnly(src), expected, `regex misread: ${src}`);
}

// Unterminated `/` on a line never crosses LF, CRLF, or trailing backslash before newline
assert.equal(codeOnly("x = /a\nconst b = 1 / 2;"), "x = /a\nconst b = 1 / 2;");
assert.equal(codeOnly("x = /a\r\nconst b = 1 / 2;"), "x = /a\r\nconst b = 1 / 2;");
assert.equal(codeOnly("x = /a\\\nconst b = /'/;"), "x = /a\\\nconst b = / /;");

// Escaped quotes inside regex and comments don't leak into later functions
const modFile = path.join(root, "sample.mjs");
const sample = [
  "export function f1(s) { return s.replace(/['\"]/g, ''); }",
  "export function f2(s) { return s.replace(/[/]/g, '-'); }",
  "export function f3(s) { return /'/.test(s); }",
  "export function f4(a, b, c) { return a / b / c; }",
  "export function f5(y) { const x = y / 2; // don't break\n  return x; }",
  "export function f6(s) { const t = `a / b`; return t + s; }",
].join("\n") + "\n";
fs.writeFileSync(modFile, sample);
for (const [line0, name] of [[0, "f1"], [1, "f2"], [2, "f3"], [3, "f4"], [4, "f5"], [6, "f6"]]) {
  const info = describe(modFile, line0, name, null, root);
  assert.ok(info, `describe missed ${name}`);
  assert.deepEqual(info.blockers, [], `${name} had unexpected blockers: ${info.blockers}`);
}

// JSX closing tags (`</tag>`, `</>`) and self-closing tags (`/>`) must not be misread as regex
const jsxFile = path.join(root, "sample.jsx");
const jsxSample = [
  'export function ViewA() { return <span>Home</span><a href="/about">About</a>; }',
  "export function afterA(x) { return x + 1; }",
  "export function ViewB() { return <span>a</span><span>{console.log(1)}</span>; }",
  "export function ViewC(a) { return <div><span>a</span>{a && <span>b</span>}</div>; }",
  "export function ViewD() { return <Icon size={16} /><button onClick={() => console.log(1)}>ok</button>; }",
  "export function ViewFrag() { return <><span>a</span></>; }",
  "export function afterFrag(x) { return x + 2; }",
].join("\n") + "\n";
fs.writeFileSync(jsxFile, jsxSample);
for (const [line0, name] of [[1, "afterA"], [3, "ViewC"], [6, "afterFrag"]]) {
  const info = describe(jsxFile, line0, name, null, root);
  assert.ok(info, `describe missed JSX ${name}`);
  assert.deepEqual(info.blockers, [], `JSX ${name} had unexpected blockers: ${info.blockers}`);
}
for (const [line0, name] of [[2, "ViewB"], [4, "ViewD"]]) {
  const info = describe(jsxFile, line0, name, null, root);
  assert.ok(info, `describe missed JSX ${name}`);
  assert.ok(info.blockers.some((b) => b.includes("console")), `JSX ${name} lost console blocker: ${info.blockers}`);
}

// TypeScript postfix non-null assertion `!` before `/` must not swallow following functions or console I/O
const tsxFile = path.join(root, "sample.tsx");
const tsxSample = [
  "export function tsDiv(m) { const x = m.get('a')! / 2; // don't break",
  "  return x; }",
  "export function afterTs(x) { return x + 1; }",
  "export function tsConsole(a) { return a! / console.log(1) / 2; }",
].join("\n") + "\n";
fs.writeFileSync(tsxFile, tsxSample);
const afterTs = describe(tsxFile, 2, "afterTs", null, root);
assert.ok(afterTs, "describe missed afterTs");
assert.deepEqual(afterTs.blockers, [], `afterTs had unexpected blockers: ${afterTs.blockers}`);
const tsConsole = describe(tsxFile, 3, "tsConsole", null, root);
assert.ok(tsConsole, "describe missed tsConsole");
assert.ok(tsConsole.blockers.some((b) => b.includes("console")), `tsConsole lost console blocker: ${tsConsole.blockers}`);

// Adversarial inputs with unterminated slashes (odd repeat count prevents even-pairing false passes)
for (const worst of [
  "x = /a\n".repeat(5001) + "const ok = 1;",
  "x = /[".repeat(10000) + "\nconst ok = 1;",
]) {
  const res = codeOnly(worst);
  assert.equal(res, worst);
}
"#,
        )
        .unwrap();
        let out = Command::new("node").arg("check.mjs").current_dir(&dir).output().unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{}", combined(&out));
    }

    #[test]
    fn numbers_match_to_their_size_or_their_arrays() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("fa-node-same-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let stripped = RUNNER.split("const [mode, jobPath]").next().unwrap();
        fs::write(dir.join("lib.mjs"), format!("{stripped}\nexport {{ same }};\n")).unwrap();
        fs::write(
            dir.join("check.mjs"),
            r#"import assert from "node:assert/strict";
import { same } from "./lib.mjs";

assert.ok(!same(4.2441e-19, 4.2441e-28));
assert.ok(!same(-0, 0) && !same(0, -0) && same(-0, -0) && same(0, 0));
assert.ok(same([1.0, 2.0, 5.551115123125783e-17], [1.0, 2.0, 0.0]));
assert.ok(same(new Float64Array([1, 5.551115123125783e-17]), new Float64Array([1, 0])));
assert.ok(!same({ a: 1, b: 5.551115123125783e-17 }, { a: 1, b: 0 }));
assert.ok(!same([1e10, 3], [1e10, 4]));
assert.ok(same(1.0, 1.0 + 5e-10) && !same(1.0, 1.0 + 2e-9));
assert.ok(same(NaN, NaN) && !same(NaN, 0) && same(Infinity, Infinity) && !same(Infinity, -Infinity));
assert.ok(!same([Number.MAX_VALUE, Infinity], [Number.MAX_VALUE, Number.MAX_VALUE]));
"#,
        )
        .unwrap();
        let out = Command::new("node").arg("check.mjs").current_dir(&dir).output().unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{}", combined(&out));
    }

    #[test]
    fn bun_and_deno_get_their_own_command_lines() {
        let args = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(Runtime::of("/usr/local/bin/bun"), Some(Runtime::Bun));
        assert_eq!(Runtime::of("C:\\deno\\deno.exe"), Some(Runtime::Deno));
        assert!(is_node("bun") && is_node("deno") && !is_node("python"));
        assert_eq!(Runtime::Node.with(&["--require", "r.cjs"], &args("app.js x")), args("--require r.cjs app.js x"));
        assert_eq!(Runtime::Bun.with(&["--preload", "r.cjs"], &args("run app.ts")), args("--preload r.cjs app.ts"));
        assert_eq!(Runtime::Deno.with(&["--cpu-prof"], &args("run -A app.ts x")), args("run --cpu-prof -A app.ts x"));
        assert_eq!(Runtime::Deno.with(&["--cpu-prof"], &args("app.ts")), args("run --cpu-prof app.ts"));
        assert_eq!(Runtime::Deno.script(Path::new("runner.mjs")), args("run -A runner.mjs"));
        assert_eq!(entry(&args("run -A --allow-ffi app.ts data.txt")).map(String::as_str), Some("app.ts"));
        assert_eq!(entry(&args("run app.ts")).map(String::as_str), Some("app.ts"));
    }

    #[test]
    fn duration_ends_an_app_that_runs_until_closed() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let tmp = std::env::temp_dir();
        // macOS's temp folder is behind a symlink; the profile has the real paths
        let tmp = if cfg!(windows) { tmp } else { tmp.canonicalize().unwrap() };
        let dir = tmp.join(format!("fa-node-duration-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // a server's shape: work on a timer, forever
        let app = "function work(n) {\n  let s = 0;\n  for (let i = 0; i < n; i++) s += i * i;\n  return s;\n}\nsetInterval(() => work(3e6), 5);\n";
        fs::write(dir.join("app.js"), app).unwrap();
        let ctx = Ctx::open(&dir).unwrap();
        let started = std::time::Instant::now();
        let p = profile(&ctx, "node", &["app.js".into()], Some(1.5));
        let took = started.elapsed().as_secs_f64();
        let _ = fs::remove_dir_all(&dir);
        let p = p.unwrap();
        assert!(p.functions.iter().any(|f| f.name == "work"), "{:?}", p.functions);
        assert!(took < 12.0, "{took}");
    }

    #[test]
    fn worker_threads_are_recorded() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("fa-node-workers-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("recorder.cjs"), RECORDER).unwrap();
        fs::write(
            dir.join("lib.cjs"),
            "function sq(x) {\n  return x * x;\n}\nsq = globalThis.__fastAnythingWrap ? globalThis.__fastAnythingWrap(sq) : sq;\nmodule.exports = { sq };\n",
        )
        .unwrap();
        fs::write(
            dir.join("app.cjs"),
            "const { Worker, isMainThread, parentPort } = require(\"node:worker_threads\");\n\
             const { sq } = require(\"./lib.cjs\");\n\
             if (isMainThread) {\n\
             \x20 sq(1);\n\
             \x20 for (let i = 0; i < 2; i++) new Worker(__filename).on(\"message\", (m) => console.log(m));\n\
             } else {\n\
             \x20 let t = 0;\n\
             \x20 for (let i = 0; i < 5; i++) t += sq(i);\n\
             \x20 parentPort.postMessage(t);\n\
             }\n",
        )
        .unwrap();
        let out = Command::new("node")
            .args(["--require", "./recorder.cjs", "app.cjs"])
            .current_dir(&dir)
            .env("FAST_ANYTHING_RECORD_OUT", dir.join("rec.jsonl"))
            .env("FAST_ANYTHING_RECORD_META", dir.join("rec.meta.json"))
            .env("FAST_ANYTHING_RECORD_LIMIT", "500")
            .output()
            .unwrap();
        let meta = fs::read_to_string(dir.join("rec.meta.json")).unwrap_or_default();
        let lines = fs::read_to_string(dir.join("rec.jsonl")).unwrap_or_default().lines().count();
        let leftovers = fs::read_dir(&dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".t"))
            .count();
        let _ = fs::remove_dir_all(&dir);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let meta: Value = serde_json::from_str(&meta).unwrap();
        assert_eq!((meta["calls"].clone(), lines), (json!(11), 11), "1 call in the main thread, 5 in each worker");
        assert_eq!(leftovers, 0, "worker files are absorbed and removed");
    }

    /// Calls reached through an import: `readFileSync` after `import { readFileSync } from "node:fs"`,
    /// `cp.spawnSync` after `const cp = require("child_process")`. Each case says what scan must block,
    /// and what it must leave alone (pure members, shadowed names, the app's own modules, comments).
    const IMPORT_CASES: &[(&str, &str, &str, &[&str])] = &[
        (
            "named",
            "import { readFileSync } from \"node:fs\";\nexport function f(p) {\n  return readFileSync(p, \"utf8\").length;\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "alias",
            "import { execSync as run } from \"child_process\";\nexport function f(x) {\n  run(\"true\");\n  return x;\n}\n",
            "f",
            &["does I/O: child_process.execSync"],
        ),
        (
            "namespace",
            "import * as files from \"node:fs\";\nexport function f(p) {\n  return files.statSync(p).size;\n}\n",
            "f",
            &["does I/O: fs.statSync"],
        ),
        (
            "default",
            "import disk from \"fs\";\nexport function f(p, x) {\n  disk.writeFileSync(p, String(x));\n  return x;\n}\n",
            "f",
            &["does I/O: fs.writeFileSync"],
        ),
        (
            "promises member",
            "import { promises as fsp } from \"fs\";\nexport function f(p) {\n  return fsp.stat(p);\n}\n",
            "f",
            &["does I/O: fs.promises"],
        ),
        (
            "fs/promises",
            "import { readFile } from \"node:fs/promises\";\nexport function f(p) {\n  return readFile(p);\n}\n",
            "f",
            &["does I/O: fs.readFile"],
        ),
        (
            "multi-line",
            "import {\n  readFileSync,\n  statSync,\n} from \"node:fs\";\nexport function f(p) {\n  return statSync(p).size;\n}\n",
            "f",
            &["does I/O: fs.statSync"],
        ),
        (
            "random",
            "import { randomInt } from \"node:crypto\";\nexport function f(x) {\n  return x + randomInt(10);\n}\n",
            "f",
            &["not deterministic: crypto.randomInt"],
        ),
        (
            "perf alias",
            "import { performance as perf } from \"node:perf_hooks\";\nexport function f(x) {\n  return x + perf.now();\n}\n",
            "f",
            &["not deterministic: perf_hooks.performance"],
        ),
        (
            "process member",
            "import { env } from \"node:process\";\nexport function f(x) {\n  return env.HOME ?? x;\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "require destructure",
            "const { readFileSync: rd } = require(\"fs\");\nfunction f(p) {\n  return rd(p).length;\n}\nmodule.exports = { f };\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "require module",
            "const cp = require(\"node:child_process\");\nfunction f(x) {\n  cp.spawnSync(\"true\");\n  return x;\n}\nmodule.exports = { f };\n",
            "f",
            &["does I/O: child_process.spawnSync"],
        ),
        (
            "require member",
            "const fsp = require(\"fs\").promises;\nfunction f(p) {\n  return fsp.readFile(p);\n}\nmodule.exports = { f };\n",
            "f",
            &["does I/O: fs.promises"],
        ),
        (
            "fs as fs, once",
            "import fs from \"node:fs\";\nexport function f(p) {\n  return fs.readFileSync(p).length;\n}\n",
            "f",
            &["does I/O: fs"],
        ),
        (
            "hash pure",
            "import { createHash } from \"node:crypto\";\nexport function f(s) {\n  return createHash(\"sha1\").update(s).digest(\"hex\");\n}\n",
            "f",
            &[],
        ),
        (
            "path pure",
            "import { join } from \"node:path\";\nexport function f(a, b) {\n  return join(a, b);\n}\n",
            "f",
            &[],
        ),
        (
            "fs.constants pure",
            "import { constants } from \"node:fs\";\nexport function f(x) {\n  return constants.F_OK + x;\n}\n",
            "f",
            &[],
        ),
        (
            "net.isIP pure",
            "import * as net from \"node:net\";\nexport function f(s) {\n  return net.isIP(s) === 4;\n}\n",
            "f",
            &[],
        ),
        (
            "param shadow",
            "import { env } from \"node:process\";\nexport function f(env) {\n  return env.k;\n}\n",
            "f",
            &[],
        ),
        (
            "local shadow",
            "import { readFileSync } from \"fs\";\nexport function f(x) {\n  const readFileSync = (v) => v + 1;\n  return readFileSync(x);\n}\n",
            "f",
            &[],
        ),
        (
            "arrow param shadow",
            "import * as os from \"node:os\";\nexport function f(items) {\n  return items.map((os) => os.name);\n}\n",
            "f",
            &[],
        ),
        (
            "object key",
            "import { env } from \"node:process\";\nexport function f(x) {\n  return { env: x };\n}\n",
            "f",
            &[],
        ),
        (
            "commented import",
            "/*\nimport { statSync } from \"fs\";\n*/\nfunction statSync(x) {\n  return { size: x };\n}\nexport function f(x) {\n  return statSync(x).size;\n}\n",
            "f",
            &[],
        ),
        ("own module", "import { load } from \"./io.mjs\";\nexport function f(x) {\n  return load(x);\n}\n", "f", &[]),
        (
            "other function",
            "import { readFileSync } from \"fs\";\nexport function load(p) {\n  return readFileSync(p);\n}\nexport function f(x) {\n  return x * 2;\n}\n",
            "f",
            &[],
        ),
        (
            "type import",
            "import type { Stats } from \"node:fs\";\nexport function f(s: Stats): number {\n  return s.size;\n}\n",
            "f",
            &[],
        ),
        // a name taken over in one place still counts everywhere else
        (
            "arrow shadow, used outside it",
            "import * as os from \"node:os\";\nexport function f(items) {\n  const host = os.hostname();\n  return items.map((os) => os.name).concat(host);\n}\n",
            "f",
            &["does I/O: os.hostname"],
        ),
        (
            "used in a default",
            "import { readFileSync } from \"node:fs\";\nexport function f(p, fallback = readFileSync(\"/x\", \"utf8\")) {\n  return readFileSync(p, \"utf8\") || fallback;\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "used in an arrow's default",
            "import { env } from \"node:process\";\nexport function f(xs) {\n  return xs.map((x = env.DEFAULT) => x);\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "destructured key",
            "import { env } from \"node:process\";\nexport function f(cfg) {\n  const { env: mode } = cfg;\n  return mode + env.HOME;\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "computed key",
            "import { env } from \"node:process\";\nexport function f(cfg) {\n  const { [env.KEY]: val } = cfg;\n  return val;\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "parameter pattern key",
            "import { env } from \"node:process\";\nexport function f({ env: mode }) {\n  return mode + env.HOME;\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "for-of pattern",
            "import { env } from \"node:process\";\nexport function f(items, t) {\n  for (const { a } of items) t[env.HOME] = a;\n  return t;\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        // import forms
        (
            "comment in an import",
            "import {\n  // sync read\n  readFileSync,\n} from \"node:fs\";\nexport function f(p) {\n  return readFileSync(p, \"utf8\").length;\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "require, comment inside",
            "const {\n  readFileSync, // reads\n  statSync,\n} = require(\"node:fs\");\nfunction f(p) {\n  return statSync(p).size;\n}\nmodule.exports = { f };\n",
            "f",
            &["does I/O: fs.statSync"],
        ),
        (
            "require, defaults",
            "const { fn = () => 1, readFileSync } = require(\"node:fs\");\nfunction f(p) {\n  return readFileSync(p).length;\n}\nmodule.exports = { f };\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "require, nested",
            "const { promises: { readFile } } = require(\"node:fs\");\nfunction f(p) {\n  return readFile(p);\n}\nmodule.exports = { f };\n",
            "f",
            &["does I/O: fs.promises"],
        ),
        (
            "require, .default",
            "const { randomBytes } = require(\"node:crypto\").default;\nfunction f() {\n  return randomBytes(16).toString(\"hex\");\n}\nmodule.exports = { f };\n",
            "f",
            &["not deterministic: crypto.randomBytes"],
        ),
        (
            "require, .default pure",
            "const { isIP } = require(\"node:net\").default;\nfunction f(s) {\n  return isIP(s);\n}\nmodule.exports = { f };\n",
            "f",
            &[],
        ),
        (
            "require in a function keeps the top one",
            "const { readFileSync: run } = require(\"node:fs\");\nfunction helper() {\n  const run = require(\"node:path\").join;\n  return run(\"a\", \"b\");\n}\nfunction f(p) {\n  return run(p, \"utf8\").length;\n}\nmodule.exports = { helper, f };\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "require in a function stays there",
            "const env = { scale: 2 };\nfunction readHome() {\n  const { env } = require(\"node:process\");\n  return env.HOME;\n}\nfunction f(x) {\n  return x * env.scale;\n}\nmodule.exports = { readHome, f };\n",
            "f",
            &[],
        ),
        (
            "compact import",
            "import{readFileSync}from\"node:fs\";\nexport function f(p) {\n  return readFileSync(p).length;\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "type: import = require",
            "import cp = require(\"node:child_process\");\nexport function f(x: number): number {\n  cp.spawnSync(\"true\");\n  return x;\n}\n",
            "f",
            &["does I/O: child_process.spawnSync"],
        ),
        (
            "top-level alias",
            "import fs from \"node:fs\";\nconst { readFileSync } = fs;\nexport function f(p) {\n  return readFileSync(p).length;\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "member named type",
            "import { type as osType } from \"node:os\";\nexport function f() {\n  return osType();\n}\n",
            "f",
            &["does I/O: os.type"],
        ),
        // modules and members
        (
            "console",
            "import { log } from \"node:console\";\nexport function f(x) {\n  log(x);\n  return x * 2;\n}\n",
            "f",
            &["does I/O: console.log"],
        ),
        (
            "pseudoRandomBytes",
            "import { pseudoRandomBytes } from \"node:crypto\";\nexport function f() {\n  return pseudoRandomBytes(16).toString(\"hex\");\n}\n",
            "f",
            &["not deterministic: crypto.pseudoRandomBytes"],
        ),
        (
            "crypto destructured",
            "import * as c from \"node:crypto\";\nexport function f() {\n  const { randomUUID } = c;\n  return randomUUID();\n}\n",
            "f",
            &["not deterministic: crypto"],
        ),
        // names that aren't the import
        (
            "method shorthand",
            "import { env } from \"node:process\";\nexport function f(x) {\n  return { env(v) { return v + 1; } }.env(x);\n}\n",
            "f",
            &[],
        ),
        (
            "function expression param",
            "import * as os from \"node:os\";\nexport function f(items) {\n  return items.map(function (os) {\n    return os.name;\n  });\n}\n",
            "f",
            &[],
        ),
        (
            "nested function param",
            "import { env } from \"node:process\";\nexport function f() {\n  function getK(env) {\n    return env.k;\n  }\n  return getK({ k: 1 });\n}\n",
            "f",
            &[],
        ),
        (
            "second declarator",
            "import { readFileSync } from \"node:fs\";\nexport function f(x) {\n  const a = 1, readFileSync = (v) => v + 1;\n  return readFileSync(x) + a;\n}\n",
            "f",
            &[],
        ),
        (
            "type: optional key",
            "import { env } from \"node:process\";\nexport function f(x: string): string {\n  const cfg: { env?: string } = { env: x };\n  return cfg.env;\n}\n",
            "f",
            &[],
        ),
        // scopes that end where JavaScript ends them
        (
            "for-of loop variable",
            "import { env } from \"node:process\";\nexport function f(items) {\n  for (const env of items) {\n    if (env.stop) break;\n  }\n  return env.HOME;\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "var in a nested function",
            "import { env } from \"node:process\";\nexport function f() {\n  function helper() {\n    var env = { HOME: \"x\" };\n    return env.HOME;\n  }\n  return env.HOME + helper();\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "var in an arrow",
            "import { env } from \"node:process\";\nexport function f(items) {\n  const xs = items.map((x) => { var env = x; return env; });\n  return env.HOME + xs;\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "named function expression",
            "import { readFileSync } from \"node:fs\";\nexport function f(p) {\n  const g = function readFileSync(x) { return x; };\n  return readFileSync(p).length + g(1);\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "arrow without semicolons",
            "import * as os from \"node:os\";\nexport function f() {\n  const getName = (os) => os.name\n  return os.hostname() + getName({ name: 1 })\n}\n",
            "f",
            &["does I/O: os.hostname"],
        ),
        (
            "arrow in a ternary",
            "import * as os from \"node:os\";\nexport function f(x) {\n  return x ? (os) => os.name : os.hostname();\n}\n",
            "f",
            &["does I/O: os.hostname"],
        ),
        (
            "arrow with a ternary body",
            "import * as os from \"node:os\";\nexport function f(items, c) {\n  return items.map((os) => (c ? os.a : os.b));\n}\n",
            "f",
            &[],
        ),
        (
            "call in a ternary",
            "import { readFileSync } from \"node:fs\";\nexport function f(cond, p) {\n  return cond ? JSON.parse(readFileSync(p, \"utf8\")) : { empty: true };\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "type: function type annotation",
            "import { env } from \"node:process\";\nexport function f(): number {\n  const fn: (env: string) => number = () => env.HOME.length;\n  return fn(\"a\");\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "type: arrow return type",
            "import * as os from \"node:os\";\nexport function f(items: Array<{ name: string }>): string[] {\n  return items.map((os): string => os.name);\n}\n",
            "f",
            &[],
        ),
        (
            "type: generic nested function",
            "import { env } from \"node:process\";\nexport function f(): number {\n  function id<T>(env: T): T {\n    return env;\n  }\n  return id(1);\n}\n",
            "f",
            &[],
        ),
        (
            "type: key after a semicolon",
            "import { env } from \"node:process\";\nexport function f(x: string): string {\n  const cfg: { a: number; env?: string } = { a: 1, env: x };\n  return cfg.env ?? \"\";\n}\n",
            "f",
            &[],
        ),
        (
            "type: import type{",
            "import type{ Stats } from \"node:fs\";\nexport function f(x: unknown): boolean {\n  const s = x as Stats;\n  return s.size > 0;\n}\n",
            "f",
            &[],
        ),
        (
            "comparison before a binding",
            "import { env } from \"node:process\";\nexport function f(x) {\n  const ok = x < 10, env = { k: x };\n  return env.k + ok;\n}\n",
            "f",
            &[],
        ),
        (
            "declaration over two lines",
            "import { readFileSync } from \"node:fs\";\nexport function f(x) {\n  const a = x +\n    1, readFileSync = (v) => v;\n  return readFileSync(a);\n}\n",
            "f",
            &[],
        ),
        (
            "property on the next line",
            "import { exec } from \"node:child_process\";\nexport function f(re, s) {\n  return re\n    .exec(s);\n}\n",
            "f",
            &[],
        ),
        (
            "import in a template string",
            "const doc = `\nimport { readFileSync } from \"fs\";\n`;\nfunction readFileSync(x) {\n  return x;\n}\nexport function f(x) {\n  return readFileSync(x) + doc;\n}\n",
            "f",
            &[],
        ),
        // more import forms
        (
            "require, no semicolons",
            "const { a } = getConfig()\nconst { readFileSync } = require(\"node:fs\")\nfunction f(p) {\n  return readFileSync(p, \"utf8\").length\n}\nfunction getConfig() {\n  return { a: 1 }\n}\nmodule.exports = { f, a }\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "require, rest",
            "const { createHash, ...rest } = require(\"node:crypto\");\nfunction f() {\n  return rest.randomInt(10);\n}\nmodule.exports = { f, createHash };\n",
            "f",
            &["not deterministic: crypto.randomInt"],
        ),
        (
            "require, after a regex with a brace",
            "const OPEN = /\\{/;\nconst { readFileSync } = require(\"node:fs\");\nfunction f(p) {\n  return readFileSync(p, \"utf8\").length + OPEN.source.length;\n}\nmodule.exports = { f };\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "require, .default.member pure",
            "const isIP = require(\"node:net\").default.isIP;\nfunction f(s) {\n  return isIP(s) === 4;\n}\nmodule.exports = { f };\n",
            "f",
            &[],
        ),
        (
            "chained alias",
            "import fs from \"node:fs\";\nconst read = fs.promises.readFile;\nexport function f(p) {\n  return read(p);\n}\n",
            "f",
            &["does I/O: fs.promises"],
        ),
        (
            "exported alias",
            "import fs from \"node:fs\";\nexport const { readFileSync } = fs;\nexport function f(p) {\n  return readFileSync(p).length;\n}\n",
            "f",
            &["does I/O: fs.readFileSync"],
        ),
        (
            "type: as const without semicolons",
            "import { writeFileSync } from \"node:fs\";\nexport function f(p: string) {\n  const opts = { encoding: \"utf8\" } as const\n  writeFileSync(p, \"hi\", opts)\n  return 1\n}\n",
            "f",
            &["does I/O: fs.writeFileSync"],
        ),
        (
            "type: a function type's parameter",
            "import { env } from \"node:process\";\nexport function f(): number {\n  const fn: (env: string) => number = (s) => s.length;\n  return fn(\"a\");\n}\n",
            "f",
            &[],
        ),
        (
            "param named document and window",
            "export function f(document, window) {\n  return document.length + window;\n}\n",
            "f",
            &[],
        ),
        (
            "local const named window and fs",
            "export function f(xs) {\n  const window = 4, fs = 44100;\n  return xs.length + window + fs;\n}\n",
            "f",
            &[],
        ),
        (
            "property access named process and fetch",
            "export function f(row) {\n  return row.process + row.fetch + row.document;\n}\n",
            "f",
            &[],
        ),
        (
            "object key named document and window",
            "export function f(x) {\n  return { document: x, window: x + 1 };\n}\n",
            "f",
            &[],
        ),
        (
            "method shorthand named fetch and process",
            "export function f(x) {\n  return { fetch(v) { return v + 1; }, process(v) { return v * 2; } }.fetch(x);\n}\n",
            "f",
            &[],
        ),
        (
            "crypto.createHash through default crypto import",
            "import crypto from \"node:crypto\";\nexport function f(s) {\n  return crypto.createHash(\"sha256\").update(s).digest(\"hex\");\n}\n",
            "f",
            &[],
        ),
        (
            "crypto.randomUUID through default crypto import still blocks",
            "import crypto from \"node:crypto\";\nexport function f() {\n  return crypto.randomUUID();\n}\n",
            "f",
            &["not deterministic: crypto.randomUUID"],
        ),
        (
            "global crypto.getRandomValues still blocks",
            "export function f(buf) {\n  return crypto.getRandomValues(buf);\n}\n",
            "f",
            &["not deterministic: crypto.getRandomValues"],
        ),
        (
            "fs.constants through default fs import is pure",
            "import fs from \"node:fs\";\nexport function f(mode) {\n  return (mode & fs.constants.R_OK) !== 0;\n}\n",
            "f",
            &[],
        ),
        (
            "globalThis.fetch still blocks",
            "export function f(url) {\n  return globalThis.fetch(url);\n}\n",
            "f",
            &["does I/O: fetch"],
        ),
        (
            "shadowed in nested arrow, real global outside still blocks",
            "export function f(xs) {\n  const a = xs.map((window) => window + 1);\n  return a.length + process.pid;\n}\n",
            "f",
            &["does I/O: process"],
        ),
        ("param named Date shadows new Date", "export function f(Date) {\n  return new Date(1);\n}\n", "f", &[]),
        ("property import does not block", "export function f(obj) {\n  return obj.import(1);\n}\n", "f", &[]),
        (
            "method shorthand named import does not block",
            "export function f(x) {\n  return { import(v) { return v + 1; } }.import(x);\n}\n",
            "f",
            &[],
        ),
        (
            "dollar-prefixed $import is not dynamic import",
            "export function f(p) {\n  return $import(p);\n}\n",
            "f",
            &[],
        ),
        (
            "real dynamic import still blocks",
            "export function f(p) {\n  return import(p);\n}\n",
            "f",
            &["does I/O: import("],
        ),
        (
            "spread of process.env still blocks",
            "export function f() {\n  return { ...process.env };\n}\n",
            "f",
            &["does I/O: process"],
        ),
        (
            "spread of crypto.getRandomValues still blocks",
            "export function f(a) {\n  return [...crypto.getRandomValues(a)];\n}\n",
            "f",
            &["not deterministic: crypto.getRandomValues"],
        ),
        (
            "spread of fs.readdirSync still blocks",
            "import fs from \"node:fs\";\nexport function f() {\n  return [...fs.readdirSync(\".\")];\n}\n",
            "f",
            &["does I/O: fs"],
        ),
        (
            "spread of imported env still blocks",
            "import { env } from \"node:process\";\nexport function f() {\n  return { ...env };\n}\n",
            "f",
            &["does I/O: process.env"],
        ),
        (
            "optional chain globalThis?.fetch still blocks",
            "export function f(url) {\n  return globalThis?.fetch(url);\n}\n",
            "f",
            &["does I/O: fetch"],
        ),
        (
            "optional chain global?.process still blocks",
            "export function f() {\n  return global?.process.pid;\n}\n",
            "f",
            &["does I/O: process"],
        ),
        (
            "optional chain globalThis?.crypto.randomUUID still blocks",
            "export function f() {\n  return globalThis?.crypto.randomUUID();\n}\n",
            "f",
            &["not deterministic: crypto.randomUUID"],
        ),
        (
            "globalThis.fetch bypasses param named fetch",
            "export function f(fetch, url) {\n  return globalThis.fetch(url) + fetch;\n}\n",
            "f",
            &["does I/O: fetch"],
        ),
        (
            "globalThis.fetch bypasses local const named fetch",
            "export function f(url) {\n  const fetch = 1;\n  return globalThis.fetch(url) + fetch;\n}\n",
            "f",
            &["does I/O: fetch"],
        ),
        (
            "globalThis.fetch bypasses pure import aliased as fetch",
            "import { join as fetch } from \"node:path\";\nexport function f(url) {\n  return globalThis.fetch(url);\n}\n",
            "f",
            &["does I/O: fetch"],
        ),
        (
            "globalThis.crypto.randomUUID bypasses param named crypto",
            "export function f(crypto) {\n  return globalThis.crypto.randomUUID();\n}\n",
            "f",
            &["not deterministic: crypto.randomUUID"],
        ),
        (
            "multiline property x.globalThis.fetch is not global",
            "export function f(x) {\n  return x.\n    globalThis.fetch(1);\n}\n",
            "f",
            &[],
        ),
        (
            "pure module imported as fs is not I/O",
            "import fs from \"node:path\";\nexport function f(a, b) {\n  return fs.join(a, b);\n}\n",
            "f",
            &[],
        ),
        (
            "node:crypto imported as fs reports crypto.randomUUID",
            "import fs from \"node:crypto\";\nexport function f() {\n  return fs.randomUUID();\n}\n",
            "f",
            &["not deterministic: crypto.randomUUID"],
        ),
        (
            "non-builtin package imported as crypto is not nondet",
            "import crypto from \"my-pure-lib\";\nexport function f(x) {\n  return crypto.sign(x);\n}\n",
            "f",
            &[],
        ),
        (
            "ASI after obj.const does not shadow next-line fetch",
            "export function f(obj, url) {\n  const a = obj.const\n  fetch(url)\n  return a\n}\n",
            "f",
            &["does I/O: fetch"],
        ),
        (
            "mixed fs.constants and fs.readFileSync still blocks",
            "import fs from \"node:fs\";\nexport function f(p) {\n  return fs.constants.R_OK + fs.readFileSync(p).length;\n}\n",
            "f",
            &["does I/O: fs"],
        ),
        (
            "mixed crypto.createHash and crypto.randomUUID still blocks",
            "import crypto from \"node:crypto\";\nexport function f(s) {\n  return crypto.createHash(\"sha256\").update(s).digest(\"hex\") + crypto.randomUUID();\n}\n",
            "f",
            &["not deterministic: crypto.randomUUID"],
        ),
    ];

    #[test]
    fn scan_sees_io_reached_through_an_import() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        // the profile step describes the 40 hottest functions: 30 cases a run
        for (chunk, cases) in IMPORT_CASES.chunks(30).enumerate() {
            let dir = std::env::temp_dir().join(format!("fa-node-imports-{}-{chunk}", std::process::id()));
            let prof = dir.join("cpuprof");
            fs::create_dir_all(&prof).unwrap();
            // a made-up profile with one sample in each case's function
            let root = json!({ "functionName": "(root)", "url": "", "lineNumber": -1, "columnNumber": -1 });
            let mut nodes = vec![json!({ "id": 1, "callFrame": root, "children": [] })];
            for (i, (name, src, func, _)) in cases.iter().enumerate() {
                let ext = if name.starts_with("require") {
                    "cjs"
                } else if name.starts_with("type") {
                    "ts"
                } else {
                    "mjs"
                };
                let file = dir.join(format!("case{i}.{ext}"));
                fs::write(&file, src).unwrap();
                let line = src.lines().position(|l| l.contains(&format!("function {func}("))).unwrap();
                let url = format!("file://{}", file.display());
                let frame = json!({ "functionName": func, "url": url, "lineNumber": line, "columnNumber": 0 });
                nodes.push(json!({ "id": i + 2, "callFrame": frame, "children": [] }));
            }
            let ids: Vec<usize> = (2..cases.len() + 2).collect();
            nodes[0]["children"] = json!(ids);
            let deltas = vec![1000; ids.len()];
            let profile =
                json!({ "nodes": nodes, "startTime": 0, "endTime": 1_000_000, "samples": ids, "timeDeltas": deltas });
            fs::write(prof.join("a.cpuprofile"), profile.to_string()).unwrap();
            fs::write(dir.join("runner.mjs"), RUNNER).unwrap();
            let result = dir.join("profile.json");
            let job = json!({ "root": dir, "dir": prof, "entry": dir.join("main.mjs"), "out": result });
            fs::write(dir.join("job.json"), job.to_string()).unwrap();
            let out =
                Command::new("node").args(["runner.mjs", "profile", "job.json"]).current_dir(&dir).output().unwrap();
            let result = fs::read_to_string(&result).unwrap_or_default();
            let _ = fs::remove_dir_all(&dir);
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let result: Value = serde_json::from_str(&result).unwrap();
            let functions = result["functions"].as_array().unwrap();
            for (i, (name, _, _, expected)) in cases.iter().enumerate() {
                let prefix = format!("case{i}.");
                let f = functions
                    .iter()
                    .find(|f| f["file"].as_str().is_some_and(|p| p.starts_with(&prefix)))
                    .unwrap_or_else(|| panic!("{name}: not described"));
                let got: Vec<&str> = f["blockers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|b| b.starts_with("does I/O") || b.starts_with("not deterministic"))
                    .collect();
                assert_eq!(got, *expected, "{name}");
            }
        }
    }

    #[test]
    fn maps_typescript() {
        assert_eq!(rust_type("number").as_deref(), Some("f64"));
        assert_eq!(rust_type("string[]").as_deref(), Some("Vec<String>"));
        assert_eq!(rust_type("Array<number>").as_deref(), Some("Vec<f64>"));
        assert_eq!(rust_type("readonly number[]").as_deref(), Some("Vec<f64>"));
        assert_eq!(rust_type("number | undefined").as_deref(), Some("Option<f64>"));
        assert_eq!(rust_type("string | null").as_deref(), Some("Option<String>"));
        assert_eq!(rust_type("Float64Array").as_deref(), Some("Float64Array"));
        assert_eq!(rust_type("Uint8ClampedArray").as_deref(), Some("Uint8ClampedArray"));
        assert_eq!(rust_type("BigInt64Array"), None);
        assert_eq!(rust_type("Buffer").as_deref(), Some("Buffer"));
        assert_eq!(rust_type("Set<string>").as_deref(), Some("std::collections::HashSet<String>"));
        assert_eq!(rust_type("Set<number>"), None);
        assert_eq!(rust_type("Map<string, number>"), None);
        assert_eq!(rust_type("Record<string, number>").as_deref(), Some("std::collections::HashMap<String, f64>"));
        assert_eq!(rust_type("string | number"), None);
        assert_eq!(rust_type("[number, number][]").as_deref(), Some("Vec<Vec<f64>>"));
        assert_eq!(rust_type("[number, string]"), None);
        assert_eq!(rust_type("Point"), None);
    }

    #[test]
    fn finds_the_script() {
        let args: Vec<String> =
            ["--require", "x.cjs", "--no-warnings", "app.mjs", "data.txt"].map(String::from).to_vec();
        assert_eq!(entry(&args).map(String::as_str), Some("app.mjs"));
        assert!(is_node("node") && is_node("/usr/bin/node") && is_node("C:\\nodejs\\node.exe") && !is_node("python"));
        assert_eq!(snake("editDistance"), "edit_distance");
        assert_eq!(snake("type"), "r#type");
    }

    #[test]
    fn entry_script_blocks_go_before_the_main_block() {
        let app = "function f(x) {\n  return x;\n}\n\nif (import.meta.main) {\n  console.log(f(1));\n}\n";
        let on = add_marked(app, "app.mjs:f", "f = g;", true);
        let (block, main) = (on.find("f = g;").unwrap(), on.find("if (import.meta.main)").unwrap());
        assert!(block < main, "{on}");
        assert_eq!(backend::strip_marked(&on, "//", "app.mjs:f"), app);
        let cjs = "function f(x) {\n  return x;\n}\nif (require.main === module) main();\n";
        let on = add_marked(cjs, "app.js:f", "f = g;", true);
        assert!(on.find("f = g;").unwrap() < on.find("if (require.main").unwrap(), "{on}");
        assert!(add_marked(app, "m.f", "f = g;", false).trim_end().ends_with("// <<< fast-anything: m.f <<<"));
    }

    #[test]
    fn const_functions_switch_to_let_and_back() {
        let port = Port {
            id: "lib.pairs".into(),
            file: "lib.ts".into(),
            module: "lib".into(),
            func: "pairs".into(),
            ext: "fa_lib_pairs".into(),
            crate_dir: ".fast/ports/lib.pairs".into(),
            recorded: 1,
            calls_seen: 1,
            verify: None,
            enabled: false,
            lang: backend::Lang::Node,
            owner: None,
            line: 1,
            sources: vec![],
            backend: crate::backend::Backend::Rust,
        };
        for original in [
            "export const pairs = (names: string[]): number => {\n  return 0;\n};\n",
            "const pairs = function (names) {\n  return 0;\n};\nmodule.exports = { pairs };\n",
            "const pairsCount = 1;\nexport  const\tpairs = (n) => n;\r\n",
        ] {
            let on = with_shim(original, &port, "pairs = rust.pairs;");
            assert!(declaration(&on, "let", "pairs").is_some() && declaration(&on, "const", "pairs").is_none(), "{on}");
            assert_eq!(on.contains("const pairsCount"), original.contains("const pairsCount"), "{on}");
            assert_eq!(with_shim(&on, &port, "pairs = rust.pairs;"), on, "enabling twice changes nothing");
            assert_eq!(without_shim(&on, &port), original);
        }
        // a function that was declared with let stays let
        let original = "let pairs = (n) => n;\n";
        let on = with_shim(original, &port, "pairs = rust.pairs;");
        assert!(!on.contains("puts the const back"));
        assert_eq!(without_shim(&on, &port), original);
    }

    #[test]
    fn static_methods_go_through_their_class() {
        let port = Port {
            id: "lib.Text.pairs".into(),
            file: "lib.mjs".into(),
            module: "lib".into(),
            func: "pairs".into(),
            ext: "fa_lib_text_pairs".into(),
            crate_dir: ".fast/ports/lib.Text.pairs".into(),
            recorded: 1,
            calls_seen: 1,
            verify: None,
            enabled: false,
            lang: backend::Lang::Node,
            owner: Some("Text".into()),
            line: 2,
            sources: vec![],
            backend: crate::backend::Backend::Rust,
        };
        let block = shim(&port, "./x.node", true);
        assert!(block.contains("Text.pairs = process.getBuiltinModule"), "{block}");
        assert!(block.contains("(\"./x.node\").pairs;"), "{block}");
        // a same-named const elsewhere is left alone
        let original = "const pairs = 1;\nexport class Text {\n  static pairs(n) {\n    return n;\n  }\n}\n";
        let on = with_shim(original, &port, &block);
        assert!(on.starts_with("const pairs = 1;"), "{on}");
        assert_eq!(without_shim(&on, &port), original);
    }

    #[test]
    fn shim_loads_the_addon() {
        let port = Port {
            id: "fuzzy.levenshtein".into(),
            file: "fuzzy.mjs".into(),
            module: "fuzzy".into(),
            func: "levenshtein".into(),
            ext: "fa_fuzzy_levenshtein".into(),
            crate_dir: ".fast/ports/fuzzy.levenshtein".into(),
            recorded: 1,
            calls_seen: 1,
            verify: None,
            enabled: false,
            lang: backend::Lang::Node,
            owner: None,
            line: 1,
            sources: vec![],
            backend: crate::backend::Backend::Rust,
        };
        let esm = shim(&port, "./.fast/lib/fa_fuzzy_levenshtein.node", true);
        assert!(esm.contains("createRequire(import.meta.url)(\"./.fast/lib/fa_fuzzy_levenshtein.node\").levenshtein"));
        assert!(!esm.contains("module.exports"), "ESM shim must stay untouched: {esm}");
        assert!(shim(&port, "./x.node", false).contains("levenshtein = require(\"./x.node\").levenshtein;"));
        let original = "export function levenshtein(a, b) {\n  return 0;\n}\n";
        let on = backend::append_marked(original, "//", &port.id, &esm);
        assert_eq!(backend::strip_marked(&on, "//", &port.id), original);
    }

    #[test]
    fn cjs_exports_are_recorded() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let cases = [
            (
                "object",
                "function sq(x) {\n  return x * x;\n}\nmodule.exports = { sq };\n",
                "const { sq } = require(\"./lib.js\");\nconsole.log(sq(2) + sq(3) + sq(4));\n",
                3u64,
            ),
            (
                "default",
                "function sq(x) {\n  return x * x;\n}\nmodule.exports = sq;\n",
                "const sq = require(\"./lib.js\");\nconsole.log(sq(2) + sq(3));\n",
                2u64,
            ),
            (
                "named",
                "function sq(x) {\n  return x * x;\n}\nexports.sq = sq;\n",
                "const { sq } = require(\"./lib.js\");\nconsole.log(sq(5));\n",
                1u64,
            ),
            (
                "alias-and-const",
                "const sq = (x) => x * x;\nconst other = (x) => x + 1;\nmodule.exports = { sq, square: sq, other };\n",
                "const { sq, square, other } = require(\"./lib.js\");\nif (other(3) !== 4) process.exit(2);\nconsole.log(sq(2) + square(3));\n",
                2u64,
            ),
            (
                "default-with-props",
                "function sq(x) {\n  return x * x;\n}\nsq.helper = (x) => x + 1;\nsq.VERSION = \"1.0\";\nsq.default = sq;\nmodule.exports = sq;\n",
                "const sq = require(\"./lib.js\");\nif (sq.helper(3) !== 4 || sq.VERSION !== \"1.0\") process.exit(2);\nconsole.log(sq(2) + sq.default(3));\n",
                2u64,
            ),
            (
                "function-container",
                "function sq(x) {\n  return x * x;\n}\nfunction container() { return 0; }\ncontainer.sq = sq;\nmodule.exports = container;\n",
                "const c = require(\"./lib.js\");\nconsole.log(c.sq(2) + c.sq(3));\n",
                2u64,
            ),
            (
                "getter-and-readonly",
                "\"use strict\";\nfunction sq(x) {\n  return x * x;\n}\nconst ex = {};\nObject.defineProperty(ex, \"ro\", { value: sq, enumerable: true, writable: false });\nObject.defineProperty(ex, \"boom\", { enumerable: true, get() { throw new Error(\"getter ran\"); } });\nObject.defineProperty(ex, \"live\", { enumerable: true, get: () => sq });\nex.sq = sq;\nmodule.exports = ex;\n",
                "const { sq, live } = require(\"./lib.js\");\nconsole.log(sq(4) + live(5));\n",
                2u64,
            ),
            (
                "readonly-module-exports",
                "\"use strict\";\nfunction sq(x) {\n  return x * x;\n}\nObject.defineProperty(module, \"exports\", { value: sq, writable: false, configurable: false });\n",
                "const sq = require(\"./lib.js\");\nconsole.log(sq(3));\n",
                0u64,
            ),
            (
                "null-exports",
                "function sq(x) {\n  return x * x;\n}\nsq(2);\nmodule.exports = null;\n",
                "require(\"./lib.js\");\n",
                0u64,
            ),
            (
                "auto-esm-module-null",
                "const module = null;\nexport function sq(x) {\n  return x * x;\n}\n",
                "import { sq } from \"./lib.js\";\nconsole.log(sq(3));\n",
                1u64,
            ),
        ];
        let base = std::env::temp_dir().join(format!("fa-node-cjs-rec-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        for (name, lib_src, app_src, expected_calls) in cases {
            let dir = base.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("lib.js"), lib_src).unwrap();
            fs::write(dir.join("app.js"), app_src).unwrap();
            let ctx = Ctx::open(&dir).unwrap();
            let hs = Hotspot {
                id: "lib.sq".into(),
                file: "lib.js".into(),
                name: "sq".into(),
                qualname: "sq".into(),
                module: Some("lib".into()),
                ..Default::default()
            };
            let out = dir.join("rec.jsonl");
            let rec = record(&ctx, "node", &[String::from("app.js")], &hs, &out, 500);
            let restored = fs::read_to_string(dir.join("lib.js")).unwrap();
            let _ = fs::remove_dir_all(&dir);
            let rec = rec.unwrap_or_else(|e| panic!("{name}: record failed: {e:#}"));
            assert_eq!(rec.calls, expected_calls, "{name}: wrong call count");
            assert_eq!(rec.kept, expected_calls as usize, "{name}: wrong kept count");
            assert_eq!(restored, lib_src, "{name}: lib.js must be restored byte for byte after record");
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn cjs_exports_are_enabled_and_restored() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let cases = [
            (
                "object",
                "function sq(x) {\n  return x * x;\n}\nmodule.exports = { sq };\n",
                "const { sq } = require(\"./lib.js\");\nconsole.log(sq(3));\n",
                "1009",
                "9",
            ),
            (
                "default",
                "function sq(x) {\n  return x * x;\n}\nmodule.exports = sq;\n",
                "const sq = require(\"./lib.js\");\nconsole.log(sq(3));\n",
                "1009",
                "9",
            ),
            (
                "named",
                "function sq(x) {\n  return x * x;\n}\nexports.sq = sq;\n",
                "const { sq } = require(\"./lib.js\");\nconsole.log(sq(3));\n",
                "1009",
                "9",
            ),
            (
                "alias-crlf-const",
                "const sq = (x) => x * x;\r\nconst other = (x) => x + 1;\r\nmodule.exports = { sq, square: sq, other };\r\n",
                "const { sq, square, other } = require(\"./lib.js\");\nconsole.log(`${sq(3)},${square(4)},${other(3)}`);\n",
                "1009,1016,4",
                "9,16,4",
            ),
            (
                "default-with-props",
                "function sq(x) {\n  return x * x;\n}\nsq.helper = (x) => x + 1;\nsq.VERSION = \"1.0\";\nsq.default = sq;\nmodule.exports = sq;\n",
                "const sq = require(\"./lib.js\");\nconsole.log(`${sq(3)},${sq.default(4)},${sq.helper(3)},${sq.VERSION}`);\n",
                "1009,1016,4,1.0",
                "9,16,4,1.0",
            ),
            (
                "function-container",
                "function sq(x) {\n  return x * x;\n}\nfunction container() { return 0; }\ncontainer.sq = sq;\nmodule.exports = container;\n",
                "const c = require(\"./lib.js\");\nconsole.log(c.sq(3));\n",
                "1009",
                "9",
            ),
            (
                "getter-and-readonly",
                "\"use strict\";\nfunction sq(x) {\n  return x * x;\n}\nconst ex = {};\nObject.defineProperty(ex, \"ro\", { value: sq, enumerable: true, writable: false });\nObject.defineProperty(ex, \"boom\", { enumerable: true, get() { throw new Error(\"getter ran\"); } });\nObject.defineProperty(ex, \"live\", { enumerable: true, get: () => sq });\nex.sq = sq;\nmodule.exports = ex;\n",
                "const { sq, live } = require(\"./lib.js\");\nconsole.log(`${sq(3)},${live(4)}`);\n",
                "1009,1016",
                "9,16",
            ),
        ];
        let base = std::env::temp_dir().join(format!("fa-node-cjs-en-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        for (name, lib_src, app_src, want_rust, want_orig) in cases {
            let dir = base.join(name);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("fake_addon.cjs"), "module.exports = { sq: (x) => x * x + 1000 };\n").unwrap();
            fs::write(dir.join("app.js"), app_src).unwrap();
            let port = Port {
                id: "lib.sq".into(),
                file: "lib.js".into(),
                module: "lib".into(),
                func: "sq".into(),
                ext: "fa_lib_sq".into(),
                crate_dir: ".fast/ports/lib.sq".into(),
                recorded: 1,
                calls_seen: 1,
                verify: None,
                enabled: false,
                lang: backend::Lang::Node,
                owner: None,
                line: 1,
                sources: vec![],
                backend: crate::backend::Backend::Rust,
            };
            let enabled = with_shim(lib_src, &port, &shim(&port, "./fake_addon.cjs", false));
            fs::write(dir.join("lib.js"), &enabled).unwrap();
            let rust_run =
                Command::new("node").arg("app.js").current_dir(&dir).env_remove(DISABLE_ENV).output().unwrap();
            let orig_run = Command::new("node").arg("app.js").current_dir(&dir).env(DISABLE_ENV, "1").output().unwrap();
            let restored = without_shim(&enabled, &port);

            // Also verify real enable()/disable() round-trip on disk.
            fs::write(dir.join("lib.js"), lib_src).unwrap();
            let ctx = Ctx::open(&dir).unwrap();
            enable(&ctx, &port).unwrap();
            let on_disk = fs::read_to_string(dir.join("lib.js")).unwrap();
            disable(&ctx, &port).unwrap();
            let back_on_disk = fs::read_to_string(dir.join("lib.js")).unwrap();
            let _ = fs::remove_dir_all(&dir);

            assert!(rust_run.status.success(), "{name}: enabled run failed: {}", combined(&rust_run));
            assert_eq!(String::from_utf8_lossy(&rust_run.stdout).trim(), want_rust, "{name}: enabled output");
            assert!(orig_run.status.success(), "{name}: disabled run failed: {}", combined(&orig_run));
            assert_eq!(String::from_utf8_lossy(&orig_run.stdout).trim(), want_orig, "{name}: disabled output");
            assert_eq!(restored, lib_src, "{name}: without_shim must restore byte for byte");
            assert!(on_disk.contains("module.exports"), "{name}: CJS enable should include exports sync");
            assert_eq!(back_on_disk, lib_src, "{name}: disable must restore byte for byte");
        }

        // ESM files keep the original ESM path unchanged and restore byte for byte.
        let esm_dir = base.join("esm");
        fs::create_dir_all(&esm_dir).unwrap();
        let esm_src = "export function sq(x) {\n  return x * x;\n}\n";
        fs::write(esm_dir.join("lib.mjs"), esm_src).unwrap();
        let ctx = Ctx::open(&esm_dir).unwrap();
        let esm_port = Port {
            id: "lib.sq".into(),
            file: "lib.mjs".into(),
            module: "lib".into(),
            func: "sq".into(),
            ext: "fa_lib_sq".into(),
            crate_dir: ".fast/ports/lib.sq".into(),
            recorded: 1,
            calls_seen: 1,
            verify: None,
            enabled: false,
            lang: backend::Lang::Node,
            owner: None,
            line: 1,
            sources: vec![],
            backend: crate::backend::Backend::Rust,
        };
        enable(&ctx, &esm_port).unwrap();
        let esm_on = fs::read_to_string(esm_dir.join("lib.mjs")).unwrap();
        disable(&ctx, &esm_port).unwrap();
        let esm_off = fs::read_to_string(esm_dir.join("lib.mjs")).unwrap();
        assert!(!esm_on.contains("module.exports"), "ESM enable must not add CJS sync: {esm_on}");
        assert_eq!(esm_off, esm_src, "ESM disable must restore byte for byte");

        // Verify two const functions in the same CJS file don't collide on _faOrig and restore byte for byte.
        let two_dir = base.join("two");
        fs::create_dir_all(&two_dir).unwrap();
        let two_src = "const a = (x) => x + 1;\nconst b = (x) => x + 2;\nmodule.exports = { a, b };\n";
        fs::write(two_dir.join("fake_addon.cjs"), "module.exports = { a: (x) => x + 100, b: (x) => x + 200 };\n")
            .unwrap();
        fs::write(two_dir.join("app.js"), "const { a, b } = require(\"./lib.js\");\nconsole.log(`${a(1)},${b(1)}`);\n")
            .unwrap();
        let mk_port = |func: &str| Port {
            id: format!("lib.{func}"),
            file: "lib.js".into(),
            module: "lib".into(),
            func: func.into(),
            ext: format!("fa_lib_{func}"),
            crate_dir: format!(".fast/ports/lib.{func}"),
            recorded: 1,
            calls_seen: 1,
            verify: None,
            enabled: false,
            lang: backend::Lang::Node,
            owner: None,
            line: 1,
            sources: vec![],
            backend: crate::backend::Backend::Rust,
        };
        let (pa, pb) = (mk_port("a"), mk_port("b"));
        let on_a = with_shim(two_src, &pa, &shim(&pa, "./fake_addon.cjs", false));
        let on_ab = with_shim(&on_a, &pb, &shim(&pb, "./fake_addon.cjs", false));
        fs::write(two_dir.join("lib.js"), &on_ab).unwrap();
        let two_run =
            Command::new("node").arg("app.js").current_dir(&two_dir).env_remove(DISABLE_ENV).output().unwrap();
        assert!(two_run.status.success(), "two-port run failed: {}", combined(&two_run));
        assert_eq!(String::from_utf8_lossy(&two_run.stdout).trim(), "101,201");
        assert_eq!(without_shim(&without_shim(&on_ab, &pa), &pb), two_src);

        // Verify CJS record_wrap is a safe no-op when __fastAnythingWrap is unset, and ESM record_wrap has no CJS sync.
        let nowrap_dir = base.join("nowrap");
        fs::create_dir_all(&nowrap_dir).unwrap();
        let nw_src = "function sq(x) {\n  return x * x;\n}\nmodule.exports = { sq };\n";
        let nw_wrapped = add_marked(nw_src, "recording lib.sq", &record_wrap("sq", false), false);
        fs::write(nowrap_dir.join("lib.js"), &nw_wrapped).unwrap();
        fs::write(nowrap_dir.join("app.js"), "const { sq } = require(\"./lib.js\");\nconsole.log(sq(4));\n").unwrap();
        let nw_run = Command::new("node").arg("app.js").current_dir(&nowrap_dir).output().unwrap();
        assert!(nw_run.status.success(), "nowrap failed: {}", combined(&nw_run));
        assert_eq!(String::from_utf8_lossy(&nw_run.stdout).trim(), "16");
        assert!(!record_wrap("sq", true).contains("module.exports"));

        // Verify static method exported via class and aliased property in CJS.
        let st_dir = base.join("static");
        fs::create_dir_all(&st_dir).unwrap();
        let st_src =
            "class Calc {\n  static sq(x) {\n    return x * x;\n  }\n}\nmodule.exports = { Calc, sq: Calc.sq };\n";
        fs::write(st_dir.join("fake_addon.cjs"), "module.exports = { sq: (x) => x * x + 1000 };\n").unwrap();
        fs::write(
            st_dir.join("app.js"),
            "const { Calc, sq } = require(\"./lib.js\");\nconsole.log(`${Calc.sq(3)},${sq(4)}`);\n",
        )
        .unwrap();
        let st_port = Port { owner: Some("Calc".into()), id: "lib.Calc.sq".into(), ..mk_port("sq") };
        let st_on = with_shim(st_src, &st_port, &shim(&st_port, "./fake_addon.cjs", false));
        fs::write(st_dir.join("lib.js"), &st_on).unwrap();
        let st_run = Command::new("node").arg("app.js").current_dir(&st_dir).env_remove(DISABLE_ENV).output().unwrap();
        assert!(st_run.status.success(), "static CJS run failed: {}", combined(&st_run));
        assert_eq!(String::from_utf8_lossy(&st_run.stdout).trim(), "1009,1016");
        assert_eq!(without_shim(&st_on, &st_port), st_src);

        // Verify is_esm_file across runtimes and extensions.
        assert!(!is_esm_file(&base, &base.join("a.js"), Runtime::Node));
        assert!(is_esm_file(&base, &base.join("a.mjs"), Runtime::Node));
        assert!(is_esm_file(&base, &base.join("a.js"), Runtime::Deno));
        assert!(is_esm_file(&base, &base.join("a.ts"), Runtime::Deno));
        assert!(!is_esm_file(&base, &base.join("a.cjs"), Runtime::Deno));
        assert!(!is_esm_file(&base, &base.join("a.cts"), Runtime::Deno));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn auto_detected_esm_without_package_type_enables_and_loads() {
        // Syntax-level checks for has_esm_syntax.
        assert!(has_esm_syntax("export function f(x) { return x; }\n"));
        assert!(has_esm_syntax("const x = 1\nexport const f = (n) => n + x\n"));
        assert!(has_esm_syntax("import { a } from './a.js';\nfunction f() { return a; }\n"));
        assert!(has_esm_syntax("import './side-effect.js';\n"));
        assert!(has_esm_syntax("import type from './mod.js';\n"));
        assert!(has_esm_syntax("import type, { a } from './mod.js';\n"));
        assert!(has_esm_syntax("const re = /'/;\nexport default function f(x) { return x; }\n"));
        assert!(has_esm_syntax("function f() { return import.meta.url; }\n"));
        assert!(has_esm_syntax("\u{feff}#!/usr/bin/env node\nexport function f() {}\n"));
        assert!(has_esm_syntax("const s = `a ${1 + 2} b`;\nexport const f = () => s;\n"));
        assert!(has_esm_syntax("let i = 4;\nconst a = i++ / 2; // don't break\nexport function f(x) { return x; }\n"));
        assert!(has_esm_syntax("let i = 4;\nconst a = i-- / 2; // '\nexport function f(x) { return x; }\n"));
        assert!(has_esm_syntax("const a = { valueOf() { return 4; } } / 2; // '\nexport function f() {}\n"));
        assert!(has_esm_syntax("const a = m.get('a')! / 2; // '\nexport function f() {}\n"));
        assert!(has_esm_syntax("if (ok) /'/.test(s);\nexport function f() {}\n"));
        assert!(has_esm_syntax("const x = 1 /*\n*/ export function f(n) { return n; }\n"));
        assert!(has_esm_syntax("\u{00a0}export function f(x) { return x; }\n"));
        assert!(has_esm_syntax("const x = 1\u{2028}export function f(x) { return x; }\n"));
        assert!(!has_esm_syntax("function f(x) { return x; }\nmodule.exports = { f };\n"));
        assert!(!has_esm_syntax("exports.f = function(x) { return x; };\n"));
        assert!(!has_esm_syntax("// export function f() {}\n/* import x from 'y'; */\nmodule.exports = {};\n"));
        assert!(!has_esm_syntax("const s = \"export function f() {}\";\nconst t = `import.meta.url`;\n"));
        assert!(!has_esm_syntax("import('./x.js');\nmodule.exports = {};\n"));
        assert!(!has_esm_syntax("async function f() { return import('./x.js'); }\nmodule.exports = { f };\n"));
        assert!(!has_esm_syntax(
            "import type { Stats } from 'node:fs';\nexport type T = number;\nmodule.exports = {};\n"
        ));
        assert!(!has_esm_syntax("import type from from './types.ts';\nmodule.exports = {};\n"));
        assert!(!has_esm_syntax("import fs = require('node:fs');\nexport interface I { x: number }\n"));
        assert!(!has_esm_syntax("const obj = { export: 1, import: 2 };\nconsole.log(obj.import.meta);\n"));
        assert!(!has_esm_syntax(
            "let i = 4;\nconst a = i++ / 2; // '\nconst s = \"'; export function fake() {}\";\nmodule.exports = { a, s };\n"
        ));
        assert!(!has_esm_syntax(
            "const a = { valueOf() { return 4; } } / 2; // '\nconst s = \"'; export function fake() {}\";\nmodule.exports = { a, s };\n"
        ));
        assert!(!has_esm_syntax(
            "if (ok) /'/.test(s);\nconst t = \"'; export function fake() {}\";\nmodule.exports = { t };\n"
        ));
        // Unterminated `/` on a long line must stay linear rather than quadratic.
        let pathological = format!("{}\nconst ok = 1;\n", "x = /[".repeat(20_000));
        assert!(!has_esm_syntax(&pathological));

        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let tmp = std::env::temp_dir();
        let tmp = if cfg!(windows) { tmp } else { tmp.canonicalize().unwrap() };
        let base = tmp.join(format!("fa-node-auto-esm-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);

        // 1. Auto-detected ESM in .ts (no package.json) and .js (package.json without "type"):
        //    enable() must emit ESM createRequire(import.meta.url) so Node loads the addon instead of
        //    silently swallowing ReferenceError: require is not defined.
        for (name, ext, pkg, lib_src, app_src) in [
            (
                "ts-no-pkg",
                "ts",
                None,
                "export function sq(x: number): number {\n  return x * x;\n}\n",
                "import { sq } from \"./lib.ts\";\nconsole.log(sq(3));\n",
            ),
            (
                "js-pkg-no-type",
                "js",
                Some("{\"name\": \"demo\"}\n"),
                "const module = null;\nexport function sq(x) {\n  return x * x;\n}\n",
                "import { sq } from \"./lib.js\";\nconsole.log(sq(3));\n",
            ),
            (
                "js-no-semicolons",
                "js",
                None,
                "const offset = 0\nexport const sq = (x) => x * x + offset\n",
                "import { sq } from \"./lib.js\"\nconsole.log(sq(3))\n",
            ),
        ] {
            let dir = base.join(name);
            fs::create_dir_all(dir.join(".fast/lib")).unwrap();
            if let Some(pkg) = pkg {
                fs::write(dir.join("package.json"), pkg).unwrap();
            }
            let lib_name = format!("lib.{ext}");
            let app_name = format!("app.{ext}");
            fs::write(dir.join(&lib_name), lib_src).unwrap();
            fs::write(dir.join(&app_name), app_src).unwrap();
            fs::write(dir.join("fake_addon.cjs"), "module.exports = { sq: (x) => x * x + 1000 };\n").unwrap();
            let port = Port {
                id: "lib.sq".into(),
                file: lib_name.clone(),
                module: "lib".into(),
                func: "sq".into(),
                ext: "fa_lib_sq".into(),
                crate_dir: ".fast/ports/lib.sq".into(),
                recorded: 1,
                calls_seen: 1,
                verify: None,
                enabled: false,
                lang: backend::Lang::Node,
                owner: None,
                line: 1,
                sources: vec![],
                backend: crate::backend::Backend::Rust,
            };
            let ctx = Ctx::open(&dir).unwrap();
            assert!(is_esm(&dir, &dir.join(&lib_name)), "{name}: expected auto-detected ESM");
            enable(&ctx, &port).unwrap();
            let on_disk = fs::read_to_string(dir.join(&lib_name)).unwrap();
            assert!(on_disk.contains("createRequire(import.meta.url)"), "{name}: expected ESM shim, got:\n{on_disk}");
            assert!(!on_disk.contains("module.exports"), "{name}: ESM shim must not emit CJS sync:\n{on_disk}");

            // Run with a real Node subprocess using a CJS fake addon loaded through the ESM shim.
            let runnable = on_disk.replace("./.fast/lib/fa_lib_sq.node", "./fake_addon.cjs");
            fs::write(dir.join(&lib_name), &runnable).unwrap();
            let rust_run =
                Command::new("node").arg(&app_name).current_dir(&dir).env_remove(DISABLE_ENV).output().unwrap();
            let orig_run =
                Command::new("node").arg(&app_name).current_dir(&dir).env(DISABLE_ENV, "1").output().unwrap();
            fs::write(dir.join(&lib_name), &on_disk).unwrap();
            disable(&ctx, &port).unwrap();
            let restored = fs::read_to_string(dir.join(&lib_name)).unwrap();

            assert!(rust_run.status.success(), "{name}: enabled run failed: {}", combined(&rust_run));
            assert_eq!(
                String::from_utf8_lossy(&rust_run.stdout).trim(),
                "1009",
                "{name}: Rust addon must actually load in auto-detected ESM"
            );
            assert!(orig_run.status.success(), "{name}: disabled run failed: {}", combined(&orig_run));
            assert_eq!(String::from_utf8_lossy(&orig_run.stdout).trim(), "9", "{name}: FAST_ANYTHING_DISABLE=1");
            assert_eq!(restored, lib_src, "{name}: disable must restore byte for byte");
        }

        // 2. Explicit package.json "type": "commonjs" disables syntax detection (stays CJS),
        //    and a .ts file with only `import type` / `export type` + `module.exports` stays CJS.
        let cjs_pkg = base.join("pkg-cjs");
        fs::create_dir_all(&cjs_pkg).unwrap();
        fs::write(cjs_pkg.join("package.json"), "{\"type\": \"commonjs\"}\n").unwrap();
        fs::write(cjs_pkg.join("lib.js"), "export function sq(x) { return x * x; }\n").unwrap();
        assert!(!is_esm(&cjs_pkg, &cjs_pkg.join("lib.js")), "explicit type=commonjs must stay CJS");

        let ts_type_only = base.join("ts-type-only");
        fs::create_dir_all(&ts_type_only).unwrap();
        fs::write(
            ts_type_only.join("lib.ts"),
            "export type Num = number;\nfunction sq(x: Num): Num { return x * x; }\nmodule.exports = { sq };\n",
        )
        .unwrap();
        assert!(!is_esm(&ts_type_only, &ts_type_only.join("lib.ts")), "type-only TS export with CJS must stay CJS");

        // 3. Nearest package.json wins even when it omits "type" (does not inherit parent "type").
        let nested_cjs_parent = base.join("nested-cjs-parent");
        fs::create_dir_all(nested_cjs_parent.join("sub")).unwrap();
        fs::write(nested_cjs_parent.join("package.json"), "{\"type\": \"commonjs\"}\n").unwrap();
        fs::write(nested_cjs_parent.join("sub/package.json"), "{\"name\": \"sub\"}\n").unwrap();
        fs::write(nested_cjs_parent.join("sub/lib.js"), "export function sq(x) { return x * x; }\n").unwrap();
        assert!(
            is_esm(&nested_cjs_parent, &nested_cjs_parent.join("sub/lib.js")),
            "nearest package.json omitting type must detect ESM instead of inheriting parent commonjs"
        );

        let nested_esm_parent = base.join("nested-esm-parent");
        fs::create_dir_all(nested_esm_parent.join("sub")).unwrap();
        fs::write(nested_esm_parent.join("package.json"), "{\"type\": \"module\"}\n").unwrap();
        fs::write(nested_esm_parent.join("sub/package.json"), "{\"name\": \"sub\"}\n").unwrap();
        fs::write(nested_esm_parent.join("sub/lib.js"), "function sq(x) { return x * x; }\nmodule.exports = { sq };\n")
            .unwrap();
        assert!(
            !is_esm(&nested_esm_parent, &nested_esm_parent.join("sub/lib.js")),
            "nearest package.json omitting type must stay CJS for CJS syntax instead of inheriting parent module"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn shadowed_io_names_and_pure_crypto_are_portable_in_scan() {
        if !Command::new("node").arg("--version").output().is_ok_and(|o| o.status.success()) {
            eprintln!("no node on PATH, skipping");
            return;
        }
        let tmp = std::env::temp_dir();
        let tmp = if cfg!(windows) { tmp } else { tmp.canonicalize().unwrap() };
        let dir = tmp.join(format!("fa-node-io-shadow-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("work.mjs"),
            "import crypto from \"node:crypto\";\n\
             import fs from \"node:fs\";\n\n\
             export function scoreDoc(document, window, row) {\n\
               let total = 0;\n\
               for (let i = 0; i < 4000; i++) {\n\
                 total += (document.length + window + row.process + (i & fs.constants.R_OK)) % 97;\n\
               }\n\
               return crypto.createHash(\"sha256\").update(String(total)).digest(\"hex\");\n\
             }\n",
        )
        .unwrap();
        fs::write(
            dir.join("app.mjs"),
            "import { scoreDoc } from \"./work.mjs\";\n\
             let acc = 0;\n\
             for (let r = 0; r < 1500; r++) acc += scoreDoc(\"hello\", 8, { process: 3 }).length;\n\
             console.log(acc);\n",
        )
        .unwrap();
        let scanned = crate::pipeline::scan(&dir, vec!["node".into(), "app.mjs".into()], None);
        let _ = fs::remove_dir_all(&dir);
        let scanned = scanned.unwrap();
        let h = scanned
            .hotspots
            .iter()
            .find(|h| h.name == "scoreDoc")
            .unwrap_or_else(|| panic!("missing hotspot scoreDoc; got {:?}", scanned.hotspots));
        assert!(h.portable() && h.blockers.is_empty(), "scoreDoc should be portable, got {:?}", h.blockers);
    }
}
