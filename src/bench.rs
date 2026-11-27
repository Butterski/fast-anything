//! Times the whole app in rounds of original, original again (an A/A control) and Rust, rotated so
//! drift hits every side equally. Wall time decides; CPU time shows what the ports cost in cores.

use crate::backend::Lang;
use crate::python::DISABLE_ENV;
use crate::state::Ctx;
use crate::{ui, util};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Output, Stdio};
use std::time::Instant;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub median: f64,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
    pub stddev: f64,
}

impl Stats {
    pub fn of(xs: &[f64]) -> Stats {
        if xs.is_empty() {
            return Stats::default();
        }
        let mut v = xs.to_vec();
        v.sort_by(f64::total_cmp);
        let n = v.len() as f64;
        let mean = v.iter().sum::<f64>() / n;
        let median = if v.len() % 2 == 1 { v[v.len() / 2] } else { (v[v.len() / 2 - 1] + v[v.len() / 2]) / 2.0 };
        let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n;
        Stats { median, mean, min: v[0], max: v[v.len() - 1], stddev: var.sqrt() }
    }
}

/// Median and quartiles of per-round values.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Spread {
    pub median: f64,
    pub q1: f64,
    pub q3: f64,
}

impl Spread {
    pub fn of(xs: &[f64]) -> Spread {
        let (q1, q3) = quartiles(xs);
        Spread { median: Stats::of(xs).median, q1, q3 }
    }

    /// How far the middle half reaches from zero: the run-to-run noise for an A/A change.
    pub fn band(&self) -> f64 {
        self.q1.abs().max(self.q3.abs())
    }
}

/// First and third quartile, computed like Python's statistics.quantiles(n=4) (exclusive).
fn quartiles(xs: &[f64]) -> (f64, f64) {
    let mut v = xs.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        return (0.0, 0.0);
    }
    if n < 4 {
        return (v[0], v[n - 1]);
    }
    let m = n + 1;
    let q = |i: usize| {
        let j = (i * m / 4).clamp(1, n - 1);
        let d = (i * m - 4 * j) as f64;
        (v[j - 1] * (4.0 - d) + v[j] * d) / 4.0
    };
    (q(1), q(3))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Faster,
    Slower,
    /// The Rust rounds don't clear the A/A noise band.
    Noise,
}

/// Faster or slower only when the middle half of the Rust changes lies outside the A/A noise band.
fn verdict(aa: &Spread, change: &Spread) -> Verdict {
    let band = aa.band();
    if change.q3 < -band {
        Verdict::Faster
    } else if change.q1 > band {
        Verdict::Slower
    } else {
        Verdict::Noise
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BenchResult {
    pub lang: Lang,
    pub command: Vec<String>,
    pub runs: usize,
    pub ports: Vec<String>,
    pub before: Stats,
    pub after: Stats,
    /// Median over rounds of original time / Rust time, paired within each round.
    pub speedup: f64,
    pub speedup_q1: f64,
    pub speedup_q3: f64,
    /// The original against itself in each round, in %: how much the app varies on its own.
    pub aa: Spread,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
    /// CPU time (user + system) of the app and the processes it waited for, when the OS tells.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_before: Option<Stats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_after: Option<Stats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_speedup: Option<f64>,
    /// First line where the app's output with the Rust ports differs from the original's,
    /// or OUTPUT_VARIES.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_diff: Option<String>,
    /// When and on what machine bench ran; results from older versions have none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Scope>,
}

/// When and where a bench ran: a speedup means little without the machine it was measured on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    /// UTC, e.g. "2026-11-26".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    /// As Rust names them: "linux", "macos", "windows"; "x86_64", "aarch64".
    pub os: String,
    pub arch: String,
    /// Logical CPUs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpus: Option<usize>,
    /// The CPU's model name, where the OS tells it cheaply.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
}

impl Scope {
    pub fn here() -> Scope {
        Scope {
            date: util::today(),
            os: std::env::consts::OS.into(),
            arch: std::env::consts::ARCH.into(),
            cpus: std::thread::available_parallelism().ok().map(|n| n.get()),
            cpu: cpu_model(),
        }
    }
}

/// The CPU's model name; None where it isn't cheap to get.
fn cpu_model() -> Option<String> {
    let found = if cfg!(target_os = "linux") {
        std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|t| cpuinfo_model(&t))
    } else if cfg!(target_os = "macos") {
        printed("sysctl", &["-n", "machdep.cpu.brand_string"])
    } else if cfg!(windows) {
        let key = r"HKLM\HARDWARE\DESCRIPTION\System\CentralProcessor\0";
        printed("reg", &["query", key, "/v", "ProcessorNameString"])
            .and_then(|t| registry_value(&t))
            .or_else(|| std::env::var("PROCESSOR_IDENTIFIER").ok())
    } else {
        None
    };
    found.map(|m| m.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|m| !m.is_empty())
}

/// What a command prints, if it starts and succeeds.
fn printed(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The first `model name` in /proc/cpuinfo (ARM kernels may have none).
fn cpuinfo_model(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (key, value) = l.split_once(':')?;
        (key.trim() == "model name").then(|| value.trim().to_string())
    })
}

/// The data in `reg query` output: `    ProcessorNameString    REG_SZ    Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz`.
fn registry_value(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.split_once("REG_SZ").map(|(_, v)| v.trim().to_string()))
}

impl BenchResult {
    /// "CPU time 95×", or how much more CPU time the Rust ports take.
    pub fn cpu_note(&self) -> Option<String> {
        self.cpu_speedup.map(|x| {
            if x >= 1.0 {
                format!("CPU time {}", util::times(x))
            } else {
                format!("CPU time {} more", util::times(1.0 / x))
            }
        })
    }
}

pub const OUTPUT_VARIES: &str =
    "the original prints something different from run to run, so its output can't be compared with the ports'";

/// Whether the Rust runs print what the original did, checked against every run of the original.
struct OutputCheck {
    first: Vec<u8>,
    varies: bool,
    rust_diff: Option<String>,
}

impl OutputCheck {
    fn new(first: Vec<u8>) -> OutputCheck {
        OutputCheck { first, varies: false, rust_diff: None }
    }

    fn see(&mut self, original: bool, out: &[u8]) {
        if original {
            self.varies |= out != self.first;
        } else if self.rust_diff.is_none() {
            self.rust_diff = first_difference(&self.first, out);
        }
    }

    fn diff(self) -> Option<String> {
        if self.varies { Some(OUTPUT_VARIES.into()) } else { self.rust_diff }
    }
}

/// One run of the app: wall time, CPU time if the OS tells, and what it printed.
struct Run {
    wall: f64,
    cpu: Option<f64>,
    stdout: Vec<u8>,
}

fn time_once(ctx: &Ctx, exe: &str, args: &[String], original: bool) -> Result<Run> {
    let mut cmd = Command::new(exe);
    cmd.args(args).current_dir(&ctx.root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if original {
        cmd.env(DISABLE_ENV, "1");
    } else {
        cmd.env_remove(DISABLE_ENV);
    }
    let start = Instant::now();
    let (out, cpu) = output_with_cpu(&mut cmd).context("running the app")?;
    let wall = start.elapsed().as_secs_f64();
    ensure!(
        out.status.success(),
        "the app failed ({}) with the {} version:\n{}",
        out.status,
        if original { ctx.state.lang.label() } else { "ported" },
        util::tail(&String::from_utf8_lossy(&out.stderr), 20)
    );
    Ok(Run { wall, cpu, stdout: out.stdout })
}

/// Like Command::output, plus the CPU time of the child and everything it waited for.
fn output_with_cpu(cmd: &mut Command) -> std::io::Result<(Output, Option<f64>)> {
    let mut meter = cpu::Meter::start();
    let mut child = cmd.spawn()?;
    meter.attach(&child);
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    // stderr on its own thread so neither pipe fills up while the other is read
    let errors = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut e) = stderr {
            let _ = e.read_to_end(&mut buf);
        }
        buf
    });
    let mut out = Vec::new();
    if let Some(mut o) = stdout
        && let Err(e) = o.read_to_end(&mut out)
    {
        // don't leave the child running or unwaited for
        let _ = child.kill();
        let _ = child.wait();
        let _ = errors.join();
        return Err(e);
    }
    let err = errors.join().unwrap_or_default();
    let status = child.wait()?;
    Ok((Output { status, stdout: out, stderr: err }, meter.stop()))
}

#[cfg(unix)]
mod cpu {
    use std::process::Child;

    /// CPU seconds of every child this process has waited for, and their waited-for children.
    fn children() -> Option<f64> {
        // SAFETY: getrusage only writes into the struct it's given.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        if unsafe { libc::getrusage(libc::RUSAGE_CHILDREN, &mut usage) } != 0 {
            return None;
        }
        let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
        Some(secs(usage.ru_utime) + secs(usage.ru_stime))
    }

    /// The children's total before the run; the run is the difference after waiting for it.
    pub struct Meter(Option<f64>);

    impl Meter {
        pub fn start() -> Meter {
            Meter(children())
        }

        pub fn attach(&mut self, _child: &Child) {}

        pub fn stop(self) -> Option<f64> {
            Some(children()? - self.0?)
        }
    }
}

#[cfg(windows)]
mod cpu {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;
    use std::process::Child;
    use std::ptr::{null, null_mut};

    type Handle = *mut c_void;
    const JOB_OBJECT_BASIC_ACCOUNTING_INFORMATION: i32 = 1;

    /// JOBOBJECT_BASIC_ACCOUNTING_INFORMATION; times in 100 ns units.
    #[repr(C)]
    #[derive(Default)]
    struct Accounting {
        total_user_time: i64,
        total_kernel_time: i64,
        /// this-period times and process counts, unused
        _rest: [i64; 4],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> Handle;
        fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
        fn QueryInformationJobObject(job: Handle, class: i32, info: *mut c_void, len: u32, returned: *mut u32) -> i32;
        fn CloseHandle(handle: Handle) -> i32;
    }

    /// A job object per run: it adds up the CPU time of every process in it, children included.
    /// The child joins it just after spawn, so a process it starts before that isn't counted;
    /// spawning with CREATE_SUSPENDED and resuming its main thread after joining would close that gap.
    pub struct Meter {
        job: Handle,
        attached: bool,
    }

    impl Meter {
        pub fn start() -> Meter {
            // SAFETY: no security attributes and no name; a null handle means no CPU time.
            Meter { job: unsafe { CreateJobObjectW(null_mut(), null()) }, attached: false }
        }

        pub fn attach(&mut self, child: &Child) {
            if !self.job.is_null() {
                // SAFETY: both handles are open; the child's lives as long as `child`.
                self.attached = unsafe { AssignProcessToJobObject(self.job, child.as_raw_handle()) } != 0;
            }
        }

        pub fn stop(self) -> Option<f64> {
            if !self.attached {
                return None;
            }
            let mut info = Accounting::default();
            // SAFETY: info is a JOBOBJECT_BASIC_ACCOUNTING_INFORMATION of the size we pass.
            let ok = unsafe {
                QueryInformationJobObject(
                    self.job,
                    JOB_OBJECT_BASIC_ACCOUNTING_INFORMATION,
                    (&mut info as *mut Accounting).cast(),
                    size_of::<Accounting>() as u32,
                    null_mut(),
                )
            } != 0;
            ok.then(|| (info.total_user_time + info.total_kernel_time) as f64 / 1e7)
        }
    }

    impl Drop for Meter {
        fn drop(&mut self) {
            if !self.job.is_null() {
                // SAFETY: the job handle is ours and closed once.
                unsafe { CloseHandle(self.job) };
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Original,
    Control,
    Rust,
}

/// Round r runs the three sides rotated left by r, so none of them always goes first.
fn order(round: usize) -> [Side; 3] {
    let mut sides = [Side::Original, Side::Control, Side::Rust];
    sides.rotate_left(round % 3);
    sides
}

/// Wall and CPU times per round (indexed by Side) into the numbers bench reports.
fn summarize(wall: &[[f64; 3]], cpu: &[[Option<f64>; 3]]) -> BenchResult {
    let (o, c, r) = (Side::Original as usize, Side::Control as usize, Side::Rust as usize);
    let column = |i: usize| wall.iter().map(|w| w[i]).collect::<Vec<_>>();
    let change = |i: usize| wall.iter().map(|w| (w[i] / w[o] - 1.0) * 100.0).collect::<Vec<_>>();
    let (aa, rust) = (Spread::of(&change(c)), Spread::of(&change(r)));
    let ratio = Spread::of(&wall.iter().map(|w| w[o] / w[r]).collect::<Vec<_>>());
    let cpu_column = |i: usize| cpu.iter().map(|c| c[i]).collect::<Option<Vec<f64>>>();
    let (cpu_before, cpu_after) = (cpu_column(o), cpu_column(r));
    let cpu_speedup = match (&cpu_before, &cpu_after) {
        (Some(b), Some(a)) if !a.is_empty() && a.iter().all(|&x| x > 0.0) => {
            Some(Stats::of(&b.iter().zip(a).map(|(b, a)| b / a).collect::<Vec<_>>()).median)
        }
        _ => None,
    };
    BenchResult {
        before: Stats::of(&column(o)),
        after: Stats::of(&column(r)),
        speedup: ratio.median,
        speedup_q1: ratio.q1,
        speedup_q3: ratio.q3,
        aa,
        verdict: Some(verdict(&aa, &rust)),
        cpu_before: cpu_before.map(|x| Stats::of(&x)),
        cpu_after: cpu_after.map(|x| Stats::of(&x)),
        cpu_speedup,
        ..BenchResult::default()
    }
}

/// The first line that differs between two outputs, if any.
pub fn first_difference(original: &[u8], rust: &[u8]) -> Option<String> {
    if original == rust {
        return None;
    }
    let (a, b) = (String::from_utf8_lossy(original), String::from_utf8_lossy(rust));
    let (mut a_lines, mut b_lines) = (a.lines(), b.lines());
    for n in 1.. {
        match (a_lines.next(), b_lines.next()) {
            (Some(x), Some(y)) if x == y => continue,
            (None, None) => return Some("the outputs differ only in line endings or a final newline".into()),
            (x, y) => {
                let clip = |l: Option<&str>| match l {
                    None => "(no line)".to_string(),
                    Some(l) if l.chars().count() > 80 => format!("{}…", l.chars().take(79).collect::<String>()),
                    Some(l) => l.to_string(),
                };
                return Some(format!("line {n}: {:?} became {:?}", clip(x), clip(y)));
            }
        }
    }
    unreachable!()
}

pub fn run(ctx: &Ctx, runs: usize) -> Result<BenchResult> {
    let (exe, args) = ctx.state.lang.bench_command(ctx)?;
    let (exe, args) = (exe.as_str(), args.as_slice());
    let ports: Vec<String> = ctx.state.ports.values().filter(|p| p.enabled).map(|p| p.id.clone()).collect();
    ensure!(
        !ports.is_empty(),
        "no ports are enabled, so there is nothing to compare: port, verify and enable one first (fast-anything go)"
    );
    ui::step(format!(
        "timing {} ({runs} rounds of {} twice and the ports once, rotated)",
        ui::bold(ctx.state.command.join(" ")),
        ctx.state.lang.label()
    ));
    // one uncounted warm-up per side; every later run must print what this first original did
    let mut check = OutputCheck::new(time_once(ctx, exe, args, true)?.stdout);
    check.see(false, &time_once(ctx, exe, args, false)?.stdout);
    let (mut wall, mut cpu) = (Vec::with_capacity(runs), Vec::with_capacity(runs));
    for round in 0..runs {
        let (mut w, mut c) = ([0.0; 3], [None; 3]);
        for side in order(round) {
            let original = side != Side::Rust;
            let run = time_once(ctx, exe, args, original)?;
            check.see(original, &run.stdout);
            (w[side as usize], c[side as usize]) = (run.wall, run.cpu);
        }
        wall.push(w);
        cpu.push(c);
    }
    Ok(BenchResult {
        lang: ctx.state.lang,
        command: ctx.state.command.clone(),
        runs,
        ports,
        output_diff: check.diff(),
        scope: Some(Scope::here()),
        ..summarize(&wall, &cpu)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats() {
        let s = Stats::of(&[3.0, 1.0, 2.0, 10.0]);
        assert_eq!(s.median, 2.5);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.max, 10.0);
        assert_eq!(s.mean, 4.0);
        assert_eq!(Stats::of(&[5.0]).median, 5.0);
    }

    #[test]
    fn output_difference() {
        assert_eq!(first_difference(b"a\nb\n", b"a\nb\n"), None);
        assert_eq!(first_difference(b"a\n1.0\n", b"a\n1\n").as_deref(), Some(r#"line 2: "1.0" became "1""#));
        assert_eq!(first_difference(b"a\nb\n", b"a\n").as_deref(), Some(r#"line 2: "b" became "(no line)""#));
        assert!(first_difference(b"a\r\n", b"a\n").unwrap().contains("line endings"));
    }

    #[test]
    fn quartiles_match_python() {
        // statistics.quantiles(xs, n=4)
        assert_eq!(quartiles(&[5.0, 1.0, 4.0, 2.0, 3.0]), (1.5, 4.5));
        assert_eq!(quartiles(&[1.0, 2.0, 3.0]), (1.0, 3.0));
        assert_eq!(quartiles(&[1.0, 2.0, 3.0, 4.0]), (1.25, 3.75));
        assert_eq!(quartiles(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0]), (2.75, 8.25));
        assert_eq!(quartiles(&[7.0]), (7.0, 7.0));
    }

    #[test]
    fn only_changes_beyond_the_aa_band_count() {
        let s = |q1, q3| Spread { median: (q1 + q3) / 2.0, q1, q3 };
        let aa = s(-0.5, 2.0);
        assert_eq!(aa.band(), 2.0);
        assert_eq!(verdict(&aa, &s(-90.0, -2.1)), Verdict::Faster);
        assert_eq!(verdict(&aa, &s(-90.0, -2.0)), Verdict::Noise);
        assert_eq!(verdict(&aa, &s(2.0, 9.0)), Verdict::Noise);
        assert_eq!(verdict(&aa, &s(2.1, 9.0)), Verdict::Slower);
        assert_eq!(verdict(&aa, &s(-1.0, 1.0)), Verdict::Noise);
        assert_eq!(verdict(&s(0.0, 0.0), &s(-1.0, -0.1)), Verdict::Faster);
    }

    #[test]
    fn every_side_takes_every_slot() {
        use Side::*;
        assert_eq!(order(0), [Original, Control, Rust]);
        assert_eq!(order(1), [Control, Rust, Original]);
        assert_eq!(order(2), [Rust, Original, Control]);
        assert_eq!(order(3), order(0));
    }

    #[test]
    fn rounds_are_paired() {
        // a machine that gets 2x slower halfway: pairing keeps the speedup at 10x
        let wall = [[1.0, 1.01, 0.1], [2.0, 1.98, 0.2], [2.0, 2.02, 0.2]];
        let cpu =
            [[Some(4.0), Some(4.0), Some(1.0)], [Some(8.0), Some(8.0), Some(2.0)], [Some(8.0), Some(8.0), Some(2.0)]];
        let b = summarize(&wall, &cpu);
        assert!((b.speedup - 10.0).abs() < 1e-9, "{}", b.speedup);
        assert!((b.speedup_q1 - 10.0).abs() < 1e-9 && (b.speedup_q3 - 10.0).abs() < 1e-9);
        assert!((b.aa.band() - 1.0).abs() < 1e-9, "{:?}", b.aa);
        assert_eq!(b.verdict, Some(Verdict::Faster));
        assert_eq!(b.cpu_speedup, Some(4.0));
        assert_eq!(b.cpu_note().as_deref(), Some("CPU time 4.0×"));
        assert_eq!(b.before.median, 2.0);

        let unknown =
            [[Some(4.0), Some(4.0), None], [Some(8.0), Some(8.0), Some(2.0)], [Some(8.0), Some(8.0), Some(2.0)]];
        let b = summarize(&wall, &unknown);
        assert!(b.cpu_after.is_none() && b.cpu_speedup.is_none() && b.cpu_before.is_some());
    }

    #[test]
    fn varying_output_doesnt_blame_the_port() {
        let mut same = OutputCheck::new(b"a\nb\n".to_vec());
        for _ in 0..3 {
            same.see(true, b"a\nb\n");
            same.see(false, b"a\nb\n");
        }
        assert_eq!(same.diff(), None);

        let mut port = OutputCheck::new(b"a\nb\n".to_vec());
        port.see(true, b"a\nb\n");
        port.see(false, b"a\nB\n");
        port.see(false, b"x\nb\n");
        assert_eq!(port.diff().as_deref(), Some(r#"line 2: "b" became "B""#));

        let mut varies = OutputCheck::new(b"took 1.2s\n".to_vec());
        varies.see(false, b"took 0.1s\n");
        varies.see(true, b"took 1.3s\n");
        assert_eq!(varies.diff().as_deref(), Some(OUTPUT_VARIES));
    }

    #[test]
    fn the_machine_is_named_when_it_tells() {
        let cpuinfo = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 5 PRO 4650GE with Radeon Graphics\nflags\t\t: fpu\n\nprocessor\t: 1\nmodel name\t: other\n";
        assert_eq!(cpuinfo_model(cpuinfo).as_deref(), Some("AMD Ryzen 5 PRO 4650GE with Radeon Graphics"));
        assert_eq!(cpuinfo_model("processor\t: 0\nBogoMIPS\t: 48.00\n"), None);
        let reg = "\r\nHKEY_LOCAL_MACHINE\\HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0\r\n    ProcessorNameString    REG_SZ    Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz\r\n\r\n";
        assert_eq!(registry_value(reg).as_deref(), Some("Intel(R) Core(TM) i7-8700 CPU @ 3.20GHz"));
        let here = Scope::here();
        assert!(here.date.is_some() && !here.os.is_empty() && here.cpus.is_some_and(|n| n > 0), "{here:?}");
    }

    #[test]
    fn cpu_time_counts_the_child() {
        if std::env::var_os("FA_TEST_BURN_CPU").is_some() {
            // fixed work, so the CPU time doesn't depend on how busy the machine is
            let mut x = 1u64;
            for _ in 0..100_000_000u64 {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
            return;
        }
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args(["--exact", "bench::tests::cpu_time_counts_the_child", "--quiet"])
            .env("FA_TEST_BURN_CPU", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (out, cpu) = output_with_cpu(&mut cmd).unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(String::from_utf8_lossy(&out.stdout).contains("1 passed"), "{}", String::from_utf8_lossy(&out.stdout));
        let cpu = cpu.expect("CPU time of the child");
        assert!(cpu > 0.05, "{cpu}");
    }
}
