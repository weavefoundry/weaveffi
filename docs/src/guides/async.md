# Async Functions

WeaveFFI supports marking functions as asynchronous using the `async: true`
field in the IDL. Async functions represent operations that execute
off the calling thread and deliver their result via a callback or
language-native async mechanism.

## IDL declaration

```yaml
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

## How async works across the C ABI

Async functions use a **callback-based** pattern at the C ABI layer. Instead
of returning a value directly, the C function accepts a callback pointer and a
user-data pointer. When the operation completes, Rust invokes the callback
with the result (or error) on a background thread.

```c
typedef void (*weaveffi_callback_string)(
    const char* result,
    const weaveffi_error* err,
    void* user_data
);

void weaveffi_mymod_fetch_data(
    const uint8_t* url, size_t url_len,
    weaveffi_callback_string on_complete,
    void* user_data
);
```

For cancellable functions, the C ABI additionally returns a cancel handle:

```c
uint64_t weaveffi_mymod_upload_file(
    const uint8_t* path, size_t path_len,
    const uint8_t* data, size_t data_len,
    weaveffi_callback_bool on_complete,
    void* user_data
);

void weaveffi_cancel(uint64_t cancel_handle);
```

## Target language patterns

Each generator maps the callback-based C ABI to the target language's
native async idiom.

### Swift

Async functions generate Swift `async throws` methods. The wrapper bridges
from the C callback to Swift's structured concurrency using
`withCheckedThrowingContinuation`:

```swift
public static func fetchData(_ url: String) async throws -> String {
    return try await withCheckedThrowingContinuation { continuation in
        // ... marshal params, call C ABI with callback ...
    }
}
```

For cancellable functions, the generated code uses `withTaskCancellationHandler`
to wire Swift task cancellation to the C ABI cancel function.

### Kotlin/Android

Async functions generate Kotlin `suspend` functions. The wrapper uses
`suspendCancellableCoroutine` to bridge from the JNI callback to Kotlin
coroutines:

```kotlin
suspend fun fetchData(url: String): String =
    suspendCancellableCoroutine { cont ->
        // ... call JNI native method with callback ...
    }
```

Cancellable functions register an `invokeOnCancellation` handler that calls
the C ABI cancel function.

### Node.js

Async functions return a `Promise`. The wrapper creates a Promise and passes
resolve/reject callbacks through the N-API bridge:

```typescript
export function fetchData(url: string): Promise<string>
```

### WASM

Async functions return a `Promise` in the WASM JavaScript bindings:

```typescript
export function fetchData(url: string): Promise<string>
```

### Python

Async functions generate `async def` wrappers that bridge from the C callback
to Python's `asyncio` event loop using `loop.create_future()`:

```python
async def fetch_data(url: str) -> str:
    loop = asyncio.get_running_loop()
    future = loop.create_future()
    # ... call C ABI with callback that resolves the future ...
    return await future
```

### .NET

Async functions generate `Task<T>`-returning methods. The wrapper uses a
`TaskCompletionSource` to bridge from the native callback:

```csharp
public static Task<string> FetchDataAsync(string url)
{
    var tcs = new TaskCompletionSource<string>();
    // ... P/Invoke with callback that sets tcs result ...
    return tcs.Task;
}
```

### Go

Async functions generate Go functions that accept a callback parameter or
return a channel, matching Go's concurrency idioms:

```go
func MymodFetchData(url string, callback func(string, error))
```

### Ruby

Async functions are currently **skipped** by the Ruby generator. The
generated Ruby module only includes synchronous function wrappers.

### Dart

Async functions generate Dart `Future<T>`-returning methods using
`Completer`:

```dart
Future<String> fetchData(String url) {
  final completer = Completer<String>();
  // ... call FFI with callback that completes the future ...
  return completer.future;
}
```

### C / C++

The C and C++ generators emit the raw callback-based interface directly,
since C and C++ do not have a standard async runtime. The caller is
responsible for managing threading and callback lifetime.

## Validator behaviour

- Async functions with no return type emit a **warning** (async void is
  unusual and may indicate a missing return type).
- Async functions with a return type pass validation normally.
- `cancellable: true` is only meaningful when `async: true`. Setting
  `cancellable` on a synchronous function has no effect.

## Memory and lifetime

The asynchronous C ABI hands the foreign side two pointers — a function
pointer for the callback and a `void* context` opaque pointer — that the
Rust worker keeps until it dispatches the result. Every binding has to
keep those two pointers alive across the suspend point, hand ownership
across one (and only one) thread boundary, and release the underlying
language-side resources exactly once on the callback path. Getting this
wrong silently corrupts memory: a callback that gets garbage-collected
before the worker fires leaves the worker calling into freed memory; a
context pointer freed twice yields a double-free.

The matrix below is the contract every generator implements. Each row
is verified by a `{generator}_async_pins_callback_for_lifetime` unit test
and exercised under load by the per-target stress tests in
`examples/{target}/async_stress.{ext}` (1000 concurrent calls each,
wired into `examples/run_all.sh`).

| Target  | Pin (allocate / retain)                                | Unpin (free / release) on callback             | Notes |
|---------|---------------------------------------------------------|------------------------------------------------|-------|
| Swift   | `Unmanaged.passRetained(ContinuationRef(...))`          | `Unmanaged.fromOpaque(ctx).takeRetainedValue()` | The retained `+1` is dropped exactly once when the continuation resumes. |
| .NET    | `GCHandle.Alloc(callback, GCHandleType.Normal)`         | `GCHandle.FromIntPtr(context).Free()`           | `GCHandle.ToIntPtr(handle)` is passed as the `context`; the C catch path also frees the handle on synchronous failure. |
| Kotlin  | JNI `(*env)->NewGlobalRef(env, callback)`               | `(*env)->DeleteGlobalRef(env, ctx->callback)`   | The JNI shim also `malloc`s and `free`s the per-call `weaveffi_jni_async_ctx` exactly once. |
| Node.js | `napi_create_promise(env, &deferred, &promise)`         | `napi_resolve_deferred` or `napi_reject_deferred` (whichever runs first frees the deferred) | The N-API runtime owns the deferred; the per-call `weaveffi_napi_async_ctx` is `malloc`-ed and `free`-d exactly once. |
| Python  | `_cb = ctypes.CFUNCTYPE(...)(impl)` (held in the local synchronous helper) | `_ev.set()` in the callback's `finally` releases the helper's `_ev.wait()` | `ctypes` auto-acquires the GIL on the C callback thread; the helper blocks on the event so `_cb` (and the trampoline it wraps) stay alive until the callback fires. |
| C++     | `new std::promise<T>()` plus the lambda capture          | `delete p;` once at the end of the lambda       | The lambda owns the heap promise on every exit branch (set value, set exception). |
| Dart    | `NativeCallable<...>.listener(...)` (assigned via `late`) | `callable.close()` in the listener's `finally` and on the synchronous catch path | Pointer-typed parameters are kept alive in `whenComplete` so the C side can read them across the suspension. |
| WASM    | One `_registerTrampoline` per callback signature (lives for the API instance) plus `_asyncContexts.set(ctxId, ...)` per call | `_asyncContexts.delete(ctxId)` in the trampoline | The trampoline never has to be removed; per-call resolver closures are removed from the map after resolve/reject. |
| Go      | _Not async-capable._ The Go generator skips `async: true` functions because a CGo callback's lifetime cannot exceed the channel it would resume. | n/a | Re-enabling Go async requires solving the channel-vs-callback lifetime problem first. |
| Ruby    | _Not async-capable._ The Ruby generator skips `async: true` functions; the Ruby `ffi` gem has no idiomatic async primitive to bind to. | n/a | A future Ruby async implementation must `rb_global_variable` the callback and unregister it on completion. |

### Audit invariants

For every async-capable target:

1. The user-supplied `void* context` has exactly one owning entity at any
   moment. Ownership is handed off to the C worker on the call and
   handed back to (and freed by) the callback. There is no path on which
   both the caller and the callback free the same context.
2. The callback closure is pinned in language-managed memory by an
   explicit "+1" allocation (`GCHandle.Alloc`,
   `Unmanaged.passRetained`, `NewGlobalRef`, `NativeCallable.listener`,
   …) before the C worker can see it, and released by the matching "-1"
   exactly once on the callback path.
3. Synchronous failure of the C call (the callback never fires) is
   handled in a `catch` / `try` that frees the pin so it does not leak.
4. The stress test under `examples/{target}/async_stress.{ext}` spawns
   1000 concurrent calls to `weaveffi_tasks_run_n_tasks_async`, awaits
   them, asserts each returned the expected `n`, and checks that
   `weaveffi_tasks_active_callbacks()` returns to zero — a simple FFI
   counter exposed by `samples/async-demo` that tracks in-flight worker
   threads. A leaked callback would manifest as a hang (the worker
   calling into freed memory) or a non-zero counter at the end.

### Cancellation

For `cancellable: true` functions, the C ABI also takes a
`weaveffi_cancel_token*` argument. Cancellation is observed by the Rust
worker but the callback is **always** invoked exactly once — either with
the result or with a `Cancelled` error. This means the pin/unpin pair
above runs on the cancellation path identically to the success path; no
extra cleanup is required by the generator.

## Best practices

1. **Prefer async for I/O-bound operations.** Network requests, file I/O,
   and database queries are good candidates for async.
2. **Use cancellable for long-running operations.** File uploads, streaming
   downloads, and batch processing should be cancellable.
3. **Avoid async for CPU-bound work.** Short computations (math, parsing,
   validation) should remain synchronous.
4. **Always specify a return type.** Async void functions are valid but
   unusual — the validator will warn you.
