// Producer-side conformance: a C backend that *implements* the generated C ABI
// (rather than consuming a prebuilt library) must still export its symbols when
// the shared library is built with hidden default visibility
// (-fvisibility=hidden, the release-build norm and the MSVC default). The
// generated header tags every prototype with the WEAVEFFI_API visibility macro,
// so an implementing definition keeps default visibility and the symbol stays
// exported. Without the macro the symbols would be hidden and unusable, exactly
// the failure reported in https://github.com/weavefoundry/weaveffi/issues/23.
//
// This file implements every symbol the calculator header declares (the user
// functions plus the runtime helpers a non-Rust producer also has to supply)
// with trivial bodies, so it links as a self-contained shared library. The
// harness then checks with `nm` that the symbols are exported.
#include "weaveffi.h"

int32_t weaveffi_calculator_add(int32_t a, int32_t b, weaveffi_error* out_err) {
    (void)out_err;
    return a + b;
}

int32_t weaveffi_calculator_mul(int32_t a, int32_t b, weaveffi_error* out_err) {
    (void)out_err;
    return a * b;
}

int32_t weaveffi_calculator_div(int32_t a, int32_t b, weaveffi_error* out_err) {
    (void)out_err;
    return b != 0 ? a / b : 0;
}

const char* weaveffi_calculator_echo(const char* s, weaveffi_error* out_err) {
    (void)out_err;
    return s;
}

void weaveffi_error_clear(weaveffi_error* err) {
    if (err) {
        err->code = 0;
        err->message = 0;
    }
}

void weaveffi_free_string(const char* ptr) {
    (void)ptr;
}

void weaveffi_free_bytes(uint8_t* ptr, size_t len) {
    (void)ptr;
    (void)len;
}

weaveffi_cancel_token* weaveffi_cancel_token_create(void) {
    return 0;
}

void weaveffi_cancel_token_cancel(weaveffi_cancel_token* token) {
    (void)token;
}

bool weaveffi_cancel_token_is_cancelled(const weaveffi_cancel_token* token) {
    (void)token;
    return false;
}

void weaveffi_cancel_token_destroy(weaveffi_cancel_token* token) {
    (void)token;
}
