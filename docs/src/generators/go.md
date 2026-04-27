# Go

The Go generator produces idiomatic Go bindings that use CGo to call the
C ABI directly. It generates a single Go source file and a `go.mod` module
descriptor, ready to be imported by any Go project.

## Why CGo?

- **Standard toolchain.** CGo is part of the Go distribution — no third-party
  tools or custom build steps needed.
- **Direct C interop.** Go can call C functions through the `import "C"`
  pseudo-package with minimal overhead.
- **Stable ABI.** The generated code links against the same stable C ABI
  shared library used by all other language targets.

The trade-off is that CGo builds are slower than pure Go and require a C
compiler (gcc or clang) to be available. For FFI workloads the overhead is
negligible compared to the work done inside the Rust library.

## Generated artifacts

| File | Purpose |
|------|---------|
| `go/weaveffi.go` | CGo bindings: preamble, type wrappers, function wrappers |
| `go/go.mod` | Go module descriptor (configurable module path) |
| `go/README.md` | Prerequisites and build instructions |

## The CGo approach

The generated `weaveffi.go` file opens with a CGo preamble comment block
that tells the Go toolchain how to link the shared library and which
headers to include:

```go
package weaveffi

/*
#cgo LDFLAGS: -lweaveffi
#include "weaveffi.h"
#include <stdlib.h>
*/
import "C"

import (
	"fmt"
	"unsafe"
)
```

The `#cgo LDFLAGS` directive links against `libweaveffi`. At build time,
CGo compiles the preamble with a C compiler and generates the glue code
that lets Go call C functions. The `unsafe` package is imported only when
the API includes string, bytes, or collection types that require pointer
manipulation.

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

      - name: delete_contact
        params:
          - { name: id, type: handle }
        return: bool

      - name: count_contacts
        params: []
        return: i32
```

### Enums

Enums map to Go `int32` type aliases with named constants:

```go
type ContactType int32

const (
	ContactTypePersonal ContactType = 0
	ContactTypeWork     ContactType = 1
	ContactTypeOther    ContactType = 2
)
```

### Structs (opaque wrapper types)

Structs are represented as Go structs holding a pointer to the
Rust-allocated opaque C type. Field access is through getter methods
that call the C ABI getter functions. A `Close()` method calls the
C ABI destroy function to free the underlying memory:

```go
type Contact struct {
	ptr *C.weaveffi_contacts_Contact
}

func (s *Contact) Id() int64 {
	return int64(C.weaveffi_contacts_Contact_get_id(s.ptr))
}

func (s *Contact) FirstName() string {
	return C.GoString(C.weaveffi_contacts_Contact_get_first_name(s.ptr))
}

func (s *Contact) Email() *string {
	cStr := C.weaveffi_contacts_Contact_get_email(s.ptr)
	if cStr == nil {
		return nil
	}
	v := C.GoString(cStr)
	return &v
}

func (s *Contact) ContactType() ContactType {
	return ContactType(C.weaveffi_contacts_Contact_get_contact_type(s.ptr))
}

func (s *Contact) Close() {
	if s.ptr != nil {
		C.weaveffi_contacts_Contact_destroy(s.ptr)
		s.ptr = nil
	}
}
```

### Functions

Each IDL function becomes a Go function with PascalCase naming
(`module_function` becomes `ModuleFunction`). Every function returns
an `error` as its last return value. The wrapper marshals Go types to
C types, calls the C ABI function, checks for errors, and converts
the result back:

```go
func ContactsCreateContact(firstName string, lastName string, email *string, contactType ContactType) (int64, error) {
	cFirstName := C.CString(firstName)
	defer C.free(unsafe.Pointer(cFirstName))
	cLastName := C.CString(lastName)
	defer C.free(unsafe.Pointer(cLastName))
	var cEmail *C.char
	if email != nil {
		cEmail = C.CString(*email)
		defer C.free(unsafe.Pointer(cEmail))
	}
	var cErr C.weaveffi_error
	result := C.weaveffi_contacts_create_contact(cFirstName, cLastName, cEmail, C.weaveffi_contacts_ContactType(contactType), &cErr)
	if cErr.code != 0 {
		goErr := fmt.Errorf("weaveffi: %s (code %d)", C.GoString(cErr.message), int(cErr.code))
		C.weaveffi_error_clear(&cErr)
		return 0, goErr
	}
	return int64(result), nil
}

func ContactsGetContact(id int64) (*Contact, error) {
	var cErr C.weaveffi_error
	result := C.weaveffi_contacts_get_contact(C.weaveffi_handle_t(id), &cErr)
	if cErr.code != 0 {
		goErr := fmt.Errorf("weaveffi: %s (code %d)", C.GoString(cErr.message), int(cErr.code))
		C.weaveffi_error_clear(&cErr)
		return nil, goErr
	}
	return &Contact{ptr: result}, nil
}

func ContactsCountContacts() (int32, error) {
	var cErr C.weaveffi_error
	result := C.weaveffi_contacts_count_contacts(&cErr)
	if cErr.code != 0 {
		goErr := fmt.Errorf("weaveffi: %s (code %d)", C.GoString(cErr.message), int(cErr.code))
		C.weaveffi_error_clear(&cErr)
		return 0, goErr
	}
	return int32(result), nil
}
```

Void functions return only `error`:

```go
func SystemReset() error {
	var cErr C.weaveffi_error
	C.weaveffi_system_reset(&cErr)
	if cErr.code != 0 {
		goErr := fmt.Errorf("weaveffi: %s (code %d)", C.GoString(cErr.message), int(cErr.code))
		C.weaveffi_error_clear(&cErr)
		return goErr
	}
	return nil
}
```

### Optional handling

Optional types map to Go pointer types (`*T`) for scalars and strings.
Struct and collection optionals use their natural nil-able representations
(pointers and slices are already nil-able in Go):

```go
// Optional scalar parameter: *int32
var cId *C.int32_t
if id != nil {
	tmp := C.int32_t(*id)
	cId = &tmp
}

// Optional string parameter: *string
var cEmail *C.char
if email != nil {
	cEmail = C.CString(*email)
	defer C.free(unsafe.Pointer(cEmail))
}

// Optional struct return: *Contact
if result == nil {
	return nil, nil
}
return &Contact{ptr: result}, nil
```

### List/Array handling

List types map to Go slices (`[]T`). Parameters are passed as pointer+length
pairs to the C ABI. Return values are converted from a C pointer+length
pair using `unsafe.Slice`:

```go
// List return: []int32
var cOutLen C.size_t
result := C.weaveffi_store_list_ids(&cOutLen, &cErr)
// ... error check ...
count := int(cOutLen)
if count == 0 || result == nil {
	return nil, nil
}
goResult := make([]int32, count)
cSlice := unsafe.Slice((*C.int32_t)(unsafe.Pointer(result)), count)
for i, v := range cSlice {
	goResult[i] = int32(v)
}
return goResult, nil
```

## Type mapping reference

| IDL type     | Go type       | C type (CGo)               |
|--------------|---------------|----------------------------|
| `i32`        | `int32`       | `C.int32_t`                |
| `u32`        | `uint32`      | `C.uint32_t`               |
| `i64`        | `int64`       | `C.int64_t`                |
| `f64`        | `float64`     | `C.double`                 |
| `bool`       | `bool`        | `C._Bool`                  |
| `string`     | `string`      | `*C.char` (via `C.CString`/`C.GoString`) |
| `bytes`      | `[]byte`      | `*C.uint8_t` + `C.size_t`  |
| `handle`     | `int64`       | `C.weaveffi_handle_t`      |
| `Struct`     | `*StructName` | `*C.weaveffi_mod_Struct`   |
| `Enum`       | `EnumName`    | `C.weaveffi_mod_Enum`      |
| `T?`         | `*T`          | pointer to scalar; nil-able pointer for strings/structs |
| `[T]`        | `[]T`         | pointer + `C.size_t`       |
| `{K: V}`     | `map[K]V`     | key/value arrays + `C.size_t` |

Booleans use `C._Bool` rather than an integer type, matching the CGo
mapping of C's `_Bool`.

## Error handling

Every generated Go function returns `error` as its last return value,
following Go's idiomatic error-handling convention. The C ABI uses a
`weaveffi_error` struct (with `code` and `message` fields) as an out
parameter on every function call.

The generated wrapper:
1. Declares a `C.weaveffi_error` variable.
2. Passes its address as the last argument to the C function.
3. Checks `cErr.code != 0` after the call.
4. On error, extracts the message with `C.GoString`, clears the C-side
   error with `C.weaveffi_error_clear`, and returns a Go `error` via
   `fmt.Errorf` along with the zero value for the return type.

```go
var cErr C.weaveffi_error
result := C.weaveffi_calculator_add(C.int32_t(a), C.int32_t(b), &cErr)
if cErr.code != 0 {
	goErr := fmt.Errorf("weaveffi: %s (code %d)", C.GoString(cErr.message), int(cErr.code))
	C.weaveffi_error_clear(&cErr)
	return 0, goErr
}
return int32(result), nil
```

Callers use standard Go error checking:

```go
sum, err := weaveffi.CalculatorAdd(2, 3)
if err != nil {
	log.Fatalf("add failed: %v", err)
}
fmt.Println(sum)
```

## Memory management

### Strings

- **Passing strings in:** Go strings are converted to C strings via
  `C.CString()`, which allocates a copy in C memory. A `defer C.free()`
  ensures the copy is freed after the C call returns.
- **Receiving strings back:** Returned C strings are converted to Go
  strings via `C.GoString()`, which copies the data into Go-managed
  memory. The wrapper then calls `C.weaveffi_free_string()` to free
  the Rust-allocated original.

### Bytes

- **Passing bytes in:** A pointer to the first element of the byte slice
  is passed with a length parameter. The slice data is valid for the
  duration of the C call (no copy needed).
- **Receiving bytes back:** The wrapper uses `C.GoBytes()` to copy the
  data into a Go byte slice, then calls `C.weaveffi_free_bytes()` to
  free the Rust-allocated buffer.

### Structs (opaque pointers)

Struct wrappers hold a typed C pointer (`*C.weaveffi_mod_Struct`). The
`Close()` method calls the corresponding `_destroy` C function to free
the Rust-side allocation and sets the pointer to nil to prevent
double-free:

```go
func (s *Contact) Close() {
	if s.ptr != nil {
		C.weaveffi_contacts_Contact_destroy(s.ptr)
		s.ptr = nil
	}
}
```

Unlike Swift (which uses `deinit`) or Python (which uses `__del__`),
Go does not have deterministic destructors. Callers must explicitly
call `Close()` when done with a struct, or use `defer`:

```go
contact, err := weaveffi.ContactsGetContact(id)
if err != nil {
	log.Fatal(err)
}
defer contact.Close()
fmt.Println(contact.FirstName())
```

### Boolean helpers

When the API uses boolean types, the generator includes helper functions
to convert between Go `bool` and CGo `C._Bool`:

```go
func boolToC(b bool) C._Bool {
	if b {
		return 1
	}
	return 0
}

func cToBool(b C._Bool) bool {
	return b != 0
}
```

These helpers are only emitted when the API actually uses booleans,
keeping the generated code minimal.

## Build and usage

### 1. Generate bindings

```bash
weaveffi generate --input api.yaml --output generated/ --target go
```

### 2. Build the Rust shared library

```bash
cargo build --release -p your_library
```

### 3. Set up CGo environment

Point CGo at the header and shared library:

```bash
export CGO_CFLAGS="-I/path/to/headers"
export CGO_LDFLAGS="-L/path/to/lib -lweaveffi"
```

### 4. Use in your Go project

```go
package main

import (
	"fmt"
	"log"

	"weaveffi"
)

func main() {
	sum, err := weaveffi.CalculatorAdd(2, 3)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("2 + 3 = %d\n", sum)
}
```

## Configuration

The `go.mod` module path defaults to `weaveffi` but can be customized
via generator configuration:

```yaml
generators:
  go:
    module_path: "github.com/myorg/mylib"
```

This produces a `go.mod` with `module github.com/myorg/mylib` instead
of the default.
