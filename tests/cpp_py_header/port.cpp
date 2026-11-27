// Test ports for tests/cpp_py_header.rs, written like an agent's port.cpp.
#include "port.h"

#include <cmath>
#include <new>
#include <stdexcept>
#include <thread>

namespace port {

int64_t echo_int(int64_t x) { return x; }
double echo_float(double x) { return x; }
bool echo_bool(bool b) { return b; }
std::u32string echo_str(const std::u32string& s) { return s; }
int64_t str_len(const std::u32string& s) { return static_cast<int64_t>(s.size()); }
std::string echo_bytes(const std::string& b) { return b; }
Nested echo_nested(const Nested& d) { return d; }

fa::dict<std::u32string, int64_t> ordered() {
    fa::dict<std::u32string, int64_t> d;
    d[U"zeta"] = 1;
    d[U"alpha"] = 2;
    d.insert_or_assign(U"mid", 3);
    d.insert_or_assign(U"alpha", 20);  // keeps its place
    d.erase(U"zeta");
    d[U"zeta"] = 4;  // now last
    if (!d.contains(U"mid") || *d.find(U"mid") != 3 || d.find(U"none") || d.size() != 3)
        throw std::logic_error("dict lookups");
    return d;
}

int64_t dict_at(const fa::dict<std::u32string, int64_t>& d, const std::u32string& key) { return d.at(key); }

std::optional<int64_t> maybe_double(std::optional<int64_t> x) {
    if (!x) return std::nullopt;
    return *x * 2;
}

std::optional<std::u32string> maybe_str(const std::optional<std::u32string>& s) { return s; }
std::vector<std::optional<int64_t>> echo_opt_list(const std::vector<std::optional<int64_t>>& xs) { return xs; }

int64_t sum_list(const std::vector<int64_t>& xs) {
    int64_t t = 0;
    for (int64_t x : xs) t += x;
    return t;
}

std::vector<double> echo_floats(const std::vector<double>& xs) { return xs; }

std::tuple<std::u32string, int64_t> swap(const std::tuple<int64_t, std::u32string>& p) {
    return {std::get<1>(p), std::get<0>(p)};
}

fa::tuple_of<int64_t> squares(int64_t n) {
    fa::tuple_of<int64_t> out;
    for (int64_t i = 0; i < n; ++i) out.push_back(i * i);
    return out;
}

fa::set<std::u32string> unique(const std::vector<std::u32string>& xs) { return {xs.begin(), xs.end()}; }
int64_t set_size(const fa::set<int64_t>& s) { return static_cast<int64_t>(s.size()); }

fa::set<fa::tuple_of<int64_t>> dedupe_tuples(const std::vector<std::vector<int64_t>>& xs) {
    fa::set<fa::tuple_of<int64_t>> out;
    for (const auto& x : xs) out.insert(fa::tuple_of<int64_t>(x));
    return out;
}

fa::dict<std::tuple<int64_t, int64_t>, int64_t> count_pairs(const std::vector<std::tuple<int64_t, int64_t>>& xs) {
    fa::dict<std::tuple<int64_t, int64_t>, int64_t> counts;
    for (const auto& x : xs) ++counts[x];
    return counts;
}

std::tuple<int64_t, double, bool, std::u32string, std::optional<int64_t>> kw(int64_t a, double b, bool c,
                                                                            const std::u32string& d,
                                                                            std::optional<int64_t> e) {
    return {a, b, c, d, e};
}

int64_t kwonly(int64_t k) { return k; }

std::tuple<int64_t, std::vector<int64_t>, double> nd_info(const fa::ndarray<const double>& a) {
    double sum = 0;
    for (double x : a) sum += x;
    return {static_cast<int64_t>(a.ndim()), a.shape(), sum};
}

int64_t nd_sum_i32(const fa::ndarray<const int32_t>& a) {
    int64_t sum = 0;
    for (size_t i = 0; i < a.size(); ++i) sum += a[i];
    return sum;
}

std::vector<double> nd_sums(const std::vector<fa::ndarray<const double>>& arrays) {
    std::vector<fa::ndarray<const double>> copies = arrays;  // copies share the buffers
    std::vector<double> out;
    for (const auto& a : copies) out.push_back(std::get<2>(nd_info(a)));
    return out;
}

fa::array<double> outer(const fa::ndarray<const double>& a, const fa::ndarray<const double>& b) {
    fa::array<double> out;
    out.shape = {static_cast<int64_t>(a.size()), static_cast<int64_t>(b.size())};
    out.data.reserve(a.size() * b.size());
    for (double x : a)
        for (double y : b) out.data.push_back(x * y);
    return out;
}

fa::array<bool> positive(const fa::ndarray<const double>& a) {
    fa::array<bool> out;
    for (double x : a) out.data.push_back(x > 0);
    return out;
}

fa::array<double> bad_shape() { return {{1, 2, 3}, {2, 2}}; }

double norm(const Point& p) { return std::sqrt(p.x * p.x + p.y * p.y); }

double total_norm(const std::vector<Point>& ps) {
    double t = 0;
    for (const auto& p : ps) t += norm(p);
    return t;
}

fa::object identity(const fa::object& o) { return o; }

int64_t py_len(const fa::object& o) {
    Py_ssize_t n = PyObject_Size(o.get());  // needs the GIL
    if (n < 0) throw fa::py::python_error();
    return n;
}

void fail(const std::u32string& kind) {
    if (kind == U"value") throw fa::value_error("bad value é");
    if (kind == U"type") throw fa::type_error("bad type");
    if (kind == U"key") throw fa::key_error("missing");
    if (kind == U"index") throw fa::index_error("out of range");
    if (kind == U"zero") throw fa::zero_division_error("division by zero");
    if (kind == U"overflow") throw fa::overflow_error("too big");
    if (kind == U"notimpl") throw fa::not_implemented("port t.fail");
    if (kind == U"stop") throw fa::error("StopIteration", "done");
    if (kind == U"unknown") throw fa::error("NoSuchError", "unknown name");
    if (kind == U"notclass") throw fa::error("len", "not an exception class");
    if (kind == U"runtime") throw std::runtime_error("boom");
    if (kind == U"alloc") throw std::bad_alloc();
    if (kind == U"int") throw 42;
}

int64_t parallel_sum(const std::vector<int64_t>& xs, int64_t threads) {
    size_t n = static_cast<size_t>(threads);
    std::vector<int64_t> partial(n, 0);
    std::vector<std::thread> pool;
    for (size_t t = 0; t < n; ++t)
        pool.emplace_back([&, t] {
            for (size_t i = t; i < xs.size(); i += n) partial[t] += xs[i];
        });
    for (auto& th : pool) th.join();
    return sum_list(partial);
}

static std::u32string lower(const std::u32string& w) {
    bool ascii = true;
    for (char32_t c : w) ascii &= c < 0x80;
    if (!ascii) return fa::str_method<std::u32string>(w, "lower");
    std::u32string out = w;
    for (char32_t& c : out)
        if (c >= U'A' && c <= U'Z') c += 32;
    return out;
}

std::vector<std::u32string> lower_all(const std::vector<std::u32string>& words, int64_t threads) {
    size_t n = static_cast<size_t>(threads);
    std::vector<std::u32string> out(words.size());
    std::vector<std::thread> pool;
    for (size_t t = 0; t < n; ++t)
        pool.emplace_back([&, t] {
            for (size_t i = t; i < words.size(); i += n) out[i] = lower(words[i]);
        });
    for (auto& th : pool) th.join();
    return out;
}

std::vector<std::u32string> split_words(const std::u32string& s, const std::optional<std::u32string>& sep,
                                        int64_t maxsplit) {
    using words = std::vector<std::u32string>;
    if (sep) return fa::str_method<words>(s, "split", *sep, maxsplit);
    return fa::str_method<words>(s, "split");
}

std::u32string strip_dots(const std::u32string& s) { return fa::str_method<std::u32string>(s, "strip", U"."); }

void noop(int64_t x) {}

}  // namespace port
