# Async Demo sample

A WeaveFFI sample that exercises the **async function** pattern. Async
functions declared in the IDL get a C ABI entry point with an `_async`
suffix that accepts a callback + context pointer instead of returning
directly; every language generator lifts that callback into the target's
idiomatic async primitive (Swift `async throws`, Kotlin `suspend`, Python
`async def`, Node `Promise`, .NET `Task`, Ruby `Concurrent::Promise`, Dart
`Future`, etc.).

## What this sample demonstrates

- **Async function declarations** in the IDL via `async: true`.
- **Callback-based C ABI lowering** — the generated C signatures take a
  `callback` function pointer and a `void* context`; completion is signalled
  by invoking the callback from a worker thread.
- **Async with struct returns** — `run_task` asynchronously returns a
  single `TaskResult`.
- **Async with list-of-struct returns** — `run_batch` asynchronously
  returns `[TaskResult]`.
- **Mixed sync/async in the same module** — `cancel_task` is declared
  without `async: true`, so it keeps the plain blocking C signature
  alongside the async ones.
- The stable **`_async` C ABI naming convention** (`weaveffi_{module}_{fn}_async`)
  that every high-level binding wraps behind an idiomatic async surface.

## IDL highlights

From [`async_demo.yml`](async_demo.yml):

```yaml
modules:
  - name: tasks
    structs:
      - name: TaskResult
        fields:
          - { name: id,      type: i64 }
          - { name: value,   type: string }
          - { name: success, type: bool }
    functions:
      - name: run_task
        params:
          - { name: name, type: string }
        return: TaskResult
        async: true              # ← lifts to an _async C entry point

      - name: run_batch
        params:
          - { name: names, type: "[string]" }
        return: "[TaskResult]"
        async: true              # ← async + list-of-struct return

      - name: cancel_task
        params:
          - { name: id, type: i64 }
        return: bool             # ← plain sync function, coexists with the async ones
```

Key IDL features exercised:

- `async: true` on a function that returns a **struct**.
- `async: true` on a function that returns a **list of structs**.
- A sync sibling (`cancel_task`) inside the same async-heavy module.

## Generate bindings

Run the following from the repo root. Omit `--target` to generate bindings
for **all** supported targets.

```bash
# All targets
cargo run -p weaveffi-cli -- generate samples/async-demo/async_demo.yml -o generated

# A single target
cargo run -p weaveffi-cli -- generate samples/async-demo/async_demo.yml -o generated --target swift

# A comma-separated subset
cargo run -p weaveffi-cli -- generate samples/async-demo/async_demo.yml -o generated --target swift,python,node
```

Supported `--target` values: `c`, `cpp`, `swift`, `android`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`.

## What to look for in the generated output

- **`generated/c/weaveffi.h`** — the async signatures end in `_async` and
  take a callback + `void*` context, for example:
  ```c
  typedef void (*weaveffi_tasks_run_task_callback)(
      void* context, weaveffi_error* err, weaveffi_tasks_TaskResult* result);

  void weaveffi_tasks_run_task_async(
      const uint8_t* name_ptr, size_t name_len,
      weaveffi_tasks_run_task_callback callback, void* context);
  ```
  The sync sibling keeps the normal shape:
  `bool weaveffi_tasks_cancel_task(int64_t id, weaveffi_error* err);`.
- **`generated/swift/Sources/WeaveFFI/WeaveFFI.swift`** — async functions
  are exposed as `async throws` and backed by `CheckedContinuation`:
  `public static func runTask(name: String) async throws -> TaskResult`
  and `public static func runBatch(names: [String]) async throws -> [TaskResult]`.
- **`generated/python/weaveffi/__init__.py`** — each async function is an
  `async def` that awaits on an `asyncio.Event` set from the callback
  thread: `async def run_task(name: str) -> TaskResult`.
- **`generated/node/weaveffi.js` + `types.d.ts`** — async functions return
  `Promise`s, typed as
  `export function tasks_run_task(name: string): Promise<TaskResult>;`.
- **`generated/dotnet/WeaveFFI.cs`** — async functions return
  `Task<TaskResult>` / `Task<TaskResult[]>`, with the callback marshalled
  through a `TaskCompletionSource<T>`.
- **`generated/ruby/lib/weaveffi.rb`** — both a block-style
  `run_task_async(name) { |result, err| ... }` helper and a
  `run_task(name)` wrapper that returns a `Concurrent::Promise`.
- **`generated/go/weaveffi.go`** — each async function forwards to the
  matching `C.weaveffi_tasks_run_task_async(...)` entry point and uses a
  Go channel to bridge the callback back to the caller.
- **`generated/dart/lib/weaveffi.dart`** — async functions return
  `Future<TaskResult>` via a `ReceivePort` / `Completer`.
- **`cancel_task` stays synchronous** in every target, proving that the
  async lowering is per-function and does not accidentally promote sync
  functions.

## Build the cdylib

From the repo root:

```bash
cargo build -p async-demo
cargo test  -p async-demo
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libasync_demo.dylib`
- Linux: `target/debug/libasync_demo.so`
- Windows: `target\debug\async_demo.dll`
