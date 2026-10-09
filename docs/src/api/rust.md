# Rust API Map

A map of the public API a Rust producer sees: the `weaveffi` facade and its
`weaveffi::abi` runtime. `cargo doc` has the full signatures and safety
contracts; this page says which items you'll reach for.

```toml
[dependencies]
weaveffi = "0.25"
```

## The `weaveffi` facade

| Item | Purpose |
|------|---------|
| `#[weaveffi::module]` | Marks an exported module, validates its tree, and emits its thunks, contract table, and the library metadata the CLI reads the API from. |
| `#[weaveffi::export]` | Exports a function (`async fn` is async; a `Result` return throws its error type when that's a declared domain, else `any`). |
| `#[weaveffi::record]` | A by-value struct. |
| `#[weaveffi::enumeration]` | A `#[repr(i32)]` C-style enum or a rich enum. |
| `#[weaveffi::interface]` | A reference-counted object type, with its inherent `impl`. |
| `#[weaveffi::skip]` | Leaves a `pub fn` of an interface's `impl` unexported. |
| `#[weaveffi::callback_interface]` | A trait the consumer implements, each method returning `Result<T, E>` with `E: From<ForeignError>` (a domain `E` receives the consumer's typed errors); accepted as `Arc<dyn Trait>` or `Option<Arc<dyn Trait>>`. |
| `#[weaveffi::error]` | An error domain (a module may declare several); generates `Display` and `std::error::Error` unless written `#[weaveffi::error(no_display)]`. |
| `#[weaveffi::custom]` | A type alias that crosses as another type, with `lift` and `lower` functions. |
| `#[weaveffi::cancellable]` | An async function taking a `CancelToken`. |
| `weaveffi::export_runtime!()` | Exports the runtime symbols under the crate's prefix; call once. |
| `Iter<T>` | The return type for `iter<T>`; build with `Iter::new(iterator)`. |
| `CancelToken` | A cancellable function's last parameter; `is_cancelled()` for cooperative cleanup. |
| `ForeignError` | A consumer callback's failure (`code`, `message`, `payload`), which every callback method's error type converts from; `domain::<E>()` decodes a declared domain error by hand. As an exported function's error type it's `throws: any`. |
| `ErrorDomain` | Implemented by every `#[weaveffi::error]` enum (with `Display` as a supertrait): each variant's code and payload. |
| `set_spawner`, `Spawner`, `BoxFuture` | Install the executor async functions run on, overriding the default (Tokio, or a thread per call without the `tokio` feature). |
| `abi` | The C ABI runtime (below). |

The `leak-check` feature enables the runtime's leak counters, and the
default `tokio` feature runs async functions on Tokio
(`default-features = false` runs each on a thread of its own). The
[producer macro guide](../guides/producer-macro.md) covers every attribute.

## The `weaveffi::abi` runtime

Items marked **(macro)** are called by generated thunks; read them to
understand the expansion, but you shouldn't need to call them. Everything
but the `leak` constants is re-exported at the root of `weaveffi::abi`.

| Module | Items |
|--------|-------|
| root | `ABI_VERSION` (`5`). |
| `error` | `FfiError`, the `#[repr(C)]` error struct (the C `{prefix}_error`, with `message_ptr` and `message_len`); the codes `GENERIC_ERROR_CODE` through `CANCELLED_ERROR_CODE` (`-1` to `-5`); `ErrorDomain`; `panic_message`. **(macro)** `error_set`, `error_set_c`, `error_set_payload_c`, `error_store`, `error_clear`, `error_free`, `boxed_error`. |
| `convert` | `(ptr, len)` conversions: `lift_str`, `lift_string`, `lift_byte_slice`, `lift_bytes` read borrowed parameters; `lower_string`, `lower_bytes`, `bytes_into_raw`, `slice_into_raw` hand out 8-aligned runs (`RUN_ALIGN`); `alloc` allocates a run for the producer to adopt; `adopt_bytes` adopts one; `free_bytes` releases either kind. Useful in tests that call thunks directly. |
| `scalar` | `Scalar`, the trait for values that cross as one C scalar (every fixed-width integer, float, and `bool`, `usize` and `isize` as `u64` and `i64`, and every C-style enum as an `i32`); `Text` (`String`, `str`, `char`); `Custom`, which a `#[weaveffi::custom]` declaration implements. |
| `marshal` | **(macro)** the per-family `lift_*_param` and `lower_*_ret` functions (including the OptDirect `lift_opt_param`/`lower_opt_ret` and Slice `lift_slice_param`/`lower_slice_ret` pairs), `lift_custom_param`, `lift_self`, `lift_self_arc`, `read_enum`, `write_enum`, the `*_slots` and `*_run` helpers, `call_sync`, and `Sentinel`. |
| `object` | **(macro)** `lower_object`, `lower_object_opt`, `object_ref`, `object_arc`, `object_clone`, `object_destroy`, and the buffer token pair `object_to_token` / `object_from_token`. |
| `callback` | `ForeignError`; `VtableHeader`, the fixed start of every vtable; `VTABLE_THREAD_AFFINE` and `OFF_THREAD_MESSAGE`. **(macro)** `ForeignCallback` (with its `check_thread`), `Vtable`, `VtableTooSmall`, `CallbackInterface`, `lift_callback`, `lift_callback_opt`, `callback_status`, `callback_status_in`, `convert_foreign`, and the `callback_ret_*` adopters. |
| `cancel` | `CancelToken`. **(macro)** `Cancellable`, `FfiCancelToken`, `AtomicWaker`, and the `cancel_token_*` bodies. |
| `iter` | `Iter`. **(macro)** `IterHandle`, `iter_into_raw`, `iter_next`, `iter_destroy`. |
| `buffer` | The value-buffer codec: `BufferValue`, `BufferWriter`, `BufferReader`, `BufferDecodeError`, `encode_value`, `decode_value`, the `ByValue` marker that records and rich enums implement, and `FixedWidth`, the token behind single-copy lists of numbers. `decode_value` and `BufferValue::read_value` are `unsafe`, because decoding an object token adopts a reference; a `BufferReader::token_free` reader refuses tokens and is safe for any input. |
| `contract` | `ContractEntry`. **(macro)** `contract_table`, `contract_len`, `contract_compact`. |
| `spawn` | `set_spawner`, `Spawner`, `SpawnerAlreadySet`, `BoxFuture`, `SpawnError`, and `block_on` (a blocking executor, also the one the thread-per-call default uses). **(macro)** `launch_async`, `run_async`, `spawn`, `CatchUnwind`. |
| `leak` | `debug_live`, the `ENABLED` flag, and the kind constants `COUNTING` (`-1`), `OBJECTS`, `CALLBACKS`, `ITERATORS`, `TOKENS`, and `ALLOCATIONS` (`0` to `4`); counters are live only with `leak-check`. |

The [C ABI contract](../reference/abi.md) is the specification these items
implement, and [Async and Cancellation](../guides/async.md) covers the
spawner in practice.
