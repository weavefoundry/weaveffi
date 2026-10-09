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
//!   wrapper does with the result (use, copy, decode, or adopt) and which
//!   release it owes (`{prefix}_free_bytes` or the interface's `_destroy`).
//! * **Errors** ([`ErrorStrategy`]): when a call reports through `out_err`,
//!   is that a typed domain error the caller can catch, or a producer bug the
//!   wrapper must trap on?
//! * **Iterators** ([`IteratorProtocol`]): the pull contract of `iter<T>`,
//!   including the requirement that wrappers stay **lazy** (one producer
//!   `next` per consumer step, never a hidden drain into a list).
//! * **Async** ([`AsyncProtocol`]): the completion-callback contract,
//!   including the rule that results and errors are owned by the consumer
//!   and released through the runtime free symbols.
//! * **Callback interfaces** ([`CallbackProtocol`]): the contract for the
//!   consumer-implemented vtable the producer calls back into.
//!
//! Every classification here derives from [`Ty::family`] and every symbol
//! comes from the [`Model`](crate::model::Model), so a backend that renders
//! these plans in its own syntax cannot drift from the others on semantics;
//! only the spelling differs.

use crate::abi::AbiParam;
use crate::model::{
    AsyncBinding, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding, IteratorBinding,
    ParamBinding,
};
use crate::ty::{Family, Ty};

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
    /// A *negative* code (generic `-1`, panic `-2`, marshalling failure
    /// `-3`, foreign callback failure `-4`, cancelled `-5`) surfaces through
    /// the same channel as the target's runtime error type, distinct from
    /// every domain error type, and `-5` as the language's cancellation error
    /// where it has one.
    Throws,
    /// The function does not throw: the only way `out_err` reports failure
    /// is a producer bug or a runtime trap (a caught panic, code `-2`; a
    /// consumer callback that raised, code `-4`). The wrapper surfaces it
    /// through the target's *programming-error* idiom (an unchecked exception
    /// where the language has them, a Go `panic`, a Swift `fatalError`).
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
    /// A borrowed `(ptr, len)` pair of UTF-8 bytes, not NUL-terminated. The
    /// wrapper encodes the string and keeps the encoding alive for the
    /// duration of the call; the producer copies what it needs.
    String {
        /// The `const uint8_t*` slot.
        ptr: &'a AbiParam,
        /// The `size_t` length slot.
        len: &'a AbiParam,
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
    /// (the interface's
    /// [`clone_symbol`](crate::model::InterfaceBinding::clone_symbol)).
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
    /// calls the vtable's `free` (see [`CallbackProtocol`]). When
    /// `nullable`, the IDL type is `Cb?` and a null `vtable` means none.
    Callback {
        /// The `void*` context slot.
        ctx: &'a AbiParam,
        /// The `const {vtable}*` slot.
        vtable: &'a AbiParam,
        /// `true` for `Cb?`: a null vtable is a legal "none" argument.
        nullable: bool,
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
            Family::String => {
                let (ptr, len) = pair();
                ArgPass::String { ptr, len }
            }
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
            Family::Callback { nullable } => {
                let (ctx, vtable) = pair();
                ArgPass::Callback {
                    ctx,
                    vtable,
                    nullable,
                }
            }
            Family::Iterator => unreachable!("iterators are never parameters"),
        }
    }
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
    /// Owned `(const uint8_t*, out_len)` UTF-8: decode into the native string,
    /// then release with
    /// `{prefix}_free_bytes`.
    String,
    /// Owned `(const uint8_t*, out_len)` raw bytes: copy, then release with
    /// `{prefix}_free_bytes`.
    Bytes,
    /// Owned `(const uint8_t*, out_len)` value buffer: decode via the wire
    /// format ([`Ty::wire`]), adopting any object tokens it carries, then
    /// release with `{prefix}_free_bytes`.
    Buffer,
    /// One strong object reference the wrapper adopts into its disposal
    /// idiom, eventually released with the interface's `_destroy` symbol
    /// ([`InterfaceBinding::destroy_symbol`], from
    /// [`Model::interface`](crate::model::Model::interface)). When
    /// `nullable`, the IDL type is `Interface?` and a null return means
    /// none.
    ///
    /// [`InterfaceBinding::destroy_symbol`]: crate::model::InterfaceBinding::destroy_symbol
    Object {
        /// `true` for `Interface?`: a null return is a legal "none" result.
        nullable: bool,
    },
}

impl RetPass {
    /// The receiving contract for a value of type `ty` produced by a
    /// callable. `None` (a void return) is [`RetPass::Void`].
    ///
    /// # Panics
    ///
    /// Panics on an iterator return, whose contract is [`IteratorProtocol`],
    /// not a value-passing plan (backends dispatch on
    /// [`CallShape`](crate::model::CallShape) before consulting this), and on
    /// a callback interface, which validation never admits as a return.
    pub fn of(ty: Option<&Ty>) -> RetPass {
        let Some(ty) = ty else {
            return RetPass::Void;
        };
        match ty.family() {
            Family::Direct => RetPass::Direct,
            Family::String => RetPass::String,
            Family::Bytes => RetPass::Bytes,
            Family::Buffer => RetPass::Buffer,
            Family::Object { nullable } => RetPass::Object { nullable },
            Family::Callback { .. } => panic!("callback interfaces are never returned"),
            Family::Iterator => panic!("iterator returns follow IteratorProtocol, not a RetPass"),
        }
    }
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
    /// How `out_err` reports from the launcher and each `next` call are
    /// interpreted.
    pub error: ErrorStrategy,
}

impl IteratorBinding {
    /// Build the full pull contract for this iterator of `f`.
    pub fn protocol<'a>(&'a self, f: &FnBinding) -> IteratorProtocol<'a> {
        IteratorProtocol {
            binding: self,
            elem: RetPass::of(Some(&self.elem)),
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
///    consumer. String results are released with `{prefix}_free_bytes`,
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
    /// owed (`{prefix}_free_bytes` or the destroy symbol an adopted object
    /// owes).
    pub result: RetPass,
    /// How the callback's `err` slot is interpreted.
    pub error: ErrorStrategy,
}

impl AsyncBinding {
    /// Build the full completion contract for this async function `f`.
    pub fn protocol<'a>(&'a self, f: &FnBinding) -> AsyncProtocol<'a> {
        AsyncProtocol {
            binding: self,
            cancellable: f.cancellable,
            result: RetPass::of(f.ret.as_ref()),
            error: f.error_strategy(),
        }
    }
}

/// The callback-interface contract every backend renders.
///
/// A callback interface is the consumer's side of the boundary: the consumer
/// supplies an implementation, the producer calls it. The contract has five
/// clauses:
///
/// 1. **One static vtable per interface.** The wrapper emits exactly one
///    process-wide vtable value for the interface. It starts with the fixed
///    header, `size` (`sizeof` the vtable as the wrapper compiled it),
///    `flags` (`0`), and `free`, followed by one trampoline per method from
///    the C signature ([`CallbackMethodBinding::abi_params`]) into the native
///    implementation. The producer rejects a vtable smaller than the one it
///    was built with, so a stale binding fails with `-3` instead of calling
///    through a missing slot.
/// 2. **Context is a handle-table key.** The wrapper stores the native
///    implementation in a table keyed by an integer or pointer it passes as
///    `ctx`, so the implementation stays alive as long as the producer holds
///    the callback and garbage collectors never see a raw pointer. `free`
///    may run on any producer thread.
/// 3. **Arguments are received like returns.** Strings, bytes, and buffers
///    arriving in a trampoline are borrowed for the call: the wrapper copies
///    or decodes them before returning and frees nothing. Object arguments
///    transfer one strong reference the wrapper adopts.
/// 4. **Returns transfer to the producer** ([`method_returns`](Self::method_returns)).
///    A direct value is the C return. An object is returned as one strong
///    reference (a fresh `_clone` of the wrapper's handle). A string, bytes,
///    or buffer is written to the trailing `out_ptr`/`out_len` slots as a run
///    the wrapper allocates with `{prefix}_alloc`; the producer adopts it.
/// 5. **Failures go through `out_err`.** When the native implementation
///    raises, the trampoline calls `{prefix}_error_set(out_err, code,
///    message)` and returns a zero value; it must never let an exception
///    unwind through the C frame. A method whose
///    [`method_errors`](Self::method_errors) entry is
///    [`ErrorStrategy::Throws`] reports a declared domain error with its
///    positive code and its payload fields (`{prefix}_error_set_payload`);
///    every other failure uses `-4`, and the producer treats any code it
///    can't attribute to the method's domain as `-4`.
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
    /// How each method's return value crosses back to the producer, in
    /// method order. [`RetPass::String`], [`RetPass::Bytes`], and
    /// [`RetPass::Buffer`] are written to the `out_ptr`/`out_len` slots as a
    /// `{prefix}_alloc` run.
    pub method_returns: Vec<RetPass>,
    /// How each method may report failure, in method order.
    pub method_errors: Vec<ErrorStrategy>,
}

impl CallbackMethodBinding {
    /// The error strategy of this method: [`ErrorStrategy::Throws`] when it
    /// declares `throws: true`, otherwise [`ErrorStrategy::Trap`] (any
    /// failure reaches the producer as `-4`).
    pub fn error_strategy(&self) -> ErrorStrategy {
        if self.throws {
            ErrorStrategy::Throws
        } else {
            ErrorStrategy::Trap
        }
    }
}

impl CallbackInterfaceBinding {
    /// Build the full contract for this callback interface.
    pub fn protocol(&self) -> CallbackProtocol<'_> {
        CallbackProtocol {
            binding: self,
            method_args: self
                .methods
                .iter()
                .map(|m| m.params.iter().map(|p| RetPass::of(Some(&p.ty))).collect())
                .collect(),
            method_returns: self
                .methods
                .iter()
                .map(|m| RetPass::of(m.ret.as_ref()))
                .collect(),
            method_errors: self
                .methods
                .iter()
                .map(CallbackMethodBinding::error_strategy)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Api, Function, Param, TypeRef};
    use crate::model::Model;
    use crate::pkg::Identity;
    use crate::ty::Prim;

    fn param(name: &str, ty: &str) -> Param {
        Param {
            name: name.into(),
            ty: crate::ir::parse_type_ref(ty).unwrap(),
            doc: None,
        }
    }

    /// A model with interface `Store` and callback interface `Listener` in
    /// `kv`, a record `Contact`, and one function `f` taking `params`.
    fn model(params: Vec<Param>) -> Model {
        let yaml = r#"
version: "0.11.0"
modules:
  - name: kv
    structs: [{ name: Contact, fields: [{ name: id, type: i64 }] }]
    interfaces: [{ name: Store, methods: [{ name: get }] }]
    callback_interfaces: [{ name: Listener, methods: [{ name: on }] }]
"#;
        let mut api: Api = serde_yaml::from_str(yaml).unwrap();
        api.modules[0].functions.push(Function {
            name: "f".into(),
            params,
            returns: Some(TypeRef::Prim(Prim::I32)),
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
        });
        Model::assume_valid(&api, Identity::named("weaveffi"))
    }

    #[test]
    fn arg_pass_classifies_every_family() {
        let m = model(vec![
            param("x", "i32"),
            param("s", "string"),
            param("data", "bytes"),
            param("c", "Contact"),
            param("o", "i32?"),
            param("store", "Store"),
            param("maybe", "Store?"),
            param("l", "Listener"),
            param("ml", "Listener?"),
        ]);
        let p = &m.modules[0].functions[0].params;
        assert!(matches!(p[0].arg_pass(), ArgPass::Direct { slot } if slot.name == "x"));
        assert!(matches!(p[1].arg_pass(), ArgPass::String { ptr, .. } if ptr.name == "s_ptr"));
        assert!(matches!(
            p[2].arg_pass(),
            ArgPass::Bytes { ptr, len } if ptr.name == "data_ptr" && len.name == "data_len"
        ));
        assert!(matches!(
            p[3].arg_pass(),
            ArgPass::Buffer { ptr, len } if ptr.name == "c_ptr" && len.name == "c_len"
        ));
        assert!(matches!(p[4].arg_pass(), ArgPass::Buffer { .. }));
        assert!(matches!(
            p[5].arg_pass(),
            ArgPass::Object {
                nullable: false,
                ..
            }
        ));
        assert!(matches!(
            p[6].arg_pass(),
            ArgPass::Object { nullable: true, .. }
        ));
        assert!(matches!(
            p[7].arg_pass(),
            ArgPass::Callback { ctx, vtable, nullable: false }
                if ctx.name == "l_ctx" && vtable.name == "l_vtable"
        ));
        assert!(matches!(
            p[8].arg_pass(),
            ArgPass::Callback { nullable: true, .. }
        ));
    }

    #[test]
    fn ret_pass_distinguishes_copy_decode_and_adopt() {
        assert_eq!(RetPass::of(None), RetPass::Void);
        assert_eq!(RetPass::of(Some(&Ty::Prim(Prim::I64))), RetPass::Direct);
        assert_eq!(RetPass::of(Some(&Ty::Prim(Prim::String))), RetPass::String);
        assert_eq!(RetPass::of(Some(&Ty::Prim(Prim::Bytes))), RetPass::Bytes);
        for ty in [
            Ty::Record("Contact".into()),
            Ty::RichEnum("Shape".into()),
            Ty::List(Box::new(Ty::Prim(Prim::String))),
            Ty::List(Box::new(Ty::Interface("Store".into()))),
            Ty::Optional(Box::new(Ty::Prim(Prim::I64))),
        ] {
            assert_eq!(RetPass::of(Some(&ty)), RetPass::Buffer, "{ty}");
        }
        assert_eq!(
            RetPass::of(Some(&Ty::Interface("Store".into()))),
            RetPass::Object { nullable: false }
        );
        assert_eq!(
            RetPass::of(Some(&Ty::Optional(Box::new(Ty::Interface("Store".into()))))),
            RetPass::Object { nullable: true }
        );
    }
}
