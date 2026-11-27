//! How results look in the terminal.

use crate::bench::{BenchResult, OUTPUT_VARIES, Verdict};
use crate::pipeline::{PortOut, ScanOut};
use crate::report::ReportOut;
use crate::state::{Ctx, Verify};
use crate::ui::{self, bold, dim, green, red, rust, yellow};
use crate::util::{nanos, secs, thousands, times};
use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};

fn bar(share: f64, width: usize) -> String {
    const PARTS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let eighths = (share.clamp(0.0, 1.0) * (width * 8) as f64).round() as usize;
    let mut s = "█".repeat(eighths / 8);
    if !eighths.is_multiple_of(8) {
        s.push(PARTS[eighths % 8 - 1]);
    }
    format!("{s:<width$}")
}

fn pad(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len > n {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    } else {
        format!("{s}{}", " ".repeat(n - len))
    }
}

/// Fit a function id into n columns, trimming the leading module prefix when it's too long
/// so the function name at the end stays readable.
fn pad_id(s: &str, n: usize) -> String {
    let len = s.chars().count();
    if len > n { format!("…{}", s.chars().skip(len - (n - 1)).collect::<String>()) } else { pad(s, n) }
}

pub fn next(cmd: &str) {
    ui::line(format!("  {} {}", dim("→"), bold(cmd)));
}

pub fn scan(s: &ScanOut) {
    ui::ok(format!("{} ran in {}", bold(s.command.join(" ")), secs(s.wall)));
    ui::line("");
    if s.hotspots.is_empty() {
        ui::line("  No functions from your own code showed up. Is the app doing its work in a library?");
        for n in &s.notes {
            ui::line(format!("  {}", yellow(format!("! {n}"))));
        }
        return;
    }
    ui::line(dim(format!("   #  {}  {:<16}{:>11}  verdict", pad("function", 30), "time", "calls")));
    for (i, h) in s.hotspots.iter().take(10).enumerate() {
        let verdict = if let Some(by) = &h.covered_by {
            dim(format!("↳ ported inside {}", by.rsplit(['.', ':']).next().unwrap_or(by)))
        } else if h.worth_it() {
            green("✓ port it")
        } else if h.portable() {
            dim("· too small to matter")
        } else {
            dim(format!("✗ {}", h.blockers[0]))
        };
        let row = format!(
            "  {:>2}  {}  {} {:>5.1}%{:>11}  {verdict}",
            i + 1,
            pad_id(&h.id, 30),
            rust(bar(h.share, 9)),
            h.share * 100.0,
            if h.ncalls == 0 { "-".to_string() } else { thousands(h.ncalls) }
        );
        ui::line(if h.worth_it() { row } else { dim(row) });
        if h.worth_it() {
            for w in &h.warnings {
                ui::line(format!("{}{}", " ".repeat(36), yellow(format!("! {w}"))));
            }
        }
    }
    ui::line("");
    for n in &s.notes {
        ui::line(format!("  {}", yellow(format!("! {n}"))));
    }
    if !s.notes.is_empty() {
        ui::line("");
    }
    match s.hotspots.iter().find(|h| h.worth_it()) {
        Some(h) => next(&format!("fast-anything port {}", h.id)),
        None => {
            ui::line("  Nothing portable takes a real share of the run yet. The ✗ reasons above say what's in the way.")
        }
    }
}

pub fn port(p: &PortOut, show_next: bool) {
    ui::ok(format!(
        "recorded {} real call{} to {} (of {} made){}",
        thousands(p.recorded as u64),
        if p.recorded == 1 { "" } else { "s" },
        bold(&p.id),
        thousands(p.calls_seen),
        if p.skipped > 0 { format!(", skipped {} with non-plain data", p.skipped) } else { String::new() }
    ));
    let what = match p.backend {
        crate::backend::Backend::Cpp => "C++ port",
        crate::backend::Backend::Rust => "Rust crate",
    };
    ui::ok(format!("{what} ready: {}", p.crate_dir.display()));
    if show_next {
        let file = p.lib_rs.strip_prefix(&p.crate_dir).unwrap_or(&p.lib_rs).to_string_lossy().replace('\\', "/");
        ui::line(format!("  Have your coding agent follow {} and write {}, then:", bold("TASK.md"), bold(file)));
        next(&format!("fast-anything verify {}", p.id));
    }
}

pub fn verify(id: &str, v: &Verify) {
    if let Some(log) = &v.build_log {
        ui::error(format!("{} doesn't compile yet:", bold(id)));
        for line in log.lines().take(25) {
            ui::line(format!("    {}", dim(line)));
        }
        return;
    }
    if v.ok() {
        if v.derived + v.held_out > 0 {
            let mut parts = vec![format!("{} recorded", v.total - v.derived - v.held_out)];
            if v.held_out > 0 {
                parts.push(format!("{} held out", v.held_out));
            }
            if v.edges > 0 {
                parts.push(format!("{} edge cases", v.edges));
                parts.push(format!("{} derived", v.derived - v.edges));
            } else if v.derived > 0 {
                parts.push(format!("{} derived", v.derived));
            }
            ui::ok(format!(
                "{}/{} calls give the same result {}",
                v.passed,
                v.total,
                dim(format!("({})", parts.join(" + ")))
            ));
        } else {
            ui::ok(format!("{}/{} recorded calls give the same result", v.passed, v.total));
        }
        if v.too_cheap() {
            ui::warn(crate::pipeline::too_cheap_hint(&bold(id), v));
        } else if let Some(m) = v.micro {
            ui::ok(format!(
                "{} per call: {} in {}, {} in {}: {}",
                bold(id),
                nanos(m.original_ns),
                v.lang.label(),
                nanos(m.rust_ns),
                v.backend.label(),
                if v.slower() {
                    red(bold(format!("{} slower", times(1.0 / m.speedup))))
                } else {
                    rust(bold(format!("{} faster", times(m.speedup))))
                }
            ));
            if v.slower() {
                ui::warn(crate::pipeline::slower_hint());
            }
        }
        return;
    }
    ui::error(format!("{}/{} calls don't match", v.failed, v.total));
    for f in v.failures.iter().take(5) {
        ui::line(format!("    {}", f.call));
        ui::line(format!("      expected {}", green(&f.expected)));
        ui::line(format!("      got      {}", red(&f.got)));
    }
}

pub fn bench(b: &BenchResult) {
    let cmd = b.command.join(" ");
    ui::line("");
    ui::line(format!("  {}  {} {}", pad(b.lang.label(), 11), dim(bar(1.0, 24)), secs(b.before.median)));
    ui::line(format!(
        "  {}  {} {}",
        pad("Ported", 11),
        rust(bar(b.after.median / b.before.median.max(1e-12), 24)),
        secs(b.after.median)
    ));
    ui::line("");
    let (word, x, lo, hi) = if b.speedup >= 1.0 {
        ("faster", b.speedup, b.speedup_q1, b.speedup_q3)
    } else {
        ("slower", 1.0 / b.speedup, 1.0 / b.speedup_q3, 1.0 / b.speedup_q1)
    };
    let headline = bold(format!("{} {word}", times(x)));
    let mut detail = format!("median of {} rounds, {}–{}", b.runs, times(lo).trim_end_matches('×'), times(hi));
    if let Some(cpu) = b.cpu_note() {
        detail.push_str(&format!("; {cpu}"));
    }
    let noise = b.aa.band();
    detail.push_str(&format!("; A/A ±{noise:.1}%"));
    ui::ok(format!(
        "{} is {} end to end {}",
        bold(cmd),
        if word == "faster" { rust(headline) } else { red(headline) },
        dim(format!("({detail})"))
    ));
    match b.verdict {
        Some(Verdict::Noise) => ui::warn(format!(
            "that's within run-to-run noise: the original alone varies by up to ±{noise:.1}% between runs. More rounds may settle it:  fast-anything bench --runs 15"
        )),
        Some(Verdict::Slower) => ui::warn(format!(
            "the app is slower with the ports, beyond its own run-to-run noise (±{noise:.1}%). See which port costs more than it saves:  fast-anything status"
        )),
        Some(Verdict::Faster) | None => {}
    }
    match &b.output_diff {
        Some(diff) if diff == OUTPUT_VARIES => ui::warn(diff),
        Some(diff) => ui::warn(format!(
            "the app printed something else with the ports ({diff}), while every run of the original printed the same: a port behaves differently from the original"
        )),
        None => {}
    }
}

pub fn report(r: &ReportOut) {
    ui::ok(format!("report: {}", r.markdown.display()));
    ui::ok(format!("share card: {}", r.svg.display()));
}

pub fn status(ctx: &Ctx) {
    let s = &ctx.state;
    if s.command.is_empty() {
        ui::line("  No scan yet.");
        next("fast-anything scan -- python your_app.py");
        return;
    }
    ui::line(format!("  app: {}   profiled run: {}", bold(s.command.join(" ")), secs(s.wall)));
    if s.ports.is_empty() {
        ui::line("  Nothing ported yet.");
        if let Some(h) = s.hotspots.iter().find(|h| h.worth_it()) {
            next(&format!("fast-anything port {}", h.id));
        }
        return;
    }
    ui::line("");
    for p in s.ports.values() {
        let on = format!("● on ({})", p.backend.label());
        let waiting = format!("○ waiting for the {}", p.backend.label());
        let (label, paint): (&str, fn(String) -> String) = match (&p.verify, p.enabled) {
            (_, true) => (&on, |s| green(s)),
            (Some(v), false) if v.ok() => ("○ verified, off", |s| yellow(s)),
            (Some(v), false) if !v.built => ("✗ doesn't compile", |s| red(s)),
            (Some(_), false) => ("✗ mismatches", |s| red(s)),
            (None, false) => (&waiting, |s| dim(s)),
        };
        let state = paint(pad(label, 26));
        let speed = p
            .verify
            .as_ref()
            .and_then(|v| v.micro)
            .map(|m| format!("{} per call", times(m.speedup)))
            .unwrap_or_default();
        ui::line(format!("  {}  {}  {}", pad_id(&p.id, 32), state, dim(speed)));
        if ctx.stale(p) {
            ui::line(format!("  {}  {}", pad("", 32), red(format!("! {} changed since it was recorded", p.file))));
        }
    }
    if let Some(b) = &s.bench {
        ui::line("");
        ui::line(format!(
            "  last bench: {} → {} ({})",
            secs(b.before.median),
            secs(b.after.median),
            rust(bold(times(b.speedup)))
        ));
    }
}

pub fn json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn scan_json(s: &ScanOut) -> Value {
    let hotspots: Vec<Value> = s
        .hotspots
        .iter()
        .map(|h| {
            json!({
                "id": h.id,
                "file": h.file,
                "line": h.line,
                "share": h.share,
                "calls": h.ncalls,
                "tottime": h.tottime,
                "cumtime": h.cumtime,
                "portable": h.portable(),
                "worth_porting": h.worth_it(),
                "why_not": h.why_not(),
                "covered_by": h.covered_by,
                "blockers": h.blockers,
                "warnings": h.warnings,
            })
        })
        .collect();
    json!({
        "command": s.command,
        "wall_seconds": s.wall,
        "hotspots": hotspots,
        "notes": s.notes,
    })
}

pub fn verify_json(id: &str, v: &Verify) -> Value {
    let mut out = serde_json::to_value(v).unwrap_or(Value::Null);
    if let Some(obj) = out.as_object_mut() {
        obj.insert("id".into(), json!(id));
        obj.insert("ok".into(), json!(v.ok()));
        if !v.ok() {
            obj.insert("fix".into(), json!(crate::pipeline::feedback(v)));
        }
    }
    out
}

pub fn status_json(ctx: &Ctx) -> Value {
    let s = &ctx.state;
    let ports: serde_json::Map<String, Value> = s
        .ports
        .iter()
        .map(|(id, p)| {
            let mut v = serde_json::to_value(p).unwrap_or(Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.insert("stale".into(), json!(ctx.stale(p)));
            }
            (id.clone(), v)
        })
        .collect();
    let lang = (!s.command.is_empty()).then_some(s.lang);
    json!({
        "command": s.command,
        "lang": lang,
        "wall_seconds": s.wall,
        "ports": ports,
        "bench": s.bench,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Lang;
    use crate::hotspot::Hotspot;
    use crate::state::{Failure, Micro, Port, State};
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    #[test]
    fn long_ids_keep_the_function_name() {
        let a = pad_id("services.original-engines.cx-filter.decodeArchive", 30);
        let b = pad_id("services.original-engines.cx-filter.checkHeader", 30);
        assert_ne!(a, b);
        assert_eq!(a.chars().count(), 30);
        assert_eq!(b.chars().count(), 30);
        assert!(a.starts_with('…') && a.ends_with("decodeArchive"), "{a}");
        assert!(b.starts_with('…') && b.ends_with("checkHeader"), "{b}");

        let exact = "a".repeat(30);
        assert_eq!(pad_id(&exact, 30), exact);
        assert_eq!(pad(&exact, 30), exact);

        assert_eq!(pad_id("fuzzy.lev", 12), "fuzzy.lev   ");

        let unicode = pad_id("café.résumé.naïve.über.引擎.decodeArchive", 20);
        assert_eq!(unicode.chars().count(), 20);
        assert!(unicode.starts_with('…') && unicode.ends_with("decodeArchive"), "{unicode}");

        let status_a = pad_id("services.original-engines.cx-filter.decodeArchive", 32);
        let status_b = pad_id("services.original-engines.cx-filter.checkHeader", 32);
        assert_ne!(status_a, status_b);
        assert_eq!(status_a.chars().count(), 32);
        assert!(status_a.ends_with("decodeArchive") && status_b.ends_with("checkHeader"));
    }

    #[test]
    fn json_helpers_produce_valid_plain_json() {
        let parse = |v: Value| -> Value {
            let raw = serde_json::to_string_pretty(&v).unwrap();
            assert!(!raw.contains("\x1b["), "no ANSI escapes: {raw}");
            serde_json::from_str(&raw).unwrap()
        };

        let scan = ScanOut {
            command: vec!["python".into(), "app.py".into()],
            wall: 2.5,
            hotspots: vec![
                Hotspot {
                    id: "fuzzy.lev".into(),
                    file: "fuzzy.py".into(),
                    line: 10,
                    ncalls: 1000,
                    share: 0.8,
                    ..Hotspot::default()
                },
                Hotspot {
                    id: "fuzzy.io".into(),
                    file: "fuzzy.py".into(),
                    line: 30,
                    ncalls: 1,
                    share: 0.2,
                    blockers: vec!["does I/O".into()],
                    ..Hotspot::default()
                },
            ],
            notes: vec!["note".into()],
        };
        let parsed = parse(scan_json(&scan));
        assert_eq!(parsed["command"], json!(["python", "app.py"]));
        assert_eq!(parsed["wall_seconds"], 2.5);
        assert_eq!(
            (&parsed["hotspots"][0]["worth_porting"], &parsed["hotspots"][1]["why_not"]),
            (&json!(true), &json!("does I/O"))
        );

        let ok_v = Verify {
            built: true,
            total: 10,
            passed: 10,
            micro: Some(Micro { original_ns: 100.0, rust_ns: 10.0, speedup: 10.0, clock_ns: 0.0 }),
            lang: Lang::Python,
            ..Verify::default()
        };
        let ok_parsed = parse(verify_json("fuzzy.lev", &ok_v));
        assert_eq!((&ok_parsed["id"], &ok_parsed["ok"]), (&json!("fuzzy.lev"), &json!(true)));
        assert!(ok_parsed.get("fix").is_none());

        let bad_v = Verify {
            built: true,
            total: 10,
            passed: 9,
            failed: 1,
            failures: vec![Failure { case: 1, call: "lev('a','b')".into(), expected: "1".into(), got: "2".into() }],
            ..Verify::default()
        };
        let bad_parsed = parse(verify_json("fuzzy.lev", &bad_v));
        assert_eq!(bad_parsed["ok"], false);
        assert!(bad_parsed["fix"].as_str().unwrap().contains("1 of 10"));

        let empty_ctx = Ctx { root: PathBuf::from("."), fast: PathBuf::from(".fast"), state: State::default() };
        let empty_status = parse(status_json(&empty_ctx));
        assert!(empty_status["lang"].is_null() && empty_status["ports"] == json!({}));

        let mut ports = BTreeMap::new();
        ports.insert(
            "fuzzy.lev".into(),
            Port {
                id: "fuzzy.lev".into(),
                file: "fuzzy.py".into(),
                module: "fuzzy".into(),
                func: "lev".into(),
                ext: "fa_fuzzy_lev".into(),
                crate_dir: ".fast/ports/fuzzy.lev".into(),
                recorded: 10,
                calls_seen: 10,
                verify: Some(ok_v),
                enabled: true,
                lang: Lang::Python,
                owner: None,
                line: 10,
                sources: vec![],
                backend: crate::backend::Backend::Rust,
            },
        );
        let ctx = Ctx {
            root: PathBuf::from("."),
            fast: PathBuf::from(".fast"),
            state: State {
                command: vec!["python".into(), "app.py".into()],
                lang: Lang::Python,
                wall: 1.25,
                ports,
                ..State::default()
            },
        };
        let status_parsed = parse(status_json(&ctx));
        assert_eq!((&status_parsed["lang"], &status_parsed["wall_seconds"]), (&json!("python"), &json!(1.25)));
        assert_eq!(
            (&status_parsed["ports"]["fuzzy.lev"]["enabled"], &status_parsed["ports"]["fuzzy.lev"]["stale"]),
            (&json!(true), &json!(false))
        );
    }
}
