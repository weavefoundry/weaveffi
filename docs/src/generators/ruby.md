# Ruby

## Overview

The Ruby target produces pure-Ruby FFI bindings using the
[ffi](https://github.com/ffi/ffi) gem to call the C ABI directly. There
is no native extension to compile — `gem install ffi` is the only
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
| `bool`       | `true`/`false`     | `:int32` (0/1 conversion)      |
| `string`     | `String`           | `:string` (param) / `:pointer` (return) |
| `bytes`      | `String` (binary)  | `:pointer` + `:size_t`         |
| `handle`     | `Integer`          | `:uint64`                      |
| `Struct`     | `StructName`       | `:pointer`                     |
| `Enum`       | `Integer`          | `:int32`                       |
| `T?`         | `T` or `nil`       | `:pointer` for scalars; same pointer for strings/structs |
| `[T]`        | `Array`            | `:pointer` + `:size_t`         |
| `{K: V}`     | `Hash`             | key/value pointer arrays + `:size_t` |
| `iter<T>`    | `Array`            | `:pointer` iterator handle     |

Booleans cross as `:int32` (`0`/`1`); the wrapper converts both
directions.

## Example IDL → generated code

```yaml
version: "0.3.0"
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

  case FFI::Platform::OS
  when /darwin/  then ffi_lib 'libweaveffi.dylib'
  when /mswin|mingw/ then ffi_lib 'weaveffi.dll'
  else ffi_lib 'libweaveffi.so'
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

Functions are class methods on the module and raise on failure:

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
  are read with `read_string(len)` and the Rust side is responsible
  for the buffer it returned.
- **Structs:** wrappers hold an `FFI::AutoPointer` whose `release`
  callback invokes the C `_destroy` function on GC. Use the explicit
  `destroy` method for deterministic cleanup.
- **Maps:** keys and values are marshalled into parallel
  `FFI::MemoryPointer` buffers; the wrapper rebuilds a Ruby `Hash`
  from the returned arrays.

## Async support

Async IDL functions (`async: true`) are exposed as blocking wrapper
methods. The wrapper creates a `Queue`, builds an `FFI::Function`
completion callback that pushes either the converted result or an
`Error` onto it, calls the `_async`-suffixed C launcher, then pops the
queue and raises if the producer reported an error:

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
      queue << Error.new(code, msg)
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

For functions marked `cancellable: true` the C launcher takes an extra
cancel-token parameter. The wrapper always passes `FFI::Pointer::NULL`
for it — the token is not exposed (the generated comment reads
"cancellation token not exposed; pass-through is NULL"). Cancellation
tokens are currently surfaced only by the C, C++, and Kotlin targets.

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

- **GC safety** — the `FFI::Function` trampoline is pinned in a
  module-level registry (`@listener_refs`), keyed by subscription id,
  so it cannot be garbage-collected while the producer may still call
  it. Unregistering deletes the registry entry.
- **Subscription ids** — registration returns the `uint64` id produced
  by `weaveffi_events_register_message_listener(fn, context)`; pass it
  to `unregister_message_listener` to stop delivery and release the
  trampoline.
- **Threading** — the callback fires on the producer's thread, not the
  thread that registered it. Do not block inside it; marshal results
  to your own thread or event loop (a `Queue` works well).

Typical round trip:

```ruby
id = WeaveFFI.register_message_listener { |message| puts message }
WeaveFFI.send_message('hello')
WeaveFFI.unregister_message_listener(id)
```

## Iterators

Functions returning `iter<T>` receive an opaque iterator handle from
the C ABI. The wrapper drains it eagerly with the generated `_next`
binding, frees each returned string, destroys the handle, and returns
a fully materialised `Array` — there is no lazy `Enumerator`:

```ruby
attach_function :weaveffi_events_get_messages, [:pointer], :pointer
attach_function :weaveffi_events_GetMessagesIterator_next,
                [:pointer, :pointer, :pointer], :int32
attach_function :weaveffi_events_GetMessagesIterator_destroy,
                [:pointer], :void

def self.get_messages()
  err = ErrorStruct.new
  iter = weaveffi_events_get_messages(err)
  check_error!(err)
  items = []
  return items if iter.null?
  loop do
    out_item = FFI::MemoryPointer.new(:pointer)
    item_err = ErrorStruct.new
    has_item = weaveffi_events_GetMessagesIterator_next(iter, out_item, item_err)
    # ... destroy the iterator and check_error! if item_err is set ...
    break if has_item.zero?
    item_ptr = out_item.read_pointer
    # ... empty string for NULL ...
    items << item_ptr.read_string
    weaveffi_free_string(item_ptr)
  end
  weaveffi_events_GetMessagesIterator_destroy(iter)
  items
end
```

If `_next` reports an error the wrapper destroys the handle first and
then raises `WeaveFFI::Error` via `check_error!`; on success the
handle is destroyed before the array is returned.

## Troubleshooting

- **`LoadError: Could not open library 'libweaveffi.dylib'`** — the
  cdylib is not on the loader path. Set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the library next to your script.
- **`FFI::NotFoundError: Function 'weaveffi_*' not found`** — the
  cdylib does not export the symbol. Rebuild the Rust crate after
  regenerating the IDL.
- **Segmentation faults on Ruby exit** — the generated wrappers pin
  listener trampolines in `@listener_refs` and keep async completion
  callbacks referenced until they fire. If you call the
  `attach_function` bindings directly, keep your own `FFI::Function`
  objects alive for the lifetime of the call; letting them be
  garbage-collected mid-call corrupts the C side.
- **Strings come back as binary garbage** — UTF-8 strings should round
  trip through `read_string`; for binary data use
  `read_bytes(length)` with the `out_len` returned by the C ABI.
