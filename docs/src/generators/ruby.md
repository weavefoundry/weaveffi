# Ruby

The Ruby target generates a pure-Ruby gem that binds the C ABI (revision 5)
through the [ffi](https://github.com/ffi/ffi) gem. There's no native
extension to compile: the gem depends on `ffi ~> 1.16` and loads the
producer's shared library at `require` time. Ruby 3.2 or newer is required
(the gemspec says so in `required_ruby_version`).

Ruby is a [Tier 2](../stability.md#target-tiers) target: it may lag a new
ABI revision for a while, but it passes the same conformance suite as every
other target before a release.

## What gets generated

For a package named `kvstore` (prefix `kvstore`, library `kvstore`):

```text
ruby/
  kvstore.gemspec         gem `kvstore`, Ruby >= 3.2, depends on ffi ~> 1.16
  README.md
  lib/
    kvstore.rb            require 'kvstore': module Kvstore, the bindings
    kvstore/
      runtime.rb          loader, load-time checks, errors, marshalling
```

Names come from the package identity:

| Name | Default | Override (`[generators.ruby]`) |
|------|---------|--------------------------------|
| Gem | the package name (`key-store`) | `name` |
| Ruby module | the package name in PascalCase (`KeyStore`) | `module_name` |
| `require` path | the C prefix (`key_store`) | none |

Every IDL module, nested ones included, lands in the one Ruby module, and a
function keeps its bare name (`render_report` in module `report`). Type and
function names are global in the IDL (validation rejects duplicates), so
nothing collides, and an API with one module (most of them) doesn't stutter
(`Calculator.add`, not `Calculator::Calculator.add`).

The module's public surface is only the idiomatic API: error classes, enum
constants, records, rich enums, interface classes, callback-interface
modules, and module functions. The C functions are attached in a private
`Native` module, and the marshalling the wrappers share lives in a private
`Bridge` module (both are `private_constant`, so `Kvstore::Native` raises
`NameError`). `lib/{prefix}/runtime.rb` is fixed code shipped with the
generator (the source lives in `targets/ruby/runtime/runtime.rb`); only the
module name, the C prefix, and the library names are substituted into it.

A wrapper converts its arguments and hands the C call to one runtime helper
as a block, so the generated code stays short:

```ruby
def get(key)
  key_ptr = Bridge.string_arg(key)
  Bridge.pin(self) do |self_|
    Bridge.call(KvError, [:buffer, Entry]) do |out_len, out_err|
      Native.kvstore_kv_Store_get(self_, key_ptr, key_ptr.bytesize, out_len, out_err)
    end
  end
end
```

`Bridge.call` allocates the error and out slots, raises per the function's
error domain, and receives the result (here, decodes an `Entry` from the
returned value buffer and releases it); `Bridge.pin` keeps the receiver
alive for the call.

## Install, build, and load

```bash
weaveffi generate samples/kvstore -o bindings --target ruby
cargo build --release -p kvstore
cd bindings/ruby
gem build kvstore.gemspec
gem install kvstore-1.0.0.gem
```

The loader opens the library in this order:

1. the path in `{PREFIX}_LIBRARY` (`KVSTORE_LIBRARY=/path/to/libkvstore.dylib`),
2. a copy bundled at `lib/native/` (platform gems from `weaveffi package`),
3. `libkvstore.dylib`, `libkvstore.so`, or `kvstore.dll` on the system
   search path.

```ruby
require 'kvstore'

store = Kvstore::Store.open('/tmp/data')
store.put('greeting', 'hi'.b, Kvstore::EntryKind::VOLATILE)
store.close
```

`weaveffi package --target ruby` writes one precompiled platform gem per
desktop platform built, `ruby/{gem}-{version}-{platform}.gem` (`arm64-darwin`,
`x86_64-darwin`, `x86_64-linux`, `aarch64-linux`, `x64-mingw-ucrt`), with the
library at `lib/native/`. The gems install with `gem install` and publish
with `gem push`; Android, iOS, and `wasm32` builds are skipped. See
[Packaging](../guides/packaging.md).

## Load-time checks

`require` checks, before anything else is attached, that the library
implements C ABI revision 5, and then that every declaration the bindings
were generated with is in its top-level module's
[contract table](../reference/abi.md#load-time-checks) with the same
signature. The expected rows are embedded in the bindings, each with its
canonical signature as a comment:

```ruby
module Kvstore
  Bridge.check_contract!(
    kvstore_kv_contract: [
      # method get(string) -> Entry throws KvError
      [0x..., 0x..., 'kv.Store.get'],
      # ...one row per declaration, error code, and callback method in `kv`
    ],
    kvstore_report_contract: [
      # ...
    ],
  )
```

Any failure raises `Kvstore::LoadError`, a subclass of Ruby's `LoadError`,
so both `rescue Kvstore::LoadError` and a plain `rescue LoadError` around
the `require` catch it: a library that can't be opened, a different ABI
revision, a missing contract table, or a declaration that's missing or
changed (`kvstore: kv.Store.put is missing from the library`,
`kvstore: kv.Store.put changed since these bindings were generated`).
Declarations, error codes, and callback methods the library has and the
bindings don't are fine, so a library that only adds to its API keeps
working with older bindings. A failed `require` isn't recorded as loaded,
so it can be retried (after setting `{PREFIX}_LIBRARY`, for example).

## Type mapping

| IDL type | Ruby type | C slots |
|----------|-----------|---------|
| `i8` to `i64`, `u8` to `u64` | `Integer` | `:int8` to `:uint64` |
| `f32`, `f64` | `Float` | `:float`, `:double` |
| `bool` | `true` or `false` | `:bool` |
| `string` | `String` (UTF-8) | `:pointer` + `:size_t` |
| `bytes` | `String` (binary) | `:pointer` + `:size_t` |
| C-style enum | `Integer` constants in a module | `:int32` |
| record | `Data` class | value buffer |
| rich enum | module of `Data` variant classes | value buffer |
| `T?` of a scalar, `bool`, or C-style enum | `T` or `nil` | `:bool` + `T` (no buffer) |
| `[T]` of a numeric scalar (not `u8`, not `bool`) | `Array` of `Integer` or `Float` | `:pointer` + `:size_t` (a typed array) |
| any other `T?`, `[T]`, `{K: V}` | `T` or `nil`, `Array`, `Hash` | value buffer |
| interface | wrapper class | `:pointer` |
| `Interface?` | wrapper or `nil` | `:pointer` (NULL for `nil`) |
| callback interface | any object with the methods | context + vtable |
| `Cb?` | such an object or `nil` | context + vtable (NULL for `nil`) |
| `iter<T>` | lazy `Enumerator` | iterator handle |

Trailing optional parameters (`T?`, `Interface?`, `Cb?`) default to `nil`,
so `store.keys` is `store.keys(nil)`.

Arguments are checked before the call:

- An integer (or a C-style enum value) must be an `Integer` in its type's
  range: `RangeError` rather than silent wrapping, `TypeError` for a
  `Float`. That holds for scalars, optional scalars, typed-array elements,
  and integers inside value buffers.
- A string must be a `String` (or convert with `to_str`) holding valid
  UTF-8; a string in another encoding is converted. Anything else raises
  `TypeError` or `ArgumentError`. Strings cross as a pointer plus a length,
  so interior NUL characters survive. Returned strings are copied, tagged
  UTF-8, and released.
- A typed-array argument must be an `Array` (or convert with `to_ary`); its
  range-checked elements are packed into an aligned native array
  (`FFI::MemoryPointer#put_array_of_*`) the library borrows for the call. A
  typed-array return is copied into a new `Array` and released.
- An optional scalar crosses as a presence flag plus the value, never
  through a value buffer: `nil` in, `nil` out.
- An object argument must be a wrapper of the declared interface, and a
  record argument an instance of the record's class (`TypeError`
  otherwise).

Records are `Data` classes: immutable, constructed with keywords or
positionally, with value equality and hashing, `with`, `to_h`, and pattern
matching:

```ruby
entry = Kvstore::Entry.new(key: 'k', value: 'v'.b, kind: Kvstore::EntryKind::VOLATILE,
                           version: 1, expires_at: nil, tags: [], metadata: {})
entry.with(version: 2)
```

A rich enum is a module whose variants are `Data` classes that include it,
so `change.is_a?(Kvstore::Change)` holds and `case change in
Kvstore::Change::Put(entry:)` works; `change.tag` (and each variant's `TAG`)
is the wire tag.

Value buffers use the [value-buffer format](../reference/value-buffers.md).
Nothing per composite type is generated: the runtime's codec walks a
literal type description (`[:list, [:opt, Entry]]`, `[:map, :string, :i64]`),
and each record, variant, and error payload registers its field layout
once, at the end of the bindings (`Bridge.record(Entry, key: :string, ...)`).

A field named like a method a record or error relies on gains a trailing
`_` (`hash_`, `to_h_`; an error field `message` is `message_`), and so does
a type named like a constant the bindings use (`Error_`, `Data_`, `FFI_`);
keywords are fine as member names (`entry.end`). See
[reserved member names](../reference/naming.md#identifiers-in-generated-code).

## Objects and lifetime

Each interface becomes a subclass of the runtime's `Bridge::Handle`, holding
one strong reference in an `FFI::AutoPointer`:

```ruby
  # An embedded key-value store. Each object owns its entries, logical
  # clock, listeners, and policy; the last release drops them (and frees
  # the consumer's callbacks).
  class Store < Bridge::Handle
```

- `close` releases the reference now and is idempotent; otherwise the GC
  finalizer releases it. The object itself lives until its last reference
  anywhere (another wrapper, a record field, the library) goes.
- `closed?` says whether the wrapper was closed.
- Two open wrappers of the same native object are `==` (and `eql?`, with
  equal `hash`), so a record holding objects compares as expected.
- `dup` and `clone` make an independent wrapper with its own reference.
- A constructor named `new` becomes `initialize`; other constructors are
  class methods (`Store.open`). An interface without one hides `new`.
- Every call pins its receiver and object arguments, so a `close` from
  another thread or from a callback during the call takes effect only when
  the call returns.
- An object inside a value buffer carries its own reference, minted just
  before the call; an object decoded from one is adopted into a new
  wrapper.
- Using a closed wrapper raises `Error` ("Kvstore::Store used after close").
- A method spelled like one the wrapper relies on (`close`, `initialize`,
  `class`, `dup`, `hash`, `inspect`, ...), or a static or factory named
  `allocate`, `new`, or `name`, gains a trailing `_`: an IDL method `close`
  is `close_`.

## Errors

Every error a call reports derives from `{Module}::Error` (a
`StandardError`), which carries `code`. Each error
domain becomes a class under it, named by the shared rule (`KvError` stays
`KvError`, `KitchenErrors` becomes `KitchenError`, `Failure` becomes
`FailureError`), with one nested class per code. A code's class carries
`CODE`, its documented `MESSAGE`, and a reader per payload field:

```ruby
begin
  store.get('missing')
rescue Kvstore::KvError::KeyNotFound => e
  e.code    # => 1001
  e.key     # => "missing"
  e.message # => "key not found: missing"
end
```

A module may declare several domains, and a function names the one it
throws, so codes are only unique per domain: `Calculator.divide` raises
`CalcError::DivisionByZero` for code 1 and `Calculator.parse` raises
`ParseError::NotANumber` for its code 1. Domains are open: a positive code
these bindings don't know (a newer library added it) raises the domain
class itself, with the code and the library's message.

What a failure raises follows the
[trap policy](../guides/errors-and-memory.md#the-trap-policy):

| Call | Domain code | Runtime code (`-1` to `-4`) | `-5` |
|------|-------------|-----------------------------|------|
| `throws: Domain` | the code's class (`KvError::Expired`), or the domain class for an unknown code | `Error` with that code and the library's message | `Cancelled` |
| `throws: any` | not possible | `Error` (a failure is code `-1` and its message) | `Cancelled` |
| no `throws` | not possible | `NativeBugError` | `Cancelled` |

`NativeBugError < Error` marks a bug in the library (a panic, an argument it
couldn't accept, a callback failure it let through a call that can't report
one), never an outcome to handle. Its message names the code and the
library's message, as in `native call failed with code -2: {the panic
message}`; a malformed value the library returned raises it with code `-3`.
The runtime codes are constants on `Error` (`Error::GENERIC`,
`Error::PANIC`, `Error::MARSHAL`, `Error::CALLBACK`, `Error::CANCELLED`).

An error code's class can be raised from a callback implementation; its
payload fields are required keywords:

```ruby
raise Kvstore::KvError::Rejected.new('read-only', key: entry.key, reason: 'read-only')
```

## Async and cancellation

An async function blocks its caller until the library's completion fires,
then returns the result or raises. The wait is a `Thread::Queue#pop`, so:

- It releases the GVL; other threads keep running. Run calls in threads
  for concurrency.
- Under a Fiber scheduler (`Fiber.set_scheduler`, such as the `async` gem's),
  `Queue#pop` in a non-blocking Fiber yields to the scheduler, so only the
  calling Fiber waits and the thread's other Fibers keep running; the
  completion wakes it through the scheduler's `unblock`. The conformance
  suite checks this with two overlapping calls.

A cancellable function takes a `cancel:` keyword:

```ruby
token = Kvstore::CancelToken.new
Thread.new { sleep 0.5; token.cancel }
begin
  store.compact(60_000, cancel: token)
rescue Kvstore::Cancelled
  # cancelled while the pause was running
ensure
  token.close
end
```

`CancelToken#cancel` may be called from any thread; the call then raises
`Cancelled` (code `-5`) unless it already completed. A token stays
cancelled, can be shared by several calls, and is released by `close` or
the GC. Without a token, interrupting the waiting thread (`Thread#raise`,
`Timeout`) still cancels the library's work through a private token. The
completion functions are shared by every async call with the same result
slots and live for the process, so nothing the library may still call is
ever garbage-collected.

Optional scalars and typed arrays arrive as async results directly (a flag
and a value, or a typed array the runtime copies and releases).

## Callback interfaces

A callback interface becomes a Ruby module whose methods raise
`NotImplementedError`. Any object that responds to the methods works;
including the module documents intent and supplies the defaults.

```ruby
class FileLoader
  include Kvstore::Loader

  def name
    'files'
  end

  def fallback(_key)
    nil
  end

  def load(key)
    path = File.join('/data', key)
    raise Kvstore::KvError::KeyNotFound.new("no file for #{key}", key: key) unless File.exist?(path)

    File.binread(path)
  end
end

entry = store.get_or_load('settings', FileLoader.new)
```

Passing an implementation stores it in a handle table and hands the library
its key, so Ruby object addresses never cross the boundary; the entry is
removed when the library calls the vtable's `free`, which may happen on any
library thread. Each interface has one vtable for the life of the process
(the `{size, flags, free}` header, then one trampoline per method), with
`flags` 0: the methods may be called from any thread. A required callback
parameter rejects `nil` with `TypeError`; an optional one
(`loader = nil`) passes a null vtable.

Arguments and returns follow the
[ABI](../reference/abi.md#callback-interfaces):

- Strings, bytes, value buffers, and typed arrays arrive as copies; an
  optional scalar arrives as the value or `nil`; an object argument arrives
  as a wrapper that adopted the reference the library passed, which the
  implementation may keep.
- A returned number, boolean, C-style enum, or optional scalar crosses by
  value (checked against its type's range); a returned object (`Store` or
  `Store?`) as a fresh reference the library adopts (`nil` is null, which
  the library rejects where the return isn't optional); and a returned
  string, bytes, typed array, or value buffer as a run allocated with
  `{prefix}_alloc`, which the library adopts.
- A method that throws a domain may raise that domain's code classes: the
  library receives the code and the fields as the payload, exactly as if
  the producer had raised it (the caller then sees the domain's own message
  for those fields). Any other exception, including `NotImplementedError`,
  is reported as code `-1` with its message (`-4` for a method that declares
  no errors). Nothing unwinds through C.

### Threads and the GVL

Every call into the library except the trivial runtime helpers (`_clone`,
`_destroy`, an iterator's `_destroy`, `{prefix}_free_bytes`, `{prefix}_alloc`,
`{prefix}_debug_live`, the error and cancel-token functions, and the
load-time checks) releases the GVL (`blocking: true`), so a long call
doesn't stall other Ruby threads. That's also what lets a callback arrive
while the call that triggered it is in flight: a callback on the calling
thread reacquires the GVL there, and one from a library thread (such as the
notifications `compact` sends from its worker) runs on the ffi gem's
callback thread, which needs the GVL too.

## Iterators

An `iter<T>` return is a lazy `Enumerator`, with every `Enumerable` method.
Each enumeration launches its own native iterator on its first pull, makes
one `_next` call per element, and destroys the iterator exactly once, when
iteration finishes, raises, or stops early (`first(2)`, `break`), so the
same `Enumerator` can be enumerated again. An external enumeration (`next`)
releases its iterator the same way when it reaches the end or raises; one
abandoned midway can't run its `ensure`, so the iterator handle is an
`FFI::AutoPointer` whose GC finalizer destroys it once the suspended
enumeration is collected. Either path destroys the iterator exactly once.
Elements that are optional scalars or typed arrays arrive directly, like
returns.

## Known limitations

- Async calls block the calling thread (or, under a Fiber scheduler, the
  calling Fiber); there's no promise-based surface.
- Callback interfaces are duck-typed: a missing method is detected only
  when the library calls it (and reported to it as a failure).
- A callback that blocks waiting for the thread that made the triggering
  call deadlocks.
- A deprecated callable warns on every call in the `:deprecated` warning
  category, which Ruby hides by default; enable it with
  `Warning[:deprecated] = true` (or `ruby -W:deprecated`).
