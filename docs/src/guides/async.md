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
make it cancellable. This is the `kvstore` sample's `Store::compact`:

```rust
#[weaveffi::interface]
pub struct Store { /* ... */ }

impl Store {
    /// Remove every expired entry and complete with how many were removed,
    /// after pausing for `pause_ms` on a thread that polls the token.
    #[weaveffi::cancellable]
    pub async fn compact(&self, pause_ms: u32, cancel: weaveffi::CancelToken) -> u32 {
        Pause::start(pause_ms, &cancel).await;
        self.sweep()
    }
}
```

In an IDL, set `async: true` and optionally `cancellable: true` on a
function, method, or static:

```yaml
version: "0.12.0"
modules:
  - name: kv
    errors:
      - name: KvError
        codes:
          - { name: InvalidPath, code: 1004, message: "invalid path" }
    interfaces:
      - name: Store
        constructors:
          - name: open
            params:
              - { name: path, type: string }
            throws: KvError
        methods:
          - name: compact
            params:
              - { name: pause_ms, type: u32 }
            return: u32
            async: true
            cancellable: true
          - name: version_of
            params:
              - { name: key, type: string }
            return: u32?
            async: true
```

`async` composes with `throws`, with object and buffered results, and with
every parameter type. Constructors can't be async (`AsyncConstructor`):
declare an async static that returns the interface instead, which in Rust is
an `async fn` associated function returning `Self` (the macro reads only a
synchronous one as a constructor). Async functions can't return iterators
(`AsyncIteratorReturn`), and `cancellable` without `async` is a validation
error (`CancellableNotAsync`).

## The C ABI shape

An async function lowers to a launcher named after the function itself and a
completion callback typedef named `{symbol}_callback`:

```c
typedef void (*kvstore_kv_Store_compact_callback)(
    void* context, kvstore_error* err, uint32_t result);

void kvstore_kv_Store_compact(
    const kvstore_kv_Store* self,
    uint32_t pause_ms,
    kvstore_cancel_token* cancel_token,     /* cancellable functions only */
    kvstore_kv_Store_compact_callback callback,
    void* context);
```

The result slots follow the value's family:

| Result | Completion slots after `context, err` |
|--------|----------------------------------------|
| none (`void`) | none |
| Direct (scalar, `bool`, C-style enum) | `T result` |
| OptDirect (`u32?`, `bool?`, `Color?`) | `bool has_result, T result` |
| Slice (`[i32]`, `[f64]`, ...) | `const T* result_ptr, size_t result_len` (element count) |
| string, bytes, value buffer | `const uint8_t* result_ptr, size_t result_len` |
| object | `{tag}* result` (one strong reference) |

`version_of` above completes through
`void (*)(void* context, kvstore_error* err, bool has_result, uint32_t
result)`. On failure every result slot is zero (`false`, `0`, `NULL`). The
launcher has no `out_err`: every failure, including a marshalling failure of
an input, arrives through the callback.

## The completion contract

The runtime guarantees these for every launch:

1. **Exactly once.** The callback fires exactly once, on success, on a
   domain error, on a marshalling failure (`-3`), on a panic (`-2`, including
   one while the launcher lifts its inputs), on a consumer callback failure
   (`-4`), on cancellation (`-5`), and when the executor can't take the call
   (`-1`).
2. **From any thread.** It runs on whatever thread completes the future: an
   executor thread, a Tokio worker, or (on `wasm32`) the caller's own stack.
   Each binding hops back to its own scheduler before touching consumer state.
3. **Owned results.** `err` is `NULL` on success; otherwise it's heap-boxed,
   owned by the consumer, and released with `{prefix}_error_free`. A string,
   bytes, or buffered result is owned by the consumer and released with
   `{prefix}_free_bytes(result_ptr, result_len)` after decoding; a typed
   array with `{prefix}_free_bytes((uint8_t*)result_ptr, result_len *
   sizeof(T))`. An OptDirect or Direct result is copied, and an object
   result transfers one strong reference.
4. **Dropped futures complete.** If the executor drops a future before it
   finishes (a runtime shutting down, say), the drop guard completes the call
   with `-5`.
5. **Spawn failures complete.** If the executor can't take the future (a
   custom spawner panics, or the default executor can't start its threads),
   the call completes with `-1` and a message saying why.

A consumer of the raw C surface follows the same rules in its callback: read
or decode everything, free it, and adopt object pointers.

## Cancellation

The runtime surface provides reference-counted cancel tokens:

```c
kvstore_cancel_token* token = kvstore_cancel_token_create();   /* refcount 1 */
kvstore_kv_Store_compact(store, 100, token, on_done, ctx);
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
runtime routes every future to the process-wide executor:

- **Tokio (the default).** The `weaveffi` crate's `tokio` feature is on by
  default, and with it the executor is Tokio: the current runtime when the
  launcher is called from inside one, otherwise a multi-thread runtime the
  library creates on first use (its threads are named `weaveffi-async`).
  That runtime enables every Tokio driver the build compiles in, so turn on
  the Tokio features your futures need (`time`, `net`, and so on) in your
  own dependency on `tokio`. Use `tokio::task::spawn_blocking` for
  synchronous work inside a future.

  ```toml
  [dependencies]
  weaveffi = "0.25"
  tokio = { version = "1", features = ["time"] }
  ```

- **A thread per call.** With `default-features = false`, there's no Tokio
  dependency, and each async call runs on a thread of its own (also named
  `weaveffi-async`), which drives the future with a minimal blocking
  executor and exits when the call completes. A future woken from another
  thread resumes on its own thread. There's no reactor, so a future that
  awaits Tokio's I/O or timers never wakes; use this for futures that wait
  only on channels, other threads, or nothing at all. A thread that can't
  start completes the call with `-1`.

  ```toml
  [dependencies]
  weaveffi = { version = "0.25", default-features = false }
  ```

- **Custom.** Call `weaveffi::set_spawner` once at startup, before the first
  async launch; it overrides both defaults. It accepts any
  `Fn(BoxFuture) + Send + Sync + 'static`:

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
  `-2` completion, a spawner that drops a future still produces a `-5`
  completion, and a spawner that panics completes the call with `-1`.
- **`wasm32`.** There are no threads (Tokio is never compiled in there, so
  the `tokio` feature has no effect), and the future is polled inline before
  the launcher returns. A future that finishes after bounded work is fine. A
  future that's still pending once nothing can wake it (it awaits a timer or
  I/O) is dropped, and the call completes with `-1` and the message "async
  function suspended with no executor on wasm32" rather than hanging the
  module. A call usually completes before an `AbortSignal` can fire, so on
  this target cancellation mostly matters for a signal that's already
  aborted when the call starts.

## Pitfalls

- **Awaiting Tokio without the `tokio` feature.** The thread-per-call
  executor has no reactor, so the future never wakes and the callback never
  fires; keep the default feature or install a Tokio spawner.
- **Blocking a Tokio worker.** A future that blocks (synchronous I/O, a
  long computation) holds a runtime thread; wrap the work in
  `tokio::task::spawn_blocking`.
- **Expecting the token to stop CPU-bound work.** Cancellation drops the
  future at a suspension point. Long synchronous loops should yield or check
  `is_cancelled()`.
- **Clearing an async error.** `err` is boxed; release it with
  `{prefix}_error_free`, not `error_clear`.
- **Leaking a result buffer.** A raw C callback that only copies the result
  must still free it (a typed array's length times `sizeof(T)`).
- **Async functions with no return value.** Valid, but `weaveffi validate
  --warn` flags an async free function that returns nothing
  (`AsyncVoidFunction`), since it's usually a missing return type.
