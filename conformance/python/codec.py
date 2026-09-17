"""Conformance consumer: codec sample, Python target.

Drives the generated ctypes wrapper's value-buffer encoder and decoder against
the producer's round-trip oracle: the canonical `Scalars` and `Composite`
fixtures are decoded and checked field by field (producer encodes, consumer
decodes), handed back through `verify_*` (consumer encodes, producer decodes),
and re-encoded through `roundtrip_*` and compared; consumer-built values with
edge cases (every integer extreme, NaN, the infinities, negative zero, empty
strings, lists, and maps, non-BMP unicode) round-trip; every `Shape` variant
crosses alone, in a list, and inside `Composite`; the direct, string, bytes,
and optional families round-trip; and `Holder` carries `Token` objects in a
field, an optional, and a list (`sum_holder`, `primary_of` returning the same
underlying object, `same_primary`, and every wrapper released with an
idempotent `close()`). The generated package is placed on sys.path via WV_PY;
the cdylib is selected with WEAVEFFI_LIBRARY.
"""
import dataclasses
import gc
import math
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import codec as wv  # noqa: E402

I64_MIN = -(1 << 63)
I64_MAX = (1 << 63) - 1
U64_MAX = (1 << 64) - 1


def check(cond: bool, what: str) -> None:
    if not cond:
        print(f"python/codec: FAIL: {what}", file=sys.stderr)
        sys.exit(1)


def same_float(a: float, b: float) -> bool:
    """Bitwise-aware float equality: NaN matches NaN and -0.0 differs from 0.0."""
    if math.isnan(a) or math.isnan(b):
        return math.isnan(a) and math.isnan(b)
    return a == b and math.copysign(1.0, a) == math.copysign(1.0, b)


def canonical_scalars() -> wv.Scalars:
    return wv.Scalars(
        i8_value=-8,
        u8_value=200,
        i16_value=-16_000,
        u16_value=60_000,
        i32_value=-2_000_000_000,
        u32_value=4_000_000_000,
        i64_value=-9_007_199_254_740_993,
        u64_value=U64_MAX,
        f32_value=1.5,
        f64_value=-2.25e100,
        flag=True,
        color=wv.Color.Blue,
    )


def canonical_composite() -> wv.Composite:
    scalars = canonical_scalars()
    return wv.Composite(
        name="héllo wörld ✓",
        blob=bytes([0, 1, 2, 253, 254, 255]),
        some_i64=I64_MIN,
        none_i64=None,
        some_text="",
        names=["a", "", "ccc"],
        matrix=[[1, 2, 3], [], [-4]],
        empty=[],
        by_name={"neg": -3, "one": 1, "two": 2},
        by_id={-1: scalars, 42: dataclasses.replace(scalars, flag=False)},
        scalars=scalars,
        shape=wv.Shape.Labeled(label="tag", count=3),
        shapes=[
            wv.Shape.Empty(),
            wv.Shape.Circle(radius=2.5),
            wv.Shape.Rect(width=1.0, height=0.5),
            wv.Shape.Labeled(label="", count=-1),
            wv.Shape.Nested(inner=scalars, note="n"),
        ],
        maybe_shape=wv.Shape.Nested(inner=scalars, note=None),
        maybe_list=bytes([9, 8]),
        sparse=[True, None, False],
        colors=[wv.Color.Red, wv.Color.Green, wv.Color.Blue],
    )


def scalars() -> None:
    check(wv.Color.Red == 0 and wv.Color.Green == 1 and wv.Color.Blue == 7, "Color discriminants")

    # Producer encodes, consumer decodes: every field of the fixture.
    sample = wv.sample_scalars()
    check(isinstance(sample, wv.Scalars), "sample_scalars type")
    check(sample.i8_value == -8 and sample.u8_value == 200, "i8/u8")
    check(sample.i16_value == -16_000 and sample.u16_value == 60_000, "i16/u16")
    check(sample.i32_value == -2_000_000_000 and sample.u32_value == 4_000_000_000, "i32/u32")
    check(sample.i64_value == -9_007_199_254_740_993, f"i64 {sample.i64_value}")
    check(sample.u64_value == U64_MAX, f"u64 {sample.u64_value}")
    check(sample.f32_value == 1.5 and sample.f64_value == -2.25e100, "f32/f64")
    check(sample.flag is True and sample.color is wv.Color.Blue, "flag/color")
    check(sample == canonical_scalars(), "sample_scalars equals the local canonical value")

    # Consumer encodes, producer decodes.
    check(wv.verify_scalars(sample) is True, "verify_scalars(sample)")
    check(wv.verify_scalars(canonical_scalars()) is True, "verify_scalars(local canonical)")
    check(wv.roundtrip_scalars(sample) == sample, "roundtrip_scalars(sample)")

    # A one-field change is a typed Mismatch.
    changed = dataclasses.replace(sample, u8_value=201)
    try:
        wv.verify_scalars(changed)
        check(False, "expected CodecError.Mismatch")
    except wv.CodecError.Mismatch as exc:
        check(exc.code == 1 and exc.CODE == 1, f"Mismatch code {exc.code}")
        check(isinstance(exc, wv.CodecError) and isinstance(exc, wv.WeaveFFIError), "hierarchy")
        check(exc.message == "value does not match the canonical fixture",
              f"Mismatch message {exc.message!r}")
    check(wv.Mismatch is wv.CodecError.Mismatch, "bare Mismatch is the scoped alias")

    # Consumer-built extremes: every integer bound, negative zero, NaN, inf.
    edges = wv.Scalars(
        i8_value=-128, u8_value=255, i16_value=-32768, u16_value=65535,
        i32_value=-(1 << 31), u32_value=(1 << 32) - 1, i64_value=I64_MIN, u64_value=U64_MAX,
        f32_value=-0.0, f64_value=float("nan"), flag=False, color=wv.Color.Red,
    )
    back = wv.roundtrip_scalars(edges)
    check(back.i8_value == -128 and back.u8_value == 255, "edge i8/u8")
    check(back.i16_value == -32768 and back.u16_value == 65535, "edge i16/u16")
    check(back.i32_value == -(1 << 31) and back.u32_value == (1 << 32) - 1, "edge i32/u32")
    check(back.i64_value == I64_MIN and back.u64_value == U64_MAX, "edge i64/u64")
    check(same_float(back.f32_value, -0.0), f"f32 negative zero {back.f32_value!r}")
    check(same_float(back.f64_value, float("nan")), f"f64 NaN {back.f64_value!r}")
    check(back.flag is False and back.color is wv.Color.Red, "edge flag/color")
    highs = wv.Scalars(
        i8_value=127, u8_value=0, i16_value=32767, u16_value=0,
        i32_value=(1 << 31) - 1, u32_value=0, i64_value=I64_MAX, u64_value=0,
        f32_value=float("inf"), f64_value=float("-inf"), flag=True, color=wv.Color.Green,
    )
    back = wv.roundtrip_scalars(highs)
    check(back.i8_value == 127 and back.i16_value == 32767 and back.i32_value == (1 << 31) - 1,
          "high signed bounds")
    check(back.u8_value == 0 and back.u16_value == 0 and back.u32_value == 0
          and back.u64_value == 0, "unsigned zeros")
    check(back.i64_value == I64_MAX, "i64 max")
    check(back.f32_value == float("inf") and back.f64_value == float("-inf"), "infinities")
    check(back.color is wv.Color.Green, "Green through a record")
    # f32 rounds to the nearest single: 0.1 does not survive, 0.25 does.
    check(wv.roundtrip_scalars(dataclasses.replace(highs, f32_value=0.25)).f32_value
          == 0.25, "f32 exact value")
    check(wv.roundtrip_scalars(dataclasses.replace(highs, f32_value=0.1)).f32_value
          != 0.1, "f32 narrows to single precision")


def composite() -> None:
    sample = wv.sample_composite()
    check(isinstance(sample, wv.Composite), "sample_composite type")
    check(sample.name == "héllo wörld ✓", f"name {sample.name!r}")
    check(sample.blob == bytes([0, 1, 2, 253, 254, 255]), f"blob {sample.blob!r}")
    check(sample.some_i64 == I64_MIN and sample.none_i64 is None, "optionals")
    check(sample.some_text == "", f"some_text {sample.some_text!r}")
    check(sample.names == ["a", "", "ccc"], f"names {sample.names}")
    check(sample.matrix == [[1, 2, 3], [], [-4]], f"matrix {sample.matrix}")
    check(sample.empty == [], "empty list")
    check(sample.by_name == {"one": 1, "two": 2, "neg": -3}, f"by_name {sample.by_name}")
    check(sorted(sample.by_id) == [-1, 42], f"by_id keys {sorted(sample.by_id)}")
    check(sample.by_id[-1] == canonical_scalars(), "by_id[-1]")
    check(sample.by_id[42].flag is False and sample.by_id[42].u64_value == U64_MAX, "by_id[42]")
    check(sample.scalars == canonical_scalars(), "nested scalars")
    check(sample.shape == wv.Shape.Labeled("tag", 3), f"shape {sample.shape}")
    check(sample.shape.tag == wv.Shape.Tag.Labeled and isinstance(sample.shape, wv.Shape),
          "shape tag")
    check([s.tag for s in sample.shapes] == list(wv.Shape.Tag), "shapes one of each variant")
    check(sample.shapes[1] == wv.Shape.Circle(2.5) and sample.shapes[2] == wv.Shape.Rect(1.0, 0.5),
          "circle / rect")
    check(sample.shapes[3] == wv.Shape.Labeled("", -1), "labeled with empty label")
    check(sample.shapes[4] == wv.Shape.Nested(canonical_scalars(), "n"), "nested with note")
    check(sample.maybe_shape == wv.Shape.Nested(canonical_scalars(), None), "maybe_shape")
    check(sample.maybe_list == bytes([9, 8]), f"maybe_list {sample.maybe_list!r}")
    check(sample.sparse == [True, None, False], f"sparse {sample.sparse}")
    check(sample.colors == [wv.Color.Red, wv.Color.Green, wv.Color.Blue], f"colors {sample.colors}")
    check(all(isinstance(c, wv.Color) for c in sample.colors), "colors are enum members")
    check(sample == canonical_composite(), "sample_composite equals the local canonical value")

    check(wv.verify_composite(sample) is True, "verify_composite(sample)")
    check(wv.verify_composite(canonical_composite()) is True, "verify_composite(local canonical)")
    check(wv.roundtrip_composite(sample) == sample, "roundtrip_composite(sample)")
    described = wv.describe_composite(sample)
    check(described.startswith("Composite { name: \"héllo wörld ✓\"") and "sparse: [Some(true), None, Some(false)]" in described,
          f"describe_composite {described[:80]!r}")

    changed = dataclasses.replace(sample, sparse=[True, True, False])
    try:
        wv.verify_composite(changed)
        check(False, "expected CodecError.Mismatch for a changed composite")
    except wv.CodecError.Mismatch:
        pass

    # A consumer-built composite full of empties and extremes.
    nan_scalars = wv.Scalars(0, 0, 0, 0, 0, 0, 0, 0, float("nan"), -0.0, False, wv.Color.Green)
    mine = wv.Composite(
        name="",
        blob=b"",
        some_i64=I64_MAX,
        none_i64=-1,
        some_text=None,
        names=[],
        matrix=[[], [(1 << 31) - 1, -(1 << 31)]],
        empty=[float("inf"), float("-inf"), -0.0, float("nan"), 1e-308, 1.7976931348623157e308],
        by_name={"": I64_MIN, "𝔘𝔫𝔦𝔠𝔬𝔡𝔢 日本語 🧵": I64_MAX},
        by_id={},
        scalars=nan_scalars,
        shape=wv.Shape.Empty(),
        shapes=[],
        maybe_shape=None,
        maybe_list=None,
        sparse=[],
        colors=[],
    )
    back = wv.roundtrip_composite(mine)
    check(back.name == "" and back.blob == b"" and back.names == [], "empties")
    check(back.some_i64 == I64_MAX and back.none_i64 == -1 and back.some_text is None,
          "swapped optionals")
    check(back.matrix == [[], [(1 << 31) - 1, -(1 << 31)]], f"matrix {back.matrix}")
    check(len(back.empty) == 6 and all(same_float(a, b) for a, b in zip(back.empty, mine.empty)),
          f"float list {back.empty}")
    check(back.by_name == mine.by_name, f"unicode-keyed map {back.by_name}")
    check(back.by_id == {} and back.shapes == [] and back.sparse == [] and back.colors == [],
          "empty containers")
    check(back.maybe_shape is None and back.maybe_list is None, "absent optionals")
    check(back.shape == wv.Shape.Empty(), "unit variant")
    check(same_float(back.scalars.f32_value, float("nan"))
          and same_float(back.scalars.f64_value, -0.0), "NaN / -0.0 inside a nested record")
    check(back.scalars.color is wv.Color.Green, "nested enum")
    check(wv.describe_composite(mine).startswith("Composite { name: \"\", blob: []"),
          "describe_composite of the local value")
    # Presence flags matter: an absent optional list differs from an empty one.
    with_empty_list = dataclasses.replace(mine, maybe_list=b"")
    check(wv.roundtrip_composite(with_empty_list).maybe_list == b"", "present empty optional list")


def shapes() -> None:
    variants = [
        wv.Shape.Empty(),
        wv.Shape.Circle(radius=-0.0),
        wv.Shape.Rect(width=0.5, height=float("inf")),
        wv.Shape.Labeled(label="ünïcødé ✓", count=-(1 << 31)),
        wv.Shape.Nested(inner=canonical_scalars(), note=""),
        wv.Shape.Nested(inner=canonical_scalars(), note=None),
    ]
    for v in variants:
        back = wv.roundtrip_shape(v)
        check(type(back) is type(v) and back.tag == v.tag, f"roundtrip_shape type {v}")
        check(back == v, f"roundtrip_shape {v} -> {back}")
    check(same_float(wv.roundtrip_shape(variants[1]).radius, -0.0), "Circle -0.0 radius")
    check(wv.roundtrip_shape(variants[2]).height == float("inf"), "Rect inf height")
    check(wv.roundtrip_shapes(variants) == variants, "roundtrip_shapes")
    check(wv.roundtrip_shapes([]) == [], "roundtrip_shapes([])")
    check(wv.describe_shape(wv.Shape.Empty()) == "Empty", "describe Empty")
    check(wv.describe_shape(wv.Shape.Circle(2.5)) == "Circle { radius: 2.5 }", "describe Circle")
    check(wv.describe_shape(wv.Shape.Rect(1.0, 0.5)) == "Rect { width: 1.0, height: 0.5 }",
          "describe Rect")
    check(wv.describe_shape(wv.Shape.Labeled("x", 7)) == "Labeled { label: \"x\", count: 7 }",
          "describe Labeled")
    check(wv.describe_shape(wv.Shape.Nested(canonical_scalars(), None)).startswith(
        "Nested { inner: Scalars { i8_value: -8,"), "describe Nested")
    check(wv.Shape.Tag.Nested == 4 and wv.Shape.Nested.TAG == wv.Shape.Tag.Nested, "Tag values")


def direct_families() -> None:
    check(wv.roundtrip_opt_i64(None) is None, "opt_i64 None")
    check(wv.roundtrip_opt_i64(0) == 0, "opt_i64 0")
    check(wv.roundtrip_opt_i64(I64_MIN) == I64_MIN, "opt_i64 min")
    check(wv.roundtrip_opt_i64(I64_MAX) == I64_MAX, "opt_i64 max")
    check(wv.roundtrip_map({}) == {}, "map {}")
    m = {"": 0, "a": I64_MIN, "ü": I64_MAX, "z" * 300: -1}
    check(wv.roundtrip_map(m) == m, "map contents")
    check(wv.roundtrip_string("") == "", "string ''")
    check(wv.roundtrip_string("héllo 日本語 🧵") == "héllo 日本語 🧵", "unicode string")
    check(wv.roundtrip_bytes(b"") == b"", "bytes b''")
    check(wv.roundtrip_bytes(bytes(range(256))) == bytes(range(256)), "all byte values")
    for v in (0, 1, -1, I64_MIN, I64_MAX):
        check(wv.roundtrip_i64(v) == v, f"i64 {v}")
    for v in (0, 1, 1 << 63, U64_MAX):
        check(wv.roundtrip_u64(v) == v, f"u64 {v}")
    for v in (0.0, -0.0, 1.5, -2.25e100, float("inf"), float("-inf"), float("nan"),
              5e-324, 1.7976931348623157e308):
        check(same_float(wv.roundtrip_f64(v), v), f"f64 {v!r} -> {wv.roundtrip_f64(v)!r}")
    check(wv.roundtrip_bool(True) is True and wv.roundtrip_bool(False) is False, "bool")
    for c in wv.Color:
        check(wv.roundtrip_color(c) is c, f"color {c}")


def holders() -> None:
    holder = wv.make_holder(10, True)
    check(isinstance(holder, wv.Holder), "make_holder type")
    check(isinstance(holder.primary, wv.Token) and holder.primary.value() == 10, "primary")
    check(holder.spare is not None and holder.spare.value() == 11, "spare present")
    check([t.value() for t in holder.many] == [12, 13, 14], "many")
    check(wv.sum_holder(holder) == 10 + 11 + 12 + 13 + 14, "sum_holder")
    # Encoding clones each token, so the wrappers stay usable afterwards and
    # the holder can be sent again and again.
    check(wv.sum_holder(holder) == 60 and holder.primary.value() == 10, "sum_holder twice")

    # primary_of hands back a wrapper over the very same object as
    # holder.primary, which the producer confirms by pointer identity.
    primary = wv.primary_of(holder)
    check(isinstance(primary, wv.Token) and primary is not holder.primary, "primary_of wrapper")
    check(primary.value() == 10, "primary_of value")
    check(wv.same_primary(holder, wv.Holder(primary=primary, spare=None, many=[])) is True,
          "primary_of is the same object as holder.primary")
    check(wv.same_primary(holder, holder) is True, "same_primary(holder, holder)")
    check(wv.same_primary(holder, wv.make_holder(10, True)) is False,
          "distinct tokens with equal values are not the same object")
    primary.close()
    primary.close()
    check(holder.primary.value() == 10, "holder.primary alive after closing the other wrapper")

    without = wv.make_holder(0, False)
    check(without.spare is None, "spare absent")
    check(wv.sum_holder(without) == 0 + 2 + 3 + 4, "sum without spare")

    # Consumer-built tokens in every buffered position.
    with wv.Token(1) as a, wv.Token(2) as b:
        c, d = wv.Token(3), wv.Token(4)
        mine = wv.Holder(primary=a, spare=b, many=[c, d, a])
        check(wv.sum_holder(mine) == 1 + 2 + 3 + 4 + 1, "sum of a local holder")
        check(wv.sum_holder(wv.Holder(primary=a, spare=None, many=[])) == 1, "minimal holder")
        p = wv.primary_of(mine)
        check(p.value() == 1 and wv.same_primary(mine, wv.Holder(p, None, [])) is True,
              "primary_of a local holder")
        check(wv.same_primary(mine, wv.Holder(b, None, [])) is False, "different primary")
        p.close()
        c.close()
        d.close()
        c.close()
        # A closed token anywhere in the holder rejects the whole encoding
        # before any reference is minted, so the live tokens leak nothing.
        for broken in (wv.Holder(primary=c, spare=None, many=[]),
                       wv.Holder(primary=a, spare=b, many=[a, d])):
            try:
                wv.sum_holder(broken)
                check(False, "expected error encoding a closed token")
            except wv.WeaveFFIError as exc:
                check("Token used after close" in exc.message,
                      f"closed token message {exc.message!r}")
        check(a.value() == 1, "a still alive inside the with block")
    try:
        a.value()
        check(False, "expected use-after-close error")
    except wv.WeaveFFIError:
        pass

    # Release every producer-built wrapper explicitly (twice), then let the
    # rest go to the garbage collector.
    for t in [holder.primary, holder.spare, without.primary] + holder.many + without.many:
        t.close()
        t.close()
    del holder, without, primary
    gc.collect()


def main() -> None:
    scalars()
    composite()
    shapes()
    direct_families()
    holders()
    gc.collect()
    print("python/codec: OK")


main()
