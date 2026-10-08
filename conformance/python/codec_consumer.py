"""Conformance consumer: codec sample, Python target (ABI revision 4).

The shared-vector loop: for every vector the producer serves, decode it with
the generated codecs, pass it back to `check_vector` (which re-decodes the
generated encoding and compares), confirm it never matches its neighbor, and
push each primitive vector's value through the matching direct-family
`echo_*`. Then vectors built from literals (so a symmetric encode/decode bug
can't hide), spot checks of decoded values, the typed out-of-range error
with its payload, malformed input rejected on both sides, and object
identity and reference counting through buffers. Ends with the leak check
(see harness.py).
"""
import ctypes
import math
import struct
from typing import Any, Callable, Dict

import codec
from harness import Consumer

consumer = Consumer("codec", codec)
check = consumer.check
impl = consumer.impl

INT64_MIN = -(2**63)
INT64_MAX = 2**63 - 1
UINT64_MAX = 2**64 - 1


def f32_bits(x: float) -> bytes:
    return struct.pack("<f", x)


def f64_bits(x: float) -> bytes:
    return struct.pack("<d", x)


def same_f32(a: float, b: float) -> bool:
    return f32_bits(a) == f32_bits(b) or (math.isnan(a) and math.isnan(b))


def same_f64(a: float, b: float) -> bool:
    return f64_bits(a) == f64_bits(b) or (math.isnan(a) and math.isnan(b))


# The direct-family echo for each primitive vector variant, and how to
# compare its result with the vector's value.
ECHOES: Dict[type, Callable[[Any], bool]] = {
    codec.VectorI8: lambda v: codec.echo_i8(v) == v,
    codec.VectorU8: lambda v: codec.echo_u8(v) == v,
    codec.VectorI16: lambda v: codec.echo_i16(v) == v,
    codec.VectorU16: lambda v: codec.echo_u16(v) == v,
    codec.VectorI32: lambda v: codec.echo_i32(v) == v,
    codec.VectorU32: lambda v: codec.echo_u32(v) == v,
    codec.VectorI64: lambda v: codec.echo_i64(v) == v,
    codec.VectorU64: lambda v: codec.echo_u64(v) == v,
    codec.VectorF32: lambda v: same_f32(codec.echo_f32(v), v),
    codec.VectorF64: lambda v: same_f64(codec.echo_f64(v), v),
    codec.VectorFlag: lambda v: codec.echo_bool(v) is v,
    codec.VectorText: lambda v: codec.echo_text(v) == v,
    codec.VectorBlob: lambda v: codec.echo_blob(v) == v,
    codec.VectorHue: lambda v: codec.echo_color(v) is v,
}


def find(n: int, name: str) -> int:
    for i in range(n):
        if codec.vector_name(i) == name:
            return i
    check(False, f"no vector named {name}")
    raise AssertionError  # unreachable


def every_vector(n: int) -> None:
    for i in range(n):
        v = codec.vector(i)
        if not codec.check_vector(i, v):
            check(False, f"vector {i} ({codec.vector_name(i)}) did not round-trip; "
                         f"producer saw {codec.describe_vector(v)}")
        check(not codec.check_vector((i + 1) % n, v), f"vector {i} matches its neighbor")
        echo = ECHOES.get(type(v))
        if echo is not None:
            check(echo(v.value), f"echo of vector {i} ({codec.vector_name(i)})")
        del v  # releases any tokens inside


def canonical_scalars() -> codec.Scalars:
    return codec.Scalars(
        i8_value=-8, u8_value=200, i16_value=-16000, u16_value=60000,
        i32_value=-2000000000, u32_value=4000000000, i64_value=-9007199254740993,
        u64_value=UINT64_MAX, f32_value=1.5, f64_value=-2.25e100, flag=True,
        color=codec.Color.Blue,
    )


def literal_vectors(n: int) -> None:
    s = canonical_scalars()
    check(codec.check_vector(find(n, "scalars canonical"), codec.VectorAllScalars(s)),
          "scalars canonical")
    s.u16_value = 60001
    check(not codec.check_vector(find(n, "scalars canonical"), codec.VectorAllScalars(s)),
          "a changed field no longer matches")

    check(codec.check_vector(find(n, "shape labeled"),
                             codec.VectorFigure(codec.ShapeLabeled(label="tag", count=3))),
          "shape labeled")
    check(codec.check_vector(find(n, "string interior nul"), codec.VectorText("nul\0inside\0")),
          "string interior nul")

    # Any NaN matches the NaN vector; zero keeps its sign.
    nan = struct.unpack("<d", struct.pack("<Q", 0x7FF8000000000001))[0]
    check(codec.check_vector(find(n, "f64 nan"), codec.VectorF64(nan)), "f64 nan payload")
    check(codec.check_vector(find(n, "f64 -0"), codec.VectorF64(-0.0)), "f64 -0")
    check(not codec.check_vector(find(n, "f64 -0"), codec.VectorF64(0.0)), "+0 is not -0")

    check(codec.check_vector(find(n, "u64 max"), codec.VectorU64(UINT64_MAX)), "u64 max")
    check(codec.check_vector(find(n, "enum infrared"), codec.VectorHue(codec.Color.Infrared)),
          "enum infrared")

    check(codec.check_vector(find(n, "optional zero"), codec.VectorMaybeI64(0)), "Some(0)")
    check(codec.check_vector(find(n, "optional absent"), codec.VectorMaybeI64(None)), "None")
    check(not codec.check_vector(find(n, "optional zero"), codec.VectorMaybeI64(None)),
          "None is not Some(0)")

    # A map's entry order doesn't matter on the wire.
    counts = {"x": 0, "héllo": -1, "": INT64_MAX}
    check(codec.check_vector(find(n, "map of strings"), codec.VectorCounts(counts)),
          "map of strings in another order")

    check(codec.check_vector(find(n, "blank"), codec.VectorBlank()), "blank")

    with codec.Token(-1) as lone:
        sparse = codec.Holder(primary=lone, spare=None, many=[], by_name={})
        check(codec.check_vector(find(n, "objects sparse"), codec.VectorObjects(sparse)),
              "objects sparse from a consumer-made token")


def spot_checks(n: int) -> None:
    v: Any = codec.vector(find(n, "i64 past 2^53"))
    check(isinstance(v, codec.VectorI64) and v.value == -9007199254740993, "i64 past 2^53")

    v = codec.vector(find(n, "f32 min subnormal"))
    check(isinstance(v, codec.VectorF32) and f32_bits(v.value) == b"\x01\0\0\0",
          "f32 min subnormal bits")

    v = codec.vector(find(n, "string astral"))
    check(v == codec.VectorText("🦀 crab 😀"), f"string astral {v!r}")

    v = codec.vector(find(n, "scalars minimum"))
    check(isinstance(v, codec.VectorAllScalars), "scalars minimum variant")
    m = v.value
    check((m.i8_value, m.u8_value, m.i16_value, m.u16_value, m.i32_value, m.u32_value,
           m.i64_value, m.u64_value) == (-128, 0, -32768, 0, -(2**31), 0, INT64_MIN, 0),
          f"scalars minimum integers {m!r}")
    check(m.f32_value == -math.inf and math.isnan(m.f64_value), "scalars minimum floats")
    check(m.color is codec.Color.Infrared and m.flag is False, "scalars minimum enum and flag")

    v = codec.vector(find(n, "composite canonical"))
    check(isinstance(v, codec.VectorDeep), "composite canonical variant")
    c = v.value
    check(c.name == "héllo wörld ✓", "name")
    check(c.blob == bytes([0, 1, 2, 253, 254, 255]), "blob")
    check(c.some_i64 == INT64_MIN and c.none_i64 is None and c.some_text == "", "optionals")
    check(c.names == ["a", "", "ccc"], "names")
    check(c.matrix == [[1, 2, 3], [], [-4]], "matrix")
    check(len(c.floats) == 6 and math.isnan(c.floats[0]) and c.floats[1:3] == [math.inf, -math.inf]
          and f64_bits(c.floats[3]) == f64_bits(-0.0) and c.floats[4] == 5e-324
          and c.floats[5] == 0.1, f"floats {c.floats!r}")
    check(c.by_name == {"one": 1, "two": 2, "neg": -3, "": INT64_MAX}, "by_name")
    check(sorted(c.by_id) == [-1, 42, 2**31 - 1] and c.by_id[-1] == canonical_scalars(), "by_id")
    check(c.by_color == {codec.Color.Infrared: "below", codec.Color.Blue: "sky"}, "by_color")
    check(c.flags == {0: False, UINT64_MAX: True}, "flags")
    check(c.scalars == canonical_scalars(), "scalars")
    check(c.shape == codec.ShapeLabeled(label="tag", count=3), "shape")
    check(len(c.shapes) == 6 and isinstance(c.shapes[5], codec.ShapeNested)
          and c.shapes[5].note is None, f"shapes {c.shapes!r}")
    check(isinstance(c.maybe_shape, codec.ShapeNested) and c.maybe_shape.note is None,
          "maybe_shape")
    check(c.maybe_list == b"\x09\x08", "maybe_list")
    check(c.sparse == [True, None, False], "sparse")
    check(c.colors == [codec.Color.Red, codec.Color.Green, codec.Color.Blue, codec.Color.Infrared],
          "colors")
    check(c.shape.tag is codec.Shape.Tag.Labeled and codec.Shape.Labeled is codec.ShapeLabeled,
          "rich enum tag and scoped alias")


def out_of_range(n: int) -> None:
    exc = consumer.raises(codec.CodecError.OutOfRange, lambda: codec.vector(n), "vector(n)")
    check(exc.code == 1 and exc.index == n and exc.count == n, f"OutOfRange payload {exc!r}")
    check(exc.message == f"vector {n} is out of range (count {n})", f"message {exc.message!r}")
    check(isinstance(exc, codec.CodecError) and isinstance(exc, codec.Error), "hierarchy")

    exc = consumer.raises(codec.OutOfRange, lambda: codec.vector_name(n + 5), "vector_name")
    check(exc.index == n + 5 and exc.count == n, "vector_name payload")

    check(not codec.check_vector(n, codec.VectorBlank()), "check_vector past the end")


def check_raw(data: bytes) -> int:
    """Send `data` to check_vector through its bound C function, returning the
    error code."""
    err = impl._ErrorStruct()
    impl._c_codec_check_vector(0, data, len(data), ctypes.byref(err))
    code = err.code
    impl._error_clear(ctypes.byref(err))
    return code


def malformed() -> None:
    i32 = struct.Struct("<i").pack
    u32 = struct.Struct("<I").pack
    tag = codec.Vector.Tag
    bad = [
        ("truncated", i32(tag.I64) + u32(7)),
        ("unknown tag", i32(999)),
        ("trailing bytes", i32(tag.Blank) + b"\0"),
        ("bool 2", i32(tag.Flag) + b"\x02"),
        ("undeclared enum", i32(tag.Hue) + i32(3)),
        ("repeated map key", i32(tag.Counts) + u32(2) + u32(1) + b"a" + struct.pack("<q", 1)
         + u32(1) + b"a" + struct.pack("<q", 2)),
        ("non-UTF-8", i32(tag.Text) + u32(2) + b"\xc3\x28"),
    ]
    for what, data in bad:
        check(check_raw(data) == -3, f"producer rejects {what}")
        # The generated decoder rejects the same input as a trap.
        exc = consumer.raises(codec.InternalError, lambda: impl._decode(data, impl._read_Vector),
                              f"decoder rejects {what}")
        check(exc.code == -3, f"decoder code for {what}")

    # An undeclared C-style enum value in a direct slot is a marshalling
    # failure; echo_color declares no errors, so it traps.
    exc = consumer.raises(codec.InternalError, lambda: codec.echo_color(3), "echo_color(3)")
    check(exc.code == -3 and isinstance(exc, RuntimeError), f"echo_color trap {exc!r}")


def objects(n: int) -> None:
    v: Any = codec.vector(find(n, "objects full"))
    check(isinstance(v, codec.VectorObjects), "objects full variant")
    h = v.value
    check(h.primary.value() == 10 and h.spare is not None and h.spare.value() == 11, "tokens")
    check([t.value() for t in h.many] == [12, 13, INT64_MIN], "many")
    check({k: t.value() for k, t in h.by_name.items()} == {"a": 20, "b": 21}, "by_name")

    # Each encoding mints fresh references, so the holder can be sent twice.
    expected = (10 + 11 + 12 + 13 + 20 + 21 + INT64_MIN + 2**63) % 2**64 - 2**63
    check(codec.sum_holder(h) == expected and codec.sum_holder(h) == expected, "sum_holder twice")

    # primary_of returns the same native object (a new wrapper over it).
    p = codec.primary_of(h)
    check(p == h.primary and p is not h.primary, "primary_of is the same object")
    check(codec.same_primary(h, codec.Holder(primary=p, spare=None, many=[], by_name={})),
          "same_primary by identity")
    twin = codec.Token(10)
    check(twin != h.primary, "an equal value is a different object")
    check(not codec.same_primary(h, codec.Holder(primary=twin, spare=None, many=[], by_name={})),
          "same_primary is identity, not value")

    # A holder built from consumer-made tokens, one wrapper in every slot.
    minus = codec.Token(-4)
    mine = codec.Holder(primary=twin, spare=twin, many=[twin, twin, minus], by_name={"k": twin})
    check(codec.sum_holder(mine) == 10 * 5 - 4, "consumer-built holder")

    # A closed wrapper can't be encoded.
    minus.close()
    consumer.raises(ValueError, lambda: codec.sum_holder(mine), "closed token")
    twin.close()
    twin.close()
    p.close()


def main() -> None:
    n = codec.vector_count()
    check(n >= 60, f"vector_count {n}")
    every_vector(n)
    literal_vectors(n)
    spot_checks(n)
    out_of_range(n)
    malformed()
    objects(n)
    consumer.finish()


main()
