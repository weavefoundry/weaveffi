# IDL Schema

An IDL document describes an API: its modules, types, and functions. YAML
and JSON are accepted (TOML is only for [`weaveffi.toml`](../guides/config.md),
never for an IDL); this page uses YAML. It documents schema version
`0.12.0`, the only version the current tools accept. A Rust producer
never writes one by hand (the macro embeds the API in the built library, and
[`weaveffi extract`](../guides/extract.md) prints it as an IDL), but the
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
version: "0.12.0"
modules:
  - name: my_module
    doc: "..."
    structs: [...]
    enums: [...]
    interfaces: [...]
    callback_interfaces: [...]
    functions: [...]
    errors: [...]
    modules: [...]
```

The document describes only the API. Package identity and generator options
live in [`weaveffi.toml`](../guides/config.md). Every object rejects unknown
keys, so a misspelled field fails to parse instead of being ignored.

| Field | Type | Required | Meaning |
|-------|------|----------|---------|
| `version` | string | yes | Must be `"0.12.0"` |
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
| `errors` | array of ErrorDomain | The module's error domains |
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
| `throws` | string | The error domain the callable reports (`KvError`), or `any` for an untyped error; omit when it can't fail (see [Error domains](#error-domains)) |
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
| `Name` | A struct, enum, interface, or callback interface, by its bare name |
| `T?` | Optional |
| `[T]` | List |
| `{K:V}` | Map |
| `iter<T>` | Lazy iterator (returns only) |

A user type is always written as its bare name: type names are global (see
[Modules and names](#modules-and-names)), so there's no module-qualified
spelling, and a dotted name such as `kv.Store` is rejected with
`QualifiedTypeRef`. `usize`, `isize`, `u128`, `i128`, and `char` aren't
primitives; a reference to one is reported as `UnsupportedPrimitive` (use
`u64` or `i64` for sizes and wide integers, and `string` for characters).

The parser reads outside in: `[Contact?]` is a list of optional contacts and
`[Contact]?` an optional list. Composites nest to any depth. Quote anything
YAML would otherwise interpret (`"string?"`, `"[i32]"`, `"{string:i32}"`).
The Node and Wasm targets surface `i64` and `u64` as `BigInt`.

How a type is spelled never depends on how it crosses, but two shapes
cross faster than the rest at a call boundary: an optional scalar or C-style
enum (`i32?`, `bool?`, `Color?`, the OptDirect family) is a presence flag
plus the value, and a list of a fixed-width number (`[i32]`, `[f64]`, every
integer and float type except `u8` and `bool`, the Slice family) is a typed
array. Every binding still surfaces them as its ordinary optional and list
types; inside a record, variant, or error payload they use the value-buffer
encoding like any other type. See the
[C ABI contract](abi.md#families-and-slots).

**Map keys** must be an integer, `bool`, `string`, or a C-style enum, so
every target can use its native dictionary with exact equality. Floats,
`bytes`, composites, and objects are rejected:

```yaml
# rejected: InvalidMapKey
version: "0.12.0"
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
version: "0.12.0"
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
| `string` | yes | yes | yes | yes | yes | yes | yes |
| `bytes` | yes | yes | yes | yes | no | yes | yes |
| C-style enum | yes | yes | yes | yes | yes | yes | yes |
| struct, rich enum | yes | yes | yes | yes | no | yes | yes |
| interface | yes | yes | yes | yes | no | yes | yes |
| callback interface | yes | no | no | `Cb?` as a top-level parameter only | no | no | no |
| `T?`, `[T]`, `{K:V}` | yes | yes | yes | yes | no | yes | yes |
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
version: "0.12.0"
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
`int32_t` and may be a map key. When any variant has `fields`, the enum is **rich** (a sum type): it crosses
as a value buffer holding the variant's `value` as a tag followed by its
fields, and bindings map it to a Swift enum with associated values, a sealed
class hierarchy, a tagged union, and so on. Unit and data variants may mix.

```yaml
version: "0.12.0"
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
payloads, iterator elements, async results, and callback parameters and
returns. A map keyed by an interface reports both `InvalidMapKey` and
`InterfaceInInvalidPosition`. `Store?`
at the top level of a parameter or return is a nullable pointer. Every
position transfers or borrows references by the rules in
[Errors and Memory](../guides/errors-and-memory.md#objects).

```yaml
version: "0.12.0"
modules:
  - name: kv
    errors:
      - name: KvError
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
            throws: KvError
        methods:
          - name: get
            params:
              - { name: key, type: string }
            return: bytes
            throws: KvError
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
            throws: KvError
```

```yaml
# rejected: InvalidMapKey, InterfaceInInvalidPosition
version: "0.12.0"
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

Methods are restricted:

- synchronous: no `async`, no `cancellable` (`InvalidCallbackMethod`);
- return nothing or any type except an iterator (`InvalidCallbackMethod`)
  or a callback interface (`CallbackInterfaceInInvalidPosition`); a string,
  bytes, or buffered return is a run the consumer allocates with
  `{prefix}_alloc` and the producer adopts, and an object return transfers
  one reference to the producer;
- parameters may be any type except an iterator
  (`IteratorInInvalidPosition`) or a callback interface
  (`CallbackInterfaceInInvalidPosition`); an object parameter transfers one
  reference to the consumer.

A method may declare `throws` like any callable. With `throws: KvError` its
consumer may fail with one of the domain's codes, with the code's fields
(which can't include objects), and the producer receives the typed error;
with `throws: any` a consumer failure reaches the producer as an untyped
error with its message. Every failure of a method without `throws`, and a
code its domain doesn't declare, reaches the producer as the runtime code
`-4`. Each callback method is its own contract entry, so adding a method to
a callback interface doesn't break a deployed binding.

A callback interface is valid only as a top-level parameter of a function,
constructor, method, or static, passed bare (`Listener`) or optional
(`Listener?`, where none is a null vtable). Anything else is
`CallbackInterfaceInInvalidPosition`:

```yaml
# rejected: CallbackInterfaceInInvalidPosition
version: "0.12.0"
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
version: "0.12.0"
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
          - name: name
            return: string
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

A callback interface lowers to a vtable type that starts with a header (the
vtable's `size`, its `flags`, and the `free` release hook) followed by one
entry per method in declaration order; a parameter lowers to a `ctx` pointer
plus the vtable. From the `kitchen_sink` fixture, whose `label` returns a
`string` and throws, `latest` returns an `Item?`, and `favorite` returns a
`Gadget`:

```c
typedef struct kitchen_sink_kitchen_ReadyListener_vtable {
    uint32_t size;
    uint32_t flags;
    void (*free)(void* ctx);
    void (*on_ready)(void* ctx, int32_t code, const uint8_t* msg_ptr, size_t msg_len, kitchen_sink_error* out_err);
    bool (*on_item)(void* ctx, const uint8_t* item_ptr, size_t item_len, kitchen_sink_kitchen_Gadget* gadget, kitchen_sink_error* out_err);
    void (*label)(void* ctx, uint8_t** out_ptr, size_t* out_len, kitchen_sink_error* out_err);
    void (*latest)(void* ctx, uint8_t** out_ptr, size_t* out_len, kitchen_sink_error* out_err);
    kitchen_sink_kitchen_Gadget* (*favorite)(void* ctx, kitchen_sink_error* out_err);
} kitchen_sink_kitchen_ReadyListener_vtable;

int32_t kitchen_sink_kitchen_subscribe(void* listener_ctx, const kitchen_sink_kitchen_ReadyListener_vtable* listener_vtable, kitchen_sink_error* out_err);
bool kitchen_sink_kitchen_maybe_subscribe(void* listener_ctx, const kitchen_sink_kitchen_ReadyListener_vtable* listener_vtable, kitchen_sink_error* out_err);
```

## Error domains

An error domain is a named set of stable codes that callables report. Each
binding generates a typed error (an exception hierarchy, a Swift `Error`
enum, a Go error type) so consumers match on the names you declared. A
module may declare any number of domains in its `errors:` list.

| Field | Type | Meaning |
|-------|------|---------|
| `name` | string (required) | Domain type name (`KvError`) |
| `codes` | array of Code (required) | The codes |

Each code has a required `name` (PascalCase by convention), positive `code`,
and default `message`, plus an optional `doc` and optional `fields`: a
structured payload with the same shape as struct fields, delivered as
properties of the raised error.

```yaml
version: "0.12.0"
modules:
  - name: kv
    errors:
      - name: KvError
        codes:
          - name: KeyNotFound
            code: 1001
            message: key not found
            fields:
              - { name: key, type: string }
          - { name: StoreFull, code: 1003, message: store has reached capacity }
      - name: ParseError
        codes:
          - { name: BadSyntax, code: 1, message: bad syntax }
    functions:
      - name: lookup
        params:
          - { name: key, type: string }
        return: bytes
        throws: KvError
      - name: parse_key
        params:
          - { name: text, type: string }
        return: string
        throws: ParseError
      - name: load_file
        params:
          - { name: path, type: string }
        return: bytes
        throws: any
      - name: count
        return: i64
```

A function, constructor, method, static, or callback method says how it can
fail with `throws`:

- **`throws: {Domain}`** names the one domain it reports (`lookup` above).
  The name is global, so the domain can be declared in any module.
- **`throws: any`** reports an untyped error: a message, with no code a
  consumer can match on (`load_file`). Bindings surface it as the library's
  base error type. A Rust producer's `Result<T, E>` becomes `throws: any`
  whenever `E` isn't a declared domain (an `anyhow::Error`, a
  `std::io::Error`, a `String`).
- **No `throws`** (`count`) means a plain signature that can't report an
  error; a failure there is a producer bug and traps (see
  [Errors and Memory](../guides/errors-and-memory.md#domain-errors-and-the-trap-channel)).

Boolean `throws: true` is gone; a document that still uses it fails to
parse with a message pointing at the new syntax. A `throws` that names
something other than an error domain is rejected:

```yaml
# rejected: UnknownErrorDomain
version: "0.12.0"
modules:
  - name: contacts
    functions:
      - name: get_contact
        params:
          - { name: id, type: i64 }
        return: string
        throws: ContactError
```

**Domains are open.** Each code is its own contract entry, so a producer may
add codes to a domain without breaking a deployed binding. A binding that
receives a positive code it wasn't generated with reports it as the domain's
base error type with the code and message preserved.

Codes must be positive (`0` is success and negative codes belong to the
runtime) and unique within their domain. Code names must be unique across
every domain in the API, because several targets flatten them into one
namespace; qualify one (`OrderNotFound`) when two domains need the same idea.
A domain name is a type name (unique with every other type), but it isn't a
value type: `type: KvError` is `ErrorDomainAsType`. `any` is reserved and
can't name a domain.

## Async, cancellable, and deprecated

`async: true` makes a function, method, or static complete through a
callback, surfaced as each language's async idiom; `cancellable: true` adds a
cancel token. Both are covered in [Async and Cancellation](../guides/async.md).
Constructors can't be async, and `cancellable` requires `async`:

```yaml
# rejected: CancellableNotAsync
version: "0.12.0"
modules:
  - name: net
    functions:
      - { name: ping, return: bool, cancellable: true }
```

`deprecated:` on a function, method, static, struct, enum, interface, or
callback interface propagates its message to every binding's native
deprecation marker.

## Modules and names

Modules nest to any depth. A nested module shares the validation rules of a
top-level one.

**Names are global.** Modules group declarations and namespace their C
symbols, but they don't scope names:

- Struct, enum, interface, callback interface, and error domain names are
  unique across the whole API (`DuplicateTypeName`).
- Free-function names are unique across the whole API
  (`DuplicateFunctionName`), because several targets flatten every module's
  functions into one namespace. A free function also can't share its name
  with an error domain (`NameCollisionWithErrorDomain`).
- Error code names are unique across every domain (`DuplicateErrorCodeName`).

Members are scoped to their owner: parameters to their callable, fields to
their record or variant, variants to their enum, and constructors, methods,
and statics to their interface. Sibling modules need distinct names
(`DuplicateModuleName`), but modules in different subtrees may share one.

Because every type name identifies one declaration, a reference is always the
bare name, wherever the type is declared. A reference to a name nothing
declares is an `UnknownTypeRef`. C symbols join the declaring module's path
with underscores: a function `get_record` in module `app.data` is
`{prefix}_app_data_get_record`, and a record `Record` declared there is
`{prefix}_app_data_Record`.

```yaml
version: "0.12.0"
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
              - { name: store, type: Store }
            return: Stats
```

```yaml
# rejected: DuplicateFunctionName, QualifiedTypeRef
version: "0.12.0"
modules:
  - name: kv
    interfaces:
      - name: Store
        constructors:
          - { name: open, params: [{ name: path, type: string }] }
    functions:
      - { name: stats, params: [{ name: store, type: kv.Store }], return: i64 }
  - name: metrics
    functions:
      - { name: stats, return: i64 }
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
source span located within the enclosing declaration when one can be found.
`--format json` emits the same diagnostics, each with a `code` field naming
the variant alongside the variant's own fields (`name`, `module`, and so
on); `--warn` adds advisory warnings.

The rules in brief:

- Names are identifiers (a letter or `_`, then ASCII letters, digits, and
  `_`) and not one of `if`, `else`, `for`, `while`, `loop`, `match`, `type`,
  `return`, `async`, `await`, `break`, `continue`, `fn`, `struct`, `enum`,
  `mod`, `use` (and no error domain is named `any`). Generators escape other
  languages' keywords themselves.
- Type names, free-function names, and error code names are unique across
  the whole API; every other name is unique in its scope (see
  [Modules and names](#modules-and-names)).
- Structs, enums, interfaces, and callback interfaces aren't empty.
- Types are bare names that resolve, and appear only in the positions in the
  table above.
- `throws` names an error domain (or is `any`); `cancellable` needs
  `async`.
- Codes are positive and their values unique within a domain; no free
  function shares a domain's name.
- **No two declarations lower to the same C symbol.** Every symbol is the
  prefix plus the underscore-joined module path plus the declaration name,
  so a function `x_y` in module `m` collides with a function `y` in module
  `m.x` or `m_x`; so do a free function `Store_get` and a `get` method on
  interface `Store`, a function named `Store_clone` and `Store`'s implicit
  `_clone`, and a function named `foo_callback` and the completion type of
  an async `foo`. Symbols that fall in a family the C value-buffer helpers
  reserve (`{prefix}_list_*`, `{prefix}_map_*`, and so on, which covers
  everything in a top-level module named `list`) collide too. The full
  symbol table is in the [C ABI contract](abi.md#symbol-names).
- **No two C slots of one callable share a name.** A parameter `name` lowers
  to `name_ptr` and `name_len` (or `has_name` and `name`, or `name_ctx` and
  `name_vtable`), so it collides with a parameter called `name_ptr` next to
  it, and a parameter named `out_err`, `out_len`, `out_value`, `callback`,
  `context`, or `cancel_token` can collide with a slot the lowering adds.

A document with an unsupported `version` reports only
`UnsupportedSchemaVersion`.

### Error catalog

| Code | Reported when |
|------|---------------|
| `UnsupportedSchemaVersion` | `version` isn't `0.12.0` |
| `NoModuleName` | a module has no name |
| `InvalidModuleName` | a module name isn't an identifier or is reserved |
| `DuplicateModuleName` | two sibling modules share a name |
| `InvalidIdentifier` | a name isn't a valid identifier |
| `ReservedKeyword` | a name is a reserved word, or an error domain is named `any` |
| `DuplicateFunctionName` | two free functions anywhere in the API share a name |
| `DuplicateParamName` | two parameters of a callable share a name |
| `NameCollisionWithErrorDomain` | a free function and an error domain share a name |
| `DuplicateStructField` | two fields of a struct share a name |
| `EmptyStruct` | a struct has no fields |
| `EmptyEnum` | an enum has no variants |
| `DuplicateEnumVariant` | two variants of an enum share a name |
| `DuplicateEnumValue` | two variants of an enum share a value |
| `DuplicateEnumVariantField` | two fields of a rich-enum variant share a name |
| `DuplicateInterfaceMember` | two members of an interface share a name |
| `EmptyInterface` | an interface has no members |
| `ConstructorHasReturn` | a constructor declares `return` |
| `AsyncConstructor` | a constructor is `async` |
| `InterfaceInInvalidPosition` | an interface is used as a map key |
| `EmptyCallbackInterface` | a callback interface has no methods |
| `DuplicateCallbackMethod` | two methods of a callback interface share a name |
| `InvalidCallbackMethod` | a callback method is async or cancellable, or returns an iterator |
| `CallbackInterfaceInInvalidPosition` | a callback interface appears other than as a top-level parameter (bare or `Cb?`) |
| `DuplicateTypeName` | two types (including error domains) anywhere in the API share a name |
| `UnknownTypeRef` | a reference names no declaration |
| `QualifiedTypeRef` | a reference is a dotted, module-qualified name |
| `UnsupportedPrimitive` | a reference names `usize`, `isize`, `u128`, `i128`, or `char` |
| `InvalidMapKey` | a map key isn't an integer, `bool`, `string`, or C-style enum |
| `IteratorInInvalidPosition` | `iter<T>` appears other than as an outermost return |
| `AsyncIteratorReturn` | an async function returns `iter<T>` |
| `CancellableNotAsync` | a synchronous callable is `cancellable` |
| `UnknownErrorDomain` | `throws` names something that isn't an error domain |
| `ErrorDomainAsType` | an error domain is used as a value type |
| `ErrorDomainMissingName` | an error domain has no name |
| `DuplicateErrorCode` | two codes in one domain share a value |
| `DuplicateErrorCodeName` | two codes anywhere in the API share a name |
| `InvalidErrorCode` | a code is zero or negative |
| `SymbolCollision` | two declarations lower to the same C identifier, or one falls in a reserved helper family |
| `SlotCollision` | two C slots of one callable's lowered signature share a name |

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

An address book combining an enum, a record, an error domain, an interface,
and a nested module that uses its parent's types:

```yaml
version: "0.12.0"
modules:
  - name: contacts
    doc: An in-memory address book.
    errors:
      - name: ContactsError
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
            throws: ContactsError
          - name: get
            params:
              - { name: id, type: i64 }
            return: Contact
            throws: ContactsError
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
