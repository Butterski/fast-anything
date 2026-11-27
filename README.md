<p align="center">
  <img src="docs/assets/wordmark.png" alt="fast-anything" width="520">
</p>

<p align="center"><b>Rewrite the 5% that's slow, in C++ with SIMD, and prove it behaves the same.</b></p>

<p align="center">
  <a href="https://github.com/Butterski/fast-anything/actions/workflows/ci.yml"><img src="https://github.com/Butterski/fast-anything/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <img src="https://img.shields.io/badge/ports-C%2B%2B20%20%C2%B7%20SIMD-00599C" alt="Ports in C++20 with SIMD">
  <img src="https://img.shields.io/badge/Python%20%C2%B7%20Node%20%C2%B7%20TypeScript%20%C2%B7%20Ruby%20%C2%B7%20Java%20%C2%B7%20Kotlin%20%C2%B7%20Go%20%C2%B7%20PHP%20%C2%B7%20C%20%C2%B7%20C%2B%2B%20%C2%B7%20C%23%20%C2%B7%20R-2563eb" alt="Python, Node, TypeScript, Ruby, Java, Kotlin, Go, PHP, C, C++, C#, R">
  <img src="https://img.shields.io/badge/MCP-server-555" alt="MCP server">
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-555" alt="MIT"></a>
</p>

This is a maintained fork of [EL4CTEO/fast-anything](https://github.com/EL4CTEO/fast-anything). fast-anything profiles your app, has your coding agent port only its hottest pure functions to native code, proves each port gives the same results on real recorded calls, and swaps it in, fully reversible.

Upstream writes those ports in Rust. **This fork writes them in C++, with SIMD where it pays**, the way [RayoMD](https://github.com/Butterski/rayomd) (Markdown to PDF) and [RayoPDF](https://github.com/Butterski/rayopdf) (PDF to Markdown) are optimised, and we maintain it.

## Status

| Your app | Its port |
|---|---|
| Python 3.11+ on Linux and macOS | **C++20** (the default): a CPython stable-ABI module, built with `-O3 -march=native` |
| Python on Windows; Node.js/TypeScript, Ruby, Java, Kotlin, Go, PHP, C, C++, C#, R | Rust, as upstream (C++ for these is on the [roadmap](#roadmap)) |

`port --backend rust` gives a Python app a Rust port too.

## Ports, the RayoMD way

Upstream's proof stays as it is: recorded calls, derived edge cases, held-out calls, and a benchmark with an A/A control. On top of it, ports here follow what the RayoMD and RayoPDF work showed:

- **SIMD, with a plain path next to it.** RayoMD's inline-parse test jumps between the bytes that matter 32 at a time with AVX2 nibble tables, and keeps a byte-at-a-time loop for CPUs without AVX2: essay.md took 10.7% fewer instructions, and every PDF stayed byte for byte the same ([performance.md](https://github.com/Butterski/rayomd/blob/main/docs/development/performance.md), "The inline parse test 32 bytes a step"). RayoPDF computes CRC-32 with PCLMULQDQ folding and Adler-32 with AVX2. Ports here are built with `-O3 -march=native`, and their TASK.md asks for hot loops the compiler can vectorize, or intrinsics behind `#ifdef __AVX2__` / `__ARM_NEON` with a plain loop for the rest.
- **The same output, proven, before any speed counts.** RayoMD checks each change byte for byte against the PDFs it made before (16 documents in 10 option sets and 2 books for the inline-parse one). Every port here passes verify on all recorded and derived calls before it can be switched on.
- **Python's float semantics, kept.** `-ffp-contract=off` and no fast-math: the compiler may not fuse or reorder float operations, so results match Python's. A vectorized float sum is allowed only where the original adds in that order.
- **A better algorithm first, then threads.** The demo's reference port replaces the edit-distance table with a bit-parallel one (a column of 64 cells per machine-word step) and spreads rows over threads. The GIL is released while a port runs.

## Results

AMD Ryzen 5 PRO 4650GE, Ubuntu 26.04 in Docker, g++ 15, Python 3.14, 2026-11-27. Reproduce it from [`examples/fuzzy-dedupe`](examples/fuzzy-dedupe) (`reference/port.cpp` and `reference/lib.rs` stand in for the agent).

| Python fuzzy dedupe (700 names), one call | time | speed-up |
|---|---:|---:|
| the Python original | 4,744 ms | 1× |
| upstream's Rust reference port (edit-distance table, threads) | 11.1 ms | 427× |
| this fork's C++ reference port (bit-parallel edit distance, threads) | 3.3 ms | 1,454× |

The whole app went from 5.50 s to 38 ms with the C++ port (149×; CPU time 136×). The two reference ports use different algorithms, so the table compares the ports as written, not the languages.

## Install

```sh
cargo install --git https://github.com/Butterski/fast-anything
```

The CLI itself is Rust: building it needs [rustup](https://rustup.rs) and a C linker. C++ ports need g++ or clang++ (or `$CXX`) and the headers of the Python your app runs on (Debian/Ubuntu: `sudo apt install build-essential python3-dev`); Rust ports need the Rust toolchain. No toolchain on the host? Build and test in the image instead:

```sh
docker build -t fast-anything-dev -f docker/dev.Dockerfile docker
docker/dev.sh cargo test
```

This fork isn't on npm or crates.io: `npm i -g @el4cteo/fast-anything` and `cargo install fast-anything` install upstream, with Rust ports.

## Use

```sh
fast-anything go --agent claude -- python app.py     # or: -- node app.ts / bun app.ts / deno run -A app.ts / ruby app.rb / java App.java / mvn exec:java / gradle run / go run . / php app.php / clang -O2 main.c / Rscript app.R
```

Or step by step:

| command | what it does |
|---|---|
| `scan -- <cmd>` | profiles the app, ranks functions by time × how portable they are |
| `port [id]` | records up to 500 real calls and creates the port folder + `TASK.md`: C++ for Python apps, else a one-function Rust crate (`--backend cpp\|rust` to choose) |
| `verify` | release-builds the port and replays every recorded call |
| `enable` / `disable` | swaps the port in or out (one marked block, removed byte for byte) |
| `bench` | times the whole app in rotated rounds of original, original again (an A/A control for noise) and ported, plus one warm-up per side (`--runs 5` = 11 original runs, 6 ported runs); wall and CPU time |
| `report` | markdown + SVG card for your PR |
| `doctor` | checks the C++ compiler and Python headers, Rust, your runtimes and your agents |

`--agent` takes `claude`, `codex`, or any command that reads a prompt on stdin. Without it, hand `TASK.md` to whatever you use.

## What a C++ port looks like

`fast-anything port` writes `.fast/ports/<id>/`:

| file | what it is |
|---|---|
| `port.cpp` | the function, written by your agent (fast-anything writes a stub once) |
| `port.h` | the C++ signature, generated from the Python annotations |
| `glue.cpp`, `fa/py.hpp` | the CPython module: argument parsing like CPython's, conversions, exceptions, the GIL |
| `TASK.md` | the original, real calls, the rules; `compile_flags.txt` for clangd |

Python values arrive as C++ values: `str` as `std::u32string` (one `char32_t` per code point, so lengths and indexing match Python), `int` as `int64_t`, `float` as `double`, `bytes` as `std::string`, `X | None` as `std::optional`, `list[T]` as `std::vector<T>`, `tuple[A, B]` as `std::tuple`, `dict[K, V]` as `fa::dict<K, V>` (insertion-ordered, like Python's), `set[T]` as `fa::set<T>`, `NDArray[np.float64]` as `fa::ndarray<const double>` (results: `fa::array<double>`), a dataclass parameter as a generated struct. Anything else is a `fa::object`, and then the GIL stays held. A port raises Python exceptions by throwing `fa::value_error`, `fa::key_error`, ... or `fa::error("AnyBuiltinError", "...")`, and `fa::str_method<R>(s, "lower")` runs Python's own string methods for the non-ASCII cases.

## Proof, not vibes

On top of the recorded calls, fast-anything derives extra cases and lets the original answer them: edge cases (NaN, ±infinity, -0.0, zero and ±1, non-ASCII text, empty and up to 16× bigger inputs) and shrunk inputs (tiny and random slices). Nothing is enabled until every case matches: same values (floats within 1e-9 of their size or of the largest value in the same list, -0.0 not 0.0), errors where the original errors. A port that is right but slower per call than the original (calling into it costs more than the work saved) isn't enabled either, nor is one whose original takes about as long as a clock read.

Every 5th recorded call (from 5 calls up) is held out of the folder the agent works in: verify replays it too but reports a mismatch there without the values, so a port that special-cases the calls it was shown still fails. It's a guard against a port fitted to the calls it was shown, not a sandbox: an agent that goes looking can still read them in `.fast/holdout`.

## Agents

**Plugin**: the repo is an agent plugin marketplace ([`.agents/plugins/marketplace.json`](.agents/plugins/marketplace.json)) that bundles the skill and the MCP server. Install the CLI first, then:

- **Codex**: `codex plugin marketplace add Butterski/fast-anything`, then install `fast-anything` from `/plugins`
- **Claude Code**: `/plugin marketplace add Butterski/fast-anything`, then `/plugin install fast-anything@fast-anything` and `/fast-anything:fast python app.py`

Or wire it up by hand:

- **MCP server** (Claude Code, Codex, Cursor…), built into the binary: `claude mcp add fast-anything -- fast-anything mcp`. 8 tools: `fast_scan`, `fast_port` (takes `backend`), `fast_verify`, `fast_enable`, `fast_disable`, `fast_bench`, `fast_report`, `fast_status`.
- **Skill**: [`skills/fast-anything`](skills/fast-anything/SKILL.md) teaches any agent the loop.

## Languages

| language | port and binding | status |
|---|---|---|
| Python 3.11+ (Linux, macOS) | C++20 through the CPython stable ABI (`fa/py.hpp`); servers and GUIs: `scan --duration 30` (stops them like Ctrl+C) | ✅ |
| Python 3.10+ (Windows, or `--backend rust`) | Rust through PyO3 | ✅ |
| JavaScript / TypeScript (Node 22+, Bun, Deno 2+: `bun app.ts`, `deno run -A app.ts`) | Rust through napi-rs; servers: `scan --duration 30` (ends them with `process.exit`) | ✅ |
| Ruby 3.3+ | Rust, C ABI + JSON, through Ruby's built-in Fiddle; servers: `scan --duration 30` (stops them like Ctrl+C) | ✅ |
| Java 22+ (`java App.java`, Maven, Gradle) | the same C bridge, through the FFM API; servers: `scan --duration 30` | ✅ |
| Kotlin/JVM (Gradle, `src/main/kotlin`, JDK 22+) | the Java bridge; top-level functions, `object` and companion members, extension functions | ✅ |
| Go 1.22+ (`go run .`) | the same C bridge, through the Windows loader or dlopen (cgo); servers: `scan --duration 30` | ✅ |
| PHP 8.1+ (`php app.php`) | the same C bridge, through PHP's FFI extension (`extension=ffi`); servers: `scan --duration 30` (with `pcntl`) | ✅ |
| C and C++ (gcc, clang or Intel oneAPI icx/icpx: `clang -O2 main.c lib.c`) | the same C bridge (C passes its arguments as they are, no JSON), through LoadLibrary or dlopen; servers: `scan --duration 30` | ✅ |
| C# / .NET 8+ (`dotnet run -c Release`) | the same C bridge, through NativeLibrary; profiled with EventPipe, nothing to install; GUIs and servers: `scan --duration 30` | ✅ |
| R 4.0+ (`Rscript app.R`) | the same C bridge, through base R's `.C()`; profiled with Rprof, no packages or R headers needed; servers: `scan --duration 30` | ✅ |

It ports top-level, deterministic functions that take plain data (numbers, strings, lists, dicts, typed arrays…), including ones in your entry script when its main code sits under `if __name__ == "__main__":` (or `if __FILE__ == $0`, or `if (import.meta.main)` / `if (require.main === module)`). In Python that means module-level functions, `@staticmethod`s and `@classmethod`s that don't use `cls` (dataclasses, NamedTuples and numeric NumPy arrays count as plain data), in Java, C# and PHP static methods (and PHP functions), in Go plain functions, in C and C++ free functions and static methods. `scan` says why anything else can't move yet.

## Roadmap

- **Memory safety in verify.** C++ has no borrow checker, and equal results don't rule out an out-of-bounds read: verify will also replay the calls against an AddressSanitizer and UndefinedBehaviorSanitizer build.
- **Instruction counts.** RayoMD gates its changes on instructions as well as time (`perf stat`); verify and bench will report them per port.
- **C++ for the C-bridge languages** (Ruby, Java, Kotlin, Go, PHP, C, C++, C#, R): the same C entry point with JSON in and out, built by `c++` instead of cargo, so the bridges stay as they are.
- **Node.js** through Node-API in C++, and **Windows** (MSVC or clang-cl).

## Upstream

`main` here is upstream's latest release plus this fork's commits; upstream releases are merged in as they come. [CHANGELOG.md](CHANGELOG.md) has the fork's changes on top.

## License

MIT, as upstream ([LICENSE](LICENSE) keeps EL4CTEO's notice).
