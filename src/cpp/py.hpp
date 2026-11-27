// fast-anything: C++ ports of Python functions. Value types, conversions, argument parsing and
// exception mapping for the generated CPython glue. Written by fast-anything; don't edit.
#pragma once

#ifndef Py_LIMITED_API
#define Py_LIMITED_API 0x030B0000
#endif
#ifndef PY_SSIZE_T_CLEAN
#define PY_SSIZE_T_CLEAN
#endif
#include <Python.h>

#include <bit>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <exception>
#include <functional>
#include <initializer_list>
#include <limits>
#include <memory>
#include <new>
#include <optional>
#include <stdexcept>
#include <string>
#include <string_view>
#include <tuple>
#include <type_traits>
#include <unordered_map>
#include <unordered_set>
#include <utility>
#include <vector>

namespace fa::py {
// A CPython call failed and left its exception set; guard() raises that exception unchanged.
struct python_error : std::exception {
    const char* what() const noexcept override { return "a Python call failed"; }
};
}  // namespace fa::py

namespace fa {

// ---------------------------------------------------------------------------------------------
// Exceptions: the glue raises the Python builtin exception named by type().

struct error : std::exception {
    error(std::string type, std::string message) : type_(std::move(type)), message_(std::move(message)) {}
    const std::string& type() const noexcept { return type_; }
    const char* what() const noexcept override { return message_.c_str(); }

private:
    std::string type_;
    std::string message_;
};

struct value_error : error {
    explicit value_error(std::string message) : error("ValueError", std::move(message)) {}
};
struct type_error : error {
    explicit type_error(std::string message) : error("TypeError", std::move(message)) {}
};
struct key_error : error {
    explicit key_error(std::string message) : error("KeyError", std::move(message)) {}
};
struct index_error : error {
    explicit index_error(std::string message) : error("IndexError", std::move(message)) {}
};
struct zero_division_error : error {
    explicit zero_division_error(std::string message) : error("ZeroDivisionError", std::move(message)) {}
};
struct overflow_error : error {
    explicit overflow_error(std::string message) : error("OverflowError", std::move(message)) {}
};
struct not_implemented : error {
    explicit not_implemented(std::string message) : error("NotImplementedError", std::move(message)) {}
};

// ---------------------------------------------------------------------------------------------
// Value types

// tuple[X, ...] as a result: a vector that converts to a Python tuple.
template <class T>
struct tuple_of : std::vector<T> {
    using std::vector<T>::vector;
    tuple_of() = default;
    tuple_of(std::vector<T> v) : std::vector<T>(std::move(v)) {}
};

namespace detail {
inline size_t hash_mix(size_t seed, size_t h) noexcept {
    return seed ^ (h + static_cast<size_t>(0x9e3779b97f4a7c15ULL) + (seed << 6) + (seed >> 2));
}
template <class>
inline constexpr bool always_false = false;
}  // namespace detail

template <class T>
struct hash {
    size_t operator()(const T& v) const { return std::hash<T>{}(v); }
};
template <class T>
struct hash<std::vector<T>> {
    size_t operator()(const std::vector<T>& v) const {
        size_t seed = v.size();
        for (const auto& x : v) seed = detail::hash_mix(seed, hash<T>{}(x));
        return seed;
    }
};
template <class T>
struct hash<tuple_of<T>> : hash<std::vector<T>> {};
template <class T>
struct hash<std::optional<T>> {
    size_t operator()(const std::optional<T>& v) const {
        return v ? detail::hash_mix(1, hash<T>{}(*v)) : static_cast<size_t>(0x6a09e667f3bcc908ULL);
    }
};
template <class... Ts>
struct hash<std::tuple<Ts...>> {
    size_t operator()(const std::tuple<Ts...>& t) const {
        return std::apply(
            [](const auto&... x) {
                size_t seed = sizeof...(Ts);
                ((seed = detail::hash_mix(seed, hash<std::remove_cvref_t<decltype(x)>>{}(x))), ...);
                return seed;
            },
            t);
    }
};

// Insertion-ordered like Python's dict. Iteration yields std::pair<K, V>& in insertion order.
template <class K, class V>
class dict {
public:
    using value_type = std::pair<K, V>;
    using iterator = typename std::vector<value_type>::iterator;
    using const_iterator = typename std::vector<value_type>::const_iterator;

    dict() = default;
    dict(std::initializer_list<value_type> init) {
        reserve(init.size());
        for (const auto& kv : init) insert_or_assign(kv.first, kv.second);
    }

    size_t size() const noexcept { return items_.size(); }
    bool empty() const noexcept { return items_.empty(); }
    void reserve(size_t n) {
        items_.reserve(n);
        index_.reserve(n);
    }
    void clear() noexcept {
        items_.clear();
        index_.clear();
    }

    iterator begin() noexcept { return items_.begin(); }
    iterator end() noexcept { return items_.end(); }
    const_iterator begin() const noexcept { return items_.begin(); }
    const_iterator end() const noexcept { return items_.end(); }

    bool contains(const K& k) const { return index_.find(k) != index_.end(); }
    V* find(const K& k) {
        auto it = index_.find(k);
        return it == index_.end() ? nullptr : &items_[it->second].second;
    }
    const V* find(const K& k) const {
        auto it = index_.find(k);
        return it == index_.end() ? nullptr : &items_[it->second].second;
    }
    V& at(const K& k) {
        if (V* v = find(k)) return *v;
        throw key_error("key not found");
    }
    const V& at(const K& k) const {
        if (const V* v = find(k)) return *v;
        throw key_error("key not found");
    }
    V& operator[](const K& k) {
        if (V* v = find(k)) return *v;
        items_.emplace_back(k, V{});
        add_last_to_index();
        return items_.back().second;
    }
    // An existing key keeps its position, like Python's d[k] = v.
    void insert_or_assign(K k, V v) {
        if (V* old = find(k)) {
            *old = std::move(v);
            return;
        }
        items_.emplace_back(std::move(k), std::move(v));
        add_last_to_index();
    }
    // The other items keep their order. O(n) unless k is the last item.
    bool erase(const K& k) {
        auto it = index_.find(k);
        if (it == index_.end()) return false;
        size_t pos = it->second;
        index_.erase(it);
        items_.erase(items_.begin() + static_cast<std::ptrdiff_t>(pos));
        if (pos != items_.size())
            for (auto& entry : index_)
                if (entry.second > pos) --entry.second;
        return true;
    }

    // Equal like Python dicts: same keys and values, order ignored.
    friend bool operator==(const dict& a, const dict& b) {
        if (a.size() != b.size()) return false;
        for (const auto& [k, v] : a) {
            const V* other = b.find(k);
            if (!other || !(*other == v)) return false;
        }
        return true;
    }

private:
    void add_last_to_index() {
        try {
            index_.emplace(items_.back().first, items_.size() - 1);
        } catch (...) {
            items_.pop_back();
            throw;
        }
    }

    std::vector<value_type> items_;
    std::unordered_map<K, size_t, hash<K>> index_;
};

template <class T>
using set = std::unordered_set<T, hash<T>>;

// A Python object (strong reference). Ports taking or returning one keep the GIL.
class object {
public:
    object() = default;
    static object borrow(PyObject* p) noexcept {
        Py_XINCREF(p);
        return object(p);
    }
    static object steal(PyObject* p) noexcept { return object(p); }
    object(const object& o) noexcept : p_(o.p_) { Py_XINCREF(p_); }
    object(object&& o) noexcept : p_(std::exchange(o.p_, nullptr)) {}
    object& operator=(const object& o) noexcept {
        Py_XINCREF(o.p_);
        Py_XDECREF(std::exchange(p_, o.p_));
        return *this;
    }
    object& operator=(object&& o) noexcept {
        if (this != &o) Py_XDECREF(std::exchange(p_, std::exchange(o.p_, nullptr)));
        return *this;
    }
    ~object() { Py_XDECREF(p_); }

    PyObject* get() const noexcept { return p_; }
    // Gives up the reference (for returning it to Python).
    PyObject* release() noexcept { return std::exchange(p_, nullptr); }
    explicit operator bool() const noexcept { return p_ != nullptr; }

private:
    explicit object(PyObject* p) noexcept : p_(p) {}
    PyObject* p_ = nullptr;
};

namespace detail {
// A Py_buffer (and the contiguous copy it may view), released with the GIL taken, so the last
// ndarray copy may die anywhere.
struct buffer_owner {
    Py_buffer view{};
    bool filled = false;
    PyObject* copy = nullptr;
    std::vector<int64_t> shape;

    buffer_owner() = default;
    buffer_owner(const buffer_owner&) = delete;
    buffer_owner& operator=(const buffer_owner&) = delete;
    ~buffer_owner() {
        if (!filled && !copy) return;
        PyGILState_STATE gil = PyGILState_Ensure();
        if (filled) PyBuffer_Release(&view);
        Py_XDECREF(copy);
        PyGILState_Release(gil);
    }
};

template <class E>
constexpr const char* dtype_name() noexcept {
    if constexpr (std::is_same_v<E, bool>) return "bool";
    else if constexpr (std::is_same_v<E, float>) return "float32";
    else if constexpr (std::is_same_v<E, double>) return "float64";
    else if constexpr (std::is_integral_v<E> && std::is_signed_v<E>) {
        if constexpr (sizeof(E) == 1) return "int8";
        else if constexpr (sizeof(E) == 2) return "int16";
        else if constexpr (sizeof(E) == 4) return "int32";
        else return "int64";
    } else if constexpr (std::is_integral_v<E>) {
        if constexpr (sizeof(E) == 1) return "uint8";
        else if constexpr (sizeof(E) == 2) return "uint16";
        else if constexpr (sizeof(E) == 4) return "uint32";
        else return "uint64";
    } else {
        static_assert(always_false<E>, "fa::ndarray / fa::array: unsupported element type");
        return "";
    }
}

// Does a buffer of struct-module format `fmt` and item size `itemsize` hold E values?
template <class E>
bool format_matches(const char* fmt, Py_ssize_t itemsize) noexcept {
    if (itemsize != static_cast<Py_ssize_t>(sizeof(E))) return false;
    if (!fmt) fmt = "B";
    switch (*fmt) {
        case '@':
        case '=': ++fmt; break;
        case '<':
            if (std::endian::native != std::endian::little) return false;
            ++fmt;
            break;
        case '>':
        case '!':
            if (std::endian::native != std::endian::big) return false;
            ++fmt;
            break;
        default: break;
    }
    if (!fmt[0] || fmt[1]) return false;
    char c = fmt[0];
    if constexpr (std::is_same_v<E, bool>) return c == '?';
    else if constexpr (std::is_floating_point_v<E>) return c == (sizeof(E) == 4 ? 'f' : 'd');
    else if constexpr (std::is_signed_v<E>) return std::strchr("bhilqn", c) != nullptr;
    else return std::strchr("BHILQN", c) != nullptr;
}
}  // namespace detail

// NumPy input: C-contiguous read-only view (non-contiguous input is copied first). Copies share
// the buffer.
template <class T>
class ndarray {
    static_assert(std::is_const_v<T>, "fa::ndarray is a read-only view: use fa::ndarray<const double>");

public:
    using element_type = T;
    ndarray() = default;
    ndarray(T* data, size_t size, std::shared_ptr<const detail::buffer_owner> owner) noexcept
        : data_(data), size_(size), owner_(std::move(owner)) {}

    T* data() const noexcept { return data_; }
    size_t size() const noexcept { return size_; }
    bool empty() const noexcept { return size_ == 0; }
    size_t ndim() const { return shape().size(); }
    const std::vector<int64_t>& shape() const {
        static const std::vector<int64_t> none{0};
        return owner_ ? owner_->shape : none;
    }
    T& operator[](size_t i) const noexcept { return data_[i]; }
    T* begin() const noexcept { return data_; }
    T* end() const noexcept { return data_ + size_; }

private:
    T* data_ = nullptr;
    size_t size_ = 0;
    std::shared_ptr<const detail::buffer_owner> owner_;
};

// NumPy output: owning; returned as a writable numpy array of that dtype and shape.
template <class T>
struct array {
    std::vector<T> data;
    std::vector<int64_t> shape;  // {} means 1-D of data.size()
};

// Call a str method with Python's own semantics (Unicode case mapping, whitespace split, ...).
// Takes the GIL itself, so it works inside the GIL-free part of a port. Slow: use an ASCII
// fast path and this for the rest. A Python exception becomes fa::error of the same name.
template <class R, class... A>
R str_method(const std::u32string& self, const char* name, const A&... args);

}  // namespace fa

namespace fa::detail {

// UTF-8 text of a str object; "" when it has none (lone surrogates).
inline std::string utf8(PyObject* s) {
    Py_ssize_t len = 0;
    const char* p = s ? PyUnicode_AsUTF8AndSize(s, &len) : nullptr;
    if (!p) {
        PyErr_Clear();
        return {};
    }
    return std::string(p, static_cast<size_t>(len));
}

inline std::string type_name(PyObject* o) {
    std::string name = utf8(object::steal(PyType_GetName(Py_TYPE(o))).get());
    PyErr_Clear();
    return name.empty() ? std::string("object") : name;
}

inline type_error expected(const char* what, PyObject* o) {
    return type_error(std::string("expected ") + what + ", got " + type_name(o));
}

inline PyObject* str_from(std::u32string_view s) {
    static_assert(sizeof(char32_t) == sizeof(Py_UCS4));
    int byteorder = std::endian::native == std::endian::little ? -1 : 1;
    PyObject* r = PyUnicode_DecodeUTF32(reinterpret_cast<const char*>(s.data()),
                                        static_cast<Py_ssize_t>(s.size() * sizeof(char32_t)), "surrogatepass",
                                        &byteorder);
    if (!r) throw py::python_error();
    return r;
}

inline object import(const char* module) {
    object m = object::steal(PyImport_ImportModule(module));
    if (!m) throw py::python_error();
    return m;
}

// Calls f(item) for each item of a list, tuple or other iterable (not a str).
template <class F>
void for_each_item(PyObject* o, const char* what, F&& f) {
    if (PyList_Check(o)) {
        for (Py_ssize_t i = 0; i < PyList_Size(o); ++i) {
            object item = object::borrow(PyList_GetItem(o, i));
            f(item.get());
        }
        return;
    }
    if (PyTuple_Check(o)) {
        Py_ssize_t n = PyTuple_Size(o);
        for (Py_ssize_t i = 0; i < n; ++i) f(PyTuple_GetItem(o, i));
        return;
    }
    if (PyUnicode_Check(o)) throw expected(what, o);
    object it = object::steal(PyObject_GetIter(o));
    if (!it) {
        PyErr_Clear();
        throw expected(what, o);
    }
    while (PyObject* raw = PyIter_Next(it.get())) {
        object item = object::steal(raw);
        f(item.get());
    }
    if (PyErr_Occurred()) throw py::python_error();
}

// Takes the current Python exception and throws it as fa::error(its class name, str(it)).
[[noreturn]] inline void throw_as_error() {
#if (defined(Py_LIMITED_API) && Py_LIMITED_API + 0 >= 0x030C0000) || \
    (!defined(Py_LIMITED_API) && PY_VERSION_HEX >= 0x030C0000)
    object exc = object::steal(PyErr_GetRaisedException());
#else
    PyObject *type = nullptr, *value = nullptr, *traceback = nullptr;
    PyErr_Fetch(&type, &value, &traceback);
    PyErr_NormalizeException(&type, &value, &traceback);
    Py_XDECREF(type);
    Py_XDECREF(traceback);
    object exc = object::steal(value);
#endif
    if (!exc) throw error("RuntimeError", "unknown Python error");
    std::string name = type_name(exc.get());
    std::string message = utf8(object::steal(PyObject_Str(exc.get())).get());
    PyErr_Clear();
    throw error(std::move(name), std::move(message));
}

// Raises builtins.<type>(message), or RuntimeError when builtins has no such exception class.
inline void raise(const char* type, const char* message) noexcept {
    PyObject* cls = PyExc_RuntimeError;
    object builtins = object::steal(PyImport_ImportModule("builtins"));
    object found = builtins ? object::steal(PyObject_GetAttrString(builtins.get(), type)) : object();
    if (found && PyExceptionClass_Check(found.get())) cls = found.get();
    PyErr_Clear();
    object text = object::steal(
        PyUnicode_DecodeUTF8(message, static_cast<Py_ssize_t>(std::strlen(message)), "replace"));
    if (text) PyErr_SetObject(cls, text.get());
}

// Releases the GIL for its lifetime (when asked to) and takes it back at relock() or on unwind.
class unlocked {
public:
    explicit unlocked(bool release) noexcept : state_(release ? PyEval_SaveThread() : nullptr) {}
    unlocked(const unlocked&) = delete;
    unlocked& operator=(const unlocked&) = delete;
    void relock() noexcept {
        if (state_) PyEval_RestoreThread(std::exchange(state_, nullptr));
    }
    ~unlocked() { relock(); }

private:
    PyThreadState* state_;
};
}  // namespace fa::detail

namespace fa::py {

// ---------------------------------------------------------------------------------------------
// Conversions. from(): GIL held, borrowed reference in, throws fa::type_error (wrong type) /
// fa::overflow_error (int out of range) / python_error. to(): GIL held, returns a new reference.
// Specialise for generated structs (from() only).

template <class T, class Enable = void>
struct conv;

template <class T>
struct conv<T, std::enable_if_t<std::is_integral_v<T> && !std::is_same_v<T, bool>>> {
    static T from(PyObject* o) {
        object index;
        if (!PyLong_Check(o)) {  // bool is an int, like in Python; numpy integers have __index__
            if (!PyIndex_Check(o)) throw detail::expected("int", o);
            index = object::steal(PyNumber_Index(o));
            if (!index) throw python_error();
            o = index.get();
        }
        int overflow = 0;
        long long v = PyLong_AsLongLongAndOverflow(o, &overflow);
        if (v == -1 && !overflow && PyErr_Occurred()) throw python_error();
        if constexpr (std::is_signed_v<T>) {
            if constexpr (sizeof(T) < sizeof(long long)) {
                if (v < static_cast<long long>(std::numeric_limits<T>::min()) ||
                    v > static_cast<long long>(std::numeric_limits<T>::max()))
                    overflow = 1;
            }
            if (overflow) throw too_large();
            return static_cast<T>(v);
        } else {
            if (overflow < 0 || (!overflow && v < 0)) throw overflow_error("can't convert negative int to unsigned");
            unsigned long long u = static_cast<unsigned long long>(v);
            if (overflow) {
                u = PyLong_AsUnsignedLongLong(o);
                if (u == static_cast<unsigned long long>(-1) && PyErr_Occurred()) {
                    PyErr_Clear();
                    throw too_large();
                }
            }
            if constexpr (sizeof(T) < sizeof(unsigned long long)) {
                if (u > static_cast<unsigned long long>(std::numeric_limits<T>::max())) throw too_large();
            }
            return static_cast<T>(u);
        }
    }
    static PyObject* to(T v) {
        PyObject* r;
        if constexpr (std::is_signed_v<T>) r = PyLong_FromLongLong(static_cast<long long>(v));
        else r = PyLong_FromUnsignedLongLong(static_cast<unsigned long long>(v));
        if (!r) throw python_error();
        return r;
    }

private:
    static overflow_error too_large() {
        return overflow_error(std::string("int too large to convert to ") + detail::dtype_name<T>());
    }
};

template <class T>
struct conv<T, std::enable_if_t<std::is_floating_point_v<T>>> {
    static T from(PyObject* o) {
        if (!PyFloat_Check(o) && !PyLong_Check(o) && !PyIndex_Check(o)) throw detail::expected("float", o);
        double v = PyFloat_AsDouble(o);
        if (v == -1.0 && PyErr_Occurred()) throw python_error();
        return static_cast<T>(v);
    }
    static PyObject* to(T v) {
        PyObject* r = PyFloat_FromDouble(static_cast<double>(v));
        if (!r) throw python_error();
        return r;
    }
};

template <>
struct conv<bool> {
    static bool from(PyObject* o) {
        if (o == Py_True) return true;
        if (o == Py_False) return false;
        throw detail::expected("bool", o);
    }
    static PyObject* to(bool v) { return PyBool_FromLong(v ? 1 : 0); }
};

template <>
struct conv<std::u32string> {
    static std::u32string from(PyObject* o) {
        if (!PyUnicode_Check(o)) throw detail::expected("str", o);
        Py_ssize_t n = PyUnicode_GetLength(o);
        if (n < 0) throw python_error();
        std::u32string s(static_cast<size_t>(n), U'\0');
        if (n && !PyUnicode_AsUCS4(o, reinterpret_cast<Py_UCS4*>(s.data()), n, 0)) throw python_error();
        return s;
    }
    static PyObject* to(const std::u32string& s) { return detail::str_from(s); }
};

// bytes and bytearray in, bytes out.
template <>
struct conv<std::string> {
    static std::string from(PyObject* o) {
        if (PyBytes_Check(o)) {
            char* p = nullptr;
            Py_ssize_t n = 0;
            if (PyBytes_AsStringAndSize(o, &p, &n) < 0) throw python_error();
            return std::string(p, static_cast<size_t>(n));
        }
        if (PyByteArray_Check(o))
            return std::string(PyByteArray_AsString(o), static_cast<size_t>(PyByteArray_Size(o)));
        throw detail::expected("bytes", o);
    }
    static PyObject* to(const std::string& s) {
        PyObject* r = PyBytes_FromStringAndSize(s.data(), static_cast<Py_ssize_t>(s.size()));
        if (!r) throw python_error();
        return r;
    }
};

template <>
struct conv<object> {
    static object from(PyObject* o) { return object::borrow(o); }
    static PyObject* to(const object& o) { return Py_NewRef(o ? o.get() : Py_None); }
};

template <class T>
struct conv<std::optional<T>> {
    static std::optional<T> from(PyObject* o) {
        if (o == Py_None) return std::nullopt;
        return conv<T>::from(o);
    }
    static PyObject* to(const std::optional<T>& v) { return v ? conv<T>::to(*v) : Py_NewRef(Py_None); }
};

// list, tuple or any iterable (not a str) in; list out.
template <class T>
struct conv<std::vector<T>> {
    static std::vector<T> from(PyObject* o) {
        std::vector<T> v;
        if (PyList_Check(o) || PyTuple_Check(o)) v.reserve(static_cast<size_t>(PySequence_Size(o)));
        detail::for_each_item(o, "list", [&](PyObject* item) { v.push_back(conv<T>::from(item)); });
        return v;
    }
    static PyObject* to(const std::vector<T>& v) {
        object list = object::steal(PyList_New(static_cast<Py_ssize_t>(v.size())));
        if (!list) throw python_error();
        Py_ssize_t i = 0;
        for (const auto& x : v) PyList_SetItem(list.get(), i++, conv<T>::to(x));
        return list.release();
    }
};

// Like a vector in; tuple out.
template <class T>
struct conv<tuple_of<T>> {
    static tuple_of<T> from(PyObject* o) { return tuple_of<T>(conv<std::vector<T>>::from(o)); }
    static PyObject* to(const tuple_of<T>& v) {
        object tuple = object::steal(PyTuple_New(static_cast<Py_ssize_t>(v.size())));
        if (!tuple) throw python_error();
        Py_ssize_t i = 0;
        for (const auto& x : v) PyTuple_SetItem(tuple.get(), i++, conv<T>::to(x));
        return tuple.release();
    }
};

// A tuple (or list) of exactly that length in; tuple out.
template <class... Ts>
struct conv<std::tuple<Ts...>> {
    static std::tuple<Ts...> from(PyObject* o) {
        object tuple;
        if (PyTuple_Check(o)) tuple = object::borrow(o);
        else if (PyList_Check(o)) {
            tuple = object::steal(PySequence_Tuple(o));
            if (!tuple) throw python_error();
        } else throw detail::expected("tuple", o);
        Py_ssize_t n = PyTuple_Size(tuple.get());
        if (n != static_cast<Py_ssize_t>(sizeof...(Ts)))
            throw type_error("expected a tuple of length " + std::to_string(sizeof...(Ts)) + ", got length " +
                             std::to_string(n));
        return [&]<size_t... I>(std::index_sequence<I...>) {
            return std::tuple<Ts...>{conv<Ts>::from(PyTuple_GetItem(tuple.get(), I))...};
        }(std::index_sequence_for<Ts...>{});
    }
    static PyObject* to(const std::tuple<Ts...>& v) {
        object tuple = object::steal(PyTuple_New(sizeof...(Ts)));
        if (!tuple) throw python_error();
        [&]<size_t... I>(std::index_sequence<I...>) {
            ((void)PyTuple_SetItem(tuple.get(), I, conv<Ts>::to(std::get<I>(v))), ...);
        }(std::index_sequence_for<Ts...>{});
        return tuple.release();
    }
};

// A dict or any mapping with items() in; dict (same order) out.
template <class K, class V>
struct conv<dict<K, V>> {
    static dict<K, V> from(PyObject* o) {
        dict<K, V> d;
        if (PyDict_Check(o)) {
            d.reserve(static_cast<size_t>(PyDict_Size(o)));
            Py_ssize_t pos = 0;
            PyObject *key, *value;
            while (PyDict_Next(o, &pos, &key, &value)) {
                object k = object::borrow(key), v = object::borrow(value);
                d.insert_or_assign(conv<K>::from(k.get()), conv<V>::from(v.get()));
            }
            return d;
        }
        object items = object::steal(PyUnicode_Check(o) ? nullptr : PyMapping_Items(o));
        if (!items) {
            PyErr_Clear();
            throw detail::expected("dict", o);
        }
        detail::for_each_item(items.get(), "dict", [&](PyObject* kv) {
            if (!PyTuple_Check(kv) || PyTuple_Size(kv) != 2) throw detail::expected("dict", o);
            d.insert_or_assign(conv<K>::from(PyTuple_GetItem(kv, 0)), conv<V>::from(PyTuple_GetItem(kv, 1)));
        });
        return d;
    }
    static PyObject* to(const dict<K, V>& d) {
        object out = object::steal(PyDict_New());
        if (!out) throw python_error();
        for (const auto& [k, v] : d) {
            object key = object::steal(conv<K>::to(k));
            object value = object::steal(conv<V>::to(v));
            if (PyDict_SetItem(out.get(), key.get(), value.get()) < 0) throw python_error();
        }
        return out.release();
    }
};

// set, frozenset or any iterable (not a str) in; set out.
template <class T, class H, class E, class A>
struct conv<std::unordered_set<T, H, E, A>> {
    using set_type = std::unordered_set<T, H, E, A>;
    static set_type from(PyObject* o) {
        set_type s;
        if (PyAnySet_Check(o)) s.reserve(static_cast<size_t>(PySet_Size(o)));
        detail::for_each_item(o, "set", [&](PyObject* item) { s.insert(conv<T>::from(item)); });
        return s;
    }
    static PyObject* to(const set_type& s) {
        object out = object::steal(PySet_New(nullptr));
        if (!out) throw python_error();
        for (const auto& x : s) {
            object item = object::steal(conv<T>::to(x));
            if (PySet_Add(out.get(), item.get()) < 0) throw python_error();
        }
        return out.release();
    }
};

// Anything with the buffer protocol whose format matches T; other objects (lists, non-contiguous
// arrays) go through numpy.ascontiguousarray first.
template <class T>
struct conv<ndarray<T>> {
    using E = std::remove_const_t<T>;
    static ndarray<T> from(PyObject* o) {
        constexpr int flags = PyBUF_C_CONTIGUOUS | PyBUF_FORMAT;
        auto owner = std::make_shared<detail::buffer_owner>();
        if (PyObject_GetBuffer(o, &owner->view, flags) == 0) owner->filled = true;
        else {
            PyErr_Clear();
            object numpy = object::steal(PyImport_ImportModule("numpy"));
            if (numpy) owner->copy = PyObject_CallMethod(numpy.get(), "ascontiguousarray", "O", o);
            if (owner->copy && PyObject_GetBuffer(owner->copy, &owner->view, flags) == 0) owner->filled = true;
            else {
                PyErr_Clear();
                throw detail::expected(array_of(), o);
            }
        }
        const Py_buffer& view = owner->view;
        if (!detail::format_matches<E>(view.format, view.itemsize))
            throw type_error(std::string("expected ") + array_of() + ", got elements of format '" +
                             (view.format ? view.format : "B") + "'");
        owner->shape.assign(view.shape, view.shape + view.ndim);
        size_t size = view.itemsize ? static_cast<size_t>(view.len / view.itemsize) : 0;
        T* data = static_cast<T*>(view.buf);
        return ndarray<T>(data, size, std::move(owner));
    }

private:
    static const char* array_of() {
        static const std::string text = std::string("a numpy array of ") + detail::dtype_name<E>();
        return text.c_str();
    }
};

template <class T>
struct conv<array<T>> {
    static PyObject* to(const array<T>& a) {
        size_t n = a.data.size();
        object shape;
        if (a.shape.empty()) {
            shape = object::steal(PyTuple_New(1));
            if (!shape) throw python_error();
            PyTuple_SetItem(shape.get(), 0, conv<int64_t>::to(static_cast<int64_t>(n)));
        } else {
            int64_t count = 1;
            for (int64_t d : a.shape) count *= d;
            if (count < 0 || static_cast<size_t>(count) != n)
                throw value_error("fa::array: shape holds " + std::to_string(count) + " elements, data has " +
                                  std::to_string(n));
            shape = object::steal(conv<std::vector<int64_t>>::to(a.shape));
            shape = object::steal(PySequence_Tuple(shape.get()));
            if (!shape) throw python_error();
        }
        object numpy = detail::import("numpy");
        object out = object::steal(
            PyObject_CallMethod(numpy.get(), "empty", "Os", shape.get(), detail::dtype_name<T>()));
        if (!out) throw python_error();
        Py_buffer view;
        if (PyObject_GetBuffer(out.get(), &view, PyBUF_WRITABLE | PyBUF_C_CONTIGUOUS) < 0) throw python_error();
        if constexpr (std::is_same_v<T, bool>) {
            auto* bytes = static_cast<unsigned char*>(view.buf);
            for (size_t i = 0; i < n; ++i) bytes[i] = a.data[i] ? 1 : 0;
        } else if (n) {
            std::memcpy(view.buf, a.data.data(), n * sizeof(T));
        }
        PyBuffer_Release(&view);
        return out.release();
    }
};

// Read attribute `name` of `o` as T (for generated dataclass/NamedTuple structs).
template <class T>
T attr(PyObject* o, const char* name) {
    object value = object::steal(PyObject_GetAttrString(o, name));
    if (!value) throw python_error();
    try {
        return conv<T>::from(value.get());
    } catch (const error& e) {
        throw error(e.type(), std::string("attribute '") + name + "': " + e.what());
    }
}

// ---------------------------------------------------------------------------------------------
// One call's arguments, by parameter name. names: every parameter in order; positional: how many
// may come positionally (the rest are keyword-only).

class args {
public:
    args(PyObject* positional_args, PyObject* kwargs, std::initializer_list<const char*> names, size_t positional)
        : n_(names.size()), positional_(positional < names.size() ? positional : names.size()) {
        if (n_ > inline_capacity) {
            heap_.resize(n_);
            params_ = heap_.data();
        }
        size_t j = 0;
        for (const char* name : names) params_[j++].name = name;

        Py_ssize_t given = positional_args ? PyTuple_Size(positional_args) : 0;
        if (given < 0) throw python_error();
        if (static_cast<size_t>(given) > positional_) {
            size_t g = static_cast<size_t>(given);
            throw type_error("function takes " + std::to_string(positional_) + " positional argument" +
                             (positional_ == 1 ? "" : "s") + " but " + std::to_string(g) +
                             (g == 1 ? " was" : " were") + " given");
        }
        for (Py_ssize_t i = 0; i < given; ++i) params_[i].value = PyTuple_GetItem(positional_args, i);

        if (!kwargs) return;
        Py_ssize_t pos = 0;
        PyObject *key, *value;
        while (PyDict_Next(kwargs, &pos, &key, &value)) {
            if (!PyUnicode_Check(key)) throw type_error("keywords must be strings");
            Py_ssize_t len = 0;
            const char* text = PyUnicode_AsUTF8AndSize(key, &len);
            if (!text) throw python_error();
            std::string_view k(text, static_cast<size_t>(len));
            param* p = nullptr;
            for (size_t i = 0; i < n_ && !p; ++i)
                if (k == params_[i].name) p = &params_[i];
            if (!p) throw type_error("function got an unexpected keyword argument '" + std::string(k) + "'");
            if (p->value) throw type_error("function got multiple values for argument '" + std::string(k) + "'");
            p->value = value;
        }
    }
    args(const args&) = delete;
    args& operator=(const args&) = delete;

    // Required parameter.
    template <class T>
    T get(size_t i) {
        if (!params_[i].value)
            throw type_error(std::string("function missing 1 required ") +
                             (i < positional_ ? "positional" : "keyword-only") + " argument: '" + params_[i].name +
                             "'");
        return convert<T>(i);
    }
    // Parameter with a default.
    template <class T>
    T get_or(size_t i, T fallback) {
        if (!params_[i].value) return fallback;
        return convert<T>(i);
    }

private:
    template <class T>
    T convert(size_t i) {
        try {
            return conv<T>::from(params_[i].value);
        } catch (const error& e) {
            throw error(e.type(), std::string("argument '") + params_[i].name + "': " + e.what());
        }
    }

    struct param {
        const char* name = nullptr;
        PyObject* value = nullptr;  // borrowed from the call's args tuple / kwargs dict
    };
    static constexpr size_t inline_capacity = 8;
    param inline_[inline_capacity];
    std::vector<param> heap_;
    param* params_ = inline_;
    size_t n_;
    size_t positional_;
};

// Run the port: with release_gil, the GIL is released while fn runs (re-taken on exception).
// Converts fn's result with conv<R>::to (void -> None).
template <class F>
PyObject* run(bool release_gil, F&& fn) {
    using R = std::remove_cvref_t<std::invoke_result_t<F&>>;
    detail::unlocked gil(release_gil);
    if constexpr (std::is_void_v<R>) {
        fn();
        gil.relock();
        return Py_NewRef(Py_None);
    } else {
        R result = fn();
        gil.relock();
        return conv<R>::to(result);
    }
}

// try { return fn(); } and map every C++ exception to a Python exception (see fa::error); a
// python_error leaves the error CPython set.
template <class F>
PyObject* guard(F&& fn) noexcept {
    try {
        return fn();
    } catch (const python_error&) {
        if (!PyErr_Occurred()) PyErr_SetString(PyExc_SystemError, "a Python call failed without an exception");
    } catch (const error& e) {
        detail::raise(e.type().c_str(), e.what());
    } catch (const std::bad_alloc&) {
        PyErr_NoMemory();
    } catch (const std::exception& e) {
        detail::raise("RuntimeError", e.what());
    } catch (...) {
        PyErr_SetString(PyExc_RuntimeError, "unknown C++ exception");
    }
    return nullptr;
}

}  // namespace fa::py

namespace fa {

namespace detail {
struct gil_taken {
    PyGILState_STATE state = PyGILState_Ensure();
    gil_taken() = default;
    gil_taken(const gil_taken&) = delete;
    gil_taken& operator=(const gil_taken&) = delete;
    ~gil_taken() { PyGILState_Release(state); }
};

template <class A>
PyObject* str_method_arg(const A& a) {
    if constexpr (std::is_convertible_v<const A&, std::u32string_view>) return str_from(std::u32string_view(a));
    else return py::conv<A>::to(a);
}
}  // namespace detail

template <class R, class... A>
R str_method(const std::u32string& self, const char* name, const A&... args) {
    detail::gil_taken gil;
    try {
        object s = object::steal(detail::str_from(self));
        object method = object::steal(PyObject_GetAttrString(s.get(), name));
        if (!method) throw py::python_error();
        object call_args = object::steal(PyTuple_New(sizeof...(A)));
        if (!call_args) throw py::python_error();
        Py_ssize_t i = 0;
        ((void)PyTuple_SetItem(call_args.get(), i++, detail::str_method_arg(args)), ...);
        object result = object::steal(PyObject_CallObject(method.get(), call_args.get()));
        if (!result) throw py::python_error();
        if constexpr (std::is_void_v<R>) return;
        else return py::conv<R>::from(result.get());
    } catch (const py::python_error&) {
        detail::throw_as_error();
    }
}

}  // namespace fa
