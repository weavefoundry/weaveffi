# 08 — Same Call, Five Native Shapes (Thread)

The "look how *native* this is" thread. Each post takes the same data —
a `Contact` with an optional email and an enum — and shows it in one
language's idiomatic shape. Function names mirror the C ABI for
traceability across logs and grep; *types* and *error handling* are
fully idiomatic.

---

## 1/ Hook

> "But surely the generated code is ugly?"
>
> Nope. One IDL → idiomatic *types* in every language. Same name in
> every language (great for logs and grep).
>
> Same struct. Same call. Five language flavours 🧵

The IDL banner:

```yaml
- name: Contact
  fields:
    - { name: id,    type: i64 }
    - { name: name,  type: string }
    - { name: email, type: "string?" }
    - { name: kind,  type: ContactType }
```

---

## 2/ Swift — `String?`, `enum`, `throws`, namespaced module

```swift
let alice = try Contacts.contacts_create_contact(
    "Alice", "alice@example.com", .personal
)
print("\(alice.name) <\(alice.email ?? "—")>")
```

> Module is a Swift `enum` namespace. `email` is `String?`. `throws`.
> `.personal` is the auto-shortened enum case. SwiftPM package.

---

## 3/ TypeScript — `string | null`, `enum`, no wrappers

```typescript
import { contacts_create_contact, ContactType } from "@you/contacts";

const alice = contacts_create_contact("Alice", "alice@example.com", ContactType.Personal);
console.log(`${alice.name} <${alice.email ?? "—"}>`);
```

> `email: string | null` in `types.d.ts`. `ContactType` is a real TS
> enum. N-API addon underneath.

---

## 4/ Python — typed, `Optional[str]`, `IntEnum`

```python
from weaveffi import contacts_create_contact, ContactType

alice = contacts_create_contact("Alice", "alice@example.com", ContactType.Personal)
print(f"{alice.name} <{alice.email or '—'}>")
```

> `.pyi` stubs included. Mypy sees `Optional[str]` for `email`.
> `ContactType` is a real `IntEnum`.

---

## 5/ Dart — camelCased, `String?`, `enum`, `dart:ffi`

```dart
final alice = createContact('Alice', 'alice@example.com', ContactType.personal);
print('${alice.name} <${alice.email ?? "—"}>');
```

> Dart auto-camelCases (`createContact`). Same C symbol underneath as
> every other panel. Drop into Flutter via `flutter pub add`.

---

## 6/ Kotlin/Android — `String?`, real enum, JNI underneath

```kotlin
val alice = WeaveFFI.contacts_create_contact("Alice", "alice@example.com", ContactType.Personal)
println("${alice.name} <${alice.email ?: "—"}>")
```

> Generated JNI shim + Gradle skeleton. `email` is a real `String?`.
> `ContactType` lives in the same package.

---

## 7/ Close

> Same IDL. Five native shapes.
>
> Types are idiomatic. Errors are idiomatic. Names are stable across
> every target — `contacts_create_contact` in your Python tests,
> `contacts_create_contact` in your Swift logs, `createContact` in
> Dart (auto-translated to the same C symbol).
>
> https://weaveffi.com

---

## Why this works

- **Visual repetition.** The eye locks onto "this is the same program"
  immediately, so the *differences* (`String?` vs `string | null` vs
  `Optional[str]`) become the story.
- **Pre-empts the fear.** "Generated code is ugly" is the #1 silent
  objection. This thread settles it.
- **Reusable across launches.** Swap in your own struct any time.

---

## Alt text (apply per panel)

"A four-to-six line code snippet in [language] that creates a Contact
with name 'Alice', email 'alice@example.com', and type Personal, then
prints the name and email. Each panel uses the language's native
optional/null syntax (`String?` in Swift/Dart/Kotlin, `string | null`
in TypeScript, `Optional[str]` in Python)."
