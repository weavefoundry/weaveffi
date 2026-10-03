#!/usr/bin/env bash
# Compile the generated C++ wrapper header, together with the C header it
# ships, as C++17 with warnings as errors. The translation unit includes the
# wrapper and then the C header directly, so including both must compile.
set -euo pipefail
dir=$1
hpp=$(ls "$dir"/cpp/*.hpp | head -1)
h=$(ls "$dir"/cpp/*.h | head -1)
printf '#include "%s"\n#include "%s"\nint main() { return 0; }\n' "$hpp" "$h" > "$dir/cpp_check.cpp"
"${CXX:-clang++}" -std=c++17 -Wall -Wextra -Wpedantic -Werror -fsyntax-only "$dir/cpp_check.cpp"
