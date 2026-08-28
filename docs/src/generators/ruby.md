# Ruby

## Overview

The Ruby target produces pure-Ruby FFI bindings using the
[ffi](https://github.com/ffi/ffi) gem to call the C ABI directly. There's
no native extension to compile; `gem install ffi` is the only
prerequisite. The generator emits a single `.rb` file plus a `gemspec`
ready for `gem build` and `gem install`.

The trade-off is that FFI gem calls are slower than a hand-written C
extension. For typical FFI workloads the overhead is negligible compared
to the work done inside the Rust library.

## What gets generated

| File | Purpose |
|------|---------|
| `ruby/lib/weaveffi.rb` | FFI bindings: library loader, `attach_function` declarations, wrapper classes |
| `ruby/weaveffi.gemspec` | Gem specification with `ffi ~> 1.15` dependency |
| `ruby/README.md` | Prerequisites and usage instructions |

The file names follow the gem name (IDL `package.name`): a package
named `events` produces `lib/events.rb` and `events.gemspec`;
`weaveffi` is the default.

## Type mapping

| IDL type     | Ruby type          | FFI type                       |
|--------------|--------------------|--------------------------------|
| `i32`        | `Integer`          | `:int32`                       |
| `u32`        | `Integer`          | `:uint32`                      |
| `i64`        | `Integer`          | `:int64`                       |
| `f64`        | `Float`            | `:double`                      |
| `i8`         | `Integer`          | `:int8`                        |
| `i16`        | `Integer`          | `:int16`                       |
| `u8`         | `Integer`          | `:uint8`                       |
| `u16`        | `Integer`          | `:uint16`                      |
| `u64`        | `Integer`          | `:uint64`                      |
| `f32`        | `Float`            | `:float`                       |
| `bool`       | `true`/`false`     | `:int32` (0/1 conversion)      |
| `string`     | `String`           | `:string` (param) / `:pointer` (return) |
| `bytes`      | `String` (binary)  | `:pointer` + `:size_t`         |
| `handle`     | `Integer`          | `:uint64`                      |
| `Struct`     | `StructName` (plain class) | value buffer (`:pointer` + `:size_t`) |
| `Interface`  | `InterfaceName`    | `:pointer`                     |
| `Enum` (plain) | `Integer`        | `:int32`                       |
| `Enum` (rich)  | `EnumName` (nested variant classes) | value buffer (`:pointer` + `:size_t`) |
| `T?`         | `T` or `nil`       | value buffer; `Interface?` stays a nullable `:pointer` |
| `[T]`        | `Array`            | value buffer (`:pointer` + `:size_t`) |
| `{K: V}`     | `Hash`             | value buffer (`:pointer` + `:size_t`) |
| `iter<T>`    | `Enumerator` (lazy) | `:pointer` iterator handle    |

Booleans cross as `:int32` (`0`/`1`); the wrapper converts both
directions.

## Example IDL → generated code

```yaml
version: "0.7.0"
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
        return: handle

      - name: get_contact
        params:
          - { name: id, type: handle }
        return: Contact

      - name: list_contacts
        params: []
        return: "[Contact]"
```

The generated module extends `FFI::Library` and selects the right
shared library at load time:

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

A `Contact` crosses the ABI serialized in the
[value-buffer format](../reference/value-buffers.md) as a single
pointer-plus-length pair; the module carries private
`_wv_write_contact`/`_wv_read_contact` codec helpers over a small buffer
writer and reader.

Functions are snake_case class methods on the module, with the IDL
module prefix stripped by default (a `kv.open_store` function surfaces
as `open_store`, not `kv_open_store`; the `attach_function` bindings
keep the full C symbol names). Set `strip_module_prefix: false` in the
Ruby generator config (or under `[global]`) to keep prefixed names:

```ruby
def self.create_contact(first_name, email, contact_type)
  err = ErrorStruct.new
  # Optionals are buffered: pack the argument into a value buffer that
  # the producer borrows for the duration of the call.
  email_buf = ... # generated pack code writes the optional into a binary String
  result = weaveffi_contacts_create_contact(
    first_name, email_buf, email_buf.bytesize, contact_type, err)
  check_error!(err)
  result
end

def self.get_contact(id)
  err = ErrorStruct.new
  out_len = FFI::MemoryPointer.new(:size_t)
  result = weaveffi_contacts_get_contact(id, out_len, err)
  check_error!(err)
  # Decode the returned value buffer, then release it.
  len = out_len.read(:size_t)
  value = _wv_read_contact(WvBufferReader.new(result.read_string(len)))
  weaveffi_free_bytes(result, len)
  value
end
```

The shared error machinery:

```ruby
class ErrorStruct < FFI::Struct
  layout :code, :int32,
         :message, :pointer,
         :payload_ptr, :pointer,
         :payload_len, :size_t
end

class Error < StandardError
  attr_reader :code

  def initialize(code, message)
    @code = code
    super(message)
  end
end

def self.check_error!(err)
  return if err[:code].zero?
  code = err[:code]
  msg_ptr = err[:message]
  msg = msg_ptr.null? ? '' : msg_ptr.read_string
  weaveffi_error_clear(err.to_ptr)
  raise Error.new(code, msg)
end
```

Catch errors with standard `begin`/`rescue`:

```ruby
require 'weaveffi'

begin
  handle = WeaveFFI.create_contact("Alice", nil, WeaveFFI::ContactType::WORK)
rescue WeaveFFI::Error => e
  puts "Error #{e.code}: #{e.message}"
end
```

## Typed errors

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
def self.kv_error_from(code, message)
  cls = KV_ERROR_CODES[code]
  return Error.new(code, message) if cls.nil?
  message.empty? ? cls.new : cls.new(message)
end
```

Only callables marked `throws: true` in the IDL raise the typed
classes: their wrappers call `check_kv_error!`, so you can rescue
`Kvstore::KvError::KeyNotFound` for one code or `Kvstore::KvError` for
the whole domain. A callable without `throws` uses the generic
`check_error!`, which raises `Error` only if the producer misbehaves.

An error code that declares payload `fields:` carries them serialized
in the error's payload buffer (`payload_ptr`/`payload_len` on
`ErrorStruct`); the domain factory decodes them into attributes on the
raised error object before `weaveffi_error_clear` releases the buffer.

## Interfaces

An `interfaces:` entry becomes a class wrapping an `FFI::AutoPointer`
subclass, so the C destructor runs when Ruby garbage-collects the
wrapper. Constructors become class methods (`Store.open`; a
constructor named `new` maps to the ordinary `Store.new`), methods are
snake_case instance methods, statics are class methods, and `destroy`
frees the native object deterministically. From the `kvstore` sample
(trimmed):

```ruby
class StorePtr < FFI::AutoPointer
  def self.release(ptr)
    Kvstore.weaveffi_kv_Store_destroy(ptr)
  end
end

# An embedded key-value store owning its entries
class Store
  attr_reader :handle

  # Wraps an owned pointer the producer handed over, without
  # re-running initialize.
  def self._from_ptr(ptr)
    obj = allocate
    obj.instance_variable_set(:@handle, StorePtr.new(ptr))
    obj
  end

  def destroy
    return if @handle.nil?
    @handle.free
    @handle = nil
  end

  # Open (or create) a store backed by the given filesystem path
  def self.open(path)
    err = ErrorStruct.new
    result = Kvstore.weaveffi_kv_Store_open(path, err)
    Kvstore.check_kv_error!(err)
    raise Error.new(-1, 'null pointer') if result.null?
    _from_ptr(result)
  end

  def put(key, value, kind, ttl_seconds) # raises typed KvError subclasses
    # ...
  end

  def list_keys(prefix) # lazy Enumerator; see Iterators
    # ...
  end

  def count() # generic check only (no throws)
    # ...
  end

  def compact() # blocking async; see Async support
    # ...
  end

  # Legacy single-shot put kept for compatibility
  def legacy_put(key, value)
    warn "[DEPRECATED] use put() with explicit kind"
    # ...
  end

  # The largest number of live entries one store will hold
  def self.default_capacity()
    # ...
  end
end
```

Functions elsewhere in the IDL pass the wrapper's `handle` across the
boundary (`Kvstore.get_stats(store)` returns a new `Stats`).
Deprecated members print a `[DEPRECATED]` warning at call time:

```ruby
store = Kvstore::Store.open('/tmp/cache.kv')
store.put('alpha', "\x01".b, Kvstore::EntryKind::PERSISTENT, nil)
puts "#{store.count} / #{Kvstore::Store.default_capacity}"
reclaimed = store.compact
store.destroy
```

## Rich (algebraic) enums

A rich (algebraic) enum is a sum type whose variants carry associated
data. A plain C-style `Enum` crosses as a bare `:int32` discriminant; a
rich enum instead becomes a plain class hierarchy: a base class with a
`tag` reader plus one nested subclass per variant carrying that
variant's fields as attributes, each pinning its `TAG` constant. Rich
enums declare no C symbols; values cross the ABI serialized in value
buffers as an `i32` tag followed by the active variant's fields.

For a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and `Labeled { label: string,
count: u8 }`, the generator emits:

```ruby
# An algebraic shape (sum type with associated data)
class Shape
  # The active variant's integer tag.
  def tag
    self.class::TAG
  end

  # The empty shape
  class Empty < Shape
    TAG = 0
  end

  # A circle with a radius
  class Circle < Shape
    TAG = 1

    # Radius in points
    attr_reader :radius

    def initialize(radius:)
      @radius = radius
    end
  end

  # A labeled shape with a small count
  class Labeled < Shape
    TAG = 3

    attr_reader :label
    attr_reader :count

    def initialize(label:, count:)
      @label = label
      @count = count
    end
  end
end
```

Construct variants directly and branch with `case`/`when` on the class:

```ruby
require 'weaveffi'

circle = WeaveFFI::Shape::Circle.new(radius: 2.0)
labeled = WeaveFFI::Shape::Labeled.new(label: 'unit', count: 3)

case circle
when WeaveFFI::Shape::Circle
  puts circle.radius                 # 2.0
end
puts labeled.count                   # 3

puts WeaveFFI.describe(circle)       # packs the buffer via the C ABI
bigger = WeaveFFI.scale(circle, 3.0) # returns a new Shape value
```

Values are plain Ruby data; there's no native handle and nothing to
destroy. The private `_wv_write_shape`/`_wv_read_shape` codec helpers
write and read the tagged wire format.

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

4. Make the cdylib findable at runtime:

   - macOS: `DYLD_LIBRARY_PATH=$PWD/../../target/release ruby your_script.rb`
   - Linux: `LD_LIBRARY_PATH=$PWD/../../target/release ruby your_script.rb`
   - Windows: place `weaveffi.dll` next to the script or add its
     directory to `PATH`.

The Ruby module name and gem name can be customised via generator
configuration:

```toml
[ruby]
module_name = "MyBindings"
gem_name = "my_bindings"
```

## Memory and ownership

- **Strings in:** Ruby strings are passed as `:string` parameters and
  the FFI gem encodes them to null-terminated C strings.
- **Strings out:** the wrapper reads the returned `:pointer` with
  `read_string`, then calls `weaveffi_free_string` to release the
  Rust-owned buffer.
- **Bytes:** an `FFI::MemoryPointer` is allocated for inputs; outputs
  are copied with `read_string(len)` and the returned buffer is
  released with `weaveffi_free_bytes`.
- **Interfaces:** wrappers hold an `FFI::AutoPointer` whose `release`
  callback invokes the C `_destroy` function on GC. Use the explicit
  `destroy` method for deterministic cleanup.
- **Buffered values (structs, rich enums, optionals, lists, maps):**
  parameters are packed into a binary `String` value buffer that the
  producer borrows for the duration of the call; returns are copied
  into Ruby memory with `read_string(len)`, released with
  `weaveffi_free_bytes`, and decoded with the `_wv_read_*` helper.
  Nothing to destroy afterward.

## Async support

Async IDL functions (`async: true`) are exposed as blocking wrapper
methods. The wrapper creates a `Queue`, builds an `FFI::Function`
completion callback that pushes either the converted result or an
error onto it, calls the `_async`-suffixed C launcher, then pops the
queue and raises if the producer reported an error. For a callable
marked `throws: true`, the error goes through the domain mapper
(`task_error_from` here, `kv_error_from` on `Store#compact`), so the
raised object is the typed class:

```ruby
# Blocks until the async producer completes.
def self.run_task(name)
  queue = Queue.new
  callback = FFI::Function.new(
    :void, [:pointer, :pointer, :pointer, :size_t]
  ) do |_context, err_ptr, result_ptr, result_len|
    err = err_ptr.null? ? nil : ErrorStruct.new(err_ptr)
    if err && err[:code] != 0
      # ... read code/message/payload, then weaveffi_error_free ...
      queue << task_error_from(code, msg)
    else
      # TaskResult is a record: copy the owned buffer, free it, decode.
      reader = WvBufferReader.new(result_ptr.read_string(result_len))
      weaveffi_free_bytes(result_ptr, result_len) unless result_ptr.null?
      queue << _wv_read_task_result(reader)
    end
  end
  weaveffi_tasks_run_task_async(name, callback, FFI::Pointer::NULL)
  value = queue.pop
  raise value if value.is_a?(Error)
  value
end
```

There is no promise/future type and no `concurrent-ruby` dependency:
the calling thread blocks until the completion callback fires. Wrap
the call in a `Thread` when you need concurrency:

```ruby
t = Thread.new { WeaveFFI.run_task('demo') }
result = t.value  # joins; re-raises a WeaveFFI::Error from the call
```

The local `callback` reference keeps the `FFI::Function` alive until
`queue.pop` returns, so the completion callback cannot be collected
mid-flight.

Result ownership follows the async contract: string, bytes, and
buffered results (records, rich enums, optionals, arrays, and maps,
arriving as a pointer-plus-length pair) are owned by the consumer, so
the callback copies or decodes them into Ruby values and then releases
them with `weaveffi_free_string` or `weaveffi_free_bytes`. A reported
error is heap-boxed: the callback copies its code, message, and
payload, then releases it with `weaveffi_error_free`. An owned
interface result transfers ownership too: the wrapper adopts the
pointer into its `FFI::AutoPointer`, so the destructor runs on GC or
an explicit `destroy`.

For functions marked `cancellable: true` the C launcher takes an extra
cancel-token parameter. The wrapper always passes `FFI::Pointer::NULL`
for it. The token isn't exposed (the generated comment reads
"cancellation token not exposed; pass-through is NULL"). Cancellation
tokens are currently surfaced only by the C and C++ targets.

## Callbacks and listeners

IDL `callbacks` declare a C function-pointer type; a `listener` pairs
one with register/unregister entry points:

```yaml
callbacks:
  - name: OnMessage
    params:
      - { name: message, type: string }
listeners:
  - name: message_listener
    event_callback: OnMessage
```

The generated module declares the FFI callback type and exposes a
register/unregister pair. Registering takes a block, wraps it in an
`FFI::Function` trampoline, and returns a `uint64` subscription id:

```ruby
callback :weaveffi_events_OnMessage_fn, [:string, :pointer], :void
attach_function :weaveffi_events_register_message_listener,
                [:weaveffi_events_OnMessage_fn, :pointer], :uint64
attach_function :weaveffi_events_unregister_message_listener, [:uint64], :void

# Registers a OnMessage listener block. Returns a subscription id for
# unregister_message_listener.
def self.register_message_listener(&block)
  trampoline = FFI::Function.new(:void, [:string, :pointer]) do |message, _context|
    block.call(message)
  end
  listener_id = weaveffi_events_register_message_listener(trampoline, FFI::Pointer::NULL)
  @listener_refs[listener_id] = trampoline
  listener_id
end

def self.unregister_message_listener(listener_id)
  weaveffi_events_unregister_message_listener(listener_id)
  @listener_refs.delete(listener_id)
  nil
end
```

- **GC safety**: the `FFI::Function` trampoline is pinned in a
  module-level registry (`@listener_refs`), keyed by subscription id,
  so it cannot be garbage-collected while the producer may still call
  it. Unregistering deletes the registry entry.
- **Subscription ids**: registration returns the `uint64` id produced
  by `weaveffi_events_register_message_listener(fn, context)`; pass it
  to `unregister_message_listener` to stop delivery and release the
  trampoline.
- **Threading**: the callback fires on the producer's thread, not the
  thread that registered it. Do not block inside it; marshal results
  to your own thread or event loop (a `Queue` works well).

Typical round trip:

```ruby
id = WeaveFFI.register_message_listener { |message| puts message }
WeaveFFI.send_message('hello')
WeaveFFI.unregister_message_listener(id)
```

## Iterators

Functions returning `iter<T>` return a lazy `Enumerator` that streams
one element per pull: each consumer step issues exactly one call to
the generated `_next` binding, so nothing is drained up front. Call
`.to_a` if you want an eager `Array`:

```ruby
attach_function :weaveffi_events_get_messages, [:pointer], :pointer
attach_function :weaveffi_events_GetMessagesIterator_next,
                [:pointer, :pointer, :pointer], :int32
attach_function :weaveffi_events_GetMessagesIterator_destroy,
                [:pointer], :void

# Return an iterator over all sent messages
# Returns a lazy Enumerator that streams one element per pull; call
# `.to_a` to collect eagerly. The underlying producer iterator is
# launched on the first pull, so launch errors raise at that point
# rather than when this method returns. The iterator handle is
# released exactly once, when iteration finishes or is abandoned
# early (for example by `break`).
def self.get_messages()
  Enumerator.new do |y|
    err = ErrorStruct.new
    iter = weaveffi_events_get_messages(err)
    begin
      check_error!(err)
      unless iter.null?
        loop do
          out_item = FFI::MemoryPointer.new(:pointer)
          item_err = ErrorStruct.new
          has_item = weaveffi_events_GetMessagesIterator_next(iter, out_item, item_err)
          check_error!(item_err)
          break if has_item.zero?
          item_ptr = out_item.read_pointer
          if item_ptr.null?
            y << ''
          else
            item = item_ptr.read_string
            weaveffi_free_string(item_ptr)
            y << item
          end
        end
      end
    ensure
      weaveffi_events_GetMessagesIterator_destroy(iter) unless iter.null?
    end
  end
end
```

The producer iterator launches on the first pull, so a launch error
raises then, not when the method returns. Each string element is
copied with `read_string` and freed with `weaveffi_free_string`;
a buffered element arrives as a value buffer that's decoded with the
`_wv_read_*` helper and released with `weaveffi_free_bytes`. The
`ensure` block destroys the handle exactly once, whether
iteration exhausts, raises, or is abandoned early (Ruby runs `ensure`
when the enumerator's fiber is torn down, for example after `break`).

The per-step error check follows the function's error strategy: the
throwing `kvstore` sample's `Store#list_keys` checks the launcher and
each `next` with `check_kv_error!`, so a failing step raises the typed
`KvError` subclass; the non-throwing `get_messages` uses the generic
`check_error!`, which raises only on a producer bug.

## Troubleshooting

- **`LoadError: Could not open library 'libweaveffi.dylib'`**: the
  cdylib is not on the loader path. Set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the library next to your script.
- **`FFI::NotFoundError: Function 'weaveffi_*' not found`**: the
  cdylib does not export the symbol. Rebuild the Rust crate after
  regenerating the IDL.
- **Segmentation faults on Ruby exit**: the generated wrappers pin
  listener trampolines in `@listener_refs` and keep async completion
  callbacks referenced until they fire. If you call the
  `attach_function` bindings directly, keep your own `FFI::Function`
  objects alive for the lifetime of the call; letting them be
  garbage-collected mid-call corrupts the C side.
- **Strings come back as binary garbage**: UTF-8 strings should round
  trip through `read_string`; for binary data use
  `read_bytes(length)` with the `out_len` returned by the C ABI.
