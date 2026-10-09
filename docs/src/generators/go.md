# Go

The Go target emits a self-contained cgo module that binds the library's C
ABI. Records and rich enums are plain Go values, interfaces are
reference-counted wrappers with `Close`, throwing calls return `error`
values you match with `errors.As`, async functions block on a
`context.Context`, and `iter<T>` returns standard-library `iter.Seq`
sequences, so the module requires Go 1.23 or later.

## What's generated

For a library whose identity is `kvstore`:

```text
go/
  go.mod        module path (the package name, or `name`)
  README.md
  kvstore.h     a copy of the C header
  bindings.go   the API: types, wrappers, trampolines, load-time checks
  runtime.go    load-time checks, errors, strings, objects, callbacks, async
  codec.go      the value-buffer writer and reader
```

Every name comes from the package identity:

| Name | Default | Override |
|------|---------|----------|
| Module path | the package `name` | `[generators.go] name` |
| Package name | the C prefix | none |
| Linked library | `-l{library}` | none |
| Header | `{library}.h` | none |

```toml
[generators.go]
name = "github.com/example/kvstore"
```

All modules render into one Go package, and Go spells every exported name
in PascalCase with Go's initialisms (`user_id` is `UserID`, `ttl_seconds`
is `TTLSeconds`):

| Declaration | Go name |
|-------------|---------|
| function `kv.stats.summarize` | `Summarize` |
| interface `Store` | `*Store` |
| constructor `Store.open` | `OpenStore` (and `new` is `NewStore`) |
| method `Store.get_or_load` | `(*Store).GetOrLoad` |
| static `Store.default_capacity` | `StoreDefaultCapacity` |
| C-style enum `EntryKind`, variant `Volatile` | `EntryKind`, `EntryKindVolatile` |
| rich enum `Change`, variant `Put` | `Change`, `ChangePut` |
| error domain `KvError`, code `KeyNotFound` | `KvError`, `*KeyNotFoundError` |

A free function whose bare name would clash with a type, an enum
constant, a constructor, a static, an error type, the runtime's `Error`
or `DebugLive`, or another free function's bare name is prefixed with its
module path: the kvstore sample's async `kv.open_store` sits beside the
`OpenStore` constructor, so it's `KvOpenStore` (and a function in module
`kv.stats` would get a `KvStats` prefix). Methods never clash this way,
since each lives on its own type.

## Build and load

The bindings include the bundled header and link with
`#cgo LDFLAGS: -l{library}`, so a consumer supplies only the library's
directory:

```sh
export CGO_LDFLAGS="-L/path/to/lib"
export LD_LIBRARY_PATH="/path/to/lib"   # DYLD_LIBRARY_PATH on macOS
go build ./...
```

cgo links the library into the program when it's built, and the
platform's dynamic loader finds it when the program starts. The bindings
never open the library themselves, so the `{PREFIX}_LIBRARY` override the
other targets read at load time doesn't exist for Go; to choose the library
at run time, set the loader's search path (`LD_LIBRARY_PATH`,
`DYLD_LIBRARY_PATH`, or `PATH` on Windows). To record the directory in the
binary instead, add an rpath when building:

```sh
export CGO_LDFLAGS="-L/path/to/lib -Wl,-rpath,/path/to/lib"
```

`weaveffi package` writes the module to `go/{package}/` with each desktop
platform's library under `lib/<platform>/` and replaces the single link
line with one per GOOS and GOARCH, so cgo picks the right library with no
`CGO_LDFLAGS`:

```go
/*
#cgo darwin,arm64 LDFLAGS: -L${SRCDIR}/lib/darwin-arm64 -Wl,-rpath,${SRCDIR}/lib/darwin-arm64
#cgo linux,amd64 LDFLAGS: -L${SRCDIR}/lib/linux-x64 -Wl,-rpath,${SRCDIR}/lib/linux-x64
#cgo windows,amd64 LDFLAGS: -L${SRCDIR}/lib/windows-x64
#cgo LDFLAGS: -lkvstore
*/
```

`${SRCDIR}` expands to the module's directory (in the module cache, for a
dependency), so a program built on a machine runs there without a loader
path. A binary you ship elsewhere still needs the library beside it or on
the loader's path: copy it and link with an rpath such as
`-Wl,-rpath,$ORIGIN` (`@executable_path` on macOS). Windows has no rpath;
put the DLL next to the executable or on `PATH`. Android, iOS, and
`wasm32` libraries aren't bundled. Publish the module by pushing the
directory to the module path's repository; see
[Packaging](../guides/packaging.md).

## Load-time checks

Importing the package runs `init`, which checks the library's ABI revision
and then, for every top-level module, the library's
[contract table](../reference/abi.md#load-time-checks) against the entries
the bindings were generated with:

```go
func init() {
	wvCheckABI()
	var n C.size_t
	table := C.kvstore_kv_contract(&n)
	wvCheckContract(table, n, []wvContractEntry{
		{0x0969575bfbb012d7, 0xebd38766e3532c4f, "kv.Store.fork"},
		// ...one entry per declaration
	})
	table = C.kvstore_report_contract(&n)
	wvCheckContract(table, n, []wvContractEntry{
		{0xa21a2e7274bf28c6, 0x342e0b83b12d73ec, "report.render_report"},
		{0xd504fae45f64ab45, 0xd430b8de9d574a4c, "report.ReportError"},
	})
}
```

A mismatch panics with an error naming the declaration:
`kvstore: kv.Store.put is missing from the library` or
`kvstore: kv.Store.put changed since these bindings were generated`.
Declarations the library has and the bindings don't are fine, so a library
that only adds functions keeps working with older bindings.

## Type mapping

| IDL type | Go type | Crosses the ABI as |
|----------|---------|--------------------|
| `i8` to `i64`, `u8` to `u64` | `int8` to `int64`, `uint8` to `uint64` | the C integer |
| `f32`, `f64` | `float32`, `float64` | `float`, `double` |
| `bool` | `bool` | C `bool` |
| `string` | `string` | borrowed `(ptr, len)` UTF-8; returns are copied and freed |
| `bytes` | `[]byte` | borrowed `(ptr, len)`; returns are copied and freed |
| C-style enum | `type E int32` plus `E{Variant}` constants | `int32_t` |
| rich enum | sealed `interface` plus one struct per variant | value buffer |
| record | plain struct with exported fields | value buffer |
| `T?` | `*T`; a slice, map, rich enum, `[]byte`, or wrapper stays as it is and nil is none | value buffer |
| `[T]`, `{K: V}` | `[]T`, `map[K]V` | value buffer |
| interface | `*Iface` | object pointer |
| `Iface?` | `*Iface` (nil is none) | object pointer or null |
| callback interface | a Go `interface` you implement | handle plus a static vtable |
| `Cb?` | the same interface (nil is none) | handle plus a vtable, or a null vtable |
| `iter<T>` | `iter.Seq[T]` or `iter.Seq2[T, error]` | iterator handle |

Strings pass a view of Go memory with no copy and no NUL terminator, so
they may contain `\x00`. A Go string can hold bytes that aren't UTF-8; the
library rejects them as a marshalling failure (`-3`).

Each type that crosses inside a value buffer has one codec pair,
`wvWrite{T}` and `wvRead{T}`, built from the pairs of its parts and shared
by every parameter, result, field, and callback argument of that type:

```go
func wvWriteMapStringStore(w *wvWriter, v map[string]*Store) {
	wvWriteMap(w, v, (*wvWriter).writeString, wvWriteStore)
}
```

## Objects and lifetime

Each wrapper owns one strong reference. `Close() error` releases it (so a
wrapper satisfies `io.Closer`), and a finalizer releases it if you forget.
Every call borrows the native pointer for its duration:

```go
func (s *Store) Get(key string) (Entry, error) {
	cSelf := s.native()
	defer s.ref.release()
	cKeyPtr, cKeyLen := wvStr(key)
	var cRetLen C.size_t
	var cErr C.kvstore_error
	cRet := C.kvstore_kv_Store_get(cSelf, cKeyPtr, cKeyLen, &cRetLen, &cErr)
	if cErr.code != 0 {
		return Entry{}, wvKvError(wvTakeError(&cErr))
	}
	return wvDecode(cRet, cRetLen, wvReadEntry), nil
}
```

`Close` is idempotent and goroutine-safe. When it races a call in flight
(even one that closes the wrapper from inside a callback), the reference is
released as soon as that call returns, never mid-call. Using a wrapper after
`Close`, or passing a nil `*Iface` where an object is required, panics. An
object inside a record, list, or map crosses as a token that carries a
fresh reference, so the wrapper you passed stays valid. Two wrappers may
refer to the same object (a method that returns its receiver hands back a
new wrapper); each is closed on its own.

Because every wrapper has `Close`, an IDL method named `close` is
`Close_` ([reserved member names](../reference/naming.md#identifiers-in-generated-code)).

## Errors

A function declared `throws` returns `(T, error)`. A positive code comes
back as its code's type, a pointer to a struct holding the code's payload
fields and a `Message`, and every code type of a domain implements the
domain's sealed interface:

```go
_, err := store.Get("missing")
var missing *kvstore.KeyNotFoundError
if errors.As(err, &missing) {
	fmt.Println(missing.Key, missing.Code()) // missing 1001
}
var kvErr kvstore.KvError
if errors.As(err, &kvErr) {
	// any code of the kv domain
}
```

`Error()` returns the library's message, or the code's default message when
`Message` is empty. A runtime code (`-1` generic, `-2` a panic in the
library, `-3` a marshalling failure, `-4` a callback failure) or a code
outside the domain comes back as the package's `*Error`, which keeps the
code and message and never matches a domain interface.

A function that isn't `throws` can only fail because of a bug, so its
wrapper has a plain signature and panics with an `*Error` instead (see
[the trap policy](../guides/errors-and-memory.md#the-trap-policy)). The
panic is an ordinary recoverable one, and its text names the code and the
library's message, as in `kvstore: <message> (code -3)`.

## Async and cancellation

Async functions take a `context.Context` first and always return an error.
They block the calling goroutine on a channel that a completion trampoline
fills from the library's thread; call them from a goroutine of your own for
concurrency. A `cancellable` function creates a native cancel token,
cancels it when the context is done, and returns `ctx.Err()`
(`context.Canceled` or `context.DeadlineExceeded`) for the cancelled
completion (code `-5`), or `context.Canceled` if the library cancelled the
call on its own:

```go
ctx, cancel := context.WithTimeout(context.Background(), time.Second)
defer cancel()
removed, err := store.Compact(ctx, 60000)
if errors.Is(err, context.DeadlineExceeded) {
	// The native call was cancelled.
}
```

Any other async function returns `ctx.Err()` as soon as the context is done
and discards the native result when it arrives. A context that's already
done never launches the call. A throwing async function returns its domain
errors like a sync one; any other panics on failure.

## Callbacks

A callback interface is a Go interface. Passing an implementation stores it
in a handle table and hands the library the address of one static vtable
per interface: the header's `size` and `flags`, the `free` entry that
deletes the handle once the library is done, and one `//export` trampoline
per method. The library may call methods from any thread, and `free` may run
on any thread too.

```go
type Policy interface {
	// Admit an entry about to be stored, returning it as it should be
	// stored. ...
	//
	// Return a KvError to report that code, with its fields, to the native
	// caller.
	Admit(entry Entry) (Entry, error)
	// Route returns the store `key` belongs in: ...
	Route(key string, home *Store) *Store
}
```

- **Arguments.** Strings, bytes, and buffered values are copied or decoded
  for the call. An object argument (`home` above) is a new wrapper that
  owns its own reference: keep it, return it, or `Close` it (the finalizer
  releases it otherwise).
- **Returns.** A method may return any type but an iterator or a callback
  interface. A direct value returns by value; a string, `[]byte`, or
  buffered value is copied into a run allocated with the library's
  allocator; an object returns a fresh reference to the wrapper's object
  (the wrapper stays yours). Returning nil for a required object, or a value
  the library can't read (text that isn't UTF-8, an undeclared enum value),
  fails the call in progress with `-3`.
- **Errors.** A method declared `throws` returns `(T, error)` or `error`.
  Returning one of the domain's code types (for example
  `&kvstore.RejectedError{Key: k, Reason: "no secrets", Message: "secrets
  are not stored"}`) reports that code, its message, and its payload
  fields, so the original caller receives exactly that error. Any other
  error reports `-4` with its text. A method that panics, throwing or not,
  is recovered and reported as `-4` with the panic's text; nothing unwinds
  into the library.
- **Optional callbacks.** A `Cb?` parameter accepts nil, which passes a null
  vtable. A required one panics on nil.

## Iterators

An `iter<T>` return becomes a lazy sequence: each `range` launches the
native iterator, pulls one element per step, and destroys the iterator when
the loop ends, including on an early `break`. A throwing function returns
an `iter.Seq2[T, error]` that yields its error as a final `(zero, err)`
pair:

```go
for key, err := range store.Keys(nil) {
	if err != nil {
		return err
	}
	fmt.Println(key)
}
```

## Leak checks

`DebugLive(kind)` reports the library's live objects (0), callbacks (1),
iterators (2), cancel tokens (3), and byte runs (4), and `DebugLive(-1)` is
1 when the library counts at all. Every count is 0 unless the library was
built with the `leak-check` feature. Wrappers you don't close are released
by finalizers, so run `runtime.GC()` before reading the counts.

## Limitations

- All modules share one Go package, which the global naming rules make
  safe; only a free function whose Go name collides gains its module path
  (see [What's generated](#whats-generated)).
- The library is bound at link time, so there's no run-time override of
  which library file loads beyond the platform loader's search path.
- A non-cancellable async call abandoned by its context keeps running in
  the library until it finishes.
