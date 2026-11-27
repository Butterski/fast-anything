// CPython module fa_test_py, shaped like the glue fast-anything generates. Test fixture for
// tests/cpp_py_header.rs.
#include "port.h"

template <> struct fa::py::conv<port::Point> {
    static port::Point from(PyObject* o) { return port::Point{fa::py::attr<double>(o, "x"), fa::py::attr<double>(o, "y")}; }
};

static PyObject* fa_echo_int(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"x"}, 1);
        auto x = fa_args.get<int64_t>(0);
        return fa::py::run(true, [&] { return port::echo_int(x); });
    });
}

static PyObject* fa_echo_float(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"x"}, 1);
        auto x = fa_args.get<double>(0);
        return fa::py::run(true, [&] { return port::echo_float(x); });
    });
}

static PyObject* fa_echo_bool(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"b"}, 1);
        auto b = fa_args.get<bool>(0);
        return fa::py::run(true, [&] { return port::echo_bool(b); });
    });
}

static PyObject* fa_echo_str(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"s"}, 1);
        auto s = fa_args.get<std::u32string>(0);
        return fa::py::run(true, [&] { return port::echo_str(s); });
    });
}

static PyObject* fa_str_len(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"s"}, 1);
        auto s = fa_args.get<std::u32string>(0);
        return fa::py::run(true, [&] { return port::str_len(s); });
    });
}

static PyObject* fa_echo_bytes(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"b"}, 1);
        auto b = fa_args.get<std::string>(0);
        return fa::py::run(true, [&] { return port::echo_bytes(b); });
    });
}

static PyObject* fa_echo_nested(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"d"}, 1);
        auto d = fa_args.get<port::Nested>(0);
        return fa::py::run(true, [&] { return port::echo_nested(d); });
    });
}

static PyObject* fa_ordered(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {}, 0);
        return fa::py::run(true, [&] { return port::ordered(); });
    });
}

static PyObject* fa_dict_at(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"d", "key"}, 2);
        auto d = fa_args.get<fa::dict<std::u32string, int64_t>>(0);
        auto key = fa_args.get<std::u32string>(1);
        return fa::py::run(true, [&] { return port::dict_at(d, key); });
    });
}

static PyObject* fa_maybe_double(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"x"}, 1);
        auto x = fa_args.get_or<std::optional<int64_t>>(0, std::nullopt);
        return fa::py::run(true, [&] { return port::maybe_double(x); });
    });
}

static PyObject* fa_maybe_str(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"s"}, 1);
        auto s = fa_args.get<std::optional<std::u32string>>(0);
        return fa::py::run(true, [&] { return port::maybe_str(s); });
    });
}

static PyObject* fa_echo_opt_list(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"xs"}, 1);
        auto xs = fa_args.get<std::vector<std::optional<int64_t>>>(0);
        return fa::py::run(true, [&] { return port::echo_opt_list(xs); });
    });
}

static PyObject* fa_sum_list(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"xs"}, 1);
        auto xs = fa_args.get<std::vector<int64_t>>(0);
        return fa::py::run(true, [&] { return port::sum_list(xs); });
    });
}

static PyObject* fa_echo_floats(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"xs"}, 1);
        auto xs = fa_args.get<std::vector<double>>(0);
        return fa::py::run(true, [&] { return port::echo_floats(xs); });
    });
}

static PyObject* fa_swap(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"p"}, 1);
        auto p = fa_args.get<std::tuple<int64_t, std::u32string>>(0);
        return fa::py::run(true, [&] { return port::swap(p); });
    });
}

static PyObject* fa_squares(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"n"}, 1);
        auto n = fa_args.get<int64_t>(0);
        return fa::py::run(true, [&] { return port::squares(n); });
    });
}

static PyObject* fa_unique(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"xs"}, 1);
        auto xs = fa_args.get<std::vector<std::u32string>>(0);
        return fa::py::run(true, [&] { return port::unique(xs); });
    });
}

static PyObject* fa_set_size(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"s"}, 1);
        auto s = fa_args.get<fa::set<int64_t>>(0);
        return fa::py::run(true, [&] { return port::set_size(s); });
    });
}

static PyObject* fa_dedupe_tuples(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"xs"}, 1);
        auto xs = fa_args.get<std::vector<std::vector<int64_t>>>(0);
        return fa::py::run(true, [&] { return port::dedupe_tuples(xs); });
    });
}

static PyObject* fa_count_pairs(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"xs"}, 1);
        auto xs = fa_args.get<std::vector<std::tuple<int64_t, int64_t>>>(0);
        return fa::py::run(true, [&] { return port::count_pairs(xs); });
    });
}

static PyObject* fa_kw(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"a", "b", "c", "d", "e"}, 2);
        auto a = fa_args.get<int64_t>(0);
        auto b = fa_args.get_or<double>(1, 2.5);
        auto c = fa_args.get_or<bool>(2, true);
        auto d = fa_args.get_or<std::u32string>(3, U"\u00e9\u65e5");
        auto e = fa_args.get_or<std::optional<int64_t>>(4, std::nullopt);
        return fa::py::run(true, [&] { return port::kw(a, b, c, d, e); });
    });
}

static PyObject* fa_kwonly(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"k"}, 0);
        auto k = fa_args.get<int64_t>(0);
        return fa::py::run(true, [&] { return port::kwonly(k); });
    });
}

static PyObject* fa_nd_info(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"a"}, 1);
        auto a = fa_args.get<fa::ndarray<const double>>(0);
        return fa::py::run(true, [&] { return port::nd_info(a); });
    });
}

static PyObject* fa_nd_sum_i32(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"a"}, 1);
        auto a = fa_args.get<fa::ndarray<const int32_t>>(0);
        return fa::py::run(true, [&] { return port::nd_sum_i32(a); });
    });
}

static PyObject* fa_nd_sums(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"arrays"}, 1);
        auto arrays = fa_args.get<std::vector<fa::ndarray<const double>>>(0);
        return fa::py::run(true, [&] { return port::nd_sums(arrays); });
    });
}

static PyObject* fa_outer(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"a", "b"}, 2);
        auto a = fa_args.get<fa::ndarray<const double>>(0);
        auto b = fa_args.get<fa::ndarray<const double>>(1);
        return fa::py::run(true, [&] { return port::outer(a, b); });
    });
}

static PyObject* fa_positive(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"a"}, 1);
        auto a = fa_args.get<fa::ndarray<const double>>(0);
        return fa::py::run(true, [&] { return port::positive(a); });
    });
}

static PyObject* fa_bad_shape(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {}, 0);
        return fa::py::run(true, [&] { return port::bad_shape(); });
    });
}

static PyObject* fa_norm(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"p"}, 1);
        auto p = fa_args.get<port::Point>(0);
        return fa::py::run(true, [&] { return port::norm(p); });
    });
}

static PyObject* fa_total_norm(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"ps"}, 1);
        auto ps = fa_args.get<std::vector<port::Point>>(0);
        return fa::py::run(true, [&] { return port::total_norm(ps); });
    });
}

static PyObject* fa_identity(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"o"}, 1);
        auto o = fa_args.get<fa::object>(0);
        return fa::py::run(false, [&] { return port::identity(o); });
    });
}

static PyObject* fa_py_len(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"o"}, 1);
        auto o = fa_args.get<fa::object>(0);
        return fa::py::run(false, [&] { return port::py_len(o); });
    });
}

static PyObject* fa_fail(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"kind"}, 1);
        auto kind = fa_args.get<std::u32string>(0);
        return fa::py::run(true, [&] { return port::fail(kind); });
    });
}

static PyObject* fa_parallel_sum(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"xs", "threads"}, 2);
        auto xs = fa_args.get<std::vector<int64_t>>(0);
        auto threads = fa_args.get_or<int64_t>(1, INT64_C(4));
        return fa::py::run(true, [&] { return port::parallel_sum(xs, threads); });
    });
}

static PyObject* fa_lower_all(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"words", "threads"}, 2);
        auto words = fa_args.get<std::vector<std::u32string>>(0);
        auto threads = fa_args.get_or<int64_t>(1, INT64_C(4));
        return fa::py::run(true, [&] { return port::lower_all(words, threads); });
    });
}

static PyObject* fa_split_words(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"s", "sep", "maxsplit"}, 3);
        auto s = fa_args.get<std::u32string>(0);
        auto sep = fa_args.get_or<std::optional<std::u32string>>(1, std::nullopt);
        auto maxsplit = fa_args.get_or<int64_t>(2, INT64_C(-1));
        return fa::py::run(true, [&] { return port::split_words(s, sep, maxsplit); });
    });
}

static PyObject* fa_strip_dots(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"s"}, 1);
        auto s = fa_args.get<std::u32string>(0);
        return fa::py::run(true, [&] { return port::strip_dots(s); });
    });
}

static PyObject* fa_noop(PyObject* fa_self, PyObject* fa_pos, PyObject* fa_kw) {
    return fa::py::guard([&]() -> PyObject* {
        fa::py::args fa_args(fa_pos, fa_kw, {"x"}, 1);
        auto x = fa_args.get<int64_t>(0);
        return fa::py::run(true, [&] { return port::noop(x); });
    });
}

static PyMethodDef fa_methods[] = {
    {"echo_int", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_int)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"echo_float", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_float)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"echo_bool", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_bool)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"echo_str", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_str)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"str_len", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_str_len)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"echo_bytes", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_bytes)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"echo_nested", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_nested)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"ordered", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_ordered)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"dict_at", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_dict_at)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"maybe_double", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_maybe_double)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"maybe_str", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_maybe_str)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"echo_opt_list", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_opt_list)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"sum_list", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_sum_list)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"echo_floats", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_echo_floats)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"swap", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_swap)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"squares", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_squares)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"unique", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_unique)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"set_size", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_set_size)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"dedupe_tuples", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_dedupe_tuples)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"count_pairs", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_count_pairs)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"kw", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_kw)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"kwonly", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_kwonly)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"nd_info", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_nd_info)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"nd_sum_i32", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_nd_sum_i32)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"nd_sums", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_nd_sums)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"outer", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_outer)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"positive", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_positive)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"bad_shape", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_bad_shape)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"norm", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_norm)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"total_norm", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_total_norm)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"identity", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_identity)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"py_len", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_py_len)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"fail", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_fail)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"parallel_sum", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_parallel_sum)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"lower_all", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_lower_all)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"split_words", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_split_words)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"strip_dots", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_strip_dots)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {"noop", reinterpret_cast<PyCFunction>(reinterpret_cast<void (*)(void)>(fa_noop)), METH_VARARGS | METH_KEYWORDS, nullptr},
    {nullptr, nullptr, 0, nullptr},
};

static PyModuleDef fa_module = {PyModuleDef_HEAD_INIT, "fa_test_py", nullptr, -1, fa_methods, nullptr, nullptr, nullptr, nullptr};

PyMODINIT_FUNC PyInit_fa_test_py(void) { return PyModule_Create(&fa_module); }
