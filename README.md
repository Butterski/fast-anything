<p align="center">
  <img src="https://raw.githubusercontent.com/EL4CTEO/fast-anything/main/docs/assets/wordmark.png" alt="fast-anything" width="520">
</p>

<p align="center"><b>Don't rewrite it in Rust. Rewrite the 5% that's slow.</b></p>

<p align="center">
  <a href="https://trendshift.io/repositories/371252"><img src="https://trendshift.io/api/badge/repositories/371252" alt="EL4CTEO/fast-anything | Trendshift" width="250" height="55"></a>
</p>

<p align="center">
  <a href="https://www.npmjs.com/package/@el4cteo/fast-anything"><img src="https://img.shields.io/npm/v/@el4cteo/fast-anything?color=f74c00&label=npm" alt="npm"></a>
  <a href="https://crates.io/crates/fast-anything"><img src="https://img.shields.io/crates/v/fast-anything?color=f74c00" alt="crates.io"></a>
  <a href="https://github.com/EL4CTEO/fast-anything/actions/workflows/ci.yml"><img src="https://github.com/EL4CTEO/fast-anything/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/Python%20%C2%B7%20Node%20%C2%B7%20TypeScript%20%C2%B7%20Ruby%20%C2%B7%20Java%20%C2%B7%20Kotlin%20%C2%B7%20Go%20%C2%B7%20PHP%20%C2%B7%20C%20%C2%B7%20C%2B%2B%20%C2%B7%20C%23%20%C2%B7%20R-2563eb" alt="Python, Node, TypeScript, Ruby, Java, Kotlin, Go, PHP, C, C++, C#, R">
  <img src="https://img.shields.io/badge/Claude%20Code-plugin-d97757" alt="Claude Code plugin">
  <img src="https://img.shields.io/badge/MCP-server-555" alt="MCP server">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-555" alt="MIT"></a>
</p>

Rewriting a whole app in Rust costs months, minute-long compiles and gigabytes of `target/`. Most of the runtime sits in a few functions anyway.

**fast-anything** finds those functions, has your coding agent port *only them* to Rust, proves the port gives the same results on real recorded calls, and swaps it in. One command, fully reversible.

<p align="center"><img src="https://raw.githubusercontent.com/EL4CTEO/fast-anything/main/docs/assets/demo.gif" alt="fast-anything go --agent claude -- python app.py" width="820"></p>

## This fork: C++ ports for Python apps

This fork ([Butterski/fast-anything](https://github.com/Butterski/fast-anything), branch `cpp`) ports the hot functions of **Python apps to C++** on Linux and macOS: the agent writes `port.cpp`, and fast-anything generates the signature (`port.h`) and a CPython stable-ABI module (`glue.cpp` on top of `fa/py.hpp`), builds it with `-O3 -march=native -ffp-contract=off`, and verifies, enables and benchmarks it as it does Rust ports. The other languages keep Rust ports, and `port --backend rust` gives a Python app one too.

- Needs a C++20 compiler (g++ or clang++, or `$CXX`) and the headers of Python 3.11 or newer (Debian/Ubuntu: `apt install build-essential python3-dev`).
- Python values arrive as C++ values: `str` as `std::u32string` (code points), `int` as `int64_t`, `float` as `double`, `list[T]` as `std::vector<T>`, `dict[K, V]` as `fa::dict<K, V>` (insertion-ordered), `NDArray[np.float64]` as `fa::ndarray<const double>`. Each port's TASK.md has the full list.
- The GIL is released while a port runs, so it may use threads; SIMD comes from the compiler's vectorizer or from intrinsics.
- `examples/fuzzy-dedupe/reference/port.cpp` is a reference port: bit-parallel edit distance on threads.
- No compiler on the host? `docker build -t fast-anything-dev -f docker/dev.Dockerfile docker`, then `docker/dev.sh cargo test` builds and tests inside the image.

## Install

```sh
npm i -g @el4cteo/fast-anything      # or: cargo install fast-anything
cargo install --git https://github.com/Butterski/fast-anything --branch cpp   # this fork
```

Rust ports are compiled with your Rust toolchain ([rustup](https://rustup.rs)), C++ ports with g++ or clang++.

## Use

```sh
fast-anything go --agent claude -- python app.py     # or: -- node app.ts / bun app.ts / deno run -A app.ts / ruby app.rb / java App.java / mvn exec:java / gradle run / go run . / php app.php / clang -O2 main.c / Rscript app.R
```

Or step by step:

| command | what it does |
|---|---|
| `scan -- <cmd>` | profiles the app, ranks functions by time × how portable they are |
| `port [id]` | records up to 500 real calls, creates a one-function Rust crate + `TASK.md` |
| `verify` | release-builds the port and replays every recorded call |
| `enable` / `disable` | swaps the Rust in or out (one marked block, removed byte for byte) |
| `bench` | times the whole app in rotated rounds of original, original again (an A/A control for noise) and Rust, plus one warm-up per side (`--runs 5` = 11 original runs, 6 Rust runs); wall and CPU time |
| `report` | markdown + SVG card for your PR |
| `doctor` | checks Rust, your runtimes and your agents |

`--agent` takes `claude`, `codex`, or any command that reads a prompt on stdin. Without it, hand `TASK.md` to whatever you use.

## Proof, not vibes

<p align="center"><img src="https://raw.githubusercontent.com/EL4CTEO/fast-anything/main/docs/assets/verify.gif" alt="verify catches a wrong port" width="820"></p>

That port looked right but compared bytes instead of characters. 148 of 500 real calls caught it. On top of the recorded calls, fast-anything derives extra cases and lets the original answer them: edge cases (NaN, ±infinity, -0.0, zero and ±1, non-ASCII text, empty and up to 16× bigger inputs) and shrunk inputs (tiny and random slices). Nothing is enabled until every case matches: same values (floats within 1e-9 of their size or of the largest value in the same list, -0.0 not 0.0), errors where the original errors. A port that is right but slower per call than the original (calling into Rust costs more than the work saved) isn't enabled either, nor is one whose original takes about as long as a clock read (where each call is timed on its own: C, C++, Go, Java, Kotlin, PHP, C#), since the per-call times can't show a saving there. `FAST_ANYTHING_DISABLE=1` runs the original any time.

Every 5th recorded call (from 5 calls up) is held out of the crate folder the agent works in: verify replays it too but reports a mismatch there without the values, so a port that special-cases the calls it was shown still fails. verify deletes its working copy of those calls when it's done. It's a guard against a port fitted to the calls it was shown, not a sandbox: an agent that goes looking can still read them in `.fast/holdout`.

## Results

Real runs on an i5-10500, Windows 11 (C and C++: a 4-core GitHub Actions Linux runner). Reproduce them from [`examples/`](examples).

| app | before | after |
|---|---|---|
| Python fuzzy dedupe (700 names) | 4.91s | **42ms, 116×** |
| TypeScript fuzzy dedupe (2,000 names) | 3.11s | **155ms, 20×** |
| PHP fuzzy dedupe (600 names) | 2.94s | **53ms, 55×** |
| Go fuzzy dedupe (3,000 names) | 3.02s | **144ms, 21×** |
| Ruby fuzzy dedupe (400 names) | 3.13s | **228ms, 14×** |
| C# fuzzy dedupe (4,000 names) | 4.03s | **312ms, 13×** |
| Java fuzzy dedupe (4,000 names) | 3.50s | **1.18s, 3×** (11× on the function; the rest is JVM startup) |
| C mandelbrot (8 zoom levels) | 431ms | **127ms, 3.4×** |
| C++ fuzzy dedupe (6,000 names) | 3.02s | **1.09s, 2.7×** |

fast-anything ports the outermost loop it can, not the tiny function called inside it, so the app crosses into Rust once instead of millions of times. Porting only the inner `levenshtein` gave 35× and 1.9×.

## Why not rewrite it all?

- **Compile time**: each port is a one-function crate. Seconds, not minutes.
- **Cache**: every port shares one `.fast/target`. Delete it whenever you like (`fast-anything clean`).
- **Risk**: your code stays as it is. Rust is opt-in per function, checked against real behavior.

## Agents

**Plugin**: the repo is an agent plugin marketplace ([`.agents/plugins/marketplace.json`](.agents/plugins/marketplace.json)) that bundles the skill and the MCP server. Install the CLI first, then:

- **Codex**: `codex plugin marketplace add EL4CTEO/fast-anything`, then install `fast-anything` from `/plugins`
- **Claude Code**: `/plugin marketplace add EL4CTEO/fast-anything`, then `/plugin install fast-anything@fast-anything` and `/fast-anything:fast python app.py`

Or wire it up by hand:

- **MCP server** (Claude Code, Codex, Cursor…), built into the binary: `claude mcp add fast-anything -- fast-anything mcp`. 8 tools: `fast_scan`, `fast_port`, `fast_verify`, `fast_enable`, `fast_disable`, `fast_bench`, `fast_report`, `fast_status`.
- **Skill**: [`skills/fast-anything`](skills/fast-anything/SKILL.md) teaches any agent the loop.

## Languages

| language | binding | status |
|---|---|---|
| Python 3.10+ | PyO3; servers and GUIs: `scan --duration 30` (stops them like Ctrl+C) | ✅ |
| JavaScript / TypeScript (Node 22+, Bun, Deno 2+: `bun app.ts`, `deno run -A app.ts`) | napi-rs; servers: `scan --duration 30` (ends them with `process.exit`) | ✅ |
| Ruby 3.3+ | C ABI + JSON, through Ruby's built-in Fiddle; servers: `scan --duration 30` (stops them like Ctrl+C) | ✅ |
| Java 22+ (`java App.java`, Maven, Gradle) | the same C bridge, through the FFM API; servers: `scan --duration 30` | ✅ |
| Kotlin/JVM (Gradle, `src/main/kotlin`, JDK 22+) | the Java bridge; top-level functions, `object` and companion members, extension functions | ✅ |
| Go 1.22+ (`go run .`) | the same C bridge, through the Windows loader or dlopen (cgo); servers: `scan --duration 30` | ✅ |
| PHP 8.1+ (`php app.php`) | the same C bridge, through PHP's FFI extension (`extension=ffi`); servers: `scan --duration 30` (with `pcntl`) | ✅ |
| C and C++ (gcc, clang or Intel oneAPI icx/icpx: `clang -O2 main.c lib.c`) | the same C bridge (C passes its arguments as they are, no JSON), through LoadLibrary or dlopen; servers: `scan --duration 30` | ✅ |
| C# / .NET 8+ (`dotnet run -c Release`) | the same C bridge, through NativeLibrary; profiled with EventPipe, nothing to install; GUIs and servers: `scan --duration 30` | ✅ |
| R 4.0+ (`Rscript app.R`) | the same C bridge, through base R's `.C()`; profiled with Rprof, no packages or R headers needed; servers: `scan --duration 30` | ✅ |

It ports top-level, deterministic functions that take plain data (numbers, strings, lists, dicts, typed arrays…), including ones in your entry script when its main code sits under `if __name__ == "__main__":` (or `if __FILE__ == $0`, or `if (import.meta.main)` / `if (require.main === module)`). In Python that means module-level functions, `@staticmethod`s and `@classmethod`s that don't use `cls` (dataclasses, NamedTuples and numeric NumPy arrays count as plain data), in Java, C# and PHP static methods (and PHP functions), in Go plain functions, in C and C++ free functions and static methods. `scan` says why anything else can't move yet.

With Intel oneAPI on Windows, run fast-anything from a shell where `setvars.bat` ran (it sets up the linker). SYCL kernels run on the device and aren't ported; with `-fsycl`, only host functions are profiled and ported.

Ruby, Java, Go, PHP, C, C++, C# and R need no headers, bindings or extra tools: the port is a plain Rust library with one C entry point taking and returning JSON, and fast-anything replays the recorded calls against it directly. Any language that can call C can use that bridge. A C port also has a typed entry point that takes the function's own arguments and returns its result, NaN and infinity included; the C app calls that one, and verify replays and times it.

## License

MIT
