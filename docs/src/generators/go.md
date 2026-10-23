# Go

The Go target emits a self-contained cgo module that binds the library's C
ABI (revision 5). Records and rich enums are plain Go values, interfaces are
reference-counted wrappers with `Close`, calls that declare errors return
`error` values you match with `errors.As`, async functions block on a
`context.Context`, and `iter<T>` returns standard-library `iter.Seq`
sequences. The module requires Go 1.24 or later.

Go is a [Tier 2](../stability.md#target-tiers) target: it may lag behind a
new ABI revision for a while, but it runs the same conformance suite as
every other target before a release.

## What's generated

For a library whose identity is `kvstore`:

```text
go/
  go.mod        module path (the package name, or `name`), go 1.24
  README.md
  kvstore.h     a copy of the C header
  bindings.go   the API: types, wrappers, trampolines, load-time checks
  runtime.go    Check, errors, strings, typed arrays, objects, callbacks,
                iterators, async
  codec.go      the value-buffer writer and reader
```

Every Go file carries the standard `// Code generated ... DO NOT EDIT.`
line before its package clause, so linters skip it and editors warn before
you change it.

Every name comes from the package identity:

| Name | Default | Override |
|------|---------|----------|
| Module path | the package `name` | `[generators.go] name` |
| Package name | the C prefix without underscores (`kitchen_sink` is `kitchensink`) | `[generators.go] package` |
| Linked library | `-l{library}` | none |
| Header | `{library}.h` | none |

```toml
[generators.go]
name = "github.com/example/kvstore"
package = "kvstore"
```

A package name that's a Go keyword gains a trailing underscore.

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
| error domain `KitchenErrors` | `KitchenError` (one `Error` suffix, never doubled) |

A free function whose bare name would clash with a type, an enum
constant, a constructor, a static, an error type, the runtime's `Check`,
`Error`, or `DebugLive`, or another free function's bare name is prefixed
with its module path: the kvstore sample's async `kv.open_store` sits beside
the `OpenStore` constructor, so it's `KvOpenStore` (and a function in module
`kv.stats` would get a `KvStats` prefix). Methods never clash this way,
since each lives on its own type.

Doc comments are the IDL's docs. A package-level declaration's comment
starts with its name, as Go convention asks, so a doc that doesn't is
prefixed with `Name: `. Backticked API names become Go doc links
(`` `new_op` `` is `[NewOp]`, a method `[Store.Get]`) or their Go spelling
(a field `ExpiresAt`, a parameter `ttlSeconds`), and a deprecation becomes
the final `Deprecated:` paragraph that Go tools recognize.

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

`Check() error` checks the library's ABI revision and then, for every
top-level module, the library's
[contract table](../reference/abi.md#load-time-checks) against the rows
the bindings were generated with, including one row per error code and per
callback method:

```go
if err := kvstore.Check(); err != nil {
	log.Fatal(err) // kvstore: kv.Store.put changed since these bindings were generated
}
```

The checks run once, on the first call to `Check` or to any function of the
package, and `Check` returns the same result from then on. Importing the
package never panics. A function called after a failed check panics with
the error `Check` returns, so call `Check` at startup when you'd rather
handle a mismatched library as an error. The error names the first
declaration that fails: `kvstore: kv.Store.put is missing from the library`
or `kvstore: kv.Store.put changed since these bindings were generated`.
Rows the library has and the bindings don't are fine, so a library that
only adds functions, error codes, or callback methods keeps working with
older bindings.

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
| scalar `T?` (integer, float, `bool`, C-style enum) | `*T` | a presence flag and the value |
| other `T?` | `*T`; a slice, map, rich enum, `[]byte`, or wrapper stays as it is and nil is none | value buffer |
| numeric `[T]` (`i8` to `i64`, `u16` to `u64`, `f32`, `f64`) | `[]T` | a typed array (pointer and element count) |
| other `[T]`, `{K: V}` | `[]T`, `map[K]V` | value buffer |
| interface | `*Iface` | object pointer |
| `Iface?` | `*Iface` (nil is none) | object pointer or null |
| callback interface | a Go `interface` you implement | handle plus a static vtable |
| `Cb?` | the same interface (nil is none) | handle plus a vtable, or null |
| `iter<T>` | `iter.Seq[T]` or `iter.Seq2[T, error]` | iterator handle |

Strings pass a view of Go memory with no copy and no NUL terminator, so
they may contain `\x00`. A Go string can hold bytes that aren't UTF-8; the
library rejects them as a marshalling failure (`-3`).

A numeric slice parameter passes the slice's own backing array for the
call, with no copy and no encoding; cgo keeps it in place until the call
returns, and the library only reads it. A returned array is copied into a
new slice and the library's run is freed. An empty result is an empty,
non-nil slice. Inside a record, list, or map, numeric lists and optional
scalars use the value-buffer encoding like everything else; the Go types
are the same either way.

Each type that crosses inside a value buffer has one codec pair, shared by
every parameter, result, field, and callback argument of that type. A
composite is named after its shared stem (the same name every target uses):

```go
func wvWrite_map_string_Store(w *wvWriter, v map[string]*Store) {
	wvWriteMap(w, v, (*wvWriter).writeString, wvWriteStore)
}
```

## Objects and lifetime

Each wrapper embeds the runtime's object core, which owns one strong
reference. `Close() error` releases it (so a wrapper satisfies
`io.Closer`), and a `runtime.AddCleanup` cleanup releases it some time after
an unclosed wrapper becomes unreachable. Every call borrows the native
pointer for its duration:

```go
func (s *Store) Get(key string) (Entry, error) {
	cSelf := s.native()
	defer s.release()
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

A function's `throws` decides its Go signature:

| IDL | Go signature | A failure is |
|-----|--------------|--------------|
| `throws: KvError` | `(T, error)` | a code of the domain, or an `*Error` for a runtime code |
| `throws: any` | `(T, error)` | an `*Error` with code `-1` and the library's message |
| no `throws` | `T` | a bug: the wrapper panics with an `*Error` |

A domain code comes back as its code's type, a pointer to a struct holding
the code's payload fields and a `Message`, and every code type of a domain
implements the domain's sealed interface:

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

Domains are open: a positive code the bindings don't declare (from a newer
library) comes back as the domain's `*Unknown{Domain}` type
(`*UnknownKvError`), which keeps the code and message and still matches the
domain's interface. Codes are unique only within a domain, so the
calculator's `CalcError.DivisionByZero` and `ParseError.NotANumber` can both
be 1; each function maps codes through its own domain.

`Error()` returns the library's message, or the code's default message when
`Message` is empty. A runtime code (`-1` generic, `-2` a panic in the
library, `-3` a marshalling failure, `-4` a callback failure) comes back as
the package's `*Error`, which keeps the code and message and never matches a
domain interface. A function that declares no errors panics with that
`*Error` instead (see
[the trap policy](../guides/errors-and-memory.md#the-trap-policy)). The
panic is an ordinary recoverable one, and its text names the code and the
library's message, as in `kvstore: <message> (code -3)`.

## Async and cancellation

Async functions take a `context.Context` first and always return an error;
they never panic on a failure. A function with no `throws` reports its
failure as an `*Error` like a `throws: any` one. They block the calling
goroutine on a channel that a completion trampoline fills from the
library's thread; call them from a goroutine of your own for concurrency. A
`cancellable` function creates a native cancel token, cancels it when the
context is done, and returns `ctx.Err()` (`context.Canceled` or
`context.DeadlineExceeded`) for the cancelled completion (code `-5`), or
`context.Canceled` if the library cancelled the call on its own:

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
done never launches the call. An optional scalar result is a `*T` and a
numeric list result a `[]T`, as in a sync call.

## Callbacks

A callback interface is a Go interface. Passing an implementation stores it
in a handle table and hands the library the address of one static vtable
per interface: the header's `size` and `flags` (0: Go may run a method on
any thread), the `free` entry that deletes the handle once the library is
done, and one `//export` trampoline per method. The library may call
methods from any thread, and `free` may run on any thread too.

```go
type Policy interface {
	// The TTL a write of `key` gets, given the TTL the caller
	// requested (absent for none): ...
	//
	// Return a KvError to report one of its codes, with its fields, to the native
	// caller; any other error reports code -1 with its text.
	TTLFor(key string, requested *int64) (*int64, error)
	Admit(entry Entry) (Entry, error)
	Route(key string, home *Store) (*Store, error)
}
```

- **Arguments.** Strings, bytes, typed arrays, and buffered values are
  copied or decoded for the call, and an optional scalar is a `*T`. An
  object argument (`home` above) is a new wrapper that owns its own
  reference: keep it, return it, or `Close` it.
- **Returns.** A method may return any type but an iterator or a callback
  interface. A direct value returns by value, an optional scalar as its
  flag and value; a string, `[]byte`, typed array, or buffered value is
  copied into a run allocated with the library's allocator; an object
  returns a fresh reference to the wrapper's object (the wrapper stays
  yours). Returning nil for a required object, or a value the library can't
  read (text that isn't UTF-8, an undeclared enum value), fails the call in
  progress.
- **Errors.** A method that declares `throws` returns `(T, error)` or
  `error`. Returning one of its domain's code types (for example
  `&kvstore.RejectedError{Key: k, Reason: "no secrets"}`) reports that code
  and its payload fields, so the original caller can receive exactly that
  error. Any other error reports code `-1` with its text. A method that
  panics, throwing or not, is recovered and reported as `-4` with the
  panic's text; nothing unwinds into the library. What the caller finally
  sees is the producer's choice: a Rust producer renders a typed error's
  message from its fields, and the kvstore sample turns every other
  callback failure into `KvError.CallbackFailed`.
- **Nil implementations.** A `Cb?` parameter accepts nil, which passes no
  callback; so does a typed nil (an interface holding a nil pointer, map,
  slice, func, or channel), which would otherwise reach the library as an
  implementation whose every call panics. A required one panics on either.

## Iterators

An `iter<T>` return becomes a lazy sequence: each `range` launches the
native iterator, pulls one element per step, and destroys the iterator when
the loop ends, including on an early `break`. A function that declares
errors returns an `iter.Seq2[T, error]` that yields its error as a final
`(zero, err)` pair:

```go
for key, err := range store.Keys(nil) {
	if err != nil {
		return err
	}
	fmt.Println(key)
}
```

Elements may be optional scalars (`iter.Seq[*int64]`) or numeric lists
(`iter.Seq[[]int32]`), which cross as a flag and a value or as a typed
array.

## Leak checks

`DebugLive(kind)` reports the library's live objects (0), callbacks (1),
iterators (2), cancel tokens (3), and byte runs (4), and `DebugLive(-1)` is
1 when the library counts at all. Every count is 0 unless the library was
built with the `leak-check` feature. Wrappers you don't close are released
by cleanups, so run `runtime.GC()` (and give the cleanups a moment) before
reading the counts.

## Limitations

- All modules share one Go package, which the global naming rules make
  safe; only a free function whose Go name collides gains its module path
  (see [What's generated](#whats-generated)).
- The library is bound at link time, so there's no run-time override of
  which library file loads beyond the platform loader's search path, and
  `Check` reports a mismatched library only after the program has linked
  and started.
- A non-cancellable async call abandoned by its context keeps running in
  the library until it finishes.
- Vtables are never thread-affine, so a producer may call a Go callback
  from any thread; Go's scheduler makes that safe.
