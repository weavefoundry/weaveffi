// Conformance consumer: codec sample, C++ target (ABI revision 2).
//
// Round-trips every value-buffer wire shape through the producer oracle with
// the generated header-only wrapper:
//  - `sample_*` (producer encodes, consumer decodes) is checked field by
//    field against the canonical fixture, then handed back to `verify_*`
//    (consumer encodes, producer decodes) which throws `MismatchError`
//    unless the bytes decode to exactly the canonical value;
//  - `roundtrip_*` covers the direct, string, and bytes families plus
//    consumer-built values with edge cases (empty strings, lists, and maps,
//    non-ASCII text, i64/u64 extremes, NaN, infinities, negative zero);
//  - `Shape` exercises every rich-enum variant as a std::variant;
//  - `Holder` puts `Token` objects inside a record field, an optional, and a
//    list: decoding adopts one reference per token, encoding clones one per
//    token, `primary_of` returns the same object as `holder.primary`, and
//    `same_primary` compares identity through two encodings.
// Exits non-zero on the first failed check.

#include <cmath>
#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <limits>
#include <optional>
#include <string>
#include <unordered_map>
#include <utility>
#include <variant>
#include <vector>

#include "weaveffi.hpp"

// The root namespace is `codec` and the module namespace is `codec::codec`,
// so types live at `codec::Scalars` and functions at `codec::codec::f`.
namespace oracle = codec::codec;
using codec::Color;
using codec::Composite;
using codec::Holder;
using codec::Scalars;
using codec::Shape;
using codec::Token;

static void check(bool ok, const char* what) {
    if (!ok) {
        std::fprintf(stderr, "cpp/codec: FAIL: %s\n", what);
        std::exit(1);
    }
}

static bool same_f32(float a, float b) {
    if (std::isnan(a) && std::isnan(b)) return true;
    return a == b && std::signbit(a) == std::signbit(b);
}

static bool same_f64(double a, double b) {
    if (std::isnan(a) && std::isnan(b)) return true;
    return a == b && std::signbit(a) == std::signbit(b);
}

static bool scalars_equal(const Scalars& a, const Scalars& b) {
    return a.i8_value == b.i8_value && a.u8_value == b.u8_value && a.i16_value == b.i16_value &&
           a.u16_value == b.u16_value && a.i32_value == b.i32_value &&
           a.u32_value == b.u32_value && a.i64_value == b.i64_value &&
           a.u64_value == b.u64_value && same_f32(a.f32_value, b.f32_value) &&
           same_f64(a.f64_value, b.f64_value) && a.flag == b.flag && a.color == b.color;
}

static bool shapes_equal(const Shape& a, const Shape& b) {
    if (a.tag() != b.tag()) return false;
    switch (a.tag()) {
    case Shape::Tag::Empty:
        return true;
    case Shape::Tag::Circle:
        return same_f64(std::get<Shape::Circle>(a.value).radius,
                        std::get<Shape::Circle>(b.value).radius);
    case Shape::Tag::Rect: {
        const auto& x = std::get<Shape::Rect>(a.value);
        const auto& y = std::get<Shape::Rect>(b.value);
        return same_f32(x.width, y.width) && same_f32(x.height, y.height);
    }
    case Shape::Tag::Labeled: {
        const auto& x = std::get<Shape::Labeled>(a.value);
        const auto& y = std::get<Shape::Labeled>(b.value);
        return x.label == y.label && x.count == y.count;
    }
    case Shape::Tag::Nested: {
        const auto& x = std::get<Shape::Nested>(a.value);
        const auto& y = std::get<Shape::Nested>(b.value);
        return scalars_equal(x.inner, y.inner) && x.note == y.note;
    }
    }
    return false;
}

static bool composites_equal(const Composite& a, const Composite& b) {
    if (a.name != b.name || a.blob != b.blob || a.some_i64 != b.some_i64 ||
        a.none_i64 != b.none_i64 || a.some_text != b.some_text || a.names != b.names ||
        a.matrix != b.matrix || a.by_name != b.by_name || a.maybe_list != b.maybe_list ||
        a.sparse != b.sparse || a.colors != b.colors) {
        return false;
    }
    if (a.empty.size() != b.empty.size()) return false;
    for (size_t i = 0; i < a.empty.size(); i++) {
        if (!same_f64(a.empty[i], b.empty[i])) return false;
    }
    if (a.by_id.size() != b.by_id.size()) return false;
    for (const auto& kv : a.by_id) {
        auto it = b.by_id.find(kv.first);
        if (it == b.by_id.end() || !scalars_equal(kv.second, it->second)) return false;
    }
    if (!scalars_equal(a.scalars, b.scalars) || !shapes_equal(a.shape, b.shape)) return false;
    if (a.shapes.size() != b.shapes.size()) return false;
    for (size_t i = 0; i < a.shapes.size(); i++) {
        if (!shapes_equal(a.shapes[i], b.shapes[i])) return false;
    }
    if (a.maybe_shape.has_value() != b.maybe_shape.has_value()) return false;
    if (a.maybe_shape.has_value() && !shapes_equal(*a.maybe_shape, *b.maybe_shape)) return false;
    return true;
}

static void check_canonical_scalars(const Scalars& s, const char* who) {
    std::string prefix = std::string(who) + ": canonical scalars ";
    check(s.i8_value == -8, (prefix + "i8").c_str());
    check(s.u8_value == 200, (prefix + "u8").c_str());
    check(s.i16_value == -16000, (prefix + "i16").c_str());
    check(s.u16_value == 60000, (prefix + "u16").c_str());
    check(s.i32_value == -2000000000, (prefix + "i32").c_str());
    check(s.u32_value == 4000000000u, (prefix + "u32").c_str());
    check(s.i64_value == -9007199254740993LL, (prefix + "i64").c_str());
    check(s.u64_value == std::numeric_limits<uint64_t>::max(), (prefix + "u64").c_str());
    check(s.f32_value == 1.5f, (prefix + "f32").c_str());
    check(s.f64_value == -2.25e100, (prefix + "f64").c_str());
    check(s.flag, (prefix + "flag").c_str());
    check(s.color == Color::Blue, (prefix + "color").c_str());
}

int main() {
    codec::check_abi_version();

    // Scalars: producer encodes, consumer decodes, and back again.
    Scalars sample = oracle::sample_scalars();
    check_canonical_scalars(sample, "sample_scalars");
    check(oracle::verify_scalars(sample), "verify_scalars accepts the re-encoded sample");
    Scalars echoed = oracle::roundtrip_scalars(sample);
    check_canonical_scalars(echoed, "roundtrip_scalars");
    check(scalars_equal(sample, echoed), "roundtrip_scalars is identical");

    // A mismatch is the typed domain exception with its declared code.
    {
        Scalars changed = sample;
        changed.u8_value = 201;
        bool caught = false;
        try {
            oracle::verify_scalars(changed);
        } catch (const codec::MismatchError& e) {
            caught = (e.code() == 1);
            check(dynamic_cast<const codec::CodecError*>(&e) != nullptr,
                  "MismatchError is a CodecError");
            check(std::string(e.what()) == "value does not match the canonical fixture",
                  "MismatchError carries the doc-comment message");
        }
        check(caught, "verify_scalars throws MismatchError for a changed value");
        changed = sample;
        changed.flag = false;
        caught = false;
        try {
            oracle::verify_scalars(changed);
        } catch (const codec::CodecError& e) {
            caught = (e.code() == 1);
        }
        check(caught, "a flipped bool is detected");
    }

    // Consumer-built scalars at the edges of every width.
    {
        Scalars edge{std::numeric_limits<int8_t>::min(),   std::numeric_limits<uint8_t>::max(),
                     std::numeric_limits<int16_t>::min(),  std::numeric_limits<uint16_t>::max(),
                     std::numeric_limits<int32_t>::min(),  std::numeric_limits<uint32_t>::max(),
                     std::numeric_limits<int64_t>::min(),  std::numeric_limits<uint64_t>::max(),
                     -0.0f,                                std::numeric_limits<double>::quiet_NaN(),
                     false,                                Color::Red};
        Scalars back = oracle::roundtrip_scalars(edge);
        check(scalars_equal(edge, back), "edge scalars round-trip");
        check(std::signbit(back.f32_value) && back.f32_value == 0.0f, "negative zero f32 keeps its sign");
        check(std::isnan(back.f64_value), "NaN f64 round-trips as NaN");

        Scalars inf = edge;
        inf.f32_value = std::numeric_limits<float>::infinity();
        inf.f64_value = -std::numeric_limits<double>::infinity();
        inf.i64_value = std::numeric_limits<int64_t>::max();
        inf.color = Color::Green;
        back = oracle::roundtrip_scalars(inf);
        check(scalars_equal(inf, back), "infinity scalars round-trip");
        check(std::isinf(back.f32_value) && back.f32_value > 0, "+inf f32");
        check(std::isinf(back.f64_value) && back.f64_value < 0, "-inf f64");
    }

    // Composite: every nested shape, checked against the canonical fixture.
    Composite comp = oracle::sample_composite();
    check(comp.name == "h\xC3\xA9llo w\xC3\xB6rld \xE2\x9C\x93", "composite name (UTF-8)");
    check(comp.blob == std::vector<uint8_t>({0, 1, 2, 253, 254, 255}), "composite blob");
    check(comp.some_i64.has_value() && *comp.some_i64 == std::numeric_limits<int64_t>::min(),
          "composite some_i64 is i64::MIN");
    check(!comp.none_i64.has_value(), "composite none_i64 absent");
    check(comp.some_text.has_value() && comp.some_text->empty(), "composite some_text is Some(\"\")");
    check(comp.names.size() == 3 && comp.names[0] == "a" && comp.names[1].empty() &&
              comp.names[2] == "ccc",
          "composite names");
    check(comp.matrix.size() == 3 && comp.matrix[0] == std::vector<int32_t>({1, 2, 3}) &&
              comp.matrix[1].empty() && comp.matrix[2] == std::vector<int32_t>({-4}),
          "composite matrix");
    check(comp.empty.empty(), "composite empty list");
    check(comp.by_name.size() == 3 && comp.by_name.at("one") == 1 && comp.by_name.at("two") == 2 &&
              comp.by_name.at("neg") == -3,
          "composite by_name");
    check(comp.by_id.size() == 2, "composite by_id has two entries");
    check_canonical_scalars(comp.by_id.at(-1), "by_id[-1]");
    check(!comp.by_id.at(42).flag && comp.by_id.at(42).u64_value == std::numeric_limits<uint64_t>::max(),
          "composite by_id[42] differs only in flag");
    check_canonical_scalars(comp.scalars, "composite.scalars");
    check(comp.shape.tag() == Shape::Tag::Labeled &&
              std::get<Shape::Labeled>(comp.shape.value).label == "tag" &&
              std::get<Shape::Labeled>(comp.shape.value).count == 3,
          "composite shape is Labeled{tag, 3}");
    check(comp.shapes.size() == 5, "composite shapes has one of each variant");
    check(comp.shapes[0].tag() == Shape::Tag::Empty, "shapes[0] Empty");
    check(comp.shapes[1].tag() == Shape::Tag::Circle &&
              std::get<Shape::Circle>(comp.shapes[1].value).radius == 2.5,
          "shapes[1] Circle{2.5}");
    check(comp.shapes[2].tag() == Shape::Tag::Rect &&
              std::get<Shape::Rect>(comp.shapes[2].value).width == 1.0f &&
              std::get<Shape::Rect>(comp.shapes[2].value).height == 0.5f,
          "shapes[2] Rect{1, 0.5}");
    check(comp.shapes[3].tag() == Shape::Tag::Labeled &&
              std::get<Shape::Labeled>(comp.shapes[3].value).label.empty() &&
              std::get<Shape::Labeled>(comp.shapes[3].value).count == -1,
          "shapes[3] Labeled{\"\", -1}");
    check(comp.shapes[4].tag() == Shape::Tag::Nested, "shapes[4] Nested");
    {
        const auto& nested = std::get<Shape::Nested>(comp.shapes[4].value);
        check_canonical_scalars(nested.inner, "shapes[4].inner");
        check(nested.note.has_value() && *nested.note == "n", "shapes[4].note is Some(n)");
    }
    check(comp.maybe_shape.has_value() && comp.maybe_shape->tag() == Shape::Tag::Nested &&
              !std::get<Shape::Nested>(comp.maybe_shape->value).note.has_value(),
          "composite maybe_shape is Nested with no note");
    check(comp.maybe_list.has_value() && *comp.maybe_list == std::vector<uint8_t>({9, 8}),
          "composite maybe_list");
    check(comp.sparse.size() == 3 && comp.sparse[0] == std::optional<bool>(true) &&
              !comp.sparse[1].has_value() && comp.sparse[2] == std::optional<bool>(false),
          "composite sparse");
    check(comp.colors == std::vector<Color>({Color::Red, Color::Green, Color::Blue}),
          "composite colors");

    check(oracle::verify_composite(comp), "verify_composite accepts the re-encoded sample");
    Composite comp_back = oracle::roundtrip_composite(comp);
    check(composites_equal(comp, comp_back), "roundtrip_composite is identical");
    check(oracle::verify_composite(comp_back), "the round-tripped composite still verifies");
    {
        std::string text = oracle::describe_composite(comp);
        check(text.find("h\xC3\xA9llo w\xC3\xB6rld \xE2\x9C\x93") != std::string::npos,
              "describe_composite renders the name");
        check(text.find("Labeled") != std::string::npos, "describe_composite renders the shape");

        Composite changed = comp;
        changed.sparse[1] = true;
        bool caught = false;
        try {
            oracle::verify_composite(changed);
        } catch (const codec::MismatchError&) {
            caught = true;
        }
        check(caught, "a changed nested optional is detected");
        changed = comp;
        changed.by_id.erase(42);
        caught = false;
        try {
            oracle::verify_composite(changed);
        } catch (const codec::MismatchError&) {
            caught = true;
        }
        check(caught, "a removed map entry is detected");
    }

    // A consumer-built composite with the sparse/empty corners.
    {
        Scalars zero{0, 0, 0, 0, 0, 0, 0, 0, 0.0f, 0.0, false, Color::Red};
        Composite mine;
        mine.name = "";
        mine.blob = {};
        mine.some_i64 = std::numeric_limits<int64_t>::max();
        mine.none_i64 = -1;
        mine.some_text = std::nullopt;
        mine.names = {"\xF0\x9F\x8E\x89", "", "\xE6\x97\xA5\xE6\x9C\xAC\xE8\xAA\x9E"};
        mine.matrix = {};
        mine.empty = {-0.0, std::numeric_limits<double>::infinity(), 1e-300};
        mine.by_name = {};
        mine.by_id = {{std::numeric_limits<int32_t>::min(), zero},
                      {0, sample},
                      {std::numeric_limits<int32_t>::max(), zero}};
        mine.scalars = zero;
        mine.shape = Shape{Shape::Empty{}};
        mine.shapes = {};
        mine.maybe_shape = std::nullopt;
        mine.maybe_list = std::vector<uint8_t>{};
        mine.sparse = {std::nullopt, std::nullopt};
        mine.colors = {};
        Composite back = oracle::roundtrip_composite(mine);
        check(composites_equal(mine, back), "consumer-built composite round-trips");
        check(back.name.empty() && back.blob.empty() && back.matrix.empty() &&
                  back.by_name.empty() && back.shapes.empty() && back.colors.empty(),
              "empty containers stay empty");
        check(back.maybe_list.has_value() && back.maybe_list->empty(),
              "Some(empty list) stays Some");
        check(!back.some_text.has_value() && back.none_i64 == std::optional<int64_t>(-1),
              "optional fields keep their presence");
        check(back.by_id.size() == 3 && back.by_id.count(std::numeric_limits<int32_t>::min()) == 1,
              "int-keyed map keeps extreme keys");
        check(std::signbit(back.empty[0]) && std::isinf(back.empty[1]) && back.empty[2] == 1e-300,
              "f64 list keeps -0.0, inf, and denormal-range values");
        bool caught = false;
        try {
            oracle::verify_composite(mine);
        } catch (const codec::MismatchError&) {
            caught = true;
        }
        check(caught, "a non-canonical composite is rejected");
    }

    // Rich enum variants one at a time, plus a list of them.
    {
        std::vector<Shape> all{
            Shape{Shape::Empty{}},
            Shape{Shape::Circle{-0.0}},
            Shape{Shape::Rect{std::numeric_limits<float>::infinity(), -1.25f}},
            Shape{Shape::Labeled{"h\xC3\xA9", std::numeric_limits<int32_t>::min()}},
            Shape{Shape::Nested{sample, std::string("")}},
            Shape{Shape::Nested{sample, std::nullopt}},
        };
        for (const Shape& s : all) {
            Shape back = oracle::roundtrip_shape(s);
            check(shapes_equal(s, back), "roundtrip_shape preserves the variant");
        }
        check(oracle::describe_shape(all[0]) == "Empty", "describe_shape Empty");
        check(oracle::describe_shape(Shape{Shape::Circle{2.5}}) == "Circle { radius: 2.5 }",
              "describe_shape Circle");
        check(oracle::describe_shape(Shape{Shape::Labeled{"x", 3}}) ==
                  "Labeled { label: \"x\", count: 3 }",
              "describe_shape Labeled");
        std::vector<Shape> many_back = oracle::roundtrip_shapes(all);
        check(many_back.size() == all.size(), "roundtrip_shapes keeps the count");
        for (size_t i = 0; i < all.size(); i++) {
            check(shapes_equal(all[i], many_back[i]), "roundtrip_shapes preserves each element");
        }
        check(oracle::roundtrip_shapes({}).empty(), "roundtrip_shapes of an empty list");
    }

    // Top-level optionals, maps, strings, bytes, and direct scalars.
    {
        check(oracle::roundtrip_opt_i64(std::nullopt) == std::nullopt, "roundtrip_opt_i64 none");
        check(oracle::roundtrip_opt_i64(std::numeric_limits<int64_t>::min()) ==
                  std::optional<int64_t>(std::numeric_limits<int64_t>::min()),
              "roundtrip_opt_i64 some(i64::MIN)");
        check(oracle::roundtrip_opt_i64(0) == std::optional<int64_t>(0), "roundtrip_opt_i64 some(0)");

        std::unordered_map<std::string, int64_t> m{{"", 0}, {"k", -1}, {"\xE2\x9C\x93", 7}};
        check(oracle::roundtrip_map(m) == m, "roundtrip_map");
        check(oracle::roundtrip_map({}).empty(), "roundtrip_map empty");

        check(oracle::roundtrip_string("") == "", "roundtrip_string empty");
        check(oracle::roundtrip_string("h\xC3\xA9llo \xE2\x9C\x93 \xF0\x9F\x8E\x89") ==
                  "h\xC3\xA9llo \xE2\x9C\x93 \xF0\x9F\x8E\x89",
              "roundtrip_string non-ASCII");

        check(oracle::roundtrip_bytes({}).empty(), "roundtrip_bytes empty");
        std::vector<uint8_t> all_bytes;
        for (int i = 0; i < 256; i++) all_bytes.push_back(static_cast<uint8_t>(i));
        check(oracle::roundtrip_bytes(all_bytes) == all_bytes, "roundtrip_bytes 0..255");

        check(oracle::roundtrip_i64(std::numeric_limits<int64_t>::min()) ==
                  std::numeric_limits<int64_t>::min(),
              "roundtrip_i64 min");
        check(oracle::roundtrip_i64(std::numeric_limits<int64_t>::max()) ==
                  std::numeric_limits<int64_t>::max(),
              "roundtrip_i64 max");
        check(oracle::roundtrip_u64(std::numeric_limits<uint64_t>::max()) ==
                  std::numeric_limits<uint64_t>::max(),
              "roundtrip_u64 max");
        check(oracle::roundtrip_u64(1ull << 63) == (1ull << 63), "roundtrip_u64 2^63");
        check(std::isnan(oracle::roundtrip_f64(std::numeric_limits<double>::quiet_NaN())),
              "roundtrip_f64 NaN");
        check(oracle::roundtrip_f64(-std::numeric_limits<double>::infinity()) ==
                  -std::numeric_limits<double>::infinity(),
              "roundtrip_f64 -inf");
        double neg_zero = oracle::roundtrip_f64(-0.0);
        check(neg_zero == 0.0 && std::signbit(neg_zero), "roundtrip_f64 -0.0");
        check(oracle::roundtrip_bool(true) && !oracle::roundtrip_bool(false), "roundtrip_bool");
        check(oracle::roundtrip_color(Color::Blue) == Color::Blue, "roundtrip_color Blue (7)");
        check(oracle::roundtrip_color(Color::Red) == Color::Red, "roundtrip_color Red");
    }

    // Objects inside buffers.
    {
        Token t(5);
        check(t.value() == 5, "Token constructor and method");
        Token t_copy = t;
        check(t_copy.handle() == t.handle() && t_copy.value() == 5, "Token copy shares the object");

        Holder holder = oracle::make_holder(10, true);
        check(holder.primary.value() == 10, "holder.primary");
        check(holder.spare.has_value() && holder.spare->value() == 11, "holder.spare");
        check(holder.many.size() == 3 && holder.many[0].value() == 12 &&
                  holder.many[1].value() == 13 && holder.many[2].value() == 14,
              "holder.many");
        check(holder.primary.handle() != holder.spare->handle(), "distinct tokens are distinct objects");

        // Encoding clones one reference per token; the wrappers stay valid.
        check(oracle::sum_holder(holder) == 10 + 11 + 12 + 13 + 14, "sum_holder");
        check(oracle::sum_holder(holder) == 60, "sum_holder is repeatable (buffers are single use)");
        check(holder.primary.value() == 10 && holder.many[2].value() == 14,
              "wrappers survive being encoded");

        Token primary = oracle::primary_of(holder);
        check(primary.handle() == holder.primary.handle(), "primary_of returns the same object");
        check(primary.value() == 10, "primary_of is usable");

        check(oracle::same_primary(holder, holder), "same_primary(h, h)");
        Holder holder_copy = holder;
        check(holder_copy.primary.handle() == holder.primary.handle(),
              "a copied record shares its objects");
        check(oracle::same_primary(holder, holder_copy), "same_primary with a copied holder");
        Holder other = oracle::make_holder(10, true);
        check(!oracle::same_primary(holder, other), "same_primary with a fresh holder is false");
        check(oracle::sum_holder(other) == 60, "fresh holder sums the same");

        Holder without = oracle::make_holder(0, false);
        check(!without.spare.has_value(), "make_holder(_, false) has no spare");
        check(oracle::sum_holder(without) == 0 + 2 + 3 + 4, "sum_holder without spare");

        // A consumer-built holder: objects the consumer created cross inside
        // a buffer and come back as the same objects.
        Holder mine{Token(1), Token(2), {Token(3), Token(4)}};
        check(oracle::sum_holder(mine) == 10, "consumer-built holder sums");
        Token mine_primary = oracle::primary_of(mine);
        check(mine_primary.handle() == mine.primary.handle(), "consumer-built primary identity");
        Holder bare{Token(100), std::nullopt, {}};
        check(oracle::sum_holder(bare) == 100, "holder with no spare and an empty list");
        Holder mixed{t, holder.spare, {t, primary, without.primary}};
        check(oracle::sum_holder(mixed) == 5 + 11 + 5 + 10 + 0, "holder mixing shared tokens");
        check(oracle::same_primary(mixed, Holder{t_copy, std::nullopt, {}}),
              "same_primary through a consumer-built pair");

        // Moving out leaves an empty wrapper whose destructor is a no-op.
        Token moved = std::move(t_copy);
        check(t_copy.handle() == nullptr && moved.value() == 5, "Token move");
        Holder moved_holder = std::move(holder_copy);
        check(holder_copy.primary.handle() == nullptr && moved_holder.primary.value() == 10,
              "Holder move");

        // Many encode/decode cycles must neither leak nor double free: every
        // wrapper here is still valid at scope exit and releases exactly once.
        for (int i = 0; i < 1000; i++) {
            Holder h = oracle::make_holder(i, (i % 2) == 0);
            int64_t expected = i + (i % 2 == 0 ? i + 1 : 0) + (i + 2) + (i + 3) + (i + 4);
            check(oracle::sum_holder(h) == expected, "sum_holder in a loop");
            Token p = oracle::primary_of(h);
            check(p.handle() == h.primary.handle(), "primary_of in a loop");
        }
        check(t.value() == 5 && primary.value() == 10 && mine.many[1].value() == 4,
              "long-lived tokens are still alive after the loop");
    }

    std::printf("cpp/codec: OK\n");
    return 0;
}
