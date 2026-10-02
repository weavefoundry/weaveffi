#!/usr/bin/env bash
# Compile every generated C header (the ABI header and, when present, the
# value-buffer helper header) as C11 and C++17 with warnings as errors.
set -euo pipefail
dir=$1
for header in "$dir"/c/*.h; do
    name=$(basename "$header" .h)
    src="$dir/c_check_$name"
    printf '#include "%s.h"\nint main(void) { return 0; }\n' "$name" > "$src.c"
    cc -std=c11 -Wall -Wextra -Wpedantic -Werror -fsyntax-only -I "$dir/c" "$src.c"
    cp "$src.c" "$src.cpp"
    c++ -std=c++17 -Wall -Wextra -Wpedantic -Werror -fsyntax-only -I "$dir/c" "$src.cpp"
done
