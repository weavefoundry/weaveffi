# Async and Cancellation

An async function returns immediately and delivers its result later, through
the language's own async idiom: `async`/`await` in Swift, Python,
JavaScript, C#, and Dart, a `suspend fun` in Kotlin, a `std::future` in C++,
and a blocking call that honors a `context.Context` in Go. A cancellable
async function is also cancelled through that idiom: cancelling the awaiting
task, coroutine, or promise cancels the native call.

## Declaring async functions

In Rust, write an `async fn` (a free function, method, or static). A final
`weaveffi::CancelToken` parameter makes it cancellable. From the
[macro tour](producer-macro.md):

```rust,ignore
{{#include ../../../examples/macro-tour/src/lib.rs:async}}
```

In an IDL, set `async: true`, and optionally `cancellable: true`, on a
function, method, or static:

```yaml
{{#include ../../../examples/idl/contacts.yml:async}}
```

`async` composes with `throws`, with object and buffered results, and with
every parameter type. Constructors can't be async (`AsyncConstructor`), so
an async factory is an async function or static that returns the interface.
In Rust, an `async fn` in an exported `impl` that returns `Self` (or the
interface) is exported as such a static, and a free `async fn` returning
the object works too. Async functions
can't return iterators (`AsyncIteratorReturn`), and `cancellable` without
`async` is a validation error (`CancellableNotAsync`). A Rust `async fn`
can't borrow an optional parameter (`Option<&T>`); take `Option<T>`.

## The C ABI shape

An async function lowers to a launcher named after the function itself and a
completion callback type named `{symbol}_callback`:

```c
typedef void (*macro_tour_greet_later_callback)(void* context, macro_tour_error* err,
                                                macro_tour_run result);

void macro_tour_greet_later(macro_tour_run name, uint64_t delay,
                            macro_tour_cancel_token* cancel_token,  /* cancellable only */
                            macro_tour_greet_later_callback callback, void* context);
```

`result` is the return's one C value, exactly as for a synchronous return (a
scalar, a `{p}_opt_*`, a run, or an object pointer), and is absent for a
function that returns nothing: the completion is then `void (*)(void*
context, {p}_error* err)`. kvstore's async `Store.version_of(key: string) ->
u32?` completes through `void (*)(void* context, kvstore_error* err,
kvstore_opt_u32 result)`. On failure `result` is the zero value. The
launcher has no `out_err`: every failure, including a marshalling failure of
an input, arrives through the callback.

## The completion contract

The runtime guarantees these for every launch:

1. **Exactly once.** The callback fires exactly once, on success, on a
   domain error, on a marshalling failure (`-3`), on a panic (`-2`, including
   one while the launcher lifts its inputs), on a consumer callback failure
   (`-4`), on cancellation (`-5`), and when the executor can't take the call
   (`-1`). It may fire before the launcher returns, and a null `callback`
   discards the result.
2. **From any thread.** It runs on whatever thread completes the future: an
   executor thread, a Tokio worker, or (on `wasm32`) the caller's own stack.
   Each binding hops back to its own scheduler before touching consumer state.
3. **Inputs taken at launch.** The launcher copies runs and retains objects
   and `self` before it returns, so the consumer may release its arguments
   as soon as the launcher returns.
4. **Owned results.** `err` is `NULL` on success; otherwise it's heap-boxed,
   owned by the consumer, and released with `{p}_error_free`. A run result
   is owned by the consumer and released with `{p}_run_free` after decoding,
   and an object result moves one strong reference.
5. **Dropped futures complete.** If the executor drops a future before it
   finishes (a runtime shutting down, say), the call completes with `-5`.
6. **Spawn failures complete.** If the executor can't take the future (a
   custom spawner panics, or the default executor can't start its threads),
   the call completes with `-1` and a message saying why.

A consumer of the raw C surface follows the same rules in its callback: read
or decode everything, free it, and adopt object pointers.

## Cancellation

The runtime surface provides reference-counted cancel tokens:

```c
macro_tour_cancel_token* token = macro_tour_cancel_token_create();  /* refcount 1 */
macro_tour_greet_later(name, 60000, token, on_done, ctx);
macro_tour_cancel_token_cancel(token);    /* any thread, any time; idempotent */
macro_tour_cancel_token_destroy(token);   /* drops the consumer's reference */
```

The launcher takes its own reference on the token, so the consumer may cancel
and destroy its reference at any point after the launch, even before the
call completes. Cancelling wakes the producer's future; the runtime then
drops the future and completes the call with code `-5` and the message
`cancelled`, unless it had already completed. A null token means "never
cancelled".

Producer code doesn't need to check the token: dropping the future cancels
it at its next suspension point, and RAII guards run as usual. Poll
`cancel.is_cancelled()` only for cooperative cleanup of work the future
doesn't own, such as a thread it spawned (the `kvstore` sample's
`Store::compact` stops its background pause that way). A future that never
suspends can't be interrupted; it runs to completion and its result wins.

## Per-target surface

| Target | Async surface | Cancellation |
|--------|---------------|--------------|
| [C](../generators/c.md) | launcher plus callback | a `{p}_cancel_token*` argument |
| [C++](../generators/cpp.md) | `std::future<T>` | a cancel-token argument |
| [Swift](../generators/swift.md) | `async` (`async throws`) | cancelling the `Task` |
| [Kotlin](../generators/kotlin.md) | `suspend fun` | cancelling the coroutine |
| [Node.js](../generators/node.md) | `Promise<T>` | an `AbortSignal` |
| [WebAssembly](../generators/wasm.md) | `Promise<T>` | an `AbortSignal` |
| [Python](../generators/python.md) | awaitable | cancelling the awaiting task |
| [.NET](../generators/dotnet.md) | `Task<T>` | a `CancellationToken` |
| [Dart](../generators/dart.md) | `Future<T>` | a cancel-token object |
| [Go](../generators/go.md) | blocking call with a `context.Context` | the context's cancellation |
| [Ruby](../generators/ruby.md) | blocking call | a cancel-token keyword argument |

A cancelled call raises the language's cancellation error. Go and Ruby block
the calling goroutine or thread until the completion fires (the native work
still runs off-thread), so call them from a goroutine or `Thread` for
concurrency. The exact types and signatures are on each language page.

## Choosing an executor

Something has to drive each future between launch and completion. The
runtime routes every future to the process-wide executor:

- **Tokio (the default).** The `weaveffi` crate's `tokio` feature is on by
  default, and with it the executor is Tokio: the current runtime when the
  launcher is called from inside one, otherwise a multi-thread runtime the
  library creates on first use (its threads are named `weaveffi-async`).
  That runtime enables every Tokio driver the build compiles in, so turn on
  the Tokio features your futures need (`time`, `net`, and so on) in your
  own dependency on `tokio`, as the macro tour does for `tokio::time::sleep`.
  Use `tokio::task::spawn_blocking` for synchronous work inside a future.

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

  ```rust,ignore
  {{#include ../../../examples/macro-tour/src/lib.rs:spawner}}
  ```

  The first call wins; later calls return an error. The spawner must not
  block, because the launcher calls it on the consumer's thread. Every
  future it receives is already wrapped so a panic becomes a `-2`
  completion, a spawner that drops a future still produces a `-5`
  completion, and a spawner that panics completes the call with `-1`.
- **`wasm32`.** There are no threads (Tokio is never compiled in there, so
  the `tokio` feature has no effect), and the future is polled inline before
  the launcher returns. A future that finishes after bounded work is fine. A
  future that's still pending once nothing can wake it (it awaits a timer or
  I/O) is dropped, and the call completes with `-1` and the message `async
  function suspended with no executor on wasm32` rather than hanging the
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
  `{p}_error_free`, not `error_clear`.
- **Leaking a result run.** A raw C callback that only copies the result
  must still free it with `{p}_run_free`.
