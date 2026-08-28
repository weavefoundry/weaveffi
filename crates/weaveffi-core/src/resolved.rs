//! The resolved IR: a proof-carrying wrapper around a validated [`Api`].
//!
//! WeaveFFI's pipeline has two distinct representations of an API that used
//! to share one mutable type:
//!
//! * the **IDL document**: the [`Api`] tree exactly as parsed from
//!   YAML/JSON/TOML (or extracted from annotated Rust), in which every
//!   user-type reference is still an unresolved [`TypeRef::Named`]; and
//! * the **resolved IR**: the same tree after [`validate_api`] has checked
//!   every rule and rewritten each `Named` reference into its resolved kind
//!   ([`TypeRef::Record`], [`TypeRef::RichEnum`], [`TypeRef::Enum`], or
//!   [`TypeRef::Interface`]), qualifying cross-module references.
//!
//! [`ResolvedApi`] is the second representation, made unforgeable: the only
//! checked way to obtain one is [`validate_api`], and the wrapper exposes no
//! mutable access, so a generator holding a `&ResolvedApi` can rely on the
//! post-resolution invariants (no `Named` reference remains anywhere in the
//! tree) instead of documenting them as a convention.
//!
//! Everything downstream of validation consumes this type: the
//! [`BindingModel`](crate::model::BindingModel), the
//! [`Generator`](crate::codegen::Generator) and
//! [`LanguageBackend`](crate::backend::LanguageBackend) traits, and the
//! [`Orchestrator`](crate::codegen::Orchestrator). Code that manipulates the
//! *document* (parsing, `weaveffi format`, `weaveffi extract`) keeps working
//! with the plain [`Api`].
//!
//! [`validate_api`]: crate::validate::validate_api
//! [`TypeRef::Named`]: weaveffi_ir::ir::TypeRef::Named
//! [`TypeRef::Record`]: weaveffi_ir::ir::TypeRef::Record
//! [`TypeRef::RichEnum`]: weaveffi_ir::ir::TypeRef::RichEnum
//! [`TypeRef::Enum`]: weaveffi_ir::ir::TypeRef::Enum
//! [`TypeRef::Interface`]: weaveffi_ir::ir::TypeRef::Interface

use weaveffi_ir::ir::Api;

/// A validated API whose type references have all been resolved.
///
/// This is the input every generator consumes. It cannot be constructed
/// from an unchecked document: obtain one through
/// [`validate_api`](crate::validate::validate_api), or through
/// [`assume_resolved`](Self::assume_resolved) when resolution has provably
/// happened by other means (the producer macro's expansion path, or tests
/// that build already-resolved trees by hand).
///
/// `ResolvedApi` dereferences to [`Api`] for read access; there is no
/// mutable access, so the resolution invariants cannot be broken after
/// construction.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedApi {
    api: Api,
}

impl ResolvedApi {
    /// Wrap an [`Api`] the caller asserts is already validated and resolved.
    ///
    /// This is the escape hatch for the two construction sites that cannot
    /// route through [`validate_api`](crate::validate::validate_api): the
    /// `#[weaveffi::module]` proc-macro (which resolves the single module it
    /// expands) and tests that build pre-resolved trees directly. The caller
    /// asserts that no [`TypeRef::Named`](weaveffi_ir::ir::TypeRef::Named)
    /// reference remains anywhere in `api`; downstream lowering treats a
    /// surviving one as a bug and panics on it.
    pub fn assume_resolved(api: Api) -> Self {
        Self { api }
    }

    /// Borrow the underlying resolved [`Api`] tree.
    pub fn api(&self) -> &Api {
        &self.api
    }
}

impl std::ops::Deref for ResolvedApi {
    type Target = Api;

    fn deref(&self) -> &Api {
        &self.api
    }
}

impl AsRef<Api> for ResolvedApi {
    fn as_ref(&self) -> &Api {
        &self.api
    }
}
