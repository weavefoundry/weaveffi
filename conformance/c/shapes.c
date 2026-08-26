// Conformance consumer: shapes sample, C target.
//
// Includes the *generated* C header and links the shapes cdylib, exercising
// rich (algebraic) enums crossing the ABI as value buffers (i32 tag followed
// by the active variant's fields) plus the expanded numeric set (f32 fields,
// u8 field, u64 return). Exits 0 on success; aborts on any failed assertion.

#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

#include "weaveffi.h"
#include "wvbuf.h"

// Shape variant tags, in declaration order from the IDL.
enum { SHAPE_EMPTY = 0, SHAPE_CIRCLE = 1, SHAPE_RECTANGLE = 2, SHAPE_LABELED = 3 };

int main(void) {
    weaveffi_error err = {0};

    // Encode Circle { radius: 2.5 }: i32 tag + f64 radius.
    wv_writer circle;
    wv_w_init(&circle);
    wv_put_i32(&circle, SHAPE_CIRCLE);
    wv_put_f64(&circle, 2.5);

    // describe: buffered rich-enum parameter, string return.
    const char* desc = weaveffi_shapes_describe(circle.buf, circle.len, &err);
    assert(err.code == 0);
    assert(strcmp(desc, "circle(r=2.5)") == 0);
    weaveffi_free_string(desc);

    // Rectangle { width: 3.0f32, height: 4.0f32 } dispatches on its tag too.
    wv_writer rect;
    wv_w_init(&rect);
    wv_put_i32(&rect, SHAPE_RECTANGLE);
    wv_put_f32(&rect, 3.0f);
    wv_put_f32(&rect, 4.0f);
    desc = weaveffi_shapes_describe(rect.buf, rect.len, &err);
    assert(err.code == 0);
    assert(strstr(desc, "rect") != NULL);
    weaveffi_free_string(desc);
    wv_w_free(&rect);

    // Labeled { label: "hex", count: 6 }: string + u8 payload.
    wv_writer labeled;
    wv_w_init(&labeled);
    wv_put_i32(&labeled, SHAPE_LABELED);
    wv_put_str(&labeled, "hex");
    wv_put_u8(&labeled, 6);
    desc = weaveffi_shapes_describe(labeled.buf, labeled.len, &err);
    assert(err.code == 0);
    assert(strstr(desc, "hex") != NULL);
    weaveffi_free_string(desc);
    wv_w_free(&labeled);

    // Empty: tag only.
    wv_writer empty;
    wv_w_init(&empty);
    wv_put_i32(&empty, SHAPE_EMPTY);
    desc = weaveffi_shapes_describe(empty.buf, empty.len, &err);
    assert(err.code == 0);
    weaveffi_free_string(desc);
    wv_w_free(&empty);

    // scale: rich enum in and out. The returned buffer holds the scaled shape.
    size_t out_len = 0;
    const uint8_t* big =
        weaveffi_shapes_scale(circle.buf, circle.len, 4.0, &out_len, &err);
    assert(err.code == 0 && big != NULL);
    wv_reader r;
    wv_r_init(&r, big, out_len);
    assert(wv_get_i32(&r) == SHAPE_CIRCLE);
    assert(fabs(wv_get_f64(&r) - 10.0) < 1e-9);
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)big, out_len);
    wv_w_free(&circle);

    // A C-style enum keeps its plain int32 constants.
    assert(weaveffi_shapes_Channel_Green == 1);

    // numerics: `[u8]` canonicalizes to `bytes`, so it keeps the raw
    // pointer-plus-length ABI (no buffer framing).
    uint8_t raw[4] = {250, 250, 250, 250};
    uint64_t total = weaveffi_shapes_sum_bytes(raw, sizeof raw, &err);
    assert(err.code == 0);
    assert(total == 1000);

    printf("c/shapes: OK\n");
    return 0;
}
