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
| `iter<T>`    | `[]T` (drained eagerly) | opaque iterator pointer + `_next`/`_destroy` |

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
   weaveffi generate api.yaml -o generated --target go
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

## Callbacks and listeners

A `callbacks:` entry in the IDL defines a C function-pointer type; a
`listeners:` entry generates a register/unregister pair around it:

```yaml
modules:
  - name: events
    callbacks:
      - name: OnMessage
        params:
          - { name: message, type: string }
    listeners:
      - name: message_listener
        event_callback: OnMessage
```

The C ABI is `weaveffi_events_register_message_listener(callback,
void* context)`, which returns a `uint64_t` subscription id, plus
`weaveffi_events_unregister_message_listener(id)`. The Go surface
takes a closure and returns that id:

```go
// Returns a subscription id for EventsUnregisterMessageListener.
func EventsRegisterMessageListener(callback func(message string)) uint64 {
	ctxID := wvCallbackStore(callback)
	id := uint64(C.weaveffi_events_register_message_listener(
		C.weaveffi_events_OnMessage_fn(unsafe.Pointer(C.goWv_weaveffi_events_OnMessage_fn)),
		unsafe.Pointer(uintptr(ctxID))))
	wvCallbackMu.Lock()
	wvListenerCtx[id] = ctxID
	wvCallbackMu.Unlock()
	return id
}

func EventsUnregisterMessageListener(id uint64) {
	C.weaveffi_events_unregister_message_listener(C.uint64_t(id))
	wvCallbackMu.Lock()
	ctxID, ok := wvListenerCtx[id]
	delete(wvListenerCtx, id)
	wvCallbackMu.Unlock()
	if ok {
		wvCallbackDelete(ctxID)
	}
}
```

CGo forbids passing Go pointers to C, so the closure itself never
crosses the boundary. The bindings keep a mutex-guarded registry
(`wvCallbacks`, written through `wvCallbackStore`) and hand C two
things: a `//export`ed trampoline (`goWv_weaveffi_events_OnMessage_fn`,
declared `extern` in the CGo preamble) as the function pointer, and
the registry key as the `void* context` — an integer id cast via
`unsafe.Pointer(uintptr(ctxID))` that the C side never dereferences.
When the event fires, the trampoline looks the closure up and calls it:

```go
//export goWv_weaveffi_events_OnMessage_fn
func goWv_weaveffi_events_OnMessage_fn(message *C.char, context unsafe.Pointer) {
	v := wvCallbackLoad(uint64(uintptr(context)))
	if v == nil {
		return
	}
	cb := v.(func(message string))
	arg0 := ""
	if message != nil {
		arg0 = C.GoString(message)
	}
	cb(arg0)
}
```

- **Subscription ids:** the native library mints the `uint64` id; pair
  every register with exactly one unregister. Unregistering tears down
  the native subscription, then uses `wvListenerCtx` (subscription id →
  registry key) to delete the stored closure so it can be collected. A
  leaked subscription pins the closure forever.
- **Threading:** the callback runs as a CGo callback on whatever thread
  the producer fires it from — in the events sample, synchronously
  inside `EventsSendMessage`. Don't block in it; forward to a channel
  or goroutine if handling is slow.

## Async support

Functions marked `async: true` are exposed through `_async`-suffixed C
launchers that take a completion callback plus `void* context`. The
generated Go wrapper turns that into a plain blocking call: it makes a
buffered channel, stores it in the same callback registry the listener
bindings use, launches the C call with an exported trampoline and the
integer context id, then receives from the channel:

```go
// Blocks until the async producer completes.
func TasksRunTask(name string) (*TaskResult, error) {
	ch := make(chan wvOutcomeTasksRunTask, 1)
	ctxID := wvCallbackStore(ch)
	cName := C.CString(name)
	defer C.free(unsafe.Pointer(cName))
	C.weaveffi_tasks_run_task_async(cName,
		C.weaveffi_tasks_run_task_callback(unsafe.Pointer(C.goWv_weaveffi_tasks_run_task_callback)),
		unsafe.Pointer(uintptr(ctxID)))
	outcome := <-ch
	if outcome.err != nil {
		return nil, outcome.err
	}
	return outcome.val, nil
}
```

The completion trampoline removes the channel from the registry with
`wvCallbackTake` (one-shot), converts the C error or result, and sends
a single `wvOutcome…` value. The native producer already runs on its
own thread, so the wrapper simply blocks the calling goroutine; callers
that want concurrency run the call from a goroutine of their own.

For functions marked `cancellable: true` the C launcher gains a
`weaveffi_cancel_token*` parameter. The Go wrapper passes `nil` for it
and does not expose the token — only the C, C++, and Kotlin targets
surface cancellation tokens.

## Iterators

`iter<T>` returns map to plain `[]T`. The wrapper obtains the opaque
iterator pointer, drains it eagerly through the generated `_next`
symbol, and destroys it before returning:

```go
func EventsGetMessages() ([]string, error) {
	var cErr C.weaveffi_error
	it := C.weaveffi_events_get_messages(&cErr)
	// ... error check ...
	defer C.weaveffi_events_GetMessagesIterator_destroy(it)
	goResult := []string{}
	for {
		var outItem *C.char
		var iterErr C.weaveffi_error
		if C.weaveffi_events_GetMessagesIterator_next(it, &outItem, &iterErr) == 0 {
			break
		}
		// ... error check ...
		goResult = append(goResult, C.GoString(outItem))
		C.weaveffi_free_string(outItem)
	}
	return goResult, nil
}
```

Each yielded element is copied into Go memory and its Rust allocation
released (strings via `weaveffi_free_string`); the iterator handle is
destroyed by the deferred `_destroy` call.

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
