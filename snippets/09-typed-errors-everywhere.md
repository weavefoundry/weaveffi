# 09 — Typed Errors Everywhere (Single Post)

The error-handling angle. Most generators give you "an exception" — this
one gives you a typed, named, catchable code in every target.

---

## Hook

> Declare an error domain once:

```yaml
errors:
  name: KvError
  codes:
    - { name: KEY_NOT_FOUND, code: 1001, message: "Key not found" }
    - { name: EXPIRED,       code: 1002, message: "Entry expired"   }
    - { name: STORE_FULL,    code: 1003, message: "Store is full"   }
    - { name: IO_ERROR,      code: 1004, message: "I/O error"       }
```

> Catch the *specific* one in every language:

---

## Image (single, four panels)

**Swift:**

```swift
do {
    let entry = try Kv.kv_get(store, "missing")
} catch WeaveFFIError.keyNotFound {
    // typed enum case; exhaustive in a switch
}
```

**Kotlin:**

```kotlin
try {
    val entry = WeaveFFI.kv_get(store, "missing")
} catch (e: WeaveFFIException.KEY_NOT_FOUND) {
    // sealed-class member; the compiler knows the full set
}
```

**C++:**

```cpp
try {
    auto entry = kv::kv_get(store, "missing");
} catch (const kvstore::KEY_NOT_FOUNDError& e) {
    // typed subclass of WeaveFFIError
}
```

**Python:**

```python
try:
    entry = kv_get(store, "missing")
except WeaveffiError as e:
    if e.code == 1001:   # KEY_NOT_FOUND
        ...
```

---

## Body

> One IDL `errors:` block becomes:
>
>   • Swift `enum WeaveFFIError: Error` with cases (`.keyNotFound`)
>   • Kotlin `sealed class WeaveFFIException` with members
>     (`WeaveFFIException.KEY_NOT_FOUND`)
>   • C++ typed subclasses (`KEY_NOT_FOUNDError`) extending
>     `WeaveFFIError`
>   • Python `WeaveffiError(Exception)` carrying `.code` + `.message`
>
> Every code is named. Every name is stable. Every message survives the
> ABI boundary.

---

## Why this works

- **Settles a real fear.** Devs assume FFI = `int errno`. The post
  proves otherwise in 4 panels.
- **Shows real interop.** Same name (`KEY_NOT_FOUND`) across panels =
  same incident, traceable across services.
- **Compact.** Four short snippets that fit on one mobile screen.

---

## Alt text

"A YAML block declaring an error domain `KvError` with four named codes
(KEY_NOT_FOUND, EXPIRED, STORE_FULL, IO_ERROR), followed by four code
panels (Swift, Kotlin, C++, Python) all catching a KEY_NOT_FOUND error
using their language's idiomatic exception or error-handling syntax —
Swift's `catch WeaveFFIError.keyNotFound`, Kotlin's
`catch (e: WeaveFFIException.KEY_NOT_FOUND)`, C++'s typed
`KEY_NOT_FOUNDError`, and Python's `if e.code == 1001`."
