//! The **marshalling plan**: the language-neutral calling contracts every
//! backend renders, stated once.
//!
//! The [`crate::model`] layer answers *which symbols exist and what their C
//! signatures are*. This module answers the questions one level up, the ones
//! the eleven generators used to answer independently (and inconsistently):
//!
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
//!   including the rule that result buffers are borrowed for the callback's
//!   duration and must be copied before it returns.
//!
//! A backend that renders these plans in its own syntax cannot drift from the
//! others on semantics; only the spelling differs.

use weaveffi_ir::ir::TypeRef;

use crate::abi::lower::{is_buffered, split_qualified};
use crate::model::{AsyncBinding, FnBinding, IteratorBinding};

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
/// 2. **Borrowed results.** Result buffers passed to the callback (strings,
///    bytes, and buffered values) are owned by the producer and valid **only
///    for the callback's duration**; the wrapper must deep-copy or decode
///    them before the callback returns and must not free them.
///    Interface-object results are the exception: the callback receives
///    ownership and adopts the pointer.
/// 3. **Foreign-thread delivery.** The callback runs on a producer thread,
///    so the wrapper must hop back to its native scheduler before touching
///    consumer state (`call_soon_threadsafe`, a threadsafe function, a
///    dispatched continuation) rather than resolving inline where the
///    target's runtime forbids it.
///
/// The callback's `err` slot follows the owning function's [`ErrorStrategy`].
/// The error struct itself is producer-owned and borrowed for the callback's
/// duration: the wrapper copies the code, message, and payload inside the
/// callback and the producer releases them afterward. A wrapper may also call
/// `error_clear` itself; the clear is idempotent (it nulls the owned
/// pointers), so the producer's own release stays safe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncProtocol<'a> {
    /// The lowered async surface: launcher and callback typedef.
    pub binding: &'a AsyncBinding,
    /// Whether the launcher carries a `cancel_token` slot before
    /// `callback`/`context`.
    pub cancellable: bool,
    /// The release owed for an *owned interface* result adopted by the
    /// callback; [`ReturnFree::None`] for borrowed (copy-only) results.
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
    /// shape is borrowed and copied or decoded.
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
