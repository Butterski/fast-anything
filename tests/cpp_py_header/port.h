// Test ports for tests/cpp_py_header.rs, shaped like a generated port.h.
#pragma once
#include "fa/py.hpp"

namespace port {
struct Point { double x; double y; };

using Nested = fa::dict<std::u32string, std::vector<std::tuple<int64_t, double>>>;

int64_t echo_int(int64_t x);
double echo_float(double x);
bool echo_bool(bool b);
std::u32string echo_str(const std::u32string& s);
int64_t str_len(const std::u32string& s);
std::string echo_bytes(const std::string& b);
Nested echo_nested(const Nested& d);
fa::dict<std::u32string, int64_t> ordered();
int64_t dict_at(const fa::dict<std::u32string, int64_t>& d, const std::u32string& key);
std::optional<int64_t> maybe_double(std::optional<int64_t> x);
std::optional<std::u32string> maybe_str(const std::optional<std::u32string>& s);
std::vector<std::optional<int64_t>> echo_opt_list(const std::vector<std::optional<int64_t>>& xs);
int64_t sum_list(const std::vector<int64_t>& xs);
std::vector<double> echo_floats(const std::vector<double>& xs);
std::tuple<std::u32string, int64_t> swap(const std::tuple<int64_t, std::u32string>& p);
fa::tuple_of<int64_t> squares(int64_t n);
fa::set<std::u32string> unique(const std::vector<std::u32string>& xs);
int64_t set_size(const fa::set<int64_t>& s);
fa::set<fa::tuple_of<int64_t>> dedupe_tuples(const std::vector<std::vector<int64_t>>& xs);
fa::dict<std::tuple<int64_t, int64_t>, int64_t> count_pairs(const std::vector<std::tuple<int64_t, int64_t>>& xs);
std::tuple<int64_t, double, bool, std::u32string, std::optional<int64_t>> kw(int64_t a, double b, bool c,
                                                                            const std::u32string& d,
                                                                            std::optional<int64_t> e);
int64_t kwonly(int64_t k);
std::tuple<int64_t, std::vector<int64_t>, double> nd_info(const fa::ndarray<const double>& a);
int64_t nd_sum_i32(const fa::ndarray<const int32_t>& a);
std::vector<double> nd_sums(const std::vector<fa::ndarray<const double>>& arrays);
fa::array<double> outer(const fa::ndarray<const double>& a, const fa::ndarray<const double>& b);
fa::array<bool> positive(const fa::ndarray<const double>& a);
fa::array<double> bad_shape();
double norm(const Point& p);
double total_norm(const std::vector<Point>& ps);
fa::object identity(const fa::object& o);
int64_t py_len(const fa::object& o);
void fail(const std::u32string& kind);
int64_t parallel_sum(const std::vector<int64_t>& xs, int64_t threads);
std::vector<std::u32string> lower_all(const std::vector<std::u32string>& words, int64_t threads);
std::vector<std::u32string> split_words(const std::u32string& s, const std::optional<std::u32string>& sep,
                                        int64_t maxsplit);
std::u32string strip_dots(const std::u32string& s);
void noop(int64_t x);
}  // namespace port
