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

## Best practices

1. **Prefer async for I/O-bound operations.** Network requests, file I/O,
   and database queries are good candidates for async.
2. **Use cancellable for long-running operations.** File uploads, streaming
   downloads, and batch processing should be cancellable.
3. **Avoid async for CPU-bound work.** Short computations (math, parsing,
   validation) should remain synchronous.
4. **Always specify a return type.** Async void functions are valid but
   unusual — the validator will warn you.
