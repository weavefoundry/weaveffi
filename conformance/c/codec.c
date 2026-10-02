// Conformance consumer: codec sample, C target (ABI revision 3).
//
// A round-trip check of the value-buffer protocol between the producer's own
// codec and the generated `codec_buffer.h`: every fixed-width scalar,
// strings (non-ASCII and with interior NUL bytes), bytes, present and absent
// optionals, lists (nested and empty), string- and integer-keyed maps with
// record values, nested records, every rich-enum variant, lists of optionals
// and enums, and objects in a record field, an optional, and a list.
// Fixtures fetched from the producer are checked field by field (producer
// encodes, consumer decodes), handed back to `verify_*` (consumer encodes,
// producer decodes), and re-encoded through `roundtrip_*`, which must return
// the very same bytes; hand-built edge values (empty containers, 64-bit
// extremes, NaN, infinities, negative zero) take the same path. Malformed
// input is rejected on both sides. Ends by asserting the producer's leak
// counters are zero.

#include "harness.h"

#include <math.h>

#include "codec_buffer.h"

// ── comparisons ────────────────────────────────────────────────────────────

// Bitwise float equality, so NaN payloads and the sign of zero count.
static int f32_bits_eq(float a, float b) { return memcmp(&a, &b, 4) == 0; }
static int f64_bits_eq(double a, double b) { return memcmp(&a, &b, 8) == 0; }

static int str_eq(codec_str a, codec_str b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.ptr, b.ptr, a.len) == 0);
}

static int bytes_view_eq(codec_bytes a, codec_bytes b) {
    return a.len == b.len && (a.len == 0 || memcmp(a.ptr, b.ptr, a.len) == 0);
}

static int opt_str_eq(const codec_str* a, const codec_str* b) {
    return (a == NULL) == (b == NULL) && (a == NULL || str_eq(*a, *b));
}

static int scalars_eq(const codec_codec_Scalars* a, const codec_codec_Scalars* b) {
    return a->i8_value == b->i8_value && a->u8_value == b->u8_value &&
           a->i16_value == b->i16_value && a->u16_value == b->u16_value &&
           a->i32_value == b->i32_value && a->u32_value == b->u32_value &&
           a->i64_value == b->i64_value && a->u64_value == b->u64_value &&
           f32_bits_eq(a->f32_value, b->f32_value) && f64_bits_eq(a->f64_value, b->f64_value) &&
           a->flag == b->flag && a->color == b->color;
}

static int shape_eq(const codec_codec_Shape* a, const codec_codec_Shape* b) {
    if (a->tag != b->tag) return 0;
    switch (a->tag) {
    case codec_codec_Shape_Empty:
        return 1;
    case codec_codec_Shape_Circle:
        return f64_bits_eq(a->as.Circle.radius, b->as.Circle.radius);
    case codec_codec_Shape_Rect:
        return f32_bits_eq(a->as.Rect.width, b->as.Rect.width) &&
               f32_bits_eq(a->as.Rect.height, b->as.Rect.height);
    case codec_codec_Shape_Labeled:
        return str_eq(a->as.Labeled.label, b->as.Labeled.label) &&
               a->as.Labeled.count == b->as.Labeled.count;
    case codec_codec_Shape_Nested:
        return scalars_eq(&a->as.Nested.inner, &b->as.Nested.inner) &&
               opt_str_eq(a->as.Nested.note, b->as.Nested.note);
    default:
        return 0;
    }
}

static int composite_eq(const codec_codec_Composite* a, const codec_codec_Composite* b) {
    if (!str_eq(a->name, b->name) || !bytes_view_eq(a->blob, b->blob)) return 0;
    if ((a->some_i64 == NULL) != (b->some_i64 == NULL)) return 0;
    if (a->some_i64 && *a->some_i64 != *b->some_i64) return 0;
    if ((a->none_i64 == NULL) != (b->none_i64 == NULL)) return 0;
    if (a->none_i64 && *a->none_i64 != *b->none_i64) return 0;
    if (!opt_str_eq(a->some_text, b->some_text)) return 0;
    if (a->names.len != b->names.len) return 0;
    for (size_t i = 0; i < a->names.len; i++) {
        if (!str_eq(a->names.items[i], b->names.items[i])) return 0;
    }
    if (a->matrix.len != b->matrix.len) return 0;
    for (size_t i = 0; i < a->matrix.len; i++) {
        const codec_list_i32* x = &a->matrix.items[i];
        const codec_list_i32* y = &b->matrix.items[i];
        if (x->len != y->len) return 0;
        for (size_t j = 0; j < x->len; j++) {
            if (x->items[j] != y->items[j]) return 0;
        }
    }
    if (a->empty.len != b->empty.len) return 0;
    for (size_t i = 0; i < a->empty.len; i++) {
        if (!f64_bits_eq(a->empty.items[i], b->empty.items[i])) return 0;
    }
    if (a->by_name.len != b->by_name.len) return 0;
    for (size_t i = 0; i < a->by_name.len; i++) {
        if (!str_eq(a->by_name.keys[i], b->by_name.keys[i]) ||
            a->by_name.values[i] != b->by_name.values[i]) {
            return 0;
        }
    }
    if (a->by_id.len != b->by_id.len) return 0;
    for (size_t i = 0; i < a->by_id.len; i++) {
        if (a->by_id.keys[i] != b->by_id.keys[i] ||
            !scalars_eq(&a->by_id.values[i], &b->by_id.values[i])) {
            return 0;
        }
    }
    if (!scalars_eq(&a->scalars, &b->scalars) || !shape_eq(&a->shape, &b->shape)) return 0;
    if (a->shapes.len != b->shapes.len) return 0;
    for (size_t i = 0; i < a->shapes.len; i++) {
        if (!shape_eq(&a->shapes.items[i], &b->shapes.items[i])) return 0;
    }
    if ((a->maybe_shape == NULL) != (b->maybe_shape == NULL)) return 0;
    if (a->maybe_shape && !shape_eq(a->maybe_shape, b->maybe_shape)) return 0;
    if ((a->maybe_list == NULL) != (b->maybe_list == NULL)) return 0;
    if (a->maybe_list && !bytes_view_eq(*a->maybe_list, *b->maybe_list)) return 0;
    if (a->sparse.len != b->sparse.len) return 0;
    for (size_t i = 0; i < a->sparse.len; i++) {
        const bool* x = a->sparse.items[i];
        const bool* y = b->sparse.items[i];
        if ((x == NULL) != (y == NULL) || (x && *x != *y)) return 0;
    }
    if (a->colors.len != b->colors.len) return 0;
    for (size_t i = 0; i < a->colors.len; i++) {
        if (a->colors.items[i] != b->colors.items[i]) return 0;
    }
    return 1;
}

// The canonical fixture the producer hands out and expects back.
static const codec_codec_Scalars CANONICAL_SCALARS = {
    -8, 200, -16000, 60000, -2000000000, 4000000000u, -9007199254740993LL, UINT64_MAX,
    1.5f, -2.25e100, true, codec_codec_Color_Blue,
};

// ── ABI helpers ────────────────────────────────────────────────────────────

// A function taking one buffered value and returning one buffered value.
typedef const uint8_t* (*buffer_fn)(const uint8_t*, size_t, size_t*, codec_error*);

// Call `fn` with the bytes in `w` and return its owned result, asserting it
// succeeded and (when `exact`) echoed the input byte for byte. The caller
// decodes `*out_len` bytes and releases them with codec_free_bytes.
static const uint8_t* call_buffer(buffer_fn fn, codec_writer* w, size_t* out_len, int exact) {
    assert(!w->failed);
    codec_error err = {0};
    const uint8_t* p = fn(w->ptr, w->len, out_len, &err);
    assert(err.code == 0 && p != NULL);
    if (exact) assert(*out_len == w->len && memcmp(p, w->ptr, w->len) == 0);
    codec_writer_free(w);
    return p;
}

#define ENCODE(write, value, w)    \
    codec_writer w;                \
    memset(&w, 0, sizeof w);       \
    write(&w, value)

// Decode an owned buffer with `decode`, then release it.
#define TAKE(decode, p, len, out)                   \
    do {                                            \
        assert(decode(p, len, out));                \
        codec_free_bytes((uint8_t*)(p), len);       \
    } while (0)

static void fetch_scalars(codec_codec_Scalars* out) {
    codec_error err = {0};
    size_t len = 0;
    const uint8_t* p = codec_codec_sample_scalars(&len, &err);
    assert(err.code == 0);
    TAKE(codec_codec_Scalars_decode, p, len, out);
}

static void fetch_composite(codec_codec_Composite* out) {
    codec_error err = {0};
    size_t len = 0;
    const uint8_t* p = codec_codec_sample_composite(&len, &err);
    assert(err.code == 0);
    TAKE(codec_codec_Composite_decode, p, len, out);
}

static void roundtrip_scalars(const codec_codec_Scalars* in, codec_codec_Scalars* out) {
    ENCODE(codec_codec_Scalars_write, in, w);
    size_t len = 0;
    const uint8_t* p = call_buffer(codec_codec_roundtrip_scalars, &w, &len, 1);
    TAKE(codec_codec_Scalars_decode, p, len, out);
}

static void roundtrip_composite(const codec_codec_Composite* in, codec_codec_Composite* out) {
    ENCODE(codec_codec_Composite_write, in, w);
    size_t len = 0;
    const uint8_t* p = call_buffer(codec_codec_roundtrip_composite, &w, &len, 1);
    TAKE(codec_codec_Composite_decode, p, len, out);
}

static void roundtrip_shape(const codec_codec_Shape* in, codec_codec_Shape* out) {
    ENCODE(codec_codec_Shape_write, in, w);
    size_t len = 0;
    const uint8_t* p = call_buffer(codec_codec_roundtrip_shape, &w, &len, 1);
    TAKE(codec_codec_Shape_decode, p, len, out);
}

static int verify_scalars(const codec_codec_Scalars* s, codec_error* err) {
    ENCODE(codec_codec_Scalars_write, s, w);
    int ok = codec_codec_verify_scalars(w.ptr, w.len, err);
    codec_writer_free(&w);
    return ok;
}

static int verify_composite(const codec_codec_Composite* c, codec_error* err) {
    ENCODE(codec_codec_Composite_write, c, w);
    int ok = codec_codec_verify_composite(w.ptr, w.len, err);
    codec_writer_free(&w);
    return ok;
}

// describe_* return an owned (ptr, len) string; copy it out NUL-terminated.
static char* take_string(const uint8_t* p, size_t len) {
    char* s = (char*)calloc(len + 1, 1);
    assert(s != NULL);
    if (len) memcpy(s, p, len);
    codec_free_bytes((uint8_t*)p, len);
    return s;
}

static char* describe_shape(const codec_codec_Shape* s) {
    ENCODE(codec_codec_Shape_write, s, w);
    codec_error err = {0};
    size_t len = 0;
    const uint8_t* p = codec_codec_describe_shape(w.ptr, w.len, &len, &err);
    codec_writer_free(&w);
    assert(err.code == 0);
    return take_string(p, len);
}

static codec_codec_Shape shape_of(codec_codec_Shape_Tag tag) {
    codec_codec_Shape s;
    memset(&s, 0, sizeof s);
    s.tag = tag;
    return s;
}

// ── sections ───────────────────────────────────────────────────────────────

static void scalars(void) {
    codec_error err = {0};

    // Producer encodes, consumer decodes.
    codec_codec_Scalars s;
    fetch_scalars(&s);
    assert(s.i8_value == -8);
    assert(s.u8_value == 200);
    assert(s.i16_value == -16000);
    assert(s.u16_value == 60000);
    assert(s.i32_value == -2000000000);
    assert(s.u32_value == 4000000000u);
    assert(s.i64_value == -9007199254740993LL);
    assert(s.u64_value == UINT64_MAX);
    assert(s.f32_value == 1.5f);
    assert(s.f64_value == -2.25e100);
    assert(s.flag);
    assert(s.color == codec_codec_Color_Blue && s.color == 7);
    assert(scalars_eq(&s, &CANONICAL_SCALARS));

    // Consumer encodes, producer decodes and compares to its canonical value.
    assert(verify_scalars(&s, &err) && err.code == 0);

    // Consumer encodes, producer re-encodes the same bytes.
    codec_codec_Scalars s2;
    roundtrip_scalars(&s, &s2);
    assert(scalars_eq(&s, &s2));

    // A one-field change is a Mismatch (code 1) with the Display message.
    s2.flag = false;
    assert(!verify_scalars(&s2, &err));
    assert(err.code == codec_codec_CodecError_Mismatch && err.code == 1);
    assert(err.message != NULL &&
           strcmp(err.message, "value does not match the canonical fixture") == 0);
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    codec_error_clear(&err);

    // Hand-built edge scalars: extremes, NaN, infinities, negative zero.
    codec_codec_Scalars edge = {
        INT8_MIN, UINT8_MAX, INT16_MIN, UINT16_MAX, INT32_MIN, UINT32_MAX,
        INT64_MIN, UINT64_MAX, NAN, -0.0, false, codec_codec_Color_Red,
    };
    roundtrip_scalars(&edge, &s2);
    assert(scalars_eq(&edge, &s2));
    assert(isnan(s2.f32_value) && signbit(s2.f64_value) && s2.f64_value == 0.0);
    edge.i8_value = INT8_MAX;
    edge.i16_value = INT16_MAX;
    edge.i32_value = INT32_MAX;
    edge.i64_value = INT64_MAX;
    edge.u8_value = 0;
    edge.u16_value = 0;
    edge.u32_value = 0;
    edge.u64_value = 0;
    edge.f32_value = -INFINITY;
    edge.f64_value = INFINITY;
    edge.color = codec_codec_Color_Green;
    roundtrip_scalars(&edge, &s2);
    assert(scalars_eq(&edge, &s2));
    assert(isinf(s2.f32_value) && s2.f32_value < 0 && isinf(s2.f64_value) && s2.f64_value > 0);
}

static void composite(void) {
    codec_error err = {0};

    // Producer encodes, consumer decodes.
    codec_codec_Composite c;
    fetch_composite(&c);
    assert(strcmp(c.name.ptr, "h\xC3\xA9llo w\xC3\xB6rld \xE2\x9C\x93") == 0);
    const uint8_t blob[6] = {0, 1, 2, 253, 254, 255};
    assert(c.blob.len == 6 && memcmp(c.blob.ptr, blob, 6) == 0);
    assert(c.some_i64 != NULL && *c.some_i64 == INT64_MIN);
    assert(c.none_i64 == NULL);
    assert(c.some_text != NULL && c.some_text->len == 0 && c.some_text->ptr[0] == '\0');
    assert(c.names.len == 3 && strcmp(c.names.items[0].ptr, "a") == 0 &&
           c.names.items[1].len == 0 && strcmp(c.names.items[2].ptr, "ccc") == 0);
    assert(c.matrix.len == 3);
    assert(c.matrix.items[0].len == 3 && c.matrix.items[0].items[0] == 1 &&
           c.matrix.items[0].items[1] == 2 && c.matrix.items[0].items[2] == 3);
    assert(c.matrix.items[1].len == 0 && c.matrix.items[1].items == NULL);
    assert(c.matrix.items[2].len == 1 && c.matrix.items[2].items[0] == -4);
    assert(c.empty.len == 0);
    // BTreeMap keys arrive sorted.
    assert(c.by_name.len == 3);
    assert(strcmp(c.by_name.keys[0].ptr, "neg") == 0 && c.by_name.values[0] == -3);
    assert(strcmp(c.by_name.keys[1].ptr, "one") == 0 && c.by_name.values[1] == 1);
    assert(strcmp(c.by_name.keys[2].ptr, "two") == 0 && c.by_name.values[2] == 2);
    assert(c.by_id.len == 2);
    assert(c.by_id.keys[0] == -1 && scalars_eq(&c.by_id.values[0], &CANONICAL_SCALARS));
    assert(c.by_id.keys[1] == 42 && !c.by_id.values[1].flag &&
           c.by_id.values[1].u64_value == UINT64_MAX);
    assert(scalars_eq(&c.scalars, &CANONICAL_SCALARS));
    assert(c.shape.tag == codec_codec_Shape_Labeled &&
           strcmp(c.shape.as.Labeled.label.ptr, "tag") == 0 && c.shape.as.Labeled.count == 3);
    assert(c.shapes.len == 5);
    const codec_codec_Shape* sh = c.shapes.items;
    assert(sh[0].tag == codec_codec_Shape_Empty);
    assert(sh[1].tag == codec_codec_Shape_Circle && sh[1].as.Circle.radius == 2.5);
    assert(sh[2].tag == codec_codec_Shape_Rect && sh[2].as.Rect.width == 1.0f &&
           sh[2].as.Rect.height == 0.5f);
    assert(sh[3].tag == codec_codec_Shape_Labeled && sh[3].as.Labeled.label.len == 0 &&
           sh[3].as.Labeled.count == -1);
    assert(sh[4].tag == codec_codec_Shape_Nested &&
           scalars_eq(&sh[4].as.Nested.inner, &CANONICAL_SCALARS) &&
           sh[4].as.Nested.note != NULL && strcmp(sh[4].as.Nested.note->ptr, "n") == 0);
    assert(c.maybe_shape != NULL && c.maybe_shape->tag == codec_codec_Shape_Nested &&
           c.maybe_shape->as.Nested.note == NULL &&
           scalars_eq(&c.maybe_shape->as.Nested.inner, &CANONICAL_SCALARS));
    assert(c.maybe_list != NULL && c.maybe_list->len == 2 && c.maybe_list->ptr[0] == 9 &&
           c.maybe_list->ptr[1] == 8);
    assert(c.sparse.len == 3);
    assert(c.sparse.items[0] != NULL && *c.sparse.items[0] == true);
    assert(c.sparse.items[1] == NULL);
    assert(c.sparse.items[2] != NULL && *c.sparse.items[2] == false);
    assert(c.colors.len == 3 && c.colors.items[0] == codec_codec_Color_Red &&
           c.colors.items[1] == codec_codec_Color_Green &&
           c.colors.items[2] == codec_codec_Color_Blue);

    // Consumer encodes, producer decodes.
    assert(verify_composite(&c, &err) && err.code == 0);

    // Consumer encodes, producer re-encodes the same bytes, consumer decodes.
    codec_codec_Composite c2;
    roundtrip_composite(&c, &c2);
    assert(composite_eq(&c, &c2));

    // Perturb one deeply nested field: Mismatch.
    *c2.sparse.items[2] = true;
    assert(!verify_composite(&c2, &err));
    assert(err.code == codec_codec_CodecError_Mismatch);
    codec_error_clear(&err);
    codec_codec_Composite_free(&c2);

    // describe_composite renders what the producer saw.
    {
        ENCODE(codec_codec_Composite_write, &c, w);
        size_t len = 0;
        const uint8_t* p = codec_codec_describe_composite(w.ptr, w.len, &len, &err);
        codec_writer_free(&w);
        assert(err.code == 0);
        char* text = take_string(p, len);
        assert(strstr(text, "name: \"h\xC3\xA9llo w\xC3\xB6rld \xE2\x9C\x93\"") != NULL);
        assert(strstr(text, "some_i64: Some(-9223372036854775808)") != NULL);
        assert(strstr(text, "shape: Labeled { label: \"tag\", count: 3 }") != NULL);
        free(text);
    }
    codec_codec_Composite_free(&c);
    assert(c.names.items == NULL && c.some_i64 == NULL && "free zeroes the value");
}

static void edge_composite(void) {
    codec_error err = {0};

    // Hand-built edge composite borrowing caller memory: everything empty or
    // absent, a name with interior NUL bytes, maps with extreme keys, a list
    // of absent optionals, NaN and negative zero.
    static const char name[] = "\xE6\x97\xA5\xE6\x9C\xAC\0nul\0 \xF0\x9F\x8E\x89";
    int64_t some = INT64_MAX;
    int64_t none = -1;
    int32_t row[2] = {INT32_MIN, INT32_MAX};
    codec_list_i32 rows[1] = {{row, 2}};
    double floats[2] = {-0.0, NAN};
    codec_str keys[1] = {{"", 0}};
    int64_t values[1] = {INT64_MIN};
    codec_bytes empty_bytes = {NULL, 0};
    bool* sparse[2] = {NULL, NULL};

    codec_codec_Composite e;
    memset(&e, 0, sizeof e);
    e.name.ptr = name;
    e.name.len = sizeof name - 1;
    e.some_i64 = &some;
    e.none_i64 = &none;
    e.matrix.items = rows;
    e.matrix.len = 1;
    e.empty.items = floats;
    e.empty.len = 2;
    e.by_name.keys = keys;
    e.by_name.values = values;
    e.by_name.len = 1;
    e.scalars = CANONICAL_SCALARS;
    e.shape = shape_of(codec_codec_Shape_Empty);
    e.maybe_list = &empty_bytes;
    e.sparse.items = sparse;
    e.sparse.len = 2;

    codec_codec_Composite e2;
    roundtrip_composite(&e, &e2);
    assert(composite_eq(&e, &e2));
    assert(e2.name.len == sizeof name - 1 && memcmp(e2.name.ptr, name, sizeof name - 1) == 0);
    assert(e2.blob.len == 0 && e2.maybe_list != NULL && e2.maybe_list->len == 0);
    assert(isnan(e2.empty.items[1]) && signbit(e2.empty.items[0]));
    assert(e2.sparse.len == 2 && e2.sparse.items[0] == NULL && e2.sparse.items[1] == NULL);
    assert(!verify_composite(&e, &err) && err.code == 1);
    codec_error_clear(&err);
    codec_codec_Composite_free(&e2);
}

static void shapes(void) {
    codec_error err = {0};
    codec_str n = codec_str_of("n");
    codec_codec_Shape v[5];
    v[0] = shape_of(codec_codec_Shape_Empty);
    v[1] = shape_of(codec_codec_Shape_Circle);
    v[1].as.Circle.radius = 2.5;
    v[2] = shape_of(codec_codec_Shape_Rect);
    v[2].as.Rect.width = 1.0f;
    v[2].as.Rect.height = 0.5f;
    v[3] = shape_of(codec_codec_Shape_Labeled);
    v[3].as.Labeled.label = codec_str_of("tag");
    v[3].as.Labeled.count = 3;
    v[4] = shape_of(codec_codec_Shape_Nested);
    v[4].as.Nested.inner = CANONICAL_SCALARS;
    v[4].as.Nested.note = &n;
    const char* descriptions[5] = {
        "Empty",
        "Circle { radius: 2.5 }",
        "Rect { width: 1.0, height: 0.5 }",
        "Labeled { label: \"tag\", count: 3 }",
        NULL,
    };
    for (int i = 0; i < 5; i++) {
        codec_codec_Shape back;
        roundtrip_shape(&v[i], &back);
        assert(shape_eq(&v[i], &back));
        char* text = describe_shape(&back);
        if (descriptions[i]) {
            assert(strcmp(text, descriptions[i]) == 0);
        } else {
            assert(strstr(text, "Nested { inner: Scalars { i8_value: -8,") != NULL &&
                   strstr(text, "note: Some(\"n\") }") != NULL);
        }
        free(text);
        codec_codec_Shape_free(&back);
    }

    codec_codec_Shape no_note = shape_of(codec_codec_Shape_Nested);
    no_note.as.Nested.inner = CANONICAL_SCALARS;
    codec_codec_Shape back;
    roundtrip_shape(&no_note, &back);
    assert(shape_eq(&no_note, &back) && back.as.Nested.note == NULL);
    codec_codec_Shape_free(&back);

    codec_codec_Shape low = shape_of(codec_codec_Shape_Labeled);
    low.as.Labeled.count = INT32_MIN;
    roundtrip_shape(&low, &back);
    assert(shape_eq(&low, &back));
    codec_codec_Shape_free(&back);

    // roundtrip_shapes: a list of every variant, then an empty list.
    codec_list_codec_Shape list;
    list.items = v;
    list.len = 5;
    ENCODE(codec_list_codec_Shape_write, &list, w);
    size_t len = 0;
    const uint8_t* p = call_buffer(codec_codec_roundtrip_shapes, &w, &len, 1);
    codec_list_codec_Shape out;
    TAKE(codec_list_codec_Shape_decode, p, len, &out);
    assert(out.len == 5);
    for (int i = 0; i < 5; i++) assert(shape_eq(&v[i], &out.items[i]));
    codec_list_codec_Shape_free(&out);
    list.len = 0;
    ENCODE(codec_list_codec_Shape_write, &list, w2);
    p = call_buffer(codec_codec_roundtrip_shapes, &w2, &len, 1);
    TAKE(codec_list_codec_Shape_decode, p, len, &out);
    assert(out.len == 0 && out.items == NULL);

    // The producer rejects malformed buffers as marshalling failures (-3):
    // truncated mid-value, an unknown tag, and trailing bytes.
    const uint8_t truncated[3] = {1, 0, 0};
    assert(codec_codec_roundtrip_shape(BYTES(truncated), &len, &err) == NULL);
    assert(err.code == -3);
    codec_error_clear(&err);
    const uint8_t bad_tag[4] = {99, 0, 0, 0};
    assert(codec_codec_roundtrip_shape(BYTES(bad_tag), &len, &err) == NULL);
    assert(err.code == -3);
    codec_error_clear(&err);
    const uint8_t trailing[5] = {0, 0, 0, 0, 0};
    assert(codec_codec_roundtrip_shape(BYTES(trailing), &len, &err) == NULL);
    assert(err.code == -3);
    codec_error_clear(&err);

    // The consumer's decoder rejects them too, leaving nothing allocated.
    assert(!codec_codec_Shape_decode(BYTES(truncated), &back));
    assert(!codec_codec_Shape_decode(BYTES(bad_tag), &back));
    assert(!codec_codec_Shape_decode(BYTES(trailing), &back));
    const uint8_t bad_bool[2] = {1, 2};
    bool* flag = NULL;
    assert(!codec_opt_bool_decode(BYTES(bad_bool), &flag) && flag == NULL);
}

static void standalone(void) {
    codec_error err = {0};
    size_t len = 0;

    // Standalone optional: present, then absent.
    int64_t minus_one = -1;
    ENCODE(codec_opt_i64_write, &minus_one, w);
    const uint8_t* p = call_buffer(codec_codec_roundtrip_opt_i64, &w, &len, 1);
    int64_t* opt = NULL;
    TAKE(codec_opt_i64_decode, p, len, &opt);
    assert(opt != NULL && *opt == -1);
    codec_opt_i64_free(&opt);
    ENCODE(codec_opt_i64_write, NULL, w2);
    p = call_buffer(codec_codec_roundtrip_opt_i64, &w2, &len, 1);
    TAKE(codec_opt_i64_decode, p, len, &opt);
    assert(opt == NULL);

    // Map: encoded out of key order, comes back sorted by the BTreeMap.
    codec_str keys[3] = {codec_str_of("b"), codec_str_of("a"), codec_str_of("")};
    int64_t values[3] = {-2, 1, INT64_MAX};
    codec_map_string_i64 map;
    map.keys = keys;
    map.values = values;
    map.len = 3;
    ENCODE(codec_map_string_i64_write, &map, w3);
    p = call_buffer(codec_codec_roundtrip_map, &w3, &len, 0);
    codec_map_string_i64 sorted;
    TAKE(codec_map_string_i64_decode, p, len, &sorted);
    assert(sorted.len == 3);
    assert(sorted.keys[0].len == 0 && sorted.values[0] == INT64_MAX);
    assert(strcmp(sorted.keys[1].ptr, "a") == 0 && sorted.values[1] == 1);
    assert(strcmp(sorted.keys[2].ptr, "b") == 0 && sorted.values[2] == -2);
    codec_map_string_i64_free(&sorted);
    map.len = 0;
    ENCODE(codec_map_string_i64_write, &map, w4);
    p = call_buffer(codec_codec_roundtrip_map, &w4, &len, 1);
    TAKE(codec_map_string_i64_decode, p, len, &sorted);
    assert(sorted.len == 0);

    // Strings cross as (ptr, len) in both directions, interior NULs included.
    static const char text[] = "h\xC3\xA9llo \0w\xC3\xB6rld\0";
    p = codec_codec_roundtrip_string((const uint8_t*)text, sizeof text - 1, &len, &err);
    assert(err.code == 0 && len == sizeof text - 1 && memcmp(p, text, len) == 0);
    codec_free_bytes((uint8_t*)p, len);
    p = codec_codec_roundtrip_string(NULL, 0, &len, &err);
    assert(err.code == 0 && len == 0);
    codec_free_bytes((uint8_t*)p, len);
    // A NULL pointer with a length, and invalid UTF-8, are marshalling
    // failures.
    assert(codec_codec_roundtrip_string(NULL, 2, &len, &err) == NULL && err.code == -3);
    codec_error_clear(&err);
    const uint8_t bad_utf8[3] = {'a', 0xFF, 'b'};
    assert(codec_codec_roundtrip_string(BYTES(bad_utf8), &len, &err) == NULL && err.code == -3);
    codec_error_clear(&err);

    // Bytes, including the empty run.
    const uint8_t raw[4] = {0, 255, 0, 128};
    p = codec_codec_roundtrip_bytes(BYTES(raw), &len, &err);
    assert(err.code == 0 && len == 4 && memcmp(p, raw, 4) == 0);
    codec_free_bytes((uint8_t*)p, len);
    p = codec_codec_roundtrip_bytes(raw, 0, &len, &err);
    assert(err.code == 0 && len == 0);
    codec_free_bytes((uint8_t*)p, len);

    // Direct family.
    assert(codec_codec_roundtrip_i64(INT64_MIN, &err) == INT64_MIN);
    assert(codec_codec_roundtrip_i64(INT64_MAX, &err) == INT64_MAX);
    assert(codec_codec_roundtrip_u64(UINT64_MAX, &err) == UINT64_MAX);
    assert(codec_codec_roundtrip_u64(1ULL << 63, &err) == (1ULL << 63));
    assert(isnan(codec_codec_roundtrip_f64(NAN, &err)));
    assert(signbit(codec_codec_roundtrip_f64(-0.0, &err)));
    assert(codec_codec_roundtrip_f64(INFINITY, &err) == INFINITY);
    assert(codec_codec_roundtrip_f64(-2.25e100, &err) == -2.25e100);
    assert(codec_codec_roundtrip_bool(true, &err) == true);
    assert(codec_codec_roundtrip_bool(false, &err) == false);
    assert(codec_codec_roundtrip_color(codec_codec_Color_Blue, &err) == codec_codec_Color_Blue);
    assert(codec_codec_roundtrip_color(codec_codec_Color_Red, &err) == 0);
    assert(err.code == 0);
    codec_codec_roundtrip_color((codec_codec_Color)3, &err);
    assert(err.code == -3 && "an undeclared discriminant is a marshalling failure");
    codec_error_clear(&err);
}

static int64_t token_value(const codec_codec_Token* t) {
    codec_error err = {0};
    int64_t v = codec_codec_Token_value(t, &err);
    assert(err.code == 0);
    return v;
}

static codec_codec_Holder make_holder(int64_t base, bool with_spare) {
    codec_error err = {0};
    size_t len = 0;
    const uint8_t* p = codec_codec_make_holder(base, with_spare, &len, &err);
    assert(err.code == 0);
    codec_codec_Holder h;
    TAKE(codec_codec_Holder_decode, p, len, &h);  // adopts every token
    return h;
}

static void objects(void) {
    codec_error err = {0};

    codec_codec_Token* t = codec_codec_Token_new(5, &err);
    assert(err.code == 0 && t != NULL);
    assert(token_value(t) == 5);
    codec_codec_Token* t2 = codec_codec_Token_clone(t);
    assert(t2 == t);
    codec_codec_Token_destroy(t);
    assert(token_value(t2) == 5 && "clone survives destroying the original");
    codec_codec_Token_destroy(t2);
    codec_codec_Token_destroy(NULL);
    assert(codec_codec_Token_clone(NULL) == NULL);

    codec_codec_Holder h = make_holder(10, true);
    assert(token_value(h.primary) == 10);
    assert(h.spare != NULL && token_value(h.spare) == 11);
    assert(h.many.len == 3);
    assert(token_value(h.many.items[0]) == 12 && token_value(h.many.items[1]) == 13 &&
           token_value(h.many.items[2]) == 14);
    assert(h.primary != h.spare && h.primary != h.many.items[0]);

    // sum_holder consumes one encoding; the writer stored a fresh reference
    // per token, so ours stay intact.
    ENCODE(codec_codec_Holder_write, &h, w);
    assert(codec_codec_sum_holder(w.ptr, w.len, &err) == 10 + 11 + 12 + 13 + 14);
    assert(err.code == 0);
    codec_writer_free(&w);
    assert(token_value(h.primary) == 10 && "our references are intact");

    // primary_of returns the very same object as holder.primary, as an owned
    // reference we release.
    ENCODE(codec_codec_Holder_write, &h, w2);
    codec_codec_Token* primary = codec_codec_primary_of(w2.ptr, w2.len, &err);
    codec_writer_free(&w2);
    assert(err.code == 0 && primary == h.primary);
    codec_codec_Token_destroy(primary);
    assert(token_value(h.primary) == 10);

    // same_primary: two encodings of one holder share a primary; a different
    // holder does not.
    codec_codec_Holder h2 = make_holder(100, false);
    assert(h2.spare == NULL && h2.many.len == 3 && token_value(h2.primary) == 100);
    ENCODE(codec_codec_Holder_write, &h, a);
    ENCODE(codec_codec_Holder_write, &h, b);
    assert(codec_codec_same_primary(a.ptr, a.len, b.ptr, b.len, &err));
    assert(err.code == 0);
    codec_writer_free(&a);
    codec_writer_free(&b);
    ENCODE(codec_codec_Holder_write, &h, a2);
    ENCODE(codec_codec_Holder_write, &h2, b2);
    assert(!codec_codec_same_primary(a2.ptr, a2.len, b2.ptr, b2.len, &err));
    assert(err.code == 0);
    codec_writer_free(&a2);
    codec_writer_free(&b2);

    // A holder mixing our own tokens with the producer's, built from
    // borrowed pointers.
    codec_codec_Token* mine = codec_codec_Token_new(1000, &err);
    codec_codec_Token* neg = codec_codec_Token_new(-1, &err);
    codec_codec_Token* many[2] = {h2.primary, neg};
    codec_codec_Holder mixed;
    mixed.primary = mine;
    mixed.spare = h.primary;
    mixed.many.items = many;
    mixed.many.len = 2;
    ENCODE(codec_codec_Holder_write, &mixed, m1);
    assert(codec_codec_sum_holder(m1.ptr, m1.len, &err) == 1000 + 10 + 100 - 1);
    codec_writer_free(&m1);
    ENCODE(codec_codec_Holder_write, &mixed, m2);
    codec_codec_Token* p = codec_codec_primary_of(m2.ptr, m2.len, &err);
    codec_writer_free(&m2);
    assert(p == mine && token_value(p) == 1000);
    codec_codec_Token_destroy(p);
    codec_codec_Token_destroy(mine);
    codec_codec_Token_destroy(neg);
    assert(token_value(h.primary) == 10 && token_value(h2.primary) == 100 &&
           "the references written into `mixed` never touched ours");

    // A zero token in a required position is a marshalling failure.
    const uint8_t zero_token[13] = {0};
    assert(codec_codec_sum_holder(BYTES(zero_token), &err) == 0 && err.code == -3);
    codec_error_clear(&err);

    codec_codec_Holder_free(&h2);
    codec_codec_Holder_free(&h);
    assert(h.primary == NULL && h.many.items == NULL);
}

int main(void) {
    assert(CODEC_ABI_VERSION == 3u && codec_abi_version() == CODEC_ABI_VERSION);
    assert(codec_codec_checksum() == CODEC_CODEC_CHECKSUM);

    scalars();
    composite();
    edge_composite();
    shapes();
    standalone();
    objects();

    ASSERT_NO_LEAKS(codec_debug_live);
    printf("c/codec: OK\n");
    return 0;
}
