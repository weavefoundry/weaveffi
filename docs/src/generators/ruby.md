# Ruby

## Overview

The Ruby target produces pure-Ruby FFI bindings using the
[ffi](https://github.com/ffi/ffi) gem to call the C ABI (revision 2)
directly. There's no native extension to compile; `gem install ffi` is
the only prerequisite. The generator emits a single `.rb` file plus a
`gemspec` ready for `gem build` and `gem install`.

Interfaces become reference-counted wrapper classes (`close` plus a GC
finalizer backstop, `dup`/`clone` for a second reference), records and
rich enums become value classes packed into value buffers (with
interfaces inside them carried as object tokens), callback interfaces
become duck-typed Ruby modules backed by one static vtable of pinned
`FFI::Function` trampolines per interface, async functions block on a
`Queue`, and `iter<T>` returns are lazy `Enumerator`s.

The trade-off is that FFI gem calls are slower than a hand-written C
extension. For typical FFI workloads the overhead is negligible compared
to the work done inside the Rust library.

## What gets generated

| File | Purpose |
|------|---------|
| `ruby/lib/weaveffi.rb` | FFI bindings: library loader, ABI check, `attach_function` declarations, codecs, wrapper classes |
| `ruby/weaveffi.gemspec` | Gem specification with `ffi ~> 1.15` dependency |
| `ruby/README.md` | Prerequisites and usage instructions |

The file names follow the gem name (IDL `package.name`): a package named
`kvstore` produces `lib/kvstore.rb`, `kvstore.gemspec`, and
`module Kvstore`; `weaveffi` is the default. The module verifies the
producer's ABI revision at `require` time:

```ruby
  ABI_VERSION = 2
  begin
    attach_function :weaveffi_abi_version, [], :uint32
  rescue FFI::NotFoundError
    raise LoadError, 'the loaded WeaveFFI library predates ABI versioning ' \
                     "(these bindings expect ABI revision #{ABI_VERSION})"
  end
  _wv_abi = weaveffi_abi_version
  unless _wv_abi == ABI_VERSION
    raise LoadError, "WeaveFFI ABI mismatch: these bindings expect revision #{ABI_VERSION} " \
                     "but the loaded library reports revision #{_wv_abi}"
  end
```

## Type mapping

| IDL type     | Ruby type          | FFI type                       |
|--------------|--------------------|--------------------------------|
| `i8`, `i16`, `i32`, `i64` | `Integer` | `:int8`, `:int16`, `:int32`, `:int64` |
| `u8`, `u16`, `u32`, `u64` | `Integer` | `:uint8`, `:uint16`, `:uint32`, `:uint64` |
| `f32`, `f64` | `Float`            | `:float`, `:double`            |
| `bool`       | `true`/`false`     | `:int32` (0/1 conversion)      |
| `string`     | `String`           | `:string` (param) / `:pointer` (return) |
| `bytes`      | `String` (binary)  | `:pointer` + `:size_t`         |
| `Struct`     | `StructName` (plain class) | value buffer (`:pointer` + `:size_t`) |
| `Enum` (plain) | `Integer` (constants in a module) | `:int32`        |
| `Enum` (rich)  | `EnumName` (nested variant classes) | value buffer  |
| `Interface`  | `InterfaceName` (wrapper class) | `:pointer`        |
| `Interface?` | `InterfaceName` or `nil` | `:pointer` (NULL for `nil`) |
| `CallbackInterface` | any object responding to the methods (include the module) | `:pointer` ctx + vtable `:pointer` |
| `T?`         | `T` or `nil`       | value buffer                   |
| `[T]`        | `Array`            | value buffer                   |
| `{K: V}`     | `Hash`             | value buffer                   |
| `iter<T>`    | `Enumerator` (lazy) | `:pointer` iterator handle    |

Buffered types cross the boundary serialized in the
[value-buffer format](../reference/value-buffers.md); the module carries
private `WvBufferWriter`/`WvBufferReader` classes plus one
`_wv_write_*`/`_wv_read_*` pair per record and rich enum. Objects
nested inside a buffered value travel as object tokens (see
[Objects](#objects-interfaces)). Booleans cross as `:int32` (`0`/`1`);
the wrapper converts both directions.

### 64-bit integers and floats

Ruby's `Integer` is arbitrary precision, so `i64` and `u64` round-trip
exactly through the `:int64`/`:uint64` FFI types and the value-buffer
codec's `write_i64`/`write_u64` (`String#pack` with `q<`/`Q<`).
`f32`/`f64` map to `Float`; the `codec` conformance consumer verifies
NaN, both infinities, and `-0.0` survive a round trip bit-for-bit.

## Example IDL and generated code

```yaml
version: "0.9.0"
modules:
  - name: contacts
    enums:
      - name: ContactType
        variants:
          - { name: Personal, value: 0 }
          - { name: Work, value: 1 }
          - { name: Other, value: 2 }

    structs:
      - name: Contact
        doc: "A contact record"
        fields:
          - { name: id, type: i64 }
          - { name: first_name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }

    functions:
      - name: create_contact
        params:
          - { name: first_name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }
        return: Contact

      - name: list_contacts
        params: []
        return: "[Contact]"
```

The generated module extends `FFI::Library` and selects the right shared
library at load time:

```ruby
require 'ffi'

module WeaveFFI
  extend FFI::Library

  # An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a
  # specific build artifact regardless of its file name or location.
  _wv_override = ENV['WEAVEFFI_LIBRARY']
  if _wv_override && !_wv_override.empty?
    ffi_lib _wv_override
  else
    case FFI::Platform::OS
    when /darwin/  then ffi_lib 'libweaveffi.dylib'
    when /mswin|mingw/ then ffi_lib 'weaveffi.dll'
    else ffi_lib 'libweaveffi.so'
    end
  end
end
```

Enums become Ruby modules with constants:

```ruby
module ContactType
  PERSONAL = 0
  WORK = 1
  OTHER = 2
end
```

Structs become plain Ruby value classes: one `attr_reader` per field, a
keyword-argument initializer, and structural equality. They hold no
native pointer and declare no C symbols:

```ruby
# A contact record
class Contact
  attr_reader :id
  attr_reader :first_name
  attr_reader :email
  attr_reader :contact_type

  def initialize(id:, first_name:, email:, contact_type:)
    @id = id
    @first_name = first_name
    @email = email
    @contact_type = contact_type
  end

  # Structural equality over every field.
  def ==(other)
    # ...
  end
end
```

Functions are snake_case class methods on the module, with the IDL
module prefix stripped by default (a `kv.open_store` function surfaces
as `open_store`, not `kv_open_store`; the `attach_function` bindings
keep the full C symbol names). Set `strip_module_prefix: false` in the
Ruby generator config (or under `[global]`) to keep prefixed names. A
buffered return is copied out, freed, and decoded; from the `kvstore`
sample's cross-module `get_stats`:

```ruby
  def self.get_stats(store)
    err = ErrorStruct.new
    out_len = FFI::MemoryPointer.new(:size_t)
    result = weaveffi_kv_stats_get_stats(store.handle, out_len, err)
    check_kv_error!(err)
    len = out_len.read(:size_t)
    data = result.null? ? ''.b : result.read_string(len)
    weaveffi_free_bytes(result, len) unless result.null?
    _wv_r = WvBufferReader.new(data)
    _wv_value = _wv_read_stats(_wv_r)
    _wv_r.expect_end!
    _wv_value
  end
```

## Typed errors

The shared error machinery: an `ErrorStruct` mirroring the C error
slot, the four runtime trap codes as module constants, and a generic
`Error`:

```ruby
  class ErrorStruct < FFI::Struct
    layout :code, :int32,
           :message, :pointer,
           :payload_ptr, :pointer,
           :payload_len, :size_t
  end

  # The runtime-reserved error codes. Positive codes belong to a module's
  # declared error domain; these negative codes are programming errors the
  # wrappers raise as a plain Error regardless of the function's `throws`.
  GENERIC_ERROR_CODE = -1
  PANIC_ERROR_CODE = -2
  MARSHAL_ERROR_CODE = -3
  # A callback-interface implementation raised; the message carries the
  # exception text.
  FOREIGN_ERROR_CODE = -4

  class Error < StandardError
    attr_reader :code

    def initialize(code, message)
      @code = code
      super(message)
    end
  end
```

A module's error domain adds a base class extending `Error` with one
nested class per code, each pinning its stable `CODE`, plus a mapper
that falls back to the generic `Error` for codes outside the domain.
From the `kvstore` sample:

```ruby
# Base error for the `kv` module's error domain.
class KvError < Error
  # key not found
  class KeyNotFound < KvError
    CODE = 1001

    def initialize(message = 'key not found')
      super(1001, message)
    end
  end

  # Expired, StoreFull, IoError follow the same shape.
end

# Builds the KvError subclass matching `code`, or a generic Error
# for codes outside the domain (panics, marshalling).
def self.kv_error_from(code, message, payload = nil)
  cls = KV_ERROR_CODES[code]
  return Error.new(code, message) if cls.nil?
  message.empty? ? cls.new : cls.new(message)
end
```

Only callables marked `throws: true` in the IDL raise the typed
classes: their wrappers call `check_kv_error!`, so you can rescue
`Kvstore::KvError::KeyNotFound` for one code or `Kvstore::KvError` for
the whole domain. A callable without `throws` uses the generic
`check_error!`, which raises `Error` if the producer misbehaves. Both
copy the message (and payload) out and release the slot with
`weaveffi_error_clear` before raising.

An error code that declares payload `fields:` carries them serialized in
the error's payload buffer (`payload_ptr`/`payload_len` on
`ErrorStruct`); the domain factory decodes them into attributes on the
raised error object before the buffer is released.

```ruby
begin
  store.delete('missing')
rescue Kvstore::KvError::KeyNotFound
  # specific code
rescue Kvstore::KvError => e
  puts "kv error #{e.code}: #{e.message}"
end
```

### Runtime error codes

| Code | Constant | Meaning | Where it surfaces |
|------|----------|---------|-------------------|
| `-1` | `GENERIC_ERROR_CODE` | The producer reported an error without a declared code, or the wrapper itself detected misuse (a NULL object pointer, a wrapper used after `close`). | Raised as `Error`. |
| `-2` | `PANIC_ERROR_CODE` | The Rust implementation panicked; the export macros and the async spawner catch the unwind. | Raised as `Error` (a blocking async call re-raises it on the caller's thread). |
| `-3` | `MARSHAL_ERROR_CODE` | Malformed input at the boundary (invalid UTF-8, a truncated value buffer, a bad enum discriminant). | Raised as `Error`. |
| `-4` | `FOREIGN_ERROR_CODE` | A callback-interface method implemented in Ruby raised. | Raised as `Error` from the producer call that invoked the callback (see [Callback interfaces](#callback-interfaces)). |

There's no non-raising call path in Ruby: a non-throwing callable whose
error slot comes back non-zero still raises `Error`.

## Objects (interfaces)

An `interfaces:` entry becomes a class holding an `FFI::AutoPointer`
subclass that owns one strong reference to a reference-counted producer
object; the `AutoPointer`'s `release` hook calls the `_destroy` symbol
when Ruby garbage-collects it. From the `kvstore` sample (trimmed):

```ruby
  # Owns one strong reference to a Store; releases it exactly once.
  class StorePtr < FFI::AutoPointer
    def self.release(ptr)
      Kvstore.weaveffi_kv_Store_destroy(ptr)
    end
  end

  class Store
    # Adopts one strong reference the producer handed over, without
    # re-running initialize.
    def self._from_ptr(ptr)
      obj = allocate
      obj.instance_variable_set(:@handle, StorePtr.new(ptr))
      obj
    end

    # The borrowed object pointer passed to producer calls.
    def handle
      raise Error.new(-1, 'Store used after close') if @handle.nil?
      @handle
    end

    # Whether close has released this wrapper's reference.
    def closed?
      @handle.nil?
    end

    # Releases this wrapper's reference now rather than at GC time.
    # Idempotent; the object itself is dropped when the last reference
    # anywhere (another wrapper, a record field, the producer) goes.
    def close
      return if @handle.nil?
      @handle.free
      @handle = nil
    end

    # Mints a new strong reference the caller owns (used when this
    # object is written into a value buffer).
    def _wv_clone_ptr
      Kvstore.weaveffi_kv_Store_clone(handle)
    end

    # dup and clone produce an independent wrapper with its own reference.
    def initialize_copy(other)
      super
      @handle = StorePtr.new(Kvstore.weaveffi_kv_Store_clone(other.handle))
    end

    def self.open(path)
      err = ErrorStruct.new
      result = Kvstore.weaveffi_kv_Store_open(path, err)
      Kvstore.check_kv_error!(err)
      raise Error.new(-1, 'null pointer') if result.null?
      _from_ptr(result)
    end
  end
```

- **Construction.** A constructor named `new` becomes an ordinary
  `initialize`, so `EventBus.new` works as usual (the `events` sample);
  any other constructor is a class method (`Store.open(path)`). Methods
  are snake_case instance methods, statics are class methods
  (`Store.default_capacity`), and deprecated members print a
  `[DEPRECATED]` warning at call time.
- **Disposal.** `close` releases this wrapper's reference through the
  `_destroy` symbol now; it's idempotent, and `closed?` reports the
  state. If you never call `close`, the `FFI::AutoPointer` releases the
  reference when the wrapper is garbage-collected. The producer object
  itself is dropped only when the last reference anywhere is released.
- **Use after close.** Every call goes through `handle`, which raises
  `Error.new(-1, 'Store used after close')` on a closed wrapper, whether
  it's the receiver, a parameter, or a field of a record being packed.
- **Copies mint new references.** `dup` and `clone` are overridden via
  `initialize_copy` to mint a new strong reference, so the copy is an
  independent wrapper; closing one never affects the other. Methods that
  return the receiver or an existing object (`share`, `fork`) likewise
  return a fresh reference adopted into a new wrapper.

```ruby
store = Kvstore::Store.open('/tmp/cache.kv')
begin
  store.put('alpha', "\x01".b, Kvstore::EntryKind::PERSISTENT, nil)
  puts "#{store.count} / #{Kvstore::Store.default_capacity}"
  reclaimed = store.compact
ensure
  store.close
end
```

### Nullable objects, and objects inside values

An `Interface?` parameter passes `nil` as NULL (`other&.handle`), and an
`Interface?` return maps a NULL pointer to `nil`:

```ruby
    def larger(other)
      err = ErrorStruct.new
      result = Kvstore.weaveffi_kv_Store_larger(handle, other&.handle, err)
      Kvstore.check_error!(err)
      return nil if result.null?
      Store._from_ptr(result)
    end
```

Objects inside records, arrays, hashes, and optionals travel as 8-byte
object tokens in the value buffer. Writing a token mints a new strong
reference with `_wv_clone_ptr`; reading one adopts the reference into a
fresh wrapper. From the `StoreInfo` record (`store: Store`,
`mirror: Store?`):

```ruby
  def self._wv_write_store_info(w, v)
    w.write_string(v.label)
    w.write_u64(v.store._wv_clone_ptr.address)
    if v.mirror.nil?
      w.write_flag(false)
    else
      w.write_flag(true)
      w.write_u64(v.mirror._wv_clone_ptr.address)
    end
    w.write_i64(v.count)
  end

  def self._wv_read_store_info(r)
    _wv_label = r.read_string
    _wv_store = Store._from_ptr(r.read_object_token)
    if r.read_flag
      _wv_mirror = Store._from_ptr(r.read_object_token)
    else
      _wv_mirror = nil
    end
    _wv_count = r.read_i64
    StoreInfo.new(label: _wv_label, store: _wv_store, mirror: _wv_mirror, count: _wv_count)
  end
```

Arrays of objects work the same way in both directions
(`Store.open_many(paths)` returns an `Array` of `Store`,
`Store.total_count(stores, extra)` takes one); each wrapper in a returned
array owns its own reference and should be closed (or left to GC)
individually. Iterators over objects adopt one reference per pull, and a
blocking async call returning an object adopts the pointer inside the
completion callback.

## Rich (algebraic) enums

A rich (algebraic) enum is a sum type whose variants carry associated
data. A plain C-style `Enum` crosses as a bare `:int32` discriminant; a
rich enum instead becomes a plain class hierarchy: a base class with a
`tag` reader plus one nested subclass per variant carrying that
variant's fields as attributes, each pinning its `TAG` constant. Rich
enums declare no C symbols; values cross the ABI serialized in value
buffers as an `i32` tag followed by the active variant's fields. From
the `codec` sample:

```ruby
  class Shape
    # The active variant's integer tag.
    def tag
      self.class::TAG
    end

    # No payload.
    class Empty < Shape
      TAG = 0

      # Structural equality over the variant and its fields.
      def ==(other)
        return false unless other.is_a?(Empty)
        true
      end
    end

    # One `f64`.
    class Circle < Shape
      TAG = 1

      # Radius.
      attr_reader :radius

      def initialize(radius:)
        @radius = radius
      end

      # Structural equality over the variant and its fields.
      def ==(other)
        return false unless other.is_a?(Circle)
        # ...
      end
    end
  end
```

Construct variants directly and branch with `case`/`when` on the class:

```ruby
circle = Codec::Shape::Circle.new(radius: 2.0)

case circle
when Codec::Shape::Circle
  puts circle.radius                 # 2.0
when Codec::Shape::Empty
  puts 'empty'
end
```

Values are plain Ruby data; there's no native handle and nothing to
close. The private `_wv_write_shape`/`_wv_read_shape` codec helpers
write and read the tagged wire format; variant fields of interface type
follow the object token rules above.

## Callback interfaces

A `callback_interfaces:` entry becomes a Ruby module. Any object that
responds to the methods is accepted (duck typing); including the module
gives you `NotImplementedError` defaults for the methods you don't
override. From the `kvstore` sample:

```ruby
  # Consumer-implemented callback interface. Any object responding to the
  # methods below is accepted wherever a EvictionListener parameter is expected;
  # include this module to inherit NotImplementedError defaults. The
  # producer may call the methods from any thread until it releases the
  # implementation.
  module EvictionListener
    # An entry left the store. Returns whether the listener wants to keep
    # receiving notifications; `false` detaches it.
    # @return [Object] a bool
    def on_evict(entry, reason)
      raise NotImplementedError, "#{self.class}#on_evict is not implemented"
    end
  end
```

```ruby
class Auditor
  include Kvstore::EvictionListener

  def on_evict(entry, reason)
    puts "#{entry.key}: #{reason}"
    true
  end
end

store.set_eviction_listener(Auditor.new)
```

Behind the module is one process-wide vtable per callback interface,
whose entries are `FFI::Function` trampolines held in constants so they
live for the process lifetime. Passing an implementation registers it in
a mutex-guarded registry under an integer key; the key (as an
`FFI::Pointer`) crosses as `ctx`, so the producer never holds a Ruby
object:

```ruby
  # Trampoline for EvictionListener#on_evict.
  WV_EVICTION_LISTENER_ON_EVICT = FFI::Function.new(:int32, [:pointer, :pointer, :size_t, :int32, :pointer]) do |ctx, entry_ptr, entry_len, reason, out_err|
    begin
      impl = _wv_cb_lookup(ctx)
      entry_r = WvBufferReader.new(entry_ptr.null? ? ''.b : entry_ptr.read_string(entry_len))
      entry_v = _wv_read_entry(entry_r)
      entry_r.expect_end!
      reason_v = reason
      impl.on_evict(entry_v, reason_v) ? 1 : 0
    rescue Exception => e
      _wv_cb_fail(out_err, e)
      0
    end
  end

  # Releases a EvictionListener implementation when the producer drops its last
  # reference.
  WV_EVICTION_LISTENER_FREE = FFI::Function.new(:void, [:pointer]) do |ctx|
    _wv_cb_free(ctx)
  end

  WV_EVICTION_LISTENER_VTABLE = WvEvictionListenerVtable.new
  WV_EVICTION_LISTENER_VTABLE[:on_evict] = WV_EVICTION_LISTENER_ON_EVICT
  WV_EVICTION_LISTENER_VTABLE[:free] = WV_EVICTION_LISTENER_FREE
```

```ruby
    def set_eviction_listener(listener)
      err = ErrorStruct.new
      listener_ctx = Kvstore._wv_cb_register(listener)
      Kvstore.weaveffi_kv_Store_set_eviction_listener(handle, listener_ctx, WV_EVICTION_LISTENER_VTABLE.to_ptr, err)
      Kvstore.check_error!(err)
    end
```

- **Lifetime.** The registry entry keeps the implementation alive
  exactly as long as the producer may call it; the vtable's `free`
  trampoline removes it when the producer drops its last reference. A
  producer that retains the implementation (a store's eviction listener)
  keeps it alive across calls; one that doesn't (the `events` sample's
  `route_once`) frees it before returning. Passing the same object twice
  registers two entries.
- **Argument ownership.** Borrowed strings and buffers are copied into
  Ruby values before the method runs, so the implementation may keep
  them. An object passed to a callback method is owned by the
  implementation: the trampoline adopts it into a new wrapper
  (`impl.on_attached(EventBus._from_ptr(bus))` in the `events` sample),
  and the implementation should `close` it when done (or let GC do it).
- **Return values.** A method's return value is converted back to its C
  representation (truthiness to `1`/`0`, an enum constant as its
  `:int32`, a record as a value buffer the producer frees).
- **Exceptions.** Any exception escaping a method (including the
  `NotImplementedError` default) never unwinds through the C frame.
  `_wv_cb_fail` writes `FOREIGN_ERROR_CODE` (-4) with the exception's
  message into the producer's error slot, and the trampoline returns a
  default; the producer aborts the call in progress, and the original
  caller sees `Error` with `code == -4`. For a callable marked `throws`,
  the domain mapper falls through to `Error` (so `rescue KvError`
  doesn't catch it but `rescue Kvstore::Error` does). The VM is never
  taken down.
- **Threads.** The producer may call a method from any thread. The ffi
  gem dispatches a callback arriving on a non-Ruby thread onto the VM
  by acquiring the GVL first, so the method runs as ordinary Ruby code
  on that thread. A blocking producer call that waits for a callback
  dispatched on another thread works because the ffi gem releases the
  GVL around the C call; a callback that itself blocks waiting on the
  calling thread will deadlock.

## Build instructions

1. Generate the bindings:

   ```bash
   weaveffi generate api.yaml -o generated --target ruby
   ```

2. Build the Rust shared library:

   ```bash
   cargo build --release -p your_library
   ```

3. Build and install the gem:

   ```bash
   cd generated/ruby
   gem build weaveffi.gemspec
   gem install weaveffi-0.1.0.gem
   ```

4. Make the cdylib findable at runtime. `WEAVEFFI_LIBRARY` may point at
   an exact file; otherwise:

   - macOS: `DYLD_LIBRARY_PATH=$PWD/../../target/release ruby your_script.rb`
   - Linux: `LD_LIBRARY_PATH=$PWD/../../target/release ruby your_script.rb`
   - Windows: place `weaveffi.dll` next to the script or add its
     directory to `PATH`.

The Ruby module name and gem name can be customised in `weaveffi.toml`:

```toml
[generators.ruby]
module_name = "MyBindings"
gem_name = "my_bindings"
```

## Packaging

`weaveffi package --target ruby` assembles one precompiled platform gem
tree per supplied desktop binary under `ruby/<platform-id>/`, each with
the generated library, the native library at `lib/native/`, and a
gemspec pinned to the RubyGems platform string. The packaged loader
still honours `WEAVEFFI_LIBRARY` first, then opens the bundled library
if it exists, then falls back to the system path.

| Platform | RubyGems platform |
|----------|-------------------|
| `macos-arm64` | `arm64-darwin` |
| `macos-x64` | `x86_64-darwin` |
| `linux-x64` | `x86_64-linux` |
| `linux-arm64` | `aarch64-linux` |
| `windows-x64` | `x64-mingw-ucrt` |

Android and `wasm32` binaries have no RubyGems platform and are skipped.
Run `gem build` in each tree to produce the platform gems. See
[Packaging](../guides/packaging.md) for the shared workflow.

## Memory and ownership

- **Strings in:** Ruby strings are passed as `:string` parameters and
  the FFI gem encodes them to null-terminated C strings.
- **Strings out:** the wrapper reads the returned `:pointer` with
  `read_string` (forcing UTF-8), then calls `weaveffi_free_string` to
  release the Rust-owned buffer.
- **Bytes:** an `FFI::MemoryPointer` is allocated for inputs; outputs
  are copied with `read_string(len)` and the returned buffer is released
  with `weaveffi_free_bytes`.
- **Buffered values (structs, rich enums, optionals, arrays, hashes):**
  parameters are packed into a binary `String` staged in an
  `FFI::MemoryPointer` the producer borrows for the duration of the
  call; returns are copied into Ruby memory with `read_string(len)`,
  released with `weaveffi_free_bytes`, and decoded with the `_wv_read_*`
  helper. Object tokens written into a buffer are fresh strong
  references the producer owns; tokens read out are adopted into
  wrappers.
- **Interfaces:** one strong reference per wrapper, released by `close`
  or by the `FFI::AutoPointer` on GC.
- **Callback implementations:** held in the module's registry until the
  producer calls the vtable's `free`.

## Async support

Async IDL functions (`async: true`) are exposed as blocking wrapper
methods. The wrapper creates a `Queue`, builds an `FFI::Function`
completion callback that pushes either the converted result or an error
onto it, calls the `_async`-suffixed C launcher, then pops the queue and
raises if the producer reported an error. For a callable marked
`throws: true`, the error goes through the domain mapper
(`kv_error_from` here), so the raised object is the typed class. From
the `kvstore` sample's `Store#compact`:

```ruby
    # Blocks the current thread until the async producer completes; the
    # result (or error) is delivered through the completion callback (cancellation token not exposed; pass-through is NULL).
    def compact()
      queue = Queue.new
      callback = FFI::Function.new(:void, [:pointer, :pointer, :int64]) do |_context, err_ptr, result|
        err = err_ptr.null? ? nil : ErrorStruct.new(err_ptr)
        if err && err[:code] != 0
          code = err[:code]
          msg = err[:message].null? ? '' : err[:message].read_string.force_encoding(Encoding::UTF_8)
          payload = err[:payload_ptr].null? ? nil : err[:payload_ptr].read_string(err[:payload_len])
          Kvstore.weaveffi_error_free(err_ptr)
          queue << Kvstore.kv_error_from(code, msg, payload)
        else
          queue << result
        end
      end
      Kvstore.weaveffi_kv_Store_compact_async(handle, FFI::Pointer::NULL, callback, FFI::Pointer::NULL)
      value = queue.pop
      raise value if value.is_a?(Error)
      value
    end
```

There is no promise/future type and no `concurrent-ruby` dependency: the
calling thread blocks until the completion callback fires. `Queue#pop`
releases the GVL, and the ffi gem delivers the cross-thread completion
callback safely. Wrap the call in a `Thread` when you need concurrency:

```ruby
t = Thread.new { store.compact }
reclaimed = t.value  # joins; re-raises a Kvstore::Error from the call
```

The local `callback` reference keeps the `FFI::Function` alive until
`queue.pop` returns, so the completion callback cannot be collected
mid-flight.

Result ownership follows the async contract: string, bytes, and buffered
results (records, rich enums, optionals, arrays, and hashes, arriving as
a pointer-plus-length pair) are owned by the consumer, so the callback
copies or decodes them into Ruby values and then releases them with
`weaveffi_free_string` or `weaveffi_free_bytes`. A reported error is
heap-boxed: the callback copies its code, message, and payload, then
releases it with `weaveffi_error_free`. An owned interface result
transfers ownership too: the callback adopts the pointer into a new
wrapper. A non-throwing async callable raises the generic `Error` on a
trap code; a panic inside the spawned future surfaces as
`PANIC_ERROR_CODE` (-2).

For functions marked `cancellable: true` the C launcher takes an extra
cancel-token parameter. The wrapper always passes `FFI::Pointer::NULL`
for it and doesn't expose the token. Cancellation tokens are currently
surfaced only by the C and C++ targets.

## Iterators

Functions returning `iter<T>` return a lazy `Enumerator` that streams
one element per pull: each consumer step issues exactly one call to the
generated `_next` binding, so nothing is drained up front. Call `.to_a`
if you want an eager `Array`. From the `kvstore` sample's `list_keys`
(argument packing elided):

```ruby
    # Returns a lazy Enumerator that streams one element per pull; call
    # `.to_a` to collect eagerly. The underlying producer iterator is
    # launched on the first pull, so launch errors raise at that point
    # rather than when this method returns. The iterator handle is
    # released exactly once, when iteration finishes or is abandoned
    # early (for example by `break`).
    def list_keys(prefix)
      # ... pack the optional prefix into prefix_buf ...
      Enumerator.new do |y|
        err = ErrorStruct.new
        iter = Kvstore.weaveffi_kv_Store_list_keys(handle, prefix_buf, prefix_data.bytesize, err)
        begin
          Kvstore.check_kv_error!(err)
          unless iter.null?
            loop do
              out_item = FFI::MemoryPointer.new(:pointer)
              item_err = ErrorStruct.new
              has_item = Kvstore.weaveffi_kv_Store_ListKeysIterator_next(iter, out_item, item_err)
              Kvstore.check_kv_error!(item_err)
              break if has_item.zero?
              item_ptr = out_item.read_pointer
              if item_ptr.null?
                y << ''
              else
                item = item_ptr.read_string.force_encoding(Encoding::UTF_8)
                Kvstore.weaveffi_free_string(item_ptr)
                y << item
              end
            end
          end
        ensure
          Kvstore.weaveffi_kv_Store_ListKeysIterator_destroy(iter) unless iter.null?
        end
      end
    end
```

The producer iterator launches on the first pull, so a launch error
raises then, not when the method returns. Each string element is copied
with `read_string` and freed with `weaveffi_free_string`; a buffered
element arrives as a value buffer that's decoded with the `_wv_read_*`
helper and released with `weaveffi_free_bytes`; an object element is
adopted into a new wrapper. The `ensure` block destroys the handle
exactly once, whether iteration exhausts, raises, or is abandoned early
(Ruby runs `ensure` when the enumerator's fiber is torn down, for
example after `break`). Enumerating the same `Enumerator` again
launches a fresh producer iterator.

The per-step error check follows the function's error strategy: the
throwing `list_keys` checks the launcher and each `next` with
`check_kv_error!`, so a failing step raises the typed `KvError`
subclass; a non-throwing iterator uses the generic `check_error!`, which
raises only on a producer bug.

## Known limitations

- Async functions block the calling thread; there's no `Fiber`- or
  promise-based variant, and `cancellable: true` tokens are not exposed.
- Callback methods run on whatever thread the producer uses, under the
  GVL; a callback that blocks waiting for the calling thread deadlocks.
- Callback-interface parameters are duck-typed: a missing method is only
  detected when the producer calls it (surfacing as `Error` with
  code -4, or `NoMethodError` text if the module wasn't included).
- The plain `generate` output relies on the dynamic loader (or
  `WEAVEFFI_LIBRARY`) to find the library; only `weaveffi package`
  bundles it.

## Troubleshooting

- **`LoadError: Could not open library 'libweaveffi.dylib'`**: the
  cdylib is not on the loader path. Set `WEAVEFFI_LIBRARY`,
  `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH`, or copy the library next to
  your script.
- **`LoadError: WeaveFFI ABI mismatch`**: the library was built by a
  different `weaveffi` release than the bindings. Regenerate the bindings
  and rebuild the library together.
- **`FFI::NotFoundError: Function 'weaveffi_*' not found`**: the cdylib
  does not export the symbol. Rebuild the Rust crate after regenerating
  the IDL.
- **`Error: Store used after close` (code -1)**: a closed wrapper was
  used as a receiver, a parameter, or a record field. Keep the wrapper
  open for as long as the object is in use; `dup` it if another owner
  needs an independent reference.
- **`Error` with `code == -4`**: a callback-interface method you
  implemented raised (or wasn't implemented); the message is the
  exception's text.
- **Segmentation faults on Ruby exit**: the generated wrappers keep
  vtable trampolines in constants and async completion callbacks
  referenced until they fire. If you call the `attach_function` bindings
  directly, keep your own `FFI::Function` objects alive for the lifetime
  of the call; letting them be garbage-collected mid-call corrupts the C
  side.
- **Strings come back as binary garbage**: UTF-8 strings should round
  trip through `read_string`; for binary data use `read_bytes(length)`
  with the `out_len` returned by the C ABI.
