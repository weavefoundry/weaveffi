# Ruby

The Ruby generator produces pure-Ruby FFI bindings using the
[ffi](https://github.com/ffi/ffi) gem to call the C ABI directly. No
compilation step, no native extensions — just `require 'ffi'` and go.

## Why the FFI gem?

- **Minimal dependency.** The `ffi` gem is the standard Ruby library for
  calling native code. It is battle-tested in projects like GRPC, Sass, and
  libsodium bindings.
- **No C compiler required.** The generated `.rb` files are plain Ruby — no
  Makefile, no `extconf.rb`, no build step beyond `gem install ffi`.
- **Transparent.** Developers can read and debug the generated code directly.

The trade-off is that FFI gem calls are slower than hand-written C extensions.
For most FFI workloads the overhead is negligible compared to the work done
inside the Rust library.

## Generated artifacts

| File | Purpose |
|------|---------|
| `ruby/lib/weaveffi.rb` | FFI bindings: library loader, `attach_function` declarations, wrapper classes |
| `ruby/weaveffi.gemspec` | Gem specification with `ffi ~> 1.15` dependency |
| `ruby/README.md` | Prerequisites and usage instructions |

## The FFI gem approach

The generated module extends `FFI::Library` and uses `attach_function` to bind
each C ABI symbol. Platform detection selects the correct shared library name
at load time:

```ruby
require 'ffi'

module WeaveFFI
  extend FFI::Library

  case FFI::Platform::OS
  when /darwin/
    ffi_lib 'libweaveffi.dylib'
  when /mswin|mingw/
    ffi_lib 'weaveffi.dll'
  else
    ffi_lib 'libweaveffi.so'
  end
end
```

Every C ABI function is declared with `attach_function`, mapping parameter
types and return types to FFI type symbols (`:int32`, `:pointer`, etc.).
A thin Ruby method then wraps each raw call with argument conversion, error
checking, and return-value marshalling.

## Generated code examples

Given this IDL definition:

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
          - { name: last_name, type: string }
          - { name: email, type: "string?" }
          - { name: contact_type, type: ContactType }

    functions:
      - name: create_contact
        params:
          - { name: first_name, type: string }
          - { name: last_name, type: string }
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

      - name: count_contacts
        params: []
        return: i32
```

### Enums

Enums map to Ruby modules with `SHOUTY_SNAKE_CASE` constants:

```ruby
module ContactType
  PERSONAL = 0
  WORK = 1
  OTHER = 2
end
```

Enum values are plain integers and are passed directly to the C ABI.

### Structs (AutoPointer wrapper classes)

Structs become Ruby classes with an `FFI::AutoPointer` handle. The
`AutoPointer` ensures the C ABI `_destroy` function is called when the
object is garbage collected, preventing memory leaks:

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

  def self.create(handle)
    new(handle)
  end

  def destroy
    return if @handle.nil?
    @handle.free
    @handle = nil
  end

  def id
    result = WeaveFFI.weaveffi_contacts_Contact_get_id(@handle)
    result
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

  def contact_type
    result = WeaveFFI.weaveffi_contacts_Contact_get_contact_type(@handle)
    result
  end
end
```

### Functions

Each IDL function becomes a class method on the module. The wrapper creates
an `ErrorStruct`, calls the C symbol, checks for errors, and converts the
return value:

```ruby
def self.create_contact(first_name, last_name, email, contact_type)
  err = ErrorStruct.new
  result = weaveffi_contacts_create_contact(
    first_name, last_name, email, contact_type, err)
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

def self.list_contacts
  err = ErrorStruct.new
  out_len = FFI::MemoryPointer.new(:size_t)
  result = weaveffi_contacts_list_contacts(out_len, err)
  check_error!(err)
  return [] if result.null?
  len = out_len.read(:size_t)
  result.read_array_of_pointer(len).map { |p| Contact.new(p) }
end

def self.count_contacts
  err = ErrorStruct.new
  result = weaveffi_contacts_count_contacts(err)
  check_error!(err)
  result
end
```

### Error handling

The generated module defines an `ErrorStruct` (mirroring the C
`weaveffi_error`) and an `Error` exception class. Every function call
follows this pattern:

```ruby
class ErrorStruct < FFI::Struct
  layout :code, :int32,
         :message, :pointer
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

Callers use standard Ruby `begin`/`rescue`:

```ruby
require 'weaveffi'

begin
  handle = WeaveFFI.create_contact("Alice", "Smith", nil, ContactType::WORK)
rescue WeaveFFI::Error => e
  puts "Error #{e.code}: #{e.message}"
end
```

## Type mapping reference

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

Booleans are transmitted as `:int32` (`0`/`1`). The wrapper converts
`true`/`false` to integers on input and back to booleans on output.

## Gem packaging

### 1. Generate bindings

```bash
weaveffi generate --input api.yaml --output generated/ --target ruby
```

### 2. Build the Rust shared library

```bash
cargo build --release -p your_library
```

This produces `libweaveffi.dylib` (macOS), `libweaveffi.so` (Linux), or
`weaveffi.dll` (Windows) in `target/release/`.

### 3. Build and install the gem

```bash
cd generated/ruby
gem build weaveffi.gemspec
gem install weaveffi-0.1.0.gem
```

The generated gemspec declares `ffi ~> 1.15` as its only runtime dependency.

### 4. Make the shared library findable

The shared library must be on the system library search path at runtime:

**macOS:**
```bash
DYLD_LIBRARY_PATH=../../target/release ruby your_script.rb
```

**Linux:**
```bash
LD_LIBRARY_PATH=../../target/release ruby your_script.rb
```

**Windows:**
Place `weaveffi.dll` in the same directory as your script, or add its
directory to `PATH`.

### 5. Use the bindings

```ruby
require 'weaveffi'

handle = WeaveFFI.create_contact("Alice", "Smith", "alice@example.com",
                                  WeaveFFI::ContactType::WORK)
contact = WeaveFFI.get_contact(handle)
puts "#{contact.first_name} #{contact.last_name}"
puts "Email: #{contact.email || '(none)'}"
puts "Total: #{WeaveFFI.count_contacts}"
```

## Memory management

The generated Ruby wrappers handle memory ownership automatically via
`FFI::AutoPointer` and explicit free calls.

### Strings

- **Passing strings in:** Ruby strings are passed as `:string` parameters.
  FFI handles the encoding to null-terminated C strings automatically.
- **Receiving strings back:** Returned `:pointer` values are read with
  `read_string`, then the Rust-allocated pointer is freed via
  `weaveffi_free_string`. The wrapper copies the data into a Ruby string
  before freeing.

### Bytes

- **Passing bytes in:** A `FFI::MemoryPointer` is allocated, the byte data
  is copied in via `put_bytes`, and the pointer is passed with a length
  parameter.
- **Receiving bytes back:** The C function writes to an `out_len` parameter.
  The wrapper reads the data via `read_string(len)`, then the Rust side is
  responsible for the original buffer.

### Structs (AutoPointer release callbacks)

Each struct class uses `FFI::AutoPointer` to ensure automatic cleanup.
`AutoPointer` calls the `release` class method when the Ruby object is
garbage collected, which invokes the C ABI `_destroy` function:

```ruby
class ContactPtr < FFI::AutoPointer
  def self.release(ptr)
    WeaveFFI.weaveffi_contacts_Contact_destroy(ptr)
  end
end
```

For explicit lifetime control, call `destroy` to free immediately:

```ruby
contact = WeaveFFI.get_contact(handle)
puts contact.first_name
contact.destroy
```

### Maps

Maps are passed across the FFI boundary as parallel arrays of keys and
values plus a length. The wrapper builds `FFI::MemoryPointer` buffers for
keys and values, and reconstructs a Ruby `Hash` from the returned arrays
using `each_with_object`.

## Configuration

The Ruby module name and gem name can be customized via generator
configuration:

```toml
ruby_module_name = "MyBindings"
ruby_gem_name = "my_bindings"
```

This changes the generated `module MyBindings` declaration and the
gemspec `s.name` field.
