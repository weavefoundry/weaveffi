# Async and Cancellation

An async function returns immediately and delivers its result later, through
the language's own async idiom: `async`/`await` in Swift, Python, JavaScript,
C#, and Dart, a `suspend fun` in Kotlin, a `std::future` in C++, and a
blocking call that honors a `context.Context` in Go. A cancellable async
function is also cancelled through that idiom: cancelling the awaiting task,
coroutine, or promise cancels the native call.

## Declaring async functions

In Rust, write an `async fn` (a free function, method, or static). Add
`#[weaveffi::cancellable]` and a final `weaveffi::CancelToken` parameter to
make it cancellable:

```rust
#[weaveffi::interface]
pub struct Store { /* ... */ }

impl Store {
    /// Reclaim space; returns the bytes reclaimed.
    #[weaveffi::cancellable]
    pub async fn compact(&self, cancel: weaveffi::CancelToken) -> Result<i64, KvError> {
        let _ = cancel;
        /* ... */
        Ok(0)
    }
}
```

In an IDL, set `async: true` and optionally `cancellable: true` on a
function, method, or static:

```yaml
version: "0.10.0"
modules:
  - name: kv
    errors:
      name: KvError
      codes:
        - { name: IoError, code: 1004, message: "I/O failure" }
    interfaces:
      - name: Store
        constructors:
          - name: open
            params:
              - { name: path, type: string }
            throws: true
        methods:
          - name: compact
            return: i64
            throws: true
            async: true
            cancellable: true
```

`async` composes with `throws`, with object and buffered results, and with
every parameter type. Constructors can't be async (use an async static that
returns the interface), async functions can't return iterators, and
`cancellable` without `async` is a validation error (`CancellableNotAsync`).

## The C ABI shape

An async function lowers to a launcher named after the function itself and a
completion callback typedef named `{symbol}_callback`:

```c
typedef void (*kvstore_kv_Store_compact_callback)(
    void* context, kvstore_error* err, int64_t result);

void kvstore_kv_Store_compact(
    const kvstore_kv_Store* self,
    kvstore_cancel_token* cancel_token,     /* cancellable functions only */
    kvstore_kv_Store_compact_callback callback,
    void* context);
```

The result slots are nothing (a `void` function), one direct value or object
pointer named `result`, or `const uint8_t* result_ptr, size_t result_len` for
strings, bytes, and value buffers. The launcher has no `out_err`: every
failure, including a marshalling failure of an input, arrives through the
callback.

## The completion contract

The runtime guarantees these for every launch:

1. **Exactly once.** The callback fires exactly once, on success, on a
   domain error, on a marshalling failure (`-3`), on a panic (`-2`), on a
   consumer callback failure (`-4`), and on cancellation (`-5`).
2. **From any thread.** It runs on whatever thread completes the future: an
   executor thread, a Tokio worker, or (on `wasm32`) the caller's own stack.
   Each binding hops back to its own scheduler before touching consumer state.
3. **Owned results.** `err` is `NULL` on success; otherwise it's heap-boxed,
   owned by the consumer, and released with `{prefix}_error_free`. A string,
   bytes, or buffered result is owned by the consumer and released with
   `{prefix}_free_bytes(result_ptr, result_len)` after decoding. An object
   result transfers one strong reference.
4. **Dropped futures complete.** If the executor drops a future before it
   finishes (a runtime shutting down, say), the drop guard completes the call
   with `-5`.

A consumer of the raw C surface follows the same rules in its callback: read
or decode everything, free it, and adopt object pointers.

## Cancellation

The runtime surface provides reference-counted cancel tokens:

```c
kvstore_cancel_token* token = kvstore_cancel_token_create();   /* refcount 1 */
kvstore_kv_Store_compact(store, token, on_done, ctx);
kvstore_cancel_token_cancel(token);    /* any thread, any time; idempotent */
kvstore_cancel_token_destroy(token);   /* drops the consumer's reference */
```

The launcher takes its own reference on the token, so the consumer may cancel
and destroy its reference at any point after the launch, even before the
call completes. Cancelling wakes the producer's future; the runtime then
drops the future and completes the call with code `-5` and the message
"cancelled", unless it had already completed. A null token means "never
cancelled".

Producer code doesn't need to check the token: dropping the future cancels
it at its next suspension point, and RAII guards run as usual. Poll
`cancel.is_cancelled()` only for cooperative cleanup of work the future
doesn't own, such as a thread it spawned. Note that a future that never
suspends can't be interrupted; it runs to completion and its result wins.

## Per-target surface

| Target | Async surface | Cancellation | `-5` raises |
|--------|---------------|--------------|-------------|
| [C](../generators/c.md) | launcher plus callback | `{prefix}_cancel_token*` slot | the code in `err` |
| [C++](../generators/cpp.md) | `std::future<T>` | RAII `CancelToken` argument | a `Cancelled` exception |
| [Swift](../generators/swift.md) | `async` (`async throws`) | cancelling the `Task` | `CancellationError` |
| [Kotlin](../generators/kotlin.md) | `suspend fun` | cancelling the coroutine | `CancellationException` |
| [Node.js](../generators/node.md) | `Promise<T>` | `AbortSignal` (`{ signal }`) | `CancelledError` |
| [WebAssembly](../generators/wasm.md) | `Promise<T>` | `AbortSignal` (`{ signal }`) | `CancelledError` |
| [Python](../generators/python.md) | awaitable | cancelling the awaiting task | `asyncio.CancelledError` |
| [.NET](../generators/dotnet.md) | `Task<T>` | `CancellationToken` | `OperationCanceledException` |
| [Dart](../generators/dart.md) | `Future<T>` | a generated cancel-token object | `CancelledException` |
| [Go](../generators/go.md) | blocking call, `context.Context` first | `ctx` done | returns `ctx.Err()` |
| [Ruby](../generators/ruby.md) | blocking call | `cancel:` keyword with a `CancelToken` | a `Cancelled` error |

Go and Ruby block the calling goroutine or thread until the completion fires
(the native work still runs off-thread), so call them from a goroutine or
`Thread` for concurrency. Exact type names and signatures are on each
language page.

## Choosing an executor

Something has to drive each future between launch and completion. The
runtime routes every future to the process-wide spawner:

- **Default.** Each launch gets a fresh thread that drives the future with a
  minimal executor. It needs no runtime and suits CPU-bound work and futures
  woken from other threads, but it has no reactor: a future that awaits
  Tokio's I/O or timers never wakes.
- **Custom.** Call `weaveffi::set_spawner` once at startup, before the first
  async launch. It accepts any `Fn(BoxFuture) + Send + Sync + 'static`:

  ```rust
  let handle = tokio::runtime::Handle::current();
  weaveffi::set_spawner(move |fut| {
      handle.spawn(fut);
  })
  .expect("spawner installed once");
  ```

  The first call wins; later calls return `Err(SpawnerAlreadySet)`. The
  spawner must not block, because the launcher calls it on the consumer's
  thread. Every future it receives is already wrapped so a panic becomes a
  `-2` completion, and a spawner that drops a future still produces a `-5`
  completion.
- **`wasm32`.** There are no threads, so the default drives the future
  inline before the launcher returns. Futures that finish after bounded work
  are fine; one that waits for an external wake (a timer, I/O) hangs the
  module. A call usually completes before an `AbortSignal` can fire, so on
  this target cancellation mostly matters for a signal that's already
  aborted when the call starts.

## Pitfalls

- **Awaiting Tokio under the default spawner.** The future never wakes and
  the callback never fires; install a Tokio spawner first.
- **Expecting the token to stop CPU-bound work.** Cancellation drops the
  future at a suspension point. Long synchronous loops should yield or check
  `is_cancelled()`.
- **Clearing an async error.** `err` is boxed; release it with
  `{prefix}_error_free`, not `error_clear`.
- **Leaking a result buffer.** A raw C callback that only copies the result
  must still free it.
- **Async functions with no return value.** Valid, but `weaveffi validate
  --warn` flags them (`AsyncVoidFunction`), since they're usually a missing
  return type.
