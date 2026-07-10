# Go

## Overview

The Go target produces idiomatic Go bindings that use CGo to call the C
ABI. The generator emits one Go source file (`weaveffi.go`) plus a
`go.mod` so the result can be imported by any Go module. Functions
marked `throws: true` return `(value, error)` to match Go conventions;
all other wrappers return plain values. Struct and interface wrappers
expose methods plus an explicit `Close()`.

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
| `i8`         | `int8`        | `C.int8_t`                 |
| `i16`        | `int16`       | `C.int16_t`                |
| `u8`         | `uint8`       | `C.uint8_t`                |
| `u16`        | `uint16`      | `C.uint16_t`               |
| `u64`        | `uint64`      | `C.uint64_t`               |
| `f32`        | `float32`     | `C.float`                  |
| `bool`       | `bool`        | `C._Bool`                  |
| `string`     | `string`      | `*C.char` (via `C.CString`/`C.GoString`) |
| `bytes`      | `[]byte`      | `*C.uint8_t` + `C.size_t`  |
| `handle`     | `int64`       | `C.weaveffi_handle_t`      |
| `Struct`     | `*StructName` | `*C.weaveffi_mod_Struct`   |
| `Interface`  | `*InterfaceName` | `*C.weaveffi_mod_Interface` |
| `Enum` (plain) | `EnumName`  | `C.weaveffi_mod_Enum`      |
| `Enum` (rich)  | `*EnumName` | `*C.weaveffi_mod_Enum`     |
| `T?`         | `*T`          | pointer to scalar; nil-able pointer for strings/structs |
| `[T]`        | `[]T`         | pointer + `C.size_t`       |
| `{K: V}`     | `map[K]V`     | key/value arrays + `C.size_t` |
| `iter<T>`    | `[]T` (drained eagerly) | opaque iterator pointer + `_next`/`_destroy` |

Booleans map to `C._Bool`, matching CGo's representation of `_Bool`.

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

Function wrappers are PascalCase with the IDL module prefix stripped
(`CreateContact`, not `ContactsCreateContact`); set
`strip_module_prefix: false` in the Go generator config (or under
`[global]`) to keep prefixed names. A function without `throws` returns
a plain value; its error slot is checked by `wvTrap`, which panics,
because a non-zero code there can only be a producer panic or a
marshalling failure:

```go
func CreateContact(firstName string, email *string, contactType ContactType) int64 {
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
	wvTrap(&cErr)
	return int64(result)
}
```

A function marked `throws: true` returns `(value, error)` instead; see
[Typed errors](#typed-errors).

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
version: "0.5.0"
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

## Typed errors

The package defines a generic `WeaveFFIError` struct with `Code` and
`Message` fields. A module's error domain adds a typed error struct
named after the domain, package-level code constants, and a mapper
that falls back to `*WeaveFFIError` for codes outside the domain. From
the `kvstore` sample:

```go
// KvError is a typed error reported by the `kv` module.
type KvError struct {
	// Code is the numeric ABI error code (one of the KvError constants).
	Code int32
	// Message is the human-readable error message.
	Message string
}

func (e *KvError) Error() string {
	return fmt.Sprintf("kv: %s (code %d)", e.Message, e.Code)
}

// KvError codes.
const (
	// KvErrorKeyNotFound key not found
	KvErrorKeyNotFound int32 = 1001
	// KvErrorExpired entry expired
	KvErrorExpired int32 = 1002
	// KvErrorStoreFull store has reached capacity
	KvErrorStoreFull int32 = 1003
	// KvErrorIoError: I/O failure
	KvErrorIoError int32 = 1004
)
```

A callable marked `throws: true` returns `(value, error)` and maps a
non-zero error slot through the domain mapper (`wvMapKv`); match it
with `errors.As` and compare the code constants:

```go
_, err := store.Delete("missing")
var kvErr *KvError
if errors.As(err, &kvErr) && kvErr.Code == KvErrorKeyNotFound {
	// specific code
}
```

A callable without `throws` returns a plain value and checks its slot
with `wvTrap`, which panics on the codes that can only mean a producer
bug.

## Interfaces

An `interfaces:` entry becomes a struct holding the typed C pointer.
Constructors become package-level factory functions combining the
constructor and type names (`open` becomes `OpenStore`, `new` becomes
`NewContactBook`), methods hang off the wrapper, statics become
package-level functions prefixed by the type name
(`StoreDefaultCapacity`), and `Close()` frees the native object. From
the `kvstore` sample (trimmed):

```go
type Store struct {
	ptr *C.weaveffi_kv_Store
}

// OpenStore: Open (or create) a store backed by the given filesystem path
func OpenStore(path string) (*Store, error) {
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	var cErr C.weaveffi_error
	result := C.weaveffi_kv_Store_open(cPath, &cErr)
	if cErr.code != 0 {
		return nil, wvMapKv(wvTakeError(&cErr))
	}
	return &Store{ptr: result}, nil
}

// Put: Insert or replace a value, returning true on success
func (s *Store) Put(key string, value []byte, kind EntryKind, ttlSeconds *int64) (bool, error) { /* ... */ }

// Count: Return the number of live entries in the store
func (s *Store) Count() int64 {
	var cErr C.weaveffi_error
	result := C.weaveffi_kv_Store_count(s.ptr, &cErr)
	wvTrap(&cErr)
	return int64(result)
}

// Compact: Reclaim space asynchronously; returns the number of bytes reclaimed
// Blocks until the async producer completes.
func (s *Store) Compact() (int64, error) { /* see Async support */ }

// LegacyPut: Legacy single-shot put kept for compatibility
// Deprecated: use put() with explicit kind
func (s *Store) LegacyPut(key string, value []byte) (bool, error) { /* ... */ }

// StoreDefaultCapacity: The largest number of live entries one store will hold
func StoreDefaultCapacity() int64 { /* ... */ }

func (s *Store) Close() {
	if s.ptr != nil {
		C.weaveffi_kv_Store_destroy(s.ptr)
		s.ptr = nil
	}
}
```

Functions elsewhere in the IDL pass the wrapper's pointer across the
boundary (`GetStats(store)` returns a new `*Stats`). Deprecated
members carry a standard `// Deprecated:` comment that `go vet` and
editors understand. As with structs, pair every wrapper with
`defer store.Close()`:

```go
store, err := OpenStore("/tmp/cache.kv")
if err != nil {
	return err
}
defer store.Close()
ok, err := store.Put("alpha", []byte{1}, EntryKindPersistent, nil)
fmt.Println(store.Count(), StoreDefaultCapacity())
```

## Rich (algebraic) enums

A *rich* (algebraic) enum, a sum type whose variants carry associated
data, lowers to an **opaque object pointer** at the C ABI, exactly like a
struct, and shares the same ownership model as the struct wrappers above.
The Go wrapper is a struct holding a typed C pointer, with one
`New<Enum><Variant>` constructor per variant, a `Tag()` method returning
the `int32` discriminant, per-variant field getter methods, and an
explicit `Close()`. (A plain C-style enum with no payloads stays a typed
`int32` alias with `const` values; see above.)

For the `shapes` module's `Shape` enum (`Empty`, `Circle { radius: f64 }`,
`Rectangle { width: f32, height: f32 }`, and
`Labeled { label: string, count: u8 }`), the generator emits (abridged):

```go
// Shape: An algebraic shape (sum type with associated data)
type Shape struct {
	ptr *C.weaveffi_shapes_Shape
}

const (
	// ShapeEmpty: The empty shape
	ShapeEmpty int32 = 0
	// ShapeCircle: A circle with a radius
	ShapeCircle int32 = 1
	// ShapeRectangle: An axis-aligned rectangle
	ShapeRectangle int32 = 2
	// ShapeLabeled: A labeled shape with a small count
	ShapeLabeled int32 = 3
)

func (s *Shape) Tag() int32 {
	return int32(C.weaveffi_shapes_Shape_tag(s.ptr))
}

// NewShapeCircle: A circle with a radius
func NewShapeCircle(radius float64) (*Shape, error) {
	var cErr C.weaveffi_error
	result := C.weaveffi_shapes_Shape_Circle_new(C.double(radius), &cErr)
	if cErr.code != 0 {
		goErr := fmt.Errorf("weaveffi: %s (code %d)", C.GoString(cErr.message), int(cErr.code))
		C.weaveffi_error_clear(&cErr)
		return nil, goErr
	}
	return &Shape{ptr: result}, nil
}

// NewShapeLabeled: A labeled shape with a small count
func NewShapeLabeled(label string, count uint8) (*Shape, error) {
	cLabel := C.CString(label)
	defer C.free(unsafe.Pointer(cLabel))
	var cErr C.weaveffi_error
	result := C.weaveffi_shapes_Shape_Labeled_new(cLabel, C.uint8_t(count), &cErr)
	if cErr.code != 0 {
		goErr := fmt.Errorf("weaveffi: %s (code %d)", C.GoString(cErr.message), int(cErr.code))
		C.weaveffi_error_clear(&cErr)
		return nil, goErr
	}
	return &Shape{ptr: result}, nil
}

// CircleRadius: Radius in points
func (s *Shape) CircleRadius() float64 {
	return float64(C.weaveffi_shapes_Shape_Circle_get_radius(s.ptr))
}

func (s *Shape) LabeledCount() uint8 {
	return uint8(C.weaveffi_shapes_Shape_Labeled_get_count(s.ptr))
}

func (s *Shape) Close() {
	if s.ptr != nil {
		C.weaveffi_shapes_Shape_destroy(s.ptr)
		s.ptr = nil
	}
}
```

Each `NewShape<Variant>` calls a per-variant constructor
(`weaveffi_shapes_Shape_<Variant>_new`); `Tag()` reads the discriminant
(`weaveffi_shapes_Shape_tag`) and can be compared against the package
constants `ShapeEmpty`/`ShapeCircle`/`ShapeRectangle`/`ShapeLabeled`; the
getter methods read one variant field
(`weaveffi_shapes_Shape_<Variant>_get_<field>`); and `Close()` frees the
pointer (`weaveffi_shapes_Shape_destroy`). Free functions that take or
return the enum pass the wrapper's pointer across the boundary
(`Describe(*Shape)`, `Scale(*Shape, float64)`; both are non-throwing
here, so they return plain values):

```go
c, err := NewShapeCircle(2.0)
if err != nil {
	return err
}
defer c.Close()
fmt.Println(c.Tag() == ShapeCircle) // true
fmt.Println(c.CircleRadius())       // 2

bigger := Scale(c, 3.0) // returns a new *Shape
defer bigger.Close()
fmt.Println(Describe(bigger))
```

**Ownership:** a `*Shape` owns its native pointer. Go has no deterministic
destructors, so pair every constructor (and every `*Shape` returned by
`Scale`) with `defer s.Close()`.

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
- **Structs and interfaces:** wrappers hold a typed C pointer. Always
  pair with `defer s.Close()` because Go has no deterministic
  destructors.
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
// Returns a subscription id for UnregisterMessageListener.
func RegisterMessageListener(callback func(message string)) uint64 {
	ctxID := wvCallbackStore(callback)
	id := uint64(C.weaveffi_events_register_message_listener(
		C.weaveffi_events_OnMessage_fn(unsafe.Pointer(C.goWv_weaveffi_events_OnMessage_fn)),
		unsafe.Pointer(uintptr(ctxID))))
	wvCallbackMu.Lock()
	wvListenerCtx[id] = ctxID
	wvCallbackMu.Unlock()
	return id
}

func UnregisterMessageListener(id uint64) {
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
the registry key as the `void* context`, an integer id cast via
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
the producer fires it from (in the events sample, synchronously
inside `SendMessage`). Don't block in it; forward to a channel
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
func RunTask(name string) (*TaskResult, error) {
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
a single `wvOutcome…` value. For a callable marked `throws: true`, the
trampoline maps the error through the domain mapper, so the returned
`error` is the typed one (`*KvError` from `store.Compact()`). The
native producer already runs on its own thread, so the wrapper simply
blocks the calling goroutine; callers that want concurrency run the
call from a goroutine of their own.

For functions marked `cancellable: true` the C launcher gains a
`weaveffi_cancel_token*` parameter. The Go wrapper passes `nil` for it
and doesn't expose the token; only the C and C++ targets
surface cancellation tokens.

## Iterators

`iter<T>` returns map to plain `[]T` (plus an `error` when the
function throws, as with `Store.ListKeys`). The wrapper obtains the
opaque iterator pointer, drains it eagerly through the generated
`_next` symbol, and destroys it before returning:

```go
func GetMessages() []string {
	var cErr C.weaveffi_error
	it := C.weaveffi_events_get_messages(&cErr)
	wvTrap(&cErr)
	defer C.weaveffi_events_GetMessagesIterator_destroy(it)
	goResult := []string{}
	for {
		var outItem *C.char
		var iterErr C.weaveffi_error
		if C.weaveffi_events_GetMessagesIterator_next(it, &outItem, &iterErr) == 0 {
			break
		}
		wvTrap(&iterErr)
		goResult = append(goResult, C.GoString(outItem))
		C.weaveffi_free_string(outItem)
	}
	return goResult
}
```

Each yielded element is copied into Go memory and its Rust allocation
released (strings via `weaveffi_free_string`); the iterator handle is
destroyed by the deferred `_destroy` call.

## Troubleshooting

- **`undefined reference to weaveffi_*`**: `CGO_LDFLAGS` is missing
  the `-l` flag or `-L` directory. Recheck the environment exports.
- **`could not determine kind of name` in CGo**: ensure
  `CGO_CFLAGS` points at the directory containing `weaveffi.h`.
- **Crashes after struct goes out of scope**: Go doesn't call
  `Close()` for you. Either `defer s.Close()` or wrap usage in a
  helper that takes a closure.
- **`go: cannot find module providing package weaveffi`**: change
  the generator config so `go.mod` declares the module path you
  actually import, e.g. `github.com/myorg/mylib`.
