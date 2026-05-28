# 06 — Kitchen Sink Thread (Thread)

A "wait, it does *that* too?" thread. Each post is a single IDL feature
from the kvstore sample, paired with the per-language idiom it produces.
Designed to be screenshotted by the kind of developer who quietly
bookmarks tools they want to come back to.

---

## 1/ Hook

> One YAML file. Every FFI feature you've ever wanted, in 11 languages.
>
> A tour 🧵

**Image:** A header card with the kvstore IDL filename and a list of
the features below.

```
samples/kvstore/kvstore.yml
─────────────────────────────────
  ✓ Typed handles            handle<Store>
  ✓ Optional fields          expires_at: i64?
  ✓ List fields              tags: [string]
  ✓ Map fields               metadata: {string:string}
  ✓ Documented enums         EntryKind
  ✓ Documented error domains KvError
  ✓ Callbacks + listeners    OnEvict
  ✓ Streaming iterators      iter<string>
  ✓ Cancellable async        compact_async
  ✓ Deprecated functions     legacy_put
  ✓ Nested modules           kv.stats
  ✓ Per-target overrides     generators: { swift, dotnet, cpp, ... }
```

---

## 2/ Typed handles

```yaml
- name: open_store
  params: [{ name: path, type: string }]
  return: "handle<Store>"
```

Swift:

```swift
let store = try Kv.kv_open_store("./data")
defer { try? Kv.kv_close_store(store) }
```

> `Store` is a typed handle — an opaque struct that wraps the C
> pointer. The compiler stops you from passing a `Product` where a
> `Store` is expected.

---

## 3/ Optionals, lists, maps — first-class

```yaml
- name: Entry
  fields:
    - { name: expires_at, type: "i64?" }
    - { name: tags,       type: "[string]" }
    - { name: metadata,   type: "{string:string}" }
```

Python:

```python
entry.expires_at        # Optional[int]
entry.tags              # List[str]
entry.metadata          # Dict[str, str]
```

> Real types in the `.pyi`. Mypy sees the real shape.

---

## 4/ Cancellable async

```yaml
- name: compact_async
  async: true
  cancellable: true
  params: [{ name: store, type: "handle<Store>" }]
  return: i64
```

Python:

```python
task = asyncio.create_task(kv_compact_async(store))
# ... later ...
task.cancel()      # plumbs through to weaveffi_cancel(handle)
bytes_reclaimed = await task
```

> Cancellation maps to the language's native primitive — Swift
> `Task.cancel()`, Kotlin `Job.cancel()`, Node `AbortSignal`, Python
> `task.cancel()`. The callback fires exactly once, every time.

---

## 5/ Streaming iterators

```yaml
- name: list_keys
  params:
    - { name: prefix, type: "string?" }
  return: "iter<string>"
```

Python:

```python
for key in kv_list_keys(store, "user:"):
    print(key)
```

> `iter<T>` becomes a native `Iterator[str]` in Python, `Iterable<String>`
> in Dart, `string[]` in TS (eagerly drained today). Lazy iteration where
> the host language supports it.

---

## 6/ Typed error domains

```yaml
errors:
  name: KvError
  codes:
    - { name: KEY_NOT_FOUND, code: 1001 }
    - { name: EXPIRED,       code: 1002 }
    - { name: STORE_FULL,    code: 1003 }
    - { name: IO_ERROR,      code: 1004 }
```

Swift:

```swift
do {
    let entry = try Kv.kv_get(store, "missing")
} catch WeaveFFIError.keyNotFound {
    // typed enum case, exhaustive in a switch
}
```

Kotlin:

```kotlin
try {
    val entry = WeaveFFI.kv_get(store, "missing")
} catch (e: WeaveFFIException.KEY_NOT_FOUND) {
    // sealed-class member; the compiler knows the full set
}
```

> Same IDL domain → Swift cases (`.keyNotFound`), Kotlin sealed-class
> members (`WeaveFFIException.KEY_NOT_FOUND`), C++ subclasses
> (`KEY_NOT_FOUNDError`). Code and message stay stable across the ABI.

---

## 7/ Callbacks + listeners

```yaml
callbacks:
  - { name: OnEvict, params: [{ name: key, type: string }] }
listeners:
  - { name: eviction_listener, event_callback: OnEvict }
```

Generated C ABI:

```c
void weaveffi_kv_eviction_listener_register(
    void (*on_evict)(const char* key),
    weaveffi_error* out_err);
void weaveffi_kv_eviction_listener_unregister(
    weaveffi_error* out_err);
```

> Every language target gets a matching `register` / `unregister` pair
> over this contract. One YAML stanza becomes a typed callback in
> Swift, Kotlin, TS, Python, C#, Dart, Go, Ruby. No raw function
> pointers in the consumer's code path.

---

## 8/ Close

> One IDL. Eleven SDKs. Every FFI feature: handles, optionals, lists,
> maps, async, cancellation, iterators, errors, callbacks, deprecations,
> nested modules.
>
> Generated. Tested. Publishable.
>
> `cargo install weaveffi-cli`
>
> https://weaveffi.com

---

## Alt text (apply per panel)

Each panel: "YAML snippet declaring an FFI feature, followed by a short
code snippet showing how the same feature is consumed idiomatically in
[language]."
