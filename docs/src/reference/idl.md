# IDL Schema

An IDL document describes an API: its modules, types, and functions. YAML,
JSON, and TOML are all accepted; this page uses YAML. It documents schema
version `0.10.0`, the only version the current tools accept. A Rust producer
never writes one by hand (the CLI extracts it from the source), but the
schema is the same either way.

Every YAML block on this page is a complete document that passes
`weaveffi validate`, except blocks whose first line is `# rejected:`, which
show what the validator refuses and the error it reports.

## Editor support

The CLI prints a JSON Schema for the document with
`weaveffi schema --format json-schema`, and a copy is checked in as
[`weaveffi.schema.json`](https://github.com/weavefoundry/weaveffi/blob/main/weaveffi.schema.json).
Editors using the YAML Language Server pick it up from a header comment:

```yaml
# yaml-language-server: $schema=./weaveffi.schema.json
```

## Document structure

```text
version: "0.10.0"
modules:
  - name: my_module
    doc: "..."
    structs: [...]
    enums: [...]
    interfaces: [...]
    callback_interfaces: [...]
    functions: [...]
    errors: { ... }
    modules: [...]
```

The document describes only the API. Package identity and generator options
live in [`weaveffi.toml`](../guides/config.md). Every object rejects unknown
keys, so a misspelled field fails to parse instead of being ignored.

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `version` | string | yes | Must be `"0.10.0"` |
| `modules` | array of Module | yes | Top-level modules |

### Module

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Namespace segment and C symbol component (`kv`) |
| `doc` | string | Module documentation |
| `functions` | array of Function | Free functions |
| `structs` | array of Struct | Records (value types) |
| `enums` | array of Enum | C-style or rich enums |
| `interfaces` | array of Interface | Reference-counted object types |
| `callback_interfaces` | array of CallbackInterface | Method sets the consumer implements |
| `errors` | ErrorDomain | The module's error domain |
| `modules` | array of Module | Nested submodules |

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
| `throws` | bool | Report typed domain errors (default `false`) |
| `async` | bool | Complete asynchronously (default `false`) |
| `cancellable` | bool | Accept a cancel token; requires `async: true` |
| `deprecated` | string | Deprecation message propagated to every binding |

A Param has a required `name` and `type` and an optional `doc`.

## Types

A `TypeRef` is a string:

| Syntax | Meaning |
|--------|---------|
| `i8` `i16` `i32` `i64` `u8` `u16` `u32` `u64` | Fixed-width integers |
| `f32` `f64` | IEEE 754 floats |
| `bool` | Boolean |
| `string` | UTF-8 text |
| `bytes` | Byte string (`[u8]` is accepted and becomes `bytes`) |
| `Name`, `a.b.Name` | A struct, enum, interface, or callback interface |
| `T?` | Optional |
| `[T]` | List |
| `{K:V}` | Map |
| `iter<T>` | Lazy iterator (returns only) |

The parser reads outside in: `[Contact?]` is a list of optional contacts and
`[Contact]?` an optional list. Composites nest to any depth. Quote anything
YAML would otherwise interpret (`"string?"`, `"[i32]"`, `"{string:i32}"`).
The Node and Wasm targets surface `i64` and `u64` as `BigInt`.

**Map keys** must be an integer, `bool`, `string`, or a C-style enum, so
every target can use its native dictionary with exact equality. Floats,
`bytes`, composites, and objects are rejected:

```yaml
# rejected: InvalidMapKey
version: "0.10.0"
modules:
  - name: maps
    functions:
      - name: by_weight
        params:
          - { name: index, type: "{f64:i32}" }
```

**Iterators** are lazy: every binding pulls one element per step of its
native iteration idiom and destroys the handle exactly once. `iter<T>` is
valid only as the outermost return of a synchronous callable. It's rejected
in a parameter, a field, or nested in another type (`IteratorInInvalidPosition`),
and as the return of an async function (`AsyncIteratorReturn`):

```yaml
# rejected: IteratorInInvalidPosition
version: "0.10.0"
modules:
  - name: streaming
    functions:
      - name: consume
        params:
          - { name: items, type: "iter<i32>" }
```

### Type positions

"Buffered field" means a struct field, rich-enum variant field, or error
payload field.

| Type | Param | Return | Buffered field | Inside `T?` `[T]` `{K:V}` | Map key | Callback param | Callback return |
|------|-------|--------|----------------|---------------------------|---------|----------------|-----------------|
| integers, floats, `bool` | yes | yes | yes | yes | integers, `bool` | yes | yes |
| `string` | yes | yes | yes | yes | yes | yes | no |
| `bytes` | yes | yes | yes | yes | no | yes | no |
| C-style enum | yes | yes | yes | yes | yes | yes | yes |
| struct, rich enum | yes | yes | yes | yes | no | yes | no |
| interface | yes | yes | yes | yes | no | yes | no |
| callback interface | yes | no | no | no | no | no | no |
| `T?`, `[T]`, `{K:V}` | yes | yes | yes | yes | no | yes | no |
| `iter<T>` | no | yes (sync) | no | no | no | no | no |

How each family crosses the boundary is in the [C ABI contract](abi.md#families-and-slots).

## Structs

A struct is a record: a value type with named fields, crossing the ABI by
value as a [value buffer](value-buffers.md). Bindings map it to a data
class, struct, or record type; no per-struct C functions exist.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Type name |
| `doc` | string | Documentation |
| `deprecated` | string | Deprecation message |
| `fields` | array of Field (required, non-empty) | Each with `name`, `type`, optional `doc` |

```yaml
version: "0.10.0"
modules:
  - name: geometry
    structs:
      - name: Point
        doc: A point in the plane.
        fields:
          - { name: x, type: f64 }
          - { name: "y", type: f64 }
      - name: Polygon
        fields:
          - { name: points, type: "[Point]" }
          - { name: label, type: "string?" }
    functions:
      - name: area
        params:
          - { name: shape, type: Polygon }
        return: f64
```

## Enums

An enum is a fixed set of variants, each with an explicit `i32` `value`.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Type name |
| `doc`, `deprecated` | string | Documentation, deprecation message |
| `variants` | array of Variant (required, non-empty) | Each with `name`, `value`, optional `doc` and `fields` |

When no variant has `fields`, the enum is **C-style**: it crosses as an
`int32_t`, may be a map key, and may be returned from a callback method.
When any variant has `fields`, the enum is **rich** (a sum type): it crosses
as a value buffer holding the variant's `value` as a tag followed by its
fields, and bindings map it to a Swift enum with associated values, a sealed
class hierarchy, a tagged union, and so on. Unit and data variants may mix.

```yaml
version: "0.10.0"
modules:
  - name: shapes
    enums:
      - name: Unit
        variants:
          - { name: Metric, value: 0 }
          - { name: Imperial, value: 1 }
      - name: Shape
        variants:
          - { name: Empty, value: 0 }
          - name: Circle
            value: 1
            fields:
              - { name: radius, type: f64 }
          - name: Rect
            value: 2
            fields:
              - { name: width, type: f32 }
              - { name: height, type: f32 }
    functions:
      - name: area
        params:
          - { name: shape, type: Shape }
          - { name: unit, type: Unit }
        return: f64
```

Variant names and values are unique within an enum, and a variant's field
names are unique within the variant.

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

An interface needs at least one member, and constructor, method, and static
names share one namespace. A constructor named `new` becomes the canonical
constructor where the target has one (Swift `init`, Python `__init__`);
others become static factories. For an async factory, declare an `async`
static that returns the interface.

An interface may appear in every position except a map key: parameters,
returns, optionals, lists, map values, record and variant fields, error
payloads, iterator elements, async results, and callback parameters. `Store?`
at the top level of a parameter or return is a nullable pointer. Every
position transfers or borrows references by the rules in
[Errors and Memory](../guides/errors-and-memory.md#objects).

```yaml
version: "0.10.0"
modules:
  - name: kv
    errors:
      name: KvError
      codes:
        - { name: KeyNotFound, code: 1001, message: "key not found" }
    structs:
      - name: StoreInfo
        fields:
          - { name: label, type: string }
          - { name: store, type: Store }
    interfaces:
      - name: Store
        constructors:
          - name: open
            params:
              - { name: path, type: string }
            throws: true
        methods:
          - name: get
            params:
              - { name: key, type: string }
            return: bytes
            throws: true
          - name: share
            return: Store
          - name: larger
            params:
              - { name: other, type: "Store?" }
            return: "Store?"
          - name: describe
            params:
              - { name: label, type: string }
            return: StoreInfo
        statics:
          - name: open_many
            params:
              - { name: paths, type: "[string]" }
            return: "[Store]"
            throws: true
```

```yaml
# rejected: InterfaceInInvalidPosition
version: "0.10.0"
modules:
  - name: kv
    interfaces:
      - name: Store
        methods:
          - { name: count, return: i64 }
    functions:
      - name: index
        params:
          - { name: stores, type: "{Store:i32}" }
```

An interface lowers to an opaque type, one symbol per member (methods take a
leading `self`), and implicit `_clone` and `_destroy` symbols you never
declare. From the `kitchen_sink` test fixture (prefix `kitchen_sink`, module
`kitchen`):

```c
typedef struct kitchen_sink_kitchen_Gadget kitchen_sink_kitchen_Gadget;
kitchen_sink_kitchen_Gadget* kitchen_sink_kitchen_Gadget_new(int64_t id, kitchen_sink_error* out_err);
const uint8_t* kitchen_sink_kitchen_Gadget_describe(const kitchen_sink_kitchen_Gadget* self, size_t* out_len, kitchen_sink_error* out_err);
kitchen_sink_kitchen_Gadget* kitchen_sink_kitchen_Gadget_clone(const kitchen_sink_kitchen_Gadget* self);
void kitchen_sink_kitchen_Gadget_destroy(kitchen_sink_kitchen_Gadget* self);
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

Methods are restricted, each enforced as `InvalidCallbackMethod`:

- synchronous: no `async`, no `cancellable`;
- no `throws`: a consumer failure is reported as the runtime code `-4`;
- return nothing or a direct value (an integer, float, `bool`, or C-style
  enum), because no consumer allocation can flow back to the producer;
- parameters may be any type except a callback interface or an iterator;
  an object parameter transfers one reference to the consumer.

A callback interface is valid only as a top-level parameter of a function,
constructor, method, or static. Anything else is
`CallbackInterfaceInInvalidPosition`:

```yaml
# rejected: CallbackInterfaceInInvalidPosition
version: "0.10.0"
modules:
  - name: events
    callback_interfaces:
      - name: Listener
        methods:
          - { name: on_event, params: [{ name: id, type: i64 }] }
    functions:
      - name: current_listener
        return: Listener
```

```yaml
version: "0.10.0"
modules:
  - name: events
    enums:
      - name: Delivery
        variants:
          - { name: Accept, value: 0 }
          - { name: Skip, value: 1 }
    structs:
      - name: Message
        fields:
          - { name: topic, type: string }
          - { name: text, type: string }
    callback_interfaces:
      - name: Subscriber
        methods:
          - name: route
            params:
              - { name: topic, type: string }
            return: Delivery
          - name: on_message
            params:
              - { name: message, type: Message }
    interfaces:
      - name: EventBus
        constructors:
          - name: new
        methods:
          - name: subscribe
            params:
              - { name: subscriber, type: Subscriber }
            return: i64
```

A callback interface lowers to a vtable type with one entry per method and a
trailing `free`; a parameter lowers to a `ctx` pointer plus the vtable. From
the `kitchen_sink` fixture:

```c
typedef struct kitchen_sink_kitchen_ReadyListener_vtable {
    void (*on_ready)(void* ctx, int32_t code, const uint8_t* msg_ptr, size_t msg_len, kitchen_sink_error* out_err);
    bool (*on_item)(void* ctx, const uint8_t* item_ptr, size_t item_len, kitchen_sink_kitchen_Gadget* gadget, kitchen_sink_error* out_err);
    void (*free)(void* ctx);
} kitchen_sink_kitchen_ReadyListener_vtable;

int32_t kitchen_sink_kitchen_subscribe(void* listener_ctx, const kitchen_sink_kitchen_ReadyListener_vtable* listener_vtable, kitchen_sink_error* out_err);
```

## Error domains

A module may declare one error domain: named, stable codes its throwing
callables report. Each binding generates a typed error (an exception
hierarchy, a Swift `Error` enum, a Go error type) so consumers match on the
names you declared.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Domain type name (`KvError`) |
| `codes` | array of Code (required) | The codes |

Each code has a `name` (PascalCase by convention), a positive `code`, a
default `message`, an optional `doc`, and optional `fields`: a structured
payload with the same shape as struct fields, delivered as properties of the
raised error.

```yaml
version: "0.10.0"
modules:
  - name: kv
    errors:
      name: KvError
      codes:
        - name: KeyNotFound
          code: 1001
          message: key not found
          fields:
            - { name: key, type: string }
        - { name: StoreFull, code: 1003, message: store has reached capacity }
    functions:
      - name: lookup
        params:
          - { name: key, type: string }
        return: bytes
        throws: true
      - name: count
        return: i64
```

Declaring a domain reserves the codes; a function, constructor, method, or
static opts in with `throws: true`. A callable without `throws` (`count`
above) has a plain signature and can't report a domain error; a failure there
is a producer bug and traps (see
[Errors and Memory](../guides/errors-and-memory.md#domain-errors-and-the-trap-channel)).
A domain is in scope for its module and every module nested in it, and
`throws: true` with no domain in scope is rejected:

```yaml
# rejected: ThrowsWithoutErrorDomain
version: "0.10.0"
modules:
  - name: contacts
    functions:
      - name: get_contact
        params:
          - { name: id, type: i64 }
        return: string
        throws: true
```

Codes must be positive (`0` is success and negative codes belong to the
runtime) and unique within their domain. Code names must be unique across
every domain in the API, because several targets flatten them into one
namespace; qualify one (`OrderNotFound`) when two domains need the same idea.

## Async, cancellable, and deprecated

`async: true` makes a function, method, or static complete through a
callback, surfaced as each language's async idiom; `cancellable: true` adds a
cancel token. Both are covered in [Async and Cancellation](../guides/async.md).
Constructors can't be async, and `cancellable` requires `async`:

```yaml
# rejected: CancellableNotAsync
version: "0.10.0"
modules:
  - name: net
    functions:
      - { name: ping, return: bool, cancellable: true }
```

`deprecated:` on a function, method, static, struct, enum, interface, or
callback interface propagates its message to every binding's native
deprecation marker.

## Modules and type references

Modules nest to any depth. A nested module shares the validation rules of a
top-level one, and an error domain declared on a parent serves the subtree.
C symbols join the path with underscores: a function `get_record` in module
`app.data` is `{prefix}_app_data_get_record`.

Every resolved type is identified by its **absolute path**: the declaring
module's path plus the name (`kv.Store`, `app.data.Record`). A reference
spells it one of two ways:

- **Bare** (`Store`): resolves through the API-wide type namespace. Struct,
  enum, interface, callback interface, and error domain names are unique
  across the whole API (`DuplicateTypeName`), so a bare name is never
  ambiguous.
- **Qualified** (`kv.Store`): the qualifier must match the declaring module's
  path exactly; `stats.Store` for a `Store` declared in `kv` is an
  `UnknownTypeRef`.

A reference to a name nothing declares is always an `UnknownTypeRef`.

```yaml
version: "0.10.0"
modules:
  - name: kv
    interfaces:
      - name: Store
        constructors:
          - name: open
            params:
              - { name: path, type: string }
    modules:
      - name: stats
        structs:
          - name: Stats
            fields:
              - { name: total_entries, type: i64 }
        functions:
          - name: get_stats
            params:
              - { name: store, type: kv.Store }
            return: Stats
```

## Documentation comments

`doc:` is accepted on modules, functions, parameters, structs, fields, enums,
variants, interfaces and their members, callback interfaces and their
methods, and error codes. Every generator emits it in the target's native
doc-comment syntax (`///`, `/** */`, docstrings, XML docs); multi-line YAML
block strings (`|`) are preserved. An absent `doc:` emits nothing.

## Validation

`weaveffi validate` (and every generating command) runs every rule and
reports every violation in one pass, each with a message, a suggestion, and a
source span when one can be located. `--format json` emits the same
diagnostics with a `code` field naming the variant; `--warn` adds advisory
warnings.

The rules in brief:

- Names are identifiers (a letter or `_`, then ASCII letters, digits, and
  `_`) and not one of `if`, `else`, `for`, `while`, `loop`, `match`, `type`,
  `return`, `async`, `await`, `break`, `continue`, `fn`, `struct`, `enum`,
  `mod`, `use`. Generators escape other languages' keywords themselves.
- Names are unique in their scope; type names and error code names are
  unique across the whole API.
- Structs, enums, interfaces, and callback interfaces aren't empty.
- Types resolve, and appear only in the positions in the table above.
- `throws` needs a domain in scope; `cancellable` needs `async`.
- Codes are positive and unique; a domain's name doesn't collide with a
  function in the same module.
- **No two declarations lower to the same C symbol.** Every symbol is the
  prefix plus the underscore-joined module path plus the declaration name,
  so `m.x_y`, `m.x.y`, and `m_x.y` collide; so do a free function
  `Store_get` and a `get` method on interface `Store`, a function named
  `Store_clone` and `Store`'s implicit `_clone`, and a function named
  `foo_callback` and the completion type of an async `foo`. The full symbol
  table is in the [C ABI contract](abi.md#symbol-names).

A document with an unsupported `version` reports only
`UnsupportedSchemaVersion`.

### Error catalog

| Code | Reported when |
|------|---------------|
| `UnsupportedSchemaVersion` | `version` isn't `0.10.0` |
| `NoModuleName` | a module has no name |
| `InvalidModuleName` | a module name isn't an identifier or is reserved |
| `DuplicateModuleName` | two sibling modules share a name |
| `InvalidIdentifier` | a name isn't a valid identifier |
| `ReservedKeyword` | a name is a reserved word |
| `DuplicateFunctionName` | two functions in a module share a name |
| `DuplicateParamName` | two parameters of a callable share a name |
| `NameCollisionWithErrorDomain` | a function and the module's error domain share a name |
| `DuplicateStructName` | two structs in a module share a name |
| `DuplicateStructField` | two fields of a struct share a name |
| `EmptyStruct` | a struct has no fields |
| `DuplicateEnumName` | two enums in a module share a name |
| `EmptyEnum` | an enum has no variants |
| `DuplicateEnumVariant` | two variants of an enum share a name |
| `DuplicateEnumValue` | two variants of an enum share a value |
| `DuplicateEnumVariantField` | two fields of a rich-enum variant share a name |
| `DuplicateInterfaceName` | two interfaces in a module share a name |
| `DuplicateInterfaceMember` | two members of an interface share a name |
| `EmptyInterface` | an interface has no members |
| `ConstructorHasReturn` | a constructor declares `return` |
| `AsyncConstructor` | a constructor is `async` |
| `InterfaceInInvalidPosition` | an interface is used as a map key |
| `DuplicateCallbackInterfaceName` | two callback interfaces in a module share a name |
| `EmptyCallbackInterface` | a callback interface has no methods |
| `DuplicateCallbackMethod` | two methods of a callback interface share a name |
| `InvalidCallbackMethod` | a callback method is async, cancellable, throwing, or returns a non-direct type |
| `CallbackInterfaceInInvalidPosition` | a callback interface appears other than as a top-level parameter |
| `DuplicateTypeName` | two types anywhere in the API share a bare name |
| `UnknownTypeRef` | a reference names no declaration, or its qualifier doesn't match the declaring module |
| `InvalidMapKey` | a map key isn't an integer, `bool`, `string`, or C-style enum |
| `IteratorInInvalidPosition` | `iter<T>` appears other than as an outermost return |
| `AsyncIteratorReturn` | an async function returns `iter<T>` |
| `CancellableNotAsync` | a synchronous callable is `cancellable` |
| `ThrowsWithoutErrorDomain` | `throws: true` with no error domain in scope |
| `ErrorDomainMissingName` | an error domain has no name |
| `DuplicateErrorName` | two codes in a domain share a name |
| `DuplicateErrorCode` | two codes in a domain share a value |
| `DuplicateErrorCodeName` | two domains declare a code with the same name |
| `InvalidErrorCode` | a code is zero or negative |
| `SymbolCollision` | two declarations lower to the same C identifier |

### Warnings

Warnings never fail validation.

| Code | Fires when |
|------|------------|
| `LargeEnumVariantCount` | an enum has more than 100 variants |
| `DeepNesting` | a type nests composites more than three levels deep |
| `EmptyModuleDoc` | a module with functions has no `doc:` on itself or any function |
| `AsyncVoidFunction` | an async free function returns nothing |
| `DeprecatedFunction` | a free function is deprecated (informational) |

## Complete example

A trimmed version of the `contacts` sample, combining an enum, a record, an
error domain, an interface, and a nested module that uses its parent's types:

```yaml
version: "0.10.0"
modules:
  - name: contacts
    doc: An in-memory address book.
    errors:
      name: ContactsError
      codes:
        - { name: InvalidName, code: 1, message: name must not be empty }
        - { name: NotFound, code: 2, message: contact not found }
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
    structs:
      - name: Contact
        fields:
          - { name: id, type: i64 }
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }
    interfaces:
      - name: ContactBook
        constructors:
          - name: new
        methods:
          - name: add
            params:
              - { name: name, type: string }
              - { name: email, type: "string?" }
              - { name: contact_type, type: ContactType }
            return: Contact
            throws: true
          - name: get
            params:
              - { name: id, type: i64 }
            return: Contact
            throws: true
          - name: list
            return: "[Contact]"
    modules:
      - name: groups
        functions:
          - name: count_of_type
            params:
              - { name: book, type: ContactBook }
              - { name: contact_type, type: ContactType }
            return: i32
```
