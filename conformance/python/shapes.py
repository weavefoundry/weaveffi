"""Conformance consumer: shapes sample, Python target.

Drives the generated ctypes wrapper for rich (algebraic) enums as value
types: the `Shape` base class with its nested `Tag` IntEnum and `tag`
reader, the per-variant dataclasses reachable as `Shape.Circle(...)` with
natural field names (`radius`, not `circle_radius`) and value equality,
plus the free functions that take and return `Shape` as value buffers
(module-prefix-stripped: `describe`, `scale`, `sum_bytes`). Also covers the
expanded numerics (f32 fields, u8 field, u64 return) and the plain C-style
`Channel` enum. The generated package is placed on sys.path via WV_PY; the
cdylib is selected with WEAVEFFI_LIBRARY.
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import shapes as wv  # noqa: E402


def main() -> None:
    # Unit variant: tag only.
    empty = wv.Shape.Empty()
    assert isinstance(empty, wv.Shape)
    assert empty.tag == wv.Shape.Tag.Empty

    # f64 payload.
    circle = wv.Shape.Circle(2.5)
    assert circle.tag == wv.Shape.Tag.Circle
    assert abs(circle.radius - 2.5) < 1e-9

    # Two f32 payloads.
    rect = wv.Shape.Rectangle(3.0, 4.0)
    assert rect.tag == wv.Shape.Tag.Rectangle
    assert abs(rect.width - 3.0) < 1e-6
    assert abs(rect.height - 4.0) < 1e-6

    # string + u8 payload.
    labeled = wv.Shape.Labeled("hex", 6)
    assert labeled.tag == wv.Shape.Tag.Labeled
    assert labeled.label == "hex"
    assert labeled.count == 6

    # Free functions: Shape in, string/Shape out.
    assert wv.describe(circle) == "circle(r=2.5)"

    big = wv.scale(circle, 4.0)
    assert isinstance(big, wv.Shape.Circle)
    assert big.tag == wv.Shape.Tag.Circle
    assert abs(big.radius - 10.0) < 1e-9

    # Variants are dataclasses: values decoded from the producer compare
    # equal to locally constructed ones.
    assert big == wv.Shape.Circle(10.0)
    assert wv.Shape.Labeled("hex", 6) == labeled

    # A C-style enum keeps its plain integer constants.
    assert wv.Channel.Green == 1

    # Numerics: `[u8]` canonicalizes to `bytes` in, u64 out.
    assert wv.sum_bytes(bytes([250, 250, 250, 250])) == 1000

    print("python/shapes: OK")


main()
