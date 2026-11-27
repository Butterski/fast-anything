//! What every language backend does, and the parts they share: building the crate and
//! the marked blocks appended to source files.

use crate::hotspot::Hotspot;
use crate::state::{Ctx, Port, Verify};
use crate::util::{output, tail};
use crate::{cfamily, cpp, csharp, golang, java, kotlin, node, php, python, r, ruby};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    Python,
    Node,
    Ruby,
    Java,
    Go,
    Php,
    C,
    Cpp,
    #[serde(rename = "csharp")]
    CSharp,
    R,
    Kotlin,
}

/// What a port is written in. Ports from before this fork had C++ are Rust.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    #[default]
    Rust,
    Cpp,
}

impl Backend {
    /// The backend of a new port: the requested one, else C++ where this fork has it (Python apps
    /// on Linux and macOS), else Rust.
    pub fn choose(lang: Lang, requested: Option<Backend>) -> Result<Backend> {
        let cpp = lang == Lang::Python && !cfg!(windows);
        match requested {
            Some(Backend::Cpp) if !cpp => bail!(
                "C++ ports cover Python apps on Linux and macOS so far; port this {} app with --backend rust",
                lang.label()
            ),
            Some(backend) => Ok(backend),
            None => Ok(if cpp { Backend::Cpp } else { Backend::Rust }),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Backend::Rust => "Rust",
            Backend::Cpp => "C++",
        }
    }

    /// As written on the command line and in state.json.
    pub fn name(self) -> &'static str {
        match self {
            Backend::Rust => "rust",
            Backend::Cpp => "cpp",
        }
    }
}

#[derive(Deserialize)]
pub struct Profile {
    pub wall: f64,
    pub base: String,
    pub functions: Vec<Hotspot>,
    /// What the scan should tell the user besides the ranking.
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Deserialize)]
pub struct Recording {
    pub calls: u64,
    pub kept: usize,
    pub skipped: usize,
    pub unsupported: Vec<String>,
    /// The function changed an argument in place during a recorded call.
    #[serde(default)]
    pub mutates: bool,
}

/// Launchers that can't be unwrapped the way uv run or poetry run are (see python::unwrap_env_runner).
fn wrapper_hint(program: &str) -> Option<&'static str> {
    Some(match crate::util::program_stem(program).as_str() {
        "tsx" | "ts-node" | "npx" => {
            "Node.js 22.6+ runs TypeScript itself, and fast-anything needs node to start the app:\n    fast-anything scan -- node app.ts"
        }
        "bundle" => {
            "bundle exec sets up gems for its own process only. Scan with ruby directly, and if the app needs Bundler, put  require \"bundler/setup\"  at the top of its script:\n    fast-anything scan -- ruby app.rb"
        }
        _ => return None,
    })
}

impl Lang {
    pub fn detect(program: &str) -> Result<Lang> {
        if python::is_python(program) {
            Ok(Lang::Python)
        } else if node::is_node(program) {
            Ok(Lang::Node)
        } else if ruby::is_ruby(program) {
            Ok(Lang::Ruby)
        } else if java::is_java(program) {
            Ok(Lang::Java)
        } else if golang::is_go(program) {
            Ok(Lang::Go)
        } else if php::is_php(program) {
            Ok(Lang::Php)
        } else if csharp::is_csharp(program) {
            Ok(Lang::CSharp)
        } else if r::is_r(program) {
            Ok(Lang::R)
        } else if let Some(lang) = cfamily::detect(program) {
            Ok(lang)
        } else if let Some(hint) = wrapper_hint(program) {
            bail!("{hint}")
        } else {
            bail!(
                "fast-anything speaks Python, Node.js (JavaScript/TypeScript), Ruby, Java, Go, PHP, C, C++, C# and R. Start the command with one of them:\n    fast-anything scan -- python app.py\n    fast-anything scan -- node app.js  (or bun app.ts, deno run -A app.ts)\n    fast-anything scan -- ruby app.rb\n    fast-anything scan -- java App.java\n    fast-anything scan -- go run .\n    fast-anything scan -- php app.php\n    fast-anything scan -- clang -O2 main.c  (or gcc, g++, clang++, icpx)\n    fast-anything scan -- dotnet run -c Release\n    fast-anything scan -- Rscript app.R"
            )
        }
    }

    /// Ports called through the C entry point, with the port in src/port.rs.
    pub fn c_bridge(self) -> bool {
        matches!(
            self,
            Lang::Ruby
                | Lang::Java
                | Lang::Go
                | Lang::Php
                | Lang::C
                | Lang::Cpp
                | Lang::CSharp
                | Lang::Kotlin
                | Lang::R
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::Python => "Python",
            Lang::Node => "JavaScript",
            Lang::Ruby => "Ruby",
            Lang::Java => "Java",
            Lang::Go => "Go",
            Lang::Php => "PHP",
            Lang::C => "C",
            Lang::Cpp => "C++",
            Lang::CSharp => "C#",
            Lang::R => "R",
            Lang::Kotlin => "Kotlin",
        }
    }

    pub fn resolve(self, program: &str, args: &[String], root: &Path) -> Result<String> {
        match self {
            Lang::Python => python::resolve(program, args, root),
            Lang::Node => node::resolve(program, root),
            Lang::Ruby => ruby::resolve(program, root),
            Lang::Java => java::resolve(program, root),
            Lang::Go => golang::resolve(program, root),
            Lang::Php => php::resolve(program, root),
            Lang::C | Lang::Cpp => cfamily::resolve(program, root),
            Lang::CSharp => csharp::resolve(program, root),
            Lang::R => r::resolve(program, root),
            Lang::Kotlin => java::resolve(program, root),
        }
    }

    /// Whether `scan --duration` can stop this language's apps.
    pub fn stops_after_duration(self) -> bool {
        // R: scan interrupts it with SIGINT, which Windows doesn't have
        matches!(
            self,
            Lang::Python
                | Lang::Node
                | Lang::Go
                | Lang::Java
                | Lang::Kotlin
                | Lang::Ruby
                | Lang::Php
                | Lang::C
                | Lang::Cpp
                | Lang::CSharp
        ) || (self == Lang::R && cfg!(unix))
    }

    /// `duration` (see stops_after_duration): stop the app after that many seconds.
    pub fn profile(self, ctx: &Ctx, exe: &str, args: &[String], duration: Option<f64>) -> Result<Profile> {
        match self {
            Lang::Python => python::profile(ctx, exe, args, duration),
            Lang::Node => node::profile(ctx, exe, args, duration),
            Lang::Ruby => ruby::profile(ctx, exe, args, duration),
            Lang::Java => java::profile(ctx, exe, args, duration),
            Lang::Go => golang::profile(ctx, exe, args, duration),
            Lang::Php => php::profile(ctx, exe, args, duration),
            Lang::C | Lang::Cpp => cfamily::profile(ctx, exe, args, self, duration),
            Lang::CSharp => csharp::profile(ctx, exe, args, duration),
            Lang::R => r::profile(ctx, exe, args, duration),
            Lang::Kotlin => kotlin::profile(ctx, exe, args, duration),
        }
    }

    pub fn record(
        self,
        ctx: &Ctx,
        exe: &str,
        args: &[String],
        hs: &Hotspot,
        out: &Path,
        limit: usize,
    ) -> Result<Recording> {
        match self {
            Lang::Python => python::record(ctx, exe, args, hs, out, limit),
            Lang::Node => node::record(ctx, exe, args, hs, out, limit),
            Lang::Ruby => ruby::record(ctx, exe, args, hs, out, limit),
            Lang::Java => java::record(ctx, exe, args, hs, out, limit),
            Lang::Go => golang::record(ctx, exe, args, hs, out, limit),
            Lang::Php => php::record(ctx, exe, args, hs, out, limit),
            Lang::C | Lang::Cpp => cfamily::record(ctx, exe, args, hs, out, self),
            Lang::CSharp => csharp::record(ctx, exe, args, hs, out, limit),
            Lang::R => r::record(ctx, exe, args, hs, out, limit),
            Lang::Kotlin => kotlin::record(ctx, exe, args, hs, out, limit),
        }
    }

    pub fn scaffold(self, ctx: &Ctx, hs: &Hotspot, helpers: &[Hotspot], port: &Port) -> Result<String> {
        if port.backend == Backend::Cpp {
            return cpp::scaffold(ctx, hs, helpers, port);
        }
        match self {
            Lang::Python => python::scaffold(ctx, hs, helpers, port),
            Lang::Node => node::scaffold(ctx, hs, helpers, port),
            Lang::Ruby => ruby::scaffold(ctx, hs, helpers, port),
            Lang::Java => java::scaffold(ctx, hs, helpers, port),
            Lang::Go => golang::scaffold(ctx, hs, helpers, port),
            Lang::Php => php::scaffold(ctx, hs, helpers, port),
            Lang::C | Lang::Cpp => cfamily::scaffold(ctx, hs, helpers, port),
            Lang::CSharp => csharp::scaffold(ctx, hs, helpers, port),
            Lang::R => r::scaffold(ctx, hs, helpers, port),
            Lang::Kotlin => kotlin::scaffold(ctx, hs, helpers, port),
        }
    }

    pub fn verify(self, ctx: &Ctx, port: &Port) -> Result<Verify> {
        match self {
            Lang::Python => python::verify(ctx, port),
            Lang::Node => node::verify(ctx, port),
            Lang::Ruby => ruby::verify(ctx, port),
            Lang::Java => java::verify(ctx, port),
            Lang::Go => golang::verify(ctx, port),
            Lang::Php => php::verify(ctx, port),
            Lang::C | Lang::Cpp => cfamily::verify(ctx, port),
            Lang::CSharp => csharp::verify(ctx, port),
            Lang::R => r::verify(ctx, port),
            Lang::Kotlin => kotlin::verify(ctx, port),
        }
    }

    pub fn enable(self, ctx: &Ctx, port: &Port) -> Result<()> {
        match self {
            Lang::Python => python::enable(ctx, port),
            Lang::Node => node::enable(ctx, port),
            Lang::Ruby => ruby::enable(ctx, port),
            Lang::Java => java::enable(ctx, port),
            Lang::Go => golang::enable(ctx, port),
            Lang::Php => php::enable(ctx, port),
            Lang::C | Lang::Cpp => cfamily::enable(ctx, port),
            Lang::CSharp => csharp::enable(ctx, port),
            Lang::R => r::enable(ctx, port),
            Lang::Kotlin => kotlin::enable(ctx, port),
        }
    }

    pub fn disable(self, ctx: &Ctx, port: &Port) -> Result<()> {
        match self {
            Lang::Python => python::disable(ctx, port),
            Lang::Node => node::disable(ctx, port),
            Lang::Ruby => ruby::disable(ctx, port),
            Lang::Java => java::disable(ctx, port),
            Lang::Go => golang::disable(ctx, port),
            Lang::Php => php::disable(ctx, port),
            Lang::C | Lang::Cpp => cfamily::disable(ctx, port),
            Lang::CSharp => csharp::disable(ctx, port),
            Lang::R => r::disable(ctx, port),
            Lang::Kotlin => kotlin::disable(ctx, port),
        }
    }

    /// What bench runs: (program, args). Go apps are built once, so the compiler isn't timed.
    pub fn bench_command(self, ctx: &Ctx) -> Result<(String, Vec<String>)> {
        let exe = ctx.runtime()?;
        match self {
            Lang::Python => python::bench_command(ctx, exe),
            Lang::Go => golang::bench_command(ctx, exe),
            Lang::Java | Lang::Kotlin => java::bench_command(ctx, exe),
            Lang::C | Lang::Cpp => cfamily::bench_command(ctx, exe),
            Lang::CSharp => csharp::bench_command(ctx, exe),
            _ => Ok((exe.to_string(), ctx.state.command[1..].to_vec())),
        }
    }

    /// (file cargo builds, name it gets in .fast/lib)
    pub fn artifact(self, ext: &str) -> (String, String) {
        let built = if cfg!(windows) {
            format!("{ext}.dll")
        } else if cfg!(target_os = "macos") {
            format!("lib{ext}.dylib")
        } else {
            format!("lib{ext}.so")
        };
        let dest = match self {
            Lang::Python if cfg!(windows) => format!("{ext}.pyd"),
            Lang::Python => format!("{ext}.abi3.so"),
            Lang::Node => format!("{ext}.node"),
            // loaded through a C FFI (Fiddle, FFM, ...): the platform's own library name
            Lang::Ruby
            | Lang::Java
            | Lang::Go
            | Lang::Php
            | Lang::C
            | Lang::Cpp
            | Lang::CSharp
            | Lang::Kotlin
            | Lang::R => built.clone(),
        };
        (built, dest)
    }
}

// ------------------------------------------------------------------ held-out calls

/// Every 5th recorded call is held out.
fn held(k: usize) -> bool {
    k % 5 == 4
}

/// Move every 5th recorded call (derived and edge cases stay) from the crate's recordings to
/// `holdout`, outside the crate folder: the agent writing the port never sees them, so a port that
/// special-cases the calls it was shown fails verify. Fewer than 5 recorded calls hold none back.
/// An .rds of the recorded calls beside the recordings (R) moves along whole: verify picks the
/// held-out ones by position (see replay_order). Returns how many calls are held out.
pub fn hold_out(recordings: &Path, holdout: &Path) -> Result<usize> {
    let (rds, held_rds) = (recordings.with_extension("rds"), holdout.with_extension("rds"));
    let _ = fs::remove_file(holdout);
    let _ = fs::remove_file(&held_rds);
    let text = fs::read_to_string(recordings).context("reading recordings.jsonl")?;
    let lines: Vec<(&str, bool)> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| (l, serde_json::from_str::<serde_json::Value>(l).is_ok_and(|c| c.get("derived").is_none())))
        .collect();
    if lines.iter().filter(|(_, recorded)| *recorded).count() < 5 {
        return Ok(0);
    }
    let (mut keep, mut out, mut k) = (String::new(), String::new(), 0);
    for (line, recorded) in lines {
        let into = if recorded && held(k) { &mut out } else { &mut keep };
        into.push_str(line);
        into.push('\n');
        k += usize::from(recorded);
    }
    if let Some(dir) = holdout.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(holdout, &out)?;
    fs::write(recordings, keep)?;
    if rds.exists() {
        fs::rename(&rds, &held_rds)?;
    }
    Ok(out.lines().count())
}

/// Positions of `recorded` calls in the order verify replays them: the visible ones, then the held-out ones.
pub fn replay_order(recorded: usize) -> Vec<usize> {
    let (visible, out): (Vec<usize>, Vec<usize>) = (0..recorded).partition(|&k| !held(k));
    visible.into_iter().chain(out).collect()
}

/// What verify replays, and where the runner reports.
pub struct Replay {
    pub cases: PathBuf,
    pub out: PathBuf,
    pub held_out: usize,
}

/// Without held-out calls: the crate's recordings.jsonl and verify.json, as always. With them, both
/// sit in .fast/verify/<port>/, outside the crate folder: the cases are recordings.jsonl followed by
/// the held-out calls, and the report shows their values.
pub fn replay(ctx: &Ctx, port: &Port) -> Result<Replay> {
    let dir = ctx.crate_dir(port);
    let held_out = ctx.held_out(port);
    if held_out == 0 {
        return Ok(Replay { cases: dir.join("recordings.jsonl"), out: dir.join("verify.json"), held_out });
    }
    let mut text = fs::read_to_string(dir.join("recordings.jsonl")).context("reading recordings.jsonl")?;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&fs::read_to_string(ctx.holdout(port)).context("reading the held-out calls")?);
    let verify_dir = verify_dir(ctx, port);
    fs::create_dir_all(&verify_dir)?;
    let cases = verify_dir.join("cases.jsonl");
    fs::write(&cases, text)?;
    Ok(Replay { cases, out: verify_dir.join("verify.json"), held_out })
}

fn verify_dir(ctx: &Ctx, port: &Port) -> PathBuf {
    ctx.fast.join("verify").join(ctx.crate_dir(port).file_name().unwrap_or_default())
}

/// After the runner reports: count and redact the held-out calls, then remove .fast/verify/<port>/,
/// whose cases and report show their values. Without held-out calls verify.json stays in the crate.
pub fn finish_replay(ctx: &Ctx, port: &Port, visible: usize, v: &mut Verify) -> Result<()> {
    v.held_out = ctx.held_out(port);
    redact_held_out(v, &port.func, visible);
    let dir = verify_dir(ctx, port);
    if v.held_out > 0 && dir.exists() {
        fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
    }
    Ok(())
}

/// Failures among the held-out calls (replayed right after the `visible` lines of recordings.jsonl)
/// lose their values, so neither the output nor the agent's next prompt shows them.
pub fn redact_held_out(v: &mut Verify, func: &str, visible: usize) {
    for f in &mut v.failures {
        if (visible..visible + v.held_out).contains(&f.case) {
            f.call = format!("(held out) {func}(…)");
            f.expected = "its recorded result".into();
            f.got = "something else".into();
        }
    }
}

/// The TASK.md line about held-out calls, if there are any.
pub fn held_out_note(held_out: usize) -> String {
    match held_out {
        0 => String::new(),
        1 => "1 more recorded call is held out: verify checks it too and reports a mismatch there without the values.\n\n".into(),
        n => format!(
            "{n} more recorded calls are held out: verify checks them too and reports a mismatch there without the values.\n\n"
        ),
    }
}

/// Settings shared by every generated crate: tuned for the machine that builds it
/// (.fast/lib is rebuilt per machine, never shipped), plus macOS extension linking.
pub const CARGO_CONFIG: &str = "# Built for the machine that runs it: .fast/lib is rebuilt per machine, never shipped.\n\
[build]\nrustflags = [\"-C\", \"target-cpu=native\"]\n\n\
# Extensions on macOS resolve the host's symbols at load time.\n\
[target.aarch64-apple-darwin]\nrustflags = [\"-C\", \"target-cpu=native\", \"-C\", \"link-arg=-undefined\", \"-C\", \"link-arg=dynamic_lookup\"]\n\n\
[target.x86_64-apple-darwin]\nrustflags = [\"-C\", \"target-cpu=native\", \"-C\", \"link-arg=-undefined\", \"-C\", \"link-arg=dynamic_lookup\"]\n";

/// No LTO and the default codegen units: a port's hot loop lives in its own crate, so cross-crate
/// inlining buys nothing measurable, while rebuilds after each edit get 4-9x faster (4.1s to 1.1s;
/// a PyO3 port 3.5s to 0.37s, its run time within noise).
pub const RELEASE_PROFILE: &str = "[profile.release]\nopt-level = 3\nlto = false\n\n\
# A crate of its own, even inside another Cargo workspace.\n[workspace]\n";

fn compiler_errors(log: &str) -> String {
    let noise =
        ["Compiling", "Finished", "Updating", "Locking", "Downloading", "Downloaded", "Blocking", "Adding", "Checking"];
    let kept: Vec<&str> = log.lines().filter(|l| !noise.iter().any(|n| l.trim_start().starts_with(n))).collect();
    tail(&kept.join("\n"), 60)
}

/// Release-build a port into .fast/lib. Ok(Err(log)) when it doesn't compile.
/// Every port shares one target dir, so dependencies compile once per project.
pub fn build(ctx: &Ctx, port: &Port) -> Result<std::result::Result<PathBuf, String>> {
    if port.backend == Backend::Cpp {
        return cpp::build(ctx, port);
    }
    let exe = ctx.runtime()?;
    let dir = ctx.crate_dir(port);
    ensure!(dir.join("Cargo.toml").exists(), "{} has no crate yet: run  fast-anything port {}", port.id, port.id);
    let target = ctx.fast.join("target");
    let mut cmd = Command::new("cargo");
    cmd.args(["build", "--release", "--message-format=short"]).current_dir(&dir).env("CARGO_TARGET_DIR", &target);
    if port.lang == Lang::Python {
        cmd.env("PYO3_PYTHON", exe);
    }
    let out = output(&mut cmd, "cargo (install Rust from https://rustup.rs; scan works without it)")?;
    if !out.status.success() {
        return Ok(Err(compiler_errors(&String::from_utf8_lossy(&out.stderr))));
    }
    let (built, name) = port.lang.artifact(&port.ext);
    let lib_dir = ctx.lib_dir();
    fs::create_dir_all(&lib_dir)?;
    let dest = lib_dir.join(name);
    fs::copy(target.join("release").join(&built), &dest)
        .with_context(|| format!("copying {built} into .fast/lib (is the app still running?)"))?;
    Ok(Ok(dest))
}

/// The hash of a port's sources in either backend (see source_hash and cpp::source_hash).
pub fn port_hash(dir: &Path, backend: Backend) -> Result<String> {
    match backend {
        Backend::Rust => source_hash(dir),
        Backend::Cpp => cpp::source_hash(dir),
    }
}

/// FNV-1a over the crate's Cargo.toml and everything under src/, so build can tell whether the
/// sources are still the ones verify checked. Stable across Rust versions, unlike DefaultHasher.
pub fn source_hash(dir: &Path) -> Result<String> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() { walk(&path, out)? } else { out.push(path) }
        }
        Ok(())
    }
    let mut files = vec![dir.join("Cargo.toml")];
    if dir.join("src").is_dir() {
        walk(&dir.join("src"), &mut files)?;
    }
    files.sort();
    let mut h: u64 = 0xcbf29ce484222325;
    for file in &files {
        let rel = file.strip_prefix(dir).unwrap_or(file).to_string_lossy().replace('\\', "/");
        let bytes = fs::read(file).with_context(|| format!("reading {}", file.display()))?;
        for b in rel.bytes().chain([0]).chain(bytes).chain([0]) {
            h = (h ^ b as u64).wrapping_mul(0x100000001b3);
        }
    }
    Ok(format!("{h:016x}"))
}

// ------------------------------------------------------------------ marked blocks

pub fn markers(comment: &str, id: &str) -> (String, String) {
    (format!("{comment} >>> fast-anything: {id} >>>"), format!("{comment} <<< fast-anything: {id} <<<"))
}

fn newline(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// Remove the block for id; the rest of the file comes back byte for byte.
pub fn strip_marked(text: &str, comment: &str, id: &str) -> String {
    let (begin, end) = markers(comment, id);
    let Some(start) = text.find(&begin) else { return text.to_string() };
    let Some(len) = text[start..].find(&end) else { return text.to_string() };
    let nl = newline(text);
    let rest = &text[start + len + end.len()..];
    let rest = rest.strip_prefix(nl).unwrap_or(rest);
    if rest.is_empty() {
        // appended at the end, after one blank line
        let before = &text[..start];
        before.strip_suffix(nl).unwrap_or(before).to_string()
    } else {
        // inserted before other code, followed by one blank line
        format!("{}{}", &text[..start], rest.strip_prefix(nl).unwrap_or(rest))
    }
}

fn block(text: &str, comment: &str, id: &str, body: &str) -> String {
    let (begin, end) = markers(comment, id);
    format!("{begin}\n{body}\n{end}").replace('\n', newline(text))
}

/// Append (or replace) the block for id at the end of the file, after one blank line.
pub fn append_marked(text: &str, comment: &str, id: &str, body: &str) -> String {
    let text = strip_marked(text, comment, id);
    let nl = newline(&text);
    let gap = if text.ends_with('\n') { nl.to_string() } else { format!("{nl}{nl}") };
    format!("{text}{gap}{}{nl}", block(&text, comment, id, body))
}

/// Insert (or replace) the block for id right before the line at byte offset at.
pub fn insert_marked(text: &str, comment: &str, id: &str, body: &str, at: impl Fn(&str) -> Option<usize>) -> String {
    let text = strip_marked(text, comment, id);
    let Some(at) = at(&text) else { return append_marked(&text, comment, id, body) };
    let nl = newline(&text);
    format!("{}{}{nl}{nl}{}", &text[..at], block(&text, comment, id, body), &text[at..])
}

/// Byte offset of the first top-level line that starts with one of the prefixes.
pub fn line_starting(text: &str, prefixes: &[&str]) -> Option<usize> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = if offset == 0 { line.strip_prefix('\u{feff}').unwrap_or(line) } else { line };
        let squashed: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        let top_level = !body.starts_with([' ', '\t']);
        if top_level && prefixes.iter().any(|p| squashed.starts_with(p)) {
            return Some(offset + line.len() - body.len());
        }
        offset += line.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_hash_follows_the_sources() {
        let dir = std::env::temp_dir().join(format!("fa-hash-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\n").unwrap();
        fs::write(dir.join("src/lib.rs"), "fn f() {}\n").unwrap();
        let first = source_hash(&dir).unwrap();
        assert_eq!(source_hash(&dir).unwrap(), first);
        fs::write(dir.join("TASK.md"), "not a source").unwrap();
        assert_eq!(source_hash(&dir).unwrap(), first, "only Cargo.toml and src/ count");
        fs::write(dir.join("src/lib.rs"), "fn f() { }\n").unwrap();
        assert_ne!(source_hash(&dir).unwrap(), first);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn launchers_get_a_hint() {
        assert!(Lang::detect("npx").unwrap_err().to_string().contains("node app.ts"));
        assert!(Lang::detect("tsx").unwrap_err().to_string().contains("node app.ts"));
        assert!(Lang::detect("bundle").unwrap_err().to_string().contains("bundler/setup"));
        assert!(Lang::detect("cobol").unwrap_err().to_string().starts_with("fast-anything speaks"));
    }

    #[test]
    fn blocks_come_out_byte_for_byte() {
        let guard = |t: &str| line_starting(t, &["if__name__==\"__main__\":", "if__name__=='__main__':"]);
        for original in ["a = 1\n", "a = 1\r\n", "a = 1\n\n\nif __name__ == \"__main__\":\n    main()\n", "x"] {
            let appended = append_marked(original, "#", "m.f", "f = g");
            assert!(appended.contains("# >>> fast-anything: m.f >>>"));
            assert_eq!(
                strip_marked(&appended, "#", "m.f"),
                original.to_string() + if original == "x" { "\n" } else { "" }
            );
            let inserted = insert_marked(original, "#", "m.f", "f = g", guard);
            assert_eq!(insert_marked(&inserted, "#", "m.f", "f = g", guard), inserted, "idempotent");
            let back = strip_marked(&inserted, "#", "m.f");
            assert_eq!(back.trim_end(), original.trim_end());
            if original.contains("__main__") {
                assert_eq!(back, original);
                assert!(inserted.find(">>>").unwrap() < inserted.find("if __name__").unwrap());
            }
        }
        let two = append_marked(&append_marked("a\n", "#", "one", "1"), "#", "two", "2");
        assert_eq!(strip_marked(&strip_marked(&two, "#", "one"), "#", "two"), "a\n");

        // Leading UTF-8 BOM on line 1 is preserved when inserting and stripping a block,
        // while indented BOM lines and non-line-1 BOM characters are not treated as top-level guards.
        let bom_main = "\u{feff}if __name__ == \"__main__\":\r\n    main()\r\n";
        assert_eq!(guard(bom_main), Some("\u{feff}".len()));
        assert_eq!(guard("\u{feff}    if __name__ == \"__main__\":\r\n    main()\r\n"), None);
        assert_eq!(guard("x = 1\n\u{feff}if __name__ == \"__main__\":\n    main()\n"), None);
        let bom_on = insert_marked(bom_main, "#", "m.f", "f = g", guard);
        assert!(bom_on.starts_with('\u{feff}'));
        assert_eq!(strip_marked(&bom_on, "#", "m.f"), bom_main);
    }

    fn call(k: usize) -> String {
        format!("{{\"args\":[{k}],\"result\":{}}}", k * 1000 + 7)
    }

    #[test]
    fn every_fifth_recorded_call_is_held_out() {
        let dir = std::env::temp_dir().join(format!("fa-holdout-split-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (recordings, holdout) = (dir.join("recordings.jsonl"), dir.join("holdout/demo.jsonl"));
        let mut lines: Vec<String> = (0..12).map(call).collect();
        lines.insert(6, r#"{"args":[-1],"result":-1,"derived":"edge"}"#.into());
        lines.push(r#"{"args":[0],"result":7,"derived":true}"#.into());
        fs::write(&recordings, lines.join("\n") + "\n").unwrap();
        fs::write(dir.join("recordings.rds"), "every recorded call").unwrap();

        assert_eq!(hold_out(&recordings, &holdout).unwrap(), 2);
        assert_eq!(fs::read_to_string(&holdout).unwrap(), format!("{}\n{}\n", call(4), call(9)));
        let kept = fs::read_to_string(&recordings).unwrap();
        assert_eq!(kept.lines().count(), 12, "10 recorded calls and both derived ones stay: {kept}");
        assert!(kept.contains("\"derived\":\"edge\"") && kept.contains("\"derived\":true"));
        assert!(!kept.contains(&call(4)) && !kept.contains(&call(9)));
        assert!(!dir.join("recordings.rds").exists() && dir.join("holdout/demo.rds").exists(), "the .rds moves along");

        // Porting again records afresh; fewer than 5 recorded calls hold none back.
        let few: Vec<String> = (0..4).map(call).chain([r#"{"args":[0],"result":7,"derived":true}"#.into()]).collect();
        fs::write(&recordings, few.join("\n") + "\n").unwrap();
        assert_eq!(hold_out(&recordings, &holdout).unwrap(), 0);
        assert!(!holdout.exists() && !dir.join("holdout/demo.rds").exists());
        assert_eq!(fs::read_to_string(&recordings).unwrap(), few.join("\n") + "\n");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn verify_replays_held_out_calls_after_the_visible_ones() {
        assert_eq!(replay_order(12), [0, 1, 2, 3, 5, 6, 7, 8, 10, 11, 4, 9]);
        assert_eq!(replay_order(4), [0, 1, 2, 3]);

        let root = std::env::temp_dir().join(format!("fa-holdout-replay-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let ctx = Ctx::open(&root).unwrap();
        let port = Port {
            id: "app.f".into(),
            file: "app.py".into(),
            module: "app".into(),
            func: "f".into(),
            ext: "fa_app_f".into(),
            crate_dir: ".fast/ports/app.f".into(),
            recorded: 12,
            calls_seen: 12,
            verify: None,
            enabled: false,
            lang: Lang::Python,
            owner: None,
            line: 1,
            sources: vec![],
            backend: Backend::Rust,
        };
        let dir = ctx.crate_dir(&port);
        fs::create_dir_all(&dir).unwrap();
        let recordings = dir.join("recordings.jsonl");
        fs::write(&recordings, (0..12).map(call).collect::<Vec<_>>().join("\n") + "\n").unwrap();

        // A port from before holding out: the crate's own files, as always.
        let replay0 = replay(&ctx, &port).unwrap();
        assert_eq!((replay0.cases, replay0.out, replay0.held_out), (recordings.clone(), dir.join("verify.json"), 0));

        hold_out(&recordings, &ctx.holdout(&port)).unwrap();
        assert_eq!(ctx.held_out(&port), 2);
        let r = replay(&ctx, &port).unwrap();
        assert_eq!(r.held_out, 2);
        assert!(
            !r.cases.starts_with(&dir) && !r.out.starts_with(&dir),
            "outside the crate folder: {}",
            r.cases.display()
        );
        let merged: Vec<String> = fs::read_to_string(&r.cases).unwrap().lines().map(str::to_string).collect();
        let want: Vec<String> = replay_order(12).into_iter().map(call).collect();
        assert_eq!(merged, want, "visible calls first, then the held-out ones at 10 and 11");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_leaves_no_held_out_values_behind() {
        let root = std::env::temp_dir().join(format!("fa-holdout-finish-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let ctx = Ctx::open(&root).unwrap();
        let mut port = Port {
            id: "app.f".into(),
            file: "app.py".into(),
            module: "app".into(),
            func: "f".into(),
            ext: "fa_app_f".into(),
            crate_dir: ".fast/ports/app.f".into(),
            recorded: 4,
            calls_seen: 4,
            verify: None,
            enabled: false,
            lang: Lang::Python,
            owner: None,
            line: 1,
            sources: vec![],
            backend: Backend::Rust,
        };
        let dir = ctx.crate_dir(&port);
        fs::create_dir_all(&dir).unwrap();
        let recordings = dir.join("recordings.jsonl");
        let failed = |case: usize| Verify {
            built: true,
            total: 12,
            failed: 1,
            passed: 11,
            failures: vec![crate::state::Failure {
                case,
                call: "f(4007)".into(),
                expected: "4007".into(),
                got: "0".into(),
            }],
            ..Verify::default()
        };

        // Without held-out calls the report stays in the crate folder.
        fs::write(&recordings, (0..4).map(call).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        let r = replay(&ctx, &port).unwrap();
        fs::write(&r.out, "{}").unwrap();
        let mut v = failed(2);
        finish_replay(&ctx, &port, 4, &mut v).unwrap();
        assert!(dir.join("verify.json").exists());
        assert_eq!((v.held_out, v.failures[0].call.as_str()), (0, "f(4007)"));

        port.recorded = 12;
        fs::write(&recordings, (0..12).map(call).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        hold_out(&recordings, &ctx.holdout(&port)).unwrap();
        let r = replay(&ctx, &port).unwrap();
        fs::write(&r.out, "{\"failures\":[{\"call\":\"f(4007)\"}]}").unwrap();
        let mut v = failed(10);
        finish_replay(&ctx, &port, 10, &mut v).unwrap();
        assert!(!r.cases.exists() && !r.out.exists() && !ctx.fast.join("verify/app.f").exists());
        assert_eq!((v.total, v.passed, v.failed, v.held_out), (12, 11, 1, 2));
        assert_eq!(v.failures[0].call, "(held out) f(…)");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn held_out_failures_lose_their_values() {
        let failure = |case: usize, value: &str| crate::state::Failure {
            case,
            call: format!("f({value})"),
            expected: value.into(),
            got: "0".into(),
        };
        let mut v = Verify {
            built: true,
            total: 20,
            held_out: 2,
            derived: 8,
            failed: 3,
            failures: vec![failure(3, "3007"), failure(10, "4007"), failure(12, "9")],
            ..Verify::default()
        };
        redact_held_out(&mut v, "f", 10);
        assert_eq!(v.failures[0].call, "f(3007)", "a visible call keeps its values");
        assert_eq!(v.failures[0].expected, "3007");
        assert_eq!(v.failures[1].call, "(held out) f(…)");
        assert!(!format!("{:?}", v.failures[1]).contains("4007"), "{:?}", v.failures[1]);
        assert_eq!(v.failures[2].call, "f(9)", "derived cases come after the held-out ones");
    }
}
