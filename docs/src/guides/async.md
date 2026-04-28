# Async Functions

## Overview

WeaveFFI exposes asynchronous Rust operations through a single
callback-based C ABI and language-native async wrappers in every
target. Mark a function with `async: true` (and optionally
`cancellable: true`) in the IDL and the generators emit the right
shape per target — `async throws` in Swift, `suspend fun` in Kotlin,
`Promise<T>` in JS, `async def` in Python, `Task<T>` in .NET, and so
on.

## When to use

Use async functions for:

- I/O-bound work (network, disk, database).
- Long-running operations that should not block the consumer's
  event loop (UI threads, JS event loop, asyncio loop).
- Operations the consumer should be able to cancel — combine with
  `cancellable: true`.

Avoid async for:

- Short CPU-bound work (math, parsing, validation). The callback
  overhead is more expensive than the call itself.
- Functions whose Rust implementation is purely synchronous and
  finishes in microseconds.

## Step-by-step

### 1. Declare the function in the IDL

```yaml
version: "0.3.0"
modules:
  - name: net
    functions:
      - name: fetch_data
        params:
          - { name: url, type: string }
        return: string
        async: true
        doc: "Fetches data from the given URL"

      - name: upload_file
        params:
          - { name: path, type: string }
          - { name: data, type: bytes }
        return: bool
        async: true
        cancellable: true
        doc: "Uploads a file, can be cancelled"
```

| Field         | Type   | Default | Description                                    |
|---------------|--------|---------|------------------------------------------------|
| `async`       | bool   | `false` | Mark the function as asynchronous              |
| `cancellable` | bool   | `false` | Allow the async operation to be cancelled      |

### 2. Implement it in Rust

The generated C ABI symbol takes a callback pointer and an opaque
`void* context`. The Rust worker invokes the callback exactly once
when it is done. The pattern from `samples/async-demo/src/lib.rs`:

```rust
#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::os::raw::c_char;
use weaveffi_abi::{self as abi, weaveffi_error};

pub type weaveffi_net_fetch_data_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: *const c_char);

#[no_mangle]
pub extern "C" fn weaveffi_net_fetch_data(
    url: *const c_char,
    callback: weaveffi_net_fetch_data_callback,
    context: *mut c_void,
) {
    let url_str = abi::c_ptr_to_string(url).unwrap_or_default();
    let ctx = context as usize;
    std::thread::spawn(move || {
        let payload = std::ffi::CString::new(format!("payload from {url_str}"))
            .unwrap()
            .into_raw();
        callback(ctx as *mut c_void, std::ptr::null_mut(), payload);
    });
}
```

### 3. Call it from each target

Swift:

```swift
let payload = try await Net.fetchData("https://example.com/data")
```

Kotlin/Android:

```kotlin
val payload = Net.fetchData("https://example.com/data")
```

Node.js:

```typescript
const payload = await fetchData("https://example.com/data");
```

Python:

```python
payload = await fetch_data("https://example.com/data")
```

.NET:

```csharp
var payload = await Net.FetchDataAsync("https://example.com/data");
```

Dart:

```dart
final payload = await fetchData('https://example.com/data');
```

### 4. Cancel a running operation

For `cancellable: true` functions the wrapper plumbs the target's
native cancellation primitive into the C ABI cancel token. Cancellation
is observed by the Rust worker, but the callback is **always**
invoked exactly once — either with the result or with a `Cancelled`
error. The pin/unpin pair (see Reference) runs on the cancellation
path identically to the success path.

```swift
let task = Task { try await Net.uploadFile(path: "x", data: data) }
task.cancel()
```

```python
task = asyncio.create_task(net.upload_file("x", data))
task.cancel()
```

## Reference

### C ABI shape

```c
typedef void (*weaveffi_callback_string)(
    const char* result,
    const weaveffi_error* err,
    void* user_data
);

void weaveffi_net_fetch_data(
    const uint8_t* url, size_t url_len,
    weaveffi_callback_string on_complete,
    void* user_data
);
```

For `cancellable: true`:

```c
uint64_t weaveffi_net_upload_file(
    const uint8_t* path, size_t path_len,
    const uint8_t* data, size_t data_len,
    weaveffi_callback_bool on_complete,
    void* user_data
);

void weaveffi_cancel(uint64_t cancel_handle);
```

### Per-target async surface

| Target  | Async surface                              | Cancellation hook                         |
|---------|--------------------------------------------|-------------------------------------------|
| C       | Raw callback                               | `weaveffi_cancel(handle)`                 |
| C++     | `std::future<T>`                            | `weaveffi_cancel_token*` argument         |
| Swift   | `async throws`                              | `withTaskCancellationHandler`             |
| Kotlin  | `suspend fun`                               | `invokeOnCancellation`                    |
| Node.js | `Promise<T>`                                | `AbortSignal` (when `cancellable: true`)  |
| Python  | `async def`                                 | `asyncio.CancelledError`                  |
| .NET    | `Task<T>`                                   | `CancellationToken`                       |
| Dart    | `Future<T>` (runs on isolate)               | `Future.timeout` / cancellation token     |
| WASM    | `Promise<T>` (synchronous shim today)       | n/a                                       |
| Go      | _Not async-capable_ — generator skips today | n/a                                       |
| Ruby    | _Not async-capable_ — generator skips today | n/a                                       |

### Pin / unpin matrix

Every binding pins the user-supplied `void* context` and the callback
closure for the lifetime of the operation, then releases them exactly
once on the callback path. The matrix below is the contract every
generator implements; each row is verified by a
`{generator}_async_pins_callback_for_lifetime` unit test plus the
1000-call stress test under `examples/{target}/async_stress.{ext}`.

| Target  | Pin (allocate / retain)                                | Unpin (free / release) on callback             | Notes |
|---------|---------------------------------------------------------|------------------------------------------------|-------|
| Swift   | `Unmanaged.passRetained(ContinuationRef(...))`          | `Unmanaged.fromOpaque(ctx).takeRetainedValue()` | The retained `+1` is dropped exactly once when the continuation resumes. |
| .NET    | `GCHandle.Alloc(callback, GCHandleType.Normal)`         | `GCHandle.FromIntPtr(context).Free()`           | The catch path also frees the handle on synchronous failure. |
| Kotlin  | JNI `(*env)->NewGlobalRef(env, callback)`               | `(*env)->DeleteGlobalRef(env, ctx->callback)`   | The JNI shim `malloc`s and `free`s the per-call context exactly once. |
| Node.js | `napi_create_promise(env, &deferred, &promise)`         | `napi_resolve_deferred` or `napi_reject_deferred` | The N-API runtime owns the deferred; the per-call context is `malloc`-ed and freed exactly once. |
| Python  | `_cb = ctypes.CFUNCTYPE(...)(impl)` (kept by helper)     | `_ev.set()` in the callback's `finally` releases the helper's `_ev.wait()` | The helper blocks on the event so `_cb` (and its trampoline) outlive the callback. |
| C++     | `new std::promise<T>()` plus the lambda capture          | `delete p;` once at the end of the lambda       | The lambda owns the heap promise on every exit branch. |
| Dart    | `NativeCallable<...>.listener(...)`                      | `callable.close()` in `finally` and on the catch path | Pointer-typed parameters are kept alive in `whenComplete`. |
| WASM    | `_registerTrampoline` per signature plus `_asyncContexts.set(ctxId, ...)` per call | `_asyncContexts.delete(ctxId)` in the trampoline | Per-call resolver closures are removed after resolve/reject. |
| Go      | _Not async-capable_; `async: true` is skipped today | n/a | Re-enabling Go async requires solving channel-vs-callback lifetime. |
| Ruby    | _Not async-capable_; `async: true` is skipped today | n/a | Future async impl must `rb_global_variable` the callback and release it on completion. |

### Audit invariants

For every async-capable target:

1. The `void* context` has exactly one owner at any moment.
2. The callback closure is pinned by an explicit "+1" allocation
   (`GCHandle.Alloc`, `Unmanaged.passRetained`, `NewGlobalRef`,
   `NativeCallable.listener`, …) before the C worker can see it, and
   released by the matching "-1" exactly once on the callback path.
3. Synchronous failure of the C call (the callback never fires) is
   handled in a `catch` / `try` that frees the pin so it does not leak.
4. The stress test asserts `weaveffi_tasks_active_callbacks()` returns
   to zero after 1000 concurrent calls.

## Pitfalls

- **Async void functions** — the validator emits a warning. They are
  valid but almost always indicate a missing return type.
- **Forgetting `cancellable: true`** — without it, `weaveffi_cancel`
  is a no-op for that function.
- **Using async for CPU-bound work** — the callback overhead exceeds
  the work being done; keep it synchronous.
- **Calling Go/Ruby async functions** — the generators skip async
  functions entirely for these targets today. Either keep the function
  synchronous or run the async path from a different consumer.
- **Letting the callback closure get garbage-collected** — every
  generator pins it explicitly. Do not strip those pins when editing
  generated code by hand.
- **Returning `null` instead of invoking the callback** — the contract
  is that the callback fires **exactly once** for every async call,
  including on cancellation.
