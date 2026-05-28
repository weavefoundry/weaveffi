# 02 — One Line, Async Everywhere (Single Post)

The single-best "magic moment" post. The whole pitch fits on one phone
screen: one IDL flag becomes correct async semantics in eight languages.

---

## Hook

> Add `async: true` to one line of YAML.
>
> Get native async in 8 languages:
>
> Swift `async throws` · Kotlin `suspend` · TS `Promise<T>` · Python
> `async def` · C# `Task<T>` · Dart `Future<T>` · C++ `std::future<T>` ·
> WASM `Promise<T>`.
>
> All cancellable. One shared C ABI.

---

## Image (single screenshot, 8 panels)

Lay out as **2 columns × 4 rows** of code, with the IDL banner across
the top. Use one dark theme (e.g. "Night Owl") for every panel so the
composition reads as one image. Equal panel heights; the IDL banner is
its own row above the grid.

**Top banner — the IDL:**

```yaml
# net.yml
- name: fetch
  async: true
  cancellable: true
  params:
    - { name: url, type: string }
  return: string
```

**Panel 1 — Swift:**

```swift
let body = try await Net.fetch(url: "https://x.com/feed")
```

**Panel 2 — Kotlin:**

```kotlin
val body = Net.fetch("https://x.com/feed")  // suspend fun
```

**Panel 3 — TypeScript:**

```typescript
const body = await fetch("https://x.com/feed");
```

**Panel 4 — Python:**

```python
body = await net.fetch("https://x.com/feed")
```

**Panel 5 — C#:**

```csharp
var body = await Net.FetchAsync("https://x.com/feed");
```

**Panel 6 — Dart:**

```dart
final body = await fetch('https://x.com/feed');
```

**Panel 7 — C++:**

```cpp
auto body = net::fetch("https://x.com/feed").get();
```

**Panel 8 — WASM (JS host):**

```javascript
const body = await fetch("https://x.com/feed");
```

---

## Why this works

- **Concrete delta.** One word in YAML, eight separate "this is how my
  language does it" answers.
- **No mystery.** Every panel is one line — no boilerplate to argue with.
- **Pain-relief tone.** Anyone who has hand-written N-API + JNI + ctypes
  callback shims feels this in their chest.

---

## Alt text

"A YAML snippet declaring a function `fetch` with `async: true,
cancellable: true`, followed by eight side-by-side panels showing the
same fetch call in Swift, Kotlin, TypeScript, Python, C#, Dart, C++,
and WASM (called from JavaScript) — each written in that language's
native async style."

---

## Optional follow-up reply

> Cancellation works the same way: pass the language's native primitive
> (Swift `Task.cancel()`, Kotlin `Job.cancel()`, Node `AbortSignal`,
> Python `task.cancel()`, .NET `CancellationToken`). WeaveFFI plumbs it
> through to `weaveffi_cancel(handle)` under the hood.
