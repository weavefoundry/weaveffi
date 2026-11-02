# IDL Schema

An IDL document describes one library's API: its records, enums, error
domains, interfaces, callback interfaces, and functions. YAML and JSON are
accepted (TOML is only for [`weaveffi.toml`](../guides/config.md), never for
an IDL); this page uses YAML, read as YAML 1.2, so a name YAML reads as null
(`null`, `~`) has to be quoted (`name: "null"`). It documents schema version
`0.13.0`, the only version the current tools accept. A Rust producer never
writes one by hand (the attributes embed the API in the built library, and
[`weaveffi extract`](../guides/extract.md) prints it as an IDL), but the
schema is the same either way.

The YAML on this page is included from the documents in
[`examples/idl`](https://github.com/weavefoundry/weaveffi/tree/main/examples/idl),
which a test loads and validates the way `weaveffi validate` does. A block
whose first line is `# rejected:` is a document the validator refuses, with
the error it reports; the test checks that too.

## Editor support

The CLI prints a JSON Schema for the document with `weaveffi schema`, and a
copy is checked in as
[`weaveffi.schema.json`](https://github.com/weavefoundry/weaveffi/blob/main/weaveffi.schema.json)
and published at `https://weaveffi.com/weaveffi.schema.json`. Editors using
the YAML Language Server pick it up from a header comment:

```yaml
# yaml-language-server: $schema=https://weaveffi.com/weaveffi.schema.json
```

## Document structure

A library is **one namespace**, so a document is flat: every declaration
sits at the top level, in a list by kind.

```yaml
{{#include ../../../examples/idl/contacts.yml:header}}
```

| Key | Type | Meaning |
|-----|------|---------|
| `version` | string (required) | Must be `"0.13.0"` |
| `doc` | string | Documentation for the library as a whole |
| `imports` | array of paths | Other IDL files whose declarations join this library (see [Multi-file IDLs](#multi-file-idls)) |
| `records` | array of Record | Plain data types with named fields |
| `enums` | array of Enum | C-style or rich enums |
| `errors` | array of ErrorDomain | Error domains |
| `interfaces` | array of Interface | Reference-counted object types |
| `callbacks` | array of CallbackInterface | Method sets the consumer implements |
| `functions` | array of Function | Free functions |

The document describes only the API. Package identity and generator options
live in [`weaveffi.toml`](../guides/config.md). Every object rejects unknown
keys, so a misspelled field fails to parse instead of being ignored.

### Function

The same shape describes free functions, interface constructors, methods,
and statics, and callback-interface methods, with the restrictions noted in
their sections.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Function name |
| `params` | array of Param | Ordered parameters (default none) |
| `return` | TypeRef | Return type; omit for none |
| `doc` | string | Documentation |
| `throws` | string | The error domain the callable reports (`BookError`), or `any` for an untyped error; omit when it can't fail (see [Error domains](#error-domains)) |
| `async` | bool | Complete asynchronously (default `false`) |
| `cancellable` | bool | Accept a cancel token; requires `async: true` |
| `deprecated` | string | Deprecation message propagated to every binding |

A Param has a required `name` and `type` and an optional `doc`. A parameter
may not be named `self`, `ctx`, `out_err`, `callback`, `context`, or
`cancel_token`, the slots the C ABI adds (`SlotCollision`).

## Types

A `TypeRef` is a string:

| Syntax | Meaning |
|--------|---------|
| `i8` `i16` `i32` `i64` `u8` `u16` `u32` `u64` | Fixed-width integers |
| `f32` `f64` | IEEE 754 floats |
| `bool` | Boolean |
| `string` | UTF-8 text |
| `bytes` | Byte string (`[u8]` is accepted and becomes `bytes`) |
| `Name` | A record, enum, interface, or callback interface, by name |
| `T?` | Optional |
| `[T]` | List |
| `{K:V}` | Map |
| `iter<T>` | Lazy iterator (returns only) |

`usize`, `isize`, `u128`, `i128`, and `char` aren't primitives; a reference
to one is reported as `UnsupportedPrimitive` (use `u64` or `i64` for sizes,
and `string` for characters). A reference to a name nothing declares is
`UnknownTypeRef`.

The parser reads outside in: `[Contact?]` is a list of optional contacts and
`[Contact]?` an optional list. Composites nest to any depth. Quote anything
YAML would otherwise interpret (`"string?"`, `"[i32]"`, `"{string:i32}"`).

How a type is spelled never depends on how it crosses the ABI. At a call
boundary every value is [one C value](abi.md#one-value-one-c-value): a
scalar or C-style enum by value, an optional one (`i32?`, `Kind?`) as a
`{p}_opt_*` struct, an interface as a pointer, and everything else as a
`{p}_run` holding a string's UTF-8, raw bytes, a typed array (a list of any
integer or float but `u8`), or a [value buffer](value-buffers.md). Every
binding surfaces them as its ordinary optional and list types; inside a
record, variant, or error payload every type uses the value-buffer encoding.

**Map keys** must be an integer, `bool`, `string`, or a C-style enum, so
every target can use its native dictionary with exact equality. Floats,
`bytes`, composites, and objects are rejected:

```yaml
{{#include ../../../examples/idl/rejected/InvalidMapKey.yml}}
```

**Iterators** are lazy: every binding pulls one element per step of its
native iteration idiom and destroys the iterator exactly once. `iter<T>` is
valid only as the outermost return of a synchronous callable. It's rejected
in a parameter, a field, or nested in another type
(`IteratorInInvalidPosition`), and as the return of an async function
(`AsyncIteratorReturn`):

```yaml
{{#include ../../../examples/idl/rejected/IteratorInInvalidPosition.yml}}
```

### Type positions

"Buffered field" means a record field, rich-enum variant field, or error
payload field.

| Type | Param | Return | Buffered field | Inside `T?` `[T]` `{K:V}` | Map key | Callback param | Callback return |
|------|-------|--------|----------------|---------------------------|---------|----------------|-----------------|
| integers, floats, `bool` | yes | yes | yes | yes | integers, `bool` | yes | yes |
| `string` | yes | yes | yes | yes | yes | yes | yes |
| `bytes` | yes | yes | yes | yes | no | yes | yes |
| C-style enum | yes | yes | yes | yes | yes | yes | yes |
| record, rich enum | yes | yes | yes | yes | no | yes | yes |
| interface | yes | yes | yes, except in an error payload | yes | no | yes | yes |
| callback interface | yes, bare or `Cb?` | no | no | no | no | no | no |
| `T?`, `[T]`, `{K:V}` | yes | yes | yes | yes | no | yes | yes |
| `iter<T>` | no | yes (sync) | no | no | no | no | no |

A record or rich enum may not contain itself by value, directly or through
other records and variants (`RecursiveType`); a list, map, or optional of it
is fine.

## Records

A record is a value type with named fields, crossing the ABI by value as a
[value buffer](value-buffers.md). Bindings map it to a data class, struct,
or record type; no per-record C functions exist.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Type name |
| `doc` | string | Documentation |
| `deprecated` | string | Deprecation message |
| `fields` | array of Field (required, non-empty) | Each with `name`, `type`, optional `doc` |

```yaml
{{#include ../../../examples/idl/contacts.yml:records}}
```

## Enums

An enum is a fixed set of variants.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Type name |
| `doc`, `deprecated` | string | Documentation, deprecation message |
| `variants` | array of Variant (required, non-empty) | Each with `name`, optional `value`, `doc`, and `fields` |

A variant's `value` is its `i32` discriminant; without one it's one more
than the previous variant's (`0` for the first), as in Rust and C, so
`Kind` below is `Personal = 0`, `Work = 10`, `Other = 11`. When no variant
has `fields`, the enum is **C-style**: it crosses as an `int32_t` and may be
a map key. When any variant has `fields`, the enum is **rich** (a sum type):
it crosses as a value buffer holding the variant's `value` as a tag
followed by its fields, and bindings map it to a Swift enum with associated
values, a sealed class hierarchy, a tagged union, and so on. Unit and data
variants may mix.

```yaml
{{#include ../../../examples/idl/contacts.yml:enums}}
```

Variant names and values are unique within an enum, and a variant's field
names are unique within the variant.

## Error domains

An error domain is a named set of stable codes that callables report. Each
binding generates a typed error (an exception hierarchy, a Swift `Error`
enum, a Go error type) so consumers match on the names you declared. A
library may declare any number of domains.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Domain type name (`BookError`) |
| `doc` | string | Documentation |
| `codes` | array of Code (required) | The codes |

Each code has a required `name` (PascalCase by convention), positive `code`,
and default `message`, plus an optional `doc` and optional `fields`: a
structured payload with the same shape as record fields, delivered as
properties of the raised error.

```yaml
{{#include ../../../examples/idl/contacts.yml:errors}}
```

A function, constructor, method, static, or callback method says how it can
fail with `throws`:

- **`throws: {Domain}`** names the one domain it reports (`Book.add` below).
- **`throws: any`** reports an untyped error: a message, with no code a
  consumer can match on (`parse_number` below). Bindings surface it as the
  library's base error type. A Rust producer's `Result<T, E>` is `throws:
  any` when `E` is an untyped error (`weaveffi::Error`, `String`,
  `std::io::Error`, ...).
- **No `throws`** (`count`) means a plain signature that can't report an
  error; a failure there is a producer bug and traps (see
  [Errors and Memory](../guides/errors-and-memory.md#domain-errors-and-the-trap-channel)).

A `throws` that names something other than an error domain is rejected:

```yaml
{{#include ../../../examples/idl/rejected/UnknownErrorDomain.yml}}
```

**Domains are open.** Each code has its own contract function, so a producer
may add codes to a domain without breaking a deployed binding. A binding
that receives a positive code it wasn't generated with reports it as the
domain's base error type with the code and message preserved, and discards
its payload undecoded. That's why payload fields are plain data: a field
may not reach an interface, directly or through a record or rich enum:

```yaml
{{#include ../../../examples/idl/rejected/ObjectInErrorPayload.yml}}
```

Codes must be positive (`0` is success and negative codes belong to the
runtime) and unique within their domain (`DuplicateErrorCode`). Code names
must be unique across every domain in the library
(`DuplicateErrorCodeName`), because several targets flatten them into one
namespace; qualify one (`OrderNotFound`) when two domains need the same
idea. A domain name is a declaration name like any other, but it isn't a
value type: `type: BookError` is `ErrorDomainAsType`. `any` is reserved and
can't name a domain.

## Interfaces

An interface is an object type: state with identity and behavior that lives
in the producer and crosses the ABI as an opaque, reference-counted pointer.
Consumers see a class whose constructors, methods, and statics call into the
producer.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Type name |
| `doc`, `deprecated` | string | Documentation, deprecation message |
| `constructors` | array of Function | Return a new instance; no `return`, never `async` |
| `methods` | array of Function | Take the instance implicitly |
| `statics` | array of Function | No instance |

```yaml
{{#include ../../../examples/idl/contacts.yml:interfaces}}
```

An interface needs at least one member, and constructor, method, and static
names share one namespace. A constructor named `new` becomes the canonical
constructor where the target has one (Swift `init`, Python `__init__`);
others become static factories. For an async factory, declare an `async`
static that returns the interface.

An interface may appear in every position except a map key and an error
payload: parameters, returns, optionals, lists, map values, record and
variant fields, iterator elements, async results, and callback parameters
and returns. `Book?` at the top level of a parameter or return is a nullable
pointer. Every position moves or borrows references by the rules in
[Errors and Memory](../guides/errors-and-memory.md#objects).

An interface lowers to an opaque type, one function per member (methods take
a leading `self`), and implicit `_clone` and `_destroy` functions you never
declare. From the document above (prefix `contacts`):

```c
typedef struct contacts_Book contacts_Book;
contacts_Book* contacts_Book_new(uint32_t capacity, contacts_error* out_err);
contacts_run contacts_Book_get(const contacts_Book* self, uint64_t id, contacts_error* out_err);
contacts_Book* contacts_Book_merged_with(const contacts_Book* self, const contacts_Book* other,
                                         contacts_error* out_err);
contacts_Book* contacts_Book_clone(const contacts_Book* self);
void contacts_Book_destroy(contacts_Book* self);
```

## Callback interfaces

A callback interface is the inverse of an interface: methods the consumer
implements and the producer calls, any number of times and from any thread,
for as long as it holds the implementation.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Type name |
| `doc`, `deprecated` | string | Documentation, deprecation message |
| `methods` | array of Function (required, non-empty) | In vtable order |

```yaml
{{#include ../../../examples/idl/contacts.yml:callbacks}}
```

Methods are restricted:

- synchronous: no `async`, no `cancellable` (`InvalidCallbackMethod`);
- they return nothing or any type except an iterator
  (`InvalidCallbackMethod`) or a callback interface
  (`CallbackInterfaceInInvalidPosition`); a string, bytes, typed-array, or
  buffered return is a run the consumer allocates with `{p}_alloc` and the
  producer adopts, and an object return moves one reference to the
  producer;
- parameters may be any type except an iterator
  (`IteratorInInvalidPosition`) or a callback interface
  (`CallbackInterfaceInInvalidPosition`); an object parameter moves one
  reference to the consumer.

A method may declare `throws` like any callable. With `throws: BookError`
its consumer may fail with one of the domain's codes, with the code's
fields, and the producer receives the typed error; with `throws: any` a
consumer failure reaches the producer as an untyped error with its message.
Every failure of a method without `throws`, and a code its domain doesn't
declare, reaches the producer as the runtime code `-4`. Each callback method
has its own contract function, and vtables are open, so appending a method
to a callback interface doesn't break a deployed binding; reordering methods
or inserting one before others moves vtable slots and does.

A callback interface is valid only as a parameter of a function,
constructor, method, or static, passed bare (`Listener`) or optional
(`Listener?`, where none is a null vtable). Anything else is
`CallbackInterfaceInInvalidPosition`:

```yaml
{{#include ../../../examples/idl/rejected/CallbackInterfaceInInvalidPosition.yml}}
```

A callback interface lowers to an argument struct and a vtable type that
starts with a header (the vtable's `size`, its `flags`, and the `free`
release hook) followed by one entry per method in declaration order:

```c
typedef struct contacts_Listener { void* ctx; const contacts_Listener_vtable* vtable; } contacts_Listener;
struct contacts_Listener_vtable {
    uint32_t size;
    uint32_t flags;
    void (*free)(void* ctx);
    bool (*approve)(void* ctx, contacts_run contact, contacts_error* out_err);
    void (*on_change)(void* ctx, contacts_run change, contacts_error* out_err);
};
void contacts_Book_subscribe(const contacts_Book* self, contacts_Listener listener,
                             contacts_error* out_err);
```

## Functions

Free functions use the [Function](#function) shape:

```yaml
{{#include ../../../examples/idl/contacts.yml:functions}}
```

`async: true` makes a function, method, or static complete through a
callback, surfaced as each language's async idiom, and `cancellable: true`
adds a cancel token; both are covered in
[Async and Cancellation](../guides/async.md). Constructors can't be async
(`AsyncConstructor`), and `cancellable` requires `async`:

```yaml
{{#include ../../../examples/idl/rejected/CancellableNotAsync.yml}}
```

`deprecated:` on a function, method, static, record, enum, interface, or
callback interface propagates its message to every binding's native
deprecation marker.

## Multi-file IDLs

A large API can be split across files. A document's top-level `imports:`
lists other IDL files, as paths relative to the importing file, and every
declaration of an imported file joins the importer's one namespace. A
declaration in any file may refer to one in any other, in either direction:

```yaml
{{#include ../../../examples/idl/drawing/drawing.yml:root}}
```

`drawing.yml` uses `Shape` and throws `DrawError` from the file it imports:

```yaml
{{#include ../../../examples/idl/drawing/shapes.yml:shapes}}
```

which in turn imports `Unit`, which `drawing.yml` uses too:

```yaml
{{#include ../../../examples/idl/drawing/units.yml:units}}
```

The rules:

- Every file declares the same `version`. The library's `doc` and
  `imports` are the root document's; an imported file's `doc` is ignored.
- Imports are recursive, and a file reached twice (here `units.yml`, if
  `drawing.yml` imported it too) is read once. An import chain that leads
  back to a file importing it is an error (`import cycle`).
- Names are unique across all the files, exactly as in one document, and a
  diagnostic points into the file that declares the offending item.
- `weaveffi generate`, `validate`, and `generate --check` read the root
  document with every file it imports, so the bindings, the contract, and
  the drift check cover the whole API.

## Names

**A library is one namespace.** Record, enum, error domain, interface,
callback interface, and free function names are unique across the whole
library, whichever file declares them (`DuplicateName`):

```yaml
{{#include ../../../examples/idl/rejected/DuplicateName.yml}}
```

Error code names are unique across every domain (`DuplicateErrorCodeName`).
Members are scoped to their owner: parameters to their callable, fields to
their record or variant, variants to their enum, constructors, methods, and
statics to their interface, and methods to their callback interface.

Because a name identifies one declaration, a type reference is always the
bare name. C symbols are the library's prefix and the name: a function
`get_contact` is `{p}_get_contact`, a record `Contact` is `{p}_Contact`, and
a method `get` on `Book` is `{p}_Book_get` (see [Naming](naming.md) and the
[C ABI contract](abi.md#signatures)).

## Documentation comments

`doc:` is accepted on the library, functions, parameters, records, fields,
enums, variants, error domains and codes, interfaces and their members, and
callback interfaces and their methods. Every generator emits it in the
target's native doc-comment syntax (`///`, `/** */`, docstrings, XML docs);
multi-line YAML block strings (`|`) are preserved. An absent `doc:` emits
nothing.

## Validation

`weaveffi validate` (and every generating command) runs every rule and
reports every violation in one pass, each with a message, a suggestion, and a
source span in the file that declares the offending item when one can be
found. `--format json` emits the same diagnostics, each with a `code` field
naming the error alongside its own fields; `--warn` adds advisory warnings.

The rules in brief:

- Names are identifiers (a letter or `_`, then ASCII letters, digits, and
  `_`) and not one of `if`, `else`, `for`, `while`, `loop`, `match`, `type`,
  `return`, `async`, `await`, `break`, `continue`, `fn`, `struct`, `enum`,
  `mod`, or `use` (and no error domain is named `any`). Generators escape
  other languages' keywords themselves.
- Declaration names and error code names are unique across the library;
  every other name is unique in its scope (see [Names](#names)).
- Records, enums, interfaces, and callback interfaces aren't empty, and no
  record or rich enum contains itself by value.
- Types resolve and appear only in the positions in the table above.
- `throws` names an error domain (or is `any`); `cancellable` needs
  `async`.
- Codes are positive and their values unique within a domain.
- **No two declarations lower to the same C identifier.** Every symbol is
  the prefix plus the declaration's name (`Owner_member` for a member), so a
  free function `Book_get` collides with a `get` method on `Book`, a
  function named `Book_clone` with `Book`'s implicit `_clone`, and a
  function named `foo_callback` with the completion type of an async `foo`.
  Names in a family the runtime and the C value-buffer helpers reserve
  (`{p}_contract_*`, `{p}_reader_*`, `{p}_writer_*`, `{p}_str_*`,
  `{p}_bytes_*`, the `{p}_buf_*` codec names, and the runtime's own
  symbols) collide too.
- **No parameter is named after a slot the C ABI adds** (`self`, `ctx`,
  `out_err`, `callback`, `context`, `cancel_token`).

```yaml
{{#include ../../../examples/idl/rejected/SymbolCollision.yml}}
```

A document with an unsupported `version` reports only
`UnsupportedSchemaVersion`.

### Error catalog

| Code | Reported when |
|------|---------------|
| `UnsupportedSchemaVersion` | `version` isn't `0.13.0` |
| `InvalidIdentifier` | a name isn't a valid identifier |
| `ReservedKeyword` | a name is a reserved word, or an error domain is named `any` |
| `DuplicateName` | two declarations of the library share a name |
| `DuplicateErrorCodeName` | two codes anywhere in the library share a name |
| `SymbolCollision` | two declarations lower to the same C identifier, or one falls in a reserved family |
| `SlotCollision` | a parameter has the name of a slot the C ABI adds |
| `DuplicateParamName` | two parameters of a callable share a name |
| `CancellableNotAsync` | a synchronous callable is `cancellable` |
| `UnknownErrorDomain` | `throws` names something that isn't an error domain |
| `AsyncIteratorReturn` | an async function returns `iter<T>` |
| `DuplicateErrorCode` | two codes in one domain share a value |
| `InvalidErrorCode` | a code is zero or negative |
| `EmptyRecord` | a record has no fields |
| `DuplicateRecordField` | two fields of a record share a name |
| `RecursiveType` | a record or rich enum contains itself by value |
| `EmptyEnum` | an enum has no variants |
| `DuplicateEnumVariant` | two variants of an enum share a name |
| `DuplicateEnumVariantField` | two fields of a rich-enum variant share a name |
| `DuplicateEnumValue` | two variants of an enum share a value |
| `EmptyInterface` | an interface has no members |
| `DuplicateInterfaceMember` | two members of an interface share a name |
| `ConstructorHasReturn` | a constructor declares `return` |
| `AsyncConstructor` | a constructor is `async` |
| `EmptyCallbackInterface` | a callback interface has no methods |
| `DuplicateCallbackMethod` | two methods of a callback interface share a name |
| `InvalidCallbackMethod` | a callback method is async or cancellable, or returns an iterator |
| `UnknownTypeRef` | a reference names no declaration |
| `ErrorDomainAsType` | an error domain is used as a value type |
| `UnsupportedPrimitive` | a reference names `usize`, `isize`, `u128`, `i128`, or `char` |
| `InvalidMapKey` | a map key isn't an integer, `bool`, `string`, or C-style enum |
| `InterfaceInInvalidPosition` | an interface is used as a map key |
| `CallbackInterfaceInInvalidPosition` | a callback interface appears other than as a parameter (bare or `Cb?`) |
| `ObjectInErrorPayload` | an error code's field reaches an interface, directly or through a record or rich enum |
| `IteratorInInvalidPosition` | `iter<T>` appears other than as an outermost return |

A file that can't be read, has no IDL extension, fails to parse, declares
another schema version than the root, or closes an import cycle is reported
before validation, with the file's name.

### Warnings

Warnings never fail validation; `--warn` reports them.

| Code | Fires when |
|------|------------|
| `Undocumented` | a top-level declaration has no `doc:`, so its bindings have no doc comment |
| `DeepNesting` | a type nests composites more than three levels deep |
| `LargeEnumVariantCount` | an enum has more than 100 variants |
