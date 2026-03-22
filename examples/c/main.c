#include <stdio.h>
#include "../../generated/c/weaveffi.h"

int main() {
    struct weaveffi_error err = {0};

    int32_t sum = weaveffi_calculator_add(3, 4, &err);
    if (err.code) { printf("add error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }
    printf("add(3,4) = %d\n", sum);

    int32_t prod = weaveffi_calculator_mul(5, 6, &err);
    if (err.code) { printf("mul error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }
    printf("mul(5,6) = %d\n", prod);

    int32_t q = weaveffi_calculator_div(10, 2, &err);
    if (err.code) { printf("div error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }
    printf("div(10,2) = %d\n", q);

    const char* msg = "hello";
    const char* echoed = weaveffi_calculator_echo(msg, &err);
    if (err.code) { printf("echo error: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); return 1; }
    printf("echo(hello) = %s\n", echoed);
    weaveffi_free_string(echoed);

    // trigger fallible path
    (void)weaveffi_calculator_div(1, 0, &err);
    if (err.code) { printf("div error expected: %s\n", err.message ? err.message : ""); weaveffi_error_clear(&err); }

    return 0;
}
