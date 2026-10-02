#!/usr/bin/env bash
# Byte-compile every generated Python file, then type-check the package with
# mypy when it is available: the `.pyi` stub under --strict and the
# implementation module itself (which catches undefined names and bad calls
# into the runtime). Without mypy, only the byte-compile runs.
set -euo pipefail
dir=$1
find "$dir/python" -name '*.py' -print0 | xargs -0 python3 -m py_compile
if command -v mypy >/dev/null 2>&1; then
    mypy=(mypy)
elif python3 -c "import mypy" >/dev/null 2>&1; then
    mypy=(python3 -m mypy)
else
    echo "note: mypy not found; type checks skipped (pip install mypy)"
    exit 0
fi
for pkg in "$dir"/python/*/; do
    name=$(basename "$pkg")
    "${mypy[@]}" --strict --python-version 3.9 --no-incremental "$pkg$name.pyi"
    "${mypy[@]}" --python-version 3.9 --no-incremental "$pkg$name.py"
done
