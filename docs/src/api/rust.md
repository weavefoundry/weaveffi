# Rust API Map

A map of the public API a Rust producer sees: the `weaveffi` facade and the
`weaveffi-abi` runtime it re-exports as `weaveffi::abi`. `cargo doc` has the
full signatures and safety contracts; this page says which items you'll
reach for.

```toml
[dependencies]
weaveffi = "0.23"
```

## The `weaveffi` facade

| Item | Purpose |
|------|---------|
| `#[weaveffi::module]` | Marks an exported module and emits its thunks and checksum. |
| `#[weaveffi::export]` | Exports a function (`async fn` is async; a `Result` return throws). |
| `#[weaveffi::record]` | A by-value struct. |
| `#[weaveffi::enumeration]` | A `#[repr(i32)]` C-style enum or a rich enum. |
| `#[weaveffi::interface]` | A reference-counted object type, with its inherent `impl`. |
| `#[weaveffi::callback_interface]` | A trait the consumer implements; accepted as `Arc<dyn Trait>`. |
| `#[weaveffi::error]` | The module's error domain; requires `Display`. |
| `#[weaveffi::cancellable]` | An async function taking a `CancelToken`. |
| `weaveffi::export_runtime!()` | Exports the runtime symbols under the crate's prefix; call once. |
| `Iter<T>` | The return type for `iter<T>`; build with `Iter::new(iterator)`. |
| `CancelToken` | A cancellable function's last parameter; `is_cancelled()` for cooperative cleanup. |
| `ForeignError` | A consumer callback's failure (`code`, `message`), returned by `Result`-returning callback methods. |
| `ErrorReport` | Maps an error type to `(code, message, payload)`; implemented for `String`, `&str`, boxed errors, and every `#[weaveffi::error]` enum. |
| `set_spawner`, `Spawner`, `BoxFuture` | Install the executor async functions run on. |
| `abi` | The `weaveffi-abi` crate. |

The `leak-check` feature enables the runtime's leak counters. The
[producer macro guide](../guides/producer-macro.md) covers every attribute.

## The `weaveffi-abi` runtime

Items marked **(macro)** are called by generated thunks; read them to
understand the expansion, but you shouldn't need to call them. Everything is
re-exported at the crate root.

| Module | Items |
|--------|-------|
| root | `ABI_VERSION` (`3`). |
| `error` | `FfiError`, the `#[repr(C)]` error struct (the C `{prefix}_error`); the codes `GENERIC_ERROR_CODE` through `CANCELLED_ERROR_CODE` (`-1` to `-5`); `ErrorReport`; `panic_message`. **(macro)** `error_set`, `error_store`, `error_clear`, `error_free`, `boxed_error`. |
| `convert` | `(ptr, len)` conversions: `lift_str`, `lift_string`, `lift_byte_slice`, `lift_bytes` read borrowed parameters; `lower_string`, `lower_bytes`, `bytes_into_raw` hand out allocations; `free_bytes` releases them. Useful in tests that call thunks directly. |
| `object` | **(macro)** `lower_object`, `object_ref`, `object_arc`, `object_clone`, `object_destroy`, and the buffer token pair `object_to_token` / `object_from_token`. |
| `callback` | `ForeignError`; `raise_foreign_error`, which propagates a callback failure from producer code; `foreign_status`; `ThunkScope`. **(macro)** `ForeignCallback`, `Vtable`, `CallbackInterface`, `lift_callback`, `defer_foreign_error`, `take_foreign_error`. |
| `cancel` | `CancelToken`. **(macro)** `Cancellable`, `FfiCancelToken`, and the `cancel_token_*` bodies. |
| `iter` | `Iter`. **(macro)** `IterHandle`, `iter_into_raw`, `iter_next`, `iter_destroy`. |
| `buffer` | The value-buffer codec: `BufferValue`, `BufferWriter`, `BufferReader`, `BufferDecodeError`, `encode_value`, `decode_value`, and the `ByValue` marker that records and rich enums implement. |
| `spawn` | `set_spawner`, `Spawner`, `SpawnerAlreadySet`, `BoxFuture`, and `block_on` (the default executor). **(macro)** `run_async`, `spawn`, `CatchUnwind`. |
| `leak` | `debug_live` and the `ENABLED` flag; counters are live only with `leak-check`. |

The [C ABI contract](../reference/abi.md) is the specification these items
implement, and [Async and Cancellation](../guides/async.md) covers the
spawner in practice.
