// Conformance consumer: codec sample, C target (ABI revision 6).
//
// The shared-vector loop: for every vector the producer serves, decode it
// with the generated `codec_buffer.h`, re-encode it (which must reproduce the
// producer's bytes exactly, objects aside), pass it back to `check_vector`,
// and push each primitive vector's value through the matching direct-family
// `echo_*`. Then vectors built from literals (so a symmetric encode/decode
// bug can't hide), spot checks of decoded fields, the typed out-of-range
// error and its payload, malformed buffers (truncated, unknown tag, trailing
// bytes, a bad bool, a duplicate map key) rejected as marshalling failures,
// object identity and reference counting through buffers, and the one-value
// shapes: optional scalars (opt structs, with their `bool` bytes checked)
// and typed arrays (runs of packed elements, `len` in bytes) in both
// directions and as iterator items, a `usize`, a `char`, and a custom type.
// It also checks the per-declaration contract functions against hashes it
// computes itself. Ends by asserting the producer's leak counters are zero.

#include "harness.h"

#include <math.h>
#include <stddef.h>

#include "codec_buffer.h"

HARNESS_RUNS(codec)

static int f32_bits_eq(float a, float b) { return memcmp(&a, &b, 4) == 0; }
static int f64_bits_eq(double a, double b) { return memcmp(&a, &b, 8) == 0; }

static int str_is(codec_str s, const char* expected, size_t n) {
    return s.len == n && (n == 0 || memcmp(s.ptr, expected, n) == 0);
}

// Fetch vector `i`, keeping a copy of the producer's encoding in `*raw`
// (owned).
static codec_Vector fetch(uint32_t i, uint8_t** raw, size_t* raw_len) {
    codec_error err;
    codec_run run = codec_vector(i, &err);
    assert(err.code == 0 && run.ptr != NULL);
    if (raw != NULL) {
        *raw = (uint8_t*)malloc(run.len);
        memcpy(*raw, run.ptr, run.len);
        *raw_len = run.len;
    }
    codec_Vector v;
    assert(codec_Vector_take(run, &v));
    return v;
}

static bool check(uint32_t i, const codec_Vector* v) {
    codec_error err;
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_Vector_write(&w, v);
    assert(!w.failed);
    bool ok = codec_check_vector(i, codec_writer_run(&w), &err);
    assert(err.code == 0);
    codec_writer_free(&w);
    return ok;
}

// An owned, NUL-terminated copy of a returned string, which is released.
static char* take_string(codec_run run) {
    char* s = (char*)calloc(run.len + 1, 1);
    if (run.len > 0) memcpy(s, run.ptr, run.len);
    codec_run_free(run);
    return s;
}

static char* vector_name(uint32_t i) {
    codec_error err;
    codec_run name = codec_vector_name(i, &err);
    assert(err.code == 0);
    return take_string(name);
}

static char* describe(const codec_Vector* v) {
    codec_error err;
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_Vector_write(&w, v);
    codec_run text = codec_describe_vector(codec_writer_run(&w), &err);
    codec_writer_free(&w);
    assert(err.code == 0);
    return take_string(text);
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

// Whether a returned run holds exactly the `len` bytes at `ptr`; releases
// the run.
static int run_take_bytes(codec_run run, const void* ptr, size_t len) {
    int eq = run.len == len && (len == 0 || memcmp(run.ptr, ptr, len) == 0);
    codec_run_free(run);
    return eq;
}

// Push a primitive vector's value through its direct-family echo.
static void echo(const codec_Vector* v) {
    codec_error err = {0};
    switch (v->tag) {
    case codec_Vector_I8:
        assert(codec_echo_i8(v->as.I8.value, &err) == v->as.I8.value);
        break;
    case codec_Vector_U8:
        assert(codec_echo_u8(v->as.U8.value, &err) == v->as.U8.value);
        break;
    case codec_Vector_I16:
        assert(codec_echo_i16(v->as.I16.value, &err) == v->as.I16.value);
        break;
    case codec_Vector_U16:
        assert(codec_echo_u16(v->as.U16.value, &err) == v->as.U16.value);
        break;
    case codec_Vector_I32:
        assert(codec_echo_i32(v->as.I32.value, &err) == v->as.I32.value);
        break;
    case codec_Vector_U32:
        assert(codec_echo_u32(v->as.U32.value, &err) == v->as.U32.value);
        break;
    case codec_Vector_I64:
        assert(codec_echo_i64(v->as.I64.value, &err) == v->as.I64.value);
        break;
    case codec_Vector_U64:
        assert(codec_echo_u64(v->as.U64.value, &err) == v->as.U64.value);
        break;
    case codec_Vector_F32:
        assert(f32_bits_eq(codec_echo_f32(v->as.F32.value, &err), v->as.F32.value));
        break;
    case codec_Vector_F64:
        assert(f64_bits_eq(codec_echo_f64(v->as.F64.value, &err), v->as.F64.value));
        break;
    case codec_Vector_Flag:
        assert(codec_echo_bool(v->as.Flag.value, &err) == v->as.Flag.value);
        break;
    case codec_Vector_Hue:
        assert(codec_echo_color(v->as.Hue.value, &err) == v->as.Hue.value);
        break;
    case codec_Vector_Text: {
        codec_str s = v->as.Text.value;
        assert(run_take_bytes(codec_echo_text(codec_str_run(s), &err), s.ptr, s.len));
        break;
    }
    case codec_Vector_Blob: {
        codec_bytes b = v->as.Blob.value;
        assert(run_take_bytes(codec_echo_blob(codec_bytes_run(b), &err), b.ptr, b.len));
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
        codec_Vector v = fetch(i, &raw, &raw_len);
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
        if (v.tag != codec_Vector_Objects) {
            codec_writer w;
            memset(&w, 0, sizeof w);
            codec_Vector_write(&w, &v);
            assert(w.len == raw_len && memcmp(w.ptr, raw, raw_len) == 0);
            codec_writer_free(&w);
        }
        echo(&v);
        free(raw);
        codec_Vector_free(&v);
    }
}

static codec_Scalars canonical_scalars(void) {
    codec_Scalars s;
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
    s.color = codec_Color_Blue;
    return s;
}

static void literal_vectors(uint32_t n) {
    codec_Vector v;

    v.tag = codec_Vector_AllScalars;
    v.as.AllScalars.value = canonical_scalars();
    assert(check(find(n, "scalars canonical"), &v));
    v.as.AllScalars.value.u16_value = 60001;
    assert(!check(find(n, "scalars canonical"), &v));

    v.tag = codec_Vector_Figure;
    v.as.Figure.value.tag = codec_Shape_Labeled;
    v.as.Figure.value.as.Labeled.label = codec_str_of("tag");
    v.as.Figure.value.as.Labeled.count = 3;
    assert(check(find(n, "shape labeled"), &v));

    v.tag = codec_Vector_Text;
    v.as.Text.value.ptr = "nul\0inside\0";
    v.as.Text.value.len = 11;
    assert(check(find(n, "string interior nul"), &v));

    // Any NaN matches the NaN vector; zero keeps its sign.
    uint64_t bits = 0x7ff8000000000001ull;
    v.tag = codec_Vector_F64;
    memcpy(&v.as.F64.value, &bits, 8);
    assert(check(find(n, "f64 nan"), &v));
    v.as.F64.value = -0.0;
    assert(check(find(n, "f64 -0"), &v));
    v.as.F64.value = 0.0;
    assert(!check(find(n, "f64 -0"), &v));

    v.tag = codec_Vector_U64;
    v.as.U64.value = UINT64_MAX;
    assert(check(find(n, "u64 max"), &v));

    v.tag = codec_Vector_Hue;
    v.as.Hue.value = codec_Color_Infrared;
    assert(check(find(n, "enum infrared"), &v));

    int64_t zero = 0;
    v.tag = codec_Vector_MaybeI64;
    v.as.MaybeI64.value = &zero;
    assert(check(find(n, "optional zero"), &v));
    v.as.MaybeI64.value = NULL;
    assert(check(find(n, "optional absent"), &v));
    assert(!check(find(n, "optional zero"), &v));

    // A map's entry order doesn't matter on the wire.
    codec_str keys[3] = {codec_str_of("x"), codec_str_of("h\xc3\xa9llo"), codec_str_of("")};
    int64_t values[3] = {0, -1, INT64_MAX};
    v.tag = codec_Vector_Counts;
    v.as.Counts.value.keys = keys;
    v.as.Counts.value.values = values;
    v.as.Counts.value.len = 3;
    assert(check(find(n, "map of strings"), &v));

    v.tag = codec_Vector_Blank;
    assert(check(find(n, "blank"), &v));
}

static void spot_checks(uint32_t n) {
    codec_Vector v = fetch(find(n, "i64 past 2^53"), NULL, NULL);
    assert(v.tag == codec_Vector_I64 && v.as.I64.value == -9007199254740993LL);
    codec_Vector_free(&v);

    v = fetch(find(n, "f32 min subnormal"), NULL, NULL);
    uint32_t f32_bits;
    memcpy(&f32_bits, &v.as.F32.value, 4);
    assert(v.tag == codec_Vector_F32 && f32_bits == 1u);
    codec_Vector_free(&v);

    v = fetch(find(n, "string astral"), NULL, NULL);
    assert(str_is(v.as.Text.value, "\xf0\x9f\xa6\x80 crab \xf0\x9f\x98\x80", 14));
    codec_Vector_free(&v);

    v = fetch(find(n, "scalars minimum"), NULL, NULL);
    assert(v.tag == codec_Vector_AllScalars);
    codec_Scalars* m = &v.as.AllScalars.value;
    assert(m->i8_value == INT8_MIN && m->i16_value == INT16_MIN && m->i32_value == INT32_MIN);
    assert(m->i64_value == INT64_MIN && m->u64_value == 0);
    assert(isinf(m->f32_value) && m->f32_value < 0 && isnan(m->f64_value));
    assert(m->color == codec_Color_Infrared && !m->flag);
    codec_Vector_free(&v);

    v = fetch(find(n, "composite canonical"), NULL, NULL);
    assert(v.tag == codec_Vector_Deep);
    codec_Composite* c = &v.as.Deep.value;
    assert(str_is(c->name, "h\xc3\xa9llo w\xc3\xb6rld \xe2\x9c\x93", 17));
    assert(c->blob.len == 6 && c->blob.ptr[5] == 255);
    assert(c->some_i64 != NULL && *c->some_i64 == INT64_MIN && c->none_i64 == NULL);
    assert(c->some_text != NULL && c->some_text->len == 0);
    assert(c->names.len == 3 && c->names.items[1].len == 0);
    assert(c->matrix.len == 3 && c->matrix.items[1].len == 0 && c->matrix.items[2].items[0] == -4);
    assert(c->floats.len == 6 && isnan(c->floats.items[0]) && signbit(c->floats.items[3]));
    assert(c->by_name.len == 4 && c->by_id.len == 3 && c->by_color.len == 2 && c->flags.len == 2);
    assert(c->scalars.u32_value == 4000000000u);
    assert(c->shape.tag == codec_Shape_Labeled && c->shape.as.Labeled.count == 3);
    assert(c->shapes.len == 6 && c->shapes.items[5].as.Nested.note == NULL);
    assert(c->maybe_shape != NULL && c->maybe_shape->tag == codec_Shape_Nested);
    assert(c->maybe_list != NULL && c->maybe_list->len == 2);
    assert(c->sparse.len == 3 && c->sparse.items[1] == NULL && *c->sparse.items[0]);
    assert(c->colors.len == 4 && c->colors.items[3] == codec_Color_Infrared);
    codec_Vector_free(&v);
}

static void out_of_range(uint32_t n) {
    codec_error err;
    codec_run run = codec_vector(n, &err);
    assert(run.ptr == NULL && run.len == 0 && "a failure returns the empty run");
    assert(err.code == codec_CodecError_OutOfRange);
    char expected[64];
    snprintf(expected, sizeof expected, "vector %u is out of range (count %u)", n, n);
    assert(MSG_EQ(err, expected));
    codec_CodecError_OutOfRange_payload p;
    assert(codec_CodecError_OutOfRange_payload_decode(err.payload_ptr, err.payload_len, &p));
    assert(p.index == n && p.count == n);
    codec_error_clear(&err);

    run = codec_vector_name(n + 5, &err);
    assert(run.ptr == NULL && run.len == 0);
    assert(err.code == codec_CodecError_OutOfRange);
    assert(codec_CodecError_OutOfRange_payload_decode(err.payload_ptr, err.payload_len, &p));
    assert(p.index == n + 5 && p.count == n);
    codec_error_clear(&err);

    codec_Vector blank;
    blank.tag = codec_Vector_Blank;
    assert(!check(n, &blank));
}

// Hand a malformed buffer to check_vector, which must reject it with -3.
static void reject(const codec_writer* w) {
    codec_error err;
    assert(!codec_check_vector(0, codec_writer_run(w), &err));
    assert(err.code == -3);
    codec_error_clear(&err);
}

static void malformed(void) {
    codec_writer w;

    memset(&w, 0, sizeof w);  // truncated mid-value
    codec_writer_put_i32(&w, codec_Vector_I64);
    codec_writer_put_u32(&w, 7);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // unknown tag
    codec_writer_put_i32(&w, 999);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // trailing bytes
    codec_writer_put_i32(&w, codec_Vector_Blank);
    codec_writer_put_u8(&w, 0);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // a bool that is neither 0 nor 1
    codec_writer_put_i32(&w, codec_Vector_Flag);
    codec_writer_put_u8(&w, 2);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // an undeclared C-style enum value
    codec_writer_put_i32(&w, codec_Vector_Hue);
    codec_writer_put_i32(&w, 3);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // a map that repeats a key
    codec_writer_put_i32(&w, codec_Vector_Counts);
    codec_writer_put_u32(&w, 2);
    codec_writer_put_string(&w, codec_str_of("a"));
    codec_writer_put_i64(&w, 1);
    codec_writer_put_string(&w, codec_str_of("a"));
    codec_writer_put_i64(&w, 2);
    reject(&w);
    codec_writer_free(&w);

    memset(&w, 0, sizeof w);  // a string that isn't UTF-8
    codec_writer_put_i32(&w, codec_Vector_Text);
    const uint8_t bad[2] = {0xC3, 0x28};
    codec_writer_put_bytes(&w, codec_bytes_of(bad, 2));
    reject(&w);
    codec_writer_free(&w);

    // The generated decoder rejects the same inputs.
    codec_Vector v;
    const uint8_t unknown[4] = {0xE7, 0x03, 0, 0};
    assert(!codec_Vector_decode(unknown, sizeof unknown, &v));

    // Direct families: an undeclared enum value, a null run with a
    // length, and invalid UTF-8.
    codec_error err;
    codec_echo_color((codec_Color)3, &err);
    assert(err.code == -3);
    codec_error_clear(&err);
    codec_run null_run = {NULL, 2};
    codec_run out = codec_echo_text(null_run, &err);
    assert(out.ptr == NULL && out.len == 0 && err.code == -3);
    codec_error_clear(&err);
    out = codec_echo_text(bytes_run(bad, sizeof bad), &err);
    assert(out.ptr == NULL && out.len == 0 && err.code == -3);
    codec_error_clear(&err);
    codec_run empty = {NULL, 0};
    out = codec_echo_text(empty, &err);
    assert(out.ptr == NULL && out.len == 0 && err.code == 0);
}

static codec_Token* token(int64_t value) {
    codec_error err;
    codec_Token* t = codec_Token_new(value, &err);
    assert(err.code == 0 && t != NULL);
    return t;
}

static int64_t value_of(const codec_Token* t) {
    codec_error err;
    int64_t v = codec_Token_value(t, &err);
    assert(err.code == 0);
    return v;
}

static int64_t sum(const codec_Holder* h) {
    codec_error err;
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_Holder_write(&w, h);
    int64_t total = codec_sum_holder(codec_writer_run(&w), &err);
    assert(err.code == 0);
    codec_writer_free(&w);
    return total;
}

static void objects(uint32_t n) {
    codec_error err;

    // The full object vector decodes to live tokens with the table's values.
    codec_Vector v = fetch(find(n, "objects full"), NULL, NULL);
    assert(v.tag == codec_Vector_Objects);
    codec_Holder* h = &v.as.Objects.value;
    assert(value_of(h->primary) == 10 && h->spare != NULL && value_of(h->spare) == 11);
    assert(h->many.len == 3 && value_of(h->many.items[2]) == INT64_MIN);
    assert(h->by_name.len == 2 && value_of(h->by_name.values[1]) == 21);
    // Each encoding mints fresh references, so the holder can be sent twice.
    int64_t expected = 10 + 11 + 12 + 13 + 20 + 21 + INT64_MIN;
    assert(sum(h) == expected && sum(h) == expected);

    // primary_of returns the very same object (a new reference to it).
    codec_writer w;
    memset(&w, 0, sizeof w);
    codec_Holder_write(&w, h);
    codec_Token* p = codec_primary_of(codec_writer_run(&w), &err);
    codec_writer_free(&w);
    assert(err.code == 0 && p == h->primary);

    // same_primary compares identity, not value.
    codec_Holder other;
    memset(&other, 0, sizeof other);
    other.primary = p;
    codec_writer a, b;
    memset(&a, 0, sizeof a);
    memset(&b, 0, sizeof b);
    codec_Holder_write(&a, h);
    codec_Holder_write(&b, &other);
    assert(codec_same_primary(codec_writer_run(&a), codec_writer_run(&b), &err));
    codec_writer_free(&a);
    codec_writer_free(&b);
    codec_Token* twin = token(10);
    other.primary = twin;
    memset(&a, 0, sizeof a);
    memset(&b, 0, sizeof b);
    codec_Holder_write(&a, h);
    codec_Holder_write(&b, &other);
    assert(!codec_same_primary(codec_writer_run(&a), codec_writer_run(&b), &err));
    codec_writer_free(&a);
    codec_writer_free(&b);

    // A holder built from consumer-made tokens, one of them in every slot.
    codec_Token* many[3] = {twin, twin, token(-4)};
    codec_str names[1] = {codec_str_of("k")};
    codec_Token* values[1] = {twin};
    codec_Holder mine;
    mine.primary = twin;
    mine.spare = twin;
    mine.many.items = many;
    mine.many.len = 3;
    mine.by_name.keys = names;
    mine.by_name.values = values;
    mine.by_name.len = 1;
    assert(sum(&mine) == 10 * 5 - 4);

    // The consumer-built sparse holder checks against the table.
    codec_Vector sparse;
    sparse.tag = codec_Vector_Objects;
    memset(&sparse.as.Objects.value, 0, sizeof sparse.as.Objects.value);
    codec_Token* lone = token(-1);
    sparse.as.Objects.value.primary = lone;
    assert(check(find(n, "objects sparse"), &sparse));
    codec_Token_destroy(lone);

    // A zero token is a marshalling failure, not a crash.
    const uint8_t zero_token[17] = {0};
    assert(codec_sum_holder(bytes_run(zero_token, sizeof zero_token), &err) == 0);
    assert(err.code == -3);
    codec_error_clear(&err);

    codec_Token_destroy(many[2]);
    codec_Token_destroy(twin);
    codec_Token_destroy(p);
    codec_Vector_free(&v);  // releases every decoded token
    assert(codec_Token_clone(NULL) == NULL);
    codec_Token_destroy(NULL);
}

// An optional bool built byte by byte, so a byte other than 0 or 1 can be
// sent without forming an invalid C `bool`.
static codec_opt_bool raw_opt_bool(uint8_t has, uint8_t value) {
    codec_opt_bool o;
    memset(&o, 0, sizeof o);
    memcpy((uint8_t*)&o + offsetof(codec_opt_bool, has), &has, 1);
    memcpy((uint8_t*)&o + offsetof(codec_opt_bool, value), &value, 1);
    return o;
}

static void opt_scalars(void) {
    codec_error err;
    const codec_opt_i32 none_i32 = {false, 0};
    codec_opt_i32 i = codec_echo_opt_i32(none_i32, &err);
    assert(!i.has && i.value == 0 && err.code == 0);
    const codec_opt_i32 min_i32 = {true, INT32_MIN};
    i = codec_echo_opt_i32(min_i32, &err);
    assert(i.has && i.value == INT32_MIN && err.code == 0);
    const codec_opt_i32 zero_i32 = {true, 0};
    i = codec_echo_opt_i32(zero_i32, &err);
    assert(i.has && i.value == 0);

    const codec_opt_f64 neg_zero = {true, -0.0};
    codec_opt_f64 d = codec_echo_opt_f64(neg_zero, &err);
    assert(d.has && d.value == 0.0 && signbit(d.value));
    const codec_opt_f64 quiet_nan = {true, NAN};
    d = codec_echo_opt_f64(quiet_nan, &err);
    assert(d.has && isnan(d.value));
    const codec_opt_f64 none_f64 = {false, 0.0};
    d = codec_echo_opt_f64(none_f64, &err);
    assert(!d.has && d.value == 0.0);

    const codec_opt_bool yes = {true, true}, no = {true, false}, none_bool = {false, false};
    codec_opt_bool flag = codec_echo_opt_bool(yes, &err);
    assert(flag.has && flag.value);
    flag = codec_echo_opt_bool(no, &err);
    assert(flag.has && !flag.value);
    flag = codec_echo_opt_bool(none_bool, &err);
    assert(!flag.has && !flag.value && err.code == 0);
    // A `bool` byte (the value or `has`) other than 0 or 1 is a
    // marshalling failure.
    flag = codec_echo_opt_bool(raw_opt_bool(1, 2), &err);
    assert(!flag.has && err.code == -3);
    codec_error_clear(&err);
    flag = codec_echo_opt_bool(raw_opt_bool(2, 1), &err);
    assert(!flag.has && err.code == -3);
    codec_error_clear(&err);

    // An optional C-style enum crosses as an opt_i32.
    const codec_opt_i32 infrared = {true, codec_Color_Infrared};
    codec_opt_i32 c = codec_echo_opt_color(infrared, &err);
    assert(c.has && c.value == codec_Color_Infrared);
    const codec_opt_i32 blue = {true, codec_Color_Blue};
    c = codec_echo_opt_color(blue, &err);
    assert(c.has && c.value == codec_Color_Blue);
    c = codec_echo_opt_color(none_i32, &err);
    assert(!c.has && c.value == 0 && err.code == 0);
    // A present value is still checked; an absent one is ignored.
    const codec_opt_i32 undeclared = {true, 3};
    c = codec_echo_opt_color(undeclared, &err);
    assert(!c.has && err.code == -3);
    codec_error_clear(&err);
    const codec_opt_i32 absent_undeclared = {false, 3};
    c = codec_echo_opt_color(absent_undeclared, &err);
    assert(!c.has && err.code == 0);
}

static void slices(void) {
    codec_error err;

    uint64_t nan_bits = 0x7ff8000000000001ull;
    double f64s[4] = {0.0, -0.0, 5e-324, INFINITY};
    memcpy(&f64s[0], &nan_bits, 8);
    codec_run out = codec_echo_f64s(bytes_run(f64s, sizeof f64s), &err);
    assert(err.code == 0 && out.len == sizeof f64s && "a typed array's len counts bytes");
    assert(((uintptr_t)out.ptr % 8) == 0);
    const double* d = (const double*)(const void*)out.ptr;
    assert(isnan(d[0]) && memcmp(&d[1], &f64s[1], 3 * sizeof(double)) == 0);
    codec_run_free(out);

    const int32_t i32s[3] = {INT32_MIN, 0, INT32_MAX};
    out = codec_echo_i32s(bytes_run(i32s, sizeof i32s), &err);
    assert(err.code == 0 && run_take_bytes(out, i32s, sizeof i32s));
    codec_run empty = {NULL, 0};
    out = codec_echo_i32s(empty, &err);
    assert(out.ptr == NULL && out.len == 0 && err.code == 0);

    const uint64_t u64s[2] = {UINT64_MAX, 1ull << 63};
    out = codec_echo_u64s(bytes_run(u64s, sizeof u64s), &err);
    assert(err.code == 0 && run_take_bytes(out, u64s, sizeof u64s));
    out = codec_echo_u64s(empty, &err);
    assert(out.ptr == NULL && out.len == 0);

    // A null array with a length, one not aligned for its element type,
    // and one whose length isn't a multiple of the element's.
    codec_run null_run = {NULL, 16};
    out = codec_echo_u64s(null_run, &err);
    assert(out.ptr == NULL && err.code == -3);
    codec_error_clear(&err);
    _Alignas(8) uint8_t raw[24] = {0};
    out = codec_echo_u64s(bytes_run(raw + 4, 16), &err);
    assert(out.ptr == NULL && err.code == -3);
    codec_error_clear(&err);
    out = codec_echo_u64s(bytes_run(raw, 12), &err);
    assert(out.ptr == NULL && err.code == -3);
    codec_error_clear(&err);
}

// Pull every chunk of `chunks(values, size)`, checking it against `expected`
// (`counts[k]` elements each, laid out back to back).
static void chunks(const int32_t* values, size_t n, uint32_t size, const int32_t* expected,
                   const size_t* counts, size_t chunk_count) {
    codec_error err;
    codec_ChunksIterator* it = codec_chunks(bytes_run(values, n * sizeof(int32_t)), size, &err);
    assert(err.code == 0 && it != NULL);
    size_t seen = 0, offset = 0;
    for (;;) {
        codec_run item;
        int32_t more = codec_ChunksIterator_next(it, &item, &err);
        assert(err.code == 0);
        if (!more) break;
        assert(seen < chunk_count && item.len == counts[seen] * sizeof(int32_t));
        assert(((uintptr_t)item.ptr % 8) == 0);
        assert(run_take_bytes(item, expected + offset, counts[seen] * sizeof(int32_t)));
        offset += counts[seen];
        seen++;
    }
    assert(seen == chunk_count);
    codec_ChunksIterator_destroy(it);
}

static void iterators(void) {
    const int32_t extremes[3] = {INT32_MIN, 0, INT32_MAX};
    const size_t two_one[2] = {2, 1};
    chunks(extremes, 3, 2, extremes, two_one, 2);
    const int32_t four[4] = {1, 2, 3, 4};
    const size_t two_two[2] = {2, 2};
    chunks(four, 4, 2, four, two_two, 2);
    chunks(four, 2, 0, NULL, NULL, 0);
    chunks(NULL, 0, 3, NULL, NULL, 0);

    // `_next` checks `out_item` before it pulls an element, so a null slot
    // fails with -3 and loses nothing.
    codec_error err;
    codec_ChunksIterator* it = codec_chunks(bytes_run(four, sizeof four), 4, &err);
    assert(err.code == 0 && it != NULL);
    assert(codec_ChunksIterator_next(it, NULL, &err) == 0 && err.code == -3);
    codec_error_clear(&err);
    codec_run item;
    assert(codec_ChunksIterator_next(it, &item, &err) == 1 && err.code == 0);
    assert(run_take_bytes(item, four, sizeof four));
    assert(codec_ChunksIterator_next(it, &item, &err) == 0 && err.code == 0);
    codec_ChunksIterator_destroy(it);
}

// Echo a string through `f`, expecting `expected` (or, when `expected` is
// NULL, a -3 with `message`).
typedef codec_run (*echo_fn)(codec_run, codec_error*);
static void echo_text(echo_fn f, const char* in, const char* expected, const char* message) {
    codec_error err;
    codec_run out = f(str_run(in), &err);
    if (expected != NULL) {
        assert(err.code == 0 && run_take_eq(out, expected));
    } else {
        assert(out.ptr == NULL && err.code == -3 && MSG_EQ(err, message));
        codec_error_clear(&err);
    }
}

static void conversions(void) {
    codec_error err;
    assert(codec_echo_usize(4294967295u, &err) == 4294967295u && err.code == 0);
    assert(codec_echo_usize(UINT64_MAX, &err) == UINT64_MAX && err.code == 0);

    echo_text(codec_echo_char, "\xf0\x9f\xa6\x80", "\xf0\x9f\xa6\x80", NULL);
    echo_text(codec_echo_char, "\xc3\xa9", "\xc3\xa9", NULL);
    echo_text(codec_echo_char, "a", "a", NULL);
    // A marshalling failure names the parameter.
    echo_text(codec_echo_char, "ab", NULL, "value: \"ab\" is not a valid char");
    echo_text(codec_echo_char, "", NULL, "value: \"\" is not a valid char");

    echo_text(codec_echo_hex, "ff", "ff", NULL);
    echo_text(codec_echo_hex, "00FF", "ff", NULL);
    echo_text(codec_echo_hex, "0", "0", NULL);
    echo_text(codec_echo_hex, "xyz", NULL, "value: invalid digit found in string");
    echo_text(codec_echo_hex, "", NULL, "value: cannot parse integer from empty string");
    echo_text(codec_echo_hex, "100000000", NULL,
              "value: number too large to fit in target type");
}

// Every declaration exports `codec_contract_{fnv1a(path)}`, returning
// fnv1a of its canonical signature. Recompute a few here, independently of
// the generated table `codec_contract_check` walks.
static void contracts(void) {
    assert(codec_contract_check() == NULL);

    assert(fnv1a64("check_vector") == 0x1bed7dee9a53da67ull);
    assert(codec_contract_1bed7dee9a53da67() ==
           fnv1a64("function check_vector(u32, Vector) -> bool"));
    assert(fnv1a64("vector_name") == 0xc5e6b2820a4abbc4ull);
    assert(codec_contract_c5e6b2820a4abbc4() ==
           fnv1a64("function vector_name(u32) -> string throws CodecError"));
    assert(fnv1a64("chunks") == 0x99034d67cbe5d3a3ull);
    assert(codec_contract_99034d67cbe5d3a3() == fnv1a64("function chunks([i32], u32) -> iter<[i32]>"));
    assert(fnv1a64("echo_opt_color") == 0xc478868729c1a422ull);
    assert(codec_contract_c478868729c1a422() ==
           fnv1a64("function echo_opt_color(Color?) -> Color?"));
    // `usize` is `u64` in the signature.
    assert(fnv1a64("echo_usize") == 0x2f655b1a7ab39335ull);
    assert(codec_contract_2f655b1a7ab39335() == fnv1a64("function echo_usize(u64) -> u64"));
    // The custom type `Hex` is its repr.
    assert(fnv1a64("echo_hex") == 0x37c95403862d25b6ull);
    assert(codec_contract_37c95403862d25b6() == fnv1a64("function echo_hex(string) -> string"));
    // An interface and its members.
    assert(fnv1a64("Token") == 0xe68398e3c2c760b2ull);
    assert(codec_contract_e68398e3c2c760b2() == fnv1a64("interface Token"));
    assert(fnv1a64("Token.new") == 0x19f363a91a374252ull);
    assert(codec_contract_19f363a91a374252() == fnv1a64("constructor new(i64) -> Token"));
    assert(fnv1a64("Token.value") == 0x59d25d13b44894adull);
    assert(codec_contract_59d25d13b44894ad() == fnv1a64("method value() -> i64"));
    // A record (field types only), an enum (variant names and values), an
    // error domain and a code with fields.
    assert(fnv1a64("Holder") == 0xa1f526c44ead452full);
    assert(codec_contract_a1f526c44ead452f() ==
           fnv1a64("record Holder {Token, Token?, [Token], {string:Token}}"));
    assert(fnv1a64("Color") == 0x344ea8f4939cdc98ull);
    assert(codec_contract_344ea8f4939cdc98() ==
           fnv1a64("enum Color {Red = 0, Green = 1, Blue = 7, Infrared = -1}"));
    assert(fnv1a64("CodecError") == 0xe164ee0906a952c1ull);
    assert(codec_contract_e164ee0906a952c1() == fnv1a64("errors CodecError"));
    assert(fnv1a64("CodecError.OutOfRange") == 0x2134ae9245baee17ull);
    assert(codec_contract_2134ae9245baee17() == fnv1a64("code OutOfRange = 1 {u32, u32}"));
}

int main(void) {
    // `out_err` is overwritten, never read: garbage in it is fine.
    codec_error err;
    memset(&err, 0xA5, sizeof err);

    assert(CODEC_ABI_VERSION == 6u && codec_abi_version() == CODEC_ABI_VERSION);
    contracts();
    assert(codec_debug_live(-1) == 1);

    uint32_t n = codec_vector_count(&err);
    assert(err.code == 0 && n >= 60);

    every_vector(n);
    literal_vectors(n);
    spot_checks(n);
    out_of_range(n);
    malformed();
    objects(n);
    opt_scalars();
    slices();
    iterators();
    conversions();

    ASSERT_NO_LEAKS(codec_debug_live);
    printf("c/codec: OK (%u vectors)\n", n);
    return 0;
}
