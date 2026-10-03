# Python

The Python target produces a pip-installable package of pure-Python
`ctypes` bindings over the C ABI (revision 3), plus `.pyi` type stubs. There's
no compilation step, no native extension, and no third-party runtime
dependency; the package works on any Python 3.9+ interpreter that can load
the producer library.

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
    ├── __init__.py         # re-exports the public API
    ├── kvstore.py          # runtime, prototypes, and wrappers
    ├── kvstore.pyi         # type stub
    └── py.typed            # PEP 561 marker
```

The distribution name is the package identity's `name` and the import
package is its C prefix, so a crate named `async-demo` installs as
`async-demo` and imports as `async_demo`. Both are configurable:

```toml
[generators.python]
package_name = "kvstore-python"   # distribution name (default: package name)
import_name = "kvstore_py"        # import package (default: C prefix)
strip_module_prefix = true        # `get_stats`, not `kv_stats_get_stats`
```

## Install and load

```bash
weaveffi generate src/lib.rs -o generated --target python
pip install generated/python
```

At import, the module loads the producer library from the first of:

1. the path in `{PREFIX}_LIBRARY` (for example `KVSTORE_LIBRARY`),
2. a library bundled inside the package directory (what `weaveffi package`
   produces), and
3. the system loader's search path, by platform file name
   (`libkvstore.dylib`, `libkvstore.so`, or `kvstore.dll`).

It then checks `kvstore_abi_version()` against revision 3 and every
top-level module's contract checksum (`kvstore_kv_checksum()`) against the
value the bindings were generated with. A mismatch raises `ImportError`
naming the module, so a stale binding never misreads a buffer at runtime.
Every C prototype is bound once at this point, in a module-level table:

```python
_c_kitchen_Gadget_describe = _bind("kitchen_sink_kitchen_Gadget_describe", ctypes.c_void_p, ctypes.c_void_p, ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(_ErrorStruct))
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
| `iter<T>` | `Iterator[T]` | iterator handle |

Strings and bytes pass without per-byte loops: a `str` is encoded once and
its bytes are lent to the call, so interior NUL characters survive. Returned
strings, bytes, and buffers are copied with `ctypes.string_at` and released
with `{prefix}_free_bytes`. Lists of a fixed-width number type are packed
and unpacked with one `struct` call.

All modules share one Python namespace. Nested modules' declarations are
exported next to their parents', so names must be unique across modules
(set `strip_module_prefix = false` to prefix function names).

## Objects and lifetime

An interface wrapper holds one strong reference and releases it exactly
once: from `close()`, the `with` statement, or the `__del__` backstop.
`close()` is idempotent, and a closed wrapper raises
`Error(-1, "... used after close()")`. Each call lends the pointer for its
duration, so `close()` from another thread while a call is in flight defers
the release until the call returns:

```python
    def describe(self) -> str:
        """Render the gadget as a human-readable string"""
        _err = _ErrorStruct()
        _out_len = ctypes.c_size_t()
        _self_p = self._acquire()
        try:
            _ret = _c_kitchen_Gadget_describe(_self_p, ctypes.byref(_out_len), ctypes.byref(_err))
        finally:
            self._release()
        _check_error(_err)
        return _take_str(_ret, _out_len.value)
```

A constructor named `new` becomes `__init__`; other constructors are
`@classmethod` factories. Objects returned by the producer, decoded from a
buffer, or passed to a callback are adopted into new wrappers. Writing an
object into a value buffer (a record field or a list element) mints a new
reference only after the whole argument list has encoded, so a failed
encoding leaks nothing.

## Errors

Every exception derives from the package's root `Error` (named
`{PascalName}Error` if the API declares its own `Error` type), which
carries `code` and `message`. A module's error domain becomes a subclass
(`KvError`) with one subclass per code (`KeyNotFound`), each also reachable
as `KvError.KeyNotFound` and carrying its stable `CODE`. Payload fields
decode onto the exception as attributes.

Negative codes are runtime traps with constants on `Error`:
`GENERIC_ERROR_CODE` (-1), `PANIC_ERROR_CODE` (-2), `MARSHAL_ERROR_CODE`
(-3), and `FOREIGN_ERROR_CODE` (-4, a callback implementation raised). A
cancelled async call (-5) raises `asyncio.CancelledError` instead.

## Async and cancellation

An async function is an `async def` that must run on an `asyncio` event
loop. The launcher receives a static completion trampoline and an integer
key; the trampoline takes ownership of the result on the producer's
thread and settles the future through `call_soon_threadsafe`.

For a `cancellable` function, each call owns a native cancel token.
Cancelling the awaiting task (directly, through `asyncio.wait_for`, or by
cancelling a `gather`) cancels the token, waits for the producer's
cancelled completion, and raises `asyncio.CancelledError`:

```python
async def do_cancellable(input: str) -> str:
    """Cancellable async operation"""
    _input_b = input.encode("utf-8")
    _call, _future = _async_begin()
    _token = _cancel_token_create()
    try:
        _c_kitchen_do_cancellable(_input_b, len(_input_b), _token, _kitchen_do_cancellable_completion, _call)
    except BaseException:
        _async_abandon(_call)
        _cancel_token_destroy(_token)
        raise
    return await _async_wait_cancellable(_future, _token)
```

Cancelling the task awaiting a function that isn't `cancellable` stops the
wait only; the native call runs to completion and its result is dropped.

## Callbacks

A callback interface is an `abc.ABC` with one abstract method per IDL
method. Passing an instance registers it in a handle table under an integer
key, which the producer receives as `ctx` along with the address of the
interface's one static vtable. The producer's `free(ctx)` removes the
entry, so an implementation lives exactly as long as the producer holds it.

Trampolines run on whichever thread the producer calls from; `ctypes`
acquires the GIL first. An exception raised by an implementation is
reported through `{prefix}_error_set(out_err, -4, message)` and never
unwinds through C; the producer's call then fails with
`FOREIGN_ERROR_CODE`. A producer method declared to return
`Result<T, ForeignError>` receives the failure as an `Err` value.

## Iterators

An `iter<T>` return is a lazy iterator: each `next()` makes one native
`next` call through the pre-bound function, and the handle is released on
exhaustion, `close()`, or garbage collection.

```python
for key in store.list_keys(None):
    print(key)
```

## Packaging

`weaveffi package --target python` writes one wheel source tree per
desktop platform under `python/<platform-id>/`, with the producer library
bundled inside the import package and a `setup.py` that forces a
platform-tagged wheel. Build each tree with `python -m build --wheel` and
tag it with the platform tag listed in its `README.md`. Android and
`wasm32` binaries have no wheel platform and are skipped.

## Known limitations

- `ctypes` dispatch costs more per call than a compiled extension; hot
  loops that cross the boundary per element will feel it.
- All modules share one namespace (see "Type mapping").
- A long-running callback stalls the producer thread that called it, and a
  callback that waits for the thread blocked in the calling Python code
  deadlocks.
- The stubs type `iter<T>` returns as `Iterator[T]`, so a type checker
  doesn't know about their `close()` method.
