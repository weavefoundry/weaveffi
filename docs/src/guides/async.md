# Async Functions

## Overview

WeaveFFI exposes asynchronous Rust operations through a single
callback-based C ABI and language-native async wrappers in every target. Mark
a function with `async: true` (and optionally `cancellable: true`) in the
IDL, or write a plain `async fn` under `#[weaveffi::module]`, and the
generators emit the right shape per target: `async` in Swift (`async throws`
when the function also declares `throws: true`), `suspend fun` in Kotlin,
`Promise<T>` in JS, `async def` in Python, `Task<T>` in .NET, and so on.
When an async function declares `throws: true`, the failure that settles the
future is the module's typed domain error (see the
[Error Handling Guide](errors.md)).

The completion contract every wrapper implements is stated once, in
`weaveffi_core::plan::AsyncProtocol`: the callback fires exactly once per
launch, from an arbitrary producer thread, and everything it receives (the
error and the result) is owned by the consumer, which releases it through the
runtime free symbols or adopts it. See
[Result ownership and threading](#result-ownership-and-threading) below.

On the Rust side, something has to drive the future between the launcher
returning and the callback firing. That is the **spawner**: a process-wide
executor hook with a dependency-free default, replaceable once at startup
with `weaveffi::set_spawner` so producers built on Tokio (or any other
runtime) can run their futures where their I/O drivers live.

## When to use

Use async functions for:

- I/O-bound work (network, disk, database).
- Long-running operations that should not block the consumer's event loop
  (UI threads, JS event loop, asyncio loop).
- Operations the consumer should be able to cancel (combine with
  `cancellable: true`).

Avoid async for:

- Short CPU-bound work (math, parsing, validation). The callback overhead is
  more expensive than the call itself.
- Functions whose Rust implementation is purely synchronous and finishes in
  microseconds.

## Step-by-step

### 1. Declare the function in the IDL

```yaml
version: "0.9.0"
modules:
  - name: net
    errors:
      name: NetError
      codes:
        - { name: Unreachable, code: 1, message: "host unreachable" }
        - { name: Cancelled, code: 2, message: "the operation was cancelled" }
    functions:
      - name: fetch_data
        params:
          - { name: url, type: string }
        return: string
        async: true
        throws: true
        doc: "Fetches data from the given URL"

    interfaces:
      - name: Pool
        constructors:
          - name: new
            params:
              - { name: url, type: string }
        methods:
          - name: connect
            params: []
            return: Conn
            async: true
            cancellable: true
            throws: true
          - name: cached
            params: []
            return: Conn?
            async: true

      - name: Conn
        methods:
          - name: id
            params: []
            return: i64
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `async` | bool | `false` | Mark the function as asynchronous |
| `cancellable` | bool | `false` | Allow the async operation to be cancelled |
| `throws` | bool | `false` | Deliver failures as the module's typed domain error |

Here `fetch_data` and `connect` fail with a typed `NetError`, while `cached`
is non-throwing: the only failures it can surface are producer bugs. Async
works on free functions, interface methods, and statics alike; interface
constructors are always synchronous. An async function may return any
by-value type, a string, bytes, a buffered type (record, rich enum, list, map,
optional), an object (`Conn`), or a nullable object (`Conn?`); the one shape
the validator rejects is an async `iter<T>` (return a list instead).

### 2. Implement it in Rust

With the `#[weaveffi::module]` macro you write plain `async fn`s. The snippet
below implements the IDL above and installs Tokio as the spawner; it compiles
against `weaveffi` plus
`tokio = { version = "1", features = ["rt-multi-thread", "time"] }`:

```rust
#[weaveffi::module]
pub mod net {
    use std::sync::Arc;

    /// The network error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum NetError {
        /// host unreachable
        Unreachable = 1,
        /// the operation was cancelled
        Cancelled = 2,
    }

    /// Fetch a URL on the installed spawner.
    #[weaveffi::export]
    pub async fn fetch_data(url: String) -> Result<String, NetError> {
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        Ok(format!("payload from {url}"))
    }

    /// Install the Tokio runtime as the spawner. Call once, before the
    /// first async export is launched.
    #[weaveffi::export]
    pub fn init() -> bool {
        static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
        let rt = RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("tokio runtime")
        });
        let handle = rt.handle().clone();
        weaveffi::set_spawner(move |fut: weaveffi::BoxFuture| {
            handle.spawn(fut);
        })
        .is_ok()
    }

    /// A connection pool.
    #[weaveffi::interface]
    pub struct Pool {
        url: String,
    }

    /// One pooled connection.
    #[weaveffi::interface]
    pub struct Conn {
        pool: Arc<Pool>,
        id: i64,
    }

    impl Pool {
        /// Open a pool for `url`.
        pub fn new(url: String) -> Pool {
            Pool { url }
        }

        /// Lease a connection. The returned object holds a reference to
        /// the pool, so the pool outlives every connection.
        #[weaveffi::cancellable]
        pub async fn connect(self: Arc<Self>, cancel: weaveffi::CancelToken) -> Result<Arc<Conn>, NetError> {
            for _ in 0..10 {
                if cancel.is_cancelled() {
                    return Err(NetError::Cancelled);
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            Ok(Arc::new(Conn { pool: self, id: 1 }))
        }

        /// Look up a cached connection, if any.
        pub async fn cached(&self) -> Option<Arc<Conn>> {
            let _ = &self.url;
            None
        }
    }

    impl Conn {
        /// The connection id.
        pub fn id(&self) -> i64 {
            self.id
        }
    }
}

weaveffi::export_runtime!();
```

What the macro does with each piece:

- **Inputs are owned before the spawn.** The foreign caller may free or reuse
  its argument buffers as soon as the launcher returns, so strings and bytes
  are copied, value buffers are copied raw (and decoded inside the future),
  objects are retained with a new strong reference, and callback interfaces
  are lifted into their `Arc<dyn Trait>` on the caller's thread. A `&str` or
  `&T` spelling is then satisfied by lending the owned value.
- **The receiver is retained for the life of the call.** An async method
  takes one strong reference to its object before spawning, whether you wrote
  `&self` or `self: Arc<Self>`, so the consumer may drop its own wrapper the
  moment the launcher returns and the future still has a live object. Write
  `self: Arc<Self>` when the future needs to move the object into something
  it returns or stores (as `connect` does).
- **Object results transfer one strong reference.** `Arc<Conn>` and
  `Option<Arc<Conn>>` lower exactly like their synchronous counterparts: the
  callback's `result` slot is a `{tag}*` the consumer adopts and eventually
  releases with `weaveffi_net_Conn_destroy`; `None` is null.
- **`#[weaveffi::cancellable]` adds the token.** The attribute is required;
  the macro does not infer it from the parameter type. The producer declares
  a trailing `weaveffi::CancelToken` parameter that does not appear in the
  IDL signature, and the launcher fills it from its `cancel_token` slot.
- **Panics are caught.** The future runs under `weaveffi::abi::CatchUnwind`,
  so a panic (or a consumer callback failure) is delivered through the
  callback rather than into the executor. See
  [Panic handling](#panic-handling).

A hand-written producer implements the same pattern directly against
`weaveffi-abi`; the launcher symbol always carries the `_async` suffix
(`weaveffi_net_fetch_data_async`), keeping the plain name free for a
possible synchronous variant:

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
pub extern "C" fn weaveffi_net_fetch_data_async(
    url: *const c_char,
    callback: weaveffi_net_fetch_data_callback,
    context: *mut c_void,
) {
    let url_str = abi::c_ptr_to_string(url).unwrap_or_default();
    let ctx = context as usize;
    abi::spawn(async move {
        let payload = abi::string_to_c_ptr(&format!("payload from {url_str}"));
        // Ownership of the string transfers to the consumer, which
        // releases it with `weaveffi_free_string` after copying.
        callback(ctx as *mut c_void, std::ptr::null_mut(), payload);
    });
}
```

### 3. Choose a spawner

`weaveffi::abi::spawn` routes every async export's future to the installed
`Spawner`, or to the default when none is installed:

| | Behaviour |
|-|-----------|
| **Default** (nothing installed) | Each launch spawns a fresh OS thread and drives the future to completion there with `weaveffi::abi::block_on`, a minimal park/unpark executor. No runtime dependency; fine for CPU-bound futures and for futures woken from other threads (a channel, a `JoinHandle`). It has **no reactor**, so a future that awaits Tokio's I/O or timers never wakes. |
| **Custom** (`set_spawner`) | Your closure or `Spawner` implementation receives a `BoxFuture` (`Pin<Box<dyn Future<Output = ()> + Send + 'static>>`) and schedules it wherever you like. The future is already wrapped so a panic inside it is caught; a spawner never observes an unwinding future. |
| **`wasm32`** | There are no threads, so the default drives the future inline before the launcher returns; a future that returns `Pending` without an external wake would hang the module. See [WebAssembly](#webassembly). |

`set_spawner` is first-one-wins: it returns `Err(SpawnerAlreadySet)` on the
second call and never replaces an installed spawner. Install it from an
initialization export (as `init` above does), a library constructor, or the
first constructor of your main interface, before the first async export is
launched. `Spawner` is implemented for every
`Fn(BoxFuture) + Send + Sync + 'static`, so a closure forwarding to a
`tokio::runtime::Handle`, an `async_std::task::spawn`, or a `smol::Executor`
is enough. The trait's one rule: `spawn` must not block, because the launcher
that calls it runs on the consumer's thread.

### 4. Call it from each target

Swift:

```swift
let payload = try await Net.fetchData(url: "https://example.com/data")
let conn = try await pool.connect()
```

Kotlin:

```kotlin
val payload = Net.fetchData("https://example.com/data")
val conn = pool.connect()
```

Node.js:

```typescript
const payload = await fetchData("https://example.com/data");
const conn = await pool.connect();
```

Python:

```python
payload = await fetch_data("https://example.com/data")
conn = await pool.connect()
```

.NET:

```csharp
var payload = await Net.FetchData("https://example.com/data");
var conn = await pool.Connect();
```

Dart:

```dart
final payload = await fetchData('https://example.com/data');
final conn = await pool.connect();
```

Because `fetch_data` declares `throws: true`, the error that rejects the
promise (or resumes the continuation, or fails the task) is the typed
`NetError`, so a Swift consumer writes `catch NetError.unreachable` and a
Python consumer writes `except Unreachable`. An object result arrives as a
normal wrapper that owns one reference and releases it through the target's
disposal idiom.

### 5. Cancel a running operation

For `cancellable: true` functions the C launcher gains a
`weaveffi_cancel_token*` slot (before `callback` and `context`), and the
`weaveffi-abi` runtime provides the token lifecycle:

```c
weaveffi_cancel_token* token = weaveffi_cancel_token_create();
weaveffi_net_Pool_connect_async(pool, token, on_done, ctx);
/* later, from any thread: */
weaveffi_cancel_token_cancel(token);
/* after on_done has fired: */
weaveffi_cancel_token_destroy(token);
```

The Rust worker polls `cancel.is_cancelled()` (the `CancelToken` wraps
`weaveffi_cancel_token_is_cancelled`) at safe points and stops early, but the
callback is **always** invoked exactly once: either with the result or with
whatever error the producer chose to report for cancellation (`connect` above
returns `NetError::Cancelled`; the runtime has no reserved code for it). The
pin/unpin pair (see Reference) runs on the cancellation path identically to
the success path. A null token reads as "never cancelled", which is what the
wrappers that don't expose cancellation pass.

Today the **C and C++** surfaces expose the token (C++ as a trailing
`cancel_token = nullptr` parameter); the other wrappers pass null. The
operation runs to completion even if the consumer-side future is abandoned.

## Reference

### C ABI shape

Each async function gets its own callback typedef of the form
`(context, err, <result slots>)`, and a launcher with the `_async` suffix:

```c
typedef void (*weaveffi_net_fetch_data_callback)(
    void* context,
    weaveffi_error* err,
    const char* result);

void weaveffi_net_fetch_data_async(
    const char* url,
    weaveffi_net_fetch_data_callback callback,
    void* context);
```

The `err` argument of the callback carries the domain code for a
`throws: true` function; on a non-throwing function a non-zero code only ever
reports a producer bug (see
[Throws versus Trap](errors.md#throws-versus-trap)).

An async method takes its receiver first, and an object result is a `{tag}*`
the callback adopts:

```c
typedef void (*weaveffi_net_Pool_connect_callback)(
    void* context,
    weaveffi_error* err,
    weaveffi_net_Conn* result);

void weaveffi_net_Pool_connect_async(
    const weaveffi_net_Pool* self,
    weaveffi_cancel_token* cancel_token,
    weaveffi_net_Pool_connect_callback callback,
    void* context);

weaveffi_cancel_token* weaveffi_cancel_token_create(void);
void weaveffi_cancel_token_cancel(weaveffi_cancel_token* token);
bool weaveffi_cancel_token_is_cancelled(const weaveffi_cancel_token* token);
void weaveffi_cancel_token_destroy(weaveffi_cancel_token* token);
```

### Result ownership and threading

The completion contract has three clauses, stated once in
`weaveffi_core::plan::AsyncProtocol` and rendered by every wrapper:

1. **Single completion.** The callback fires exactly once per launch. The
   wrapper resolves its native future idiom (a Python `asyncio` future, a JS
   `Promise`, a Swift continuation, a C# `TaskCompletionSource`, a Go channel)
   exactly once and then releases the registration. This holds on every path:
   success, domain error, marshalling failure of an input (there is no
   `out_err` on a launcher, so a null string or malformed buffer is reported
   through the callback with `-3`), a null receiver, a panic, and
   cancellation.
2. **Owned results.** Result buffers passed to the callback (strings, bytes,
   and buffered results delivered as a
   `(const uint8_t* result_ptr, size_t result_len)` pair holding the
   serialized value buffer) are owned by the consumer: the wrapper copies or
   decodes them, then releases them through the runtime free symbols
   (`weaveffi_free_string` for strings, `weaveffi_free_bytes` for byte and
   value buffers). This is what lets runtimes that defer callback bodies past
   the native return, such as Dart's `NativeCallable.listener`, decode
   safely. Object results (including `Interface?`) transfer one strong
   reference the same way: the callback adopts the pointer into the wrapper's
   disposal idiom, which eventually calls the type's `_destroy` symbol.
3. **Foreign-thread delivery.** The callback runs on an arbitrary producer
   thread (a default-spawner thread, or a Tokio worker), so the wrapper hops
   back to its native scheduler before touching consumer state (Python's
   `call_soon_threadsafe`, Node's thread-safe function, a dispatched Swift
   continuation) rather than resolving inline where the target's runtime
   forbids it.

A non-null error passed to the callback is heap-boxed and also owned by the
consumer: the wrapper copies the code, message, and payload, then releases
the box exactly once with `weaveffi_error_free`. (This differs from the
synchronous `out_err` slot, which is caller-allocated and cleared with
`weaveffi_error_clear`.)

If you consume the raw C surface directly, the same rules apply to your
callback: copy or decode every buffer and then free it, release a non-null
error with `weaveffi_error_free`, and adopt object pointers.

### Panic handling

The generated launcher awaits the producer's future inside
`weaveffi::abi::CatchUnwind`, a future adapter that wraps each `poll` in
`catch_unwind` and resolves to `Err(payload)` instead of unwinding into the
executor. The launcher then reports the payload through the callback's `err`
with `error_set_panic`:

- an ordinary panic becomes code `-2` (`PANIC_ERROR_CODE`) with the message
  `producer panicked: <payload>`;
- a `ForeignError` payload (a consumer callback-interface implementation
  raised while the future was calling it) becomes code `-4`
  (`FOREIGN_ERROR_CODE`) with the consumer's own message.

Either way the callback fires exactly once and the installed spawner never
sees an unwinding task, so a panicking export can't take down a Tokio worker
or leave a JS `Promise` pending forever. On a `throws: true` function the
consumer sees these as the generic branded error; on a non-throwing function
they trap (see [Throws versus Trap](errors.md#throws-versus-trap)).

### Per-target async surface

| Target | Async surface | Cancel token exposure (`cancellable: true`) |
|--------|---------------|---------------------------------------------|
| C | Raw callback + `_async` launcher | `weaveffi_cancel_token*` slot before the callback |
| C++ | `std::future<T>` | trailing `cancel_token = nullptr` parameter |
| Swift | `async` (`async throws` with `throws: true`) | not exposed; wrapper passes `nil` |
| Kotlin | `suspend fun` | not exposed; wrapper passes `0L` |
| Node.js | `Promise<T>` (thread-safe function settling) | not exposed; wrapper passes `NULL` |
| Python | `async def` (asyncio future settled via `call_soon_threadsafe`) | not exposed; wrapper passes `None` |
| .NET | `Task<T>` | not exposed; wrapper passes `IntPtr.Zero` |
| Dart | `Future<T>` (`NativeCallable.listener`) | not exposed; wrapper passes `nullptr` |
| Wasm | `Promise<T>` (table trampolines) | not exposed; wrapper passes `0` |
| Go | blocking bridge (`chan` receive); call from a goroutine | not exposed; wrapper passes `nil` |
| Ruby | blocking bridge (`Queue#pop`); call from a Thread | not exposed; wrapper passes `NULL` |

A wrapper that does not expose the token still launches and completes the
call correctly; the operation simply runs to completion even if the consumer
abandons the future. Drop to the C surface when you need cooperative
cancellation from one of those targets.

### WebAssembly

`wasm32-unknown-unknown` has no threads, so the async story is different on
both sides of the boundary:

- **Producer.** The default spawner drives the future inline with `block_on`
  before the launcher returns, and `set_spawner` has nothing better to offer:
  there is no runtime to hand the future to. Futures that resolve after a
  bounded amount of computation work; a future that returns `Pending`
  waiting for an external wake (a timer, I/O, another thread) parks a thread
  that doesn't exist and hangs the module. Keep async exports on wasm to work
  that completes synchronously, or gate the awaiting code with
  `#[cfg(not(target_arch = "wasm32"))]` as `samples/kvstore` does for its
  clock.
- **Consumer.** The generated JavaScript glue installs one long-lived
  trampoline per async result shape in the module's function table
  (`__indirect_function_table`), registers a per-call `{ resolve, reject }`
  context by integer id, and passes that id as `context`. Because the
  producer completes inline, the trampoline runs while the launcher call is
  still on the stack; the wrapper still returns a real `Promise`, so consumer
  code is identical to Node's. Callback interfaces use the same table
  mechanism, which is why a producer that calls back from a spawned thread
  cannot run on this target at all.
- **Emscripten mode** (`emscripten = true` in `[generators.wasm]`) targets a
  pre-initialized Emscripten `Module` that exposes neither
  `WebAssembly.Function` nor a portable growable table, so async functions
  and callback interfaces are **unsupported** there. `weaveffi generate`
  refuses an API that uses them unless `allow_unsupported = true`, in which
  case they are emitted as explicit throwing stubs.

### Pin / unpin matrix

Every binding pins the user-supplied `void* context` and the callback closure
for the lifetime of the operation, then releases them exactly once on the
callback path. The matrix below is the contract every generator implements;
each row is asserted by that generator's unit tests.

| Target | Pin (allocate / retain) | Unpin (free / release) on callback | Notes |
|--------|-------------------------|------------------------------------|-------|
| Swift | `Unmanaged.passRetained(ContinuationRef(...))` | `Unmanaged.fromOpaque(ctx).takeRetainedValue()` | The retained `+1` is dropped exactly once when the continuation resumes. |
| .NET | `GCHandle.Alloc(callback, GCHandleType.Normal)` | `GCHandle.FromIntPtr(context).Free()` | The catch path also frees the handle on synchronous failure. |
| Kotlin | JNI `(*env)->NewGlobalRef(env, callback)` | `(*env)->DeleteGlobalRef(env, ctx->callback)` | The JNI shim `malloc`s and `free`s the per-call context exactly once. |
| Node.js | `napi_create_promise(env, &deferred, &promise)` | `napi_resolve_deferred` or `napi_reject_deferred` | The N-API runtime owns the deferred; the per-call context is `malloc`-ed and freed exactly once. |
| Python | `_token = _async_register(_cb)` stores the `ctypes.CFUNCTYPE` trampoline in the module-level `_async_pending` dict | `_async_pending.pop(_token, None)` when the callback fires | The callback settles the `asyncio` future via `loop.call_soon_threadsafe`; no thread blocks waiting. |
| C++ | `new std::promise<T>()` plus the lambda capture | `delete p;` once at the end of the lambda | The lambda owns the heap promise on every exit branch. |
| Dart | `NativeCallable<...>.listener(...)` | `callable.close()` in `finally` and on the catch path | Pointer-typed parameters are kept alive in `whenComplete`. |
| Wasm | one table trampoline per signature plus `_asyncContexts.set(ctxId, ...)` per call | `_asyncContexts.delete(ctxId)` in the trampoline | Per-call resolver closures are removed after resolve/reject. |
| Go | `wvCallbackStore(ch)` registers the channel in a global registry keyed by an integer id | `wvCallbackTake(id)` removes it when the exported trampoline fires | The context crossing C is an integer id, never a Go pointer (cgo rule); the channel is buffered so the producer thread never blocks. |
| Ruby | the `FFI::Function` trampoline is a local kept alive by the enclosing method scope | the blocking `queue.pop` returns only after the callback ran | The wrapper blocks the calling Ruby thread, so the trampoline cannot be collected while the producer can still call it. |

### Audit invariants

For every async-capable target:

1. The `void* context` has exactly one owner at any moment.
2. The callback closure is pinned by an explicit "+1" allocation
   (`GCHandle.Alloc`, `Unmanaged.passRetained`, `NewGlobalRef`,
   `NativeCallable.listener`, and so on) before the C worker can see it, and
   released by the matching "-1" exactly once on the callback path.
3. Synchronous failure of the C call (the callback never fires) is handled in
   a `catch` / `try` that frees the pin so it does not leak.
4. The async-demo sample exports `weaveffi_tasks_active_callbacks()` so a
   harness can assert the count returns to zero after a burst of concurrent
   calls.

## Pitfalls

- **Awaiting Tokio without installing Tokio**: the default spawner has no
  reactor, so `tokio::time::sleep` or a `TcpStream` read under it never
  wakes and the callback never fires. Call `weaveffi::set_spawner` with a
  runtime handle before the first async launch.
- **Installing the spawner twice**: `set_spawner` is first-one-wins and
  returns `Err(SpawnerAlreadySet)` afterwards; a second runtime is silently
  not used. Install once, from a single well-defined initialization point.
- **Blocking inside `Spawner::spawn`**: the launcher calls it on the
  consumer's thread. Hand the future off and return.
- **Forgetting `#[weaveffi::cancellable]`**: a `CancelToken` parameter alone
  does not make a function cancellable; without the attribute the macro
  reports a missing-argument error at the call site.
- **Async void functions**: the validator emits a warning. They are valid but
  almost always indicate a missing return type.
- **Forgetting `cancellable: true`**: without it, the launcher has no
  cancel-token slot and the operation cannot be cancelled at all.
- **Expecting a reserved "cancelled" code**: there is none. Return one of
  your own domain codes when the token is set, or complete with a partial
  result.
- **Using async for CPU-bound work**: the callback overhead exceeds the work
  being done; keep it synchronous.
- **Calling Go/Ruby async functions on a latency-sensitive thread**: both
  wrappers block the calling thread until the producer completes. Wrap the
  call in a goroutine / Ruby `Thread` when you need concurrency; the native
  work already runs off-thread.
- **Letting the callback closure get garbage-collected**: every generator
  pins it explicitly. Do not strip those pins when editing generated code by
  hand.
- **Returning `null` instead of invoking the callback**: the contract is that
  the callback fires **exactly once** for every async call, including on
  cancellation and on panic.
- **Forgetting to free an owned result**: strings, bytes, and serialized
  value buffers passed to the callback belong to the consumer. Copy or decode
  them, then release them with `weaveffi_free_string` or
  `weaveffi_free_bytes`; a callback that only copies leaks the producer's
  allocation. The generated wrappers do this for you; the rule matters when
  you consume the raw C surface.
- **Freeing the boxed error with the wrong symbol**: the error passed to an
  async callback is heap-boxed, so it is released with `weaveffi_error_free`
  (which frees the message, the payload, and the box). Calling only
  `weaveffi_error_clear` on it leaks the box; calling `free()` on it leaks
  the message and payload.
- **Awaiting an external wake on wasm**: the inline executor has no way to
  resume the future, so the module hangs. Keep wasm async exports to work
  that completes without waiting.
