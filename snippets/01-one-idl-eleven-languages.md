# 01 — One IDL, Eleven Languages (Flagship Thread)

The headline thread. Lead with this any time WeaveFFI is the topic. The
contrast — *one* tiny YAML file, *eleven* native SDKs — is the entire
pitch in a single scroll.

The thread is 7 posts. Each post below has three blocks:

- **Tweet text** — the literal words to type into the X composer.
- **Attached image** — what to put in the screenshot (and how to render it).
- **Alt text** — paste this into the image's "Description" field.

Queue every reply *before* you publish the first post. X autopublishes
the chain. Don't paste anything that isn't marked "Tweet text."

---

## Post 1 / 7 — Hook

### Tweet text

```
I wrote 30 lines of YAML.

I got 11 production SDKs: C, C++, Swift, Kotlin, Node, WASM, Python, C#, Dart, Go, and Ruby.

All idiomatic. All publishable. One C ABI underneath.

Meet WeaveFFI. 🧵
```

### Attached image

Render the YAML below as a code screenshot. Use
[carbon.now.sh](https://carbon.now.sh) with the "VSCode Dark+" theme,
no window chrome, 32 px padding, zoom so the file fills the frame.

```yaml
version: "0.3.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work,     value: 1 }
    structs:
      - name: Contact
        fields:
          - { name: id,    type: i64 }
          - { name: name,  type: string }
          - { name: email, type: "string?" }
          - { name: kind,  type: ContactType }
    functions:
      - name: create_contact
        params:
          - { name: name,  type: string }
          - { name: email, type: "string?" }
          - { name: kind,  type: ContactType }
        return: Contact
      - name: list_contacts
        params: []
        return: "[Contact]"
      - name: delete_contact
        params:
          - { name: id, type: i64 }
        return: bool
```

### Alt text

```
A 30-line YAML file titled contacts.yml. It declares a module called contacts with a ContactType enum (Personal, Work), a Contact struct with fields id, name, optional email, and kind: ContactType, and three functions: create_contact, list_contacts, and delete_contact.
```

---

## Post 2 / 7 — The one command

### Tweet text

```
One command. Eleven targets.

Each target ships its own package manifest: SwiftPM, Gradle, package.json, pyproject, csproj, pubspec, go.mod, gemspec, or CMakeLists.

Standalone packages. Your users never install WeaveFFI.
```

### Attached image

Render the terminal output below as a screenshot. Run it for real first
(`weaveffi generate samples/contacts/contacts.yml -o sdk --dry-run`) so
your screenshot matches what readers will see if they try it.

```bash
$ weaveffi generate contacts.yml -o sdk --dry-run
sdk/c/weaveffi.h
sdk/cpp/weaveffi.hpp
sdk/cpp/CMakeLists.txt
sdk/swift/Sources/WeaveFFI/WeaveFFI.swift
sdk/swift/Package.swift
sdk/android/src/main/kotlin/com/weaveffi/WeaveFFI.kt
sdk/android/src/main/cpp/weaveffi_jni.c
sdk/android/build.gradle
sdk/node/index.js
sdk/node/types.d.ts
sdk/node/package.json
sdk/wasm/weaveffi_wasm.js
sdk/wasm/weaveffi_wasm.d.ts
sdk/python/weaveffi/__init__.py
sdk/python/weaveffi/weaveffi.pyi
sdk/python/pyproject.toml
sdk/dotnet/WeaveFFI.cs
sdk/dotnet/WeaveFFI.csproj
sdk/dart/lib/weaveffi.dart
sdk/dart/pubspec.yaml
sdk/go/weaveffi.go
sdk/go/go.mod
sdk/ruby/lib/weaveffi.rb
sdk/ruby/weaveffi.gemspec
```

### Alt text

```
A terminal showing the command "weaveffi generate contacts.yml -o sdk --dry-run". The output lists the files that would be written across eleven language targets — C, C++, Swift, Kotlin/Android with a JNI shim, Node.js, WASM, Python, .NET, Dart, Go, and Ruby — including each target's package manifest.
```

---

## Post 3 / 7 — Swift

### Tweet text

```
Swift (SwiftPM):
```

### Attached image

```swift
let alice = try Contacts.contacts_create_contact(
    "Alice",
    "alice@example.com",
    .personal
)

for c in try Contacts.contacts_list_contacts() {
    print("\(c.name) <\(c.email ?? "no email")>")
}
```

### Alt text

```
A Swift snippet that calls Contacts.contacts_create_contact with name "Alice", email "alice@example.com", and the .personal enum case, then iterates over the result of Contacts.contacts_list_contacts to print each contact's name and email. Optional emails fall back to "no email".
```

---

## Post 4 / 7 — Node

### Tweet text

```
Node (npm):
```

### Attached image

```typescript
const alice = contacts_create_contact(
  "Alice",
  "alice@example.com",
  ContactType.Personal
);

for (const c of contacts_list_contacts()) {
  console.log(`${c.name} <${c.email ?? "no email"}>`);
}
```

### Alt text

```
A Node.js snippet that calls contacts_create_contact with name "Alice", email "alice@example.com", and ContactType.Personal, then iterates the contacts list to log each name and email. The npm package ships TypeScript declarations alongside the N-API addon, so the email field's type is string or null.
```

---

## Post 5 / 7 — Python

### Tweet text

```
Python (PyPI):
```

### Attached image

```python
alice = contacts_create_contact(
    "Alice",
    "alice@example.com",
    ContactType.Personal,
)

for c in contacts_list_contacts():
    print(f"{c.name} <{c.email or 'no email'}>")
```

### Alt text

```
A Python snippet that calls contacts_create_contact with name "Alice", email "alice@example.com", and ContactType.Personal, then iterates the list to print each name and email. The .pyi stubs type email as Optional[str].
```

---

## Post 6 / 7 — Dart

### Tweet text

```
Dart (pub):
```

### Attached image

```dart
final alice = createContact(
  'Alice',
  'alice@example.com',
  ContactType.personal,
);

for (final c in listContacts()) {
  print('${c.name} <${c.email ?? "no email"}>');
}
```

### Alt text

```
A Dart snippet calling top-level createContact and listContacts functions. The Dart generator camelCases method names automatically, so createContact maps to the same C symbol that the Swift, TypeScript, and Python snippets call. Optional emails fall back to "no email".
```

---

## Post 7 / 7 — Close

### Tweet text

```
One YAML file. Eleven languages. Stable C ABI underneath.

No hand-written JNI. No duplicate implementations. No "well, it's kind of working in Python."

cargo install weaveffi-cli

https://weaveffi.com
```

### Attached image

None. Let the link unfurl be the visual.

### Alt text

N/A — no image.

---

## Quick-paste cheat sheet

If you just want the seven literal posts to drop into X one after the
other:

1. *I wrote 30 lines of YAML. I got 11 production SDKs…* + IDL image
2. *One command. Eleven targets…* + terminal image
3. *Swift (SwiftPM):* + Swift image
4. *Node (npm):* + Node image
5. *Python (PyPI):* + Python image
6. *Dart (pub):* + Dart image
7. *One YAML file. Eleven languages…* + link
