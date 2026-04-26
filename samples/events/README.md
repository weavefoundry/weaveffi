# Events sample

A WeaveFFI sample that exercises **callbacks**, **event listeners**, and
**iterator return types** — the building blocks for event-driven APIs
(observers, streams, change feeds) in every target language.

## What this sample demonstrates

- A named **callback type** (`OnMessage`) declared in the IDL and lowered
  to a `typedef` / function-pointer in every target.
- A **listener** (`message_listener`) bound to that callback, which the
  generators turn into a register / unregister pair on the C ABI
  (`weaveffi_events_message_listener_register` and `..._unregister`).
- **Event-driven flow** — calling `send_message(text)` triggers the
  registered `OnMessage` listener with the message.
- An **iterator return type** — `get_messages` returns `iter<string>`,
  which lowers to an opaque `MessageIterator` handle on the C ABI with
  `_next` / `_destroy` lifecycle methods.
- A generator-level demo of how iterator consumers are rendered in each
  language (Swift `IteratorProtocol`, Python `__iter__`, Node `Iterable`,
  etc.).
- **String ownership across the boundary** — each `_next` call hands out a
  freshly allocated C string that the caller frees through
  `weaveffi_free_string`.

## IDL highlights

From [`events.yml`](events.yml):

```yaml
modules:
  - name: events
    callbacks:
      - name: OnMessage
        params:
          - { name: message, type: string }

    listeners:
      - name: message_listener
        event_callback: OnMessage

    functions:
      - name: send_message
        doc: Send a message, triggering the OnMessage callback
        params:
          - { name: text, type: string }

      - name: get_messages
        doc: Return an iterator over all sent messages
        params: []
        return: "iter<string>"
```

Key IDL features exercised:

- `callbacks:` — declaring a named callback type with typed params.
- `listeners:` — binding a listener name to that callback; generators emit
  `_register` and `_unregister` entry points automatically.
- `return: "iter<string>"` — the iterator type, rendered as a
  `MessageIterator` opaque handle on the C ABI.
- `doc:` on each function — doc strings are forwarded into the generated
  output as the target's native doc-comment style.

## Generate bindings

Run the following from the repo root. Omit `--target` to generate bindings
for **all** supported targets.

```bash
# All targets
cargo run -p weaveffi-cli -- generate samples/events/events.yml -o generated

# A single target
cargo run -p weaveffi-cli -- generate samples/events/events.yml -o generated --target c

# A comma-separated subset
cargo run -p weaveffi-cli -- generate samples/events/events.yml -o generated --target c,swift,python
```

Supported `--target` values: `c`, `cpp`, `swift`, `android`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`.

## What to look for in the generated output

- **`generated/c/weaveffi.h`** — the callback `typedef` and the listener
  register / unregister pair:
  ```c
  typedef void (*weaveffi_events_OnMessage)(const char* message);

  void weaveffi_events_message_listener_register(
      weaveffi_events_OnMessage callback, weaveffi_error* err);
  void weaveffi_events_message_listener_unregister(weaveffi_error* err);
  ```
  Iterators show up as an opaque typedef plus `_next` / `_destroy`
  lifecycle:
  ```c
  typedef struct weaveffi_events_MessageIterator
      weaveffi_events_MessageIterator;

  weaveffi_events_MessageIterator* weaveffi_events_get_messages(
      weaveffi_error* err);
  const char* weaveffi_events_MessageIterator_next(
      weaveffi_events_MessageIterator* iter, weaveffi_error* err);
  void weaveffi_events_MessageIterator_destroy(
      weaveffi_events_MessageIterator* iter);
  ```
  The `send_message` prototype is the plain void-returning form.
- **`generated/swift/Sources/WeaveFFI/WeaveFFI.swift`** — a callback
  typealias, a `registerMessageListener(callback:)` /
  `unregisterMessageListener()` pair, and a `MessageIterator` class that
  conforms to `IteratorProtocol` so callers can `for msg in iterator`.
- **`generated/python/weaveffi/__init__.py`** — a `ctypes.CFUNCTYPE`
  callback alias, `register_message_listener(callback)` /
  `unregister_message_listener()`, and a `MessageIterator` class that
  implements `__iter__` / `__next__`.
- **`generated/node/types.d.ts`** — `export type OnMessage = (message: string) => void;`,
  a `register_message_listener(cb: OnMessage)` declaration, and a
  `MessageIterator` class marked `[Symbol.iterator]`.
- **`generated/android/src/main/kotlin/com/weaveffi/WeaveFFI.kt`** — the
  callback as a `fun interface OnMessage`, listener register / unregister
  helpers on the singleton, and a `MessageIterator` class implementing
  `Iterator<String>`.
- **Listener lifecycle invariant** — across every generator, calls made to
  `send_message` *before* `register_message_listener` (or *after*
  `unregister_message_listener`) do not invoke any callback. The Rust
  crate's `#[cfg(test)]` block verifies this directly on the C ABI.

## Build the cdylib

From the repo root:

```bash
cargo build -p events
cargo test  -p events
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libevents.dylib`
- Linux: `target/debug/libevents.so`
- Windows: `target\debug\events.dll`
