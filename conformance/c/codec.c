// Conformance consumer: codec sample, C target (ABI revision 2).
//
// A round-trip check of the value-buffer protocol against the producer's own
// codec: every fixed-width scalar, strings (including non-ASCII), bytes,
// present and absent optionals, lists (nested and empty), string- and
// integer-keyed maps with record values, nested records, every rich-enum
// variant, lists of optionals and enums, and object tokens in a record
// field, an optional, and a list. Fixtures fetched from the producer are
// checked field by field (producer encodes, consumer decodes), handed back
// to `verify_*` (consumer encodes, producer decodes), and re-encoded through
// `roundtrip_*` and compared again; hand-built edge values (empty
// containers, 64-bit extremes, NaN, infinities, negative zero) go through
// the same round trip. Exits 0 on success; aborts on any mismatch.

#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "weaveffi.h"
#include "wvbuf.h"

// ── Scalars ────────────────────────────────────────────────────────────────

typedef struct {
    int8_t i8v;
    uint8_t u8v;
    int16_t i16v;
    uint16_t u16v;
    int32_t i32v;
    uint32_t u32v;
    int64_t i64v;
    uint64_t u64v;
    float f32v;
    double f64v;
    int flag;
    int32_t color;
} scalars_t;

static void read_scalars(wv_reader* r, scalars_t* s) {
    s->i8v = wv_get_i8(r);
    s->u8v = wv_get_u8(r);
    s->i16v = wv_get_i16(r);
    s->u16v = wv_get_u16(r);
    s->i32v = wv_get_i32(r);
    s->u32v = wv_get_u32(r);
    s->i64v = wv_get_i64(r);
    s->u64v = wv_get_u64(r);
    s->f32v = wv_get_f32(r);
    s->f64v = wv_get_f64(r);
    s->flag = wv_get_bool(r);
    s->color = wv_get_i32(r);
}

static void write_scalars(wv_writer* w, const scalars_t* s) {
    wv_put_i8(w, s->i8v);
    wv_put_u8(w, s->u8v);
    wv_put_i16(w, s->i16v);
    wv_put_u16(w, s->u16v);
    wv_put_i32(w, s->i32v);
    wv_put_u32(w, s->u32v);
    wv_put_i64(w, s->i64v);
    wv_put_u64(w, s->u64v);
    wv_put_f32(w, s->f32v);
    wv_put_f64(w, s->f64v);
    wv_put_bool(w, s->flag);
    wv_put_i32(w, s->color);
}

// Bitwise float equality, so NaN payloads and the sign of zero count.
static int f32_bits_eq(float a, float b) { return memcmp(&a, &b, 4) == 0; }
static int f64_bits_eq(double a, double b) { return memcmp(&a, &b, 8) == 0; }

static int scalars_eq(const scalars_t* a, const scalars_t* b) {
    return a->i8v == b->i8v && a->u8v == b->u8v && a->i16v == b->i16v &&
           a->u16v == b->u16v && a->i32v == b->i32v && a->u32v == b->u32v &&
           a->i64v == b->i64v && a->u64v == b->u64v &&
           f32_bits_eq(a->f32v, b->f32v) && f64_bits_eq(a->f64v, b->f64v) &&
           a->flag == b->flag && a->color == b->color;
}

// The canonical fixture the producer hands out and expects back.
static const scalars_t CANONICAL_SCALARS = {
    -8, 200, -16000, 60000, -2000000000, 4000000000u,
    -9007199254740993LL, 18446744073709551615ULL, 1.5f, -2.25e100, 1,
    weaveffi_codec_Color_Blue,
};

// ── Shape (rich enum) ──────────────────────────────────────────────────────

typedef struct {
    int32_t tag;
    double radius;        // Circle
    float width, height;  // Rect
    char* label;          // Labeled
    int32_t count;        // Labeled
    scalars_t inner;      // Nested
    int has_note;         // Nested
    char* note;           // Nested (when has_note)
} shape_t;

static void read_shape(wv_reader* r, shape_t* s) {
    memset(s, 0, sizeof *s);
    s->tag = wv_get_i32(r);
    switch (s->tag) {
        case weaveffi_codec_Shape_Empty:
            break;
        case weaveffi_codec_Shape_Circle:
            s->radius = wv_get_f64(r);
            break;
        case weaveffi_codec_Shape_Rect:
            s->width = wv_get_f32(r);
            s->height = wv_get_f32(r);
            break;
        case weaveffi_codec_Shape_Labeled:
            s->label = wv_get_str(r);
            s->count = wv_get_i32(r);
            break;
        case weaveffi_codec_Shape_Nested:
            read_scalars(r, &s->inner);
            s->has_note = wv_get_bool(r);
            if (s->has_note) s->note = wv_get_str(r);
            break;
        default:
            assert(0 && "unknown Shape tag");
    }
}

static void write_shape(wv_writer* w, const shape_t* s) {
    wv_put_i32(w, s->tag);
    switch (s->tag) {
        case weaveffi_codec_Shape_Empty:
            break;
        case weaveffi_codec_Shape_Circle:
            wv_put_f64(w, s->radius);
            break;
        case weaveffi_codec_Shape_Rect:
            wv_put_f32(w, s->width);
            wv_put_f32(w, s->height);
            break;
        case weaveffi_codec_Shape_Labeled:
            wv_put_str(w, s->label);
            wv_put_i32(w, s->count);
            break;
        case weaveffi_codec_Shape_Nested:
            write_scalars(w, &s->inner);
            wv_put_bool(w, s->has_note);
            if (s->has_note) wv_put_str(w, s->note);
            break;
        default:
            assert(0 && "unknown Shape tag");
    }
}

static int str_eq(const char* a, const char* b) {
    if (a == NULL || b == NULL) return a == b;
    return strcmp(a, b) == 0;
}

static int shape_eq(const shape_t* a, const shape_t* b) {
    if (a->tag != b->tag) return 0;
    switch (a->tag) {
        case weaveffi_codec_Shape_Empty:
            return 1;
        case weaveffi_codec_Shape_Circle:
            return f64_bits_eq(a->radius, b->radius);
        case weaveffi_codec_Shape_Rect:
            return f32_bits_eq(a->width, b->width) && f32_bits_eq(a->height, b->height);
        case weaveffi_codec_Shape_Labeled:
            return str_eq(a->label, b->label) && a->count == b->count;
        case weaveffi_codec_Shape_Nested:
            return scalars_eq(&a->inner, &b->inner) && a->has_note == b->has_note &&
                   (!a->has_note || str_eq(a->note, b->note));
        default:
            return 0;
    }
}

static void shape_free(shape_t* s) {
    free(s->label);
    free(s->note);
    s->label = s->note = NULL;
}

static shape_t shape_empty(void) {
    shape_t s;
    memset(&s, 0, sizeof s);
    s.tag = weaveffi_codec_Shape_Empty;
    return s;
}

static shape_t shape_circle(double radius) {
    shape_t s = shape_empty();
    s.tag = weaveffi_codec_Shape_Circle;
    s.radius = radius;
    return s;
}

static shape_t shape_rect(float w, float h) {
    shape_t s = shape_empty();
    s.tag = weaveffi_codec_Shape_Rect;
    s.width = w;
    s.height = h;
    return s;
}

// `label` is copied so every shape_t owns its strings uniformly.
static shape_t shape_labeled(const char* label, int32_t count) {
    shape_t s = shape_empty();
    s.tag = weaveffi_codec_Shape_Labeled;
    s.label = strdup(label);
    s.count = count;
    return s;
}

static shape_t shape_nested(const scalars_t* inner, const char* note) {
    shape_t s = shape_empty();
    s.tag = weaveffi_codec_Shape_Nested;
    s.inner = *inner;
    s.has_note = note != NULL;
    s.note = note ? strdup(note) : NULL;
    return s;
}

// ── Composite ──────────────────────────────────────────────────────────────

typedef struct {
    int32_t* v;
    uint32_t n;
} i32_list_t;

typedef struct {
    char* key;
    int64_t value;
} by_name_t;

typedef struct {
    int32_t key;
    scalars_t value;
} by_id_t;

typedef struct {
    int has;
    int value;
} opt_bool_t;

typedef struct {
    char* name;
    uint8_t* blob;
    uint32_t blob_len;
    int has_some_i64;
    int64_t some_i64;
    int has_none_i64;
    int64_t none_i64;
    int has_some_text;
    char* some_text;
    char** names;
    uint32_t names_n;
    i32_list_t* matrix;
    uint32_t matrix_n;
    double* empty;
    uint32_t empty_n;
    by_name_t* by_name;
    uint32_t by_name_n;
    by_id_t* by_id;
    uint32_t by_id_n;
    scalars_t scalars;
    shape_t shape;
    shape_t* shapes;
    uint32_t shapes_n;
    int has_maybe_shape;
    shape_t maybe_shape;
    int has_maybe_list;
    uint8_t* maybe_list;
    uint32_t maybe_list_n;
    opt_bool_t* sparse;
    uint32_t sparse_n;
    int32_t* colors;
    uint32_t colors_n;
} composite_t;

static void* xcalloc(size_t n, size_t size) {
    void* p = calloc(n ? n : 1, size);
    assert(p != NULL);
    return p;
}

static void read_composite(wv_reader* r, composite_t* c) {
    memset(c, 0, sizeof *c);
    c->name = wv_get_str(r);
    c->blob_len = wv_get_u32(r);
    c->blob = (uint8_t*)xcalloc(c->blob_len, 1);
    memcpy(c->blob, wv_take(r, c->blob_len), c->blob_len);
    c->has_some_i64 = wv_get_bool(r);
    if (c->has_some_i64) c->some_i64 = wv_get_i64(r);
    c->has_none_i64 = wv_get_bool(r);
    if (c->has_none_i64) c->none_i64 = wv_get_i64(r);
    c->has_some_text = wv_get_bool(r);
    if (c->has_some_text) c->some_text = wv_get_str(r);
    c->names_n = wv_get_u32(r);
    c->names = (char**)xcalloc(c->names_n, sizeof(char*));
    for (uint32_t i = 0; i < c->names_n; i++) c->names[i] = wv_get_str(r);
    c->matrix_n = wv_get_u32(r);
    c->matrix = (i32_list_t*)xcalloc(c->matrix_n, sizeof(i32_list_t));
    for (uint32_t i = 0; i < c->matrix_n; i++) {
        c->matrix[i].n = wv_get_u32(r);
        c->matrix[i].v = (int32_t*)xcalloc(c->matrix[i].n, sizeof(int32_t));
        for (uint32_t j = 0; j < c->matrix[i].n; j++) c->matrix[i].v[j] = wv_get_i32(r);
    }
    c->empty_n = wv_get_u32(r);
    c->empty = (double*)xcalloc(c->empty_n, sizeof(double));
    for (uint32_t i = 0; i < c->empty_n; i++) c->empty[i] = wv_get_f64(r);
    c->by_name_n = wv_get_u32(r);
    c->by_name = (by_name_t*)xcalloc(c->by_name_n, sizeof(by_name_t));
    for (uint32_t i = 0; i < c->by_name_n; i++) {
        c->by_name[i].key = wv_get_str(r);
        c->by_name[i].value = wv_get_i64(r);
    }
    c->by_id_n = wv_get_u32(r);
    c->by_id = (by_id_t*)xcalloc(c->by_id_n, sizeof(by_id_t));
    for (uint32_t i = 0; i < c->by_id_n; i++) {
        c->by_id[i].key = wv_get_i32(r);
        read_scalars(r, &c->by_id[i].value);
    }
    read_scalars(r, &c->scalars);
    read_shape(r, &c->shape);
    c->shapes_n = wv_get_u32(r);
    c->shapes = (shape_t*)xcalloc(c->shapes_n, sizeof(shape_t));
    for (uint32_t i = 0; i < c->shapes_n; i++) read_shape(r, &c->shapes[i]);
    c->has_maybe_shape = wv_get_bool(r);
    if (c->has_maybe_shape) read_shape(r, &c->maybe_shape);
    c->has_maybe_list = wv_get_bool(r);
    if (c->has_maybe_list) {
        c->maybe_list_n = wv_get_u32(r);
        c->maybe_list = (uint8_t*)xcalloc(c->maybe_list_n, 1);
        for (uint32_t i = 0; i < c->maybe_list_n; i++) c->maybe_list[i] = wv_get_u8(r);
    }
    c->sparse_n = wv_get_u32(r);
    c->sparse = (opt_bool_t*)xcalloc(c->sparse_n, sizeof(opt_bool_t));
    for (uint32_t i = 0; i < c->sparse_n; i++) {
        c->sparse[i].has = wv_get_bool(r);
        if (c->sparse[i].has) c->sparse[i].value = wv_get_bool(r);
    }
    c->colors_n = wv_get_u32(r);
    c->colors = (int32_t*)xcalloc(c->colors_n, sizeof(int32_t));
    for (uint32_t i = 0; i < c->colors_n; i++) c->colors[i] = wv_get_i32(r);
}

static void write_composite(wv_writer* w, const composite_t* c) {
    wv_put_str(w, c->name);
    wv_put_bytes(w, c->blob, c->blob_len);
    wv_put_bool(w, c->has_some_i64);
    if (c->has_some_i64) wv_put_i64(w, c->some_i64);
    wv_put_bool(w, c->has_none_i64);
    if (c->has_none_i64) wv_put_i64(w, c->none_i64);
    wv_put_bool(w, c->has_some_text);
    if (c->has_some_text) wv_put_str(w, c->some_text);
    wv_put_u32(w, c->names_n);
    for (uint32_t i = 0; i < c->names_n; i++) wv_put_str(w, c->names[i]);
    wv_put_u32(w, c->matrix_n);
    for (uint32_t i = 0; i < c->matrix_n; i++) {
        wv_put_u32(w, c->matrix[i].n);
        for (uint32_t j = 0; j < c->matrix[i].n; j++) wv_put_i32(w, c->matrix[i].v[j]);
    }
    wv_put_u32(w, c->empty_n);
    for (uint32_t i = 0; i < c->empty_n; i++) wv_put_f64(w, c->empty[i]);
    wv_put_u32(w, c->by_name_n);
    for (uint32_t i = 0; i < c->by_name_n; i++) {
        wv_put_str(w, c->by_name[i].key);
        wv_put_i64(w, c->by_name[i].value);
    }
    wv_put_u32(w, c->by_id_n);
    for (uint32_t i = 0; i < c->by_id_n; i++) {
        wv_put_i32(w, c->by_id[i].key);
        write_scalars(w, &c->by_id[i].value);
    }
    write_scalars(w, &c->scalars);
    write_shape(w, &c->shape);
    wv_put_u32(w, c->shapes_n);
    for (uint32_t i = 0; i < c->shapes_n; i++) write_shape(w, &c->shapes[i]);
    wv_put_bool(w, c->has_maybe_shape);
    if (c->has_maybe_shape) write_shape(w, &c->maybe_shape);
    wv_put_bool(w, c->has_maybe_list);
    if (c->has_maybe_list) {
        wv_put_u32(w, c->maybe_list_n);
        for (uint32_t i = 0; i < c->maybe_list_n; i++) wv_put_u8(w, c->maybe_list[i]);
    }
    wv_put_u32(w, c->sparse_n);
    for (uint32_t i = 0; i < c->sparse_n; i++) {
        wv_put_bool(w, c->sparse[i].has);
        if (c->sparse[i].has) wv_put_bool(w, c->sparse[i].value);
    }
    wv_put_u32(w, c->colors_n);
    for (uint32_t i = 0; i < c->colors_n; i++) wv_put_i32(w, c->colors[i]);
}

static int composite_eq(const composite_t* a, const composite_t* b) {
    if (!str_eq(a->name, b->name)) return 0;
    if (a->blob_len != b->blob_len || memcmp(a->blob, b->blob, a->blob_len) != 0) return 0;
    if (a->has_some_i64 != b->has_some_i64 || (a->has_some_i64 && a->some_i64 != b->some_i64))
        return 0;
    if (a->has_none_i64 != b->has_none_i64 || (a->has_none_i64 && a->none_i64 != b->none_i64))
        return 0;
    if (a->has_some_text != b->has_some_text ||
        (a->has_some_text && !str_eq(a->some_text, b->some_text)))
        return 0;
    if (a->names_n != b->names_n) return 0;
    for (uint32_t i = 0; i < a->names_n; i++)
        if (!str_eq(a->names[i], b->names[i])) return 0;
    if (a->matrix_n != b->matrix_n) return 0;
    for (uint32_t i = 0; i < a->matrix_n; i++) {
        if (a->matrix[i].n != b->matrix[i].n) return 0;
        for (uint32_t j = 0; j < a->matrix[i].n; j++)
            if (a->matrix[i].v[j] != b->matrix[i].v[j]) return 0;
    }
    if (a->empty_n != b->empty_n) return 0;
    for (uint32_t i = 0; i < a->empty_n; i++)
        if (!f64_bits_eq(a->empty[i], b->empty[i])) return 0;
    if (a->by_name_n != b->by_name_n) return 0;
    for (uint32_t i = 0; i < a->by_name_n; i++)
        if (!str_eq(a->by_name[i].key, b->by_name[i].key) ||
            a->by_name[i].value != b->by_name[i].value)
            return 0;
    if (a->by_id_n != b->by_id_n) return 0;
    for (uint32_t i = 0; i < a->by_id_n; i++)
        if (a->by_id[i].key != b->by_id[i].key ||
            !scalars_eq(&a->by_id[i].value, &b->by_id[i].value))
            return 0;
    if (!scalars_eq(&a->scalars, &b->scalars)) return 0;
    if (!shape_eq(&a->shape, &b->shape)) return 0;
    if (a->shapes_n != b->shapes_n) return 0;
    for (uint32_t i = 0; i < a->shapes_n; i++)
        if (!shape_eq(&a->shapes[i], &b->shapes[i])) return 0;
    if (a->has_maybe_shape != b->has_maybe_shape ||
        (a->has_maybe_shape && !shape_eq(&a->maybe_shape, &b->maybe_shape)))
        return 0;
    if (a->has_maybe_list != b->has_maybe_list) return 0;
    if (a->has_maybe_list &&
        (a->maybe_list_n != b->maybe_list_n ||
         memcmp(a->maybe_list, b->maybe_list, a->maybe_list_n) != 0))
        return 0;
    if (a->sparse_n != b->sparse_n) return 0;
    for (uint32_t i = 0; i < a->sparse_n; i++)
        if (a->sparse[i].has != b->sparse[i].has ||
            (a->sparse[i].has && a->sparse[i].value != b->sparse[i].value))
            return 0;
    if (a->colors_n != b->colors_n) return 0;
    for (uint32_t i = 0; i < a->colors_n; i++)
        if (a->colors[i] != b->colors[i]) return 0;
    return 1;
}

static void composite_free(composite_t* c) {
    free(c->name);
    free(c->blob);
    free(c->some_text);
    for (uint32_t i = 0; i < c->names_n; i++) free(c->names[i]);
    free(c->names);
    for (uint32_t i = 0; i < c->matrix_n; i++) free(c->matrix[i].v);
    free(c->matrix);
    free(c->empty);
    for (uint32_t i = 0; i < c->by_name_n; i++) free(c->by_name[i].key);
    free(c->by_name);
    free(c->by_id);
    shape_free(&c->shape);
    for (uint32_t i = 0; i < c->shapes_n; i++) shape_free(&c->shapes[i]);
    free(c->shapes);
    if (c->has_maybe_shape) shape_free(&c->maybe_shape);
    free(c->maybe_list);
    free(c->sparse);
    free(c->colors);
    memset(c, 0, sizeof *c);
}

// ── ABI helpers ────────────────────────────────────────────────────────────

// Decode a buffered return, checking it was consumed exactly, and free it.
#define TAKE_BUFFER(ptr, len, reader_body)          \
    do {                                            \
        assert((ptr) != NULL);                      \
        wv_reader r_;                               \
        wv_r_init(&r_, (ptr), (len));               \
        reader_body;                                \
        wv_r_expect_end(&r_);                       \
        weaveffi_free_bytes((uint8_t*)(ptr), (len)); \
    } while (0)

static void fetch_sample_scalars(scalars_t* out) {
    weaveffi_error err = {0};
    size_t len = 0;
    const uint8_t* p = weaveffi_codec_sample_scalars(&len, &err);
    assert(err.code == 0);
    TAKE_BUFFER(p, len, read_scalars(&r_, out));
}

static void fetch_sample_composite(composite_t* out) {
    weaveffi_error err = {0};
    size_t len = 0;
    const uint8_t* p = weaveffi_codec_sample_composite(&len, &err);
    assert(err.code == 0);
    TAKE_BUFFER(p, len, read_composite(&r_, out));
}

static void roundtrip_scalars(const scalars_t* in, scalars_t* out) {
    weaveffi_error err = {0};
    wv_writer w;
    wv_w_init(&w);
    write_scalars(&w, in);
    size_t len = 0;
    const uint8_t* p = weaveffi_codec_roundtrip_scalars(w.buf, w.len, &len, &err);
    assert(err.code == 0);
    assert(len == w.len && memcmp(p, w.buf, len) == 0 &&
           "our encoding is byte-identical to the producer's");
    wv_w_free(&w);
    TAKE_BUFFER(p, len, read_scalars(&r_, out));
}

static void roundtrip_composite(const composite_t* in, composite_t* out) {
    weaveffi_error err = {0};
    wv_writer w;
    wv_w_init(&w);
    write_composite(&w, in);
    size_t len = 0;
    const uint8_t* p = weaveffi_codec_roundtrip_composite(w.buf, w.len, &len, &err);
    assert(err.code == 0);
    assert(len == w.len && memcmp(p, w.buf, len) == 0 &&
           "our encoding is byte-identical to the producer's");
    wv_w_free(&w);
    TAKE_BUFFER(p, len, read_composite(&r_, out));
}

static void roundtrip_shape(const shape_t* in, shape_t* out) {
    weaveffi_error err = {0};
    wv_writer w;
    wv_w_init(&w);
    write_shape(&w, in);
    size_t len = 0;
    const uint8_t* p = weaveffi_codec_roundtrip_shape(w.buf, w.len, &len, &err);
    assert(err.code == 0);
    assert(len == w.len && memcmp(p, w.buf, len) == 0);
    wv_w_free(&w);
    TAKE_BUFFER(p, len, read_shape(&r_, out));
}

static int verify_scalars(const scalars_t* s, weaveffi_error* err) {
    wv_writer w;
    wv_w_init(&w);
    write_scalars(&w, s);
    int ok = weaveffi_codec_verify_scalars(w.buf, w.len, err);
    wv_w_free(&w);
    return ok;
}

static int verify_composite(const composite_t* c, weaveffi_error* err) {
    wv_writer w;
    wv_w_init(&w);
    write_composite(&w, c);
    int ok = weaveffi_codec_verify_composite(w.buf, w.len, err);
    wv_w_free(&w);
    return ok;
}

static char* describe_shape(const shape_t* s) {
    weaveffi_error err = {0};
    wv_writer w;
    wv_w_init(&w);
    write_shape(&w, s);
    const char* text = weaveffi_codec_describe_shape(w.buf, w.len, &err);
    assert(err.code == 0 && text != NULL);
    wv_w_free(&w);
    char* copy = strdup(text);
    weaveffi_free_string(text);
    return copy;
}

// ── Holder (objects inside buffers) ────────────────────────────────────────

typedef struct {
    weaveffi_codec_Token* primary;
    weaveffi_codec_Token* spare;  // NULL when absent
    weaveffi_codec_Token* many[8];
    uint32_t many_n;
} holder_t;

// Decoding adopts one strong reference per token.
static void read_holder(wv_reader* r, holder_t* h) {
    memset(h, 0, sizeof *h);
    h->primary = (weaveffi_codec_Token*)wv_get_obj(r);
    h->spare = wv_get_bool(r) ? (weaveffi_codec_Token*)wv_get_obj(r) : NULL;
    h->many_n = wv_get_u32(r);
    assert(h->many_n <= 8);
    for (uint32_t i = 0; i < h->many_n; i++) h->many[i] = (weaveffi_codec_Token*)wv_get_obj(r);
}

// Encoding mints a fresh reference per token with `_clone`; the holder's
// own references are untouched.
static void write_holder(wv_writer* w, const holder_t* h) {
    wv_put_obj(w, weaveffi_codec_Token_clone(h->primary));
    wv_put_bool(w, h->spare != NULL);
    if (h->spare) wv_put_obj(w, weaveffi_codec_Token_clone(h->spare));
    wv_put_u32(w, h->many_n);
    for (uint32_t i = 0; i < h->many_n; i++) wv_put_obj(w, weaveffi_codec_Token_clone(h->many[i]));
}

static void holder_release(holder_t* h) {
    weaveffi_codec_Token_destroy(h->primary);
    weaveffi_codec_Token_destroy(h->spare);
    for (uint32_t i = 0; i < h->many_n; i++) weaveffi_codec_Token_destroy(h->many[i]);
    memset(h, 0, sizeof *h);
}

static int64_t token_value(const weaveffi_codec_Token* t) {
    weaveffi_error err = {0};
    int64_t v = weaveffi_codec_Token_value(t, &err);
    assert(err.code == 0);
    return v;
}

static void make_holder(int64_t base, int with_spare, holder_t* out) {
    weaveffi_error err = {0};
    size_t len = 0;
    const uint8_t* p = weaveffi_codec_make_holder(base, with_spare, &len, &err);
    assert(err.code == 0);
    TAKE_BUFFER(p, len, read_holder(&r_, out));
}

// ── main ───────────────────────────────────────────────────────────────────

int main(void) {
    weaveffi_error err = {0};

    assert(weaveffi_abi_version() == 2u && WEAVEFFI_ABI_VERSION == 2u);

    // ── Scalars: producer encodes, consumer decodes ──────────────────────
    scalars_t s;
    fetch_sample_scalars(&s);
    assert(s.i8v == -8);
    assert(s.u8v == 200);
    assert(s.i16v == -16000);
    assert(s.u16v == 60000);
    assert(s.i32v == -2000000000);
    assert(s.u32v == 4000000000u);
    assert(s.i64v == -9007199254740993LL);
    assert(s.u64v == UINT64_MAX);
    assert(s.f32v == 1.5f);
    assert(s.f64v == -2.25e100);
    assert(s.flag == 1);
    assert(s.color == weaveffi_codec_Color_Blue && s.color == 7);
    assert(scalars_eq(&s, &CANONICAL_SCALARS));

    // Consumer encodes, producer decodes and compares to its canonical value.
    assert(verify_scalars(&s, &err) && err.code == 0);

    // Consumer encodes, producer re-encodes: field-by-field equality.
    scalars_t s2;
    roundtrip_scalars(&s, &s2);
    assert(scalars_eq(&s, &s2));

    // A one-field change is a Mismatch (code 1) with the doc-comment message.
    s2.flag = 0;
    assert(!verify_scalars(&s2, &err));
    assert(err.code == weaveffi_codec_CodecError_Mismatch && err.code == 1);
    assert(err.message != NULL &&
           strcmp(err.message, "value does not match the canonical fixture") == 0);
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    weaveffi_error_clear(&err);

    // Hand-built edge scalars: extremes, NaN, infinities, negative zero.
    scalars_t edge = {
        INT8_MIN, UINT8_MAX, INT16_MIN, UINT16_MAX, INT32_MIN, UINT32_MAX,
        INT64_MIN, UINT64_MAX, NAN, -0.0, 0, weaveffi_codec_Color_Red,
    };
    scalars_t edge2;
    roundtrip_scalars(&edge, &edge2);
    assert(scalars_eq(&edge, &edge2));
    assert(isnan(edge2.f32v) && signbit(edge2.f64v) && edge2.f64v == 0.0);
    edge.i8v = INT8_MAX;
    edge.i16v = INT16_MAX;
    edge.i32v = INT32_MAX;
    edge.i64v = INT64_MAX;
    edge.u8v = 0;
    edge.u16v = 0;
    edge.u32v = 0;
    edge.u64v = 0;
    edge.f32v = -INFINITY;
    edge.f64v = INFINITY;
    edge.color = weaveffi_codec_Color_Green;
    roundtrip_scalars(&edge, &edge2);
    assert(scalars_eq(&edge, &edge2));
    assert(isinf(edge2.f32v) && edge2.f32v < 0 && isinf(edge2.f64v) && edge2.f64v > 0);
    edge.f64v = NAN;
    edge.f32v = 0.0f;
    roundtrip_scalars(&edge, &edge2);
    assert(isnan(edge2.f64v) && scalars_eq(&edge, &edge2));

    // ── Composite: producer encodes, consumer decodes ────────────────────
    composite_t c;
    fetch_sample_composite(&c);
    assert(strcmp(c.name, "h\xC3\xA9llo w\xC3\xB6rld \xE2\x9C\x93") == 0);
    const uint8_t blob[6] = {0, 1, 2, 253, 254, 255};
    assert(c.blob_len == 6 && memcmp(c.blob, blob, 6) == 0);
    assert(c.has_some_i64 && c.some_i64 == INT64_MIN);
    assert(!c.has_none_i64);
    assert(c.has_some_text && strcmp(c.some_text, "") == 0);
    assert(c.names_n == 3 && strcmp(c.names[0], "a") == 0 && strcmp(c.names[1], "") == 0 &&
           strcmp(c.names[2], "ccc") == 0);
    assert(c.matrix_n == 3);
    assert(c.matrix[0].n == 3 && c.matrix[0].v[0] == 1 && c.matrix[0].v[1] == 2 &&
           c.matrix[0].v[2] == 3);
    assert(c.matrix[1].n == 0);
    assert(c.matrix[2].n == 1 && c.matrix[2].v[0] == -4);
    assert(c.empty_n == 0);
    // BTreeMap keys arrive sorted.
    assert(c.by_name_n == 3);
    assert(strcmp(c.by_name[0].key, "neg") == 0 && c.by_name[0].value == -3);
    assert(strcmp(c.by_name[1].key, "one") == 0 && c.by_name[1].value == 1);
    assert(strcmp(c.by_name[2].key, "two") == 0 && c.by_name[2].value == 2);
    assert(c.by_id_n == 2);
    assert(c.by_id[0].key == -1 && scalars_eq(&c.by_id[0].value, &CANONICAL_SCALARS));
    assert(c.by_id[1].key == 42 && c.by_id[1].value.flag == 0 &&
           c.by_id[1].value.u64v == UINT64_MAX);
    assert(scalars_eq(&c.scalars, &CANONICAL_SCALARS));
    assert(c.shape.tag == weaveffi_codec_Shape_Labeled && strcmp(c.shape.label, "tag") == 0 &&
           c.shape.count == 3);
    assert(c.shapes_n == 5);
    assert(c.shapes[0].tag == weaveffi_codec_Shape_Empty);
    assert(c.shapes[1].tag == weaveffi_codec_Shape_Circle && c.shapes[1].radius == 2.5);
    assert(c.shapes[2].tag == weaveffi_codec_Shape_Rect && c.shapes[2].width == 1.0f &&
           c.shapes[2].height == 0.5f);
    assert(c.shapes[3].tag == weaveffi_codec_Shape_Labeled && strcmp(c.shapes[3].label, "") == 0 &&
           c.shapes[3].count == -1);
    assert(c.shapes[4].tag == weaveffi_codec_Shape_Nested &&
           scalars_eq(&c.shapes[4].inner, &CANONICAL_SCALARS) && c.shapes[4].has_note &&
           strcmp(c.shapes[4].note, "n") == 0);
    assert(c.has_maybe_shape && c.maybe_shape.tag == weaveffi_codec_Shape_Nested &&
           !c.maybe_shape.has_note && scalars_eq(&c.maybe_shape.inner, &CANONICAL_SCALARS));
    assert(c.has_maybe_list && c.maybe_list_n == 2 && c.maybe_list[0] == 9 && c.maybe_list[1] == 8);
    assert(c.sparse_n == 3);
    assert(c.sparse[0].has && c.sparse[0].value == 1);
    assert(!c.sparse[1].has);
    assert(c.sparse[2].has && c.sparse[2].value == 0);
    assert(c.colors_n == 3 && c.colors[0] == weaveffi_codec_Color_Red &&
           c.colors[1] == weaveffi_codec_Color_Green && c.colors[2] == weaveffi_codec_Color_Blue);

    // Consumer encodes, producer decodes.
    assert(verify_composite(&c, &err) && err.code == 0);

    // Consumer encodes, producer re-encodes, consumer decodes again.
    composite_t c2;
    roundtrip_composite(&c, &c2);
    assert(composite_eq(&c, &c2));

    // Perturb one deeply nested field: Mismatch.
    c2.sparse[1].has = 1;
    c2.sparse[1].value = 1;
    assert(!verify_composite(&c2, &err));
    assert(err.code == weaveffi_codec_CodecError_Mismatch);
    weaveffi_error_clear(&err);
    composite_free(&c2);

    // describe_composite renders what the producer saw.
    {
        wv_writer w;
        wv_w_init(&w);
        write_composite(&w, &c);
        const char* text = weaveffi_codec_describe_composite(w.buf, w.len, &err);
        assert(err.code == 0 && text != NULL);
        assert(strstr(text, "name: \"h\xC3\xA9llo w\xC3\xB6rld \xE2\x9C\x93\"") != NULL);
        assert(strstr(text, "some_i64: Some(-9223372036854775808)") != NULL);
        assert(strstr(text, "shape: Labeled { label: \"tag\", count: 3 }") != NULL);
        weaveffi_free_string(text);
        wv_w_free(&w);
    }
    composite_free(&c);

    // Hand-built edge composite: everything empty or absent, unicode name,
    // maps with extreme keys, a list of absent optionals.
    composite_t e;
    memset(&e, 0, sizeof e);
    e.name = strdup("\xE6\x97\xA5\xE6\x9C\xAC\xE8\xAA\x9E \xF0\x9F\x8E\x89");  // 日本語 🎉
    e.blob = (uint8_t*)xcalloc(0, 1);
    e.blob_len = 0;
    e.has_some_i64 = 1;
    e.some_i64 = INT64_MAX;
    e.has_none_i64 = 1;
    e.none_i64 = -1;
    e.has_some_text = 0;
    e.names_n = 0;
    e.names = (char**)xcalloc(0, sizeof(char*));
    e.matrix_n = 1;
    e.matrix = (i32_list_t*)xcalloc(1, sizeof(i32_list_t));
    e.matrix[0].n = 2;
    e.matrix[0].v = (int32_t*)xcalloc(2, sizeof(int32_t));
    e.matrix[0].v[0] = INT32_MIN;
    e.matrix[0].v[1] = INT32_MAX;
    e.empty_n = 2;
    e.empty = (double*)xcalloc(2, sizeof(double));
    e.empty[0] = -0.0;
    e.empty[1] = NAN;
    e.by_name_n = 1;
    e.by_name = (by_name_t*)xcalloc(1, sizeof(by_name_t));
    e.by_name[0].key = strdup("");
    e.by_name[0].value = INT64_MIN;
    e.by_id_n = 0;
    e.by_id = (by_id_t*)xcalloc(0, sizeof(by_id_t));
    e.scalars = edge;
    e.shape = shape_empty();
    e.shapes_n = 0;
    e.shapes = (shape_t*)xcalloc(0, sizeof(shape_t));
    e.has_maybe_shape = 0;
    e.has_maybe_list = 1;
    e.maybe_list_n = 0;
    e.maybe_list = (uint8_t*)xcalloc(0, 1);
    e.sparse_n = 2;
    e.sparse = (opt_bool_t*)xcalloc(2, sizeof(opt_bool_t));
    e.colors_n = 0;
    e.colors = (int32_t*)xcalloc(0, sizeof(int32_t));
    composite_t e2;
    roundtrip_composite(&e, &e2);
    assert(composite_eq(&e, &e2));
    assert(strcmp(e2.name, e.name) == 0 && e2.blob_len == 0 && e2.has_maybe_list &&
           e2.maybe_list_n == 0 && isnan(e2.empty[1]) && signbit(e2.empty[0]));
    assert(!verify_composite(&e, &err) && err.code == 1);
    weaveffi_error_clear(&err);
    composite_free(&e2);
    composite_free(&e);

    // ── Shape variants through roundtrip_shape and describe_shape ───────
    shape_t variants[5];
    variants[0] = shape_empty();
    variants[1] = shape_circle(2.5);
    variants[2] = shape_rect(1.0f, 0.5f);
    variants[3] = shape_labeled("tag", 3);
    variants[4] = shape_nested(&CANONICAL_SCALARS, "n");
    const char* descriptions[5] = {
        "Empty",
        "Circle { radius: 2.5 }",
        "Rect { width: 1.0, height: 0.5 }",
        "Labeled { label: \"tag\", count: 3 }",
        NULL,
    };
    for (int i = 0; i < 5; i++) {
        shape_t back;
        roundtrip_shape(&variants[i], &back);
        assert(shape_eq(&variants[i], &back));
        char* text = describe_shape(&back);
        if (descriptions[i]) {
            assert(strcmp(text, descriptions[i]) == 0);
        } else {
            assert(strstr(text, "Nested { inner: Scalars { i8_value: -8,") != NULL &&
                   strstr(text, "note: Some(\"n\") }") != NULL);
        }
        free(text);
        shape_free(&back);
    }
    {
        shape_t no_note = shape_nested(&edge, NULL);
        shape_t back;
        roundtrip_shape(&no_note, &back);
        assert(shape_eq(&no_note, &back) && !back.has_note);
        shape_free(&back);
        shape_free(&no_note);

        shape_t huge = shape_labeled("", INT32_MIN);
        roundtrip_shape(&huge, &back);
        assert(shape_eq(&huge, &back));
        shape_free(&back);
        shape_free(&huge);
    }

    // roundtrip_shapes: a list of every variant.
    {
        wv_writer w;
        wv_w_init(&w);
        wv_put_u32(&w, 5);
        for (int i = 0; i < 5; i++) write_shape(&w, &variants[i]);
        size_t len = 0;
        const uint8_t* p = weaveffi_codec_roundtrip_shapes(w.buf, w.len, &len, &err);
        assert(err.code == 0);
        assert(len == w.len && memcmp(p, w.buf, len) == 0);
        wv_w_free(&w);
        TAKE_BUFFER(p, len, {
            assert(wv_get_u32(&r_) == 5);
            for (int i = 0; i < 5; i++) {
                shape_t back;
                read_shape(&r_, &back);
                assert(shape_eq(&variants[i], &back));
                shape_free(&back);
            }
        });
        wv_w_init(&w);
        wv_put_u32(&w, 0);
        p = weaveffi_codec_roundtrip_shapes(w.buf, w.len, &len, &err);
        assert(err.code == 0);
        TAKE_BUFFER(p, len, assert(wv_get_u32(&r_) == 0));
        wv_w_free(&w);
    }
    for (int i = 0; i < 5; i++) shape_free(&variants[i]);

    // A malformed buffer (truncated mid-value) is a marshalling failure (-3).
    {
        uint8_t truncated[3] = {1, 0, 0};
        size_t len = 0;
        assert(weaveffi_codec_roundtrip_shape(truncated, sizeof truncated, &len, &err) == NULL);
        assert(err.code == -3);
        weaveffi_error_clear(&err);
    }

    // ── Standalone optional, map, string, bytes, and direct scalars ──────
    {
        wv_writer w;
        wv_w_init(&w);
        wv_put_bool(&w, 1);
        wv_put_i64(&w, -1);
        size_t len = 0;
        const uint8_t* p = weaveffi_codec_roundtrip_opt_i64(w.buf, w.len, &len, &err);
        assert(err.code == 0);
        TAKE_BUFFER(p, len, { assert(wv_get_bool(&r_) == 1 && wv_get_i64(&r_) == -1); });
        wv_w_free(&w);
        const uint8_t none[1] = {0};
        p = weaveffi_codec_roundtrip_opt_i64(none, 1, &len, &err);
        assert(err.code == 0);
        TAKE_BUFFER(p, len, assert(wv_get_bool(&r_) == 0));

        // Map: encoded out of key order, comes back sorted by the BTreeMap.
        wv_w_init(&w);
        wv_put_u32(&w, 3);
        wv_put_str(&w, "b");
        wv_put_i64(&w, -2);
        wv_put_str(&w, "a");
        wv_put_i64(&w, 1);
        wv_put_str(&w, "");
        wv_put_i64(&w, INT64_MAX);
        p = weaveffi_codec_roundtrip_map(w.buf, w.len, &len, &err);
        assert(err.code == 0);
        TAKE_BUFFER(p, len, {
            assert(wv_get_u32(&r_) == 3);
            char* k = wv_get_str(&r_);
            assert(strcmp(k, "") == 0 && wv_get_i64(&r_) == INT64_MAX);
            free(k);
            k = wv_get_str(&r_);
            assert(strcmp(k, "a") == 0 && wv_get_i64(&r_) == 1);
            free(k);
            k = wv_get_str(&r_);
            assert(strcmp(k, "b") == 0 && wv_get_i64(&r_) == -2);
            free(k);
        });
        wv_w_free(&w);
        wv_w_init(&w);
        wv_put_u32(&w, 0);
        p = weaveffi_codec_roundtrip_map(w.buf, w.len, &len, &err);
        assert(err.code == 0);
        TAKE_BUFFER(p, len, assert(wv_get_u32(&r_) == 0));
        wv_w_free(&w);

        // Strings and bytes keep their direct ABI.
        const char* text = weaveffi_codec_roundtrip_string("h\xC3\xA9llo \xE2\x9C\x93", &err);
        assert(err.code == 0 && strcmp(text, "h\xC3\xA9llo \xE2\x9C\x93") == 0);
        weaveffi_free_string(text);
        text = weaveffi_codec_roundtrip_string("", &err);
        assert(err.code == 0 && strcmp(text, "") == 0);
        weaveffi_free_string(text);
        assert(weaveffi_codec_roundtrip_string(NULL, &err) == NULL && err.code == -3);
        weaveffi_error_clear(&err);
        const uint8_t raw[4] = {0, 255, 0, 128};
        p = weaveffi_codec_roundtrip_bytes(raw, 4, &len, &err);
        assert(err.code == 0 && len == 4 && memcmp(p, raw, 4) == 0);
        weaveffi_free_bytes((uint8_t*)p, len);
        p = weaveffi_codec_roundtrip_bytes(raw, 0, &len, &err);
        assert(err.code == 0 && len == 0);
        weaveffi_free_bytes((uint8_t*)p, len);

        // Direct family.
        assert(weaveffi_codec_roundtrip_i64(INT64_MIN, &err) == INT64_MIN);
        assert(weaveffi_codec_roundtrip_i64(INT64_MAX, &err) == INT64_MAX);
        assert(weaveffi_codec_roundtrip_u64(UINT64_MAX, &err) == UINT64_MAX);
        assert(weaveffi_codec_roundtrip_u64(1ULL << 63, &err) == (1ULL << 63));
        assert(isnan(weaveffi_codec_roundtrip_f64(NAN, &err)));
        assert(signbit(weaveffi_codec_roundtrip_f64(-0.0, &err)));
        assert(weaveffi_codec_roundtrip_f64(INFINITY, &err) == INFINITY);
        assert(weaveffi_codec_roundtrip_f64(-2.25e100, &err) == -2.25e100);
        assert(weaveffi_codec_roundtrip_bool(true, &err) == true);
        assert(weaveffi_codec_roundtrip_bool(false, &err) == false);
        assert(weaveffi_codec_roundtrip_color(weaveffi_codec_Color_Blue, &err) ==
               weaveffi_codec_Color_Blue);
        assert(weaveffi_codec_roundtrip_color(weaveffi_codec_Color_Red, &err) == 0);
        assert(err.code == 0);
        weaveffi_codec_roundtrip_color((weaveffi_codec_Color)3, &err);
        assert(err.code == -3 && "an undeclared discriminant is a marshalling failure");
        weaveffi_error_clear(&err);
    }

    // ── Token objects and Holder (objects inside buffers) ────────────────
    weaveffi_codec_Token* t = weaveffi_codec_Token_new(5, &err);
    assert(err.code == 0 && t != NULL);
    assert(token_value(t) == 5);
    weaveffi_codec_Token* t2 = weaveffi_codec_Token_clone(t);
    assert(t2 == t);
    weaveffi_codec_Token_destroy(t);
    assert(token_value(t2) == 5 && "clone survives destroying the original");
    weaveffi_codec_Token_destroy(t2);
    weaveffi_codec_Token_destroy(NULL);
    assert(weaveffi_codec_Token_clone(NULL) == NULL);

    holder_t h;
    make_holder(10, 1, &h);
    assert(token_value(h.primary) == 10);
    assert(h.spare != NULL && token_value(h.spare) == 11);
    assert(h.many_n == 3);
    assert(token_value(h.many[0]) == 12 && token_value(h.many[1]) == 13 &&
           token_value(h.many[2]) == 14);
    assert(h.primary != h.spare && h.primary != h.many[0]);

    // sum_holder consumes one encoding (one reference per token).
    {
        wv_writer w;
        wv_w_init(&w);
        write_holder(&w, &h);
        assert(weaveffi_codec_sum_holder(w.buf, w.len, &err) == 10 + 11 + 12 + 13 + 14);
        assert(err.code == 0);
        wv_w_free(&w);
        assert(token_value(h.primary) == 10 && "our references are intact");
    }

    // primary_of returns the very same object as holder.primary, as an
    // owned reference we release; the holder's reference stays valid.
    {
        wv_writer w;
        wv_w_init(&w);
        write_holder(&w, &h);
        weaveffi_codec_Token* primary = weaveffi_codec_primary_of(w.buf, w.len, &err);
        assert(err.code == 0);
        assert(primary == h.primary);
        wv_w_free(&w);
        assert(token_value(primary) == 10);
        weaveffi_codec_Token_destroy(primary);
        assert(token_value(h.primary) == 10);
    }

    // same_primary: two encodings of the same holder share a primary; a
    // different holder does not.
    holder_t h2;
    make_holder(100, 0, &h2);
    assert(h2.spare == NULL && h2.many_n == 3 && token_value(h2.primary) == 100);
    {
        wv_writer a, b;
        wv_w_init(&a);
        wv_w_init(&b);
        write_holder(&a, &h);
        write_holder(&b, &h);
        assert(weaveffi_codec_same_primary(a.buf, a.len, b.buf, b.len, &err));
        assert(err.code == 0);
        wv_w_free(&a);
        wv_w_free(&b);
        wv_w_init(&a);
        wv_w_init(&b);
        write_holder(&a, &h);
        write_holder(&b, &h2);
        assert(!weaveffi_codec_same_primary(a.buf, a.len, b.buf, b.len, &err));
        assert(err.code == 0);
        wv_w_free(&a);
        wv_w_free(&b);

        // A holder mixing our own tokens with the producer's.
        holder_t mixed;
        memset(&mixed, 0, sizeof mixed);
        mixed.primary = weaveffi_codec_Token_new(1000, &err);
        mixed.spare = weaveffi_codec_Token_clone(h.primary);
        mixed.many_n = 2;
        mixed.many[0] = weaveffi_codec_Token_clone(h2.primary);
        mixed.many[1] = weaveffi_codec_Token_new(-1, &err);
        wv_w_init(&a);
        write_holder(&a, &mixed);
        assert(weaveffi_codec_sum_holder(a.buf, a.len, &err) == 1000 + 10 + 100 - 1);
        wv_w_free(&a);
        wv_w_init(&a);
        write_holder(&a, &mixed);
        weaveffi_codec_Token* p = weaveffi_codec_primary_of(a.buf, a.len, &err);
        assert(p == mixed.primary && token_value(p) == 1000);
        weaveffi_codec_Token_destroy(p);
        wv_w_free(&a);
        holder_release(&mixed);
        assert(token_value(h.primary) == 10 && token_value(h2.primary) == 100 &&
               "the clones written into `mixed` never touched our references");
    }
    holder_release(&h2);
    holder_release(&h);

    printf("c/codec: OK\n");
    return 0;
}
