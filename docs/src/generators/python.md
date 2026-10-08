# Python

The Python target produces a pip-installable package of pure-Python
`ctypes` bindings over the C ABI (revision 4). There's no compilation step,
no native extension, and no third-party runtime dependency; the package
works on any Python 3.9+ interpreter that can load the producer library.
The implementation module is fully annotated and the package ships
`py.typed`, so type checkers read the bindings themselves (there's no
separate stub to drift out of date) and the generated code passes
`mypy --strict`.

Records and rich enums are dataclasses that cross the boundary as value
buffers. Interfaces are reference-counted wrapper classes. Callback
interfaces are abstract base classes the consumer subclasses. Async
functions are coroutines whose cancellation cancels the native call, and
`iter<T>` returns are lazy Python iterators.

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
requires_python = ">=3.10"   # default ">=3.9"
```

The module lists its public names in `__all__` (the error classes, every
type, and the free functions), so `from kvstore import *` and the package's
`__init__.py` export the API and none of the runtime's helpers.

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
4, and every top-level module's contract table must contain each
declaration the bindings were generated with, with an equal hash. The
expected entries are embedded in the module, one `(id, hash, path)` per
declaration:

```python
_CONTRACTS: Dict[str, List[Tuple[int, int, str]]] = {
    "kvstore_kv_contract": [
        (0x0969575bfbb012d7, 0xebd38766e3532c4f, "kv.Store.fork"),
        ...
    ],
    "kvstore_report_contract": [...],
}
```

A failed check raises `ImportError` naming the declaration
(`kv.Store.put is missing from the library` or `kv.Store.put changed since
these bindings were generated`), so a stale binding never misreads a
buffer at runtime. Declarations the library has and the bindings don't are
fine, so a library that grew new functions still loads. Every C prototype
is then bound once, in a module-level table:

```python
_c_kv_Store_count = _bind("kvstore_kv_Store_count", ctypes.c_uint32, ctypes.c_void_p, ctypes.POINTER(_ErrorStruct))
```

On macOS, the system `python3` strips `DYLD_LIBRARY_PATH`, so prefer
`{PREFIX}_LIBRARY` there.

## Type mapping

| IDL type | Python type | Crosses the ABI as |
|---|---|---|
| `i8` to `u64` | `int` | `c_int8` to `c_uint64` |
| `f32`, `f64` | `float` | `c_float`, `c_double` |
| `bool` | `bool` | `c_bool` |
| `string` | `str` | UTF-8 bytes plus length (`c_char_p`, `c_size_t`) |
| `bytes` | `bytes` | pointer plus length |
| C-style enum | `IntEnum` subclass | `c_int32` |
| record | `@dataclass` | value buffer |
| rich enum | base class plus one `@dataclass` per variant | value buffer |
| `T?` | `Optional[T]` | value buffer |
| `[T]`, `{K:V}` | `List[T]`, `Dict[K, V]` | value buffer |
| interface | wrapper class | `c_void_p` |
| interface `?` | `Optional[...]` | `c_void_p`, NULL for `None` |
| callback interface | subclass of the generated ABC | context key plus vtable |
| callback interface `?` | `Optional[...]` | context key plus vtable, NULL for `None` |
| `iter<T>` | `Iterator[T]` | iterator handle |

Strings and bytes pass without per-byte loops: a `str` is encoded once and
its bytes are lent to the call, so interior NUL characters survive.
Returned strings, bytes, and buffers are copied with `ctypes.string_at` and
released with `{prefix}_free_bytes`.

Every record and rich enum has one writer and one reader function
(`_write_Entry`, `_read_Entry`), and so does every distinct composite type
the API uses (`_write_list_string`, `_read_map_string_Store`,
`_read_opt_Entry`); a call site makes one call instead of inlining a loop.
Lists of a fixed-width number type are packed and unpacked with one
`struct` call. A decoded buffer that's truncated, has trailing bytes, an
invalid flag, invalid UTF-8, an undeclared enum value, or a repeated map
key raises `InternalError` with code `-3`.

All modules share one Python namespace. Nested modules' declarations are
exported next to their parents' under their bare names (`summarize`, not
`kv_stats_summarize`), which global names keep unique. The module starts
with `from __future__ import annotations`, so annotations name types
declared later in the file without quotes.

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

A call that declares errors (`throws`) raises the error domain in scope for
its module. Every such exception derives from the package's root `Error`
(named `{PascalName}Error` if the API declares its own `Error` type), which
carries `code` and `message`. A module's error domain becomes a subclass
(`KvError`) with one subclass per code (`KeyNotFound`), each also reachable
as `KvError.KeyNotFound` and carrying its stable `CODE`. A code's payload
fields are constructor arguments and attributes:

```python
try:
    store.get("nope")
except kvstore.KvError.KeyNotFound as e:
    print(e.code, e.key, e.message)  # 1001 nope key not found: nope
```

A negative code on a throwing call (a panic, a marshalling failure, or a
failed callback) raises the root `Error` with that code; the constants are
`GENERIC_ERROR_CODE` (-1), `PANIC_ERROR_CODE` (-2), `MARSHAL_ERROR_CODE`
(-3), and `FOREIGN_ERROR_CODE` (-4).

A call that declares no errors can only fail through a bug, so it follows
the [trap policy](../guides/errors-and-memory.md#the-trap-policy): it
raises `InternalError`, a `RuntimeError` subclass outside the `Error`
hierarchy whose message names the code and the producer's message
(`InternalError: (-3) ...`). A cancelled async call (-5) raises
`asyncio.CancelledError` instead.

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
passes a null vtable. The vtable starts with the revision 4 header (its
`size`, `flags`, and the shared `free` entry), and the producer's
`free(ctx)` removes the table entry, so an implementation lives exactly as
long as the producer holds it.

Trampolines run on whichever thread the producer calls from; `ctypes`
acquires the GIL first. A trampoline adopts the object arguments first,
converts the rest, calls the method, and hands the return back:

- a number, `bool`, or enum as the C return value;
- an object as a fresh strong reference the producer adopts (`None` is
  allowed only for `I?`);
- a `str`, `bytes`, record, or other buffered value through the
  `out_ptr`/`out_len` slots, in a run allocated with `{prefix}_alloc` that
  the producer adopts and frees.

```python
def _Loader_load(ctx: Optional[int], key_ptr: Optional[int], key_len: int, out_ptr: Any, out_len: Any, out_err: Any) -> None:
    try:
        _ret = _callback_get(ctx).load(_peek_bytes(key_ptr, key_len).decode("utf-8"))
        _callback_return_bytes(out_ptr, out_len, bytes(_ret))
    except BaseException as exc:
        _callback_fail(out_err, exc, KvError)
```

Nothing unwinds through C. A method that declares errors may raise its
module's domain codes, which reach the producer as that code with the
fields as the error payload (`{prefix}_error_set` and
`{prefix}_error_set_payload`):

```python
class EmptyLoader(kvstore.Loader):
    def name(self) -> str:
        return "empty"

    def fallback(self, key: str) -> Optional[kvstore.Store]:
        return None

    def load(self, key: str) -> bytes:
        raise kvstore.KeyNotFound(key=key, message="not in the loader")
```

Any other exception, a return of the wrong type, or a domain error raised
by a method that doesn't declare errors reaches the producer as
`FOREIGN_ERROR_CODE` (-4) with the exception's message. A Rust producer
receives either as the `Err` of the method's `Result<T, ForeignError>`.

## Iterators

An `iter<T>` return is a lazy iterator: each `next()` makes one native
`next` call through the pre-bound function, and the handle is released on
exhaustion, `close()`, or garbage collection.

```python
for key in store.keys(None):
    print(key)
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
  allocates its error slot.
- `ctypes` truncates an out-of-range integer argument to the parameter's C
  width instead of raising; inside a value buffer, `struct` raises.
- All modules share one namespace (see "Type mapping").
- A long-running callback stalls the producer thread that called it, and a
  callback that waits for the thread blocked in the calling Python code
  deadlocks.
- `iter<T>` returns are annotated as `Iterator[T]`, so a type checker
  doesn't know about their `close()` method.
