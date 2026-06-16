// Conformance consumer: shapes sample, C target.
//
// Includes the *generated* C header and links the shapes cdylib, exercising
// rich (algebraic) enums — opaque object + tag reader + per-variant
// constructors and field getters + destructor — plus the expanded numeric set
// (f32 fields, u8 field, u64 return). Exits 0 on success; aborts on any failed
// assertion.

#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <string.h>

#include "weaveffi.h"

int main(void) {
    weaveffi_error err = {0, NULL};

    // Empty (unit variant): tag only.
    weaveffi_shapes_Shape* empty = weaveffi_shapes_Shape_Empty_new(&err);
    assert(err.code == 0);
    assert(weaveffi_shapes_Shape_tag(empty) == weaveffi_shapes_Shape_Empty);

    // Circle (f64 payload).
    weaveffi_shapes_Shape* circle = weaveffi_shapes_Shape_Circle_new(2.5, &err);
    assert(err.code == 0);
    assert(weaveffi_shapes_Shape_tag(circle) == weaveffi_shapes_Shape_Circle);
    assert(fabs(weaveffi_shapes_Shape_Circle_get_radius(circle) - 2.5) < 1e-9);

    // Rectangle (two f32 payloads).
    weaveffi_shapes_Shape* rect = weaveffi_shapes_Shape_Rectangle_new(3.0f, 4.0f, &err);
    assert(err.code == 0);
    assert(weaveffi_shapes_Shape_tag(rect) == weaveffi_shapes_Shape_Rectangle);
    assert(fabsf(weaveffi_shapes_Shape_Rectangle_get_width(rect) - 3.0f) < 1e-6f);
    assert(fabsf(weaveffi_shapes_Shape_Rectangle_get_height(rect) - 4.0f) < 1e-6f);

    // Labeled (string + u8 payload).
    weaveffi_shapes_Shape* labeled = weaveffi_shapes_Shape_Labeled_new("hex", 6, &err);
    assert(err.code == 0);
    assert(weaveffi_shapes_Shape_tag(labeled) == weaveffi_shapes_Shape_Labeled);
    const char* label = weaveffi_shapes_Shape_Labeled_get_label(labeled);
    assert(label != NULL && strcmp(label, "hex") == 0);
    weaveffi_free_string(label);
    assert(weaveffi_shapes_Shape_Labeled_get_count(labeled) == 6);

    // describe: dispatch on the active variant.
    const char* desc = weaveffi_shapes_describe(circle, &err);
    assert(err.code == 0);
    assert(strcmp(desc, "circle(r=2.5)") == 0);
    weaveffi_free_string(desc);

    // scale: rich enum in and out.
    weaveffi_shapes_Shape* big = weaveffi_shapes_scale(circle, 4.0, &err);
    assert(err.code == 0);
    assert(weaveffi_shapes_Shape_tag(big) == weaveffi_shapes_Shape_Circle);
    assert(fabs(weaveffi_shapes_Shape_Circle_get_radius(big) - 10.0) < 1e-9);

    // numerics: list<u8> in, u64 out.
    uint8_t bytes[4] = {250, 250, 250, 250};
    uint64_t total = weaveffi_shapes_sum_bytes(bytes, 4, &err);
    assert(err.code == 0);
    assert(total == 1000);

    weaveffi_shapes_Shape_destroy(big);
    weaveffi_shapes_Shape_destroy(labeled);
    weaveffi_shapes_Shape_destroy(rect);
    weaveffi_shapes_Shape_destroy(circle);
    weaveffi_shapes_Shape_destroy(empty);

    printf("c/shapes: OK\n");
    return 0;
}
