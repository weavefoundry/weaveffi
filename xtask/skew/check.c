// The version-skew check (`cargo xtask skew`): built against C bindings
// generated from an edited copy of the calculator API and linked with the
// real calculator library. Exits 0 when the contract check passes and a
// call works, and 3, printing the path, when the check reports a
// declaration the library declares differently.

#include <stdio.h>

#include "calculator.h"

int main(void) {
    if (calculator_abi_version() != CALCULATOR_ABI_VERSION) {
        printf("abi_version\n");
        return 3;
    }
    const char* mismatch = calculator_contract_check();
    if (mismatch != NULL) {
        printf("%s\n", mismatch);
        return 3;
    }
    calculator_error err;
    if (calculator_add(2, 3, &err) != 5 || err.code != 0) {
        fprintf(stderr, "add(2, 3) didn't return 5\n");
        return 1;
    }
    return 0;
}
