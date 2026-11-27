//! fast-anything doctor: is everything a port needs installed?

use crate::ui::{self, bold, dim, green, red, yellow};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::process::Command;

fn version(program: &str, args: &[&str]) -> Option<String> {
    let run = |p: &str| Command::new(p).args(args).output().ok().filter(|o| o.status.success());
    let out = run(program).or_else(|| if cfg!(windows) { run(&format!("{program}.cmd")) } else { None })?;
    let text = String::from_utf8_lossy(if out.stdout.is_empty() { &out.stderr } else { &out.stdout }).into_owned();
    Some(text.lines().next().unwrap_or("").trim().to_string())
}

/// The oldest version fast-anything works with, as each backend checks it when it starts.
fn minimum(name: &str) -> &'static [u32] {
    match name {
        "cargo" => &[1, 85], // ports use edition 2024
        "python" => &[3, 10],
        "node" => &[22],
        "ruby" => &[3, 3],
        "java" => &[22],
        "go" => &[1, 22],
        "php" => &[8, 1],
        "dotnet" => &[8],
        "Rscript" => &[4, 0],
        "bun" => &[1],
        "deno" => &[2],
        _ => &[],
    }
}

/// The first number in a version line: "ruby 3.1.7p261 (...)" gives [3, 1, 7], "go1.22.3" [1, 22, 3].
fn parse_version(line: &str) -> Vec<u32> {
    let Some(start) = line.find(|c: char| c.is_ascii_digit()) else { return vec![] };
    let digits: String = line[start..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    digits.split('.').map_while(|p| p.parse().ok()).collect()
}

/// Why a runtime that's installed can't be used yet, if it can't.
fn problem(name: &str, found: &str) -> Option<String> {
    let min = minimum(name);
    let version = parse_version(found);
    if !min.is_empty() && !version.is_empty() && version.as_slice() < min {
        let min: Vec<String> = min.iter().map(u32::to_string).collect();
        return Some(format!("too old, {} or newer is needed", min.join(".")));
    }
    if name == "php" {
        let modules = Command::new("php").arg("-m").output().ok()?;
        if !String::from_utf8_lossy(&modules.stdout).lines().any(|l| l.trim() == "FFI") {
            return Some("the FFI extension is off: add extension=ffi to php.ini".into());
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
    pub need: &'static str,
    pub hint: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub ready_to_scan: bool,
    pub ready_to_port: bool,
    pub tools: Vec<Tool>,
}

struct Check {
    name: &'static str,
    found: Option<String>,
    need: &'static str,
    hint: &'static str,
}

impl Check {
    fn into_tool(self) -> Tool {
        let problem = self.found.as_deref().and_then(|v| problem(self.name, v));
        let ok = self.found.is_some() && problem.is_none();
        Tool { name: self.name, ok, version: self.found, problem, need: self.need, hint: self.hint }
    }
}

pub fn check() -> Report {
    let python = version("python", &["--version"]).or_else(|| version("python3", &["--version"]));
    let checks = [
        Check {
            name: "cargo",
            found: version("cargo", &["--version"]),
            need: "to port (scan works without it)",
            hint: "https://rustup.rs",
        },
        Check { name: "python", found: python, need: "for Python apps (3.10+)", hint: "https://python.org" },
        Check {
            name: "node",
            found: version("node", &["--version"]),
            need: "for JS/TS apps (22+)",
            hint: "https://nodejs.org",
        },
        Check {
            name: "ruby",
            found: version("ruby", &["--version"]),
            need: "for Ruby apps (3.3+)",
            hint: "https://ruby-lang.org",
        },
        Check {
            name: "java",
            found: version("java", &["--version"]),
            need: "for Java apps (22+)",
            hint: "https://adoptium.net",
        },
        Check {
            name: "go",
            found: version("go", &["version"]),
            need: "for Go apps (1.22+)",
            hint: "https://go.dev/dl",
        },
        Check {
            name: "php",
            found: version("php", &["--version"]),
            need: "for PHP apps (8.1+, extension=ffi)",
            hint: "https://php.net",
        },
        Check {
            name: "clang",
            found: version("clang", &["--version"])
                .or_else(|| version("gcc", &["--version"]))
                .or_else(|| version("icpx", &["--version"])),
            need: "for C/C++ apps (or gcc, icpx)",
            hint: "https://llvm.org",
        },
        Check {
            name: "dotnet",
            found: version("dotnet", &["--version"]),
            need: "for C# apps (.NET SDK 8+)",
            hint: "https://dot.net",
        },
        Check {
            name: "Rscript",
            found: version("Rscript", &["--version"]),
            need: "for R apps (4.0+)",
            hint: "https://www.r-project.org",
        },
        Check {
            name: "bun",
            found: version("bun", &["--version"]),
            need: "to run JS/TS apps on Bun (instead of node)",
            hint: "https://bun.sh",
        },
        Check {
            name: "deno",
            found: version("deno", &["--version"]),
            need: "to run JS/TS apps on Deno (instead of node)",
            hint: "https://deno.com",
        },
        Check {
            name: "claude",
            found: version("claude", &["--version"]),
            need: "--agent claude",
            hint: "https://claude.com/claude-code",
        },
        Check {
            name: "codex",
            found: version("codex", &["--version"]),
            need: "--agent codex",
            hint: "npm i -g @openai/codex",
        },
    ];
    let mut tools: Vec<Tool> = checks.into_iter().map(Check::into_tool).collect();
    let ready_to_scan = tools[1..12].iter().any(|t| t.ok);
    let cxx = cxx_tool();
    let ready_to_port = tools[0].ok || cxx.ok;
    tools.push(cxx);
    Report { ready_to_scan, ready_to_port, tools }
}

/// What C++ ports need: a compiler, and the headers of the Python the app runs on.
fn cxx_tool() -> Tool {
    let cxx = std::env::var("CXX").ok().filter(|c| !c.trim().is_empty());
    let found = cxx
        .and_then(|c| version(&c, &["--version"]))
        .or_else(|| version("c++", &["--version"]))
        .or_else(|| version("g++", &["--version"]))
        .or_else(|| version("clang++", &["--version"]));
    let python = if cfg!(windows) { "python" } else { "python3" };
    let problem = found.as_ref().and_then(|_| crate::cpp::python_include(python).err().map(|e| e.to_string()));
    Tool {
        name: "c++",
        ok: found.is_some() && problem.is_none(),
        version: found,
        problem,
        need: "for C++ ports of Python apps, with the Python headers",
        hint: "Debian/Ubuntu: apt install build-essential python3-dev",
    }
}

pub fn ensure_ready(report: &Report) -> Result<()> {
    ensure!(
        report.ready_to_scan,
        "install Python, Node.js (or Bun, Deno), Ruby, Java, Go, PHP, .NET, R or a C compiler"
    );
    Ok(())
}

pub fn run() -> Result<()> {
    let report = check();
    for t in &report.tools {
        match (&t.version, &t.problem) {
            (Some(v), None) => ui::line(format!("  {} {:<8} {}", green("✓"), bold(t.name), dim(v))),
            (Some(v), Some(why)) => ui::line(format!("  {} {:<8} {} {}", red("✗"), bold(t.name), dim(v), why)),
            (None, _) if t.name == "cargo" => {
                ui::line(format!("  {} {:<8} missing, {}: {}", red("✗"), bold(t.name), t.need, t.hint))
            }
            (None, _) => ui::line(format!(
                "  {} {:<8} {}",
                yellow("·"),
                t.name,
                dim(format!("not found ({}): {}", t.need, t.hint))
            )),
        }
    }
    ensure_ready(&report)?;
    ui::ok("ready to scan");
    // scanning works without a compiler, so a missing toolchain only warns (#142)
    if report.ready_to_port {
        ui::ok("ready to port");
    } else {
        ui::warn(
            "to port, verify and enable: a C++ compiler and the Python headers (C++ ports of Python apps), or Rust 1.85+ and a C linker (Rust ports): apt install build-essential python3-dev, https://rustup.rs",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_minimums() {
        assert_eq!(parse_version("ruby 3.1.7p261 (2025-03-26 revision 0a3704f218) [aarch64-linux-gnu]"), [3, 1, 7]);
        assert_eq!(parse_version("go version go1.22.3 linux/amd64"), [1, 22, 3]);
        assert_eq!(parse_version("v24.22.3"), [24, 22, 3]);
        assert_eq!(parse_version("openjdk 25 2025-09-16"), [25]);
        assert_eq!(parse_version("no digits"), Vec::<u32>::new());
        assert!(problem("ruby", "ruby 3.1.7p261").is_some_and(|p| p.contains("3.3 or newer")));
        assert_eq!(problem("ruby", "ruby 3.4.1"), None);
        assert_eq!(problem("node", "v22.0.0"), None);
        assert!(problem("python", "Python 3.9.18").is_some());
        assert_eq!(problem("clang", "Debian clang version 14.0.6"), None, "no minimum");
    }

    #[test]
    fn report_serializes_clean_json() {
        let mk = |name, found: Option<&str>, need, hint| {
            Check { name, found: found.map(String::from), need, hint }.into_tool()
        };
        let report = Report {
            ready_to_scan: true,
            ready_to_port: false,
            tools: vec![
                mk("cargo", None, "to port (scan works without it)", "https://rustup.rs"),
                mk("python", Some("Python 3.12.1"), "for Python apps (3.10+)", "https://python.org"),
                mk("ruby", Some("ruby 3.1.7p261"), "for Ruby apps (3.3+)", "https://ruby-lang.org"),
            ],
        };
        let raw = serde_json::to_string_pretty(&report).unwrap();
        assert!(!raw.contains("\x1b["), "no ANSI codes in JSON: {raw}");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!((&v["ready_to_scan"], &v["ready_to_port"]), (&serde_json::json!(true), &serde_json::json!(false)));
        let tools = v["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0]["ok"], false);
        assert!(tools[0].get("version").is_none() && tools[0].get("problem").is_none());
        assert_eq!(tools[1]["ok"], true);
        assert!(tools[2]["problem"].as_str().unwrap().contains("3.3 or newer"));
        assert!(ensure_ready(&report).is_ok());
        assert!(ensure_ready(&Report { ready_to_scan: false, ..report }).is_err());
    }
}
