// Calculator C++ example.
//
// Demonstrates:
//   * The generated `weaveffi::calculator_*` wrappers, which convert the
//     out-parameter C ABI into ordinary C++ return values.
//   * The `weaveffi::WeaveFFIError` exception type used to surface non-zero
//     error codes from the Rust side as C++ exceptions.

#include <cstdint>
#include <iostream>
#include <string>

#include "weaveffi.hpp"

int main() {
    std::cout << "=== C++ Calculator Example ===\n\n";

    try {
        int32_t sum = weaveffi::calculator_add(3, 4);
        std::cout << "add(3, 4) = " << sum << "\n";

        int32_t prod = weaveffi::calculator_mul(5, 6);
        std::cout << "mul(5, 6) = " << prod << "\n";

        int32_t q = weaveffi::calculator_div(10, 2);
        std::cout << "div(10, 2) = " << q << "\n";

        std::string echoed = weaveffi::calculator_echo("hello");
        std::cout << "echo(\"hello\") = " << echoed << "\n";
    } catch (const weaveffi::WeaveFFIError& e) {
        std::cerr << "WeaveFFI error " << e.code() << ": " << e.what() << "\n";
        return 1;
    }

    // Intentionally trigger the fallible path to show the exception bridge.
    try {
        (void)weaveffi::calculator_div(1, 0);
        std::cerr << "div(1, 0) unexpectedly returned without throwing\n";
        return 1;
    } catch (const weaveffi::WeaveFFIError& e) {
        std::cout << "\ndiv(1, 0) threw WeaveFFIError " << e.code() << ": "
                  << e.what() << "\n";
    }

    return 0;
}
