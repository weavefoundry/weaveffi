# SQLite Contacts sample

A real-world WeaveFFI reference that exposes a persistent CRUD API backed by
[SQLite](https://www.sqlite.org/) (via [`rusqlite`](https://crates.io/crates/rusqlite)).
Unlike the minimal samples, this one combines **async + cancellation +
iterators + optionals + structs + enums** in a single non-trivial module,
so it doubles as a canonical example of what a production-shaped binding
looks like across every target language.

## What this sample demonstrates

- **Async I/O on a shared Tokio runtime** — every CRUD entry point is
  declared `async: true` in the IDL and dispatched through
  `tokio::spawn_blocking` so the C callback fires from a worker thread
  without blocking the caller. See [`src/lib.rs`](src/lib.rs)
  (`dispatch_async`).
- **Cooperative cancellation** — `create_contact` is declared
  `cancellable: true`, so the generated C entry point threads a
  `weaveffi_cancel_token*` through to the worker. The worker polls the
  token on a short ladder and returns `ERR_CODE_CANCELLED` if the token
  is flipped.
- **Iterator return type** — `list_contacts` returns `iter<Contact>`,
  which lowers to an opaque `ListContactsIterator` handle with
  `_next` / `_destroy` lifecycle methods on the C ABI and to
  `IteratorProtocol` / `__iter__` shapes in the target languages.
- **Optionals on parameters, returns, and struct fields** —
  `email: string?`, `Status?` filters, `Contact?` return from
  `find_contact`. Every generator renders these as the target's
  native nullable/optional type (Swift `String?`, Python
  `Optional[str]`, etc.).
- **Struct with mixed field kinds** — `Contact` carries `i64`, `string`,
  `string?`, an enum-typed `status`, and a timestamp — exercising every
  getter path the generators emit.
- **Enum** — `Status` is a `#[repr(i32)]` Rust enum that becomes a native
  `enum` in every target.

## Storage model

The sample uses a **shared in-memory SQLite database** opened through the
URI `file:weaveffi_sqlite_contacts?mode=memory&cache=shared`:

- The first call lazily creates the schema via a `OnceLock<Pool>` — no
  disk I/O is ever performed. The DB lives for the lifetime of the
  process.
- A small connection pool (`Mutex<Vec<Connection>>`) hands connections
  out to worker tasks; `PooledConn`'s `Drop` returns them on any early
  exit so `?`-style error paths can't leak.
- Because every connection opens the same shared-cache URI, all workers
  observe the same rows.

This keeps the sample self-contained (no filesystem permissions, no
cleanup) while still running real SQL through real transactions — so it
behaves like a "real" database from the binding's perspective.

## IDL highlights

From [`sqlite_contacts.yml`](sqlite_contacts.yml):

```yaml
modules:
  - name: contacts
    enums:
      - name: Status
        variants:
          - { name: Active,   value: 0 }
          - { name: Archived, value: 1 }
    structs:
      - name: Contact
        fields:
          - { name: id,         type: i64 }
          - { name: name,       type: string }
          - { name: email,      type: "string?" }
          - { name: status,     type: Status }
          - { name: created_at, type: i64 }
    functions:
      - name: create_contact
        params:
          - { name: name,  type: string }
          - { name: email, type: "string?" }
        return: Contact
        async: true
        cancellable: true            # ← async + cancel token
      - name: find_contact
        params: [{ name: id, type: i64 }]
        return: "Contact?"           # ← optional return
        async: true
      - name: list_contacts
        params: [{ name: status, type: "Status?" }]
        return: "iter<Contact>"      # ← iterator
      - name: update_contact
        params:
          - { name: id,    type: i64 }
          - { name: email, type: "string?" }
        return: bool
        async: true
      - name: delete_contact
        params: [{ name: id, type: i64 }]
        return: bool
        async: true
      - name: count_contacts
        params: [{ name: status, type: "Status?" }]
        return: i64
        async: true
```

Key IDL features exercised: `async: true`, `cancellable: true`,
`iter<T>`, `T?`, enum-typed struct fields, and optional-typed
parameters.

## Generate Python and Swift bindings

From the repo root:

```bash
# Python + Swift in one run
cargo run -p weaveffi-cli -- generate \
    samples/sqlite-contacts/sqlite_contacts.yml \
    -o generated --target python,swift

# Or omit --target to emit every supported target at once
cargo run -p weaveffi-cli -- generate \
    samples/sqlite-contacts/sqlite_contacts.yml -o generated
```

Supported `--target` values: `c`, `cpp`, `swift`, `android`, `node`,
`wasm`, `python`, `dotnet`, `dart`, `go`, `ruby`.

## Python consumer code

The Python generator emits a `weaveffi` package (`generated/python/weaveffi/`)
that loads the cdylib through `ctypes`. Consumers get an `IntEnum`, a
handle-backed class with properties, `async def` wrappers, and a
plain iterator:

```python
import asyncio
from weaveffi import (
    Status,
    Contact,
    contacts_create_contact,
    contacts_find_contact,
    contacts_list_contacts,
    contacts_count_contacts,
)

async def main() -> None:
    alice: Contact = await contacts_create_contact("Alice", "alice@example.com")
    print(alice.id, alice.name, alice.email, alice.status)   # Status.Active

    maybe: Contact | None = await contacts_find_contact(alice.id)
    assert maybe is not None

    for c in contacts_list_contacts(Status.Active):          # iterator
        print(c.id, c.name)

    total = await contacts_count_contacts(None)              # optional filter
    print("rows:", total)

    # Cooperative cancellation: cancelling the awaiting task flips the
    # C cancel token; the worker returns CANCELLED and Python raises.
    task = asyncio.create_task(contacts_create_contact("slow", None))
    task.cancel()

asyncio.run(main())
```

Under the hood each `async def` dispatches the blocking C call to a
thread-pool executor and threads a `weaveffi_cancel_token` through so
`asyncio.CancelledError` propagates to the Rust worker.

## Swift consumer code

The Swift generator emits a SwiftPM package (`generated/swift/`) with a
C module map that links against the cdylib. Async functions become
`async throws` using `CheckedContinuation`, iterators are materialised
into `[Contact]`, and cancellation is wired through
`withTaskCancellationHandler`:

```swift
import WeaveFFI

func demo() async throws {
    let alice = try await Contacts.contacts_create_contact("Alice", "alice@example.com")
    print(alice.id, alice.name, alice.email ?? "-", alice.status)  // .active

    if let found = try await Contacts.contacts_find_contact(alice.id) {
        print("found:", found.name)
    }

    let active = try Contacts.contacts_list_contacts(.active)      // [Contact]
    for c in active { print(c.id, c.name) }

    let total = try await Contacts.contacts_count_contacts(nil)
    print("rows:", total)

    // Cancelling the task flips the cancel token inside the Rust worker.
    let task = Task { try await Contacts.contacts_create_contact("slow", nil) }
    task.cancel()
    _ = try? await task.value
}
```

The generated `public class Contact` owns an `OpaquePointer` and calls
`weaveffi_contacts_Contact_destroy` in `deinit`, so Contacts clean up
automatically when they go out of scope.

## Build the cdylib and run the tests

From the repo root:

```bash
cargo build -p sqlite-contacts
cargo test  -p sqlite-contacts
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libsqlite_contacts.dylib`
- Linux: `target/debug/libsqlite_contacts.so`
- Windows: `target\debug\sqlite_contacts.dll`

The `#[cfg(test)]` block covers the full CRUD round-trip, the iterator,
and a cancellation scenario that flips the token mid-call and asserts
the callback receives `ERR_CODE_CANCELLED`.
