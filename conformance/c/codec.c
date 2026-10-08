// Conformance consumer: codec sample, C target (ABI revision 4).
//
// The shared-vector loop: for every vector the producer serves, decode it
// with the generated `codec_buffer.h`, re-encode it (which must reproduce the
// producer's bytes exactly, objects aside), pass it back to `check_vector`,
// and push each primitive vector's value through the matching direct-family
// `echo_*`. Then vectors built from literals (so a symmetric encode/decode
// bug can't hide), spot checks of decoded fields, the typed out-of-range
// error and its payload, malformed buffers (truncated, unknown tag, trailing
// bytes, a bad bool, a duplicate map key) rejected as marshalling failures,
// and object identity and reference counting through buffers. Ends by
// asserting the producer's leak counters are zero.

#include "harness.h"

#include <math.h>

#include "codec_buffer.h"

static int f32_bits_eq(float a, float b) { return memcmp(&a, &b, 4) == 0; }
static int f64_bits_eq(double a, double b) { return memcmp(&a, &b, 8) == 0; }

static int str_is(codec_str s, const char* expected, size_t n) {
    return s.len == n && (n == 0 || memcmp(s.ptr, expected, n) == 0);
}

// Fetch vector `i`, keeping the producer's encoding in `*raw` (owned).
static codec_codec_Vector fetch(uint32_t i, uint8_t** raw, size_t* raw_len) {
    codec_error err = {0};
    size_t len = 0;
    const uint8_t* buf = codec_codec_vector(i, &len, &err);
    assert(err.code == 0 && buf != NULL);
    codec_codec_Vector v;
    assert(codec_codec_Vector_decode(buf, len, &v));
    if (raw != NULL) {
        *raw = (uint8_t*)malloc(len);
        memcpy(*raw, buf, len);
        *raw_len = len;
    }
    codec_free_bytes((uint8_t*)buf, len);
    return v;
}

static bool check(uint32_t i, const codec_codec_Vector* v) {
    codec_error err = {0};
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_codec_Vector_write(&w, v);
    assert(!w.failed);
    bool ok = codec_codec_check_vector(i, w.ptr, w.len, &err);
    assert(err.code == 0);
    codec_writer_free(&w);
    return ok;
}

// An owned, NUL-terminated copy of a returned string.
static char* take_string(const uint8_t* ptr, size_t len) {
    char* s = (char*)calloc(len + 1, 1);
    if (len > 0) memcpy(s, ptr, len);
    codec_free_bytes((uint8_t*)ptr, len);
    return s;
}

static char* vector_name(uint32_t i) {
    codec_error err = {0};
    size_t len = 0;
    const uint8_t* p = codec_codec_vector_name(i, &len, &err);
    assert(err.code == 0);
    return take_string(p, len);
}

static char* describe(const codec_codec_Vector* v) {
    codec_error err = {0};
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_codec_Vector_write(&w, v);
    size_t len = 0;
    const uint8_t* p = codec_codec_describe_vector(w.ptr, w.len, &len, &err);
    codec_writer_free(&w);
    assert(err.code == 0);
    return take_string(p, len);
}

static uint32_t find(uint32_t n, const char* name) {
    for (uint32_t i = 0; i < n; i++) {
        char* s = vector_name(i);
        int hit = strcmp(s, name) == 0;
        free(s);
        if (hit) return i;
    }
    fprintf(stderr, "no vector named %s\n", name);
    abort();
}

// Push a primitive vector's value through its direct-family echo.
static void echo(const codec_codec_Vector* v) {
    codec_error err = {0};
    size_t len = 0;
    switch (v->tag) {
    case codec_codec_Vector_I8:
        assert(codec_codec_echo_i8(v->as.I8.value, &err) == v->as.I8.value);
        break;
    case codec_codec_Vector_U8:
        assert(codec_codec_echo_u8(v->as.U8.value, &err) == v->as.U8.value);
        break;
    case codec_codec_Vector_I16:
        assert(codec_codec_echo_i16(v->as.I16.value, &err) == v->as.I16.value);
        break;
    case codec_codec_Vector_U16:
        assert(codec_codec_echo_u16(v->as.U16.value, &err) == v->as.U16.value);
        break;
    case codec_codec_Vector_I32:
        assert(codec_codec_echo_i32(v->as.I32.value, &err) == v->as.I32.value);
        break;
    case codec_codec_Vector_U32:
        assert(codec_codec_echo_u32(v->as.U32.value, &err) == v->as.U32.value);
        break;
    case codec_codec_Vector_I64:
        assert(codec_codec_echo_i64(v->as.I64.value, &err) == v->as.I64.value);
        break;
    case codec_codec_Vector_U64:
        assert(codec_codec_echo_u64(v->as.U64.value, &err) == v->as.U64.value);
        break;
    case codec_codec_Vector_F32:
        assert(f32_bits_eq(codec_codec_echo_f32(v->as.F32.value, &err), v->as.F32.value));
        break;
    case codec_codec_Vector_F64:
        assert(f64_bits_eq(codec_codec_echo_f64(v->as.F64.value, &err), v->as.F64.value));
        break;
    case codec_codec_Vector_Flag:
        assert(codec_codec_echo_bool(v->as.Flag.value, &err) == v->as.Flag.value);
        break;
    case codec_codec_Vector_Hue:
        assert(codec_codec_echo_color(v->as.Hue.value, &err) == v->as.Hue.value);
        break;
    case codec_codec_Vector_Text: {
        codec_str s = v->as.Text.value;
        const uint8_t* out = codec_codec_echo_text((const uint8_t*)s.ptr, s.len, &len, &err);
        assert(len == s.len && (len == 0 || memcmp(out, s.ptr, len) == 0));
        codec_free_bytes((uint8_t*)out, len);
        break;
    }
    case codec_codec_Vector_Blob: {
        codec_bytes b = v->as.Blob.value;
        const uint8_t* out = codec_codec_echo_blob(b.ptr, b.len, &len, &err);
        assert(len == b.len && (len == 0 || memcmp(out, b.ptr, len) == 0));
        codec_free_bytes((uint8_t*)out, len);
        break;
    }
    default:
        break;
    }
    assert(err.code == 0);
}

static void every_vector(uint32_t n) {
    for (uint32_t i = 0; i < n; i++) {
        uint8_t* raw = NULL;
        size_t raw_len = 0;
        codec_codec_Vector v = fetch(i, &raw, &raw_len);
        if (!check(i, &v)) {
            char* name = vector_name(i);
            char* seen = describe(&v);
            fprintf(stderr, "vector %u (%s) did not round-trip; producer saw %s\n", i, name,
                    seen);
            abort();
        }
        assert(!check((i + 1) % n, &v) && "a vector never matches its neighbor");
        // The generated encoder is deterministic, so re-encoding reproduces
        // the producer's bytes, except for object tokens (fresh references).
        if (v.tag != codec_codec_Vector_Objects) {
            codec_writer w;
            memset(&w, 0, sizeof w);
            codec_codec_Vector_write(&w, &v);
            assert(w.len == raw_len && memcmp(w.ptr, raw, raw_len) == 0);
            codec_writer_free(&w);
        }
        echo(&v);
        free(raw);
        codec_codec_Vector_free(&v);
    }
}

static codec_codec_Scalars canonical_scalars(void) {
    codec_codec_Scalars s;
    s.i8_value = -8;
    s.u8_value = 200;
    s.i16_value = -16000;
    s.u16_value = 60000;
    s.i32_value = -2000000000;
    s.u32_value = 4000000000u;
    s.i64_value = -9007199254740993LL;
    s.u64_value = UINT64_MAX;
    s.f32_value = 1.5f;
    s.f64_value = -2.25e100;
    s.flag = true;
    s.color = codec_codec_Color_Blue;
    return s;
}

static void literal_vectors(uint32_t n) {
    codec_codec_Vector v;

    v.tag = codec_codec_Vector_AllScalars;
    v.as.AllScalars.value = canonical_scalars();
    assert(check(find(n, "scalars canonical"), &v));
    v.as.AllScalars.value.u16_value = 60001;
    assert(!check(find(n, "scalars canonical"), &v));

    v.tag = codec_codec_Vector_Figure;
    v.as.Figure.value.tag = codec_codec_Shape_Labeled;
    v.as.Figure.value.as.Labeled.label = codec_str_of("tag");
    v.as.Figure.value.as.Labeled.count = 3;
    assert(check(find(n, "shape labeled"), &v));

    v.tag = codec_codec_Vector_Text;
    v.as.Text.value.ptr = "nul\0inside\0";
    v.as.Text.value.len = 11;
    assert(check(find(n, "string interior nul"), &v));

    // Any NaN matches the NaN vector; zero keeps its sign.
    uint64_t bits = 0x7ff8000000000001ull;
    v.tag = codec_codec_Vector_F64;
    memcpy(&v.as.F64.value, &bits, 8);
    assert(check(find(n, "f64 nan"), &v));
    v.as.F64.value = -0.0;
    assert(check(find(n, "f64 -0"), &v));
    v.as.F64.value = 0.0;
    assert(!check(find(n, "f64 -0"), &v));

    v.tag = codec_codec_Vector_U64;
    v.as.U64.value = UINT64_MAX;
    assert(check(find(n, "u64 max"), &v));

    v.tag = codec_codec_Vector_Hue;
    v.as.Hue.value = codec_codec_Color_Infrared;
    assert(check(find(n, "enum infrared"), &v));

    int64_t zero = 0;
    v.tag = codec_codec_Vector_MaybeI64;
    v.as.MaybeI64.value = &zero;
    assert(check(find(n, "optional zero"), &v));
    v.as.MaybeI64.value = NULL;
    assert(check(find(n, "optional absent"), &v));
    assert(!check(find(n, "optional zero"), &v));

    // A map's entry order doesn't matter on the wire.
    codec_str keys[3] = {codec_str_of("x"), codec_str_of("h\xc3\xa9llo"), codec_str_of("")};
    int64_t values[3] = {0, -1, INT64_MAX};
    v.tag = codec_codec_Vector_Counts;
    v.as.Counts.value.keys = keys;
    v.as.Counts.value.values = values;
    v.as.Counts.value.len = 3;
    assert(check(find(n, "map of strings"), &v));

    v.tag = codec_codec_Vector_Blank;
    assert(check(find(n, "blank"), &v));
}

static void spot_checks(uint32_t n) {
    codec_codec_Vector v = fetch(find(n, "i64 past 2^53"), NULL, NULL);
    assert(v.tag == codec_codec_Vector_I64 && v.as.I64.value == -9007199254740993LL);
    codec_codec_Vector_free(&v);

    v = fetch(find(n, "f32 min subnormal"), NULL, NULL);
    uint32_t f32_bits;
    memcpy(&f32_bits, &v.as.F32.value, 4);
    assert(v.tag == codec_codec_Vector_F32 && f32_bits == 1u);
    codec_codec_Vector_free(&v);

    v = fetch(find(n, "string astral"), NULL, NULL);
    assert(str_is(v.as.Text.value, "\xf0\x9f\xa6\x80 crab \xf0\x9f\x98\x80", 14));
    codec_codec_Vector_free(&v);

    v = fetch(find(n, "scalars minimum"), NULL, NULL);
    assert(v.tag == codec_codec_Vector_AllScalars);
    codec_codec_Scalars* m = &v.as.AllScalars.value;
    assert(m->i8_value == INT8_MIN && m->i16_value == INT16_MIN && m->i32_value == INT32_MIN);
    assert(m->i64_value == INT64_MIN && m->u64_value == 0);
    assert(isinf(m->f32_value) && m->f32_value < 0 && isnan(m->f64_value));
    assert(m->color == codec_codec_Color_Infrared && !m->flag);
    codec_codec_Vector_free(&v);

    v = fetch(find(n, "composite canonical"), NULL, NULL);
    assert(v.tag == codec_codec_Vector_Deep);
    codec_codec_Composite* c = &v.as.Deep.value;
    assert(str_is(c->name, "h\xc3\xa9llo w\xc3\xb6rld \xe2\x9c\x93", 17));
    assert(c->blob.len == 6 && c->blob.ptr[5] == 255);
    assert(c->some_i64 != NULL && *c->some_i64 == INT64_MIN && c->none_i64 == NULL);
    assert(c->some_text != NULL && c->some_text->len == 0);
    assert(c->names.len == 3 && c->names.items[1].len == 0);
    assert(c->matrix.len == 3 && c->matrix.items[1].len == 0 && c->matrix.items[2].items[0] == -4);
    assert(c->floats.len == 6 && isnan(c->floats.items[0]) && signbit(c->floats.items[3]));
    assert(c->by_name.len == 4 && c->by_id.len == 3 && c->by_color.len == 2 && c->flags.len == 2);
    assert(c->scalars.u32_value == 4000000000u);
    assert(c->shape.tag == codec_codec_Shape_Labeled && c->shape.as.Labeled.count == 3);
    assert(c->shapes.len == 6 && c->shapes.items[5].as.Nested.note == NULL);
    assert(c->maybe_shape != NULL && c->maybe_shape->tag == codec_codec_Shape_Nested);
    assert(c->maybe_list != NULL && c->maybe_list->len == 2);
    assert(c->sparse.len == 3 && c->sparse.items[1] == NULL && *c->sparse.items[0]);
    assert(c->colors.len == 4 && c->colors.items[3] == codec_codec_Color_Infrared);
    codec_codec_Vector_free(&v);
}

static void out_of_range(uint32_t n) {
    codec_error err = {0};
    size_t len = 0;
    assert(codec_codec_vector(n, &len, &err) == NULL);
    assert(err.code == codec_codec_CodecError_OutOfRange);
    char expected[64];
    snprintf(expected, sizeof expected, "vector %u is out of range (count %u)", n, n);
    assert(strcmp(err.message, expected) == 0);
    codec_codec_CodecError_OutOfRange_payload p;
    assert(codec_codec_CodecError_OutOfRange_payload_decode(err.payload_ptr, err.payload_len,
                                                           &p));
    assert(p.index == n && p.count == n);
    codec_error_clear(&err);

    assert(codec_codec_vector_name(n + 5, &len, &err) == NULL);
    assert(err.code == codec_codec_CodecError_OutOfRange);
    assert(codec_codec_CodecError_OutOfRange_payload_decode(err.payload_ptr, err.payload_len,
                                                           &p));
    assert(p.index == n + 5 && p.count == n);
    codec_error_clear(&err);

    codec_codec_Vector blank;
    blank.tag = codec_codec_Vector_Blank;
    assert(!check(n, &blank));
}

// Hand a malformed buffer to check_vector, which must reject it with -3.
static void reject(const codec_writer* w) {
    codec_error err = {0};
    assert(!codec_codec_check_vector(0, w->ptr, w->len, &err));
    assert(err.code == -3);
    codec_error_clear(&err);
}

static void malformed(void) {
    codec_writer w;

    memset(&w, 0, sizeof w);  // truncated mid-value
    codec_writer_put_i32(&w, codec_codec_Vector_I64);
    codec_writer_put_u32(&w, 7);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // unknown tag
    codec_writer_put_i32(&w, 999);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // trailing bytes
    codec_writer_put_i32(&w, codec_codec_Vector_Blank);
    codec_writer_put_u8(&w, 0);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // a bool that is neither 0 nor 1
    codec_writer_put_i32(&w, codec_codec_Vector_Flag);
    codec_writer_put_u8(&w, 2);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // an undeclared C-style enum value
    codec_writer_put_i32(&w, codec_codec_Vector_Hue);
    codec_writer_put_i32(&w, 3);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // a map that repeats a key
    codec_writer_put_i32(&w, codec_codec_Vector_Counts);
    codec_writer_put_u32(&w, 2);
    codec_writer_put_string(&w, codec_str_of("a"));
    codec_writer_put_i64(&w, 1);
    codec_writer_put_string(&w, codec_str_of("a"));
    codec_writer_put_i64(&w, 2);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // a string that isn't UTF-8
    codec_writer_put_i32(&w, codec_codec_Vector_Text);
    const uint8_t bad[2] = {0xC3, 0x28};
    codec_writer_put_bytes(&w, codec_bytes_of(bad, 2));
    reject(&w);
    codec_writer_free(&w);

    // The generated decoder rejects the same inputs.
    codec_codec_Vector v;
    const uint8_t unknown[4] = {0xE7, 0x03, 0, 0};
    assert(!codec_codec_Vector_decode(BYTES(unknown), &v));

    // Direct families: an undeclared enum value, a null string with a
    // length, and invalid UTF-8.
    codec_error err = {0};
    size_t len = 0;
    codec_codec_echo_color((codec_codec_Color)3, &err);
    assert(err.code == -3);
    codec_error_clear(&err);
    assert(codec_codec_echo_text(NULL, 2, &len, &err) == NULL && err.code == -3);
    codec_error_clear(&err);
    assert(codec_codec_echo_text(BYTES(bad), &len, &err) == NULL && err.code == -3);
    codec_error_clear(&err);
    assert(codec_codec_echo_text(NULL, 0, &len, &err) == NULL && len == 0 && err.code == 0);
}

static codec_codec_Token* token(int64_t value) {
    codec_error err = {0};
    codec_codec_Token* t = codec_codec_Token_new(value, &err);
    assert(err.code == 0 && t != NULL);
    return t;
}

static int64_t value_of(const codec_codec_Token* t) {
    codec_error err = {0};
    int64_t v = codec_codec_Token_value(t, &err);
    assert(err.code == 0);
    return v;
}

static int64_t sum(const codec_codec_Holder* h) {
    codec_error err = {0};
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_codec_Holder_write(&w, h);
    int64_t total = codec_codec_sum_holder(w.ptr, w.len, &err);
    assert(err.code == 0);
    codec_writer_free(&w);
    return total;
}

static void objects(uint32_t n) {
    codec_error err = {0};

    // The full object vector decodes to live tokens with the table's values.
    codec_codec_Vector v = fetch(find(n, "objects full"), NULL, NULL);
    assert(v.tag == codec_codec_Vector_Objects);
    codec_codec_Holder* h = &v.as.Objects.value;
    assert(value_of(h->primary) == 10 && h->spare != NULL && value_of(h->spare) == 11);
    assert(h->many.len == 3 && value_of(h->many.items[2]) == INT64_MIN);
    assert(h->by_name.len == 2 && value_of(h->by_name.values[1]) == 21);
    // Each encoding mints fresh references, so the holder can be sent twice.
    int64_t expected = 10 + 11 + 12 + 13 + 20 + 21 + INT64_MIN;
    assert(sum(h) == expected && sum(h) == expected);

    // primary_of returns the very same object (a new reference to it).
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_codec_Holder_write(&w, h);
    codec_codec_Token* p = codec_codec_primary_of(w.ptr, w.len, &err);
    codec_writer_free(&w);
    assert(err.code == 0 && p == h->primary);

    // same_primary compares identity, not value.
    codec_codec_Holder other;
    memset(&other, 0, sizeof other);
    other.primary = p;
    codec_writer a, b;
    memset(&a, 0, sizeof a);
    memset(&b, 0, sizeof b);
    codec_codec_Holder_write(&a, h);
    codec_codec_Holder_write(&b, &other);
    assert(codec_codec_same_primary(a.ptr, a.len, b.ptr, b.len, &err));
    codec_writer_free(&a);
    codec_writer_free(&b);
    codec_codec_Token* twin = token(10);
    other.primary = twin;
    memset(&a, 0, sizeof a);
    memset(&b, 0, sizeof b);
    codec_codec_Holder_write(&a, h);
    codec_codec_Holder_write(&b, &other);
    assert(!codec_codec_same_primary(a.ptr, a.len, b.ptr, b.len, &err));
    codec_writer_free(&a);
    codec_writer_free(&b);

    // A holder built from consumer-made tokens, one of them in every slot.
    codec_codec_Token* many[3] = {twin, twin, token(-4)};
    codec_str names[1] = {codec_str_of("k")};
    codec_codec_Token* values[1] = {twin};
    codec_codec_Holder mine;
    mine.primary = twin;
    mine.spare = twin;
    mine.many.items = many;
    mine.many.len = 3;
    mine.by_name.keys = names;
    mine.by_name.values = values;
    mine.by_name.len = 1;
    assert(sum(&mine) == 10 * 5 - 4);

    // The consumer-built sparse holder checks against the table.
    codec_codec_Vector sparse;
    sparse.tag = codec_codec_Vector_Objects;
    memset(&sparse.as.Objects.value, 0, sizeof sparse.as.Objects.value);
    codec_codec_Token* lone = token(-1);
    sparse.as.Objects.value.primary = lone;
    assert(check(find(n, "objects sparse"), &sparse));
    codec_codec_Token_destroy(lone);

    // A zero token is a marshalling failure, not a crash.
    const uint8_t zero_token[17] = {0};
    assert(codec_codec_sum_holder(BYTES(zero_token), &err) == 0 && err.code == -3);
    codec_error_clear(&err);

    codec_codec_Token_destroy(many[2]);
    codec_codec_Token_destroy(twin);
    codec_codec_Token_destroy(p);
    codec_codec_Vector_free(&v);  // releases every decoded token
    assert(codec_codec_Token_clone(NULL) == NULL);
    codec_codec_Token_destroy(NULL);
}

int main(void) {
    codec_error err = {0};
    assert(CODEC_ABI_VERSION == 4u && codec_abi_version() == CODEC_ABI_VERSION);
    assert(codec_codec_contract_check() == 0);
    assert(codec_debug_live(-1) == 1);

    uint32_t n = codec_codec_vector_count(&err);
    assert(err.code == 0 && n >= 60);

    every_vector(n);
    literal_vectors(n);
    spot_checks(n);
    out_of_range(n);
    malformed();
    objects(n);

    ASSERT_NO_LEAKS(codec_debug_live);
    printf("c/codec: OK (%u vectors)\n", n);
    return 0;
}
