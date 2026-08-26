// Conformance consumer: shapes sample, C++ target.
//
// Drives the generated header-only wrappers for rich (algebraic) enums as
// value types: `Shape` is a plain struct holding a std::variant of per-variant
// payload structs plus a `tag()` reader, and the free functions take and
// return it by value (the wrapper packs and unpacks the value buffer
// underneath). Functions live at bare snake_case names in the per-module
// namespace, so with the configured root namespace `shapes` they sit at
// `shapes::shapes::describe`. Also covers the expanded numerics (f32 fields,
// u8 field, u64 return). Aborts (non-zero) on any failed assertion.

#include <cassert>
#include <cmath>
#include <cstdio>
#include <string>
#include <variant>
#include <vector>

#include "weaveffi.hpp"

using shapes::Shape;
using namespace shapes::shapes;

int main() {
    // Unit variant.
    Shape empty{Shape::Empty{}};
    assert(empty.tag() == Shape::Tag::Empty);
    assert(std::holds_alternative<Shape::Empty>(empty.value));

    // f64 payload.
    Shape circle{Shape::Circle{2.5}};
    assert(circle.tag() == Shape::Tag::Circle);
    assert(std::fabs(std::get<Shape::Circle>(circle.value).radius - 2.5) < 1e-9);

    // Two f32 payloads.
    Shape rect{Shape::Rectangle{3.0f, 4.0f}};
    assert(rect.tag() == Shape::Tag::Rectangle);
    assert(std::fabs(std::get<Shape::Rectangle>(rect.value).width - 3.0f) < 1e-6f);
    assert(std::fabs(std::get<Shape::Rectangle>(rect.value).height - 4.0f) < 1e-6f);

    // string + u8 payload.
    Shape labeled{Shape::Labeled{"hex", 6}};
    assert(labeled.tag() == Shape::Tag::Labeled);
    assert(std::get<Shape::Labeled>(labeled.value).label == "hex");
    assert(std::get<Shape::Labeled>(labeled.value).count == 6);

    // Free functions: Shape in, string/Shape out.
    assert(describe(circle) == "circle(r=2.5)");

    Shape big = scale(circle, 4.0);
    assert(big.tag() == Shape::Tag::Circle);
    assert(std::fabs(std::get<Shape::Circle>(big.value).radius - 10.0) < 1e-9);

    // Numerics: list<u8> in, u64 out.
    std::vector<uint8_t> bytes{250, 250, 250, 250};
    assert(sum_bytes(bytes) == 1000);

    std::printf("cpp/shapes: OK\n");
    return 0;
}
