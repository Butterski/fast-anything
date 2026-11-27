//! The steps (scan, port, verify, enable, bench, report) shared by the CLI and the MCP server.

use crate::backend::{self, Backend, Lang};
use crate::bench::{self, BenchResult};
use crate::hotspot::{self, Hotspot};
use crate::report::{self, ReportOut};
use crate::state::{Ctx, Port, Verify};
use crate::{agent, show, ui, util};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct ScanOut {
    pub command: Vec<String>,
    pub wall: f64,
    pub hotspots: Vec<Hotspot>,
    pub notes: Vec<String>,
}

/// `duration`: stop the app after that many seconds (a GUI or a server that doesn't exit on its own).
pub fn scan(root: &Path, command: Vec<String>, duration: Option<f64>) -> Result<ScanOut> {
    ensure!(
        !command.is_empty(),
        "put the command that runs your app after --, e.g.  fast-anything scan -- python app.py"
    );
    let mut ctx = Ctx::open(root)?;
    let command = match crate::python::unwrap_env_runner(&command, |argv| env_runner_output(&ctx.root, argv))? {
        Some(unwrapped) => {
            ui::step(format!("{} runs {}", ui::bold(command[..2].join(" ")), ui::bold(&unwrapped[0])));
            unwrapped
        }
        None => command,
    };
    let lang = crate::cfamily::refine(Lang::detect(&command[0])?, &command[1..]);
    let lang = crate::kotlin::refine(lang, &command[0], &ctx.root);
    if let Some(d) = duration {
        ensure!(lang.stops_after_duration(), "--duration stops R apps with SIGINT, which Windows doesn't have");
        ensure!(d.is_finite() && d > 0.0, "--duration takes a number of seconds, e.g. --duration 30");
    }
    let exe = lang.resolve(&command[0], &command[1..], &ctx.root)?;
    ctx.state.command = command.clone();
    ui::step(format!("profiling {}", ui::bold(command.join(" "))));
    if let Some(d) = duration.filter(|_| lang != Lang::CSharp) {
        ui::line(format!("  stopping it after {d}s"));
    }
    let profile = lang.profile(&ctx, &exe, &command[1..], duration)?;
    let hotspots = hotspot::rank(profile.functions, profile.wall);
    ctx.state.lang = lang;
    ctx.state.runtime = Some(exe);
    ctx.state.base = profile.base;
    ctx.state.wall = profile.wall;
    ctx.state.hotspots = hotspots.clone();
    ctx.save()?;
    Ok(ScanOut { command, wall: profile.wall, hotspots, notes: profile.notes })
}

/// What an env runner (uv, poetry, ...) prints for a command run in the project.
fn env_runner_output(root: &Path, argv: &[&str]) -> Result<String> {
    let out = util::output(std::process::Command::new(argv[0]).args(&argv[1..]).current_dir(root), argv[0])?;
    ensure!(out.status.success(), "{} failed:\n{}", argv.join(" "), util::tail(&util::combined(&out), 10));
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn pick<'a>(hotspots: &'a [Hotspot], id: Option<&str>) -> Result<&'a Hotspot> {
    let Some(id) = id else {
        return hotspots
            .iter()
            .find(|h| h.worth_it())
            .context("no function worth porting in the last scan (see  fast-anything status  for why)");
    };
    if let Some(h) = hotspots.iter().find(|h| h.id == id) {
        return Ok(h);
    }
    let hits: Vec<&Hotspot> = hotspots.iter().filter(|h| h.name == id || h.qualname == id).collect();
    match hits.as_slice() {
        [one] => Ok(one),
        [] => bail!("{id} wasn't in the last scan; run  fast-anything scan -- ...  again, or check the name"),
        many => bail!("{id} is ambiguous: {}", many.iter().map(|h| h.id.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

#[derive(Serialize)]
pub struct PortOut {
    pub id: String,
    pub crate_dir: PathBuf,
    pub lib_rs: PathBuf,
    pub task_md: PathBuf,
    pub recorded: usize,
    pub calls_seen: u64,
    pub skipped: usize,
    pub task: String,
    pub backend: Backend,
}

/// A Rust port, as upstream fast-anything makes them (its tests rely on this).
#[cfg(test)]
pub fn port(root: &Path, id: Option<&str>, samples: usize) -> Result<PortOut> {
    port_with(root, id, samples, Some(Backend::Rust))
}

/// Record a function's calls and set up its port in `backend` (default: Backend::choose).
pub fn port_with(root: &Path, id: Option<&str>, samples: usize, backend: Option<Backend>) -> Result<PortOut> {
    let mut ctx = Ctx::open(root)?;
    let exe = ctx.runtime()?.to_string();
    let lang = ctx.state.lang;
    let backend = Backend::choose(lang, backend)?;
    let hs = pick(&ctx.state.hotspots, id)?.clone();
    if !hs.portable() {
        bail!("{} can't be ported yet: {}", hs.id, hs.blockers.join("; "));
    }
    let module = hs.module.clone().unwrap_or_default();
    let crate_rel = format!(".fast/ports/{}", hs.id.replace([':', '/', '\\'], "."));
    let dir = ctx.root.join(&crate_rel);
    if let Some(old) = ctx.state.ports.get(&hs.id).filter(|p| p.backend != backend && dir.exists()) {
        ensure!(
            !old.enabled,
            "{} is switched on: run  fast-anything disable {}  before porting it to {}",
            hs.id,
            hs.id,
            backend.label()
        );
        let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let aside = ctx.fast.join("backup").join(format!("{name}.{}", old.backend.name()));
        if aside.exists() {
            fs::remove_dir_all(&aside)?;
        }
        fs::create_dir_all(ctx.fast.join("backup"))?;
        fs::rename(&dir, &aside).with_context(|| format!("moving the {} port aside", old.backend.label()))?;
        ui::warn(format!("{} had a {} port; it's kept in {}", hs.id, old.backend.label(), aside.display()));
    }
    fs::create_dir_all(&dir)?;

    ui::step(format!("running the app again to record real calls to {}", ui::bold(&hs.id)));
    let app_args = ctx.state.command[1..].to_vec();
    let rec = lang.record(&ctx, &exe, &app_args, &hs, &dir.join("recordings.jsonl"), samples.max(1))?;
    if rec.mutates {
        bail!(
            "{} changes its arguments in place (seen while recording). A port can only return a value, so it would drop those changes. Have the function return the new value instead, then scan again.",
            hs.id
        );
    }
    if rec.kept == 0 {
        let types =
            if rec.unsupported.is_empty() { String::new() } else { format!(" (got: {})", rec.unsupported.join(", ")) };
        bail!(
            "recorded no usable calls to {} out of {}. Arguments and results must be plain data: numbers, strings, bytes, lists, tuples, dicts, sets{types}",
            hs.id,
            rec.calls
        );
    }
    let enabled = ctx.state.ports.get(&hs.id).is_some_and(|p| p.enabled);
    let helpers: Vec<Hotspot> = hs
        .calls
        .iter()
        .filter_map(|name| hotspot::helpers_named(&ctx.state.hotspots, &hs, name).first().copied())
        .cloned()
        .collect();
    let text = fs::read_to_string(ctx.root.join(&hs.file)).unwrap_or_default();
    let sources: Vec<&str> = std::iter::once(&hs).chain(&helpers).map(|h| h.source.as_str()).collect();
    let port = Port {
        id: hs.id.clone(),
        file: hs.file.clone(),
        module,
        func: hs.name.clone(),
        ext: format!("fa_{}", util::slug(&hs.id)),
        crate_dir: crate_rel,
        recorded: rec.kept,
        calls_seen: rec.calls,
        verify: None,
        enabled,
        lang,
        owner: hs.owner.clone(),
        line: hs.line,
        sources: crate::state::verbatim(&text, &sources),
        backend,
    };
    backend::hold_out(&dir.join("recordings.jsonl"), &ctx.holdout(&port))?;
    let task = lang.scaffold(&ctx, &hs, &helpers, &port)?;
    ctx.state.ports.insert(port.id.clone(), port);
    ctx.save()?;
    Ok(PortOut {
        id: hs.id,
        lib_rs: match backend {
            Backend::Cpp => dir.join("port.cpp"),
            Backend::Rust => dir.join("src").join(if lang.c_bridge() { "port.rs" } else { "lib.rs" }),
        },
        task_md: dir.join("TASK.md"),
        crate_dir: dir,
        recorded: rec.kept,
        calls_seen: rec.calls,
        skipped: rec.skipped,
        task,
        backend,
    })
}

pub fn verify(root: &Path, id: Option<&str>) -> Result<(String, Verify)> {
    let mut ctx = Ctx::open(root)?;
    let id = ctx.resolve_port(id)?;
    let port = ctx.state.ports[&id].clone();
    let source_hash = backend::port_hash(&ctx.crate_dir(&port), port.backend)?;
    if ctx.stale(&port) {
        ui::warn(format!("{} changed since {id} was recorded: {}", port.file, stale_hint(&id)));
    }
    ui::step(format!("building {} (release)", ui::bold(&port.ext)));
    let v = match backend::build(&ctx, &port)? {
        Err(log) => Verify { built: false, build_log: Some(log), ..Verify::default() },
        Ok(_) => {
            ui::step(format!(
                "replaying {} recorded call{} against the {} port",
                port.recorded,
                if port.recorded == 1 { "" } else { "s" },
                port.backend.label()
            ));
            let visible = util::count_lines(&ctx.crate_dir(&port).join("recordings.jsonl"));
            let mut v = port.lang.verify(&ctx, &port)?;
            backend::finish_replay(&ctx, &port, visible, &mut v)?;
            v
        }
    };
    let mut v = v;
    v.lang = port.lang;
    v.source_hash = Some(source_hash);
    v.backend = port.backend;
    if !v.ok() && port.enabled {
        port.lang.disable(&ctx, &port)?;
        if let Some(p) = ctx.state.ports.get_mut(&id) {
            p.enabled = false;
        }
        ui::warn(format!("{id} was enabled; switched it back to {} until it passes again", port.lang.label()));
    }
    if let Some(p) = ctx.state.ports.get_mut(&id) {
        p.verify = Some(v.clone());
    }
    ctx.save()?;
    Ok((id, v))
}

/// Verify every port; an enabled one that fails is switched back, as with a single verify.
pub fn verify_all(root: &Path) -> Result<Vec<(String, Verify)>> {
    let ids: Vec<String> = Ctx::open(root)?.state.ports.keys().cloned().collect();
    ensure!(!ids.is_empty(), "nothing ported yet: run  fast-anything port");
    let mut results = Vec::with_capacity(ids.len());
    for id in &ids {
        let (id, v) = verify(root, Some(id))?;
        if !ui::quiet() {
            show::verify(&id, &v);
        }
        results.push((id, v));
    }
    Ok(results)
}

/// What to do about a port whose original changed after it was recorded.
pub fn stale_hint(id: &str) -> String {
    format!(
        "its recorded calls and port may be out of date. Run  fast-anything port {id}  to record again, update the port, then verify (or --force)"
    )
}

/// What to do about a port that's right but slower than the original.
pub fn slower_hint() -> &'static str {
    "calling into the port costs more than this function's work, so it won't be switched on. Port its caller instead (fewer, bigger calls), or  enable --force"
}

/// Why a port too cheap to time (see Verify::too_cheap) isn't switched on.
pub fn too_cheap_hint(id: &str, v: &Verify) -> String {
    let m = v.micro.unwrap_or_default();
    format!(
        "{id} takes about as long as reading the clock ({} per call in {}, the clock {}), so verify can't tell whether the port saves anything: {}",
        util::nanos(m.original_ns),
        v.lang.label(),
        util::nanos(m.clock_ns),
        slower_hint()
    )
}

/// Whether the crate is not exactly the one covered by the last verify. Old state without a hash
/// keeps its legacy behavior; re-porting clears verify and therefore always needs a new one.
fn crate_needs_verify(ctx: &Ctx, port: &Port) -> Result<bool> {
    let Some(verify) = &port.verify else { return Ok(true) };
    let Some(expected) = verify.source_hash.as_deref() else { return Ok(false) };
    Ok(backend::port_hash(&ctx.crate_dir(port), port.backend)? != expected)
}

pub fn enable(root: &Path, id: Option<&str>, force: bool) -> Result<String> {
    let mut ctx = Ctx::open(root)?;
    let id = ctx.resolve_port(id)?;
    let port = ctx.state.ports[&id].clone();
    if ctx.stale(&port) && !force {
        bail!("{} changed since {id} was recorded: {}", port.file, stale_hint(&id));
    }
    let verified = port.verify.as_ref().is_some_and(Verify::ok);
    if !verified && !force {
        bail!("{id} hasn't passed verify yet: run  fast-anything verify {id}  (or --force)");
    }
    if !force && crate_needs_verify(&ctx, &port)? {
        bail!(
            "{id}'s {} changed since it passed verify: run  fast-anything verify {id}  (or --force)",
            port.backend.label()
        );
    }
    // too cheap to time comes first: a few-ns function measures 1.0x either way, so "slower" is noise
    if let Some(v) = port.verify.as_ref().filter(|v| v.too_cheap() && !force) {
        bail!("{}", too_cheap_hint(&id, v));
    }
    if let Some(m) = port.verify.as_ref().filter(|v| v.slower() && !force).and_then(|v| v.micro) {
        bail!(
            "{id} is {} slower per call in {}: {}",
            util::times(1.0 / m.speedup),
            port.backend.label(),
            slower_hint()
        );
    }
    port.lang.enable(&ctx, &port)?;
    if let Some(p) = ctx.state.ports.get_mut(&id) {
        p.enabled = true;
    }
    ctx.save()?;
    Ok(id)
}

fn markers_for_port(port: &Port) -> Vec<(String, String)> {
    let (comment, ids): (&str, Vec<String>) = match port.lang {
        Lang::Python | Lang::Ruby | Lang::R => ("#", vec![port.id.clone()]),
        Lang::Php | Lang::C | Lang::Cpp => ("//", vec![port.id.clone(), format!("{} bridge", port.id)]),
        _ => ("//", vec![port.id.clone()]),
    };
    ids.iter().map(|id| backend::markers(comment, id)).collect()
}

pub fn disable(root: &Path, id: Option<&str>) -> Result<String> {
    let mut ctx = Ctx::open(root)?;
    let id = ctx.resolve_port(id)?;
    let port = &ctx.state.ports[&id];
    let file = ctx.root.join(&port.file);
    let before = fs::read_to_string(&file).with_context(|| format!("reading {}", port.file))?;
    for (begin, end) in markers_for_port(port) {
        ensure!(
            before.contains(&begin) == before.contains(&end),
            "the fast-anything block for {id} is incomplete; restore its begin/end markers before disabling"
        );
    }
    port.lang.disable(&ctx, port)?;
    let after = fs::read_to_string(&file).with_context(|| format!("reading {}", port.file))?;
    for (begin, end) in markers_for_port(port) {
        ensure!(!after.contains(&begin) && !after.contains(&end), "the fast-anything block for {id} wasn't removed");
    }
    if let Some(p) = ctx.state.ports.get_mut(&id) {
        p.enabled = false;
    }
    ctx.save()?;
    Ok(id)
}

/// Switch every enabled port back to the original code; returns their ids.
pub fn disable_all(root: &Path) -> Result<Vec<String>> {
    let ids: Vec<String> = Ctx::open(root)?.state.ports.values().filter(|p| p.enabled).map(|p| p.id.clone()).collect();
    for id in &ids {
        disable(root, Some(id))?;
    }
    Ok(ids)
}

pub fn build_all(root: &Path) -> Result<()> {
    let ctx = Ctx::open(root)?;
    ensure!(!ctx.state.ports.is_empty(), "nothing ported yet");
    let mut failed = 0;
    for port in ctx.state.ports.values() {
        if port.enabled && crate_needs_verify(&ctx, port)? {
            failed += 1;
            ui::error(format!(
                "{} is switched on and its {} port hasn't passed verify in its current state; run  fast-anything verify {}",
                port.id,
                port.backend.label(),
                port.id
            ));
            continue;
        }
        ui::step(format!("building {}", ui::bold(&port.id)));
        match backend::build(&ctx, port)? {
            Ok(path) => ui::ok(format!("{}", path.display())),
            Err(log) => {
                failed += 1;
                ui::error(format!("{} doesn't compile:\n{log}", port.id));
            }
        }
    }
    ensure!(failed == 0, "{failed} port(s) didn't build");
    Ok(())
}

/// Remove the build cache (.fast/target) or reset the entire .fast directory.
pub fn clean(root: &Path, all: bool) -> Result<()> {
    let fast_dir = root.join(".fast");
    if !fast_dir.exists() {
        ui::ok("nothing to clean (.fast folder does not exist)");
        return Ok(());
    }

    if all {
        let disabled = disable_all(root)?;
        for id in disabled {
            ui::ok(format!("{} is back to the original code", ui::bold(&id)));
        }
        if fast_dir.exists() {
            fs::remove_dir_all(&fast_dir).with_context(|| format!("removing {}", fast_dir.display()))?;
        }
        ui::ok("removed .fast and reset all state");
    } else {
        let target_dir = fast_dir.join("target");
        if target_dir.exists() {
            fs::remove_dir_all(&target_dir).with_context(|| format!("removing {}", target_dir.display()))?;
            ui::ok("removed .fast/target build cache");
        } else {
            ui::ok(".fast/target build cache is already clean");
        }
    }
    Ok(())
}

pub fn bench(root: &Path, runs: usize) -> Result<BenchResult> {
    let mut ctx = Ctx::open(root)?;
    for p in ctx.state.ports.values().filter(|p| p.enabled && ctx.stale(p)) {
        ui::warn(format!("{} changed since {} was recorded: {}", p.file, p.id, stale_hint(&p.id)));
    }
    let result = bench::run(&ctx, runs.max(1))?;
    ctx.state.bench = Some(result.clone());
    ctx.save()?;
    Ok(result)
}

pub fn report(root: &Path) -> Result<ReportOut> {
    report::write(&Ctx::open(root)?)
}

/// The file the agent edits, as feedback names it.
fn port_file(v: &Verify) -> &'static str {
    match v.backend {
        Backend::Cpp => "port.cpp",
        Backend::Rust if v.lang.c_bridge() => "src/port.rs",
        Backend::Rust => "src/lib.rs",
    }
}

/// What the agent gets told after a failed attempt, or a right one that's slower than the original.
pub fn feedback(v: &Verify) -> String {
    if v.ok()
        && let Some(m) = v.micro.filter(|_| v.slower())
    {
        let advice = match v.backend {
            Backend::Cpp => {
                "take inputs by const reference, no allocation or copying per call, `reserve`, fast paths for common cases, hot loops the compiler can vectorize"
            }
            Backend::Rust => {
                "borrow inputs instead of converting them, no allocation or cloning per call, `with_capacity`, fast paths for common cases"
            }
        };
        return format!(
            "\n\n## Your last attempt is right but slow\n\n\
             All {} cases match, but a call takes {} in {} against {} in the original ({} slower).\n\
             Make it faster without changing any result: {advice}.\n\nFix {}.\n",
            v.total,
            util::nanos(m.rust_ns),
            v.backend.label(),
            util::nanos(m.original_ns),
            util::times(1.0 / m.speedup),
            port_file(v)
        );
    }
    let mut out = String::from("\n\n## Your last attempt failed\n\n");
    if let Some(log) = &v.build_log {
        out.push_str("It doesn't compile:\n\n```\n");
        out.push_str(log);
        out.push_str("\n```\n");
    } else {
        out.push_str(&format!("{} of {} recorded calls came out different:\n\n", v.failed, v.total));
        for f in &v.failures {
            out.push_str(&format!("- `{}` should give `{}`, the port gave `{}`\n", f.call, f.expected, f.got));
        }
        if v.failures.iter().any(|f| f.call.starts_with("(held out) ")) {
            out.push_str(
                "\nCalls marked (held out) aren't in recordings.jsonl: port the logic, not the calls you were shown.\n",
            );
        }
    }
    out.push_str(&format!("\nFix {}.\n", port_file(v)));
    out
}

pub fn agent_port(root: &Path, id: &str, agent_spec: &str, attempts: usize) -> Result<Verify> {
    let attempts = attempts.max(1);
    let mut last: Option<Verify> = None;
    // A port that's right but slower goes back to the agent to speed up; if no later attempt is both
    // right and faster, its sources are put back. One too cheap to time can't be shown faster.
    let mut right: Option<(Verify, Snapshot)> = None;
    for attempt in 1..=attempts {
        let ctx = Ctx::open(root)?;
        let port = ctx.state.ports.get(id).with_context(|| format!("{id} isn't ported"))?.clone();
        let dir = ctx.crate_dir(&port);
        let mut prompt = fs::read_to_string(dir.join("TASK.md")).context("reading TASK.md")?;
        if let Some(v) = &last {
            prompt.push_str(&feedback(v));
        }
        ui::step(format!(
            "{} is writing the {} port (attempt {attempt}/{attempts})",
            ui::bold(agent_spec),
            port.backend.label()
        ));
        agent::run(agent_spec, &dir, &prompt, &dir.join("agent.log"))?;
        let (_, v) = verify(root, Some(id))?;
        if !ui::quiet() {
            show::verify(id, &v);
        }
        if v.ok() && (!v.slower() || v.too_cheap()) {
            return Ok(v);
        }
        if v.ok() && right.as_ref().is_none_or(|(best, _)| speedup(&v) > speedup(best)) {
            right = Some((v.clone(), snapshot(&dir, port.backend)?));
        }
        last = Some(v);
    }
    if let Some((best, files)) = right
        && last.as_ref().is_none_or(|v| !v.ok() || speedup(v) < speedup(&best))
    {
        ui::step(format!("putting back the right attempt of {id} ({} per call)", util::times(speedup(&best))));
        let ctx = Ctx::open(root)?;
        let port = ctx.state.ports.get(id).with_context(|| format!("{id} isn't ported"))?;
        restore_snapshot(&ctx.crate_dir(port), &files, port.backend)?;
        let (_, v) = verify(root, Some(id))?;
        return Ok(v);
    }
    Ok(last.unwrap_or_default())
}

fn speedup(v: &Verify) -> f64 {
    v.micro.map(|m| m.speedup).unwrap_or(0.0)
}

/// Port sources for one attempt (Cargo inputs, or the C++ sources); recordings, TASK.md and
/// agent.log are deliberately excluded.
type Snapshot = Vec<(PathBuf, Vec<u8>)>;

fn snapshot_files(dir: &Path, out: &mut Snapshot) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.is_dir() {
            snapshot_files(&path, out)?;
        } else {
            out.push((path.clone(), fs::read(&path)?));
        }
    }
    Ok(())
}

fn snapshot(dir: &Path, backend: Backend) -> Result<Snapshot> {
    let mut out = vec![];
    if backend == Backend::Cpp {
        for path in crate::cpp::source_files(dir)? {
            out.push((path.clone(), fs::read(&path)?));
        }
        return Ok(out);
    }
    for name in ["Cargo.toml", "Cargo.lock", "build.rs"] {
        let path = dir.join(name);
        if path.is_file() {
            out.push((path.clone(), fs::read(path)?));
        }
    }
    let src = dir.join("src");
    if src.is_dir() {
        snapshot_files(&src, &mut out)?;
    }
    Ok(out)
}

fn restore_snapshot(dir: &Path, files: &Snapshot, backend: Backend) -> Result<()> {
    if backend == Backend::Cpp {
        // sources an attempt added after the snapshot go too
        for path in crate::cpp::source_files(dir)? {
            if !files.iter().any(|(saved, _)| saved == &path) {
                fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
            }
        }
    } else {
        for name in ["Cargo.lock", "build.rs"] {
            let path = dir.join(name);
            if !files.iter().any(|(saved, _)| saved == &path) {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e).with_context(|| format!("removing {}", path.display())),
                }
            }
        }
        let src = dir.join("src");
        if src.exists() {
            fs::remove_dir_all(&src)?;
        }
    }
    for (path, bytes) in files {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, bytes)?;
    }
    Ok(())
}

/// `backend`: what the ports are written in (default: Backend::choose).
#[allow(clippy::too_many_arguments)]
pub fn go(
    root: &Path,
    command: Vec<String>,
    agent_spec: Option<&str>,
    top: usize,
    attempts: usize,
    runs: usize,
    samples: usize,
    backend: Option<Backend>,
) -> Result<()> {
    let scanned = scan(root, command, None)?;
    show::scan(&scanned);
    let picks: Vec<String> =
        scanned.hotspots.iter().filter(|h| h.worth_it()).take(top.max(1)).map(|h| h.id.clone()).collect();
    ensure!(
        !picks.is_empty(),
        "nothing worth porting: no portable function takes {}% or more of the run",
        hotspot::WORTH_IT * 100.0
    );

    let Some(agent_spec) = agent_spec else {
        let mut label = "Rust";
        for id in &picks {
            let ported = port_with(root, Some(id), samples, backend)?;
            label = ported.backend.label();
            show::port(&ported, false);
        }
        ui::line("");
        ui::line(format!(
            "  Now write the {label}: hand each TASK.md to your coding agent (or pass {}), then:",
            ui::bold("--agent claude")
        ));
        for id in &picks {
            show::next(&format!("fast-anything verify {id} && fast-anything enable {id}"));
        }
        show::next("fast-anything bench");
        return Ok(());
    };

    let mut enabled = 0;
    for id in &picks {
        let ported = port_with(root, Some(id), samples, backend)?;
        let label = ported.backend.label();
        let file = ported.lib_rs.strip_prefix(&ported.crate_dir).unwrap_or(&ported.lib_rs).display().to_string();
        show::port(&ported, false);
        let v = agent_port(root, id, agent_spec, attempts)?;
        if v.ok() && v.too_cheap() {
            ui::warn(too_cheap_hint(id, &v));
        } else if v.ok() && v.slower() {
            ui::warn(format!("{id}: right, but slower in {label} per call; {}", slower_hint()));
        } else if v.ok() {
            enable(root, Some(id), false)?;
            ui::ok(format!("{} now runs in {label}", ui::bold(id)));
            enabled += 1;
        } else {
            ui::warn(format!(
                "{id}: not right after {attempts} attempts; fix {file} by hand (see TASK.md), then  fast-anything verify {id}"
            ));
        }
    }
    ensure!(enabled > 0, "no port both passed verification and ran faster");
    show::bench(&bench(root, runs)?);
    show::report(&report(root)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Micro;

    #[test]
    fn slow_but_right_gets_asked_for_speed() {
        let v = Verify {
            built: true,
            total: 10,
            passed: 10,
            micro: Some(Micro { original_ns: 1000.0, rust_ns: 4000.0, speedup: 0.25, clock_ns: 20.0 }),
            ..Verify::default()
        };
        let text = feedback(&v);
        assert!(
            text.contains("right but slow") && text.contains("All 10 cases match") && text.contains("4.0× slower"),
            "{text}"
        );
        let wrong = Verify { failed: 1, ..v };
        assert!(feedback(&wrong).contains("failed"));
    }

    #[test]
    fn feedback_never_shows_a_held_out_call() {
        let failure = |case: usize, value: &str| crate::state::Failure {
            case,
            call: format!("f({value}, \"secret-{value}\")"),
            expected: format!("{value}.25"),
            got: format!("{value}.5"),
        };
        let mut v = Verify {
            built: true,
            total: 600,
            held_out: 100,
            failed: 2,
            failures: vec![failure(7, "31"), failure(450, "8675309")],
            ..Verify::default()
        };
        backend::redact_held_out(&mut v, "f", 400);
        let text = feedback(&v);
        assert!(text.contains("`(held out) f(…)`") && text.contains("port the logic"), "{text}");
        assert!(!text.contains("8675309"), "{text}");
        assert!(text.contains("f(31, \"secret-31\")") && text.contains("31.25"), "{text}");
    }

    fn project(name: &str) -> (PathBuf, Port) {
        let root = std::env::temp_dir().join(format!("fa-pipeline-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let crate_dir = ".fast/ports/demo";
        fs::create_dir_all(root.join(crate_dir).join("src")).unwrap();
        fs::write(root.join(crate_dir).join("Cargo.toml"), "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n")
            .unwrap();
        fs::write(root.join(crate_dir).join("src/lib.rs"), "pub fn f() {}\n").unwrap();
        fs::write(root.join("app.py"), "def f():\n    pass\n").unwrap();
        let port = Port {
            id: "demo".into(),
            file: "app.py".into(),
            module: "app".into(),
            func: "f".into(),
            ext: "fa_demo".into(),
            crate_dir: crate_dir.into(),
            recorded: 1,
            calls_seen: 1,
            verify: None,
            enabled: false,
            lang: Lang::Python,
            owner: None,
            line: 1,
            sources: vec![],
            backend: crate::backend::Backend::Rust,
        };
        (root, port)
    }

    #[test]
    fn enable_requires_the_crate_that_passed_verify() {
        let (root, mut port) = project("enable-hash");
        let hash = backend::source_hash(&root.join(&port.crate_dir)).unwrap();
        fs::write(root.join(&port.crate_dir).join("src/lib.rs"), "pub fn f() { panic!() }\n").unwrap();
        port.verify = Some(Verify { built: true, total: 1, passed: 1, source_hash: Some(hash), ..Verify::default() });
        let mut ctx = Ctx::open(&root).unwrap();
        ctx.state.ports.insert(port.id.clone(), port);
        ctx.save().unwrap();

        let err = enable(&root, Some("demo"), false).unwrap_err();
        assert!(err.to_string().contains("Rust changed since it passed verify"), "{err}");
        assert!(!Ctx::open(&root).unwrap().state.ports["demo"].enabled);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn live_build_guard_covers_missing_changed_and_legacy_verify() {
        let (root, mut port) = project("build-hash");
        let ctx = Ctx::open(&root).unwrap();
        assert!(crate_needs_verify(&ctx, &port).unwrap());

        port.verify = Some(Verify::default());
        assert!(!crate_needs_verify(&ctx, &port).unwrap(), "legacy verify without a hash keeps working");

        let hash = backend::source_hash(&root.join(&port.crate_dir)).unwrap();
        port.verify = Some(Verify { source_hash: Some(hash), ..Verify::default() });
        assert!(!crate_needs_verify(&ctx, &port).unwrap());
        fs::write(root.join(&port.crate_dir).join("Cargo.toml"), "not a valid manifest\n").unwrap();
        assert!(crate_needs_verify(&ctx, &port).unwrap());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn snapshot_restores_crate_inputs_without_touching_run_artifacts() {
        let dir = std::env::temp_dir().join(format!("fa-agent-snapshot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src/nested")).unwrap();
        fs::write(dir.join("Cargo.toml"), "best manifest\n").unwrap();
        fs::write(dir.join("Cargo.lock"), "best lock\n").unwrap();
        fs::write(dir.join("src/lib.rs"), "best lib\n").unwrap();
        fs::write(dir.join("src/nested/helper.rs"), "best helper\n").unwrap();
        for name in ["TASK.md", "recordings.jsonl", "agent.log"] {
            fs::write(dir.join(name), "before\n").unwrap();
        }
        let saved = snapshot(&dir, Backend::Rust).unwrap();

        fs::write(dir.join("Cargo.toml"), "later manifest\n").unwrap();
        fs::write(dir.join("Cargo.lock"), "later lock\n").unwrap();
        fs::write(dir.join("src/lib.rs"), "later lib\n").unwrap();
        fs::write(dir.join("src/extra.rs"), "later extra\n").unwrap();
        fs::write(dir.join("build.rs"), "later build script\n").unwrap();
        fs::remove_file(dir.join("src/nested/helper.rs")).unwrap();
        for name in ["TASK.md", "recordings.jsonl", "agent.log"] {
            fs::write(dir.join(name), "later evidence\n").unwrap();
        }

        restore_snapshot(&dir, &saved, Backend::Rust).unwrap();
        assert_eq!(fs::read_to_string(dir.join("Cargo.toml")).unwrap(), "best manifest\n");
        assert_eq!(fs::read_to_string(dir.join("Cargo.lock")).unwrap(), "best lock\n");
        assert_eq!(fs::read_to_string(dir.join("src/lib.rs")).unwrap(), "best lib\n");
        assert_eq!(fs::read_to_string(dir.join("src/nested/helper.rs")).unwrap(), "best helper\n");
        assert!(!dir.join("src/extra.rs").exists());
        assert!(!dir.join("build.rs").exists());
        for name in ["TASK.md", "recordings.jsonl", "agent.log"] {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), "later evidence\n");
        }
        let _ = fs::remove_dir_all(dir);
    }

    fn enabled_project(name: &str, text: &str) -> (PathBuf, String) {
        let root = std::env::temp_dir().join(format!("fa-disable-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("app.py"), text).unwrap();
        let port = Port {
            id: "demo".into(),
            file: "app.py".into(),
            module: "app".into(),
            func: "f".into(),
            ext: "fa_demo".into(),
            crate_dir: ".fast/ports/demo".into(),
            recorded: 1,
            calls_seen: 1,
            verify: None,
            enabled: true,
            lang: Lang::Python,
            owner: None,
            line: 1,
            sources: vec![],
            backend: crate::backend::Backend::Rust,
        };
        let id = port.id.clone();
        let mut ctx = Ctx::open(&root).unwrap();
        ctx.state.ports.insert(port.id.clone(), port);
        ctx.save().unwrap();
        (root, id)
    }

    #[test]
    fn disable_rejects_damaged_blocks_without_changing_state() {
        let (begin, end) = backend::markers("#", "demo");
        for (name, text) in [("missing-end", format!("{begin}\nf = rust\n")), ("missing-begin", format!("{end}\n"))] {
            let (root, id) = enabled_project(name, &text);
            let err = disable(&root, Some(&id)).unwrap_err();
            assert!(err.to_string().contains("incomplete"), "{err}");
            assert_eq!(fs::read_to_string(root.join("app.py")).unwrap(), text);
            assert!(Ctx::open(&root).unwrap().state.ports[&id].enabled);
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn disable_removes_a_complete_block() {
        let (begin, end) = backend::markers("#", "demo");
        let original = "def f():\n    pass\n";
        let text = format!("{original}\n{begin}\nf = rust\n{end}\n");
        let (root, id) = enabled_project("complete", &text);
        disable(&root, Some(&id)).unwrap();
        assert_eq!(fs::read_to_string(root.join("app.py")).unwrap(), original);
        assert!(!Ctx::open(&root).unwrap().state.ports[&id].enabled);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clean_removes_target_cache() {
        let root = std::env::temp_dir().join(format!("fa-clean-cache-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let fast_dir = root.join(".fast");
        let target_dir = fast_dir.join("target/release");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("artifact"), "bytes").unwrap();
        fs::write(fast_dir.join("state.json"), "{}").unwrap();

        assert!(target_dir.exists());
        clean(&root, false).unwrap();
        assert!(!target_dir.exists());
        assert!(fast_dir.exists());

        // Calling clean again when target is already gone is a harmless no-op
        clean(&root, false).unwrap();
        assert!(!target_dir.exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clean_all_disables_ports_and_removes_fast_dir() {
        let (begin, end) = backend::markers("#", "demo");
        let original = "def f():\n    pass\n";
        let text = format!("{original}\n{begin}\nf = rust\n{end}\n");
        let (root, _id) = enabled_project("clean-all", &text);
        let target_dir = root.join(".fast/target");
        fs::create_dir_all(&target_dir).unwrap();
        fs::write(target_dir.join("dummy.o"), "dummy").unwrap();

        clean(&root, true).unwrap();
        assert_eq!(fs::read_to_string(root.join("app.py")).unwrap(), original);
        assert!(!root.join(".fast").exists());

        // Calling clean on a non-existent .fast folder is a harmless no-op
        clean(&root, true).unwrap();
        assert!(!root.join(".fast").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clean_all_aborts_if_disable_fails_preserving_fast_dir() {
        let (begin, _end) = backend::markers("#", "demo");
        let text = format!("{begin}\nf = rust\n");
        let (root, id) = enabled_project("damaged-clean", &text);

        let res = clean(&root, true);
        assert!(res.is_err(), "clean --all must abort when disable fails");
        assert!(root.join(".fast").exists(), ".fast must not be deleted if disable fails");
        assert!(Ctx::open(&root).unwrap().state.ports[&id].enabled);

        let _ = fs::remove_dir_all(root);
    }
}
