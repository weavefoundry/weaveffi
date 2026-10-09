# Rust API Map

A map of the public API a Rust producer sees: the `weaveffi` facade and its
`weaveffi::abi` runtime. `cargo doc` has the full signatures and safety
contracts; this page says which items you'll reach for.

```toml
[dependencies]
weaveffi = "0.24"
```

## The `weaveffi` facade

| Item | Purpose |
|------|---------|
| `#[weaveffi::module]` | Marks an exported module, validates its tree, and emits its thunks, contract table, and the library metadata the CLI reads the API from. |
| `#[weaveffi::export]` | Exports a function (`async fn` is async; a `Result` return throws). |
| `#[weaveffi::record]` | A by-value struct. |
| `#[weaveffi::enumeration]` | A `#[repr(i32)]` C-style enum or a rich enum. |
| `#[weaveffi::interface]` | A reference-counted object type, with its inherent `impl`. |
| `#[weaveffi::callback_interface]` | A trait the consumer implements, each method returning `Result<T, ForeignError>`; accepted as `Arc<dyn Trait>` or `Option<Arc<dyn Trait>>`. |
| `#[weaveffi::throws]` | A callback method whose consumer may fail with the error domain in scope. |
| `#[weaveffi::error]` | The module's error domain; requires `Display`. |
| `#[weaveffi::cancellable]` | An async function taking a `CancelToken`. |
| `weaveffi::export_runtime!()` | Exports the runtime symbols under the crate's prefix; call once. |
| `Iter<T>` | The return type for `iter<T>`; build with `Iter::new(iterator)`. |
| `CancelToken` | A cancellable function's last parameter; `is_cancelled()` for cooperative cleanup. |
| `ForeignError` | A consumer callback's failure (`code`, `message`, `payload`), the `Err` of every callback method; `domain::<E>()` decodes a declared domain error. Return it from an exported function to propagate a failure with `?`. |
| `ErrorDomain` | Implemented by every `#[weaveffi::error]` enum, so `ForeignError::domain` can decode it. |
| `ErrorReport` | Maps an error type to `(code, message, payload)`; implemented for `String`, `&str`, boxed errors, `ForeignError`, and every `#[weaveffi::error]` enum. |
| `set_spawner`, `Spawner`, `BoxFuture` | Install the executor async functions run on, overriding the default worker pool (or Tokio). |
| `abi` | The C ABI runtime (below). |

The `leak-check` feature enables the runtime's leak counters, and the
`tokio` feature runs async functions on Tokio by default. The
[producer macro guide](../guides/producer-macro.md) covers every attribute.

## The `weaveffi::abi` runtime

Items marked **(macro)** are called by generated thunks; read them to
understand the expansion, but you shouldn't need to call them. Everything
but the `leak` constants is re-exported at the root of `weaveffi::abi`.

| Module | Items |
|--------|-------|
| root | `ABI_VERSION` (`4`). |
| `error` | `FfiError`, the `#[repr(C)]` error struct (the C `{prefix}_error`); the codes `GENERIC_ERROR_CODE` through `CANCELLED_ERROR_CODE` (`-1` to `-5`); `ErrorReport`; `ErrorDomain`; `panic_message`. **(macro)** `error_set`, `error_set_c`, `error_set_payload_c`, `error_store`, `error_clear`, `error_free`, `boxed_error`. |
| `convert` | `(ptr, len)` conversions: `lift_str`, `lift_string`, `lift_byte_slice`, `lift_bytes` read borrowed parameters; `lower_string`, `lower_bytes`, `bytes_into_raw` hand out allocations; `alloc` allocates a run for the producer to adopt; `adopt_bytes` adopts one; `free_bytes` releases either kind. Useful in tests that call thunks directly. |
| `marshal` | `CEnum`, the trait every C-style enum implements (`from_i32`, `to_i32`). **(macro)** the per-family `lift_*_param` and `lower_*_ret` functions, `lift_self`, `lift_self_arc`, `lift_enum`, `read_enum`, `write_enum`, the `*_slots` and `*_run` helpers, `call_sync`, and `Sentinel`. |
| `object` | **(macro)** `lower_object`, `lower_object_opt`, `object_ref`, `object_arc`, `object_clone`, `object_destroy`, and the buffer token pair `object_to_token` / `object_from_token`. |
| `callback` | `ForeignError`; `VtableHeader`, the fixed start of every vtable. **(macro)** `ForeignCallback`, `Vtable`, `VtableTooSmall`, `CallbackInterface`, `lift_callback`, `lift_callback_opt`, `callback_status`, `callback_status_in`, and the `callback_ret_*` adopters. |
| `cancel` | `CancelToken`. **(macro)** `Cancellable`, `FfiCancelToken`, `AtomicWaker`, and the `cancel_token_*` bodies. |
| `iter` | `Iter`. **(macro)** `IterHandle`, `iter_into_raw`, `iter_next`, `iter_destroy`. |
| `buffer` | The value-buffer codec: `BufferValue`, `BufferWriter`, `BufferReader`, `BufferDecodeError`, `encode_value`, `decode_value`, the `ByValue` marker that records and rich enums implement, and `FixedWidth`, the token behind single-copy lists of numbers. `decode_value` and `BufferValue::read_value` are `unsafe`, because decoding an object token adopts a reference; a `BufferReader::token_free` reader refuses tokens and is safe for any input. |
| `contract` | `ContractEntry`. **(macro)** `contract_table`, `contract_len`, `contract_compact`. |
| `spawn` | `set_spawner`, `Spawner`, `SpawnerAlreadySet`, `BoxFuture`, `SpawnError`, `default_pool_size`, and `block_on` (a blocking executor for tests). **(macro)** `launch_async`, `run_async`, `spawn`, `CatchUnwind`. |
| `leak` | `debug_live`, the `ENABLED` flag, and the kind constants `COUNTING` (`-1`), `OBJECTS`, `CALLBACKS`, `ITERATORS`, `TOKENS`, and `ALLOCATIONS` (`0` to `4`); counters are live only with `leak-check`. |

The [C ABI contract](../reference/abi.md) is the specification these items
implement, and [Async and Cancellation](../guides/async.md) covers the
spawner in practice.
