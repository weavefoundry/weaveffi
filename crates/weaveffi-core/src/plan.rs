//! The **marshalling plan**: the language-neutral calling contracts every
//! backend renders, stated once.
//!
//! The [`crate::model`] layer answers *which symbols exist and what their C
//! signatures are*. This module answers the questions one level up, the ones
//! the eleven generators used to answer independently (and inconsistently):
//!
//! * **Passing** ([`ArgPass`], [`RetPass`]): how each argument crosses into
//!   its ABI slots (by value, pinned string/bytes, serialized value buffer,
//!   borrowed object pointer, or callback context plus vtable) and what the
//!   wrapper does with the result (use, copy, decode, or adopt) before any
//!   owed release.
//! * **Errors** ([`ErrorStrategy`]): when a call reports through `out_err`,
//!   is that a typed domain error the caller can catch, or a producer bug the
//!   wrapper must trap on?
//! * **Ownership** ([`RetPass::free`], [`Free`]): after copying a returned
//!   value into a native one, exactly which runtime release call does the
//!   wrapper owe, if any?
//! * **Iterators** ([`IteratorProtocol`]): the pull contract of `iter<T>`,
//!   including the requirement that wrappers stay **lazy** (one producer
//!   `next` per consumer step, never a hidden drain into a list).
//! * **Async** ([`AsyncProtocol`]): the completion-callback contract,
//!   including the rule that results and errors are owned by the consumer
//!   and released through the runtime free symbols.
//! * **Callback interfaces** ([`CallbackProtocol`]): the contract for the
//!   consumer-implemented vtable the producer calls back into.
//!
//! Every classification here derives from [`Ty::family`], so a backend that
//! renders these plans in its own syntax cannot drift from the others on
//! semantics; only the spelling differs.

use crate::abi::split_qualified;
use crate::abi::AbiParam;
use crate::model::{
    AsyncBinding, CallbackInterfaceBinding, Family, FnBinding, IteratorBinding, ParamBinding, Ty,
};

/// How a callable's `out_err` slot is interpreted by idiomatic wrappers.
///
/// Every synchronous C ABI entry point carries a trailing `out_err`, and every
/// async completion callback carries an `err` slot, regardless of `throws`.
/// What differs is the *meaning* of a non-zero code, and every backend must
/// agree on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorStrategy {
    /// The function declares `throws: true`: a non-zero code is a typed
    /// domain error. The wrapper maps the code onto the module's error
    /// domain (an exception subclass, a Swift `Error` enum case, a Go
    /// `error` value, ...), decodes any payload fields from the error's
    /// `payload_ptr`/`payload_len` buffer, and surfaces it through the
    /// target's normal error channel so callers can catch and match on it.
    /// A *negative* code (a runtime trap: panic, marshalling failure,
    /// foreign callback failure) is still a programming error and follows
    /// [`Trap`](Self::Trap) even on a throwing function.
    Throws,
    /// The function does not throw: the only way `out_err` reports failure
    /// is a producer bug or a runtime trap (a caught panic, code `-2`; a
    /// consumer callback that raised, code `-4`). The wrapper surfaces it
    /// through the target's *programming-error* idiom (a Python
    /// `WeaveFFIError`, a Go `panic`, a Swift `fatalError`, a C# exception).
    /// It must never be silently ignored, and it must never be dressed up as
    /// a typed domain error.
    Trap,
}

impl FnBinding {
    /// The error strategy of this callable: [`ErrorStrategy::Throws`] when the
    /// IDL declares `throws: true`, otherwise [`ErrorStrategy::Trap`].
    pub fn error_strategy(&self) -> ErrorStrategy {
        if self.throws {
            ErrorStrategy::Throws
        } else {
            ErrorStrategy::Trap
        }
    }
}

/// How one parameter crosses the call boundary: the passing contract a
/// wrapper renders when marshalling its native argument into ABI slots.
///
/// Exactly one variant applies to any parameter, and the borrowed
/// [`AbiParam`] references point at the parameter's own precomputed slots,
/// so a backend that dispatches on this enum cannot disagree with the C
/// header about arity or slot order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgPass<'a> {
    /// One slot passed by value: scalars, bools, and C-style enums.
    Direct {
        /// The single ABI slot.
        slot: &'a AbiParam,
    },
    /// One `char*` slot. The wrapper encodes UTF-8 plus a NUL terminator
    /// and keeps the encoding alive for the duration of the call; the
    /// producer copies what it needs.
    String {
        /// The single `char*` slot.
        slot: &'a AbiParam,
    },
    /// A borrowed `(ptr, len)` byte pair. The wrapper pins its native byte
    /// storage for the call; the producer copies what it needs.
    Bytes {
        /// The `uint8_t*` data slot.
        ptr: &'a AbiParam,
        /// The `size_t` length slot.
        len: &'a AbiParam,
    },
    /// A buffered value (record, rich enum, optional, list, or map): the
    /// wrapper serializes it into the value-buffer wire format
    /// ([`Ty::wire`]), passes the encoding as a borrowed `(ptr, len)` pair,
    /// and releases its own encoding after the call returns. Any object
    /// token written into the encoding must be a freshly cloned reference
    /// (see [`clone_symbol`]).
    Buffer {
        /// The `const uint8_t*` data slot.
        ptr: &'a AbiParam,
        /// The `size_t` length slot.
        len: &'a AbiParam,
    },
    /// A borrowed object pointer: the wrapper passes the wrapped object's
    /// native handle and retains its own reference. When `nullable`, the IDL
    /// type is `Interface?` and null means none.
    Object {
        /// The single object-pointer slot.
        slot: &'a AbiParam,
        /// `true` for `Interface?`: null is a legal "none" argument.
        nullable: bool,
    },
    /// A callback interface: the wrapper registers its native implementation
    /// in a handle table, passes the table key as `ctx` and the interface's
    /// static vtable as `vtable`, and removes the entry when the producer
    /// calls the vtable's `free` (see [`CallbackProtocol`]).
    Callback {
        /// The `void*` context slot.
        ctx: &'a AbiParam,
        /// The `const {vtable}*` slot.
        vtable: &'a AbiParam,
    },
}

impl ParamBinding {
    /// The passing contract for this parameter.
    ///
    /// # Panics
    ///
    /// Panics if the parameter's precomputed ABI slots disagree with its
    /// type's family, which would be a bug in the model construction, not a
    /// user error.
    pub fn arg_pass(&self) -> ArgPass<'_> {
        let pair = || {
            assert!(
                self.abi.len() == 2,
                "two-slot parameter '{}' must have exactly two ABI slots",
                self.name
            );
            (&self.abi[0], &self.abi[1])
        };
        let single = || {
            assert!(
                self.abi.len() == 1,
                "single-slot parameter '{}' must have exactly one ABI slot",
                self.name
            );
            &self.abi[0]
        };
        match self.ty.family() {
            Family::Direct => ArgPass::Direct { slot: single() },
            Family::String => ArgPass::String { slot: single() },
            Family::Bytes => {
                let (ptr, len) = pair();
                ArgPass::Bytes { ptr, len }
            }
            Family::Buffer => {
                let (ptr, len) = pair();
                ArgPass::Buffer { ptr, len }
            }
            Family::Object { nullable } => ArgPass::Object {
                slot: single(),
                nullable,
            },
            Family::Callback => {
                let (ctx, vtable) = pair();
                ArgPass::Callback { ctx, vtable }
            }
            Family::Iterator => unreachable!("iterators are never parameters"),
        }
    }
}

/// The runtime release a consumer wrapper owes for a producer-allocated
/// value it has copied or decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Free {
    /// By-value: nothing to free.
    None,
    /// A `const char*`: release with `{prefix}_free_string`.
    String,
    /// A `ptr` + `len` allocation (raw bytes or a value buffer): release with
    /// `{prefix}_free_bytes(ptr, len)`.
    Bytes,
}

/// How a value produced by the producer crosses back to the wrapper: the
/// receiving contract, including the decode step and the release obligation.
///
/// Sync returns, async results, and iterator elements all use this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetPass {
    /// No return value.
    Void,
    /// By-value return (scalar, bool, C-style enum): use directly, nothing to
    /// free.
    Direct,
    /// Owned `const char*`: copy into the native string, then release with
    /// `{prefix}_free_string`.
    String,
    /// Owned `(const uint8_t*, out_len)` raw bytes: copy, then release with
    /// `{prefix}_free_bytes`.
    Bytes,
    /// Owned `(const uint8_t*, out_len)` value buffer: decode via the wire
    /// format ([`Ty::wire`]), adopting any object tokens it carries, then
    /// release with `{prefix}_free_bytes`.
    Buffer,
    /// One strong object reference the wrapper adopts into its disposal
    /// idiom. When `nullable`, the IDL type is `Interface?` and a null return
    /// means none.
    Object {
        /// The `{prefix}_{module}_{Name}_destroy` symbol the adopted
        /// reference eventually owes.
        destroy_symbol: String,
        /// The `{prefix}_{module}_{Name}_clone` symbol the wrapper calls when
        /// it needs a second reference (for example to write the object into
        /// a value buffer).
        clone_symbol: String,
        /// `true` for `Interface?`: a null return is a legal "none" result.
        nullable: bool,
    },
}

impl RetPass {
    /// The runtime release owed after copying or decoding the result.
    /// Adopted objects owe their `destroy` symbol instead (see
    /// [`RetPass::Object`]), so they report [`Free::None`] here.
    pub fn free(&self) -> Free {
        match self {
            RetPass::String => Free::String,
            RetPass::Bytes | RetPass::Buffer => Free::Bytes,
            RetPass::Void | RetPass::Direct | RetPass::Object { .. } => Free::None,
        }
    }
}

/// The receiving contract for a value of type `ty` produced by a callable
/// declared inside `module` under `prefix`. `None` (a void return) is
/// [`RetPass::Void`].
///
/// # Panics
///
/// Panics on an iterator return, whose contract is [`IteratorProtocol`], not
/// a value-passing plan (backends dispatch on
/// [`CallShape`](crate::model::CallShape) before consulting this), and on a
/// callback interface, which validation never admits as a return.
pub fn ret_pass(ty: Option<&Ty>, module: &str, prefix: &str) -> RetPass {
    let Some(ty) = ty else {
        return RetPass::Void;
    };
    match ty.family() {
        Family::Direct => RetPass::Direct,
        Family::String => RetPass::String,
        Family::Bytes => RetPass::Bytes,
        Family::Buffer => RetPass::Buffer,
        Family::Object { nullable } => {
            let iface = ty
                .interface_name()
                .expect("object family names an interface");
            RetPass::Object {
                destroy_symbol: destroy_symbol(iface, module, prefix),
                clone_symbol: clone_symbol(iface, module, prefix),
                nullable,
            }
        }
        Family::Callback => panic!("callback interfaces are never returned"),
        Family::Iterator => panic!("iterator returns follow IteratorProtocol, not a RetPass"),
    }
}

/// The per-element release owed for one iterator element of type `ty`. An
/// object element is adopted (see [`IteratorProtocol::elem`]) and owes
/// [`Free::None`] here.
pub fn elem_free(ty: &Ty) -> Free {
    match ty.family() {
        Family::String => Free::String,
        Family::Bytes | Family::Buffer => Free::Bytes,
        _ => Free::None,
    }
}

/// The `{prefix}_{module}_{Name}_destroy` symbol for a (possibly
/// dot-qualified) interface name referenced from `current_module`.
pub fn destroy_symbol(name: &str, current_module: &str, prefix: &str) -> String {
    let (module, name) = split_qualified(name, current_module);
    format!("{prefix}_{module}_{name}_destroy")
}

/// The `{prefix}_{module}_{Name}_clone` symbol for a (possibly
/// dot-qualified) interface name referenced from `current_module`.
pub fn clone_symbol(name: &str, current_module: &str, prefix: &str) -> String {
    let (module, name) = split_qualified(name, current_module);
    format!("{prefix}_{module}_{name}_clone")
}

/// The `iter<T>` pull contract every backend renders.
///
/// The producer returns an opaque iterator handle; the consumer then calls
/// `next` once per element and `destroy` exactly once when done. The binding
/// contract has three clauses every wrapper must satisfy:
///
/// 1. **Laziness.** The wrapper exposes the target's native lazy iteration
///    idiom (a Python iterator, a Ruby `Enumerator`, a Go `iter.Seq2`, a C#
///    `IEnumerable`, a Dart `Iterable`, a JS iterable, a Swift `Sequence`, a
///    Kotlin `Iterator`) and issues **one producer `next` call per consumer
///    step**. Draining the producer into a hidden list defeats the point of
///    `iter<T>` (constant-memory streaming) and is a contract violation.
/// 2. **Element ownership.** Each `next` writes an element the consumer now
///    owns; the wrapper receives it exactly as it would a return of the same
///    type ([`elem`](Self::elem)): copy and free a string or bytes, decode
///    and free a buffer, adopt an object.
/// 3. **Handle lifecycle.** `destroy` is called exactly once: eagerly on
///    exhaustion, and from the wrapper's disposal idiom (RAII destructor,
///    finalizer, `close()`, generator cleanup) when iteration is abandoned
///    early.
///
/// Each `next` call also carries `out_err` and follows the owning function's
/// [`ErrorStrategy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IteratorProtocol<'a> {
    /// The lowered iterator surface: launcher, `next`, and destroy symbols.
    pub binding: &'a IteratorBinding,
    /// How each element written by `next` is received.
    pub elem: RetPass,
    /// The release owed for each element copied out of a `next` slot
    /// (`elem.free()`, kept for convenience).
    pub elem_free: Free,
    /// How `out_err` reports from the launcher and each `next` call are
    /// interpreted.
    pub error: ErrorStrategy,
}

impl IteratorBinding {
    /// Build the full pull contract for this iterator, resolving the element
    /// plan against the declaring `module` and `prefix`.
    pub fn protocol<'a>(
        &'a self,
        f: &FnBinding,
        module: &str,
        prefix: &str,
    ) -> IteratorProtocol<'a> {
        let elem = ret_pass(Some(&self.elem), module, prefix);
        IteratorProtocol {
            binding: self,
            elem_free: elem.free(),
            elem,
            error: f.error_strategy(),
        }
    }
}

/// The async completion contract every backend renders.
///
/// The launcher returns immediately; the producer later invokes the completion
/// callback exactly once, from an arbitrary producer thread. The contract has
/// three clauses:
///
/// 1. **Single completion.** The callback fires exactly once per launch; the
///    wrapper resolves its native future idiom (a Python `asyncio` future, a
///    JS `Promise`, a Swift continuation, a C# `TaskCompletionSource`, a Go
///    channel) exactly once and then releases the registration.
/// 2. **Owned results.** Everything passed to the callback is owned by the
///    consumer. String results are released with `{prefix}_free_string`,
///    byte and buffered-value results with `{prefix}_free_bytes`, and
///    interface-object results transfer one strong reference (the wrapper
///    adopts the pointer). This is what lets runtimes that defer callback
///    bodies past the native return (Dart's `NativeCallable.listener`, for
///    example) decode safely; a wrapper that processes results inline still
///    copies or decodes first and then frees.
/// 3. **Foreign-thread delivery.** The callback runs on a producer thread,
///    so the wrapper must hop back to its native scheduler before touching
///    consumer state (`call_soon_threadsafe`, a threadsafe function, a
///    dispatched continuation) rather than resolving inline where the
///    target's runtime forbids it.
///
/// The callback's `err` slot follows the owning function's [`ErrorStrategy`].
/// A non-null error is heap-boxed and owned by the consumer: the wrapper
/// copies the code, message, and payload, then releases the box with
/// `{prefix}_error_free` exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncProtocol<'a> {
    /// The lowered async surface: launcher and callback typedef.
    pub binding: &'a AsyncBinding,
    /// Whether the launcher carries a `cancel_token` slot before
    /// `callback`/`context`.
    pub cancellable: bool,
    /// How the callback's result slots are received, including the release
    /// owed ([`RetPass::free`]) or the destroy symbol an adopted object owes.
    pub result: RetPass,
    /// How the callback's `err` slot is interpreted.
    pub error: ErrorStrategy,
}

impl AsyncBinding {
    /// Build the full completion contract for this async function, resolving
    /// the result plan against the declaring `module` and `prefix`.
    pub fn protocol<'a>(&'a self, f: &FnBinding, module: &str, prefix: &str) -> AsyncProtocol<'a> {
        AsyncProtocol {
            binding: self,
            cancellable: f.cancellable,
            result: ret_pass(f.ret.as_ref(), module, prefix),
            error: f.error_strategy(),
        }
    }
}

/// The callback-interface contract every backend renders.
///
/// A callback interface is the consumer's side of the boundary: the consumer
/// supplies an implementation, the producer calls it. The contract has four
/// clauses:
///
/// 1. **One static vtable per interface.** The wrapper emits exactly one
///    process-wide vtable value for the interface whose entries are
///    trampolines from the C signature ([`CallbackMethodBinding::abi_params`])
///    into the native implementation, plus a trailing `free` entry.
/// 2. **Context is a handle-table key.** The wrapper stores the native
///    implementation in a table keyed by an integer or pointer it passes as
///    `ctx`, so the implementation stays alive as long as the producer holds
///    the callback and garbage collectors never see a raw pointer.
/// 3. **Arguments are received like returns.** Strings, bytes, and buffers
///    arriving in a trampoline are borrowed for the call: the wrapper copies
///    or decodes them before returning and frees nothing. Object arguments
///    transfer one strong reference the wrapper adopts. Method returns are
///    direct values written straight into the C return.
/// 4. **Foreign failures trap.** When the native implementation raises, the
///    trampoline calls `{prefix}_error_set(out_err, -4, message)` and returns
///    a default value; it must never let an exception unwind through the C
///    frame. The producer then aborts its call with `FOREIGN_ERROR_CODE`.
///
/// Trampolines may be invoked from any producer thread; the wrapper is
/// responsible for whatever thread affinity its runtime demands (a GIL
/// acquisition, a JNI attach, a threadsafe-function hop).
///
/// [`CallbackMethodBinding::abi_params`]: crate::model::CallbackMethodBinding::abi_params
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackProtocol<'a> {
    /// The lowered callback interface: vtable tag and methods.
    pub binding: &'a CallbackInterfaceBinding,
    /// How each method's parameters are received inside a trampoline, in
    /// method order then parameter order.
    pub method_args: Vec<Vec<RetPass>>,
}

impl CallbackInterfaceBinding {
    /// Build the full contract for this callback interface, resolving each
    /// parameter's receiving plan against the declaring `module` and
    /// `prefix`.
    pub fn protocol<'a>(&'a self, module: &str, prefix: &str) -> CallbackProtocol<'a> {
        CallbackProtocol {
            binding: self,
            method_args: self
                .methods
                .iter()
                .map(|m| {
                    m.params
                        .iter()
                        .map(|p| ret_pass(Some(&p.ty), module, prefix))
                        .collect()
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arg_pass_classifies_every_family() {
        let pb = |name: &str, ty: Ty| ParamBinding::new(name, ty, None, "m");
        assert!(matches!(
            pb("x", Ty::I32).arg_pass(),
            ArgPass::Direct { slot } if slot.name == "x"
        ));
        assert!(matches!(
            pb("s", Ty::StringUtf8).arg_pass(),
            ArgPass::String { slot } if slot.name == "s"
        ));
        assert!(matches!(
            pb("data", Ty::Bytes).arg_pass(),
            ArgPass::Bytes { ptr, len } if ptr.name == "data_ptr" && len.name == "data_len"
        ));
        assert!(matches!(
            pb("c", Ty::Record("Contact".into())).arg_pass(),
            ArgPass::Buffer { ptr, len } if ptr.name == "c_ptr" && len.name == "c_len"
        ));
        assert!(matches!(
            pb("o", Ty::Optional(Box::new(Ty::I32))).arg_pass(),
            ArgPass::Buffer { .. }
        ));
        assert!(matches!(
            pb("store", Ty::Interface("Store".into())).arg_pass(),
            ArgPass::Object {
                nullable: false,
                ..
            }
        ));
        assert!(matches!(
            pb(
                "store",
                Ty::Optional(Box::new(Ty::Interface("Store".into())))
            )
            .arg_pass(),
            ArgPass::Object { nullable: true, .. }
        ));
        assert!(matches!(
            pb("l", Ty::CallbackInterface("Listener".into())).arg_pass(),
            ArgPass::Callback { ctx, vtable } if ctx.name == "l_ctx" && vtable.name == "l_vtable"
        ));
    }

    #[test]
    fn ret_pass_distinguishes_copy_decode_and_adopt() {
        assert_eq!(ret_pass(None, "m", "weaveffi"), RetPass::Void);
        assert_eq!(ret_pass(Some(&Ty::I64), "m", "weaveffi"), RetPass::Direct);
        assert_eq!(
            ret_pass(Some(&Ty::StringUtf8), "m", "weaveffi"),
            RetPass::String
        );
        assert_eq!(ret_pass(Some(&Ty::Bytes), "m", "weaveffi"), RetPass::Bytes);
        for ty in [
            Ty::Record("Contact".into()),
            Ty::RichEnum("Shape".into()),
            Ty::List(Box::new(Ty::StringUtf8)),
            Ty::List(Box::new(Ty::Interface("Store".into()))),
            Ty::Optional(Box::new(Ty::I64)),
        ] {
            assert_eq!(
                ret_pass(Some(&ty), "m", "weaveffi"),
                RetPass::Buffer,
                "{ty}"
            );
        }
        assert_eq!(
            ret_pass(Some(&Ty::Interface("kv.Store".into())), "m", "weaveffi"),
            RetPass::Object {
                destroy_symbol: "weaveffi_kv_Store_destroy".into(),
                clone_symbol: "weaveffi_kv_Store_clone".into(),
                nullable: false,
            }
        );
        assert_eq!(
            ret_pass(
                Some(&Ty::Optional(Box::new(Ty::Interface("Store".into())))),
                "kv",
                "weaveffi"
            ),
            RetPass::Object {
                destroy_symbol: "weaveffi_kv_Store_destroy".into(),
                clone_symbol: "weaveffi_kv_Store_clone".into(),
                nullable: true,
            }
        );
        assert_eq!(RetPass::String.free(), Free::String);
        assert_eq!(RetPass::Buffer.free(), Free::Bytes);
        assert_eq!(RetPass::Direct.free(), Free::None);
    }

    #[test]
    fn iterator_elements_split_string_bytes_and_none() {
        assert_eq!(elem_free(&Ty::I32), Free::None);
        assert_eq!(elem_free(&Ty::StringUtf8), Free::String);
        assert_eq!(elem_free(&Ty::Bytes), Free::Bytes);
        assert_eq!(elem_free(&Ty::Record("Entry".into())), Free::Bytes);
        assert_eq!(elem_free(&Ty::Optional(Box::new(Ty::I32))), Free::Bytes);
        assert_eq!(elem_free(&Ty::Interface("Store".into())), Free::None);
    }
}
