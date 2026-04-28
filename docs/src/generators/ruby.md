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
   weaveffi generate --input api.yaml --output generated/ --target ruby
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
ruby_module_name = "MyBindings"
ruby_gem_name = "my_bindings"
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

Async IDL functions are exposed as Ruby methods that return a
`Concurrent::Promises::Future` (when `concurrent-ruby` is present) or
a hand-rolled callback wrapper otherwise. The Ruby thread that calls
the function blocks on a `Queue` to receive the result from the C ABI
callback:

```ruby
def self.fetch_contact(id)
  q = Queue.new
  context = FFI::Function.new(:void, [:pointer, :pointer]) do |err, result|
    q << [err, result]
  end
  weaveffi_contacts_fetch_contact_async(id, context, nil)
  err, result = q.pop
  check_error!(ErrorStruct.new(err))
  Contact.new(result)
end
```

When the IDL marks the function `cancel: true`, the wrapper accepts a
cancellation token and forwards it to the underlying
`weaveffi_cancel_token`.

## Troubleshooting

- **`LoadError: Could not open library 'libweaveffi.dylib'`** — the
  cdylib is not on the loader path. Set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the library next to your script.
- **`FFI::NotFoundError: Function 'weaveffi_*' not found`** — the
  cdylib does not export the symbol. Rebuild the Rust crate after
  regenerating the IDL.
- **Segmentation faults on Ruby exit** — keep references to FFI
  callbacks alive for the lifetime of the call. Letting them be
  garbage-collected mid-call corrupts the C side.
- **Strings come back as binary garbage** — UTF-8 strings should round
  trip through `read_string`; for binary data use
  `read_bytes(length)` with the `out_len` returned by the C ABI.
