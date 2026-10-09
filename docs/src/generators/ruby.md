# Ruby

The Ruby target generates a pure-Ruby gem that binds the C ABI (revision 4)
through the [ffi](https://github.com/ffi/ffi) gem. There's no native
extension to compile: the gem depends on `ffi ~> 1.15` and loads the
producer's shared library at `require` time. Ruby 2.7 or newer is required.

## What gets generated

For a package named `kvstore` (prefix `kvstore`, library `kvstore`):

```text
ruby/
  kvstore.gemspec         gem `kvstore`, depends on ffi ~> 1.15
  README.md
  lib/
    kvstore.rb            require 'kvstore': module Kvstore, the bindings
    kvstore/
      runtime.rb          loader, load-time checks, errors, codec, object base
```

Names come from the package identity:

| Name | Default | Override (`[generators.ruby]`) |
|------|---------|--------------------------------|
| Gem | the package name (`key-store`) | `name` |
| Ruby module | the package name in PascalCase (`KeyStore`) | `module_name` |
| `require` path | the C prefix (`key_store`) | none |

Every IDL module, nested ones included, lands in the one Ruby module, and a
function keeps its bare name (`render_report` in module `report`), which
global names keep collision-free.

`lib/{prefix}/runtime.rb` is fixed code shipped with the generator (the
source lives in `targets/ruby/runtime/runtime.rb`); only the module name,
the C prefix, and the library names are substituted into it.

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
implements C ABI revision 4, and then that every declaration the bindings
were generated with is in its top-level module's
[contract table](../reference/abi.md#load-time-checks) with the same
signature. The expected entries are embedded in the bindings:

```ruby
module Kvstore
  _wv_check_contract!(
    kvstore_kv_contract: [
      [0x0969575bfbb012d7, 0xebd38766e3532c4f, 'kv.Store.fork'],
      # ...one entry per declaration in `kv` and its submodules
    ],
    kvstore_report_contract: [
      [0xa21a2e7274bf28c6, 0x342e0b83b12d73ec, 'report.render_report'],
      [0xd504fae45f64ab45, 0xd430b8de9d574a4c, 'report.ReportError'],
    ],
  )
```

A failed check raises `LoadError` naming the declaration
(`kvstore: kv.Store.put is missing from the library` or
`kvstore: kv.Store.put changed since these bindings were generated`).
Declarations the library has and the bindings don't are fine, so a library
that only adds to its API keeps working with older bindings.

## Type mapping

| IDL type | Ruby type | C slots |
|----------|-----------|---------|
| `i8` to `i64`, `u8` to `u64` | `Integer` | `:int8` to `:uint64` |
| `f32`, `f64` | `Float` | `:float`, `:double` |
| `bool` | `true` or `false` | `:bool` |
| `string` | `String` (UTF-8) | `:pointer` + `:size_t` |
| `bytes` | `String` (binary) | `:pointer` + `:size_t` |
| C-style enum | `Integer` constants in a module | `:int32` |
| record | plain value class with keyword `initialize` and `==` | value buffer |
| rich enum | base class with one nested class per variant | value buffer |
| `T?` | `T` or `nil` | value buffer |
| `[T]`, `{K: V}` | `Array`, `Hash` | value buffer |
| interface | wrapper class | `:pointer` |
| `Interface?` | wrapper or `nil` | `:pointer` (NULL for `nil`) |
| callback interface | any object with the methods | context + vtable |
| `Cb?` | such an object or `nil` | context + vtable (NULL for `nil`) |
| `iter<T>` | lazy `Enumerator` | iterator handle |

Trailing optional parameters (`T?`, `Interface?`, `Cb?`) default to `nil`,
so `store.keys` is `store.keys(nil)`.

Strings cross as a pointer plus a length, so interior NUL characters
survive. A string argument must be a `String` (or convert with `to_str`)
holding valid UTF-8; anything else raises `TypeError` or `ArgumentError`
before the call. Returned strings are copied, tagged UTF-8, and released
with `{prefix}_free_bytes`. Integers written into a value buffer are
range-checked (`RangeError` rather than silent wrapping), and an object
argument must be a wrapper of the declared interface (`TypeError`
otherwise).

Value buffers use the [value-buffer format](../reference/value-buffers.md),
encoded and decoded by the runtime's `WvBufferWriter` and `WvBufferReader`
and by generated codec methods: one `_wv_write_*`/`_wv_read_*` pair per
record and rich enum, and one per distinct composite type the API uses
(`[Entry?]` is `_wv_write_list_opt_entry`, `{string:string}` is
`_wv_write_map_string_string`), so no call site inlines a loop.

## Objects and lifetime

Each interface becomes a subclass of the runtime's `WvObject`, holding
one strong reference in an `FFI::AutoPointer`:

```ruby
  # An embedded key-value store.
  class Store < WvObject
    WV_PTR = StorePtr

    # Create an empty store with the path `memory`.
    def initialize
```

- `close` releases the reference now and is idempotent; otherwise the GC
  finalizer releases it. The object itself lives until its last reference
  anywhere (another wrapper, a record field, the library) goes.
- `dup` and `clone` make an independent wrapper with its own reference.
- A constructor named `new` becomes `initialize`; other constructors are
  class methods (`Store.open`). An interface without one hides `new`.
- Every call pins its receiver and object arguments (`_wv_pin`), so a
  `close` from another thread or from a callback during the call takes
  effect only when the call returns.
- An object inside a value buffer carries its own reference, minted just
  before the call; an object decoded from one is adopted into a new
  wrapper.
- Using a closed wrapper raises `Error` ("Kvstore::Store used after close").
- A method spelled like one the wrapper relies on (`close`, `handle`,
  `initialize`, `class`, `dup`, `hash`, ...), or a static or factory named
  `allocate` or `name`, gains a trailing `_`: an IDL method `close` is
  `close_` ([reserved member names](../reference/naming.md#identifiers-in-generated-code)).

## Errors

Every error derives from `{Module}::Error`, which carries `code`. A module
with an error domain gets a domain class (`KvError < Error`) with one
nested class per code (`KvError::KeyNotFound`, carrying `CODE` and any
payload fields as attributes):

```ruby
begin
  store.get('missing')
rescue Kvstore::KvError::KeyNotFound => e
  e.code    # => 1001
  e.key     # => "missing"
  e.message # => "key not found: missing"
end
```

What a failure raises follows the
[trap policy](../guides/errors-and-memory.md#the-trap-policy):

| Call | Domain code | Runtime code (`-1` to `-4`) | `-5` |
|------|-------------|-----------------------------|------|
| declared `throws` | the code's class (`KvError::Expired`) | `Error` with that code and the library's message | `Cancelled` |
| not `throws` | not possible | `NativeBugError` | `Cancelled` |

`NativeBugError < Error` marks a bug in the library (a panic, an argument it
couldn't accept, a callback failure it let through a call that can't report
one), never an outcome to handle. Its message names the code and the
library's message, as in `native call failed with code -2: {the panic
message}`; a malformed value the library returned raises it with code `-3`.

## Async and cancellation

An async function blocks the calling thread until the library's completion
fires, then returns the result or raises. The wait is a `Queue#pop`, which
releases the GVL, so other threads keep running; run the call in a `Thread`
for concurrency. A cancellable function takes a `cancel:` keyword:

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
`Timeout`) still cancels the library's work through a private token. Each
async function has one completion trampoline constant, so nothing the
library may still call is ever garbage-collected.

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
library thread. Each interface has one static vtable (an `FFI::Struct` whose
`{size, flags, free}` header is followed by one trampoline per method). A
required callback parameter rejects `nil` with `TypeError`; an optional one
(`loader = nil`) passes a null vtable.

Arguments and returns follow the
[ABI](../reference/abi.md#callback-interfaces):

- Strings, bytes, and value buffers arrive as copies; an object argument
  arrives as a wrapper that adopted the reference the library passed, which
  the implementation may keep.
- A returned number, boolean, or C-style enum crosses by value (checked
  against its type's range); a returned object (`Store` or `Store?`) as a
  fresh reference the library adopts (`nil` is null, which the library
  rejects with `-3` where the return isn't optional); and a returned string,
  bytes, or value buffer (a record, a rich enum, an optional, a list, or a
  map) as a run allocated with `{prefix}_alloc`, which the library adopts.
- A method declared `throws` in the IDL may raise one of its module's
  domain error classes: the library receives its code, its message, and its
  fields as the payload, exactly as if the producer had raised it. Any other
  exception (including `NotImplementedError`, and a domain error from a
  method not declared `throws`) reaches the library as a callback failure
  (`-4`) with the exception's message. Nothing unwinds through C.

### Threads and the GVL

Every call into the library except the trivial runtime helpers (`_clone`,
`_destroy`, an iterator's `_destroy`, `{prefix}_free_bytes`, `{prefix}_alloc`,
`{prefix}_debug_live`, the error and cancel-token functions, and the
load-time checks) releases the GVL
(`blocking: true`), so a long call doesn't stall other Ruby threads. That's
also what lets a callback arrive while the call that triggered it is in
flight: a callback on the calling thread reacquires the GVL there, and one
from a library thread (such as the notifications `compact` sends from its
worker) runs on the ffi gem's callback thread, which needs the GVL too.

## Iterators

An `iter<T>` return is a lazy `Enumerator`. The library's iterator starts
on the first pull, each step makes one `next` call, and the handle is
destroyed exactly once, when iteration finishes, raises, or stops early
(`first(2)`, `break`).

## Known limitations

- Async calls block the calling thread; there's no Fiber-scheduler or
  promise-based surface.
- An `Enumerator` driven externally with `next` and then abandoned
  releases its iterator only when the GC collects it.
- Callback interfaces are duck-typed: a missing method is detected only
  when the library calls it.
- A callback that blocks waiting for the thread that made the triggering
  call deadlocks.
- A deprecated callable warns on every call (`warn ... uplevel: 1`);
  silence it with `$VERBOSE = nil` like any Ruby warning.
