"""Exercises the fa_test_py module built by tests/cpp_py_header.rs from fa/py.hpp.

Usage: python3 test.py <directory holding fa_test_py.abi3.so>
"""

import collections
import dataclasses
import math
import sys
import types
import typing

sys.path.insert(0, sys.argv[1])
import fa_test_py as m  # noqa: E402

assert m.__file__.endswith(".abi3.so"), m.__file__
cases = 0


def case(fn):
    global cases
    fn()
    cases += 1
    return fn


def same(got, want):
    """Equal with the same types all the way down (int is not float, tuple is not list)."""
    assert type(got) is type(want), (got, want)
    if isinstance(want, float):
        assert got == want or (math.isnan(got) and math.isnan(want)), (got, want)
        assert math.copysign(1, got) == math.copysign(1, want), (got, want)
    elif isinstance(want, (list, tuple)):
        assert len(got) == len(want), (got, want)
        for g, w in zip(got, want):
            same(g, w)
    elif isinstance(want, dict):
        assert list(got) == list(want), (got, want)
        for k in want:
            same(got[k], want[k])
    else:
        assert got == want, (got, want)


def raises(exc, fn, *args, match=None, **kwargs):
    try:
        fn(*args, **kwargs)
    except BaseException as e:
        assert type(e) is exc, f"{fn.__name__}{args}{kwargs}: {type(e).__name__}: {e}, wanted {exc.__name__}"
        assert match is None or match in str(e), f"{match!r} not in {str(e)!r}"
        return e
    raise AssertionError(f"{fn.__name__}{args}{kwargs} did not raise {exc.__name__}")


@case
def ints():
    for x in [0, 1, -1, 2**63 - 1, -(2**63)]:
        same(m.echo_int(x), x)
    same(m.echo_int(True), 1)
    raises(OverflowError, m.echo_int, 2**63)
    raises(OverflowError, m.echo_int, -(2**63) - 1)
    raises(TypeError, m.echo_int, 1.5, match="argument 'x': expected int, got float")
    raises(TypeError, m.echo_int, "1")
    raises(TypeError, m.echo_int, None)


@case
def floats():
    for x in [0.0, -0.0, 1.5, -2.25e300, math.inf, -math.inf, math.nan]:
        same(m.echo_float(x), x)
    same(m.echo_float(3), 3.0)
    raises(TypeError, m.echo_float, "x")
    raises(OverflowError, m.echo_float, 2**2000)


@case
def bools():
    same(m.echo_bool(True), True)
    same(m.echo_bool(False), False)
    raises(TypeError, m.echo_bool, 1)
    raises(TypeError, m.echo_bool, None)


@case
def strings():
    for s in ["", "abc", "é", "日本", "👍🏽", "a\x00b", "\ufeffbom", "\ud800", "mixé日👍🏽"]:
        same(m.echo_str(s), s)
    same(m.str_len("👍🏽"), 2)
    same(m.str_len("日本"), 2)
    raises(TypeError, m.echo_str, b"x")
    raises(TypeError, m.echo_str, 1)


@case
def byte_strings():
    same(m.echo_bytes(b""), b"")
    same(m.echo_bytes(b"\x00\xff"), b"\x00\xff")
    same(m.echo_bytes(bytearray(b"ab")), b"ab")
    raises(TypeError, m.echo_bytes, "ab")


@case
def nested():
    d = {"zeta": [(1, 0.5), (2, -1.0)], "é日👍🏽": [], "alpha": [(3, 2.0)]}
    same(m.echo_nested(d), d)
    same(m.echo_nested({}), {})
    ints_as_floats = m.echo_nested({"a": [(1, 2)]})
    same(ints_as_floats, {"a": [(1, 2.0)]})
    same(m.echo_nested(collections.OrderedDict(d)), d)
    same(m.echo_nested(types.MappingProxyType(d)), d)
    raises(TypeError, m.echo_nested, {"a": [(1, "x")]})
    raises(TypeError, m.echo_nested, {"a": [(1.5, 2.0)]})
    raises(TypeError, m.echo_nested, {1: []})
    raises(TypeError, m.echo_nested, [("a", [])])
    raises(TypeError, m.echo_nested, "abc")


@case
def dict_order_and_lookup():
    same(m.ordered(), {"alpha": 20, "mid": 3, "zeta": 4})
    same(m.dict_at({"a": 1, "b": 2}, "b"), 2)
    raises(KeyError, m.dict_at, {"a": 1}, "z")


@case
def optionals():
    same(m.maybe_double(None), None)
    same(m.maybe_double(), None)
    same(m.maybe_double(21), 42)
    same(m.maybe_str(None), None)
    same(m.maybe_str("é"), "é")
    raises(TypeError, m.maybe_str, 1)
    same(m.echo_opt_list([1, None, 3]), [1, None, 3])
    same(m.echo_opt_list([]), [])


@case
def sequences():
    same(m.sum_list([1, 2, 3]), 6)
    same(m.sum_list((1, 2, 3)), 6)
    same(m.sum_list(range(4)), 6)
    same(m.sum_list(x for x in [5, 6]), 11)
    same(m.sum_list([]), 0)
    raises(TypeError, m.sum_list, "abc")
    raises(TypeError, m.sum_list, 5)
    raises(TypeError, m.sum_list, [1, "a"], match="argument 'xs'")
    same(m.echo_floats([1, 2.5]), [1.0, 2.5])
    same(m.echo_floats(()), [])


@case
def tuples():
    same(m.swap((1, "é")), ("é", 1))
    same(m.swap([1, "a"]), ("a", 1))
    raises(TypeError, m.swap, (1,), match="length 2")
    raises(TypeError, m.swap, (1, "a", 2))
    raises(TypeError, m.swap, ("a", 1))
    same(m.squares(4), (0, 1, 4, 9))
    same(m.squares(0), ())


@case
def sets():
    same(m.unique(["a", "é", "a"]), {"a", "é"})
    same(m.unique([]), set())
    same(m.set_size({1, 2, 3}), 3)
    same(m.set_size(frozenset({1, 2})), 2)
    same(m.set_size([1, 1, 2]), 2)
    same(m.set_size(set()), 0)
    raises(TypeError, m.set_size, {"a"})
    same(m.dedupe_tuples([(1, 2), (1, 2), (), (3,)]), {(1, 2), (), (3,)})
    same(m.count_pairs([(1, 2), (3, 4), (1, 2)]), {(1, 2): 2, (3, 4): 1})


@case
def arguments():
    same(m.kw(1), (1, 2.5, True, "é日", None))
    same(m.kw(1, 3, c=False, d="x", e=7), (1, 3.0, False, "x", 7))
    same(m.kw(a=5, b=0.5), (5, 0.5, True, "é日", None))
    same(m.kw(e=None, a=2), (2, 2.5, True, "é日", None))
    raises(TypeError, m.kw, 1, 2, 3, match="takes 2 positional arguments but 3 were given")
    raises(TypeError, m.kw, 1, f=2, match="unexpected keyword argument 'f'")
    raises(TypeError, m.kw, 1, a=2, match="multiple values for argument 'a'")
    raises(TypeError, m.kw, match="missing 1 required positional argument: 'a'")
    raises(TypeError, m.kw, 1, c=1, match="argument 'c': expected bool, got int")
    same(m.kwonly(k=3), 3)
    raises(TypeError, m.kwonly, 3, match="takes 0 positional arguments but 1 was given")
    raises(TypeError, m.kwonly, match="missing 1 required keyword-only argument: 'k'")
    raises(TypeError, m.kwonly, **{"é": 1}, match="unexpected keyword argument 'é'")
    same(m.ordered(), m.ordered())
    raises(TypeError, m.ordered, 1, match="takes 0 positional arguments but 1 was given")
    same(m.noop(1), None)


@dataclasses.dataclass
class Point:
    x: float
    y: float


class PointTuple(typing.NamedTuple):
    x: float
    y: float


@case
def structs():
    same(m.norm(Point(3.0, 4.0)), 5.0)
    same(m.norm(PointTuple(3, 4)), 5.0)
    same(m.total_norm([Point(3.0, 4.0), PointTuple(6.0, 8.0)]), 15.0)
    same(m.total_norm([]), 0.0)
    raises(AttributeError, m.norm, (3.0, 4.0))
    raises(TypeError, m.norm, Point("3", 4.0), match="attribute 'x'")


@case
def objects():
    o = [1, 2, 3]
    assert m.identity(o) is o
    assert m.identity(None) is None
    same(m.py_len(o), 3)
    same(m.py_len("日本"), 2)
    raises(TypeError, m.py_len, 5)


@case
def exceptions():
    e = raises(ValueError, m.fail, "value")
    assert str(e) == "bad value é", str(e)
    raises(TypeError, m.fail, "type")
    raises(KeyError, m.fail, "key")
    raises(IndexError, m.fail, "index")
    raises(ZeroDivisionError, m.fail, "zero")
    raises(OverflowError, m.fail, "overflow")
    raises(NotImplementedError, m.fail, "notimpl", match="port t.fail")
    raises(StopIteration, m.fail, "stop")
    raises(RuntimeError, m.fail, "unknown", match="unknown name")
    raises(RuntimeError, m.fail, "notclass")
    raises(RuntimeError, m.fail, "runtime", match="boom")
    raises(MemoryError, m.fail, "alloc")
    raises(RuntimeError, m.fail, "int", match="unknown C++ exception")
    same(m.fail("none"), None)


@case
def threads():
    xs = list(range(100_000))
    same(m.parallel_sum(xs, 8), sum(xs))
    same(m.parallel_sum(xs), sum(xs))
    words = ["ΑΒΓ", "İ", "ABC", "Straße", "ÉCOLE", "", "ǅ"] * 40
    same(m.lower_all(words, 4), [w.lower() for w in words])
    same(m.lower_all(["İ"], 1), ["i̇"])


@case
def str_methods():
    s = "ΑΒΓ  İ\u3000x\n"
    same(m.split_words(s), s.split())
    same(m.split_words("a,b,,c", ","), ["a", "b", "", "c"])
    same(m.split_words("a,b,c", ",", 1), ["a", "b,c"])
    same(m.split_words("日👍🏽日", "👍🏽"), ["日", "日"])
    raises(ValueError, m.split_words, "abc", "", match="empty separator")
    same(m.strip_dots("..é.."), "é")


try:
    import numpy as np
except ImportError:
    np = None

if np is not None:

    @case
    def ndarray_inputs():
        same(m.nd_info(np.arange(6.0)), (1, [6], 15.0))
        same(m.nd_info(np.arange(6.0).reshape(2, 3)), (2, [2, 3], 15.0))
        sliced = np.arange(12.0).reshape(3, 4)[:, ::2]
        assert not sliced.flags.c_contiguous
        same(m.nd_info(sliced), (2, [3, 2], 30.0))
        same(m.nd_info(np.arange(5.0)[::-1]), (1, [5], 10.0))
        same(m.nd_info(np.array(2.5)), (0, [], 2.5))
        same(m.nd_info(np.zeros(0)), (1, [0], 0.0))
        same(m.nd_info([1.0, 2.0]), (1, [2], 3.0))
        raises(TypeError, m.nd_info, np.arange(3, dtype=np.int32), match="float64")
        raises(TypeError, m.nd_info, np.arange(3, dtype=np.float32))
        raises(TypeError, m.nd_info, np.arange(3, dtype=">f8"))
        raises(TypeError, m.nd_info, None)
        raises(TypeError, m.nd_info, "abc")
        same(m.nd_sum_i32(np.arange(5, dtype=np.int32)), 10)
        same(m.nd_sum_i32(np.arange(5, dtype=np.int32)[::-2]), 6)
        raises(TypeError, m.nd_sum_i32, np.arange(5, dtype=np.int64))
        same(m.nd_sums([np.arange(3.0), np.ones((2, 2))[:, :1], np.zeros(0)]), [3.0, 2.0, 0.0])
        same(m.nd_sums([]), [])

    @case
    def array_results():
        r = m.outer(np.array([1.0, 2.0]), np.array([3.0, 4.0, 5.0]))
        assert type(r) is np.ndarray and r.dtype == np.float64 and r.shape == (2, 3), r
        assert r.flags.writeable and r.flags.c_contiguous
        assert r.tolist() == [[3.0, 4.0, 5.0], [6.0, 8.0, 10.0]], r
        r[0, 0] = 9.0
        assert r[0, 0] == 9.0
        empty = m.outer(np.zeros(0), np.arange(3.0))
        assert empty.shape == (0, 3) and empty.dtype == np.float64
        b = m.positive(np.array([-1.0, 2.0, 0.0]))
        assert b.dtype == np.bool_ and b.shape == (3,) and b.tolist() == [False, True, False], b
        assert b.flags.writeable
        raises(ValueError, m.bad_shape, match="shape holds 4 elements, data has 3")


print(f"cpp_py_header: {cases} cases passed (numpy: {'yes' if np is not None else 'no, skipped'})")
