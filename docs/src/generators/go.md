# Go

## Overview

The Go target produces idiomatic Go bindings that use cgo to call the C
ABI (revision 2). The generator emits one Go source file (`weaveffi.go`)
plus a `go.mod` so the result can be imported by any Go module.
Functions marked `throws: true` return `(value, error)` to match Go
conventions; all other wrappers return plain values and panic on the
runtime trap codes. Records and rich enums are plain Go value types
packed and unpacked from value buffers. Interfaces are reference-counted
objects behind pointer wrappers with `Close()` and a finalizer backstop.
Callback interfaces are Go `interface` types the consumer implements,
crossing as a `cgo.Handle` plus one static vtable of exported
trampolines. Async functions block the calling goroutine on a channel,
and `iter<T>` returns produce standard-library `iter.Seq`/`iter.Seq2`
sequences, so the generated module requires Go 1.23 or later (the
emitted `go.mod` declares `go 1.23`).

## What gets generated

| File | Purpose |
|------|---------|
| `go/weaveffi.go` | cgo bindings: preamble, ABI check, codecs, type wrappers, function wrappers |
| `go/go.mod` | Go module descriptor (configurable module path) |
| `go/README.md` | Prerequisites and build instructions |

The package checks the producer's ABI revision in `init()` and panics
on mismatch, so a stale library fails at load time:

```go
// The ABI revision these bindings were generated against.
const wvABIVersion uint32 = 2

func init() {
	if found := uint32(C.weaveffi_abi_version()); found != wvABIVersion {
		panic(fmt.Sprintf("WeaveFFI ABI mismatch: these bindings expect revision %d but the loaded library reports revision %d", wvABIVersion, found))
	}
}
```

## Type mapping

| IDL type     | Go type       | C type (cgo)               |
|--------------|---------------|----------------------------|
| `i8`, `i16`, `i32`, `i64` | `int8`, `int16`, `int32`, `int64` | `C.int8_t` ... `C.int64_t` |
| `u8`, `u16`, `u32`, `u64` | `uint8`, `uint16`, `uint32`, `uint64` | `C.uint8_t` ... `C.uint64_t` |
| `f32`, `f64` | `float32`, `float64` | `C.float`, `C.double` |
| `bool`       | `bool`        | `C._Bool`                  |
| `string`     | `string`      | `*C.char` (via `C.CString`/`C.GoString`) |
| `bytes`      | `[]byte`      | `*C.uint8_t` + `C.size_t`  |
| `Struct`     | `StructName` (plain struct) | value buffer (`*C.uint8_t` + `C.size_t`) |
| `Enum` (plain) | `EnumName` (`int32` alias) | `C.weaveffi_mod_Enum` |
| `Enum` (rich)  | `EnumName` (sealed interface + variant structs) | value buffer |
| `Interface`  | `*InterfaceName` | `*C.weaveffi_mod_Interface` |
| `Interface?` | `*InterfaceName` (nil-able) | `*C.weaveffi_mod_Interface` (NULL for `nil`) |
| `CallbackInterface` | `CallbackName` (Go `interface`) | `void*` ctx + `const vtable*` |
| `T?`         | `*T`          | value buffer               |
| `[T]`        | `[]T`         | value buffer               |
| `{K: V}`     | `map[K]V`     | value buffer               |
| `iter<T>`    | `iter.Seq[T]`, or `iter.Seq2[T, error]` when the function throws | opaque iterator pointer + `_next`/`_destroy` |

Buffered types cross the boundary serialized in the
[value-buffer format](../reference/value-buffers.md); the package
carries a private `wvWriter`/`wvReader` pair plus one `wvPack*` and one
`wvUnpack*` routine per record and rich enum. Objects nested inside a
buffered value travel as `u64` object tokens (see
[Objects](#objects-interfaces)). Booleans map to `C._Bool`, matching
cgo's representation of `_Bool`.

### 64-bit integers and floats

`i64` and `u64` are native `int64` and `uint64`, both across cgo and
inside value buffers (`writeI64`/`writeU64`), so the full range
round-trips exactly. `f32`/`f64` are `float32`/`float64`, packed with
`math.Float64bits`; the `codec` conformance consumer verifies NaN, both
infinities, and `-0.0` survive a round trip bit-for-bit.

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

      - name: count_contacts
        params: []
        return: i32
```

The generated `weaveffi.go` opens with the cgo preamble. The preamble
also declares the exported trampolines and static vtables for every
callback interface and async function in the IDL (from the `kvstore`
sample):

```go
package kvstore

/*
#cgo LDFLAGS: -lkvstore
#include "weaveffi.h"
#include <stdlib.h>
static void* wvHandlePtr(uintptr_t h) { return (void*)h; }
extern bool goWv_weaveffi_kv_EvictionListener_on_evict(void* ctx, uint8_t* entry_ptr, size_t entry_len, weaveffi_kv_EvictionReason reason, weaveffi_error* out_err);
extern void goWv_weaveffi_kv_EvictionListener_free(void* ctx);
static const weaveffi_kv_EvictionListener_vtable wvVtable_weaveffi_kv_EvictionListener = {
    (bool (*)(void*, const uint8_t*, size_t, weaveffi_kv_EvictionReason, weaveffi_error*))goWv_weaveffi_kv_EvictionListener_on_evict,
    goWv_weaveffi_kv_EvictionListener_free,
};
static const weaveffi_kv_EvictionListener_vtable* wvVtablePtr_weaveffi_kv_EvictionListener(void) { return &wvVtable_weaveffi_kv_EvictionListener; }
extern void goWv_weaveffi_kv_Store_compact_callback(void* context, weaveffi_error* err, int64_t result);
*/
import "C"
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

Structs become plain Go structs with exported, typed fields:

```go
// Contact is a plain value; there's no native handle and no Close.
type Contact struct {
	Id          int64
	FirstName   string
	Email       *string
	ContactType ContactType
}
```

Function wrappers are PascalCase with the IDL module prefix stripped
(`CreateContact`, not `ContactsCreateContact`); set
`strip_module_prefix: false` in the Go generator config (or under
`[global]`) to keep prefixed names. A function marked `throws: true`
returns `(value, error)`; a buffered return is copied into Go memory,
released with `weaveffi_free_bytes`, and decoded. From the `kvstore`
sample's cross-module `GetStats`:

```go
func GetStats(store *Store) (Stats, error) {
	defer runtime.KeepAlive(store)
	var cOutLen C.size_t
	var cErr C.weaveffi_error
	result := C.weaveffi_kv_stats_get_stats(store.ptr, &cOutLen, &cErr)
	if cErr.code != 0 {
		return Stats{}, wvMapKv(wvTakeError(&cErr))
	}
	rRes := &wvReader{buf: wvCopyBuffer(result, cOutLen)}
	var goResult Stats
	goResult = wvUnpackStats(rRes)
	rRes.expectEnd()
	return goResult, nil
}
```

A function without `throws` returns a plain value; its error slot is
checked by `wvTrap`, which panics, because a non-zero code there can
only be a runtime trap:

```go
// wvTrap panics when the C error slot reports a failure. Non-throwing
// wrappers check their slot with it: a non-zero code there can only be
// a producer panic, a marshalling failure, or a callback-interface
// implementation that panicked. The panic value is a *WeaveFFIError, so a
// recover() site can still inspect the code.
func wvTrap(cErr *C.weaveffi_error) {
	if cErr.code != 0 {
		panic(wvBrandError(wvTakeError(cErr)))
	}
}
```

The Go module path follows the `[package]` name in `weaveffi.toml`
(falling back to `weaveffi`); override it directly with:

```toml
[generators.go]
module_path = "github.com/myorg/mylib"
```

## Typed errors

The package defines a generic `WeaveFFIError` struct with `Code` and
`Message` fields:

```go
// WeaveFFIError reports a failure crossing the C boundary that no typed
// error domain claims: an unknown code, a marshalling failure, a
// producer panic (code -2), or a Go callback-interface implementation
// that panicked (code -4).
type WeaveFFIError struct {
	// Code is the numeric ABI error code.
	Code int32
	// Message is the human-readable error message.
	Message string
}
```

A module's error domain adds a typed error struct named after the
domain, package-level code constants, and a mapper that falls back to
`*WeaveFFIError` for codes outside the domain. From the `kvstore`
sample:

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
non-zero error slot through the domain mapper (`wvMapKv`); match it with
`errors.As` and compare the code constants:

```go
_, err := store.Delete("missing")
var kvErr *KvError
if errors.As(err, &kvErr) && kvErr.Code == KvErrorKeyNotFound {
	// specific code
}
```

A callable without `throws` returns a plain value and checks its slot
with `wvTrap`, which panics with a `*WeaveFFIError`.

An error code that declares payload `fields:` carries them serialized in
the error's payload buffer; the mapper decodes them into typed fields on
the error value before `weaveffi_error_clear` releases the buffer.

### Runtime error codes

| Code | Meaning | Where it surfaces |
|------|---------|-------------------|
| `-1` | The producer reported an error without a declared code. | `*WeaveFFIError` returned from a throwing call, or the panic value of a non-throwing one. |
| `-2` | The Rust implementation panicked; the export macros and the async spawner catch the unwind. | Same as above (a blocking async call returns it as its `error`). |
| `-3` | Malformed input at the boundary (invalid UTF-8, a truncated value buffer, a bad enum discriminant). | Same as above. |
| `-4` | A callback-interface method implemented in Go panicked. | `*WeaveFFIError` with `Code == -4` returned from the throwing producer call that invoked the callback, or the panic value of a non-throwing one (see [Callback interfaces](#callback-interfaces)). |

The panic value from `wvTrap` is a `*WeaveFFIError`, so a `recover()`
site can still inspect the code. Using a closed wrapper is a separate
panic with a plain string message.

## Objects (interfaces)

An `interfaces:` entry becomes a struct holding the typed C pointer to a
reference-counted producer object. Constructors become package-level
factory functions combining the constructor and type names (`open`
becomes `OpenStore`, `new` becomes `NewEventBus`), methods hang off the
wrapper, statics become package-level functions prefixed by the type
name (`StoreDefaultCapacity`, `StoreOpenMany`), and `Close()` releases
the reference. From the `kvstore` sample (trimmed):

```go
// Each wrapper holds one strong reference; Close releases it, and a
// finalizer releases it if the wrapper is garbage collected first.
type Store struct {
	ptr *C.weaveffi_kv_Store
}

// wvAdoptStore adopts one owned strong reference into a new wrapper. A null
// pointer adopts to nil.
func wvAdoptStore(ptr *C.weaveffi_kv_Store) *Store {
	if ptr == nil {
		return nil
	}
	s := &Store{ptr: ptr}
	runtime.SetFinalizer(s, (*Store).Close)
	return s
}

// wvTokenStore clones o's reference into a value-buffer object token. The
// wrapper keeps its own reference; the token carries the new one.
func wvTokenStore(o *Store) uint64 {
	if o == nil || o.ptr == nil {
		panic("weaveffi: nil or closed Store cannot be encoded in a non-optional position")
	}
	return uint64(uintptr(unsafe.Pointer(C.weaveffi_kv_Store_clone(o.ptr))))
}

func OpenStore(path string) (*Store, error) {
	cPath := C.CString(path)
	defer C.free(unsafe.Pointer(cPath))
	var cErr C.weaveffi_error
	result := C.weaveffi_kv_Store_open(cPath, &cErr)
	if cErr.code != 0 {
		return nil, wvMapKv(wvTakeError(&cErr))
	}
	return wvAdoptStore(result), nil
}

// Share: A second reference to this same store (the returned pointer equals
// the receiver's; both must eventually be destroyed).
func (s *Store) Share() *Store {
	if s.ptr == nil {
		panic("weaveffi: Store used after Close")
	}
	defer runtime.KeepAlive(s)
	var cErr C.weaveffi_error
	result := C.weaveffi_kv_Store_share(s.ptr, &cErr)
	wvTrap(&cErr)
	return wvAdoptStore(result)
}

func (s *Store) Close() {
	if s.ptr != nil {
		C.weaveffi_kv_Store_destroy(s.ptr)
		s.ptr = nil
		runtime.SetFinalizer(s, nil)
	}
}
```

- **Construction.** Every constructor is a package-level function; a
  throwing one returns `(*T, error)`, a non-throwing one returns `*T`
  (`NewEventBus()` in the `events` sample). Deprecated members carry a
  standard `// Deprecated:` comment that `go vet` and editors
  understand.
- **Disposal.** `Close()` releases this wrapper's reference through the
  `_destroy` symbol and clears the finalizer. It's idempotent. A
  finalizer set at adoption releases the reference if the wrapper is
  garbage collected first, but Go finalizers run on a non-deterministic
  schedule, so pair every wrapper with `defer store.Close()`. The
  producer object itself is dropped only when the last reference
  anywhere is released.
- **Use after close.** Every method checks `s.ptr == nil` first and
  panics with `"weaveffi: Store used after Close"`. Encoding a closed
  (or nil) wrapper into a non-optional position of a value buffer panics
  from `wvTokenStore`.
- **Copies mint new references.** Methods returning the receiver or an
  existing object (`Share()`, `Fork()`) return a fresh strong reference
  adopted into a new wrapper; closing one never affects another.
  `runtime.KeepAlive` keeps the receiver (and object parameters)
  reachable until the C call returns, so the finalizer can't fire
  mid-call.

```go
store, err := OpenStore("/tmp/cache.kv")
if err != nil {
	return err
}
defer store.Close()
ok, err := store.Put("alpha", []byte{1}, EntryKindPersistent, nil)
fmt.Println(store.Count(), StoreDefaultCapacity())
```

### Nullable objects, and objects inside values

An `Interface?` parameter is a nil-able pointer that crosses as NULL
when nil, and an `Interface?` return adopts a NULL pointer to nil
(`wvAdoptStore` handles both):

```go
func (s *Store) Larger(other *Store) *Store {
	if s.ptr == nil {
		panic("weaveffi: Store used after Close")
	}
	defer runtime.KeepAlive(s)
	defer runtime.KeepAlive(other)
	var cOther *C.weaveffi_kv_Store
	if other != nil {
		cOther = other.ptr
	}
	var cErr C.weaveffi_error
	result := C.weaveffi_kv_Store_larger(s.ptr, cOther, &cErr)
	wvTrap(&cErr)
	return wvAdoptStore(result)
}
```

Objects inside records, lists, maps, and optionals travel as `u64`
object tokens in the value buffer. Writing a token mints a new strong
reference with the `_clone` symbol (`wvTokenStore`); reading one adopts
the reference into a fresh wrapper (`wvUntokenStore`). From the
`StoreInfo` record (`Store *Store`, `Mirror *Store`):

```go
func wvPackStoreInfo(w *wvWriter, v StoreInfo) {
	w.writeString(v.Label)
	w.writeU64(wvTokenStore(v.Store))
	if v.Mirror == nil {
		w.writeOptionFlag(false)
	} else {
		w.writeOptionFlag(true)
		w.writeU64(wvTokenStore(v.Mirror))
	}
	w.writeI64(v.Count)
}

func wvUnpackStoreInfo(r *wvReader) StoreInfo {
	var v StoreInfo
	v.Label = r.readString()
	v.Store = wvUntokenStore(r.readU64())
	if r.readOptionFlag() {
		var oMirror0 *Store
		oMirror0 = wvUntokenStore(r.readU64())
		v.Mirror = oMirror0
	}
	v.Count = r.readI64()
	return v
}
```

Lists of objects work the same way in both directions
(`StoreOpenMany(paths)` returns `([]*Store, error)`,
`StoreTotalCount(stores, extra)` takes `[]*Store`); each wrapper in a
returned slice owns its own reference and should be closed individually.
Iterators over objects adopt one reference per step, and a blocking
async call returning an object adopts the pointer inside the completion
trampoline.

## Rich (algebraic) enums

A *rich* (algebraic) enum, a sum type whose variants carry associated
data, crosses the C ABI as a serialized value buffer, exactly like a
struct: an `i32` tag (the declared discriminant, or declaration order)
followed by the active variant's fields in order. The Go surface is a
sealed interface plus one struct per variant, with no `Close()`. (A
plain C-style enum with no payloads stays a typed `int32` alias with
`const` values; see above.) From the `codec` sample:

```go
// Shape is a sealed sum type: exactly one of its variant structs is the
// value at a time.
type Shape interface {
	isShape()
}

// ShapeEmpty: No payload.
type ShapeEmpty struct{}

func (ShapeEmpty) isShape() {}

// ShapeCircle: One `f64`.
type ShapeCircle struct {
	// Radius: Radius.
	Radius float64
}

func (ShapeCircle) isShape() {}

// ShapeLabeled: A string and an `i32`.
type ShapeLabeled struct {
	// Label: Label text.
	Label string
	// Count: Repeat count.
	Count int32
}

func (ShapeLabeled) isShape() {}
```

Construct a variant as a struct literal and discriminate with a type
switch:

```go
var c Shape = ShapeCircle{Radius: 2.0}
switch v := c.(type) {
case ShapeCircle:
	fmt.Println(v.Radius)
case ShapeLabeled:
	fmt.Println(v.Label, v.Count)
}
```

Free functions that take or return the enum pack it into a value buffer
on the way in and unpack the returned buffer on the way out, releasing
the producer's bytes with `weaveffi_free_bytes`. Variant fields of
interface type follow the object token rules above.

## Callback interfaces

A `callback_interfaces:` entry becomes a Go `interface` the consumer
implements and passes wherever the API takes that type. From the
`kvstore` sample:

```go
// Implement this interface in Go and pass the value to native functions
// that accept it.
//
// The native library may call any method from any thread until it releases
// the implementation. A panic in a method is reported to the native caller
// as a foreign error (code -4) instead of crashing the process.
type EvictionListener interface {
	// OnEvict: An entry left the store. Returns whether the listener wants to keep
	// receiving notifications; `false` detaches it.
	OnEvict(entry Entry, reason EvictionReason) bool
}
```

```go
type auditor struct{}

func (auditor) OnEvict(entry kvstore.Entry, reason kvstore.EvictionReason) bool {
	fmt.Println(entry.Key, reason)
	return true
}

store.SetEvictionListener(auditor{})
```

cgo forbids passing Go pointers to C, so the implementation never
crosses the boundary directly. Passing one wraps it in a `cgo.Handle`
whose integer value crosses as `ctx` (widened to `void*` in C by the
preamble's `wvHandlePtr`, which keeps `go vet` quiet), together with the
address of one static vtable per interface whose slots are `//export`ed
Go trampolines:

```go
//export goWv_weaveffi_kv_EvictionListener_on_evict
func goWv_weaveffi_kv_EvictionListener_on_evict(ctx unsafe.Pointer, entry_ptr *C.uint8_t, entry_len C.size_t, reason C.weaveffi_kv_EvictionReason, out_err *C.weaveffi_error) (ret C._Bool) {
	defer func() {
		if r := recover(); r != nil {
			wvForeignError(out_err, r)
		}
	}()
	impl := cgo.Handle(uintptr(ctx)).Value().(EvictionListener)
	rArg0 := &wvReader{buf: wvBorrowBuffer(entry_ptr, entry_len)}
	var arg0 Entry
	arg0 = wvUnpackEntry(rArg0)
	rArg0.expectEnd()
	arg1 := EvictionReason(reason)
	ret = boolToC(impl.OnEvict(arg0, arg1))
	return
}

//export goWv_weaveffi_kv_EvictionListener_free
func goWv_weaveffi_kv_EvictionListener_free(ctx unsafe.Pointer) {
	cgo.Handle(uintptr(ctx)).Delete()
}
```

```go
func (s *Store) SetEvictionListener(listener EvictionListener) {
	if s.ptr == nil {
		panic("weaveffi: Store used after Close")
	}
	defer runtime.KeepAlive(s)
	hListener := cgo.NewHandle(listener)
	var cErr C.weaveffi_error
	C.weaveffi_kv_Store_set_eviction_listener(s.ptr, C.wvHandlePtr(C.uintptr_t(hListener)), C.wvVtablePtr_weaveffi_kv_EvictionListener(), &cErr)
	wvTrap(&cErr)
}
```

- **Lifetime.** The `cgo.Handle` keeps the implementation alive exactly
  as long as the producer may call it; the vtable's `free` trampoline
  deletes the handle when the producer drops its last reference. A
  producer that retains the implementation (a store's eviction listener)
  keeps it alive across calls; one that doesn't (the `events` sample's
  `RouteOnce`) frees it before returning. Passing the same value twice
  creates two handles.
- **Argument ownership.** Borrowed strings and buffers are copied into
  Go memory before the method runs (`wvBorrowBuffer`, `C.GoString`), so
  the implementation may keep them. An object passed to a callback
  method is owned by the implementation: the trampoline adopts it into a
  new wrapper (`impl.OnAttached(wvAdoptEventBus(bus))` in the `events`
  sample), and the implementation should `Close()` it when done (or let
  the finalizer run).
- **Return values.** A method's return value is converted back to its C
  representation (`bool` via `boolToC`, a plain enum to its C enum, a
  record to a value buffer the producer frees).
- **Panics.** A panic escaping a method never unwinds through the C
  frame. The deferred `recover()` hands the value to `wvForeignError`,
  which writes code -4 with `fmt.Sprint(recovered)` into the producer's
  error slot, and the trampoline returns its zero value; the producer
  aborts the call in progress. The original caller then sees
  `*WeaveFFIError` with `Code == -4` as the returned `error` (throwing
  callable) or as the `wvTrap` panic value (non-throwing callable, the
  trap idiom). The process is never taken down.
- **Threads.** The producer may call a method from any thread; cgo
  callbacks run on whatever OS thread the producer fires them from, with
  the Go runtime attaching a goroutine to it. The `kvstore` eviction
  listener fires synchronously inside `Delete()`/`Get()`; the `events`
  sample's `PublishLater` calls subscribers from the producer's worker
  thread. Don't block in a callback waiting on the goroutine that made
  the producer call if the producer invoked it synchronously from that
  call.

## Build instructions

1. Generate the bindings:

   ```bash
   weaveffi generate api.yaml -o generated --target go
   ```

2. Build the Rust shared library:

   ```bash
   cargo build --release -p your_library
   ```

3. Point cgo at the header and library. The Go package `#include`s the
   header emitted by the C target, so generate that too:

   ```bash
   weaveffi generate api.yaml -o generated --target c
   export CGO_CFLAGS="-I$PWD/generated/c"
   export CGO_LDFLAGS="-L$PWD/target/release -lweaveffi"
   ```

4. Build and run a Go consumer:

   ```bash
   cd generated/go
   go build ./...
   ```

cgo requires a C compiler (`gcc` or `clang`) on the host; on Windows use
a MinGW-w64 toolchain or the MSVC build provided by `go env`. Go 1.23 or
later is required for the `iter` package.

## Packaging

`weaveffi package --target go` emits the Go module under `go/` with a
self-contained, relocatable cgo preamble and copies each supplied
desktop binary to `go/lib/<platform-id>/` (`macos-arm64`, `macos-x64`,
`linux-x64`, `linux-arm64`, `windows-x64`). The single
`#cgo LDFLAGS: -l<name>` line is expanded into a `${SRCDIR}`-relative
include path for the packaged C header plus one `-L` (and, except on
Windows, `-Wl,-rpath`) directive per `GOOS,GOARCH` pair, so
`go build` selects the matching slice with no environment setup:

```go
#cgo CFLAGS: -I${SRCDIR}/../c/include
#cgo darwin,arm64 LDFLAGS: -L${SRCDIR}/lib/macos-arm64 -Wl,-rpath,${SRCDIR}/lib/macos-arm64
#cgo linux,amd64 LDFLAGS: -L${SRCDIR}/lib/linux-x64 -Wl,-rpath,${SRCDIR}/lib/linux-x64
#cgo windows,amd64 LDFLAGS: -L${SRCDIR}/lib/windows-x64
#cgo LDFLAGS: -lkvstore
```

Android and `wasm32` binaries are skipped. See
[Packaging](../guides/packaging.md) for the shared workflow.

## Memory and ownership

- **Strings in:** `C.CString` allocates a copy in C memory; the
  generated wrapper pairs every `CString` with a `defer C.free(...)`.
- **Strings out:** `C.GoString` copies the C string into Go-owned
  memory, then the wrapper calls `weaveffi_free_string` to release the
  Rust allocation.
- **Bytes:** input slices are passed by pointer for the duration of the
  call (no copy); returned bytes are copied with `C.GoBytes` and then
  `weaveffi_free_bytes` is called.
- **Buffered values (structs, rich enums, optionals, lists, maps):**
  parameters are packed into a Go-owned `[]byte` that the producer
  borrows for the duration of the call; returns are copied into Go
  memory with `wvCopyBuffer`, released with `weaveffi_free_bytes`, and
  decoded. Object tokens written into a buffer are fresh strong
  references the producer owns; tokens read out are adopted into
  wrappers.
- **Interfaces:** one strong reference per wrapper, released by
  `Close()` with a `runtime.SetFinalizer` backstop. Always
  `defer s.Close()`.
- **Callback implementations:** held by a `cgo.Handle` until the
  producer calls the vtable's `free`.

## Async support

Functions marked `async: true` are exposed through `_async`-suffixed C
launchers that take a completion callback plus `void* context`. Go has
no ambient async runtime, so the generated wrapper turns that into a
blocking call built on a channel: it makes a buffered channel, wraps it
in a `cgo.Handle`, launches the C call with an exported trampoline and
the handle as context, then receives from the channel. The generated
doc comment states that the call blocks. From the `kvstore` sample:

```go
// Blocks the calling goroutine until the async producer completes.
func (s *Store) Compact() (int64, error) {
	if s.ptr == nil {
		panic("weaveffi: Store used after Close")
	}
	defer runtime.KeepAlive(s)
	ch := make(chan wvOutcomeKvStoreCompact, 1)
	h := cgo.NewHandle(ch)
	C.weaveffi_kv_Store_compact_async(s.ptr, nil, C.weaveffi_kv_Store_compact_callback(unsafe.Pointer(C.goWv_weaveffi_kv_Store_compact_callback)), C.wvHandlePtr(C.uintptr_t(h)))
	outcome := <-ch
	if outcome.err != nil {
		return 0, outcome.err
	}
	return outcome.val, nil
}
```

The completion callback fires exactly once, on a producer thread. The
trampoline takes the channel out of the handle (one-shot), converts the
C error or result inside the callback, and sends a single `wvOutcome…`
value:

```go
//export goWv_weaveffi_kv_Store_compact_callback
func goWv_weaveffi_kv_Store_compact_callback(context unsafe.Pointer, err *C.weaveffi_error, result C.int64_t) {
	h := cgo.Handle(uintptr(context))
	ch := h.Value().(chan wvOutcomeKvStoreCompact)
	h.Delete()
	if err != nil && err.code != 0 {
		ch <- wvOutcomeKvStoreCompact{err: wvMapKv(wvTakeBoxedError(err))}
		return
	}
	ch <- wvOutcomeKvStoreCompact{val: int64(result)}
}
```

Result buffers such as strings and buffered values are owned by the
consumer, so the trampoline copies or decodes them into Go memory and
then releases them with the runtime free symbols; a heap-boxed error is
read and released by `wvTakeBoxedError` (`weaveffi_error_free`); an
owned interface result is adopted into a wrapper instead.

For a callable marked `throws: true`, the trampoline maps the error
through the domain mapper, so the returned `error` is the typed one
(`*KvError` from `store.Compact()`); a non-throwing async callable
returns a plain value and panics via `wvTrap` on a trap code. A panic
inside the spawned future surfaces as code -2. The native producer
already runs on its own thread, so the wrapper simply blocks the calling
goroutine; callers that want concurrency run the call from a goroutine
of their own.

For functions marked `cancellable: true` the C launcher gains a
`weaveffi_cancel_token*` parameter. The Go wrapper passes `nil` for it
and doesn't expose the token; only the C and C++ targets surface
cancellation tokens.

## Iterators

`iter<T>` returns map to the standard library's range-over-function
sequences (Go 1.23+): a non-throwing function returns `iter.Seq[T]` and
a throwing one returns `iter.Seq2[T, error]`. Nothing is drained: the
producer iterator is launched when the consumer starts ranging, and each
consumer step issues exactly one producer `next` call. From the
`kvstore` sample's throwing `ListKeys`:

```go
// A launch or per-element error is yielded as the final (zero value,
// error) pair, and iteration stops.
func (s *Store) ListKeys(prefix *string) iter.Seq2[string, error] {
	return func(yield func(string, error) bool) {
		if s.ptr == nil {
			panic("weaveffi: Store used after Close")
		}
		defer runtime.KeepAlive(s)
		// ... pack the *string prefix into a value buffer ...
		var cErr C.weaveffi_error
		it := C.weaveffi_kv_Store_list_keys(s.ptr, cPrefixPtr, C.size_t(len(wPrefix.buf)), &cErr)
		if cErr.code != 0 {
			yield("", wvMapKv(wvTakeError(&cErr)))
			return
		}
		defer C.weaveffi_kv_Store_ListKeysIterator_destroy(it)
		for {
			var outItem *C.char
			var iterErr C.weaveffi_error
			ok := C.weaveffi_kv_Store_ListKeysIterator_next(it, &outItem, &iterErr) != 0
			if iterErr.code != 0 {
				yield("", wvMapKv(wvTakeError(&iterErr)))
				return
			}
			if !ok {
				return
			}
			item := C.GoString(outItem)
			C.weaveffi_free_string(outItem)
			if !yield(item, nil) {
				return
			}
		}
	}
}
```

Consume it with `for key, err := range store.ListKeys(nil)`, checking
`err` on each step. Each yielded element is copied into Go memory and
its Rust allocation released per element (strings via
`weaveffi_free_string`; buffered elements are decoded and released with
`weaveffi_free_bytes`; object elements are adopted into wrappers). The
deferred `_destroy` call runs exactly once, whether the consumer
exhausts the sequence or breaks out of the `for range` loop early.
Ranging over the same returned sequence again launches a fresh producer
iterator.

In a non-throwing sequence (the `events` sample's `Messages()`), a
reported error can only be a producer bug or a panicking callback, so
`wvTrap` panics instead of yielding it.

## Known limitations

- Async functions block the calling goroutine; there's no channel- or
  context-based variant, and `cancellable: true` tokens are not exposed.
- Callback methods run on the producer's thread as cgo callbacks; there
  is no marshalling to a particular goroutine.
- Non-throwing wrappers panic on the runtime trap codes (including a
  panicking callback, code -4) rather than returning an `error`.
- The generated module needs Go 1.23+ (`iter`, `range` over functions)
  and a C toolchain for cgo.
- The plain `generate` output expects `weaveffi.h` from the C target and
  the library on `CGO_CFLAGS`/`CGO_LDFLAGS`; only `weaveffi package`
  produces a relocatable module.

## Troubleshooting

- **`undefined reference to weaveffi_*`**: `CGO_LDFLAGS` is missing the
  `-l` flag or `-L` directory. Recheck the environment exports.
- **`could not determine kind of name` in cgo**: ensure `CGO_CFLAGS`
  points at the directory containing `weaveffi.h` (generate the C
  target alongside Go).
- **`panic: WeaveFFI ABI mismatch`** at startup: the library was built
  by a different `weaveffi` release than the bindings. Regenerate the
  bindings and rebuild the library together.
- **`panic: weaveffi: Store used after Close`**: a closed wrapper was
  used. Keep the wrapper alive for as long as the object is in use;
  closing one wrapper doesn't affect others pointing at the same object.
- **`weaveffi: ... (code -4)`** returned or panicked: a
  callback-interface method you implemented panicked; the message is
  the panic value. Recover inside the method if the producer call should
  succeed anyway.
- **`go: cannot find module providing package weaveffi`**: change the
  generator config so `go.mod` declares the module path you actually
  import, e.g. `github.com/myorg/mylib`.
