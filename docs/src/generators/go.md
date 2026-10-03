# Go

The Go target emits a self-contained cgo module that binds the library's C
ABI. Records and rich enums are plain Go values, interfaces are
reference-counted wrappers with `Close`, async functions take a
`context.Context`, and `iter<T>` returns standard-library `iter.Seq`
sequences, so the module requires Go 1.23 or later.

## What's generated

For a library whose identity is `kvstore`:

```text
go/
  go.mod        module path (the package name, or `module_path`)
  README.md
  kvstore.h     a copy of the C header
  bindings.go   the API: types, wrappers, trampolines, load-time checks
  runtime.go    errors, string and object helpers, the async bridge
  codec.go      the value-buffer writer and reader (only when needed)
```

Every name comes from the package identity:

| Name | Default | Override |
|------|---------|----------|
| Module path | the package `name` | `[generators.go] module_path` |
| Package name | the C prefix | none |
| Linked library | `-l{library}` | none |
| Header | `{library}.h` | none |

```toml
[generators.go]
module_path = "github.com/example/kvstore"
strip_module_prefix = true # the default
```

With `strip_module_prefix`, module `kv`'s `delete` surfaces as `Delete`.
All modules render into one Go package, so a function whose stripped name
would clash with a type, a constant, or another module's function keeps its
module prefix (`directory.card` next to a `Card` record becomes
`DirectoryCard`).

## Build and load

The bindings link with `#cgo LDFLAGS: -l{library}` and include the bundled
header, so a consumer only supplies the library's directory:

```sh
export CGO_LDFLAGS="-L/path/to/lib"
export LD_LIBRARY_PATH="/path/to/lib"   # DYLD_LIBRARY_PATH on macOS
go build ./...
```

Depend on the module with `require` (plus `replace` for a local checkout)
and import it by its module path. The library is bound when the program is
linked and loaded by the platform's dynamic loader, so the
`{PREFIX}_LIBRARY` override other targets honor doesn't apply; use the
loader's search path or an rpath instead.

At package initialization, `init` checks the library's ABI revision and the
contract checksum of every top-level module, and panics with the stale
module's name on a mismatch:

```go
func init() {
	wvCheckABI()
	wvCheckModule("shared", 0x42c4ce2c8d0af052, uint64(C.kitchen_sink_shared_checksum()))
	wvCheckModule("kitchen", 0xd83da75b24b545ff, uint64(C.kitchen_sink_kitchen_checksum()))
}
```

`weaveffi package` bundles a prebuilt library per platform under
`lib/<platform>/` and adds `${SRCDIR}`-relative search and rpath lines per
GOOS and GOARCH, so no `CGO_LDFLAGS` is needed.

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
| `T?` | `*T` (nil-able types such as slices stay as they are) | value buffer |
| `[T]`, `{K: V}` | `[]T`, `map[K]V` | value buffer |
| interface | `*Iface` | object pointer |
| `Iface?` | `*Iface` (nil is none) | object pointer or NULL |
| callback interface | a Go `interface` you implement | `cgo.Handle` plus a static vtable |
| `iter<T>` | `iter.Seq[T]` or `iter.Seq2[T, error]` | iterator handle |

Strings pass a view of Go memory with no copy and no NUL terminator, so
they may contain `\x00`. Lengths are never narrowed to `C.int`.

## Objects and lifetime

Each wrapper owns one strong reference. `Close() error` releases it (so a
wrapper satisfies `io.Closer`), and a finalizer releases it if you forget:

```go
func (s *Gadget) Close() error {
	runtime.SetFinalizer(s, nil)
	s.ref.close()
	return nil
}
```

Every call borrows the native pointer for its duration:

```go
func (s *Gadget) Poke(times int32) (int32, error) {
	cSelf := s.native()
	defer s.ref.release()
	var cErr C.kitchen_sink_error
	cRet := C.kitchen_sink_kitchen_Gadget_poke(cSelf, C.int32_t(times), &cErr)
	if cErr.code != 0 {
		return 0, wvMapKitchen(wvTakeError(&cErr))
	}
	return int32(cRet), nil
}
```

`Close` is idempotent and goroutine-safe. When it races a call in flight
(even one that closes the wrapper from inside a callback), the reference is
released as soon as that call returns, never mid-call. Using a wrapper after
`Close` panics. An object inside a record or list crosses as a token that
carries a fresh reference, so the wrapper you passed stays valid.

## Errors

A function that declares `throws: true` returns `(T, error)`. Domain codes
map to the module's error type, which `errors.As` selects:

```go
_, err := store.Get("missing")
var kerr *kvstore.KvError
if errors.As(err, &kerr) && kerr.Code == kvstore.KvErrorKeyNotFound {
	// ...
}
```

Codes with payload fields attach a `{Type}{Code}Payload` struct as
`Payload`. Anything a domain doesn't claim is the package's generic
`*Error`, which keeps the code: `-1` generic, `-2` producer panic, `-3`
marshalling failure, and `-4` a callback implementation that panicked. A
function without `throws` has a plain signature and panics with `*Error`
instead, since only a bug can make it fail.

## Async and cancellation

Async functions take a `context.Context` first and always return an error.
They block the calling goroutine on a channel that a completion trampoline
fills from the producer's thread. A `cancellable` function creates a
native cancel token, cancels it when the context is done, and returns
`ctx.Err()` for the cancelled completion (code `-5`):

```go
ctx, cancel := context.WithTimeout(context.Background(), time.Second)
defer cancel()
n, err := store.Compact(ctx)
if errors.Is(err, context.DeadlineExceeded) {
	// The native call was cancelled.
}
```

Any other async function returns `ctx.Err()` as soon as the context is done
and discards the native result when it arrives. An already-cancelled
context never launches the call.

## Callbacks

A callback interface is a Go interface. Passing an implementation stores it
in a `cgo.Handle` and hands the producer one static vtable of `//export`
trampolines; the producer's `free` entry deletes the handle. The producer
may call methods from any thread. A panic inside a method is recovered and
reported to the producer as code `-4` with the panic's text, whether the
producer's method returns a plain value or a `Result`, and the caller sees
it as `*Error`.

```go
type ReadyListener interface {
	// OnReady: Fires when an item is ready
	OnReady(code int32, msg string)
	// OnItem: Receives the item itself and says whether to keep listening
	OnItem(item Item, gadget *Gadget) bool
}
```

## Iterators

An `iter<T>` return becomes a lazy sequence: each `range` launches the
native iterator, pulls one element per step, and destroys the iterator when
the loop ends, including on an early `break`. A throwing function yields
its error as a final `(zero, err)` pair of an `iter.Seq2[T, error]`.

```go
for g := range kitchen_sink.StreamGadgets() {
	fmt.Println(g.Describe())
	g.Close()
}
```

## Leak checks

`DebugLive(kind)` reports the producer's live objects (0), callbacks (1),
iterators (2), cancel tokens (3), and returned allocations (4). It's always
0 unless the library was built with the `leak-check` feature.

## Limitations

- All modules share one Go package, so two modules can't declare types
  with the same name.
- The library is bound at link time, so there's no runtime library path
  override.
- Callback methods can only return direct values (scalars, `bool`, and
  C-style enums).
- A non-cancellable async call abandoned by its context keeps running in
  the producer until it finishes.
