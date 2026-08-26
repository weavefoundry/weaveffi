// Minimal WeaveFFI value-buffer writer/reader for the C conformance
// consumers. Implements the wire format specified in
// docs/src/reference/value-buffers.md: little-endian, packed, u32 length
// prefixes. Consumers assert on malformed input, which is exactly the
// contract-violation behavior the spec prescribes for generated bindings.
#ifndef WVBUF_H
#define WVBUF_H

#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// ── writer ──────────────────────────────────────────────────────────────────

typedef struct {
    uint8_t* buf;
    size_t len;
    size_t cap;
} wv_writer;

static inline void wv_w_init(wv_writer* w) {
    w->buf = NULL;
    w->len = 0;
    w->cap = 0;
}

static inline void wv_w_free(wv_writer* w) {
    free(w->buf);
    w->buf = NULL;
    w->len = w->cap = 0;
}

static inline void wv_put_raw(wv_writer* w, const void* p, size_t n) {
    if (w->len + n > w->cap) {
        size_t cap = w->cap ? w->cap * 2 : 64;
        while (cap < w->len + n) cap *= 2;
        w->buf = (uint8_t*)realloc(w->buf, cap);
        assert(w->buf != NULL);
        w->cap = cap;
    }
    memcpy(w->buf + w->len, p, n);
    w->len += n;
}

static inline void wv_put_u8(wv_writer* w, uint8_t v) { wv_put_raw(w, &v, 1); }
static inline void wv_put_bool(wv_writer* w, int v) { wv_put_u8(w, v ? 1 : 0); }

static inline void wv_put_u32(wv_writer* w, uint32_t v) {
    uint8_t b[4] = {(uint8_t)v, (uint8_t)(v >> 8), (uint8_t)(v >> 16),
                    (uint8_t)(v >> 24)};
    wv_put_raw(w, b, 4);
}

static inline void wv_put_i32(wv_writer* w, int32_t v) {
    wv_put_u32(w, (uint32_t)v);
}

static inline void wv_put_u64(wv_writer* w, uint64_t v) {
    uint8_t b[8];
    for (int i = 0; i < 8; i++) b[i] = (uint8_t)(v >> (8 * i));
    wv_put_raw(w, b, 8);
}

static inline void wv_put_i64(wv_writer* w, int64_t v) {
    wv_put_u64(w, (uint64_t)v);
}

static inline void wv_put_f32(wv_writer* w, float v) {
    uint32_t bits;
    memcpy(&bits, &v, 4);
    wv_put_u32(w, bits);
}

static inline void wv_put_f64(wv_writer* w, double v) {
    uint64_t bits;
    memcpy(&bits, &v, 8);
    wv_put_u64(w, bits);
}

static inline void wv_put_str(wv_writer* w, const char* s) {
    size_t n = strlen(s);
    wv_put_u32(w, (uint32_t)n);
    wv_put_raw(w, s, n);
}

static inline void wv_put_bytes(wv_writer* w, const uint8_t* p, size_t n) {
    wv_put_u32(w, (uint32_t)n);
    wv_put_raw(w, p, n);
}

// ── reader ──────────────────────────────────────────────────────────────────

typedef struct {
    const uint8_t* p;
    size_t len;
    size_t off;
} wv_reader;

static inline void wv_r_init(wv_reader* r, const uint8_t* p, size_t len) {
    r->p = p;
    r->len = len;
    r->off = 0;
}

static inline const uint8_t* wv_take(wv_reader* r, size_t n) {
    assert(r->off + n <= r->len && "value buffer exhausted");
    const uint8_t* at = r->p + r->off;
    r->off += n;
    return at;
}

static inline uint8_t wv_get_u8(wv_reader* r) { return *wv_take(r, 1); }

static inline int wv_get_bool(wv_reader* r) {
    uint8_t v = wv_get_u8(r);
    assert(v <= 1 && "invalid bool byte");
    return v;
}

static inline uint32_t wv_get_u32(wv_reader* r) {
    const uint8_t* b = wv_take(r, 4);
    return (uint32_t)b[0] | ((uint32_t)b[1] << 8) | ((uint32_t)b[2] << 16) |
           ((uint32_t)b[3] << 24);
}

static inline int32_t wv_get_i32(wv_reader* r) { return (int32_t)wv_get_u32(r); }

static inline uint64_t wv_get_u64(wv_reader* r) {
    const uint8_t* b = wv_take(r, 8);
    uint64_t v = 0;
    for (int i = 0; i < 8; i++) v |= (uint64_t)b[i] << (8 * i);
    return v;
}

static inline int64_t wv_get_i64(wv_reader* r) { return (int64_t)wv_get_u64(r); }

static inline float wv_get_f32(wv_reader* r) {
    uint32_t bits = wv_get_u32(r);
    float v;
    memcpy(&v, &bits, 4);
    return v;
}

static inline double wv_get_f64(wv_reader* r) {
    uint64_t bits = wv_get_u64(r);
    double v;
    memcpy(&v, &bits, 8);
    return v;
}

// Returns a heap-allocated NUL-terminated copy; caller frees with free().
static inline char* wv_get_str(wv_reader* r) {
    uint32_t n = wv_get_u32(r);
    const uint8_t* at = wv_take(r, n);
    char* s = (char*)malloc((size_t)n + 1);
    assert(s != NULL);
    memcpy(s, at, n);
    s[n] = '\0';
    return s;
}

static inline void wv_r_expect_end(const wv_reader* r) {
    assert(r->off == r->len && "trailing bytes after value");
}

#endif  // WVBUF_H
