# Python

The Python target produces a pip-installable package of pure-Python
`ctypes` bindings over the C ABI (revision 5). There's no compilation step,
no native extension, and no third-party runtime dependency; the package
works on any Python 3.10+ interpreter that can load the producer library.
The implementation module is fully annotated and the package ships
`py.typed`, so type checkers read the bindings themselves (there's no
separate stub to drift out of date) and the generated code passes
`mypy --strict`.

Records and rich enums are frozen dataclasses that cross the boundary as
value buffers. Optional scalars and numeric lists cross directly (a flag
plus a value, and typed arrays). Interfaces are reference-counted wrapper
classes. Callback interfaces are abstract base classes the consumer
subclasses. Async functions are coroutines whose cancellation cancels the
native call, and `iter<T>` returns are lazy `NativeIterator[T]` objects.

Python is a [Tier 1](../stability.md#target-tiers) target: it tracks every
ABI revision as it lands and runs the full conformance suite in CI.

## What gets generated

For the `kvstore` sample (package name `kvstore`, C prefix `kvstore`):

```text
python/
├── pyproject.toml          # PEP 621 metadata, built with setuptools
├── README.md
└── kvstore/                # the import package, named after the C prefix
    ├── __init__.py         # re-exports the module's __all__
    ├── kvstore.py          # runtime, prototypes, and wrappers
    └── py.typed            # PEP 561 marker
```

The distribution name is the package identity's `name` and the import
package is its C prefix, so a crate named `key-store` installs as
`key-store` and imports as `key_store`. Both are configurable:

```toml
[generators.python]
name = "kvstore-python"      # distribution name (default: package name)
import_name = "kvstore_py"   # import package (default: C prefix)
requires_python = ">=3.12"   # default ">=3.10"
```

The module lists its public names in `__all__` (the error classes,
`LibraryLoadError`, `NativeIterator`, every type, and the free functions),
so `from kvstore import *` and the package's `__init__.py` export the API
and none of the runtime's helpers. Annotations use builtin generics and
`X | None`.

## Install and load

```bash
weaveffi generate -o bindings --target python   # in the producer crate
pip install bindings/python
```

At import, the module loads the producer library from the first of:

1. the path in `{PREFIX}_LIBRARY` (for example `KVSTORE_LIBRARY`),
2. a library bundled inside the package directory (what `weaveffi package`
   produces, and where `weaveffi dev` copies the debug build), and
3. the system loader's search path, by platform file name
   (`libkvstore.dylib`, `libkvstore.so`, or `kvstore.dll`).

It then makes the two [load-time checks](../reference/abi.md#load-time-checks)
before binding anything else. `kvstore_abi_version()` must report revision
5, and every top-level module's contract table must contain each
declaration the bindings were generated with, with an equal hash. The
expected rows are embedded in the module, one `(id, hash, path)` per
declaration (every function, member, type, error code, and callback
method), each with its canonical signature:

```python
_CONTRACTS: dict[str, list[tuple[int, int, str]]] = {
    "kvstore_kv_contract": [
        (0x00f1b5c049a1cd69, 0x36b344095a0f5bf9, "kv.Store.rank"),  # method rank(Scorer) -> [string] throws KvError
        ...
    ],
    "kvstore_report_contract": [...],
}
```

When the library can't be loaded, or a check fails, the import raises
`LibraryLoadError`, an `ImportError` subclass whose message says what went
wrong and whose `path` names the library:

```text
kvstore.kvstore.LibraryLoadError: kv.Store.put changed since these bindings were generated (/opt/lib/libkvstore.dylib)
```

The other messages are `kv.Store.put is missing from the library`, a C ABI
mismatch naming both revisions, a library that doesn't export
`kvstore_abi_version`, and the loader's own error with a hint to set
`KVSTORE_LIBRARY`. A stale binding never misreads a buffer at runtime.
Declarations the library has and the bindings don't are fine, so a library
that grew new functions, error codes, or callback methods still loads.
Since the failure happens during the import, catch it as `ImportError`
around the `import` statement.

Every C prototype is then bound once, in a module-level table:

```python
_c_kv_Store_count = _bind("kvstore_kv_Store_count", ctypes.c_uint64, ctypes.c_void_p, ctypes.POINTER(_ErrorStruct))
```

On macOS, the system `python3` strips `DYLD_LIBRARY_PATH`, so prefer
`{PREFIX}_LIBRARY` there.

## Type mapping

Parameters accept abstract containers (any `Sequence` or `Mapping`) and
returns hand out concrete ones (`list`, `dict`).

| IDL type | Python type | Crosses the ABI as |
|---|---|---|
| `i8` to `u64` | `int` | `c_int8` to `c_uint64`, range-checked |
| `f32`, `f64` | `float` | `c_float`, `c_double` |
| `bool` | `bool` | `c_bool` |
| `string` | `str` | UTF-8 bytes plus length (`c_char_p`, `c_size_t`) |
| `bytes` | `bytes` | pointer plus length |
| C-style enum | `IntEnum` subclass | `c_int32` |
| record | frozen `@dataclass` | value buffer |
| rich enum | base class plus one frozen `@dataclass` per variant | value buffer |
| scalar or enum `T?` | `T \| None` | `bool` flag plus the value |
| other `T?` | `T \| None` | value buffer |
| `[i8]` to `[u64]`, `[f32]`, `[f64]` | `Sequence[int]` or `Sequence[float]` in, `list` out | typed array (pointer plus element count) |
| other `[T]`, `{K:V}` | `Sequence[T]`, `Mapping[K, V]` in; `list[T]`, `dict[K, V]` out | value buffer |
| interface | wrapper class | `c_void_p` |
| interface `?` | `... \| None` | `c_void_p`, NULL for `None` |
| callback interface | subclass of the generated ABC | context key plus vtable |
| callback interface `?` | `... \| None` | context key plus vtable, NULL for `None` |
| `iter<T>` | `NativeIterator[T]` | iterator handle |

`ctypes` truncates an out-of-range integer to the parameter's C width
without a word, so every integer argument is checked first: a value out of
range raises `OverflowError` naming the parameter
(`a: 2147483648 is out of range for i32`), and a non-integer raises
`TypeError`. The checks run before anything is lent or registered for the
call, so a failed check leaks nothing.

An optional scalar (`i64?`, `bool?`, `Priority?`) never touches a value
buffer: `None` passes a false flag, and a returned flag says whether the
out value is present. A numeric list parameter accepts any sequence or
iterable of numbers (a `list`, a `tuple`, a `range`, a generator); it's
packed into an `array.array` with range checks and lent to the call, and an
`array.array` of the matching type code is lent as is, without a copy. A
`str` or `bytes` argument is refused rather than reinterpreted. Returned
typed arrays are copied into a `list` and released.

Strings and bytes pass without per-byte loops: a `str` is encoded once and
its bytes are lent to the call, so interior NUL characters survive.
Returned strings, bytes, and buffers are copied with `ctypes.string_at` and
released with `{prefix}_free_bytes`.

Every record and rich enum has one writer and one reader function
(`_write_Entry`, `_read_Entry`), and so does every distinct composite type
the API uses, named by the composite's canonical stem
(`_write_list_string`, `_read_map_string_Store`, `_read_opt_Entry`); a call
site makes one call instead of inlining a loop. Lists of a fixed-width
number type inside a buffer are packed and unpacked with one `struct` call.
A value out of range for its field raises `OverflowError` while encoding. A
decoded buffer that's truncated, has trailing bytes, an invalid flag,
invalid UTF-8, an undeclared enum value, or a repeated map key raises
`InternalError` with code `-3`.

Records are `@dataclass(frozen=True, slots=True)`: they compare and hash
by value (a record holding a `list` or `dict` field compares by value but
can't be hashed), and a changed copy comes from `dataclasses.replace`:

```python
entry = store.get("alpha")
renamed = dataclasses.replace(entry, tags=["archived"])
```

Rich-enum variants work with `isinstance` and with `match`:

```python
match change:
    case kvstore.ChangePut(entry=entry, replaced=True):
        print("replaced", entry.key)
    case kvstore.ChangeRemoved(key=key):
        print("removed", key)
```

All modules share one Python namespace. Nested modules' declarations are
exported next to their parents' under their bare names (`summarize`, not
`kv_stats_summarize`), which global names keep unique. The module starts
with `from __future__ import annotations`, so annotations name types
declared later in the file without quotes. Backticked identifiers in IDL
docs and deprecation messages are rewritten to their Python spellings in
docstrings and `DeprecationWarning`s.

## Objects and lifetime

An interface wrapper holds one strong reference and releases it exactly
once: from `close()`, the `with` statement, or the `__del__` backstop.
`close()` is idempotent, and using a closed wrapper raises `ValueError`.
Each call lends the pointer for its duration, so `close()` from another
thread while a call is in flight defers the release until the call
returns. Two wrappers compare equal (and hash alike) when they hold the
same native object, so `store.share() == store` and a record holding a
`Store` compares by identity. An interface member named `close` is
`close_`, so it never replaces the wrapper's own ([reserved member
names](../reference/naming.md#identifiers-in-generated-code)).

A constructor named `new` becomes `__init__`; other constructors are
`@classmethod` factories:

```python
    @classmethod
    def open(cls, path: str) -> Store:
        _path_b = path.encode("utf-8")
        _err = _ErrorStruct()
        _ret = _c_kv_Store_open(_path_b, len(_path_b), ctypes.byref(_err))
        if _err.code:
            raise _kv_error_from(*_read_error(_err))
        return cls._adopt(_required(_ret))
```

Objects returned by the producer, decoded from a buffer, or passed to a
callback are adopted into new wrappers. Writing an object into a value
buffer (a record field or a list element) mints a new reference only after
the whole argument list has encoded, so a failed encoding leaks nothing.

## Errors

Every declared error derives from the package's root `Error` (named
`{PascalName}Error` if the API declares its own `Error` type), which
carries `code` and `message`. Each error domain is a subclass named by the
shared rule (`KvError` stays `KvError`, `KitchenErrors` becomes
`KitchenError`, `Failure` becomes `FailureError`), and a module may declare
several. Each code is a subclass of its domain named `{Code}Error`
(`KeyNotFoundError`), also reachable as `KvError.KeyNotFound`, carrying its
stable `CODE`. When `{Code}Error` would shadow a Python builtin
(`TimeoutError`) or another declaration, it's qualified with the domain's
stem (`KvTimeoutError`). A code's payload fields are constructor arguments
and attributes; a field named like an exception attribute (`message`,
`code`, `args`) gets a trailing `_`:

```python
try:
    store.get("nope")
except kvstore.KvError.KeyNotFound as e:
    print(e.code, e.key, e.message)  # 1001 nope key not found: nope
```

What a failed call raises follows its `throws`:

- `throws: SomeDomain`: the code's class. Codes are matched within the
  callable's domain, so two domains may reuse a value. Domains are open: a
  positive code these bindings don't know (from a newer library) raises the
  domain's base class with its code and message.
- `throws: any`: the root `Error` with code -1 and the producer's message.
- A negative code on a throwing call (a panic, a marshalling failure, or a
  failed callback) raises the root `Error` with that code; the constants
  are `GENERIC_ERROR_CODE` (-1), `PANIC_ERROR_CODE` (-2),
  `MARSHAL_ERROR_CODE` (-3), and `FOREIGN_ERROR_CODE` (-4).

A call that declares no errors can only fail through a bug, so it follows
the [trap policy](../guides/errors-and-memory.md#the-trap-policy): it
raises `InternalError`, a `RuntimeError` subclass outside the `Error`
hierarchy whose message names the code and the producer's message
(`InternalError: (-3) ...`). A cancelled async call (-5) raises
`asyncio.CancelledError` instead.

Error messages cross as UTF-8 bytes plus a length, so they may contain any
character; an empty message is `""`.

## Async and cancellation

An async function is an `async def` that must run on an `asyncio` event
loop. The launcher receives a static completion trampoline and an integer
key; the trampoline takes ownership of the result on the producer's thread
and settles the future through `call_soon_threadsafe`:

```python
async def open_store(path: str) -> Store:
    _path_b = path.encode("utf-8")
    _call, _future = _async_begin()
    try:
        _c_kv_open_store(_path_b, len(_path_b), _kv_open_store_completion, _call)
    except BaseException:
        _async_abandon(_call)
        raise
    _result: Store = await _future
    return _result
```

Optional scalar and typed-array results arrive directly (`int | None`,
`list[int]`), like sync returns.

For a `cancellable` function, each call owns a native cancel token.
Cancelling the awaiting task (directly, through `asyncio.wait_for`, or by
cancelling a `gather`) cancels the token, waits for the producer's
cancelled completion, and raises `asyncio.CancelledError`. Cancelling the
task awaiting a function that isn't `cancellable` stops the wait only; the
native call runs to completion and its result is dropped.

## Callbacks

A callback interface is an `abc.ABC` with one abstract method per IDL
method. Passing an instance registers it in a handle table under an integer
key, which the producer receives as `ctx` along with the address of the
interface's one static vtable; `None` for an optional callback parameter
passes a null vtable. The vtable starts with the revision 5 header (its
`size`, `flags`, and the shared `free` entry), and the producer's
`free(ctx)` removes the table entry, so an implementation lives exactly as
long as the producer holds it. `flags` is 0: the producer may call any
method from any thread.

Trampolines run on whichever thread the producer calls from; `ctypes`
acquires the GIL first. A trampoline adopts the object arguments first,
converts the rest (an optional scalar arrives as `T | None`, a typed array
as a `list`), calls the method, and hands the return back:

- a number, `bool`, or enum as the C return value, checked for its type
  and range inside the trampoline (so a wrong return is reported as a
  failure rather than truncated);
- an optional scalar through the `out_value` slot, with the C return
  saying whether it's present;
- an object as a fresh strong reference the producer adopts (`None` is
  allowed only for `I?`);
- a `str`, `bytes`, typed array, record, or other buffered value through
  the `out_ptr`/`out_len` slots, in a run allocated with `{prefix}_alloc`
  that the producer adopts and frees.

```python
def _Loader_load(ctx: int | None, key_ptr: int | None, key_len: int, out_ptr: Any, out_len: Any, out_err: Any) -> None:
    try:
        _callback_return_bytes(out_ptr, out_len, bytes(_callback_get(ctx).load(_peek_bytes(key_ptr, key_len).decode("utf-8"))))
    except BaseException as exc:
        _callback_fail(out_err, exc, _GENERIC, KvError)
```

Nothing unwinds through C. A method that throws a domain may raise that
domain's codes, which reach the producer as that code with the fields as
the error payload (`{prefix}_error_set` and `{prefix}_error_set_payload`):

```python
class EmptyLoader(kvstore.Loader):
    def name(self) -> str:
        return "empty"

    def fallback(self, key: str) -> kvstore.Store | None:
        return None

    def load(self, key: str) -> bytes:
        raise kvstore.KeyNotFoundError(key=key)
```

A Rust producer decodes such a code into its typed error, so the caller of
the original function sees the domain's own message rendered from the
fields. Any other exception, or a return of the wrong type, reaches the
producer as code -1 with the exception's message (-4 for a method that
declares no errors); a Rust producer receives it as a `ForeignError`, which
the kvstore sample turns into `KvError.CallbackFailed`.

## Iterators

An `iter<T>` return is a `NativeIterator[T]`, a lazy iterator: each
`next()` makes one native `next` call through the pre-bound function, and
the handle is released on exhaustion, `close()`, the end of a `with` block,
or garbage collection. A closed iterator is exhausted.

```python
for key in store.keys(None):
    print(key)

with store.entries("user.") as entries:
    first = next(entries)   # the rest is released when the block ends
```

## Packaging

`weaveffi package --target python` writes one wheel per desktop platform
built, `python/{name}-{version}-py3-none-{platform}.whl` (the distribution
name escaped for a wheel file name, so `Key-Store` becomes `key_store`),
with the producer library inside the import package. The platform tag
matches the build: the `[build]` macOS deployment target
(`macosx_11_0_arm64`), the newest glibc the library links against on Linux
(`manylinux_2_17_x86_64` or later), or `win_amd64`. The wheels install
with `pip` and upload with `twine`; Android, iOS, and `wasm32` builds have
no wheel platform and are skipped. See [Packaging](../guides/packaging.md).

## Known limitations

- `ctypes` dispatch costs more per call than a compiled extension; hot
  loops that cross the boundary per element will feel it. The bindings
  keep the per-call work small (prototypes and vtable addresses are
  resolved at import, and the error check is inline), but each call still
  allocates its error slot, and each integer argument costs a range check.
- A typed-array parameter is copied into an `array.array` unless it already
  is one of the matching type code; a NumPy array is copied element by
  element.
- All modules share one namespace (see "Type mapping"), which also holds
  the typing names the module imports (`Any`, `Callable`, `ClassVar`,
  `Iterable`, `Iterator`, `Mapping`, `Sequence`): a declared type with one
  of those names still works at run time, but type checkers reject the
  module.
- A long-running callback stalls the producer thread that called it, and a
  callback that waits for the thread blocked in the calling Python code
  deadlocks.
- `LibraryLoadError` is raised during the import, so it can only be caught
  as `ImportError` (the package isn't importable to name the class).
