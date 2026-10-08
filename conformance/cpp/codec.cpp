// Conformance consumer: codec sample, C++ target (ABI revision 4).
//
// The shared-vector loop: fetch every vector the producer serves (decoded by
// the generated codecs), hand it back to `check_vector` (re-encoded by them),
// confirm it never matches its neighbor, and push each primitive vector's
// value through the matching direct-family `echo_*`. Then vectors built from
// literals (so a symmetric encode/decode bug can't hide), spot checks of
// decoded fields, the typed out-of-range error and its payload, malformed
// input rejected on both sides, and object identity and reference counting
// through value buffers. Ends by asserting the producer's leak counters are
// zero.

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <limits>
#include <string>
#include <unordered_map>
#include <variant>
#include <vector>

#include "check.hpp"
#include "codec.hpp"

namespace api = ::codec::codec;
using codec::Color;
using codec::Composite;
using codec::Holder;
using codec::InternalError;
using codec::OutOfRangeError;
using codec::Scalars;
using codec::Shape;
using codec::Token;
using codec::Vector;

template <typename T>
static bool bits_eq(T a, T b) {
    return std::memcmp(&a, &b, sizeof(T)) == 0;
}

static uint32_t find(uint32_t n, const std::string& name) {
    for (uint32_t i = 0; i < n; ++i) {
        if (api::vector_name(i) == name) return i;
    }
    std::fprintf(stderr, "no vector named %s\n", name.c_str());
    std::exit(1);
}

static Vector fetch(uint32_t n, const std::string& name) { return api::vector(find(n, name)); }

// Push a primitive vector's value through its direct-family echo.
static void echo(const Vector& v) {
    if (auto* x = std::get_if<Vector::I8>(&v.value)) CHECK(api::echo_i8(x->value) == x->value);
    if (auto* x = std::get_if<Vector::U8>(&v.value)) CHECK(api::echo_u8(x->value) == x->value);
    if (auto* x = std::get_if<Vector::I16>(&v.value)) CHECK(api::echo_i16(x->value) == x->value);
    if (auto* x = std::get_if<Vector::U16>(&v.value)) CHECK(api::echo_u16(x->value) == x->value);
    if (auto* x = std::get_if<Vector::I32>(&v.value)) CHECK(api::echo_i32(x->value) == x->value);
    if (auto* x = std::get_if<Vector::U32>(&v.value)) CHECK(api::echo_u32(x->value) == x->value);
    if (auto* x = std::get_if<Vector::I64>(&v.value)) CHECK(api::echo_i64(x->value) == x->value);
    if (auto* x = std::get_if<Vector::U64>(&v.value)) CHECK(api::echo_u64(x->value) == x->value);
    if (auto* x = std::get_if<Vector::F32>(&v.value)) CHECK(bits_eq(api::echo_f32(x->value), x->value));
    if (auto* x = std::get_if<Vector::F64>(&v.value)) CHECK(bits_eq(api::echo_f64(x->value), x->value));
    if (auto* x = std::get_if<Vector::Flag>(&v.value)) CHECK(api::echo_bool(x->value) == x->value);
    if (auto* x = std::get_if<Vector::Text>(&v.value)) CHECK(api::echo_text(x->value) == x->value);
    if (auto* x = std::get_if<Vector::Blob>(&v.value)) CHECK(api::echo_blob(x->value) == x->value);
    if (auto* x = std::get_if<Vector::Hue>(&v.value)) CHECK(api::echo_color(x->value) == x->value);
}

static void every_vector(uint32_t n) {
    for (uint32_t i = 0; i < n; ++i) {
        Vector v = api::vector(i);
        if (!api::check_vector(i, v)) {
            std::fprintf(stderr, "vector %u (%s) did not round-trip; producer saw %s\n", i,
                         api::vector_name(i).c_str(), api::describe_vector(v).c_str());
            std::exit(1);
        }
        CHECK(!api::check_vector((i + 1) % n, v));
        echo(v);
    }  // `v` releases every token it holds.
}

static Scalars canonical_scalars() {
    return Scalars{-8,
                   200,
                   -16000,
                   60000,
                   -2000000000,
                   4000000000u,
                   -9007199254740993LL,
                   UINT64_MAX,
                   1.5f,
                   -2.25e100,
                   true,
                   Color::Blue};
}

static void literal_vectors(uint32_t n) {
    Vector scalars{Vector::AllScalars{canonical_scalars()}};
    CHECK(api::check_vector(find(n, "scalars canonical"), scalars));
    std::get<Vector::AllScalars>(scalars.value).value.u16_value = 60001;
    CHECK(!api::check_vector(find(n, "scalars canonical"), scalars));

    CHECK(api::check_vector(find(n, "shape labeled"),
                            Vector{Vector::Figure{Shape{Shape::Labeled{"tag", 3}}}}));
    CHECK(api::check_vector(find(n, "string interior nul"),
                            Vector{Vector::Text{std::string("nul\0inside\0", 11)}}));

    // Any NaN matches the NaN vector; zero keeps its sign.
    uint64_t nan_bits = 0x7ff8000000000001ull;
    double nan = 0;
    std::memcpy(&nan, &nan_bits, sizeof nan);
    CHECK(api::check_vector(find(n, "f64 nan"), Vector{Vector::F64{nan}}));
    CHECK(api::check_vector(find(n, "f64 -0"), Vector{Vector::F64{-0.0}}));
    CHECK(!api::check_vector(find(n, "f64 -0"), Vector{Vector::F64{0.0}}));

    CHECK(api::check_vector(find(n, "u64 max"), Vector{Vector::U64{UINT64_MAX}}));
    CHECK(api::check_vector(find(n, "enum infrared"), Vector{Vector::Hue{Color::Infrared}}));

    CHECK(api::check_vector(find(n, "optional zero"), Vector{Vector::MaybeI64{0}}));
    CHECK(api::check_vector(find(n, "optional absent"), Vector{Vector::MaybeI64{std::nullopt}}));
    CHECK(!api::check_vector(find(n, "optional zero"), Vector{Vector::MaybeI64{std::nullopt}}));

    // A map's entry order doesn't matter on the wire.
    std::unordered_map<std::string, int64_t> counts;
    counts.emplace("x", 0);
    counts.emplace("h\xc3\xa9llo", -1);
    counts.emplace("", INT64_MAX);
    CHECK(api::check_vector(find(n, "map of strings"), Vector{Vector::Counts{counts}}));

    CHECK(api::check_vector(find(n, "blank"), Vector{Vector::Blank{}}));
}

static void spot_checks(uint32_t n) {
    Vector v = fetch(n, "i64 past 2^53");
    CHECK(std::get<Vector::I64>(v.value).value == -9007199254740993LL);

    v = fetch(n, "f32 min subnormal");
    uint32_t f32_bits = 0;
    float f = std::get<Vector::F32>(v.value).value;
    std::memcpy(&f32_bits, &f, sizeof f32_bits);
    CHECK(f32_bits == 1u);

    v = fetch(n, "string astral");
    CHECK(std::get<Vector::Text>(v.value).value == "\xf0\x9f\xa6\x80 crab \xf0\x9f\x98\x80");

    v = fetch(n, "scalars minimum");
    const Scalars& m = std::get<Vector::AllScalars>(v.value).value;
    CHECK(m.i8_value == INT8_MIN && m.i16_value == INT16_MIN && m.i32_value == INT32_MIN);
    CHECK(m.i64_value == INT64_MIN && m.u64_value == 0);
    CHECK(std::isinf(m.f32_value) && m.f32_value < 0 && std::isnan(m.f64_value));
    CHECK(m.color == Color::Infrared && !m.flag);

    v = fetch(n, "composite canonical");
    CHECK(v.tag() == Vector::Tag::Deep);
    const Composite& c = std::get<Vector::Deep>(v.value).value;
    CHECK(c.name == "h\xc3\xa9llo w\xc3\xb6rld \xe2\x9c\x93");
    CHECK(c.blob.size() == 6 && c.blob[5] == 255);
    CHECK(c.some_i64 == INT64_MIN && !c.none_i64.has_value());
    CHECK(c.some_text.has_value() && c.some_text->empty());
    CHECK(c.names.size() == 3 && c.names[1].empty());
    CHECK(c.matrix.size() == 3 && c.matrix[1].empty() && c.matrix[2][0] == -4);
    CHECK(c.floats.size() == 6 && std::isnan(c.floats[0]) && std::signbit(c.floats[3]));
    CHECK(c.by_name.size() == 4 && c.by_id.size() == 3 && c.by_color.size() == 2 &&
          c.flags.size() == 2);
    CHECK(c.scalars.u32_value == 4000000000u);
    CHECK(c.shape.tag() == Shape::Tag::Labeled && std::get<Shape::Labeled>(c.shape.value).count == 3);
    CHECK(c.shapes.size() == 6 && !std::get<Shape::Nested>(c.shapes[5].value).note.has_value());
    CHECK(c.maybe_shape.has_value() && c.maybe_shape->tag() == Shape::Tag::Nested);
    CHECK(c.maybe_list.has_value() && c.maybe_list->size() == 2);
    CHECK(c.sparse.size() == 3 && !c.sparse[1].has_value() && c.sparse[0] == true);
    CHECK(c.colors.size() == 4 && c.colors[3] == Color::Infrared);
}

static void out_of_range(uint32_t n) {
    OutOfRangeError e = expect_throw<OutOfRangeError>([n] { api::vector(n); }, "vector(n)");
    CHECK(e.code() == 1 && e.index == n && e.count == n);
    CHECK(std::string(e.what()) ==
          "vector " + std::to_string(n) + " is out of range (count " + std::to_string(n) + ")");
    CHECK(dynamic_cast<const codec::CodecError*>(&e) != nullptr);

    e = expect_throw<OutOfRangeError>([n] { api::vector_name(n + 5); }, "vector_name(n + 5)");
    CHECK(e.index == n + 5 && e.count == n);

    CHECK(!api::check_vector(n, Vector{Vector::Blank{}}));
}

static void malformed() {
    // An undeclared enum value is a marshalling failure (-3) on a call that
    // declares no errors.
    InternalError e = expect_throw<InternalError>(
        [] { api::echo_color(static_cast<Color>(3)); }, "echo_color(3)");
    CHECK(e.code() == -3);

    // The generated decoder rejects what the producer would: an unknown tag,
    // a truncated value, trailing bytes, a bad bool, and a repeated map key.
    const std::vector<std::vector<uint8_t>> bad = {
        {0xE7, 0x03, 0, 0},
        {7, 0, 0, 0, 1, 2, 3},
        {0, 0, 0, 0, 0},
        {11, 0, 0, 0, 2},
        {19, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 'a', 1, 0, 0, 0, 0, 0, 0, 0,
         1, 0, 0, 0, 'a', 2, 0, 0, 0, 0, 0, 0, 0},
    };
    for (const auto& buf : bad) {
        InternalError d = expect_throw<InternalError>(
            [&buf] { codec::detail::decode(buf.data(), buf.size(), &codec::detail::read_Vector); },
            "decoding a malformed Vector");
        CHECK(d.code() == -3);
    }
}

static void objects(uint32_t n) {
    // The full object vector decodes to live tokens with the table's values.
    Vector v = fetch(n, "objects full");
    const Holder& h = std::get<Vector::Objects>(v.value).value;
    CHECK(h.primary.value() == 10 && h.spare.has_value() && h.spare->value() == 11);
    CHECK(h.many.size() == 3 && h.many[2].value() == INT64_MIN);
    CHECK(h.by_name.size() == 2 && h.by_name.at("b").value() == 21);

    // Each encoding mints fresh references, so the holder can be sent twice.
    const int64_t expected = 10 + 11 + 12 + 13 + 20 + 21 + INT64_MIN;
    CHECK(api::sum_holder(h) == expected && api::sum_holder(h) == expected);

    // primary_of returns the very same object (a new reference to it).
    Token p = api::primary_of(h);
    CHECK(p.handle() == h.primary.handle());
    CHECK(api::same_primary(h, Holder{p, std::nullopt, {}, {}}));
    Token twin(10);
    CHECK(twin.value() == 10 && twin.handle() != h.primary.handle());
    CHECK(!api::same_primary(h, Holder{twin, std::nullopt, {}, {}}));

    // A holder built from consumer tokens, with one token in every slot.
    Holder mine{twin, twin, {twin, twin, Token(-4)}, {{"k", twin}}};
    CHECK(api::sum_holder(mine) == 10 * 5 - 4);

    // The consumer-built sparse holder checks against the table.
    CHECK(api::check_vector(find(n, "objects sparse"),
                            Vector{Vector::Objects{Holder{Token(-1), std::nullopt, {}, {}}}}));

    // Copies share the object; a moved-from wrapper is empty.
    Token copy = twin;
    CHECK(copy.handle() == twin.handle());
    Token moved = std::move(copy);
    CHECK(moved.handle() == twin.handle() && copy.handle() == nullptr);
}

int main() {
    codec::check_library();
    uint32_t n = api::vector_count();
    CHECK(n >= 60);

    every_vector(n);
    literal_vectors(n);
    spot_checks(n);
    out_of_range(n);
    malformed();
    objects(n);

    check_no_leaks(codec_debug_live, "codec");
    std::printf("cpp/codec: OK (%u vectors)\n", n);
    return 0;
}
