# Ruby

The Ruby target generates a pure-Ruby gem that binds the C ABI (revision 3)
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
      runtime.rb          loader, ABI check, errors, codec, object base
```

Names come from the package identity:

| Name | Default | Override (`[generators.ruby]`) |
|------|---------|--------------------------------|
| Gem | the package name (`async-demo`) | `gem_name` |
| Ruby module | the package name in PascalCase (`AsyncDemo`) | `module_name` |
| `require` path | the C prefix (`async_demo`) | none |

`strip_module_prefix` (default `true`) controls whether a function in
module `contacts` is spelled `create_contact` or `contacts_create_contact`.
Every IDL module, nested ones included, lands in the one Ruby module.

`lib/{prefix}/runtime.rb` is fixed code shipped with the generator (the
source lives in `targets/ruby/runtime/runtime.rb`); only the module name,
the C prefix, and the library names are substituted into it.

## Install, build, and load

```bash
weaveffi generate samples/kvstore/src/lib.rs -o generated --target ruby
cargo build --release -p kvstore
cd generated/ruby
gem build kvstore.gemspec
gem install kvstore-1.0.0.gem
```

The loader opens the library in this order:

1. the path in `{PREFIX}_LIBRARY` (`KVSTORE_LIBRARY=/path/to/libkvstore.dylib`),
2. a copy bundled at `lib/native/` (platform gems from `weaveffi package`),
3. `libkvstore.dylib`, `libkvstore.so`, or `kvstore.dll` on the system
   search path.

At `require` time the bindings then check that the library reports ABI
revision 3 and that every top-level module's contract checksum matches
the one the bindings were generated from. A mismatch raises `LoadError`
naming the module:

```ruby
module KitchenSink
  # Contract checksums of the top-level modules these bindings were
  # generated from; a library built from a different API fails to load.
  _wv_check_contract!(
    'shared' => [:kitchen_sink_shared_checksum, 0x42c4ce2c8d0af052],
    'kitchen' => [:kitchen_sink_kitchen_checksum, 0xd83da75b24b545ff],
  )
```

`weaveffi package --target ruby` writes one platform gem tree per desktop
binary under `ruby/<platform-id>/`, with the library at `lib/native/` and
`s.platform` set (`arm64-darwin`, `x86_64-darwin`, `x86_64-linux`,
`aarch64-linux`, `x64-mingw-ucrt`). Android and `wasm32` binaries are
skipped.

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
| `iter<T>` | lazy `Enumerator` | iterator handle |

Strings cross as a pointer plus a length, so interior NUL characters
survive. Returned strings are copied, tagged UTF-8, and released with
`{prefix}_free_bytes`. Value buffers use the
[value-buffer format](../reference/value-buffers.md), encoded and decoded
by the runtime's `WvBufferWriter` and `WvBufferReader` and one
`_wv_write_*`/`_wv_read_*` pair per record and rich enum.

## Objects and lifetime

Each interface becomes a subclass of the runtime's `WvObject`, holding
one strong reference in an `FFI::AutoPointer`:

```ruby
  # A pocket-sized interface exercising the object surface
  class Gadget < WvObject
    WV_PTR = GadgetPtr

    # @api private
    def self._wv_clone(ptr)
      KitchenSink.kitchen_sink_kitchen_Gadget_clone(ptr)
    end

    # Create a gadget with the given id
    def initialize(id)
```

- `close` releases the reference now and is idempotent; otherwise the GC
  finalizer releases it. The object itself lives until its last reference
  anywhere (another wrapper, a record field, the producer) goes.
- `dup` and `clone` make an independent wrapper with its own reference.
- A constructor named `new` becomes `initialize`; other constructors are
  class methods. An interface without one hides `new`.
- Every call pins its receiver and object arguments (`_wv_pin`), so a
  `close` from another thread or from a callback during the call takes
  effect only when the call returns.
- An object inside a value buffer carries its own reference, minted just
  before the call; an object decoded from one is adopted into a new
  wrapper.
- Using a closed wrapper raises `Error` ("KitchenSink::Gadget used after close").

## Errors

Every error derives from `{Module}::Error`, which carries `code`. A module
with an error domain gets a domain class (`KvError < Error`) with one
nested class per code (`KvError::KeyNotFound`, carrying `CODE` and any
payload fields as attributes). Functions that declare errors raise the
domain class; runtime failures raise the base classes:

| Code | Constant | Raised as |
|------|----------|-----------|
| -1 | `GENERIC_ERROR_CODE` | `Error` |
| -2 | `PANIC_ERROR_CODE` | `Error` (the producer panicked) |
| -3 | `MARSHAL_ERROR_CODE` | `Error` (a malformed argument or buffer) |
| -4 | `FOREIGN_ERROR_CODE` | `Error` (a callback implementation raised) |
| -5 | `CANCELLED_ERROR_CODE` | `Cancelled` |

## Async and cancellation

An async function blocks the calling thread until the producer's
completion fires, then returns the result or raises. The wait is a
`Queue#pop`, which releases the GVL, so other threads keep running; run
the call in a `Thread` for concurrency. A cancellable function takes a
`cancel:` keyword:

```ruby
  # Cancellable async operation
  # Blocks until the call completes on a producer thread.
  # @param cancel [CancelToken, nil] cancels the call; it then raises Cancelled
  def self.do_cancellable(input, cancel: nil)
    input_s = _wv_str(input)
    own_token = CancelToken.new if cancel.nil?
    token = cancel || own_token
    ctx, queue = _wv_async_begin
    kitchen_sink_kitchen_do_cancellable(input_s, input_s.bytesize, token._wv_ptr, KITCHEN_SINK_KITCHEN_DO_CANCELLABLE_CALLBACK, ctx)
    _wv_async_wait(queue, token)
  ensure
    own_token&.close
  end
```

`CancelToken#cancel` may be called from any thread; the call then raises
`Cancelled` unless it already completed. A token stays cancelled, can be
shared by several calls, and is released by `close` or the GC. Without a
token, interrupting the waiting thread (`Thread#raise`, `Timeout`) still
cancels the producer's work through a private token. Each async function
has one completion trampoline constant, so nothing the producer may still
call is ever garbage-collected.

## Callback interfaces

A callback interface becomes a Ruby module whose methods raise
`NotImplementedError`. Any object that responds to the methods works;
including the module documents intent and supplies the defaults.

```ruby
class Recorder
  include Events::Subscriber

  def route(topic)
    Events::Delivery::ACCEPT
  end
end
```

The implementation is stored in a handle table and the producer receives
its key, so Ruby object addresses never cross the boundary. The entry is
removed when the producer calls the vtable's `free`. The trampolines
copy borrowed strings and buffers, adopt object arguments, and report any
exception (including `NotImplementedError`) through
`{prefix}_error_set(out_err, -4, message)`; the caller sees
`Error` with code `-4` and the exception's message.

Producer threads may call back at any time: the ffi gem runs the
trampoline on a Ruby thread. In an API with callback interfaces every
call releases the GVL (`blocking: true`), so a producer thread can run a
callback while the call that triggered it is in flight.

## Iterators

An `iter<T>` return is a lazy `Enumerator`. The producer iterator starts
on the first pull, each step makes one `next` call, and the handle is
destroyed exactly once, when iteration finishes, raises, or stops early
(`first(2)`, `break`).

## Known limitations

- Async calls block the calling thread; there's no Fiber-scheduler or
  promise-based surface.
- An `Enumerator` driven externally with `next` and then abandoned
  releases its iterator only when the GC collects it.
- Callback interfaces are duck-typed: a missing method is detected only
  when the producer calls it.
- A callback that blocks waiting for the thread that made the triggering
  call deadlocks.
- Every IDL module shares one Ruby module, so two modules can't both
  declare a function or type with the same Ruby name.
