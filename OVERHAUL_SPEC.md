# WeaveFFI 0.5.0 overhaul: generator specification

Working document for the 0.5.0 interface/error/naming overhaul. Deleted before
the final commit. Read this fully before touching a generator.

## What already landed (do not redo)

- IR schema 0.5.0: `Module.interfaces` (`InterfaceDef` with `constructors`,
  `methods`, `statics`, each a plain `Function`), `Function.throws: bool`,
  `TypeRef::Interface(String)`.
- Validator: multi-error reporting (`ValidationDiagnostics`), interface rules,
  `throws` requires an error domain in scope, global type-name uniqueness.
- `BindingModel` (`weaveffi-core/src/model/mod.rs`):
  - `ModuleBinding.error: Option<ErrorBinding>` — the domain *in effect* for
    the module (own or inherited). `ErrorBinding { name, type_name,
    owner_path, declared_here, c_tag, codes: Vec<ErrorCodeBinding> }`.
    `ErrorCodeBinding { name, value, message, doc, c_const }`.
  - `ModuleBinding.interfaces: Vec<InterfaceBinding>`.
    `InterfaceBinding { name, doc, c_tag, constructors, methods, statics,
    destroy_symbol }`. Members are `FnBinding`s: constructors have
    `ret == Some(TypeRef::Interface(name))` and C symbol `{c_tag}_{name}`;
    methods have `has_self == true` and their `AbiFn.params` carry a leading
    `self` slot (`{c_tag}* self`, const pointer) that is NOT in
    `FnBinding.params`; statics are plain. All shapes (sync / async /
    iterator) are possible for methods and statics.
  - `FnBinding.throws: bool`, `FnBinding.has_self: bool`.
  - `ModuleBinding::callables()` yields free functions plus every interface
    member; `ModuleBinding::declares_error()`.
- `LanguageBackend` gained `render_error` and `render_interface` hooks;
  `emit_members` order is: error → enums → structs → interfaces → callbacks →
  listeners → functions.
- C ABI rendering (`weaveffi-core/src/cabi.rs`) already declares interface
  tags, member prototypes, destroy, and per-domain error-code C enums.
- The proc macro generates interface thunks, `ErrorReport` impls for
  `#[weaveffi::error]` enums, and wraps producer calls in `catch_unwind`
  (panics report through `out_err` with code `-2`,
  `weaveffi_abi::PANIC_ERROR_CODE`).
- `weaveffi_error.code` semantics at the ABI: `0` success; a declared domain
  code for a typed producer error; `-1` generic (null self, bad input, string
  errors); `-2` producer panic; `1` invalid argument from marshalling.

## Per-generator work (one agent per target)

Each generator crate must implement ALL of the following. Use
`crates/weaveffi-gen-python` and `crates/weaveffi-gen-swift` as references
once they are updated (they are being updated first, by the same spec).

### 1. Typed error surface (`render_error` hook or equivalent)

For each module where `module.error` is `Some(eb)` **and `eb.declared_here`**:

- Emit one typed error construct named `eb.type_name` (already PascalCase with
  exactly one `Error` suffix). Targets that brand exceptions (Kotlin, .NET,
  Dart) rename via `weaveffi_core::errors::exception_type_name(&eb.name)`,
  which replaces a trailing `Error` stem (`KvError` → `KvException`) instead
  of stacking suffixes.
- Code names are validated to be unique across every domain in the API
  (`ValidationError::DuplicateErrorCodeName`), so flat per-code class or
  constant names cannot collide across domains.
- One case/subclass/constant per `ErrorCodeBinding`, carrying `value` and
  default `message`; attach `doc` as a doc comment. Case naming: use the
  target's convention (Swift enum case: `c.name` in lowerCamel via heck;
  Kotlin/C#/Dart subclass or enum member: Pascal; Python exception subclass:
  `errors::type_name(&c.name, "Error")`-style Pascal class or an enum —
  match the existing flattened-error pattern of the target, but scoped to the
  domain type instead of one global bucket).
- Keep the generic brand type (`WeaveFFIError` / `WeaveFFIException`, from
  `weaveffi_core::errors::{ERROR_BRAND, EXCEPTION_BRAND}`) for unknown codes,
  marshalling failures, and panics.
- DELETE the old "flatten every domain in the API into one global error list"
  surface (`weaveffi_core::errors::all(api)` consumers). Error codes now come
  from `ModuleBinding.error` only. Note `errors::all` / `errors::has_domains`
  stay in core for now (the model uses `errors::type_name`), just stop calling
  the flattened list from generators.

### 2. Throwing vs non-throwing wrappers

- A callable with `throws == true` keeps the target's error idiom (Swift
  `throws`, Python `raise`, Go `(T, error)`, Kotlin/C#/Dart/C++/JS exception,
  Ruby `raise`), and the error it surfaces is the *module's domain type*:
  map `err.code` to the matching domain case; fall back to the generic brand
  error for unknown codes.
- A callable with `throws == false` has a PLAIN signature (no `throws`, no
  `error` return). It still checks `out_err` after the call (the slot is
  always present at the ABI level); a non-zero code can only be a producer
  panic or an argument-marshalling failure, and surfaces as the target's
  unrecoverable idiom:
  - Swift: `fatalError("\(code): \(message)")`
  - Go: `panic(...)`
  - Python: `raise WeaveFFIError(code, message)` (unchecked by convention)
  - Kotlin: `throw WeaveFFIException(...)`; C#: `throw WeaveFFIException`;
    Dart: `throw WeaveFFIException`; JS/TS: `throw WeaveFFIError`; Ruby:
    `raise WeaveFFI::Error`; C++: `throw weaveffi::Error`.
- Async callables: same split applied to the future/promise/continuation
  (a non-throwing async Swift fn is `async` but not `throws`, etc.).
- Where the target generates a shared `check(err)` helper, split it into the
  throwing flavor (maps to domain type; one mapping helper per declaring
  module, e.g. `checkKv`) and the trapping flavor.

### 3. Interfaces (`render_interface` hook or equivalent)

Emit a class per `InterfaceBinding`, following the target's EXISTING opaque
struct-wrapper pattern (private ptr/handle + destructor wiring):

- Destructor: call `i.destroy_symbol` from the same disposal hook the target
  already uses for structs (Swift `deinit`, Python `__del__`, Kotlin
  `AutoCloseable`/cleaner, C# `IDisposable`/finalizer, Node `FinalizationRegistry`,
  Go `runtime.SetFinalizer` or explicit `Close`, Ruby finalizer, Dart
  `NativeFinalizer`, C++ RAII destructor).
- Constructors (`i.constructors`): C symbol is in the member's shape
  (`CallShape::Sync(abi).symbol`); it returns an owned `{c_tag}*`. A
  constructor named `new` becomes the canonical constructor where the
  language has one (Swift `init`, Python `__init__`, Kotlin constructor or
  companion `invoke`, C# constructor); every other constructor becomes a
  static factory method (idiomatic casing). If the language cannot express a
  fallible canonical constructor cleanly, a static factory for all
  constructors is acceptable, but keep it consistent within the target.
- Methods: instance methods. The ABI call passes the wrapper's pointer as the
  leading `self` argument, then the marshalled `FnBinding.params` slots, then
  out-params/`out_err` exactly like a free function. Reuse the free-function
  marshalling code path (the `AbiFn` already includes the `self` slot, so
  slot-driven emitters mostly work unchanged; name-driven emitters must skip
  the `self` slot when binding logical params).
- Statics: static/class methods on the wrapper type.
- Iterator- and async-shaped members follow the same shapes as free functions
  (launcher carries the self slot).
- `TypeRef::Interface(name)` as a param: pass the wrapper's raw pointer
  (borrow; the callee never takes ownership). As a return: wrap the owned
  pointer in a new wrapper instance. In type-mapping code, treat
  `Interface(name)` like the existing opaque `Struct(name)` reference for
  pointer plumbing, but construct/accept the interface class.
  `local_type_name()` applies (cross-module refs are dotted).

### 4. Idiomatic naming

- Function/method/static/constructor-factory names: the target's conventional
  casing, from `heck`:
  - snake_case: Python, Ruby, C++ (functions), C (already the ABI itself)
  - lowerCamelCase: Swift, Kotlin, JS/TS, Dart, Java
  - PascalCase: Go, C#
- Parameters keep IDL spelling except where the target strongly prefers
  camelCase (Swift, Kotlin, JS/TS, Dart: camel-case them).
- Swift: use real argument labels — `func openStore(path: String)`, NOT
  `func openStore(_ path: String)`. Methods likewise. (Keep `_` only where a
  label would be redundant per Swift conventions if the generator already has
  such logic; default is labeled.)
- Module-prefix stripping is THE DEFAULT everywhere: inside a module
  namespace (Swift `enum Kv`, Kotlin `object Kv`, C# `static class Kv`, C++
  `namespace kv`, Ruby `module Kv`) emit `openStore`, never `kvOpenStore`.
  Flat targets (Python, Go single package, Node flat exports) drop the module
  prefix too; the per-target `strip_module_prefix`-style config option flips
  back to prefixed names (`Option<bool>` or bool defaulting to true — keep
  the option, flip the default). Type names were never prefixed; unchanged.
- Enum members, struct fields, and property getters: keep the target's
  current behavior (no churn beyond what naturally falls out).

### 5. Model-only consumption

- The production path must consume the driver-built `BindingModel` passed to
  `LanguageBackend::files` — remove any `BindingModel::build` call in
  non-test code of the generator crate (thin public `Api`-based wrappers used
  only by that crate's tests may stay, or move into `#[cfg(test)]`).
- Anywhere the generator iterates `module.functions`, decide whether it must
  now iterate `module.callables()` (e.g. "does the file need an async
  runtime import" checks) or handle interfaces separately.

### 6. Tests and consumers (per generator agent)

- Update the generator crate's own unit tests for: one interface fixture
  (constructor + method + static + destroy), one typed-error fixture
  (declared domain, throwing and non-throwing fns), naming (stripped default
  + casing), and keep existing coverage green (`cargo test -p weaveffi-gen-<t>`).
- Update `crates/weaveffi-cli/tests/cli_<target>.rs` expectations if that file
  asserts on generated text (fixtures have changed; regenerate expectations by
  running the CLI on the fixtures and reading the real output).
- Update `conformance/<target>/*` consumers for the new sample surfaces
  (interfaces on kvstore/contacts, typed errors, new naming). Generate the
  bindings locally to check exact API shape:
  `cargo run -p weaveffi-cli -- generate samples/kvstore/kvstore.yml --target <t> -o /tmp/out-<t>`
- Do NOT regenerate `crates/weaveffi-cli/tests/snapshots/` — the driver
  regenerates all snapshots at the end (they'd conflict across agents).
- Do NOT edit shared core files (`weaveffi-core`, `weaveffi-ir`) — report
  gaps instead.

## Sample surfaces after the rewrite (fixtures match)

- `calculator`: unchanged functions, plus `errors: CalcError` with
  `DivisionByZero = 1`; `div` gains `throws: true` (traps → typed error).
- `contacts`: `Contact` struct and `ContactType` enum unchanged; the
  handle-based functions are replaced by an interface:
  `ContactBook` with constructor `new`, methods `add(first_name, last_name,
  email?, contact_type) -> Contact` (throws `InvalidName = 1`),
  `get(id: i64) -> Contact` (throws `NotFound = 2`), `list() -> [Contact]`,
  `remove(id: i64) -> bool`, `count() -> i32`; domain `ContactsError`.
- `inventory`: `products` module gains interface `Catalog`
  (ctor `new`, methods `add_product(name, price, category) -> Product`
  (throws `InvalidPrice = 1`), `get_product(id: i64) -> Product` (throws
  `ProductNotFound = 2`), `search(category) -> [Product]`,
  `update_price(id, price) -> bool` (throws), `remove(id) -> bool`; domain
  `ProductsError`); `orders` module keeps free functions + structs, gains
  domain `OrdersError { OrderNotFound = 1, EmptyOrder = 2 }` and `throws` on
  `get_order`/`create_order`. Code names are qualified because they must be
  unique across the whole API.
- `kvstore`: `Store` becomes an interface owning its state (no registry):
  ctor `open(path)` (throws), methods `put/get/delete` (throw), `list_keys ->
  iter<string>` (throws), `count -> i64`, `clear`, async cancellable
  `compact -> i64` (throws), deprecated `legacy_put` (throws), static
  `default_capacity() -> i64`. `Entry` record + builder unchanged; callbacks/
  listeners unchanged; `KvError` codes renamed to PascalCase (`KeyNotFound =
  1001`, `Expired = 1002`, `StoreFull = 1003`, `IoError = 1004`); the nested
  `stats` module keeps `get_stats(store: Store) -> Stats` where `Store` is
  the parent module's interface (cross-module interface param).
- `async-demo`: gains `errors: TaskError { InvalidName = 1 }`; `run_task`
  gains `throws: true`; everything else unchanged.
- `events`, `shapes`: unchanged (no domains, no throws).

CLI fixtures `crates/weaveffi-cli/tests/fixtures/*.yml` mirror the samples
(01=calculator, 02=contacts, 03=inventory, 04=async_demo, 05=events,
08=kvstore, 10=shapes) plus kitchen-sink/docs/nested fixtures updated to
carry at least one interface and per-module domains where they use `throws`.
