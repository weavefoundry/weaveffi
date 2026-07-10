"""Conformance consumer: shapes sample, Python target.

Drives the generated ctypes wrapper for rich (algebraic) enums: the opaque
`Shape` class whose handle is freed in `__del__`, its nested `Tag` IntEnum +
`tag` reader, the per-variant `@classmethod` factories (`Shape.circle(...)`)
and namespaced field accessors (`circle_radius`), plus the free functions
that take and return `Shape` (module-prefix-stripped: `describe`, `scale`,
`sum_bytes`). Also covers the expanded numerics (f32 fields, u8 field, u64
return). The generated package is placed on sys.path via WV_PY; the cdylib is
selected with WEAVEFFI_LIBRARY.
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import shapes as wv  # noqa: E402


def main() -> None:
    # Unit variant: tag only.
    empty = wv.Shape.empty()
    assert empty.tag == wv.Shape.Tag.Empty

    # f64 payload.
    circle = wv.Shape.circle(2.5)
    assert circle.tag == wv.Shape.Tag.Circle
    assert abs(circle.circle_radius - 2.5) < 1e-9

    # Two f32 payloads.
    rect = wv.Shape.rectangle(3.0, 4.0)
    assert rect.tag == wv.Shape.Tag.Rectangle
    assert abs(rect.rectangle_width - 3.0) < 1e-6
    assert abs(rect.rectangle_height - 4.0) < 1e-6

    # string + u8 payload.
    labeled = wv.Shape.labeled("hex", 6)
    assert labeled.tag == wv.Shape.Tag.Labeled
    assert labeled.labeled_label == "hex"
    assert labeled.labeled_count == 6

    # Free functions: Shape in, string/Shape out.
    assert wv.describe(circle) == "circle(r=2.5)"

    big = wv.scale(circle, 4.0)
    assert big.tag == wv.Shape.Tag.Circle
    assert abs(big.circle_radius - 10.0) < 1e-9

    # Numerics: list<u8> in, u64 out.
    assert wv.sum_bytes([250, 250, 250, 250]) == 1000

    print("python/shapes: OK")


main()
