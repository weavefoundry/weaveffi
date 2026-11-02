#!/usr/bin/env bash
# Check every generated Python package three ways:
#
#   1. byte-compile every file;
#   2. import the package against a stand-in for its native library (every
#      symbol resolves, `{prefix}_abi_version` reports the bindings' revision,
#      and each contract function returns the hash the bindings embed), so
#      every module-level statement runs: each class, dataclass, enum, error
#      alias, `ctypes` binding, and the load-time contract check; then check
#      that every global name a body refers to exists, and build every
#      callback interface's vtable, so every entry is made (a `ctypes`
#      callback, or a libffi closure for a method returning a structure);
#   3. type-check it with mypy --strict: the implementation module is fully
#      annotated (it ships `py.typed` and no separate stub), so this checks
#      both the public surface consumers type against and the bodies.
#
# A missing mypy is handled as lib.sh describes, after the first two checks
# have run.
set -euo pipefail
. "$(dirname "$0")/lib.sh"
. "$(dirname "$0")/../toolchains.sh"
python=$(find_python) || missing "Python 3.10 or newer not found"
dir=$1
find "$dir/python" -name '*.py' -print0 | xargs -0 "$python" -m py_compile
for pkg in "$dir"/python/*/; do
    "$python" -I - "$pkg" <<'EOF'
import builtins
import ctypes
import importlib
import os
import symtable
import sys

pkg = sys.argv[1].rstrip("/")
parent, name = os.path.split(pkg)
sys.path.insert(0, parent)


def bindings():
    module = sys.modules.get(f"{name}.{name}")
    if module is None or not hasattr(module, "_CONTRACT"):
        raise AssertionError("a symbol was called before the contract was defined")
    return module


class Symbol:
    """One stand-in export: only the load-time check may call it."""

    def __init__(self, symbol):
        self.symbol = symbol
        self.restype = None
        self.argtypes = ()

    def __call__(self, *args):
        module = bindings()
        if self.symbol.endswith("_abi_version"):
            return module._ABI_VERSION
        for symbol, expected, _ in module._CONTRACT:
            if symbol == self.symbol:
                return expected
        raise AssertionError(f"{self.symbol} was called while importing")


class Library:
    """A stand-in for the native library: every symbol exists."""

    _name = "<stand-in library>"

    def __getattr__(self, symbol):
        if symbol.startswith("__"):
            raise AttributeError(symbol)
        found = Symbol(symbol)
        setattr(self, symbol, found)
        return found


real = ctypes.CDLL
ctypes.CDLL = lambda *args, **kwargs: Library()
try:
    importlib.import_module(name)
finally:
    ctypes.CDLL = real
module = bindings()
# Every global name a function or class body refers to exists once the
# module has loaded (a check mypy also makes, kept for when it's missing).
known = set(vars(module)) | set(dir(builtins))
with open(module.__file__, encoding="utf-8") as source:
    table = symtable.symtable(source.read(), module.__file__, "exec")
undefined = []
scopes = list(table.get_children())
while scopes:
    scope = scopes.pop()
    scopes.extend(scope.get_children())
    for sym in scope.get_symbols():
        if sym.is_referenced() and sym.is_global() and sym.get_name() not in known:
            undefined.append(f"{scope.get_name()}: {sym.get_name()}")
if undefined:
    raise AssertionError(f"undefined names: {sorted(undefined)}")
built = 0
for attr in sorted(vars(module)):
    if attr.endswith("VtableBuild"):
        vtable = getattr(module, attr)()
        if vtable.size != ctypes.sizeof(vtable) or not all(
            getattr(vtable, field) for field, _ in type(vtable)._fields_[2:]
        ):
            raise AssertionError(f"{attr}() left an entry empty")
        built += 1
print(f"imported {name}: {len(module._CONTRACT)} contract rows, {built} vtables built")
EOF
done
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
