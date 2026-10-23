#!/usr/bin/env bash
# Byte-compile every generated Python file, then type-check each package
# with mypy --strict: the implementation module is fully annotated (it ships
# `py.typed` and no separate stub), so this checks both the public surface
# consumers type against and the bodies (undefined names, bad calls into the
# runtime). A missing mypy is handled as lib.sh describes, after the
# byte-compile has run.
set -euo pipefail
. "$(dirname "$0")/lib.sh"
. "$(dirname "$0")/../toolchains.sh"
python=$(find_python) || missing "Python 3.10 or newer not found"
dir=$1
find "$dir/python" -name '*.py' -print0 | xargs -0 "$python" -m py_compile
if "$python" -c "import mypy" >/dev/null 2>&1; then
    mypy=("$python" -m mypy)
elif command -v mypy >/dev/null 2>&1; then
    mypy=(mypy)
else
    missing "mypy not found; type checks not run (pip install 'mypy<2')"
fi
for pkg in "$dir"/python/*/; do
    "${mypy[@]}" --strict --python-version 3.10 --no-incremental "$pkg"
done
