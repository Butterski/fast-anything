// src/cpp/py.hpp, the header C++ Python ports build against: compiles a module of test ports
// with generated-style glue (tests/cpp_py_header/) and runs tests/cpp_py_header/test.py on it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Python's include directory, when it holds Python.h of Python 3.11 or newer (the stable ABI the
/// header is built for).
fn python_include() -> Option<PathBuf> {
    let out = Command::new("python3")
        .args(["-c", "import sys, sysconfig; print(sys.version_info >= (3, 11)); print(sysconfig.get_path('include'))"])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let mut lines = text.lines();
    if !out.status.success() || lines.next() != Some("True") {
        return None;
    }
    let dir = PathBuf::from(lines.next()?.trim());
    dir.join("Python.h").is_file().then_some(dir)
}

/// The C++ compiler, as the port build picks it: $CXX, else c++.
fn cxx() -> String {
    std::env::var("CXX").ok().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| "c++".into())
}

#[test]
fn header_builds_a_module_that_converts_parses_and_raises_like_python() {
    // C++ ports are built on Linux and macOS only (see Backend::choose)
    let ready = !cfg!(windows) && Command::new(cxx()).arg("--version").output().is_ok_and(|o| o.status.success());
    let Some(include) = python_include().filter(|_| ready) else {
        eprintln!("no c++ or Python 3.11+ headers (or Windows), skipping");
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixtures = root.join("tests/cpp_py_header");
    let dir = std::env::temp_dir().join(format!("fa-cpp-py-header-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("fa")).unwrap();
    fs::copy(root.join("src/cpp/py.hpp"), dir.join("fa/py.hpp")).unwrap();
    let mut sources: Vec<PathBuf> = fs::read_dir(&fixtures)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "cpp"))
        .collect();
    sources.sort();

    // The test flags, and the port build's own (with the stable ABI level spelled out).
    let native = if cfg!(target_arch = "aarch64") { "-mcpu=native" } else { "-march=native" };
    let builds: [(&str, Vec<&str>); 2] = [
        ("o2", vec!["-O2"]),
        ("o3", vec!["-O3", native, "-ffp-contract=off", "-fno-math-errno", "-DPy_LIMITED_API=0x030B0000"]),
    ];
    std::thread::scope(|s| {
        let jobs: Vec<_> = builds
            .iter()
            .map(|(name, flags)| {
                let out = dir.join(name);
                fs::create_dir_all(&out).unwrap();
                let mut cmd = Command::new(cxx());
                cmd.args(["-std=c++20", "-fPIC", "-shared", "-fvisibility=hidden", "-pthread"])
                    .args(["-Wall", "-Wextra", "-Wno-unused-parameter", "-Werror"])
                    .args(flags)
                    .arg(format!("-I{}", dir.display()))
                    .arg(format!("-I{}", fixtures.display()))
                    .arg(format!("-I{}", include.display()));
                if cfg!(target_os = "macos") {
                    cmd.args(["-undefined", "dynamic_lookup"]);
                }
                cmd.args(&sources).arg("-o").arg(out.join("fa_test_py.abi3.so"));
                (name, s.spawn(move || cmd.output().unwrap()))
            })
            .collect();
        for (name, job) in jobs {
            let built = job.join().unwrap();
            assert!(built.status.success(), "{name} build:\n{}", String::from_utf8_lossy(&built.stderr));
        }
    });

    for (name, _) in &builds {
        let ran = Command::new("python3").arg(fixtures.join("test.py")).arg(dir.join(name)).output().unwrap();
        let stdout = String::from_utf8_lossy(&ran.stdout);
        let stderr = String::from_utf8_lossy(&ran.stderr);
        assert!(ran.status.success(), "{name}: test.py failed:\n{stdout}{stderr}");
        assert!(stdout.contains("cases passed"), "{name}: {stdout}{stderr}");
        eprintln!("{name}: {}", stdout.trim());
    }
    let _ = fs::remove_dir_all(&dir);
}
