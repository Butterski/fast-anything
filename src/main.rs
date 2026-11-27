//! fast-anything: find the slow 5% of your app, rewrite just that in C++ or Rust, prove it behaves the same.

mod agent;
mod backend;
mod bench;
mod cfamily;
mod cpp;
mod csharp;
mod doctor;
mod golang;
mod hotspot;
mod java;
mod kotlin;
mod mcp;
mod native;
mod node;
mod php;
mod pipeline;
mod python;
mod r;
mod report;
mod ruby;
mod show;
mod state;
mod ui;
mod util;

use anyhow::{Result, bail, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "fast-anything", version, about, arg_required_else_help = true)]
#[command(
    after_help = "One command:\n  fast-anything go --agent claude -- python app.py\n\nStep by step:\n  fast-anything scan -- python app.py\n  fast-anything port\n  fast-anything verify\n  fast-anything enable <id>\n  fast-anything bench"
)]
struct Cli {
    /// Project folder (default: the current folder)
    #[arg(short = 'C', long = "dir", global = true, default_value = ".")]
    dir: PathBuf,
    /// Print machine-readable JSON on stdout
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Everything in one go: scan, port the hottest functions with your agent, verify, switch on, benchmark
    Go {
        /// Coding agent that writes the port: claude, codex, or any command that reads a prompt on stdin
        #[arg(long)]
        agent: Option<String>,
        /// How many hot functions to port
        #[arg(long, default_value_t = 1)]
        top: usize,
        /// Agent attempts per function before giving up
        #[arg(long, default_value_t = 3)]
        attempts: usize,
        /// Benchmark rounds: each runs the original twice and the ports once, plus one warm-up per side (5 = 11 original runs, 6 port runs)
        #[arg(long, default_value_t = 5)]
        runs: usize,
        /// Real calls to record per function
        #[arg(long, default_value_t = 500)]
        samples: usize,
        /// What the ports are written in: cpp (the default for Python apps on Linux and macOS) or rust
        #[arg(long, value_enum)]
        backend: Option<backend::Backend>,
        /// How you run your app, after --
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Run your app under a profiler and rank what's worth porting
    Scan {
        /// Stop the app after this many seconds and rank what ran (Python, Node, Go, Java, Kotlin, Ruby, PHP, C, C++, R, C#: servers and GUIs that don't exit)
        #[arg(long, value_name = "SECS")]
        duration: Option<f64>,
        /// How you run your app, after --
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
    /// Record a function's real calls and set up its port: C++ for Python apps, else a Rust crate (default: the top candidate)
    Port {
        id: Option<String>,
        /// Have this coding agent write the port right away
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, default_value_t = 3)]
        attempts: usize,
        #[arg(long, default_value_t = 500)]
        samples: usize,
        /// What the port is written in: cpp (the default for Python apps on Linux and macOS) or rust
        #[arg(long, value_enum)]
        backend: Option<backend::Backend>,
    },
    /// Build the port and replay every recorded call against it
    Verify {
        id: Option<String>,
        /// Verify every port (after a toolchain or dependency upgrade, or changes to the app)
        #[arg(long, conflicts_with = "id")]
        all: bool,
    },
    /// Switch your app over to the verified port
    Enable {
        id: Option<String>,
        /// Enable even if it hasn't passed verify
        #[arg(long)]
        force: bool,
    },
    /// Switch back to the original code
    Disable {
        id: Option<String>,
        /// Switch every enabled port back
        #[arg(long, conflicts_with = "id")]
        all: bool,
    },
    /// Rebuild every port (after cloning, or a toolchain upgrade)
    Build,
    /// Remove compiler build cache (.fast/target) or all temporary state
    Clean {
        /// Remove all ports and reset .fast completely (disables any enabled ports first)
        #[arg(long)]
        all: bool,
    },
    /// Time your app with the originals vs the ports
    Bench {
        /// Rounds: each runs the original twice (an A/A control for noise) and the ports once, plus one warm-up per side (5 = 11 original runs, 6 port runs)
        #[arg(long, default_value_t = 5)]
        runs: usize,
    },
    /// Write a shareable report (markdown + SVG card)
    Report,
    /// Show the last scan and every port
    Status,
    /// Check that the compilers, Python/Node and your agents are installed
    Doctor,
    /// Run as an MCP server on stdio (for Claude Code, Codex, Cursor, ...)
    Mcp,
    /// Replays recorded calls against a C-bridge port (used by verify, in its own process)
    #[command(name = "__replay", hide = true)]
    Replay { job: PathBuf },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let root = util::absolute(&cli.dir);
    match run(cli.cmd, &root, cli.json) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            ui::error(format!("{e:#}"));
            ExitCode::FAILURE
        }
    }
}

fn run(cmd: Cmd, root: &Path, json: bool) -> Result<()> {
    if let Cmd::Replay { job } = cmd {
        return native::replay(&job);
    }
    if json {
        match &cmd {
            Cmd::Go { .. } | Cmd::Build | Cmd::Clean { .. } | Cmd::Mcp => {
                bail!(
                    "--json isn't supported for this command (supported: scan, port, verify, enable, disable, bench, report, status, doctor)"
                );
            }
            _ => ui::set_quiet(true),
        }
    }
    if matches!(cmd, Cmd::Mcp) {
        return mcp::serve();
    }
    if !json {
        ui::banner();
    }
    match cmd {
        Cmd::Go { agent, top, attempts, runs, samples, backend, command } => {
            pipeline::go(root, command, agent.as_deref(), top, attempts, runs, samples, backend)
        }
        Cmd::Scan { command, duration } => {
            let out = pipeline::scan(root, command, duration)?;
            if json {
                show::json(&show::scan_json(&out))
            } else {
                show::scan(&out);
                Ok(())
            }
        }
        Cmd::Port { id, agent, attempts, samples, backend } => {
            let out = pipeline::port_with(root, id.as_deref(), samples, backend)?;
            if let Some(agent) = agent {
                if !json {
                    show::port(&out, false);
                }
                let v = pipeline::agent_port(root, &out.id, &agent, attempts)?;
                if json {
                    show::json(&json!({ "port": out, "verify": show::verify_json(&out.id, &v) }))?;
                }
                if !v.ok() {
                    bail!("{agent} couldn't get {} right in {attempts} attempts", out.id);
                }
                if !json && !v.slower() && !v.too_cheap() {
                    show::next(&format!("fast-anything enable {}", out.id));
                }
            } else if json {
                show::json(&out)?;
            } else {
                show::port(&out, true);
            }
            Ok(())
        }
        Cmd::Verify { all: true, .. } => {
            let results = pipeline::verify_all(root)?;
            let failed: Vec<&str> = results.iter().filter(|(_, v)| !v.ok()).map(|(id, _)| id.as_str()).collect();
            if json {
                let ports: Vec<Value> = results.iter().map(|(id, v)| show::verify_json(id, v)).collect();
                show::json(&json!({ "ok": failed.is_empty(), "ports": ports }))?;
            }
            ensure!(
                failed.is_empty(),
                "{} of {} ports don't match: {}",
                failed.len(),
                results.len(),
                failed.join(", ")
            );
            ui::ok(format!("all {} ports match the original", results.len()));
            Ok(())
        }
        Cmd::Verify { id, .. } => {
            let (id, v) = pipeline::verify(root, id.as_deref())?;
            if json {
                show::json(&show::verify_json(&id, &v))?;
            } else {
                show::verify(&id, &v);
            }
            if !v.ok() {
                bail!("{id} doesn't match the original yet");
            }
            if !json && !v.slower() && !v.too_cheap() {
                show::next(&format!("fast-anything enable {id}"));
            }
            Ok(())
        }
        Cmd::Enable { id, force } => {
            let id = pipeline::enable(root, id.as_deref(), force)?;
            if json {
                return show::json(&json!({ "enabled": id }));
            }
            let label = state::Ctx::open(root)?.state.ports.get(&id).map_or("Rust", |p| p.backend.label());
            ui::ok(format!("{} now runs in {label}", ui::bold(&id)));
            show::next("fast-anything bench");
            Ok(())
        }
        Cmd::Disable { all: true, .. } => {
            let ids = pipeline::disable_all(root)?;
            if json {
                return show::json(&json!({ "disabled": ids }));
            }
            if ids.is_empty() {
                ui::ok("no port was switched on");
            }
            for id in ids {
                ui::ok(format!("{} is back to the original code", ui::bold(&id)));
            }
            Ok(())
        }
        Cmd::Disable { id, .. } => {
            let id = pipeline::disable(root, id.as_deref())?;
            if json {
                return show::json(&json!({ "disabled": id }));
            }
            ui::ok(format!("{} is back to the original code", ui::bold(&id)));
            Ok(())
        }
        Cmd::Build => pipeline::build_all(root),
        Cmd::Clean { all } => pipeline::clean(root, all),
        Cmd::Bench { runs } => {
            let out = pipeline::bench(root, runs)?;
            if json {
                show::json(&out)
            } else {
                show::bench(&out);
                show::next("fast-anything report");
                Ok(())
            }
        }
        Cmd::Report => {
            let out = pipeline::report(root)?;
            if json {
                show::json(&out)
            } else {
                show::report(&out);
                Ok(())
            }
        }
        Cmd::Status => {
            let ctx = state::Ctx::open(root)?;
            if json {
                show::json(&show::status_json(&ctx))
            } else {
                show::status(&ctx);
                Ok(())
            }
        }
        Cmd::Doctor => {
            if json {
                let report = doctor::check();
                show::json(&report)?;
                doctor::ensure_ready(&report)
            } else {
                doctor::run()
            }
        }
        Cmd::Mcp | Cmd::Replay { .. } => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_all_parses() {
        assert!(matches!(
            Cli::try_parse_from(["fast-anything", "disable", "--all"]).unwrap().cmd,
            Cmd::Disable { all: true, id: None }
        ));
        assert!(Cli::try_parse_from(["fast-anything", "disable", "fuzzy.lev", "--all"]).is_err());
    }

    #[test]
    fn clean_parses() {
        assert!(matches!(Cli::try_parse_from(["fast-anything", "clean"]).unwrap().cmd, Cmd::Clean { all: false }));
        assert!(matches!(
            Cli::try_parse_from(["fast-anything", "clean", "--all"]).unwrap().cmd,
            Cmd::Clean { all: true }
        ));
    }

    #[test]
    fn verify_all_parses() {
        let cli = Cli::try_parse_from(["fast-anything", "verify", "--all"]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Verify { all: true, id: None }));
        assert!(Cli::try_parse_from(["fast-anything", "verify", "fuzzy.lev", "--all"]).is_err(), "an id or --all");
        assert!(matches!(
            Cli::try_parse_from(["fast-anything", "verify"]).unwrap().cmd,
            Cmd::Verify { all: false, .. }
        ));
    }

    #[test]
    fn json_flag_parses_before_and_after_subcommand() {
        for argv in [
            vec!["fast-anything", "--json", "doctor"],
            vec!["fast-anything", "doctor", "--json"],
            vec!["fast-anything", "status", "--json"],
            vec!["fast-anything", "bench", "--json", "--runs", "3"],
            vec!["fast-anything", "verify", "--all", "--json"],
            vec!["fast-anything", "report", "--json"],
            vec!["fast-anything", "port", "--json", "fuzzy.lev"],
            vec!["fast-anything", "enable", "--json", "fuzzy.lev"],
            vec!["fast-anything", "disable", "--all", "--json"],
        ] {
            let cli = Cli::try_parse_from(argv).unwrap();
            assert!(cli.json);
        }

        let scan_before = Cli::try_parse_from(["fast-anything", "--json", "scan", "--", "python", "app.py"]).unwrap();
        assert!(scan_before.json);
        let scan_after = Cli::try_parse_from(["fast-anything", "scan", "--json", "--", "python", "app.py"]).unwrap();
        assert!(scan_after.json);

        // --json after -- belongs to the user's app command, not fast-anything.
        let app_flag = Cli::try_parse_from(["fast-anything", "scan", "--", "python", "app.py", "--json"]).unwrap();
        assert!(!app_flag.json);
        assert!(matches!(app_flag.cmd, Cmd::Scan { command, .. } if command == ["python", "app.py", "--json"]));
    }

    #[test]
    fn json_rejects_unsupported_subcommands() {
        let root = Path::new(".");
        for cmd in [
            Cmd::Build,
            Cmd::Clean { all: false },
            Cmd::Mcp,
            Cmd::Go {
                agent: None,
                top: 1,
                attempts: 3,
                runs: 5,
                samples: 500,
                backend: None,
                command: vec!["python".into()],
            },
        ] {
            let err = run(cmd, root, true).unwrap_err();
            assert!(err.to_string().contains("--json isn't supported"), "{err}");
        }
    }
}
