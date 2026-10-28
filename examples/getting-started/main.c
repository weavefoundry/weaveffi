/* A C consumer of the getting-started library, built against the generated
   header (bindings/c/mathlib.h). */
#include <stdio.h>
#include <string.h>
#include "mathlib.h"

int main(void) {
    /* Refuse a library built from different source than the header. */
    if (mathlib_abi_version() != MATHLIB_ABI_VERSION || mathlib_contract_check() != NULL) {
        fprintf(stderr, "mathlib.h doesn't match the loaded library\n");
        return 1;
    }

    mathlib_error err; /* every call overwrites it */
    printf("2 + 3 = %d\n", mathlib_add(2, 3, &err));

    mathlib_div(1, 0, &err);
    if (err.code == mathlib_MathError_DivisionByZero) {
        printf("error %d: %.*s\n", err.code, (int)err.message_len, (const char*)err.message_ptr);
        mathlib_error_clear(&err);
    }

    /* A string crosses as a run: its UTF-8 bytes and their length, no NUL. */
    const char* name = "C";
    mathlib_run arg = {(const uint8_t*)name, strlen(name)};
    mathlib_run greeting = mathlib_greet(arg, &err);
    printf("%.*s\n", (int)greeting.len, (const char*)greeting.ptr);
    mathlib_run_free(greeting);
    return 0;
}
