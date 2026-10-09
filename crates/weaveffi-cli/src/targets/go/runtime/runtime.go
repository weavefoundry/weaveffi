package {{PACKAGE}}

/*
#include "{{HEADER}}"

static void* wvRuntimeHandlePtr(uintptr_t h) { return (void*)h; }
*/
import "C"

import (
	"cmp"
	"context"
	"errors"
	"fmt"
	"iter"
	"reflect"
	"runtime"
	"runtime/cgo"
	"slices"
	"sync"
	"sync/atomic"
	"unsafe"
)

// wvABIVersion is the C ABI revision these bindings were generated against.
const wvABIVersion uint32 = {{ABI_VERSION}}

// The runtime error codes this package produces or interprets itself.
const (
	wvCodeGeneric   int32 = -1
	wvCodeMarshal   int32 = -3
	wvCodeForeign   int32 = -4
	wvCodeCancelled int32 = -5
)

// Error is a failure no error domain claims: a generic failure (code -1),
// including every failure of a function that fails with any error; a panic
// in the native library (-2); an argument or result that couldn't be
// marshalled (-3); or a callback implementation that failed (-4). A
// function that declares no errors panics with an *Error, since its failure
// is a bug; any other returns one.
type Error struct {
	// Code is the runtime code.
	Code int32
	// Message is the native library's message.
	Message string
}

func (e *Error) Error() string {
	return fmt.Sprintf("{{PACKAGE}}: %s (code %d)", e.Message, e.Code)
}

// DebugLive reports how many resources of one kind the native library holds:
// 0 objects, 1 callbacks, 2 iterators, 3 cancel tokens, 4 byte runs. Kind -1
// reports 1 when the library counts at all. Every count is 0 unless the
// library was built with leak checks enabled; tests use it to assert that
// nothing leaked.
func DebugLive(kind int32) uint64 {
	return uint64(C.{{PREFIX}}_debug_live(C.int32_t(kind)))
}

// ── Load-time checks ──

// wvLoad runs the load-time checks once, on first use.
var wvLoad = sync.OnceValue(wvCheckLibrary)

// Check reports whether the linked native library is the one these bindings
// were generated for: it must implement the same C ABI revision, and its
// contract tables must hold every declaration the bindings use, unchanged.
// Declarations only the library has are fine.
//
// The checks run once, on the first call to Check or to any function of the
// package, and Check returns their result from then on. Call it at startup
// to handle a mismatched library as an error: a function called after a
// failed check panics with the error Check returns.
func Check() error {
	return wvLoad()
}

// wvLoaded runs the load-time checks if they haven't run yet, and panics
// with their error when they failed.
func wvLoaded() {
	if err := wvLoad(); err != nil {
		panic(err)
	}
}

// wvCheckABI fails when the linked library implements a different C ABI
// revision than these bindings were generated for.
func wvCheckABI() error {
	if found := uint32(C.{{PREFIX}}_abi_version()); found != wvABIVersion {
		return fmt.Errorf("{{PACKAGE}}: the linked library implements C ABI revision %d, but these bindings were generated for revision %d", found, wvABIVersion)
	}
	return nil
}

// wvContractEntry is one declaration these bindings were generated with:
// the FNV-1a hashes of its dotted path and of its signature.
type wvContractEntry struct {
	id, hash uint64
	path     string
}

// wvCheckContract fails unless every entry in want is in the library's
// contract table for one top-level module (n entries sorted by id) with an
// equal signature hash.
func wvCheckContract(table *C.{{PREFIX}}_contract_entry, n C.size_t, want []wvContractEntry) error {
	have := unsafe.Slice(table, int(n))
	for _, w := range want {
		i, found := slices.BinarySearchFunc(have, w.id, func(e C.{{PREFIX}}_contract_entry, id uint64) int {
			return cmp.Compare(uint64(e.id), id)
		})
		if !found {
			return fmt.Errorf("{{PACKAGE}}: %s is missing from the library", w.path)
		}
		if uint64(have[i].hash) != w.hash {
			return fmt.Errorf("{{PACKAGE}}: %s changed since these bindings were generated", w.path)
		}
	}
	return nil
}

// ── Errors ──

// wvFailure is a non-zero error slot copied into Go memory.
type wvFailure struct {
	code    int32
	message string
	payload []byte
}

// err converts the failure into the generic *Error.
func (f wvFailure) err() error {
	return &Error{Code: f.code, Message: f.message}
}

func wvCopyFailure(cErr *C.{{PREFIX}}_error) wvFailure {
	f := wvFailure{
		code:    int32(cErr.code),
		message: wvBorrowString(cErr.message_ptr, cErr.message_len),
	}
	if cErr.payload_ptr != nil {
		f.payload = wvBorrowBytes(cErr.payload_ptr, cErr.payload_len)
	}
	return f
}

// wvTakeError copies a non-zero error slot and clears it.
func wvTakeError(cErr *C.{{PREFIX}}_error) wvFailure {
	f := wvCopyFailure(cErr)
	C.{{PREFIX}}_error_clear(cErr)
	return f
}

// wvTakeBoxedError copies the heap-boxed error an async completion receives
// and frees the box.
func wvTakeBoxedError(cErr *C.{{PREFIX}}_error) wvFailure {
	f := wvCopyFailure(cErr)
	C.{{PREFIX}}_error_free(cErr)
	return f
}

// wvTrap panics with an *Error when the slot reports a failure. Wrappers of
// calls that declare no errors check their slot with it: a failure there is
// a bug in the native library, an argument it couldn't take, or a callback
// failure it let through.
func wvTrap(cErr *C.{{PREFIX}}_error) {
	if cErr.code != 0 {
		panic(wvTakeError(cErr).err())
	}
}

// wvCodeMessage is the message of a failure in the error domain named
// domain that the library reported without one.
func wvCodeMessage(domain string, code int32) string {
	return fmt.Sprintf("%s code %d", domain, code)
}

// wvDomainError is implemented by every error-code type: its code and its
// fields encoded as a value buffer (nil when it has none).
type wvDomainError interface {
	error
	Code() int32
	wvPayload() []byte
}

// wvSetError reports a failure through a callback's error slot. The native
// library copies the message and payload, so both are only borrowed.
func wvSetError(outErr *C.{{PREFIX}}_error, code int32, message string, payload []byte) {
	ptr, n := wvStr(message)
	C.{{PREFIX}}_error_set(outErr, C.int32_t(code), ptr, n)
	if len(payload) > 0 {
		ptr, n := wvBytes(payload)
		C.{{PREFIX}}_error_set_payload(outErr, ptr, n)
	}
}

// wvCallbackError reports an error a callback method returned as a generic
// failure (code -1) with the error's text.
func wvCallbackError(outErr *C.{{PREFIX}}_error, err error) {
	wvSetError(outErr, wvCodeGeneric, err.Error(), nil)
}

// wvCallbackFailed reports the error a callback method of the domain D
// returned: an error of D reports its code, message, and payload, and
// anything else is a generic failure (see wvCallbackError).
func wvCallbackFailed[D error](outErr *C.{{PREFIX}}_error, err error) {
	var domain D
	if errors.As(err, &domain) {
		if d, ok := any(domain).(wvDomainError); ok {
			wvSetError(outErr, d.Code(), d.Error(), d.wvPayload())
			return
		}
	}
	wvCallbackError(outErr, err)
}

// wvRecoverCallback, deferred by every callback trampoline, reports a panic
// in the implementation as a callback failure (code -4) instead of letting
// it unwind into the native caller.
func wvRecoverCallback(outErr *C.{{PREFIX}}_error) {
	if r := recover(); r != nil {
		msg := fmt.Sprint(r)
		if err, ok := r.(error); ok {
			msg = err.Error()
		}
		wvSetError(outErr, wvCodeForeign, msg, nil)
	}
}

// ── Callback contexts ──

// wvIsNil reports whether a callback implementation is nil: a nil interface,
// or one holding a nil pointer, map, slice, func, or channel (a typed nil,
// which would otherwise reach the native library as an implementation whose
// every call fails).
func wvIsNil(impl any) bool {
	if impl == nil {
		return true
	}
	switch v := reflect.ValueOf(impl); v.Kind() {
	case reflect.Pointer, reflect.Map, reflect.Slice, reflect.Func, reflect.Chan, reflect.Interface:
		return v.IsNil()
	}
	return false
}

// wvNewCallback keeps impl alive in a handle table for as long as the
// native library holds it and returns the handle as the callback's context
// pointer; the vtable's free entry deletes it (see wvFreeCallback). A nil
// implementation (see wvIsNil) of the required callback interface iface
// panics.
func wvNewCallback(impl any, iface string) unsafe.Pointer {
	if wvIsNil(impl) {
		panic("{{PACKAGE}}: nil " + iface + " implementation")
	}
	return C.wvRuntimeHandlePtr(C.uintptr_t(cgo.NewHandle(impl)))
}

// wvOptionalCallback is wvNewCallback for an optional callback parameter:
// a nil implementation (including a typed nil) passes no callback, a null
// context and vtable.
func wvOptionalCallback[V any](impl any, vtable *V) (unsafe.Pointer, *V) {
	if wvIsNil(impl) {
		return nil, nil
	}
	return C.wvRuntimeHandlePtr(C.uintptr_t(cgo.NewHandle(impl))), vtable
}

// wvCallback recovers the implementation behind a callback context.
func wvCallback[T any](ctx unsafe.Pointer) T {
	return cgo.Handle(uintptr(ctx)).Value().(T)
}

// wvFreeCallback releases the implementation behind a callback context once
// the native library is done with it.
func wvFreeCallback(ctx unsafe.Pointer) {
	cgo.Handle(uintptr(ctx)).Delete()
}

// ── Strings and bytes ──

// wvStr returns a borrowed (pointer, length) view of s's UTF-8 bytes for the
// duration of one call. Strings may contain NUL bytes.
func wvStr(s string) (*C.uint8_t, C.size_t) {
	return (*C.uint8_t)(unsafe.Pointer(unsafe.StringData(s))), C.size_t(len(s))
}

// wvBytes returns a borrowed (pointer, length) view of b for the duration of
// one call.
func wvBytes(b []byte) (*C.uint8_t, C.size_t) {
	return (*C.uint8_t)(unsafe.Pointer(unsafe.SliceData(b))), C.size_t(len(b))
}

// wvBorrowString copies a borrowed UTF-8 run into a Go string. A run of
// length 0 is never read.
func wvBorrowString(ptr *C.uint8_t, n C.size_t) string {
	if n == 0 {
		return ""
	}
	return string(unsafe.Slice((*byte)(unsafe.Pointer(ptr)), n))
}

// wvTakeString copies a returned UTF-8 run into a Go string and releases the
// native allocation.
func wvTakeString(ptr *C.uint8_t, n C.size_t) string {
	s := wvBorrowString(ptr, n)
	C.{{PREFIX}}_free_bytes(ptr, n)
	return s
}

// wvBorrowBytes copies a borrowed byte run into Go memory. A run of length
// 0 copies to an empty slice.
func wvBorrowBytes(ptr *C.uint8_t, n C.size_t) []byte {
	return wvBorrowSlice[byte](ptr, n)
}

// wvTakeBytes copies a returned byte run into Go memory and releases the
// native allocation.
func wvTakeBytes(ptr *C.uint8_t, n C.size_t) []byte {
	b := wvBorrowBytes(ptr, n)
	C.{{PREFIX}}_free_bytes(ptr, n)
	return b
}

// wvHandOverString copies s into a run allocated with the native library's
// allocator and writes it to a callback's return slots; the library adopts
// the run.
func wvHandOverString(s string, outPtr **C.uint8_t, outLen *C.size_t) {
	wvHandOverSlice(unsafe.Slice(unsafe.StringData(s), len(s)), outPtr, outLen)
}

// wvHandOverBytes is wvHandOverString for a byte slice (raw bytes or an
// encoded value buffer).
func wvHandOverBytes(b []byte, outPtr **C.uint8_t, outLen *C.size_t) {
	wvHandOverSlice(b, outPtr, outLen)
}

// ── Optional scalars and typed arrays ──

// wvPresent splits an optional scalar into the presence flag and value a
// call passes: (false, zero) for nil.
func wvPresent[T any](v *T) (bool, T) {
	if v == nil {
		var zero T
		return false, zero
	}
	return true, *v
}

// wvOptional joins a presence flag and value the library passed into an
// optional scalar: nil when absent.
func wvOptional[T any](present bool, v T) *T {
	if !present {
		return nil
	}
	return &v
}

// wvSliceIn returns a borrowed (pointer, count) view of s as an array of
// the C element type E, which has T's size and layout, for the duration of
// one call. The elements hold no Go pointers, and cgo keeps the memory in
// place until the call returns.
func wvSliceIn[E, T any](s []T) (*E, C.size_t) {
	return (*E)(unsafe.Pointer(unsafe.SliceData(s))), C.size_t(len(s))
}

// wvBorrowSlice copies a borrowed array of n elements into a new slice. An
// array of length 0 is never read.
func wvBorrowSlice[T, E any](ptr *E, n C.size_t) []T {
	out := make([]T, int(n))
	if n > 0 {
		copy(out, unsafe.Slice((*T)(unsafe.Pointer(ptr)), int(n)))
	}
	return out
}

// wvTakeSlice copies a returned array of n elements into a new slice and
// releases the native allocation (n times the element size).
func wvTakeSlice[T, E any](ptr *E, n C.size_t) []T {
	out := wvBorrowSlice[T](ptr, n)
	var elem E
	C.{{PREFIX}}_free_bytes((*C.uint8_t)(unsafe.Pointer(ptr)), n*C.size_t(unsafe.Sizeof(elem)))
	return out
}

// wvHandOverSlice copies v into a run allocated with the native library's
// allocator and writes it and its element count to a callback's return
// slots; the library adopts the run.
func wvHandOverSlice[T, E any](v []T, outPtr **E, outLen *C.size_t) {
	var elem E
	size := C.size_t(len(v)) * C.size_t(unsafe.Sizeof(elem))
	run := C.{{PREFIX}}_alloc(size)
	if size > 0 {
		if run == nil {
			panic("{{PACKAGE}}: the native library couldn't allocate a callback result")
		}
		copy(unsafe.Slice((*T)(unsafe.Pointer(run)), len(v)), v)
	}
	*outPtr, *outLen = (*E)(unsafe.Pointer(run)), C.size_t(len(v))
}

// ── Objects ──

// wvRef owns one strong reference to a native object. Calls borrow the
// pointer between acquire and release. Close may race them: the reference
// is destroyed exactly once, by close when no call is in flight, or else by
// the last in-flight call to finish.
type wvRef struct {
	ptr     unsafe.Pointer
	destroy func(unsafe.Pointer)
	// state counts in-flight calls in its upper bits; the low bit is set
	// once close has run.
	state atomic.Uint64
}

// acquire borrows the pointer for one call, panicking when the wrapper was
// already closed.
func (r *wvRef) acquire(typeName string) unsafe.Pointer {
	for {
		s := r.state.Load()
		if s&1 != 0 {
			panic("{{PACKAGE}}: " + typeName + " used after Close")
		}
		if r.state.CompareAndSwap(s, s+2) {
			return r.ptr
		}
	}
}

// release ends one call's borrow.
func (r *wvRef) release() {
	if r.state.Add(^uint64(1)) == 1 {
		r.destroy(r.ptr)
	}
}

// close releases the reference once no call is in flight. Calling it again
// is a no-op.
func (r *wvRef) close() {
	for {
		s := r.state.Load()
		if s&1 != 0 {
			return
		}
		if r.state.CompareAndSwap(s, s|1) {
			if s == 0 {
				r.destroy(r.ptr)
			}
			return
		}
	}
}

// wvObject is embedded in every object wrapper: it owns the wrapper's strong
// reference, and a cleanup releases the reference when the wrapper becomes
// unreachable without being closed.
type wvObject struct {
	ref     *wvRef
	cleanup runtime.Cleanup
}

// adopt takes over one owned strong reference to ptr, released by destroy.
func (o *wvObject) adopt(ptr unsafe.Pointer, destroy func(unsafe.Pointer)) {
	o.ref = &wvRef{ptr: ptr, destroy: destroy}
	o.cleanup = runtime.AddCleanup(o, (*wvRef).close, o.ref)
}

// acquire borrows the object's pointer for one call; pair it with a deferred
// release.
func (o *wvObject) acquire(typeName string) unsafe.Pointer {
	if o.ref == nil {
		panic("{{PACKAGE}}: " + typeName + " used before it was created by this package")
	}
	return o.ref.acquire(typeName)
}

// release ends one call's borrow.
func (o *wvObject) release() {
	o.ref.release()
}

// Close releases the wrapper's strong reference and always returns nil. It's
// idempotent and safe to call from any goroutine, even while a call on the
// wrapper is in flight: the reference is then released when that call
// returns. The object itself is dropped once its last reference, from any
// wrapper, record, or the native library, is gone. A wrapper that's never
// closed is released some time after it becomes unreachable.
func (o *wvObject) Close() error {
	if o.ref != nil {
		o.cleanup.Stop()
		o.ref.close()
	}
	return nil
}

// ── Iterators ──

// wvCursor is one launched native iterator: next pulls the next element,
// reporting false at the end, and destroy releases the iterator.
type wvCursor[T any] struct {
	next    func(cErr *C.{{PREFIX}}_error) (T, bool)
	destroy func()
}

// wvSeq is the sequence over a native iterator of a function that declares
// no errors: each range launches the iterator, pulls one element per step,
// and destroys it when the range ends, early or not. A failure panics with
// an *Error.
func wvSeq[T any](launch func(cErr *C.{{PREFIX}}_error) wvCursor[T]) iter.Seq[T] {
	return func(yield func(T) bool) {
		var cErr C.{{PREFIX}}_error
		c := launch(&cErr)
		wvTrap(&cErr)
		defer c.destroy()
		for {
			v, more := c.next(&cErr)
			wvTrap(&cErr)
			if !more || !yield(v) {
				return
			}
		}
	}
}

// wvSeq2 is wvSeq for a function that declares errors: a failure, mapped by
// mapErr, is yielded as a final (zero value, error) pair.
func wvSeq2[T any](launch func(cErr *C.{{PREFIX}}_error) wvCursor[T], mapErr func(wvFailure) error) iter.Seq2[T, error] {
	return func(yield func(T, error) bool) {
		var zero T
		var cErr C.{{PREFIX}}_error
		c := launch(&cErr)
		if cErr.code != 0 {
			yield(zero, mapErr(wvTakeError(&cErr)))
			return
		}
		defer c.destroy()
		for {
			v, more := c.next(&cErr)
			if cErr.code != 0 {
				yield(zero, mapErr(wvTakeError(&cErr)))
				return
			}
			if !more || !yield(v, nil) {
				return
			}
		}
	}
}

// ── Async ──

// wvOutcome is one async completion: a converted result, or the failure the
// native library reported.
type wvOutcome[T any] struct {
	val  T
	fail *wvFailure
}

// wvAsyncCall is one pending async call: the channel its completion is
// delivered on, kept alive in a handle table while the call is in flight.
type wvAsyncCall[T any] struct {
	done   chan wvOutcome[T]
	handle cgo.Handle
}

func wvNewAsyncCall[T any]() *wvAsyncCall[T] {
	c := &wvAsyncCall[T]{done: make(chan wvOutcome[T], 1)}
	c.handle = cgo.NewHandle(c.done)
	return c
}

// context is the completion's context pointer.
func (c *wvAsyncCall[T]) context() unsafe.Pointer {
	return C.wvRuntimeHandlePtr(C.uintptr_t(c.handle))
}

// wvComplete delivers one async completion to the waiting wrapper. It runs on
// a native thread inside an exported trampoline, so it never panics: a result
// that fails to decode is delivered as a marshalling failure.
func wvComplete[T any](context unsafe.Pointer, cErr *C.{{PREFIX}}_error, convert func() T) {
	h := cgo.Handle(uintptr(context))
	done := h.Value().(chan wvOutcome[T])
	h.Delete()
	done <- wvSettle(cErr, convert)
}

func wvSettle[T any](cErr *C.{{PREFIX}}_error, convert func() T) (o wvOutcome[T]) {
	if cErr != nil {
		if f := wvTakeBoxedError(cErr); f.code != 0 {
			o.fail = &f
			return o
		}
	}
	defer func() {
		if r := recover(); r != nil {
			o = wvOutcome[T]{fail: &wvFailure{code: wvCodeMarshal, message: fmt.Sprint(r)}}
		}
	}()
	o.val = convert()
	return o
}

// wvAwait blocks until call completes or ctx is done, and never panics. With
// a cancel token, ctx's cancellation cancels the native call and waits for
// its completion, which the native library delivers promptly; without one,
// the call is abandoned and ctx.Err() returned at once. A cancelled
// completion (code -5) returns ctx.Err(), or context.Canceled when ctx
// itself isn't done; any other failure is mapped by mapErr.
func wvAwait[T any](ctx context.Context, call *wvAsyncCall[T], token *C.{{PREFIX}}_cancel_token, mapErr func(wvFailure) error) (T, error) {
	var o wvOutcome[T]
	select {
	case o = <-call.done:
	case <-ctx.Done():
		if token == nil {
			var zero T
			return zero, ctx.Err()
		}
		C.{{PREFIX}}_cancel_token_cancel(token)
		o = <-call.done
	}
	if o.fail == nil {
		return o.val, nil
	}
	var zero T
	if o.fail.code == wvCodeCancelled {
		if err := ctx.Err(); err != nil {
			return zero, err
		}
		return zero, context.Canceled
	}
	return zero, mapErr(*o.fail)
}
