//! The **marshalling plan**: the language-neutral passing contracts every
//! backend renders, computed once and stored on the model.
//!
//! The [`crate::model`] layer answers *which symbols exist and what their C
//! signatures are*. This module holds the answers one level up, which the
//! model precomputes for every binding so no backend re-derives them:
//!
//! * **Passing** ([`ArgPass`] on every parameter): which C slots one
//!   argument occupies and how it fills them (by value, a presence flag plus
//!   a value, a typed array, a pinned string or byte run, a serialized value
//!   buffer, a borrowed object pointer, or a callback context plus vtable).
//! * **Receiving** ([`RetPass`] for a call's return, [`ResultPass`] for an
//!   async completion's result, [`ItemPass`] for an iterator element,
//!   [`CallbackRetPass`] for a callback method's return): which slots carry
//!   the value back, what the receiver does with it (use, copy, decode, or
//!   adopt), and the release it owes, with every obligation resolved (the
//!   object's `_destroy` symbol, the slice's element type).
//! * **Errors** ([`ErrorStrategy`] on every callable): when a call reports
//!   through `out_err`, is that a typed domain error the caller can catch,
//!   an untyped error, or a producer bug the wrapper must trap on?
//!
//! Each variant names its own slots ([`AbiParam`]s, owned), so a backend
//! that dispatches on these enums never looks a slot up by position. The
//! slots are the same values that appear, in order, in the binding's
//! [`AbiFn`](crate::model::AbiFn).
//!
//! The protocols below (iterators, async, callback interfaces) are the
//! contract clauses every backend must satisfy; the C ABI reference
//! (`docs/src/reference/abi.md`) is normative.
//!
//! # Iterators
//!
//! The producer returns an opaque iterator handle
//! ([`RetPass::Iterator`]); the consumer calls
//! [`next`](crate::model::IteratorBinding::next) once per element and
//! [`destroy_symbol`](crate::model::IteratorBinding::destroy_symbol) exactly
//! once when done. Wrappers expose the target's native **lazy** iteration
//! idiom and issue one producer `next` per consumer step (draining the
//! producer into a hidden list is a contract violation). Each element is
//! received as its [`ItemPass`] says, and each `next` follows the owning
//! callable's [`ErrorStrategy`].
//!
//! # Async
//!
//! The launcher returns immediately; the producer invokes the completion
//! callback exactly once, from an arbitrary producer thread. Everything
//! passed to the callback is owned by the consumer: results are received as
//! their [`ResultPass`] says, and a non-null `err` is heap-boxed and
//! released with `{prefix}_error_free`. Wrappers hop back to their native
//! scheduler before touching consumer state.
//!
//! # Callback interfaces
//!
//! The wrapper emits one static vtable per interface (the fixed `size`,
//! `flags`, and `free` header, then one trampoline per method from the
//! method's [`AbiFn`](crate::model::AbiFn)) and passes a handle-table key as
//! `ctx`. Inside a trampoline, arguments are received as their [`ArgPass`]
//! names them: strings, bytes, slices, and buffers are borrowed for the call
//! (copy or decode them, free nothing), and object arguments transfer one
//! strong reference the wrapper adopts. The method's return crosses back as
//! its [`CallbackRetPass`] says. A failure is reported through `out_err`
//! with `{prefix}_error_set` (never by unwinding through the C frame); see
//! [`ErrorStrategy`] for which codes a method may report.

use crate::abi::AbiParam;
use crate::model::IteratorBinding;
use crate::ty::{Prim, Ty};

/// How a callable's `out_err` slot (or an async completion's `err`) is
/// interpreted by idiomatic wrappers.
///
/// Every synchronous C ABI entry point carries a trailing `out_err`, and every
/// async completion callback carries an `err` slot, whatever the strategy.
/// What differs is the *meaning* of a non-zero code, and every backend must
/// agree on it. A negative code (generic `-1`, panic `-2`, marshalling
/// failure `-3`, foreign callback failure `-4`, cancelled `-5`) is a runtime
/// code under every strategy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorStrategy {
    /// The callable doesn't throw: the only way `out_err` reports failure is
    /// a producer bug or a runtime trap (a caught panic, `-2`; a consumer
    /// callback that raised, `-4`). The wrapper surfaces it through the
    /// target's *programming-error* idiom (an unchecked exception where the
    /// language has them, a Go `panic`, a Swift `fatalError`), never as a
    /// typed error, and never ignores it. A callback method with this
    /// strategy reports any failure as `-4`.
    Trap,
    /// The callable throws the named error domain (`throws: KvError`), a
    /// name [`Model::error_domain`](crate::model::Model::error_domain)
    /// resolves. A positive code is one of the domain's codes: the wrapper
    /// maps it to the code's typed error, decoding any payload fields from
    /// the error's payload buffer, and maps a positive code it doesn't know
    /// to the domain's base error type, keeping the code and message
    /// (domains are open). Runtime codes surface through the same channel
    /// as the target's runtime error type (`-5` as the language's
    /// cancellation error where it has one). A callback method with this
    /// strategy may report the domain's positive codes with their payloads.
    Domain(String),
    /// The callable throws an untyped error (`throws: any`): a failure is
    /// the generic runtime code `-1` with a message, surfaced through the
    /// target's normal error channel as its runtime error type; there are
    /// no positive codes. A callback method with this strategy reports a
    /// failure as `-1` with its message.
    Untyped,
}

impl ErrorStrategy {
    /// `true` unless the strategy is [`Trap`](Self::Trap): the idiomatic
    /// signature declares that the callable can fail.
    #[must_use]
    pub fn throws(&self) -> bool {
        !matches!(self, ErrorStrategy::Trap)
    }

    /// The error domain's name for [`Domain`](Self::Domain), else `None`.
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        match self {
            ErrorStrategy::Domain(name) => Some(name),
            ErrorStrategy::Trap | ErrorStrategy::Untyped => None,
        }
    }
}

/// How one parameter crosses the call boundary: the C slots it occupies, in
/// order, and how the wrapper fills them.
///
/// As a callable's parameter, the argument is borrowed by the producer for
/// the call. As a callback method's parameter, the same slots carry the
/// producer's argument to the consumer's trampoline: borrowed for the call,
/// except an object, whose strong reference the consumer adopts. A callback
/// method's parameter is never [`Callback`](Self::Callback).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgPass {
    /// One slot passed by value: scalars, bools, and C-style enums
    /// (`int32_t`).
    Direct {
        /// The value slot, `T {name}`.
        slot: AbiParam,
    },
    /// An optional scalar, bool, or C-style enum: `bool has_{name}` then
    /// `T {name}`. When `has` is false, `value` is ignored (pass `0`).
    OptDirect {
        /// The presence slot, `bool has_{name}`.
        has: AbiParam,
        /// The value slot, `T {name}`.
        value: AbiParam,
        /// The inner type: a [`Ty::Prim`] scalar or a [`Ty::Enum`].
        inner: Ty,
    },
    /// A list of a numeric primitive passed as a typed array:
    /// `const T* {name}_ptr` then `size_t {name}_len` (the element count).
    /// The array is borrowed, must be aligned for `T`, and may be null when
    /// the count is `0`.
    Slice {
        /// The `const T*` slot.
        ptr: AbiParam,
        /// The `size_t` element-count slot.
        len: AbiParam,
        /// The element primitive.
        elem: Prim,
    },
    /// A `(ptr, len)` pair of UTF-8 bytes, not NUL-terminated:
    /// `const uint8_t* {name}_ptr`, `size_t {name}_len`. The wrapper encodes
    /// the string and keeps the encoding alive for the call.
    String {
        /// The `const uint8_t*` slot.
        ptr: AbiParam,
        /// The `size_t` byte-length slot.
        len: AbiParam,
    },
    /// A `(ptr, len)` byte pair: `const uint8_t* {name}_ptr`,
    /// `size_t {name}_len`. The wrapper pins its native byte storage for
    /// the call.
    Bytes {
        /// The `const uint8_t*` slot.
        ptr: AbiParam,
        /// The `size_t` byte-length slot.
        len: AbiParam,
    },
    /// A buffered value (record, rich enum, or an optional, list, or map of
    /// any other family): the wrapper serializes it into the value-buffer
    /// format ([`Ty::wire`]) and passes the encoding as
    /// `const uint8_t* {name}_ptr`, `size_t {name}_len`. Any object token in
    /// the encoding is a freshly cloned reference.
    Buffer {
        /// The `const uint8_t*` slot.
        ptr: AbiParam,
        /// The `size_t` byte-length slot.
        len: AbiParam,
    },
    /// An object pointer: `const {tag}* {name}` as a callable's parameter
    /// (borrowed; the wrapper keeps its own reference), or `{tag}* {name}`
    /// as a callback method's parameter (one strong reference the consumer
    /// adopts).
    Object {
        /// The pointer slot.
        slot: AbiParam,
        /// `true` for `Interface?`: null is a legal "none" argument.
        nullable: bool,
        /// The interface's bare name.
        interface: String,
    },
    /// A callback interface: `void* {name}_ctx` then
    /// `const {vtable}* {name}_vtable`. The wrapper registers its native
    /// implementation in a handle table, passes the key as `ctx` and the
    /// interface's static vtable as `vtable`, and removes the entry when the
    /// producer calls the vtable's `free`.
    Callback {
        /// The `void*` context slot.
        ctx: AbiParam,
        /// The `const {vtable}*` slot.
        vtable: AbiParam,
        /// `true` for `Cb?`: a null vtable is a legal "none" argument.
        nullable: bool,
        /// The callback interface's bare name.
        interface: String,
    },
}

impl ArgPass {
    /// The slots this argument occupies, in signature order.
    #[must_use]
    pub fn slots(&self) -> Vec<&AbiParam> {
        match self {
            ArgPass::Direct { slot } | ArgPass::Object { slot, .. } => vec![slot],
            ArgPass::OptDirect { has, value, .. } => vec![has, value],
            ArgPass::Slice { ptr, len, .. }
            | ArgPass::String { ptr, len }
            | ArgPass::Bytes { ptr, len }
            | ArgPass::Buffer { ptr, len } => vec![ptr, len],
            ArgPass::Callback { ctx, vtable, .. } => vec![ctx, vtable],
        }
    }
}

/// How a synchronous call's return crosses back to the wrapper: the C
/// return, any trailing out slots (which precede `out_err`), what the
/// wrapper does with the value, and the release it owes.
///
/// On failure every C return is a zero sentinel the wrapper must not use.
// A model holds one of these per callable and never moves them in bulk, so
// boxing the iterator variant would only cost consumers a deref.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetPass {
    /// No return value: the C return is `void`. Also an async launcher's
    /// return, whose result arrives through
    /// [`AsyncBinding::result`](crate::model::AsyncBinding::result).
    Void,
    /// By-value return (scalar, bool, C-style enum): the C return is the
    /// value. Nothing to free.
    Direct,
    /// An optional scalar, bool, or C-style enum: the C return is a `bool`
    /// (present), and the value is written to `T* out_value`. Nothing to
    /// free; on failure the return is `false`.
    OptDirect {
        /// The `T* out_value` slot.
        out_value: AbiParam,
    },
    /// A list of a numeric primitive returned as a typed array: the C
    /// return is `T*` and the element count is written to `size_t*
    /// out_len`. Copy the elements, then release the run with
    /// `{prefix}_free_bytes((uint8_t*)ptr, len * sizeof(T))` (see
    /// [`Prim::size`]).
    Slice {
        /// The `size_t* out_len` slot (element count).
        out_len: AbiParam,
        /// The element primitive.
        elem: Prim,
    },
    /// Owned UTF-8: the C return is `const uint8_t*` and the byte length is
    /// written to `size_t* out_len`. Decode into the native string, then
    /// release with `{prefix}_free_bytes(ptr, len)`.
    String {
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// Owned raw bytes, returned like [`String`](Self::String): copy, then
    /// release with `{prefix}_free_bytes(ptr, len)`.
    Bytes {
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// An owned value buffer, returned like [`String`](Self::String):
    /// decode via the wire format ([`Ty::wire`]), adopting any object tokens
    /// it carries, then release with `{prefix}_free_bytes(ptr, len)`.
    Buffer {
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// One strong object reference (`{tag}*`) the wrapper adopts into its
    /// disposal idiom, eventually released with `destroy_symbol`.
    Object {
        /// `true` for `Interface?`: a null return is a legal "none" result.
        nullable: bool,
        /// The interface's bare name.
        interface: String,
        /// The interface's `{tag}_destroy` symbol.
        destroy_symbol: String,
    },
    /// An iterator handle (`{iter_tag}*`) the wrapper drives lazily with
    /// [`IteratorBinding::next`] and releases exactly once with
    /// [`IteratorBinding::destroy_symbol`].
    Iterator(IteratorBinding),
}

impl RetPass {
    /// The trailing out slots this return adds before `out_err`, in order.
    #[must_use]
    pub fn out_slots(&self) -> Vec<&AbiParam> {
        match self {
            RetPass::OptDirect { out_value } => vec![out_value],
            RetPass::Slice { out_len, .. }
            | RetPass::String { out_len }
            | RetPass::Bytes { out_len }
            | RetPass::Buffer { out_len } => vec![out_len],
            RetPass::Void | RetPass::Direct | RetPass::Object { .. } | RetPass::Iterator(_) => {
                vec![]
            }
        }
    }
}

/// How an async call's result arrives in its completion callback, after the
/// `(void* context, {prefix}_error* err)` prefix.
///
/// Everything the callback receives is owned by the consumer. On failure
/// (`err` non-null) the result slots hold zero values the consumer must not
/// use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultPass {
    /// A `void` function: no result slots.
    Void,
    /// By value: `T result`.
    Direct {
        /// The `T result` slot.
        result: AbiParam,
    },
    /// An optional scalar, bool, or C-style enum: `bool has_result`, then
    /// `T result` (ignored when `has_result` is false).
    OptDirect {
        /// The `bool has_result` slot.
        has: AbiParam,
        /// The `T result` slot.
        value: AbiParam,
    },
    /// A typed array: `const T* result_ptr`, `size_t result_len` (the
    /// element count). Copy, then release with
    /// `{prefix}_free_bytes((uint8_t*)ptr, len * sizeof(T))`.
    Slice {
        /// The `const T*` slot.
        ptr: AbiParam,
        /// The `size_t` element-count slot.
        len: AbiParam,
        /// The element primitive.
        elem: Prim,
    },
    /// Owned UTF-8: `const uint8_t* result_ptr`, `size_t result_len`.
    /// Decode, then release with `{prefix}_free_bytes(ptr, len)`.
    String {
        /// The `const uint8_t*` slot.
        ptr: AbiParam,
        /// The `size_t` byte-length slot.
        len: AbiParam,
    },
    /// Owned raw bytes, delivered like [`String`](Self::String).
    Bytes {
        /// The `const uint8_t*` slot.
        ptr: AbiParam,
        /// The `size_t` byte-length slot.
        len: AbiParam,
    },
    /// An owned value buffer, delivered like [`String`](Self::String):
    /// decode (adopting object tokens), then release.
    Buffer {
        /// The `const uint8_t*` slot.
        ptr: AbiParam,
        /// The `size_t` byte-length slot.
        len: AbiParam,
    },
    /// One strong object reference: `{tag}* result`, adopted and eventually
    /// released with `destroy_symbol`.
    Object {
        /// The `{tag}* result` slot.
        result: AbiParam,
        /// `true` for `Interface?`: null is a legal "none" result.
        nullable: bool,
        /// The interface's bare name.
        interface: String,
        /// The interface's `{tag}_destroy` symbol.
        destroy_symbol: String,
    },
}

impl ResultPass {
    /// The result slots, in signature order.
    #[must_use]
    pub fn slots(&self) -> Vec<&AbiParam> {
        match self {
            ResultPass::Void => vec![],
            ResultPass::Direct { result } | ResultPass::Object { result, .. } => vec![result],
            ResultPass::OptDirect { has, value } => vec![has, value],
            ResultPass::Slice { ptr, len, .. }
            | ResultPass::String { ptr, len }
            | ResultPass::Bytes { ptr, len }
            | ResultPass::Buffer { ptr, len } => vec![ptr, len],
        }
    }
}

/// How one iterator element arrives through `{iter_tag}_next`'s out slots
/// (after `iter`, before `out_err`). `next` returns `1` when it wrote an
/// element and `0` when the iterator is exhausted (or failed).
///
/// Each element is owned by the consumer, exactly like a return of its
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemPass {
    /// By value: `T* out_item`.
    Direct {
        /// The `T* out_item` slot.
        out_item: AbiParam,
    },
    /// An optional scalar, bool, or C-style enum: `bool* out_has_item`,
    /// then `T* out_item` (meaningful when `*out_has_item`).
    OptDirect {
        /// The `bool* out_has_item` slot.
        out_has: AbiParam,
        /// The `T* out_item` slot.
        out_item: AbiParam,
    },
    /// A typed array: `T** out_item`, `size_t* out_len` (the element
    /// count). Copy, then release with
    /// `{prefix}_free_bytes((uint8_t*)ptr, len * sizeof(T))`.
    Slice {
        /// The `T** out_item` slot.
        out_item: AbiParam,
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
        /// The element primitive.
        elem: Prim,
    },
    /// Owned UTF-8: `const uint8_t** out_item`, `size_t* out_len`. Decode,
    /// then release with `{prefix}_free_bytes(ptr, len)`.
    String {
        /// The `const uint8_t** out_item` slot.
        out_item: AbiParam,
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// Owned raw bytes, delivered like [`String`](Self::String).
    Bytes {
        /// The `const uint8_t** out_item` slot.
        out_item: AbiParam,
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// An owned value buffer, delivered like [`String`](Self::String):
    /// decode (adopting object tokens), then release.
    Buffer {
        /// The `const uint8_t** out_item` slot.
        out_item: AbiParam,
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// One strong object reference: `{tag}** out_item`, adopted and
    /// eventually released with `destroy_symbol`.
    Object {
        /// The `{tag}** out_item` slot.
        out_item: AbiParam,
        /// `true` for `Interface?`: a null element is a legal "none".
        nullable: bool,
        /// The interface's bare name.
        interface: String,
        /// The interface's `{tag}_destroy` symbol.
        destroy_symbol: String,
    },
}

impl ItemPass {
    /// The item's out slots, in signature order.
    #[must_use]
    pub fn slots(&self) -> Vec<&AbiParam> {
        match self {
            ItemPass::Direct { out_item } | ItemPass::Object { out_item, .. } => vec![out_item],
            ItemPass::OptDirect { out_has, out_item } => vec![out_has, out_item],
            ItemPass::Slice {
                out_item, out_len, ..
            }
            | ItemPass::String { out_item, out_len }
            | ItemPass::Bytes { out_item, out_len }
            | ItemPass::Buffer { out_item, out_len } => vec![out_item, out_len],
        }
    }
}

/// How a callback method's return crosses back from the consumer's
/// trampoline to the producer, which adopts it.
///
/// The producer adopts whatever the return and out slots hold whether or
/// not the method failed, so a trampoline that fails after allocating
/// doesn't leak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackRetPass {
    /// No return value: the C return is `void`.
    Void,
    /// By value: the C return is the value.
    Direct,
    /// An optional scalar, bool, or C-style enum: the C return is a `bool`
    /// (present), and the trampoline writes the value to `T* out_value`.
    OptDirect {
        /// The `T* out_value` slot.
        out_value: AbiParam,
    },
    /// A typed array: the C return is `void`; the trampoline allocates
    /// `len * sizeof(T)` bytes with `{prefix}_alloc`, copies the elements
    /// in, and writes the pointer to `T** out_ptr` and the element count to
    /// `size_t* out_len`. Null with count `0` is the empty list.
    Slice {
        /// The `T** out_ptr` slot.
        out_ptr: AbiParam,
        /// The `size_t* out_len` slot (element count).
        out_len: AbiParam,
        /// The element primitive.
        elem: Prim,
    },
    /// UTF-8: the C return is `void`; the trampoline allocates the run with
    /// `{prefix}_alloc` and writes it to `uint8_t** out_ptr` and its byte
    /// length to `size_t* out_len`.
    String {
        /// The `uint8_t** out_ptr` slot.
        out_ptr: AbiParam,
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// Raw bytes, returned like [`String`](Self::String).
    Bytes {
        /// The `uint8_t** out_ptr` slot.
        out_ptr: AbiParam,
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// A value buffer, returned like [`String`](Self::String); any object
    /// token in it is a freshly cloned reference.
    Buffer {
        /// The `uint8_t** out_ptr` slot.
        out_ptr: AbiParam,
        /// The `size_t* out_len` slot.
        out_len: AbiParam,
    },
    /// One strong object reference as the C return (`{tag}*`): a fresh
    /// `clone_symbol` of the wrapper's handle, which the producer adopts.
    Object {
        /// `true` for `Interface?`: the trampoline may return null.
        nullable: bool,
        /// The interface's bare name.
        interface: String,
        /// The interface's `{tag}_clone` symbol.
        clone_symbol: String,
    },
}

impl CallbackRetPass {
    /// The trailing out slots this return adds before `out_err`, in order.
    #[must_use]
    pub fn out_slots(&self) -> Vec<&AbiParam> {
        match self {
            CallbackRetPass::OptDirect { out_value } => vec![out_value],
            CallbackRetPass::Slice {
                out_ptr, out_len, ..
            }
            | CallbackRetPass::String { out_ptr, out_len }
            | CallbackRetPass::Bytes { out_ptr, out_len }
            | CallbackRetPass::Buffer { out_ptr, out_len } => vec![out_ptr, out_len],
            CallbackRetPass::Void | CallbackRetPass::Direct | CallbackRetPass::Object { .. } => {
                vec![]
            }
        }
    }
}
