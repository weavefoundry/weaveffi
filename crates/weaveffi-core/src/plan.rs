//! The **marshalling plan**: the language-neutral calling contracts every
//! backend renders, stated once.
//!
//! The [`crate::model`] layer answers *which symbols exist and what their C
//! signatures are*. This module answers the questions one level up, the ones
//! the eleven generators used to answer independently (and inconsistently):
//!
//! * **Passing** ([`ArgPass`], [`RetPass`]): how each argument crosses into
//!   its ABI slots (by value, pinned string/bytes, serialized value buffer,
//!   or borrowed object pointer) and what the wrapper does with the result
//!   (use, copy, decode, or adopt) before any owed release.
//! * **Errors** ([`ErrorStrategy`]): when a call reports through `out_err`,
//!   is that a typed domain error the caller can catch, or a producer bug the
//!   wrapper must trap on?
//! * **Ownership** ([`ReturnFree`], [`ElemFree`]): after copying a returned
//!   value into a native one, exactly which runtime release call does the
//!   wrapper owe, if any?
//! * **Iterators** ([`IteratorProtocol`]): the pull contract of `iter<T>`,
//!   including the requirement that wrappers stay **lazy** (one producer
//!   `next` per consumer step, never a hidden drain into a list).
//! * **Async** ([`AsyncProtocol`]): the completion-callback contract,
//!   including the rule that results and errors are owned by the consumer
//!   and released through the runtime free symbols.
//!
//! A backend that renders these plans in its own syntax cannot drift from the
//! others on semantics; only the spelling differs.

use weaveffi_ir::ir::TypeRef;

use crate::abi::lower::{is_buffered, split_qualified};
use crate::abi::AbiParam;
use crate::model::{AsyncBinding, FnBinding, IteratorBinding, ParamBinding};

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
    Throws,
    /// The function does not throw: the only way `out_err` reports failure
    /// is a producer bug (most commonly a caught panic, code `-2`). The
    /// wrapper surfaces it through the target's *programming-error* idiom
    /// (a Python `WeaveFFIError`, a Go `panic`, a Swift `fatalError`, a C#
    /// exception). It must never be silently ignored, and it must never be
    /// dressed up as a typed domain error.
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
    /// One slot passed by value: scalars, bools, C-style enums, and handles
    /// (including typed handles, whose slot is an opaque pointer).
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
    /// ([`crate::wire`]), passes the encoding as a borrowed `(ptr, len)`
    /// pair, and releases its own encoding after the call returns.
    Buffer {
        /// The `const uint8_t*` data slot.
        ptr: &'a AbiParam,
        /// The `size_t` length slot.
        len: &'a AbiParam,
    },
    /// A borrowed object pointer: the wrapper passes the wrapped object's
    /// native handle and retains ownership. When `nullable`, the IDL type is
    /// `Interface?` and null means none.
    Object {
        /// The single object-pointer slot.
        slot: &'a AbiParam,
        /// `true` for `Interface?`: null is a legal "none" argument.
        nullable: bool,
    },
}

impl ParamBinding {
    /// The passing contract for this parameter.
    ///
    /// # Panics
    ///
    /// Panics if the parameter's precomputed ABI slots disagree with its IR
    /// type's shape, which would be a bug in the model construction, not a
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
        if is_buffered(&self.ty) {
            let (ptr, len) = pair();
            return ArgPass::Buffer { ptr, len };
        }
        match &self.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => ArgPass::String { slot: single() },
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let (ptr, len) = pair();
                ArgPass::Bytes { ptr, len }
            }
            TypeRef::Interface(_) => ArgPass::Object {
                slot: single(),
                nullable: false,
            },
            // Only `Interface?` reaches here; every other optional is
            // buffered.
            TypeRef::Optional(_) => ArgPass::Object {
                slot: single(),
                nullable: true,
            },
            _ => ArgPass::Direct { slot: single() },
        }
    }
}

/// How a callable's result crosses back to the wrapper: the receiving
/// contract, including the decode step and the release obligation.
///
/// This is [`ReturnFree`] completed with the *decode* dimension: a bytes
/// return and a buffered return share a free obligation but differ in what
/// the wrapper does before freeing (copy versus decode). Only the sync and
/// async call shapes consult this; an iterator's result contract lives in
/// [`IteratorProtocol`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetPass {
    /// No return value.
    Void,
    /// By-value return (scalar, bool, C-style enum, handle): use directly,
    /// nothing to free.
    Direct,
    /// Owned `const char*`: copy into the native string, then release with
    /// `{runtime}_free_string`.
    String,
    /// Owned `(const uint8_t*, out_len)` raw bytes: copy, then release with
    /// `{runtime}_free_bytes`.
    Bytes,
    /// Owned `(const uint8_t*, out_len)` value buffer: decode via the wire
    /// format ([`crate::wire`]), then release with `{runtime}_free_bytes`.
    Buffer,
    /// An owned object reference the wrapper adopts into its disposal idiom.
    /// When `nullable`, the IDL type is `Interface?` and a null return means
    /// none.
    Object {
        /// The `{prefix}_{module}_{Name}_destroy` symbol the adopted
        /// reference eventually owes.
        destroy_symbol: String,
        /// `true` for `Interface?`: a null return is a legal "none" result.
        nullable: bool,
    },
}

/// The receiving contract for a value of type `ty` returned from a callable
/// declared inside `module` under `prefix`. `None` (a void return) is
/// [`RetPass::Void`].
///
/// # Panics
///
/// Panics on an iterator return, whose contract is [`IteratorProtocol`], not
/// a value-passing plan; backends dispatch on
/// [`CallShape`](crate::model::CallShape) before consulting this.
pub fn ret_pass(ty: Option<&TypeRef>, module: &str, prefix: &str) -> RetPass {
    let Some(ty) = ty else {
        return RetPass::Void;
    };
    if is_buffered(ty) {
        return RetPass::Buffer;
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => RetPass::String,
        TypeRef::Bytes | TypeRef::BorrowedBytes => RetPass::Bytes,
        TypeRef::Interface(name) => RetPass::Object {
            destroy_symbol: destroy_symbol(name, module, prefix),
            nullable: false,
        },
        // Only `Interface?` reaches here (every other optional is buffered).
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(name) = inner.as_ref() else {
                unreachable!("only optional interfaces escape buffering")
            };
            RetPass::Object {
                destroy_symbol: destroy_symbol(name, module, prefix),
                nullable: true,
            }
        }
        TypeRef::Iterator(_) => {
            panic!("iterator returns follow IteratorProtocol, not a RetPass")
        }
        _ => RetPass::Direct,
    }
}

/// The release call a consumer wrapper owes for one *element* it copied out
/// of an iterator `next` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElemFree {
    /// By-value element (scalar, bool, C-style enum, handle): nothing to free.
    None,
    /// A `const char*` element: release with `{runtime}_free_string`.
    String,
    /// A `ptr` + `len` element (bytes, or any buffered value the wrapper
    /// decodes): copy or decode, then `{runtime}_free_bytes(ptr, len)`.
    Bytes,
}

/// The release call a consumer wrapper owes after copying a *returned*
/// value into a native one.
///
/// This is the single statement of the ownership contract the producer runtime
/// implements (`weaveffi-abi`'s lowering helpers): strings via
/// `{runtime}_free_string`, byte and value buffers via `{runtime}_free_bytes`,
/// owned interface objects via their `_destroy` symbol. A backend renders
/// these as its disposal calls (or wraps the object and defers the release to
/// its finalizer idiom).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReturnFree {
    /// By-value return: nothing to free.
    None,
    /// `const char*`: copy, then `{runtime}_free_string(ptr)`.
    String,
    /// `const uint8_t* + out_len`: copy (bytes) or decode (a buffered value),
    /// then `{runtime}_free_bytes(ptr, len)`.
    Bytes,
    /// An owned interface object: the caller owns the reference and
    /// eventually calls `destroy_symbol`. Wrappers adopt the pointer into
    /// their disposal idiom (RAII, `__del__`, finalizers, `close()`), rather
    /// than freeing eagerly.
    OwnedObject {
        /// The `{prefix}_{module}_{Name}_destroy` symbol to call.
        destroy_symbol: String,
    },
}

/// The per-element release owed for one iterator element of type `ty`.
pub fn elem_free(ty: &TypeRef) -> ElemFree {
    if is_buffered(ty) {
        return ElemFree::Bytes;
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ElemFree::String,
        TypeRef::Bytes | TypeRef::BorrowedBytes => ElemFree::Bytes,
        _ => ElemFree::None,
    }
}

/// The release plan for a value of type `ty` *returned* from a callable
/// declared inside `module` under `prefix`. `None` (a void return) owes
/// nothing.
pub fn return_free(ty: Option<&TypeRef>, module: &str, prefix: &str) -> ReturnFree {
    let Some(ty) = ty else {
        return ReturnFree::None;
    };
    if is_buffered(ty) {
        return ReturnFree::Bytes;
    }
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ReturnFree::String,
        TypeRef::Bytes | TypeRef::BorrowedBytes => ReturnFree::Bytes,
        TypeRef::Interface(name) => ReturnFree::OwnedObject {
            destroy_symbol: destroy_symbol(name, module, prefix),
        },
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable owned object pointer, null meaning none.
        TypeRef::Optional(inner) => return_free(Some(inner), module, prefix),
        // The iterator handle's lifecycle is the iterator protocol's own
        // destroy symbol (see `IteratorProtocol`), not a buffer release.
        TypeRef::Iterator(_) => ReturnFree::None,
        _ => ReturnFree::None,
    }
}

/// The `{prefix}_{module}_{Name}_destroy` symbol for a (possibly
/// dot-qualified) interface name referenced from `current_module`.
fn destroy_symbol(name: &str, current_module: &str, prefix: &str) -> String {
    let (module, name) = split_qualified(name, current_module);
    format!("{prefix}_{module}_{name}_destroy")
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
///    owns; after copying (or decoding) it, the wrapper owes
///    [`elem_free`](Self::elem_free).
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
    /// The release owed for each element copied out of a `next` slot.
    pub elem_free: ElemFree,
    /// How `out_err` reports from the launcher and each `next` call are
    /// interpreted.
    pub error: ErrorStrategy,
}

impl IteratorBinding {
    /// Build the full pull contract for this iterator.
    pub fn protocol<'a>(&'a self, f: &FnBinding) -> IteratorProtocol<'a> {
        IteratorProtocol {
            binding: self,
            elem_free: elem_free(&self.elem),
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
///    interface-object results transfer ownership of the object (the
///    wrapper adopts the pointer). This is what lets runtimes that defer
///    callback bodies past the native return (Dart's
///    `NativeCallable.listener`, for example) decode safely; a wrapper that
///    processes results inline still copies or decodes first and then frees.
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
    /// The release owed for an *owned interface* result adopted by the
    /// callback; [`ReturnFree::None`] for results copied or decoded and
    /// then released through the runtime free symbols.
    pub result_adopt: ReturnFree,
    /// How the callback's `err` slot is interpreted.
    pub error: ErrorStrategy,
}

impl AsyncBinding {
    /// Build the full completion contract for this async function, resolving
    /// the result-adoption plan against the declaring `module` and `prefix`.
    ///
    /// A direct or optional interface result (where an optional's null slot
    /// simply means none) is adopted by the callback; every other result
    /// shape is copied or decoded, then freed through the runtime symbols.
    pub fn protocol<'a>(&'a self, f: &FnBinding, module: &str, prefix: &str) -> AsyncProtocol<'a> {
        fn adoptable(ty: &TypeRef) -> Option<&TypeRef> {
            match ty {
                TypeRef::Interface(_) => Some(ty),
                TypeRef::Optional(inner) => adoptable(inner),
                _ => None,
            }
        }
        let result_adopt = match f.ret.as_ref().and_then(|ty| adoptable(ty)) {
            Some(ty) => return_free(Some(ty), module, prefix),
            None => ReturnFree::None,
        };
        AsyncProtocol {
            binding: self,
            cancellable: f.cancellable,
            result_adopt,
            error: f.error_strategy(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_and_bytes_have_runtime_frees() {
        assert_eq!(
            return_free(Some(&TypeRef::StringUtf8), "m", "weaveffi"),
            ReturnFree::String
        );
        assert_eq!(
            return_free(Some(&TypeRef::Bytes), "m", "weaveffi"),
            ReturnFree::Bytes
        );
        assert_eq!(return_free(None, "m", "weaveffi"), ReturnFree::None);
    }

    #[test]
    fn buffered_returns_are_freed_as_bytes() {
        for ty in [
            TypeRef::Record("Contact".into()),
            TypeRef::RichEnum("Shape".into()),
            TypeRef::List(Box::new(TypeRef::StringUtf8)),
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            TypeRef::Optional(Box::new(TypeRef::I64)),
            TypeRef::Optional(Box::new(TypeRef::Record("Contact".into()))),
        ] {
            assert_eq!(
                return_free(Some(&ty), "m", "weaveffi"),
                ReturnFree::Bytes,
                "{ty:?}"
            );
        }
    }

    #[test]
    fn interface_returns_are_adopted_with_destroy_symbols() {
        assert_eq!(
            return_free(
                Some(&TypeRef::Interface("kv.Store".into())),
                "kv_stats",
                "weaveffi"
            ),
            ReturnFree::OwnedObject {
                destroy_symbol: "weaveffi_kv_Store_destroy".into()
            }
        );
        // A nullable interface shares the plan; null means none.
        assert_eq!(
            return_free(
                Some(&TypeRef::Optional(Box::new(TypeRef::Interface(
                    "Store".into()
                )))),
                "kv",
                "weaveffi"
            ),
            ReturnFree::OwnedObject {
                destroy_symbol: "weaveffi_kv_Store_destroy".into()
            }
        );
    }

    #[test]
    fn arg_pass_classifies_every_family() {
        use crate::abi::lower_param;
        let pb = |name: &str, ty: TypeRef| ParamBinding {
            abi: lower_param(name, &ty, "m", false),
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
        };

        assert!(matches!(
            pb("x", TypeRef::I32).arg_pass(),
            ArgPass::Direct { slot } if slot.name == "x"
        ));
        assert!(matches!(
            pb("h", TypeRef::Handle).arg_pass(),
            ArgPass::Direct { .. }
        ));
        assert!(matches!(
            pb("s", TypeRef::StringUtf8).arg_pass(),
            ArgPass::String { slot } if slot.name == "s"
        ));
        assert!(matches!(
            pb("data", TypeRef::Bytes).arg_pass(),
            ArgPass::Bytes { ptr, len } if ptr.name == "data_ptr" && len.name == "data_len"
        ));
        assert!(matches!(
            pb("c", TypeRef::Record("Contact".into())).arg_pass(),
            ArgPass::Buffer { ptr, len } if ptr.name == "c_ptr" && len.name == "c_len"
        ));
        assert!(matches!(
            pb("o", TypeRef::Optional(Box::new(TypeRef::I32))).arg_pass(),
            ArgPass::Buffer { .. }
        ));
        assert!(matches!(
            pb("store", TypeRef::Interface("Store".into())).arg_pass(),
            ArgPass::Object {
                nullable: false,
                ..
            }
        ));
        assert!(matches!(
            pb(
                "store",
                TypeRef::Optional(Box::new(TypeRef::Interface("Store".into())))
            )
            .arg_pass(),
            ArgPass::Object { nullable: true, .. }
        ));
    }

    #[test]
    fn ret_pass_distinguishes_copy_decode_and_adopt() {
        assert_eq!(ret_pass(None, "m", "weaveffi"), RetPass::Void);
        assert_eq!(
            ret_pass(Some(&TypeRef::I64), "m", "weaveffi"),
            RetPass::Direct
        );
        assert_eq!(
            ret_pass(Some(&TypeRef::StringUtf8), "m", "weaveffi"),
            RetPass::String
        );
        assert_eq!(
            ret_pass(Some(&TypeRef::Bytes), "m", "weaveffi"),
            RetPass::Bytes
        );
        assert_eq!(
            ret_pass(Some(&TypeRef::Record("Contact".into())), "m", "weaveffi"),
            RetPass::Buffer
        );
        assert_eq!(
            ret_pass(
                Some(&TypeRef::Interface("kv.Store".into())),
                "m",
                "weaveffi"
            ),
            RetPass::Object {
                destroy_symbol: "weaveffi_kv_Store_destroy".into(),
                nullable: false,
            }
        );
        assert_eq!(
            ret_pass(
                Some(&TypeRef::Optional(Box::new(TypeRef::Interface(
                    "Store".into()
                )))),
                "kv",
                "weaveffi"
            ),
            RetPass::Object {
                destroy_symbol: "weaveffi_kv_Store_destroy".into(),
                nullable: true,
            }
        );
    }

    #[test]
    fn iterator_elements_split_string_bytes_and_none() {
        assert_eq!(elem_free(&TypeRef::I32), ElemFree::None);
        assert_eq!(elem_free(&TypeRef::StringUtf8), ElemFree::String);
        assert_eq!(elem_free(&TypeRef::Bytes), ElemFree::Bytes);
        assert_eq!(elem_free(&TypeRef::Record("Entry".into())), ElemFree::Bytes);
        assert_eq!(
            elem_free(&TypeRef::Optional(Box::new(TypeRef::I32))),
            ElemFree::Bytes
        );
    }
}
