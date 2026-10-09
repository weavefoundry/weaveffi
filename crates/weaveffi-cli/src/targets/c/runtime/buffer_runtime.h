/* ---------------------------------------------------------------------------
 * Value buffer runtime.
 *
 * The wire format is little-endian and packed: fixed-width scalars at their
 * natural width, `bool` as one byte, strings and bytes as a `uint32_t` length
 * followed by the raw bytes (no NUL terminator), optionals as a presence byte
 * followed by the value, lists as a `uint32_t` count followed by the
 * elements, maps as a `uint32_t` count followed by alternating keys and
 * values, and objects as a `uint64_t` token holding one strong reference.
 * ------------------------------------------------------------------------- */

/* A UTF-8 string view: `len` bytes at `ptr`, which may contain NUL bytes.
   A decoded string owns `ptr` and also NUL-terminates it, so `ptr` can be
   passed to C string functions when the text has no interior NUL. */
typedef struct {{p}}_str {
    const char* ptr;
    size_t len;
} {{p}}_str;

/* A byte string view: `len` bytes at `ptr`. A decoded value owns `ptr`. */
typedef struct {{p}}_bytes {
    const uint8_t* ptr;
    size_t len;
} {{p}}_bytes;

/* Borrow a NUL-terminated C string (NULL is the empty string). */
static inline {{p}}_str {{p}}_str_of(const char* s) {
    {{p}}_str v;
    v.ptr = s;
    v.len = s ? strlen(s) : 0;
    return v;
}

/* Borrow `len` bytes at `ptr`. */
static inline {{p}}_bytes {{p}}_bytes_of(const uint8_t* ptr, size_t len) {
    {{p}}_bytes v;
    v.ptr = ptr;
    v.len = len;
    return v;
}

/* Release a decoded string. Never call this on a borrowed view. */
static inline void {{p}}_str_free({{p}}_str* s) {
    free((void*)s->ptr);
    s->ptr = NULL;
    s->len = 0;
}

/* Release decoded bytes. Never call this on a borrowed view. */
static inline void {{p}}_bytes_free({{p}}_bytes* b) {
    free((void*)b->ptr);
    b->ptr = NULL;
    b->len = 0;
}

/* A growable output buffer. Zero-initialize it, write values, pass
   `ptr`/`len` to the call, then release it with {{p}}_writer_free. An
   allocation failure sets `failed` and turns later writes into no-ops. */
typedef struct {{p}}_writer {
    uint8_t* ptr;
    size_t len;
    size_t cap;
    bool failed;
} {{p}}_writer;

static inline void {{p}}_writer_free({{p}}_writer* w) {
    free(w->ptr);
    w->ptr = NULL;
    w->len = 0;
    w->cap = 0;
    w->failed = false;
}

static inline void {{p}}_writer_put_raw({{p}}_writer* w, const void* data, size_t n) {
    if (w->failed || n == 0) {
        return;
    }
    if (n > w->cap - w->len) {
        size_t cap = w->cap ? w->cap : 64;
        while (cap - w->len < n) {
            if (cap > SIZE_MAX / 2) {
                w->failed = true;
                return;
            }
            cap *= 2;
        }
        uint8_t* grown = (uint8_t*)realloc(w->ptr, cap);
        if (grown == NULL) {
            w->failed = true;
            return;
        }
        w->ptr = grown;
        w->cap = cap;
    }
    memcpy(w->ptr + w->len, data, n);
    w->len += n;
}

static inline void {{p}}_writer_put_u64({{p}}_writer* w, uint64_t v) {
    uint8_t b[8];
    for (int i = 0; i < 8; i++) {
        b[i] = (uint8_t)(v >> (8 * i));
    }
    {{p}}_writer_put_raw(w, b, 8);
}

static inline void {{p}}_writer_put_u32({{p}}_writer* w, uint32_t v) {
    uint8_t b[4] = {(uint8_t)v, (uint8_t)(v >> 8), (uint8_t)(v >> 16), (uint8_t)(v >> 24)};
    {{p}}_writer_put_raw(w, b, 4);
}

static inline void {{p}}_writer_put_u16({{p}}_writer* w, uint16_t v) {
    uint8_t b[2] = {(uint8_t)v, (uint8_t)(v >> 8)};
    {{p}}_writer_put_raw(w, b, 2);
}

static inline void {{p}}_writer_put_u8({{p}}_writer* w, uint8_t v) {
    {{p}}_writer_put_raw(w, &v, 1);
}

static inline void {{p}}_writer_put_i8({{p}}_writer* w, int8_t v) {
    {{p}}_writer_put_u8(w, (uint8_t)v);
}

static inline void {{p}}_writer_put_i16({{p}}_writer* w, int16_t v) {
    {{p}}_writer_put_u16(w, (uint16_t)v);
}

static inline void {{p}}_writer_put_i32({{p}}_writer* w, int32_t v) {
    {{p}}_writer_put_u32(w, (uint32_t)v);
}

static inline void {{p}}_writer_put_i64({{p}}_writer* w, int64_t v) {
    {{p}}_writer_put_u64(w, (uint64_t)v);
}

static inline void {{p}}_writer_put_bool({{p}}_writer* w, bool v) {
    {{p}}_writer_put_u8(w, v ? 1 : 0);
}

static inline void {{p}}_writer_put_f32({{p}}_writer* w, float v) {
    uint32_t bits;
    memcpy(&bits, &v, 4);
    {{p}}_writer_put_u32(w, bits);
}

static inline void {{p}}_writer_put_f64({{p}}_writer* w, double v) {
    uint64_t bits;
    memcpy(&bits, &v, 8);
    {{p}}_writer_put_u64(w, bits);
}

static inline void {{p}}_writer_put_len({{p}}_writer* w, size_t n) {
    if (n > UINT32_MAX) {
        w->failed = true;
        return;
    }
    {{p}}_writer_put_u32(w, (uint32_t)n);
}

static inline void {{p}}_writer_put_string({{p}}_writer* w, {{p}}_str v) {
    {{p}}_writer_put_len(w, v.len);
    {{p}}_writer_put_raw(w, v.ptr, v.len);
}

static inline void {{p}}_writer_put_bytes({{p}}_writer* w, {{p}}_bytes v) {
    {{p}}_writer_put_len(w, v.len);
    {{p}}_writer_put_raw(w, v.ptr, v.len);
}

/* Write an object token. The token carries one strong reference, so pass
   a reference the buffer may own (the generated codecs pass a fresh
   `_clone`). */
static inline void {{p}}_writer_put_object({{p}}_writer* w, const void* owned) {
    {{p}}_writer_put_u64(w, (uint64_t)(uintptr_t)owned);
}

/* A cursor over an input buffer. A malformed buffer sets `failed`; every
   later read then returns a zero value, so a decoder can always finish and
   release what it built. */
typedef struct {{p}}_reader {
    const uint8_t* ptr;
    size_t len;
    size_t pos;
    bool failed;
} {{p}}_reader;

static inline {{p}}_reader {{p}}_reader_of(const uint8_t* ptr, size_t len) {
    {{p}}_reader r;
    r.ptr = ptr;
    r.len = ptr ? len : 0;
    r.pos = 0;
    r.failed = false;
    return r;
}

/* `true` when every byte was consumed and nothing was malformed. */
static inline bool {{p}}_reader_finish(const {{p}}_reader* r) {
    return !r->failed && r->pos == r->len;
}

static inline const uint8_t* {{p}}_reader_take({{p}}_reader* r, size_t n) {
    if (r->failed || n > r->len - r->pos) {
        r->failed = true;
        return NULL;
    }
    const uint8_t* at = r->ptr + r->pos;
    r->pos += n;
    return at;
}

/* Zeroed storage for `n` decoded elements of `size` bytes (NULL when `n` is
   zero). `min_wire` is the smallest encoding of one element; a count the
   remaining bytes can't hold fails the reader before anything is
   allocated. */
static inline void* {{p}}_reader_alloc({{p}}_reader* r, size_t n, size_t size, size_t min_wire) {
    if (r->failed || n == 0) {
        return NULL;
    }
    if (min_wire != 0 && n > (r->len - r->pos) / min_wire) {
        r->failed = true;
        return NULL;
    }
    void* p = calloc(n, size);
    if (p == NULL) {
        r->failed = true;
    }
    return p;
}

static inline uint64_t {{p}}_reader_get_u64({{p}}_reader* r) {
    const uint8_t* b = {{p}}_reader_take(r, 8);
    uint64_t v = 0;
    if (b != NULL) {
        for (int i = 0; i < 8; i++) {
            v |= (uint64_t)b[i] << (8 * i);
        }
    }
    return v;
}

static inline uint32_t {{p}}_reader_get_u32({{p}}_reader* r) {
    const uint8_t* b = {{p}}_reader_take(r, 4);
    if (b == NULL) {
        return 0;
    }
    return (uint32_t)b[0] | ((uint32_t)b[1] << 8) | ((uint32_t)b[2] << 16) | ((uint32_t)b[3] << 24);
}

static inline uint16_t {{p}}_reader_get_u16({{p}}_reader* r) {
    const uint8_t* b = {{p}}_reader_take(r, 2);
    return b ? (uint16_t)((uint16_t)b[0] | ((uint16_t)b[1] << 8)) : (uint16_t)0;
}

static inline uint8_t {{p}}_reader_get_u8({{p}}_reader* r) {
    const uint8_t* b = {{p}}_reader_take(r, 1);
    return b ? b[0] : (uint8_t)0;
}

static inline int8_t {{p}}_reader_get_i8({{p}}_reader* r) {
    return (int8_t){{p}}_reader_get_u8(r);
}

static inline int16_t {{p}}_reader_get_i16({{p}}_reader* r) {
    return (int16_t){{p}}_reader_get_u16(r);
}

static inline int32_t {{p}}_reader_get_i32({{p}}_reader* r) {
    return (int32_t){{p}}_reader_get_u32(r);
}

static inline int64_t {{p}}_reader_get_i64({{p}}_reader* r) {
    return (int64_t){{p}}_reader_get_u64(r);
}

static inline bool {{p}}_reader_get_bool({{p}}_reader* r) {
    uint8_t v = {{p}}_reader_get_u8(r);
    if (v > 1) {
        r->failed = true;
        return false;
    }
    return v == 1;
}

static inline float {{p}}_reader_get_f32({{p}}_reader* r) {
    uint32_t bits = {{p}}_reader_get_u32(r);
    float v;
    memcpy(&v, &bits, 4);
    return v;
}

static inline double {{p}}_reader_get_f64({{p}}_reader* r) {
    uint64_t bits = {{p}}_reader_get_u64(r);
    double v;
    memcpy(&v, &bits, 8);
    return v;
}

/* A `uint32_t` length followed by that many bytes, copied into a fresh
   NUL-terminated allocation. */
static inline uint8_t* {{p}}_reader_get_run({{p}}_reader* r, size_t* out_len) {
    size_t n = {{p}}_reader_get_u32(r);
    const uint8_t* at = {{p}}_reader_take(r, n);
    *out_len = 0;
    if (at == NULL) {
        return NULL;
    }
    uint8_t* copy = (uint8_t*)malloc(n + 1);
    if (copy == NULL) {
        r->failed = true;
        return NULL;
    }
    memcpy(copy, at, n);
    copy[n] = 0;
    *out_len = n;
    return copy;
}

static inline {{p}}_str {{p}}_reader_get_string({{p}}_reader* r) {
    {{p}}_str v;
    v.ptr = (const char*){{p}}_reader_get_run(r, &v.len);
    return v;
}

static inline {{p}}_bytes {{p}}_reader_get_bytes({{p}}_reader* r) {
    {{p}}_bytes v;
    v.ptr = {{p}}_reader_get_run(r, &v.len);
    return v;
}

/* Read an object token and adopt the strong reference it carries. A zero
   token in a required position fails the reader. */
static inline void* {{p}}_reader_get_object({{p}}_reader* r) {
    uint64_t token = {{p}}_reader_get_u64(r);
    if (token == 0) {
        r->failed = true;
    }
    return (void*)(uintptr_t)token;
}
