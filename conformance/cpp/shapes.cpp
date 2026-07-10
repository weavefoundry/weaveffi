// Conformance consumer: shapes sample, C++ target.
//
// Drives the generated header-only wrappers for rich (algebraic) enums: the
// move-only RAII `Shape` class, its nested `Tag` enum + `tag()` reader, the
// per-variant static factories (`Shape::Circle(...)`) and field accessors
// (`circle_radius()`), plus the free functions that take and return `Shape` by
// value. The 0.5.0 surface puts those functions at bare snake_case names in
// the per-module namespace, so with the configured root namespace `shapes`
// they live at `shapes::shapes::describe`. Also covers the expanded numerics
// (f32 fields, u8 field, u64 return). Aborts (non-zero) on any failed
// assertion.

#include <cassert>
#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

#include "weaveffi.hpp"

using shapes::Shape;
using namespace shapes::shapes;

int main() {
    // Unit variant.
    Shape empty = Shape::Empty();
    assert(empty.tag() == Shape::Tag::Empty);

    // f64 payload.
    Shape circle = Shape::Circle(2.5);
    assert(circle.tag() == Shape::Tag::Circle);
    assert(std::fabs(circle.circle_radius() - 2.5) < 1e-9);

    // Two f32 payloads.
    Shape rect = Shape::Rectangle(3.0f, 4.0f);
    assert(rect.tag() == Shape::Tag::Rectangle);
    assert(std::fabs(rect.rectangle_width() - 3.0f) < 1e-6f);
    assert(std::fabs(rect.rectangle_height() - 4.0f) < 1e-6f);

    // string + u8 payload.
    Shape labeled = Shape::Labeled("hex", 6);
    assert(labeled.tag() == Shape::Tag::Labeled);
    assert(labeled.labeled_label() == "hex");
    assert(labeled.labeled_count() == 6);

    // Free functions: Shape in, string/Shape out.
    assert(describe(circle) == "circle(r=2.5)");

    Shape big = scale(circle, 4.0);
    assert(big.tag() == Shape::Tag::Circle);
    assert(std::fabs(big.circle_radius() - 10.0) < 1e-9);

    // Numerics: list<u8> in, u64 out.
    std::vector<uint8_t> bytes{250, 250, 250, 250};
    assert(sum_bytes(bytes) == 1000);

    std::printf("cpp/shapes: OK\n");
    return 0;
}
