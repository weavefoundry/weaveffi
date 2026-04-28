# Go

## Overview

The Go target produces idiomatic Go bindings that use CGo to call the C
ABI. The generator emits one Go source file (`weaveffi.go`) plus a
`go.mod` so the result can be imported by any Go module. Functions
return `(value, error)` to match Go conventions; struct wrappers expose
methods plus an explicit `Close()`.

## What gets generated

| File | Purpose |
|------|---------|
| `go/weaveffi.go` | CGo bindings: preamble, type wrappers, function wrappers |
| `go/go.mod` | Go module descriptor (configurable module path) |
| `go/README.md` | Prerequisites and build instructions |

## Type mapping

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

Booleans map to `C._Bool`, matching CGo's representation of `_Bool`.

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

      - name: count_contacts
        params: []
        return: i32
```

The generated `weaveffi.go` opens with the CGo preamble:

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

Enums become typed integer aliases:

```go
type ContactType int32

const (
	ContactTypePersonal ContactType = 0
	ContactTypeWork     ContactType = 1
	ContactTypeOther    ContactType = 2
)
```

Structs hold a typed C pointer and expose getters plus `Close()`:

```go
type Contact struct {
	ptr *C.weaveffi_contacts_Contact
}

func (s *Contact) FirstName() string {
	return C.GoString(C.weaveffi_contacts_Contact_get_first_name(s.ptr))
}

func (s *Contact) Email() *string {
	cStr := C.weaveffi_contacts_Contact_get_email(s.ptr)
	if cStr == nil { return nil }
	v := C.GoString(cStr)
	return &v
}

func (s *Contact) Close() {
	if s.ptr != nil {
		C.weaveffi_contacts_Contact_destroy(s.ptr)
		s.ptr = nil
	}
}
```

Functions return `(value, error)`:

```go
func ContactsCreateContact(firstName string, email *string, contactType ContactType) (int64, error) {
	cFirstName := C.CString(firstName)
	defer C.free(unsafe.Pointer(cFirstName))
	var cEmail *C.char
	if email != nil {
		cEmail = C.CString(*email)
		defer C.free(unsafe.Pointer(cEmail))
	}
	var cErr C.weaveffi_error
	result := C.weaveffi_contacts_create_contact(
		cFirstName, cEmail, C.weaveffi_contacts_ContactType(contactType), &cErr)
	if cErr.code != 0 {
		goErr := fmt.Errorf("weaveffi: %s (code %d)",
			C.GoString(cErr.message), int(cErr.code))
		C.weaveffi_error_clear(&cErr)
		return 0, goErr
	}
	return int64(result), nil
}
```

Lists round-trip through `unsafe.Slice`:

```go
var cOutLen C.size_t
result := C.weaveffi_store_list_ids(&cOutLen, &cErr)
count := int(cOutLen)
if count == 0 || result == nil { return nil, nil }
goResult := make([]int32, count)
cSlice := unsafe.Slice((*C.int32_t)(unsafe.Pointer(result)), count)
for i, v := range cSlice { goResult[i] = int32(v) }
```

The Go module path defaults to `weaveffi`; override it via the
generator config:

```yaml
version: "0.3.0"
modules:
  - name: math
    functions:
      - name: add
        params:
          - { name: a, type: i32 }
          - { name: b, type: i32 }
        return: i32
generators:
  go:
    module_path: "github.com/myorg/mylib"
```

## Build instructions

1. Generate the bindings:

   ```bash
   weaveffi generate --input api.yaml --output generated/ --target go
   ```

2. Build the Rust shared library:

   ```bash
   cargo build --release -p your_library
   ```

3. Point CGo at the header and library:

   ```bash
   export CGO_CFLAGS="-I$PWD/generated/c"
   export CGO_LDFLAGS="-L$PWD/target/release -lweaveffi"
   ```

4. Build and run a Go consumer:

   ```bash
   cd generated/go
   go build ./...
   ```

CGo requires a C compiler (`gcc` or `clang`) on the host; on Windows
use a MinGW-w64 toolchain or the MSVC build provided by `go env`.

## Memory and ownership

- **Strings in:** `C.CString` allocates a copy in C memory; the
  generated wrapper pairs every `CString` with a `defer C.free(...)`.
- **Strings out:** `C.GoString` copies the C string into Go-owned
  memory, then the wrapper calls `weaveffi_free_string` to release the
  Rust allocation.
- **Bytes:** input slices are passed by pointer for the duration of
  the call (no copy); returned bytes are copied with `C.GoBytes` and
  then `weaveffi_free_bytes` is called.
- **Structs:** wrappers hold a typed C pointer. Always pair with
  `defer s.Close()` because Go has no deterministic destructors.
- **Optionals:** scalar optionals are `*T`; struct/string optionals
  rely on a nil pointer to indicate absence.

## Async support

Async IDL functions are exposed as Go functions that return a typed
channel and an `error`. The wrapper allocates a Go-side struct,
registers it with the CGo handle table, hands the C ABI a callback,
and resolves the channel when the callback fires:

```go
func ContactsFetchContact(id int32) (<-chan ContactsFetchContactResult, error) {
    ch := make(chan ContactsFetchContactResult, 1)
    handle := cgo.NewHandle(ch)
    var cErr C.weaveffi_error
    C.weaveffi_contacts_fetch_contact_async(C.int32_t(id),
        C.weaveffi_callback(C.weaveffi_go_async_trampoline),
        unsafe.Pointer(&handle), &cErr)
    if cErr.code != 0 {
        handle.Delete()
        msg := C.GoString(cErr.message)
        C.weaveffi_error_clear(&cErr)
        return nil, fmt.Errorf("weaveffi: %s (code %d)", msg, int(cErr.code))
    }
    return ch, nil
}
```

When the IDL marks the function `cancel: true`, the wrapper accepts a
`context.Context` and forwards cancellation to the underlying
`weaveffi_cancel_token`.

## Troubleshooting

- **`undefined reference to weaveffi_*`** — `CGO_LDFLAGS` is missing
  the `-l` flag or `-L` directory. Recheck the environment exports.
- **`could not determine kind of name` in CGo** — ensure
  `CGO_CFLAGS` points at the directory containing `weaveffi.h`.
- **Crashes after struct goes out of scope** — Go does not call
  `Close()` for you. Either `defer s.Close()` or wrap usage in a
  helper that takes a closure.
- **`go: cannot find module providing package weaveffi`** — change
  the generator config so `go.mod` declares the module path you
  actually import, e.g. `github.com/myorg/mylib`.
