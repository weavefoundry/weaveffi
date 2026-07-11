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
| `Struct`     | `StructName`       | `:pointer`                     |
| `Interface`  | `InterfaceName`    | `:pointer`                     |
| `Enum` (plain) | `Integer`        | `:int32`                       |
| `Enum` (rich)  | `EnumName`       | `:pointer`                     |
| `T?`         | `T` or `nil`       | `:pointer` for scalars; same pointer for strings/structs |
| `[T]`        | `Array`            | `:pointer` + `:size_t`         |
| `{K: V}`     | `Hash`             | key/value pointer arrays + `:size_t` |
| `iter<T>`    | `Enumerator` (lazy) | `:pointer` iterator handle    |

Booleans cross as `:int32` (`0`/`1`); the wrapper converts both
directions.

## Example IDL → generated code

```yaml
version: "0.5.0"
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

Structs become classes wrapping an `FFI::AutoPointer` so the C
destructor is called when Ruby garbage-collects the wrapper:

```ruby
class ContactPtr < FFI::AutoPointer
  def self.release(ptr)
    WeaveFFI.weaveffi_contacts_Contact_destroy(ptr)
  end
end

class Contact
  attr_reader :handle

  def initialize(handle)
    @handle = ContactPtr.new(handle)
  end

  def first_name
    result = WeaveFFI.weaveffi_contacts_Contact_get_first_name(@handle)
    return '' if result.null?
    str = result.read_string
    WeaveFFI.weaveffi_free_string(result)
    str
  end

  def email
    result = WeaveFFI.weaveffi_contacts_Contact_get_email(@handle)
    return nil if result.null?
    str = result.read_string
    WeaveFFI.weaveffi_free_string(result)
    str
  end
end
```

Functions are snake_case class methods on the module, with the IDL
module prefix stripped by default (a `kv.open_store` function surfaces
as `open_store`, not `kv_open_store`; the `attach_function` bindings
keep the full C symbol names). Set `strip_module_prefix: false` in the
Ruby generator config (or under `[global]`) to keep prefixed names:

```ruby
def self.create_contact(first_name, email, contact_type)
  err = ErrorStruct.new
  result = weaveffi_contacts_create_contact(
    first_name, email, contact_type, err)
  check_error!(err)
  result
end

def self.get_contact(id)
  err = ErrorStruct.new
  result = weaveffi_contacts_get_contact(id, err)
  check_error!(err)
  raise Error.new(-1, 'null pointer') if result.null?
  Contact.new(result)
end
```

The shared error machinery:

```ruby
class ErrorStruct < FFI::Struct
  layout :code, :int32, :message, :pointer
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
rich enum instead lowers to an **opaque object handle**, so the
generator emits a wrapper class with the same ownership model as a
struct wrapper, an `FFI::AutoPointer` (`ShapePtr`) that calls the C
`_destroy` on garbage collection.

For a `Shape` enum with variants `Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and `Labeled { label: string,
count: u8 }`, the generated class carries one discriminant constant per
variant, a `tag` reader, a `self.<variant>` factory per variant, and a
field reader per payload:

```ruby
class ShapePtr < FFI::AutoPointer
  def self.release(ptr)
    WeaveFFI.weaveffi_shapes_Shape_destroy(ptr)
  end
end

# An algebraic shape (sum type with associated data)
class Shape
  attr_reader :handle

  def initialize(handle)
    @handle = ShapePtr.new(handle)
  end

  # Variant discriminants returned by #tag
  EMPTY = 0
  CIRCLE = 1
  RECTANGLE = 2
  LABELED = 3

  def tag
    WeaveFFI.weaveffi_shapes_Shape_tag(@handle)
  end

  # A circle with a radius
  def self.circle(radius)
    err = WeaveFFI::ErrorStruct.new
    result = WeaveFFI.weaveffi_shapes_Shape_Circle_new(radius, err)
    WeaveFFI.check_error!(err)
    new(result)
  end

  # A labeled shape with a small count
  def self.labeled(label, count)
    err = WeaveFFI::ErrorStruct.new
    result = WeaveFFI.weaveffi_shapes_Shape_Labeled_new(label, count, err)
    WeaveFFI.check_error!(err)
    new(result)
  end

  # Radius in points
  def circle_radius
    WeaveFFI.weaveffi_shapes_Shape_Circle_get_radius(@handle)
  end

  def labeled_count
    WeaveFFI.weaveffi_shapes_Shape_Labeled_get_count(@handle)
  end
end
```

The remaining surface follows the same pattern: factories
`Shape.empty`, `Shape.circle`, `Shape.rectangle`, and `Shape.labeled`;
readers `circle_radius`, `rectangle_width`, `rectangle_height`,
`labeled_label`, and `labeled_count`. Each maps to a
`weaveffi_shapes_Shape_<Variant>_new` /
`weaveffi_shapes_Shape_<Variant>_get_<field>` symbol, and
`weaveffi_shapes_Shape_tag` returns the discriminant.

Construct a couple of variants, read the tag and a field, then pass the
wrapper to a module function:

```ruby
require 'weaveffi'

circle = WeaveFFI::Shape.circle(2.0)
labeled = WeaveFFI::Shape.labeled('unit', 3)

if circle.tag == WeaveFFI::Shape::CIRCLE
  puts circle.circle_radius          # 2.0
end
puts labeled.labeled_count           # 3

puts WeaveFFI.describe(circle)       # render via the C ABI
bigger = WeaveFFI.scale(circle, 3.0) # returns a new Shape
```

**Ownership:** the `ShapePtr` `FFI::AutoPointer` calls
`weaveffi_shapes_Shape_destroy` when Ruby garbage-collects the wrapper;
call `#destroy` for deterministic cleanup. The `Shape` returned by
`WeaveFFI.scale` is managed the same way.

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
- **Structs and interfaces:** wrappers hold an `FFI::AutoPointer`
  whose `release` callback invokes the C `_destroy` function on GC.
  Use the explicit `destroy` method for deterministic cleanup.
- **Lists and maps:** elements are copied into a Ruby `Array` or
  `Hash`; string elements are freed individually with
  `weaveffi_free_string`, then the backing pointer buffers are freed
  with `weaveffi_free_bytes`.
- **Boxed optional scalars:** an absent value is `nil`; a present one
  is dereferenced and the box is freed with `weaveffi_free_bytes`.

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
    :void, [:pointer, :pointer, :pointer]
  ) do |_context, err_ptr, result|
    err = err_ptr.null? ? nil : ErrorStruct.new(err_ptr)
    if err && err[:code] != 0
      # ... read code/message, weaveffi_error_clear ...
      queue << task_error_from(code, msg)
    else
      # ... null-pointer guard ...
      queue << TaskResult.new(result)
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

Result ownership follows the async contract: string, bytes, array,
map, and boxed optional scalar results are borrowed for the callback's
duration, so the callback copies them into Ruby values (`read_string`,
element reads) before it returns and never frees them; the producer
does after the callback returns. Object results (records, rich enums,
interfaces, including optional ones) are the exception: the callback
receives ownership, and the wrapper adopts the pointer into its
`FFI::AutoPointer` (as `TaskResult.new(result)` does above), so the
destructor runs on GC or an explicit `destroy`.

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
record elements are adopted by their `FFI::AutoPointer`-backed
wrapper. The `ensure` block destroys the handle exactly once, whether
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
