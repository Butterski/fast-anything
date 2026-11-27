//! C++ ports for Python apps (this fork): the agent writes `port.cpp`; fast-anything writes the
//! signature (`port.h`), the CPython module (`glue.cpp`, on top of `fa/py.hpp`) and builds a
//! stable-ABI extension that verify, enable and bench load exactly like a PyO3 one.

use crate::hotspot::{Hotspot, Param};
use crate::python;
use crate::state::{Ctx, Port};
use crate::util::{output, tail};
use anyhow::{Context, Result, bail, ensure};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The CPython glue every C++ port includes as fa/py.hpp.
pub const PY_HPP: &str = include_str!("cpp/py.hpp");

/// Sources of a C++ port: what the build compiles, the source hash covers and an agent snapshot keeps.
const SOURCE_EXTS: &[&str] = &["cpp", "cc", "cxx", "h", "hh", "hpp"];

/// Compiler flags of every C++ port. Tuned for the machine that builds it (.fast/lib is rebuilt per
/// machine, never shipped), with Python's float semantics: no FMA contraction, no fast-math.
pub fn flags() -> Vec<&'static str> {
    let mut f = vec!["-std=c++20", "-O3"];
    if cfg!(target_arch = "x86_64") {
        f.push("-march=native");
    } else if cfg!(target_arch = "aarch64") {
        f.push("-mcpu=native");
    }
    f.extend([
        "-ffp-contract=off",
        "-fno-math-errno",
        "-fPIC",
        "-fvisibility=hidden",
        "-pthread",
        "-Wall",
        "-Wextra",
        "-Wno-unused-parameter",
    ]);
    f
}

/// The C++ compiler: $CXX, else c++, g++ or clang++ from PATH.
pub fn compiler() -> Result<String> {
    if let Some(cxx) = std::env::var("CXX").ok().filter(|c| !c.trim().is_empty()) {
        return Ok(cxx);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let on_path = |name: &str| std::env::split_paths(&path).any(|dir| dir.join(name).is_file());
    for name in ["c++", "g++", "clang++"] {
        if on_path(name) {
            return Ok(name.to_string());
        }
    }
    bail!("no C++ compiler on PATH: install g++ (Debian/Ubuntu: build-essential) or clang++, or set CXX")
}

/// Folder holding Python.h for this interpreter, which must be 3.11 or newer: the glue uses the
/// stable ABI of 3.11 (the buffer protocol joined it then).
pub fn python_include(python: &str) -> Result<PathBuf> {
    let out = output(
        Command::new(python).args([
            "-c",
            "import sys, sysconfig; print(sys.version_info >= (3, 11)); print(sysconfig.get_path('include'))",
        ]),
        "python",
    )?;
    ensure!(out.status.success(), "{python} couldn't tell where its headers are");
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    ensure!(
        lines.next() == Some("True"),
        "C++ ports need Python 3.11 or newer ({python} is older): port with --backend rust, or scan with a newer Python"
    );
    let dir = PathBuf::from(lines.next().unwrap_or_default().trim());
    ensure!(
        dir.join("Python.h").is_file(),
        "C++ ports need the Python headers, and {} has none. Install them (Debian/Ubuntu: python3-dev, Fedora: python3-devel), or use a Python from python.org, uv or pyenv",
        dir.join("Python.h").display()
    );
    Ok(dir)
}

// ------------------------------------------------------------------ types

const CPP_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "and_eq",
    "asm",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "char8_t",
    "char16_t",
    "char32_t",
    "class",
    "compl",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "const_cast",
    "continue",
    "co_await",
    "co_return",
    "co_yield",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "not_eq",
    "nullptr",
    "operator",
    "or",
    "or_eq",
    "private",
    "protected",
    "public",
    "register",
    "reinterpret_cast",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "thread_local",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "wchar_t",
    "while",
    "xor",
    "xor_eq",
    // the generated glue's own locals
    "fa_self",
    "fa_pos",
    "fa_kw",
    "fa_args",
];

/// A Python name as a C++ identifier: keywords and the glue's locals get a trailing underscore.
fn ident(name: &str) -> String {
    if CPP_KEYWORDS.contains(&name) { format!("{name}_") } else { name.to_string() }
}

/// C++ element type for a NumPy dtype in NDArray[...].
fn numpy_cpp(dtype: &str) -> Option<&'static str> {
    Some(match python::numpy_elem(dtype)? {
        "f64" => "double",
        "f32" => "float",
        "i64" => "int64_t",
        "i32" => "int32_t",
        "i16" => "int16_t",
        "i8" => "int8_t",
        "u64" => "uint64_t",
        "u32" => "uint32_t",
        "u16" => "uint16_t",
        "u8" => "uint8_t",
        "bool" => "bool",
        _ => return None,
    })
}

/// C++ value type for a Python annotation, as a parameter (`returned` false) or in the result. None
/// when there's no obvious one: the glue then passes a fa::object. `known` names the generated
/// struct of a dataclass or NamedTuple.
fn cpp_type(annotation: &str, returned: bool, known: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let a: String = annotation.chars().filter(|c| !c.is_whitespace()).collect();
    let a = a.trim_start_matches("typing.");
    let inner = |prefix: &[&str]| {
        prefix
            .iter()
            .find_map(|p| a.strip_prefix(p).and_then(|r| r.strip_prefix('[')).and_then(|r| r.strip_suffix(']')))
    };
    if let Some(x) = a.strip_suffix("|None").or_else(|| a.strip_prefix("None|")).or_else(|| inner(&["Optional"])) {
        return cpp_type(x, returned, known).map(|t| format!("std::optional<{t}>"));
    }
    Some(match a {
        "int" => "int64_t".into(),
        "float" => "double".into(),
        "bool" => "bool".into(),
        "str" => "std::u32string".into(),
        "bytes" | "bytearray" => "std::string".into(),
        _ => {
            if let Some(t) = known(a) {
                // a dataclass in the result is built as a Python object (see the TASK.md note)
                if returned {
                    return None;
                }
                t
            } else if let Some(x) = inner(&["NDArray", "npt.NDArray", "np.typing.NDArray", "numpy.typing.NDArray"]) {
                let elem = numpy_cpp(x)?;
                if returned { format!("fa::array<{elem}>") } else { format!("fa::ndarray<const {elem}>") }
            } else if let Some(x) = inner(&["list", "List", "Sequence"]) {
                format!("std::vector<{}>", cpp_type(x, returned, known)?)
            } else if let Some(x) = inner(&["tuple", "Tuple"]) {
                let parts = python::split_top(x);
                if parts.len() == 2 && parts[1].trim() == "..." {
                    let t = cpp_type(&parts[0], returned, known)?;
                    if returned { format!("fa::tuple_of<{t}>") } else { format!("std::vector<{t}>") }
                } else {
                    if parts.is_empty() || parts.iter().any(|p| p.trim() == "...") {
                        return None;
                    }
                    let types: Option<Vec<String>> = parts.iter().map(|p| cpp_type(p, returned, known)).collect();
                    format!("std::tuple<{}>", types?.join(", "))
                }
            } else if let Some(x) = inner(&["dict", "Dict", "Mapping"]) {
                let kv = python::split_top(x);
                if kv.len() != 2 {
                    return None;
                }
                format!("fa::dict<{}, {}>", cpp_type(&kv[0], returned, known)?, cpp_type(&kv[1], returned, known)?)
            } else {
                let x = inner(&["set", "Set", "frozenset"])?;
                format!("fa::set<{}>", cpp_type(x, returned, known)?)
            }
        }
    })
}

/// Parameter types the port takes by value; everything else comes as a const reference.
fn by_value(ty: &str) -> bool {
    let scalar = |t: &str| matches!(t, "int64_t" | "double" | "bool");
    scalar(ty) || ty.strip_prefix("std::optional<").and_then(|t| t.strip_suffix('>')).is_some_and(scalar)
}

/// A str as a C++ u32string expression (embedded NULs kept).
fn u32_literal(s: &str) -> String {
    let mut lit = String::new();
    for c in s.chars() {
        if (' '..='~').contains(&c) && c != '"' && c != '\\' {
            lit.push(c);
        } else {
            lit.push_str(&format!("\\U{:08X}", c as u32));
        }
    }
    format!("std::u32string(U\"{lit}\", {})", s.chars().count())
}

/// A Python default as a C++ expression of type `ty`, if it's a simple literal.
fn cpp_default(value: &str, ty: &str) -> Option<String> {
    let v = value.trim();
    let (inner, optional) = match ty.strip_prefix("std::optional<").and_then(|t| t.strip_suffix('>')) {
        Some(inner) => (inner, true),
        None => (ty, false),
    };
    if v == "None" {
        return optional.then(|| "std::nullopt".into());
    }
    let lit = match inner {
        "bool" => match v {
            "True" => "true".into(),
            "False" => "false".into(),
            _ => return None,
        },
        "int64_t" => match v.parse::<i64>().ok()? {
            i64::MIN => "INT64_MIN".into(),
            n => format!("INT64_C({n})"),
        },
        "double" => {
            let stripped = v.strip_prefix('+').unwrap_or(v);
            let unsigned = stripped.strip_prefix('-').unwrap_or(stripped);
            if !unsigned.bytes().next().is_some_and(|b| b.is_ascii_digit()) {
                return None;
            }
            if !stripped.parse::<f64>().is_ok_and(f64::is_finite) {
                return None;
            }
            if stripped.contains(['.', 'e', 'E']) {
                stripped.to_string()
            } else if stripped == "-0" {
                "0.0".into()
            } else {
                format!("{stripped}.0")
            }
        }
        "std::u32string" => u32_literal(&python::parse_py_str_literal(v)?),
        _ => return None,
    };
    Some(if optional { format!("{ty}({lit})") } else { lit })
}

/// A dataclass or NamedTuple the parameters use, when every field has a C++ type.
struct Record {
    class: String,
    fields: Vec<(String, String)>,
}

fn records(hs: &Hotspot) -> Vec<Record> {
    let in_params = |name: &str| hs.params.iter().any(|p| python::mentions(p.annotation.as_deref(), name));
    let mut out = vec![];
    for (class, fields) in hs.records.iter().filter(|(name, _)| in_params(name)) {
        let types: Option<Vec<String>> =
            fields.iter().map(|f| f.annotation.as_deref().and_then(|a| cpp_type(a, false, &|_| None))).collect();
        if let Some(types) = types {
            out.push(Record {
                class: class.clone(),
                fields: fields.iter().map(|f| f.name.clone()).zip(types).collect(),
            });
        }
    }
    out
}

struct SigParam {
    name: String,
    py_name: String,
    ty: String,
    default: Option<String>,
}

/// The C++ side of the Python signature.
struct Sig {
    params: Vec<SigParam>,
    /// How many may come positionally: the rest are keyword-only.
    positional: usize,
    ret: String,
    records: Vec<Record>,
    /// Python objects cross the boundary, so the GIL stays held while the port runs.
    keeps_gil: bool,
    /// Parameters whose Python default has no C++ literal: they arrive as std::nullopt.
    late_defaults: Vec<(String, String)>,
}

fn signature(hs: &Hotspot) -> Sig {
    let records = records(hs);
    let known = |a: &str| records.iter().any(|r| r.class == a).then(|| format!("port::{}", ident(a)));
    let mut late_defaults = vec![];
    let params: Vec<SigParam> = hs
        .params
        .iter()
        .map(|p: &Param| {
            let mut ty =
                p.annotation.as_deref().and_then(|a| cpp_type(a, false, &known)).unwrap_or_else(|| "fa::object".into());
            let default = p.default.as_deref().map(|d| match cpp_default(d, &ty) {
                Some(lit) => lit,
                None => {
                    if !ty.starts_with("std::optional<") {
                        ty = format!("std::optional<{ty}>");
                    }
                    if d.trim() != "None" {
                        late_defaults.push((p.name.clone(), d.to_string()));
                    }
                    "std::nullopt".into()
                }
            });
            SigParam { name: ident(&p.name), py_name: p.name.clone(), ty, default }
        })
        .collect();
    let positional = hs.params.iter().position(|p| p.kwonly).unwrap_or(hs.params.len());
    let ret = match hs.returns.as_deref().map(str::trim) {
        Some("None") => "void".into(),
        Some(a) => cpp_type(a, true, &known).unwrap_or_else(|| "fa::object".into()),
        None => "fa::object".into(),
    };
    let keeps_gil = ret.contains("fa::object") || params.iter().any(|p| p.ty.contains("fa::object"));
    Sig { params, positional, ret, records, keeps_gil, late_defaults }
}

fn declaration(hs: &Hotspot, sig: &Sig) -> String {
    let params: Vec<String> = sig
        .params
        .iter()
        .map(|p| if by_value(&p.ty) { format!("{} {}", p.ty, p.name) } else { format!("const {}& {}", p.ty, p.name) })
        .collect();
    format!("{} {}({})", sig.ret, ident(&hs.name), params.join(", "))
}

// ------------------------------------------------------------------ generated files

pub fn port_h(hs: &Hotspot) -> String {
    let sig = signature(hs);
    let mut structs = String::new();
    for r in &sig.records {
        structs.push_str(&format!("struct {} {{\n", ident(&r.class)));
        for (name, ty) in &r.fields {
            structs.push_str(&format!("    {ty} {};\n", ident(name)));
        }
        structs.push_str("};\n\n");
    }
    format!(
        "// The signature fast-anything calls, generated from {file}:{line}. Don't edit: write port.cpp.\n\
         #pragma once\n\
         #include \"fa/py.hpp\"\n\n\
         namespace port {{\n\n\
         {structs}{decl};\n\n\
         }}  // namespace port\n",
        file = hs.file,
        line = hs.line,
        decl = declaration(hs, &sig),
    )
}

pub fn glue_cpp(hs: &Hotspot, port: &Port) -> String {
    let sig = signature(hs);
    let mut convs = String::new();
    for r in &sig.records {
        let reads: Vec<String> =
            r.fields.iter().map(|(name, ty)| format!("fa::py::attr<{ty}>(o, \"{name}\")")).collect();
        convs.push_str(&format!(
            "template <> struct fa::py::conv<port::{s}> {{\n    static port::{s} from(PyObject* o) {{ return port::{s}{{{}}}; }}\n}};\n\n",
            reads.join(", "),
            s = ident(&r.class),
        ));
    }
    let names: Vec<String> = sig.params.iter().map(|p| format!("\"{}\"", p.py_name)).collect();
    let mut body =
        format!("        fa::py::args fa_args(fa_pos, fa_kw, {{{}}}, {});\n", names.join(", "), sig.positional);
    for (i, p) in sig.params.iter().enumerate() {
        match &p.default {
            Some(d) => body.push_str(&format!("        auto {} = fa_args.get_or<{}>({i}, {d});\n", p.name, p.ty)),
            None => body.push_str(&format!("        auto {} = fa_args.get<{}>({i});\n", p.name, p.ty)),
        }
    }
    let args: Vec<&str> = sig.params.iter().map(|p| p.name.as_str()).collect();
    body.push_str(&format!(
        "        return fa::py::run({}, [&] {{ return ::port::{}({}); }});\n",
        !sig.keeps_gil,
        ident(&hs.name),
        args.join(", ")
    ));
    format!(
        "// CPython module {ext}, generated by fast-anything from {file}:{line}. Don't edit: write port.cpp.\n\
         #include \"port.h\"\n\n\
         {convs}\
         static PyObject* fa_entry(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {{\n    \
             return fa::py::guard([&]() -> PyObject* {{\n{body}    }});\n}}\n\n\
         static PyMethodDef fa_methods[] = {{\n    \
             {{\"{func}\", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_entry)), METH_VARARGS | METH_KEYWORDS, nullptr}},\n    \
             {{nullptr, nullptr, 0, nullptr}},\n}};\n\n\
         static PyModuleDef fa_module = {{PyModuleDef_HEAD_INIT, \"{ext}\", nullptr, -1, fa_methods, nullptr, nullptr, nullptr, nullptr}};\n\n\
         PyMODINIT_FUNC PyInit_{ext}(void) {{ return PyModule_Create(&fa_module); }}\n",
        ext = port.ext,
        file = hs.file,
        line = hs.line,
        func = hs.name,
    )
}

pub fn port_cpp(hs: &Hotspot, port: &Port) -> String {
    format!(
        "// C++ port of {id} ({file}:{line}), generated by fast-anything.\n\
         // Replace the throw with a faithful, fast port. TASK.md has the original and real calls.\n\
         // Check it with:  fast-anything verify {id}\n\
         #include \"port.h\"\n\n\
         namespace port {{\n\n\
         {decl} {{\n    throw fa::not_implemented(\"port {id}\");\n}}\n\n\
         }}  // namespace port\n",
        id = port.id,
        file = hs.file,
        line = hs.line,
        decl = declaration(hs, &signature(hs)),
    )
}

fn compile_flags_txt(include: &Path) -> String {
    let mut lines: Vec<String> = flags().iter().map(|f| f.to_string()).collect();
    lines.extend(["-xc++".to_string(), "-I.".to_string(), format!("-I{}", include.display())]);
    lines.join("\n") + "\n"
}

/// TASK.md section listing the same-file functions the target calls, to port alongside it.
fn helpers_md(hs: &Hotspot, helpers: &[Hotspot]) -> String {
    if hs.calls.is_empty() {
        return String::new();
    }
    let mut out =
        "## Helpers it calls\n\nPort these as C++ functions in port.cpp (they stay in Python for everyone else).\n\n"
            .to_string();
    for name in &hs.calls {
        match helpers.iter().find(|h| &h.name == name) {
            Some(h) => out.push_str(&format!("```python\n{}\n```\n\n", h.source)),
            None => out.push_str(&format!("- `{name}`: read it in {}\n", hs.file)),
        }
    }
    out
}

pub fn task_md(hs: &Hotspot, helpers: &[Hotspot], port: &Port, root: &Path, dir: &Path, held_out: usize) -> String {
    let fence = "```";
    let sig = signature(hs);
    let samples = python::sample_calls(&dir.join("recordings.jsonl"), &hs.name, 8).join("\n");
    let mut notes = String::new();
    for w in &hs.warnings {
        notes.push_str(&format!("- Note: {w}.\n"));
    }
    for (name, fields) in &hs.records {
        let list: Vec<String> =
            fields.iter().map(|f| format!("{}: {}", f.name, f.annotation.as_deref().unwrap_or("?"))).collect();
        notes.push_str(&format!("- Note: `{name}` is a dataclass/NamedTuple in {} ({}).", hs.file, list.join(", ")));
        if sig.records.iter().any(|r| &r.class == name) {
            notes.push_str(&format!(" As a parameter it arrives as the struct `port::{}` in port.h.", ident(name)));
        }
        if python::mentions(hs.returns.as_deref(), name) {
            notes.push_str(&format!(
                " The result must be a `{name}` too, so the signature returns a `fa::object`: build it with the CPython API (`PyImport_ImportModule(\"{}\")`, `PyObject_GetAttrString(module, \"{name}\")`, `PyObject_CallFunction(...)`) and return `fa::object::steal(result)`; verify checks the class and every field.",
                hs.module.as_deref().unwrap_or_default()
            ));
        }
        notes.push('\n');
    }
    if sig.params.iter().any(|p| p.ty.contains("fa::ndarray")) || sig.ret.contains("fa::array") {
        notes.push_str("- Note: NDArray parameters arrive as `fa::ndarray<const T>`, C-contiguous (`data()`, `size()`, `shape()`, `operator[]` by flat index); return arrays as `fa::array<T>{data, shape}` (a `std::vector<T>` and the shape). verify compares shape, dtype and values.\n");
    }
    if sig.keeps_gil {
        notes.push_str("- Note: Python objects cross this signature (`fa::object`), so the GIL stays held while the port runs: no threads that touch them.\n");
    }
    for (name, d) in &sig.late_defaults {
        notes.push_str(&format!(
            "- Note: `{name}` defaults to `{d}` in Python; the C++ signature gets `std::nullopt` for it, use that default then.\n"
        ));
    }
    let threads = if sig.keeps_gil {
        String::new()
    } else {
        format!(
            "- The GIL is released while `{}` runs: `std::thread` workers are fine if the result is the same.\n",
            hs.name
        )
    };
    format!(
        "# Port `{id}` to C++\n\n\
You're in a C++ port folder fast-anything generated for this: `{dir}`.\n\
Write `{func}` in `port.cpp` so it behaves **exactly** like the Python below, and runs fast.\n\
`port.h` (the signature) and `glue.cpp` (the CPython module, on top of `fa/py.hpp`) are generated: don't edit them.\n\
Only edit files in this folder. Don't touch the Python project.\n\n\
## The original ({file}:{line})\n\n{fence}python\n{source}\n{fence}\n\n\
{helpers}## Real calls\n\n\
fast-anything recorded {recorded} real calls (out of {seen} while the app ran). A few:\n\n{fence}\n{samples}\n{fence}\n\n{held}\
## Rules\n\n\
- Same result for every input, edge cases included: empty input, zero, negatives, NaN and infinity, non-ASCII text, very long input. verify tests these too.\n\
- Python `str` arrives as `std::u32string`, one `char32_t` per code point, so `size()` and indexing match Python. For Unicode-aware methods (`lower`, `upper`, `casefold`, `split`, `strip`, `isspace`, ...) take an ASCII fast path and call `fa::str_method<R>(s, \"lower\")` for the rest: it runs Python's own method.\n\
- Python ints don't overflow. `int64_t` is fine unless the recorded values are bigger; signed overflow is undefined behaviour in C++, so check before it can happen.\n\
- Floats must match within 1e-9 of their size (or of the largest value in the same list), and zeros keep their sign: -0.0 is not 0.0. Keep the same order of float operations: the build has `-ffp-contract=off` and no `-ffast-math`, and a vectorized float sum adds in a different order, so vectorize float reductions only when the original's order is kept.\n\
- Where Python raises, throw the matching `fa::value_error(\"...\")`, `fa::key_error`, `fa::index_error`, `fa::zero_division_error`, `fa::type_error` or `fa::overflow_error`, or `fa::error(\"SomeBuiltinError\", \"...\")`: the glue raises that Python exception.\n\
- Make it fast. port.cpp is compiled with `-O3 -march=native`: keep hot loops over contiguous data (`std::vector`, `fa::ndarray`), free of branches and calls so they vectorize, or use SIMD intrinsics (`<immintrin.h>` behind `#ifdef __AVX2__`, `<arm_neon.h>` behind `#ifdef __ARM_NEON`) with a plain loop for the rest. No needless allocation or copying: `reserve`, reuse buffers, fast paths for common cases.\n\
{threads}\
- Standard library only. Keep the function name and signature as port.h has them; helper functions and more `.cpp` files here are fine (every `.cpp` in this folder is compiled).\n\
{notes}\n\
## Check\n\n\
`c++ @compile_flags.txt -fsyntax-only port.cpp` compiles it here. `fast-anything verify {id} -C {root}` builds every `.cpp` here into a Python extension and replays all {total} recorded calls against it.\n",
        id = port.id,
        dir = dir.display(),
        root = root.display(),
        func = hs.name,
        file = hs.file,
        line = hs.line,
        source = hs.source,
        recorded = port.recorded.saturating_sub(held_out),
        held = crate::backend::held_out_note(held_out),
        total = port.recorded,
        seen = crate::util::thousands(port.calls_seen),
        helpers = helpers_md(hs, helpers),
    )
}

/// Write the port folder: the generated header, signature and glue (always), the stub (only when
/// there's no port.cpp yet) and TASK.md.
pub fn scaffold(ctx: &Ctx, hs: &Hotspot, helpers: &[Hotspot], port: &Port) -> Result<String> {
    let dir = ctx.crate_dir(port);
    fs::create_dir_all(dir.join("fa"))?;
    fs::write(dir.join("fa/py.hpp"), PY_HPP)?;
    fs::write(dir.join("port.h"), port_h(hs))?;
    fs::write(dir.join("glue.cpp"), glue_cpp(hs, port))?;
    let stub = dir.join("port.cpp");
    if !stub.exists() {
        fs::write(&stub, port_cpp(hs, port))?;
    }
    if let Ok(include) = python_include(ctx.runtime()?) {
        fs::write(dir.join("compile_flags.txt"), compile_flags_txt(&include))?;
    }
    let task = task_md(hs, helpers, port, &ctx.root, &dir, ctx.held_out(port));
    fs::write(dir.join("TASK.md"), &task)?;
    Ok(task)
}

// ------------------------------------------------------------------ build

/// The port's sources, sorted: every C/C++ source and header in its folder and in fa/.
pub fn source_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    for folder in [dir.to_path_buf(), dir.join("fa")] {
        let Ok(entries) = fs::read_dir(&folder) else { continue };
        for entry in entries {
            let path = entry?.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default();
            if path.is_file() && SOURCE_EXTS.contains(&ext) {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Compile every .cpp of the port into .fast/lib/{ext}.abi3.so. Ok(Err(log)) when it doesn't compile.
pub fn build(ctx: &Ctx, port: &Port) -> Result<std::result::Result<PathBuf, String>> {
    let python = ctx.runtime()?;
    let dir = ctx.crate_dir(port);
    ensure!(dir.join("port.cpp").exists(), "{} has no C++ port yet: run  fast-anything port {}", port.id, port.id);
    let include = python_include(python)?;
    let sources: Vec<PathBuf> = source_files(&dir)?
        .into_iter()
        .filter(|p| {
            p.parent() == Some(dir.as_path()) && p.extension().is_some_and(|e| e != "h" && e != "hh" && e != "hpp")
        })
        .collect();
    let (_, name) = port.lang.artifact(&port.ext);
    let target = ctx.fast.join("target").join("cpp");
    fs::create_dir_all(&target)?;
    let built = target.join(&name);
    let mut cmd = Command::new(compiler()?);
    cmd.args(flags()).arg("-shared").arg("-I").arg(&dir).arg("-I").arg(&include).args(&sources).arg("-o").arg(&built);
    if cfg!(target_os = "macos") {
        cmd.args(["-undefined", "dynamic_lookup"]);
    }
    cmd.current_dir(&dir);
    let out = output(&mut cmd, "the C++ compiler (install g++ or clang++; scan works without it)")?;
    if !out.status.success() {
        return Ok(Err(tail(&String::from_utf8_lossy(&out.stderr), 60)));
    }
    let lib_dir = ctx.lib_dir();
    fs::create_dir_all(&lib_dir)?;
    let dest = lib_dir.join(name);
    fs::copy(&built, &dest)
        .with_context(|| format!("copying {} into .fast/lib (is the app still running?)", built.display()))?;
    Ok(Ok(dest))
}

/// FNV-1a over the port's sources (see source_files), like backend::source_hash for crates.
pub fn source_hash(dir: &Path) -> Result<String> {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    let files = source_files(dir)?;
    ensure!(!files.is_empty(), "{} has no C++ sources", dir.display());
    for path in files {
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        feed(rel.as_bytes());
        feed(&[0]);
        feed(&fs::read(&path).with_context(|| format!("reading {}", path.display()))?);
        feed(&[0]);
    }
    Ok(format!("{h:016x}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{Backend, Lang};
    use crate::hotspot::Param;

    fn test_port(id: &str, ext: &str) -> Port {
        Port {
            id: id.into(),
            file: "fuzzy.py".into(),
            module: "fuzzy".into(),
            func: id.rsplit('.').next().unwrap_or(id).into(),
            ext: ext.into(),
            crate_dir: format!(".fast/ports/{id}"),
            recorded: 10,
            calls_seen: 10,
            verify: None,
            enabled: false,
            lang: Lang::Python,
            owner: None,
            line: 21,
            sources: vec![],
            backend: Backend::Cpp,
        }
    }

    fn param(name: &str, annotation: &str, default: Option<&str>) -> Param {
        Param {
            name: name.into(),
            annotation: Some(annotation.into()),
            default: default.map(Into::into),
            ..Param::default()
        }
    }

    fn fuzzy() -> Hotspot {
        Hotspot {
            id: "fuzzy.find_duplicates".into(),
            file: "fuzzy.py".into(),
            line: 21,
            name: "find_duplicates".into(),
            module: Some("fuzzy".into()),
            params: vec![param("names", "list[str]", None), param("max_distance", "int", Some("2"))],
            returns: Some("list[tuple[int, int]]".into()),
            ..Hotspot::default()
        }
    }

    #[test]
    fn annotations_map_to_cpp_types() {
        let none = |_: &str| None;
        let cases = [
            ("int", false, "int64_t"),
            ("float | None", false, "std::optional<double>"),
            ("Optional[str]", false, "std::optional<std::u32string>"),
            ("bytearray", false, "std::string"),
            ("list[tuple[int, float]]", false, "std::vector<std::tuple<int64_t, double>>"),
            ("tuple[int, ...]", false, "std::vector<int64_t>"),
            ("tuple[int, ...]", true, "fa::tuple_of<int64_t>"),
            ("dict[str, list[int]]", true, "fa::dict<std::u32string, std::vector<int64_t>>"),
            ("frozenset[int]", false, "fa::set<int64_t>"),
            ("NDArray[np.float64]", false, "fa::ndarray<const double>"),
            ("npt.NDArray[np.uint8]", true, "fa::array<uint8_t>"),
        ];
        for (annotation, returned, want) in cases {
            assert_eq!(cpp_type(annotation, returned, &none).as_deref(), Some(want), "{annotation}");
        }
        for unknown in ["Callable[[int], int]", "list[None]", "tuple[()]", "NDArray[np.complex128]", "Iterator[int]"] {
            assert_eq!(cpp_type(unknown, false, &none), None, "{unknown}");
        }
    }

    #[test]
    fn defaults_become_cpp_literals() {
        assert_eq!(cpp_default("2", "int64_t").as_deref(), Some("INT64_C(2)"));
        assert_eq!(cpp_default("-9223372036854775808", "int64_t").as_deref(), Some("INT64_MIN"));
        assert_eq!(cpp_default("1e-09", "double").as_deref(), Some("1e-09"));
        assert_eq!(cpp_default("3", "double").as_deref(), Some("3.0"));
        assert_eq!(cpp_default(".5", "double"), None);
        assert_eq!(cpp_default("True", "std::optional<bool>").as_deref(), Some("std::optional<bool>(true)"));
        assert_eq!(cpp_default("None", "std::optional<int64_t>").as_deref(), Some("std::nullopt"));
        assert_eq!(cpp_default("None", "int64_t"), None);
        assert_eq!(
            cpp_default("'é\"\\n'", "std::u32string").as_deref(),
            Some("std::u32string(U\"\\U000000E9\\U00000022\\U0000000A\", 3)")
        );
        assert_eq!(cpp_default("1_000", "int64_t"), None);
    }

    #[test]
    fn fuzzy_signature_and_glue() {
        let hs = fuzzy();
        let port = test_port("fuzzy.find_duplicates", "fa_fuzzy_find_duplicates");
        let h = port_h(&hs);
        assert!(h.contains(
            "std::vector<std::tuple<int64_t, int64_t>> find_duplicates(const std::vector<std::u32string>& names, int64_t max_distance);"
        ), "{h}");
        let glue = glue_cpp(&hs, &port);
        assert!(glue.contains("fa::py::args fa_args(fa_pos, fa_kw, {\"names\", \"max_distance\"}, 2);"), "{glue}");
        assert!(glue.contains("auto max_distance = fa_args.get_or<int64_t>(1, INT64_C(2));"), "{glue}");
        assert!(
            glue.contains("return fa::py::run(true, [&] { return ::port::find_duplicates(names, max_distance); });"),
            "{glue}"
        );
        assert!(glue.contains("PyMODINIT_FUNC PyInit_fa_fuzzy_find_duplicates(void)"), "{glue}");
        assert!(port_cpp(&hs, &port).contains("throw fa::not_implemented(\"port fuzzy.find_duplicates\");"));
    }

    #[test]
    fn keyword_only_records_objects_and_late_defaults() {
        let mut hs = fuzzy();
        hs.name = "score".into();
        hs.params = vec![
            param("points", "list[Point]", None),
            param("default", "float", Some("math.pi")),
            Param { kwonly: true, ..param("scale", "float", Some("1.5")) },
            Param { name: "extra".into(), kwonly: true, ..Param::default() },
        ];
        hs.returns = Some("Point".into());
        hs.records.insert("Point".into(), vec![param("x", "float", None), param("y", "float", None)]);
        let port = test_port("geo.score", "fa_geo_score");
        let h = port_h(&hs);
        assert!(h.contains("struct Point {\n    double x;\n    double y;\n};"), "{h}");
        assert!(h.contains(
            "fa::object score(const std::vector<port::Point>& points, std::optional<double> default_, double scale, const fa::object& extra);"
        ), "{h}");
        let glue = glue_cpp(&hs, &port);
        assert!(glue.contains("template <> struct fa::py::conv<port::Point>"), "{glue}");
        assert!(
            glue.contains("return port::Point{fa::py::attr<double>(o, \"x\"), fa::py::attr<double>(o, \"y\")};"),
            "{glue}"
        );
        assert!(glue.contains("{\"points\", \"default\", \"scale\", \"extra\"}, 2);"), "{glue}");
        assert!(glue.contains("auto default_ = fa_args.get_or<std::optional<double>>(1, std::nullopt);"), "{glue}");
        assert!(glue.contains("auto extra = fa_args.get<fa::object>(3);"), "{glue}");
        assert!(glue.contains("return fa::py::run(false, "), "objects keep the GIL: {glue}");
        let task = task_md(&hs, &[], &port, Path::new("/project"), Path::new("/project/.fast/ports/geo.score"), 0);
        assert!(task.contains("`default` defaults to `math.pi` in Python"), "{task}");
        assert!(task.contains("the GIL stays held"), "{task}");
        assert!(!task.contains("std::thread` workers are fine"), "{task}");
    }
}
