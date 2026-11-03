//! The **model**: the one validated, fully lowered view of an API that every
//! language backend consumes.
//!
//! [`validate`](crate::validate::validate) checks an [`Api`](crate::ir::Api)
//! and builds the [`Model`] exactly once per run. A library is one
//! namespace, so the model is a flat list of declarations of each kind, and
//! it owns everything a generator reads:
//!
//! * the library's [`Identity`] (C symbol prefix, library name, package
//!   metadata) and the schema version;
//! * every declaration with its types **resolved** ([`Ty`], [`ParamTy`],
//!   [`RetTy`]; no unresolved names) and its **C symbols** precomputed, so
//!   every backend agrees by construction;
//! * every callable's lowered [`AbiFn`] and [`Shape`] (sync, async, or
//!   iterator), and every callback method's vtable entry;
//! * the [`TypeIndex`], which maps each type name to its declaration behind
//!   typed lookups such as [`Model::interface`] and [`Model::error_domain`].
//!
//! In ABI revision 6 every value crosses as one C value whose type is a
//! pure function of the value's type ([`CType::of`]), so a backend reads the
//! idiomatic shape from the resolved types and marshals each value by its
//! [`Family`](crate::ty::Family). Generators never see the
//! [`Api`](crate::ir::Api).

mod build;
mod symbols;
#[cfg(all(test, feature = "idl"))]
mod tests;

pub(crate) use build::{build, index};
pub use symbols::{Symbol, SymbolOwner, VALUE_CODECS};

use crate::abi::{AbiFn, CType};
use crate::pkg::Identity;
use crate::ty::{ParamTy, RetTy, Ty, TypeDecl, TypeIndex, TypeKind};

/// The C ABI revision this model lowers to. Producers export it from
/// `{prefix}_abi_version()`, and every generated consumer checks it at load.
pub const ABI_VERSION: u32 = 6;

/// The runtime symbols every producer exports (and every C header
/// declares), without the `{prefix}_` that begins each one.
pub const RUNTIME_SYMBOLS: &[&str] = &[
    "abi_version",
    "error",
    "error_set",
    "error_set_payload",
    "error_clear",
    "error_free",
    "run",
    "alloc",
    "run_free",
    "cancel_token",
    "cancel_token_create",
    "cancel_token_cancel",
    "cancel_token_is_cancelled",
    "cancel_token_destroy",
    "debug_live",
];

/// Identifier families the runtime surface and the generated C headers
/// reserve, without the `{prefix}_` that begins each: a declaration whose C
/// symbol falls in one is a collision. `contract_` holds the per-declaration
/// contract functions, and the rest the fixed value-buffer helpers of
/// `{library}_buffer.h`. (The optional-scalar structs and the value-buffer
/// header's composite codecs are exact names in [`Model::c_symbols`].)
pub const RESERVED_SYMBOL_FAMILIES: &[&str] =
    &["contract_", "reader_", "writer_", "str_", "bytes_"];

/// How a reported error is interpreted.
///
/// Every synchronous C entry point carries a trailing `out_err`, and every
/// async completion an `err`, whatever the strategy. What differs is the
/// *meaning* of a non-zero code, and every backend must agree on it. A
/// negative code (`-1` generic, `-2` panic, `-3` marshalling failure, `-4`
/// callback failure, `-5` cancelled) is a runtime code under every strategy.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ErrorStrategy {
    /// The callable doesn't throw: a non-zero code is a producer bug or a
    /// runtime trap, which the wrapper surfaces through the target's
    /// programming-error idiom, never as a typed error. A callback method
    /// with this strategy reports any failure as `-4`.
    Trap,
    /// The callable throws the named error domain, which
    /// [`Model::error_domain`] resolves. A positive code is one of the
    /// domain's codes (or, from a newer library, a code the binding doesn't
    /// know, mapped to the domain's base error type).
    Domain(String),
    /// The callable throws an untyped error: code `-1` with a message.
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

/// What kind of callable a [`FnBinding`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallableKind {
    /// A free function.
    Function,
    /// An interface constructor: its return is the interface.
    Constructor,
    /// An interface instance method: its first C slot is `self`.
    Method,
    /// An interface static function.
    Static,
}

impl CallableKind {
    /// The word the contract signature starts with (`function`,
    /// `constructor`, `method`, or `static`).
    #[must_use]
    pub fn keyword(self) -> &'static str {
        match self {
            CallableKind::Function => "function",
            CallableKind::Constructor => "constructor",
            CallableKind::Method => "method",
            CallableKind::Static => "static",
        }
    }
}

/// How a callable completes.
// A model holds one of these per callable and never moves them in bulk.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// A blocking call: [`FnBinding::abi`] returns the value (or `void`).
    Sync,
    /// An async launch: [`FnBinding::abi`] returns `void`, and the result
    /// arrives once in the completion callback.
    Async(AsyncBinding),
    /// An iterator launch: [`FnBinding::abi`] returns the iterator handle.
    Iterator(IteratorBinding),
}

/// The completion side of an `async` callable.
///
/// The launcher is [`FnBinding::abi`]: `self` (for a method), the inputs,
/// `{p}_cancel_token* cancel_token` when cancellable, then the completion
/// `callback` and `void* context`. It has no `out_err`; every failure
/// arrives through the completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncBinding {
    /// The completion function-pointer typedef, `{symbol}_callback`.
    pub callback_type: String,
    /// The completion's signature: `void* context`, `{p}_error* err`, then
    /// `result` unless the callable returns nothing. Its `symbol` is
    /// [`callback_type`](Self::callback_type).
    pub callback: AbiFn,
    /// `true` when the launcher takes a cancel token.
    pub cancellable: bool,
}

/// The surface of an `iter<T>`-returning callable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IteratorBinding {
    /// The element type `T`.
    pub elem: Ty,
    /// The opaque iterator type, `{p}_{Owner_}{Pascal}Iterator`.
    pub iter_tag: String,
    /// `int32_t {iter_tag}_next({iter_tag}* iter, T* out_item,
    /// {p}_error* out_err)`: `1` with an element, `0` when done.
    pub next: AbiFn,
    /// `void {iter_tag}_destroy({iter_tag}* iter)`.
    pub destroy_symbol: String,
}

/// One parameter of a callable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamBinding {
    /// The parameter name as written.
    pub name: String,
    /// The resolved type.
    pub ty: ParamTy,
    /// Documentation.
    pub doc: Option<String>,
}

/// A callable (free function or interface member), fully lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnBinding {
    /// The callable's name as written.
    pub name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// The deprecation message, if deprecated.
    pub deprecated: Option<String>,
    /// What kind of callable this is.
    pub kind: CallableKind,
    /// The interface a member belongs to, or `None` for a free function.
    pub owner: Option<String>,
    /// The declaration path contract entries and diagnostics name (`open`,
    /// `Store.get`).
    pub path: String,
    /// The parameters (a method's `self` is not one of them).
    pub params: Vec<ParamBinding>,
    /// The resolved return (`None` = void). A constructor's is its
    /// interface.
    pub ret: Option<RetTy>,
    /// How a reported error is interpreted.
    pub error: ErrorStrategy,
    /// The symbol to call, with its full slot list: the sync entry point,
    /// the async launcher, or the iterator launcher.
    pub abi: AbiFn,
    /// How the call completes.
    pub shape: Shape,
}

impl FnBinding {
    /// `true` for an instance method.
    #[must_use]
    pub fn has_self(&self) -> bool {
        self.kind == CallableKind::Method
    }

    /// `true` when the callable is `async`.
    #[must_use]
    pub fn is_async(&self) -> bool {
        matches!(self.shape, Shape::Async(_))
    }

    /// The completion side of an async callable.
    #[must_use]
    pub fn async_binding(&self) -> Option<&AsyncBinding> {
        match &self.shape {
            Shape::Async(a) => Some(a),
            _ => None,
        }
    }

    /// The iterator surface of an iterator-returning callable.
    #[must_use]
    pub fn iterator(&self) -> Option<&IteratorBinding> {
        match &self.shape {
            Shape::Iterator(it) => Some(it),
            _ => None,
        }
    }

    /// `true` when the callable is async and takes a cancel token.
    #[must_use]
    pub fn cancellable(&self) -> bool {
        self.async_binding().is_some_and(|a| a.cancellable)
    }

    /// The value the callable produces: the sync return or async result,
    /// or `None` for `void` and for an iterator (whose elements
    /// [`IteratorBinding::elem`] types).
    #[must_use]
    pub fn ret_value(&self) -> Option<&Ty> {
        self.ret.as_ref().and_then(RetTy::value)
    }
}

/// A field of a record, a rich-enum variant, or an error code's payload. A
/// field's declaration order is its wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldBinding {
    /// The field name as written.
    pub name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// The resolved type.
    pub ty: Ty,
}

/// A record: a value type generators emit as a native data class plus its
/// value-buffer codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordBinding {
    /// The record name as written.
    pub name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// The deprecation message, if deprecated.
    pub deprecated: Option<String>,
    /// `{p}_{name}`: the struct the C value-buffer header declares, and the
    /// stem of its codecs.
    pub c_tag: String,
    /// The fields, in declaration (and wire) order.
    pub fields: Vec<FieldBinding>,
}

/// An enum, fully lowered.
///
/// A C-style enum crosses as an `int32_t`; a rich enum (at least one variant
/// with fields) crosses as a value buffer, an `i32` tag followed by the
/// variant's fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumBinding {
    /// The enum name as written.
    pub name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// The deprecation message, if deprecated.
    pub deprecated: Option<String>,
    /// `{p}_{name}`.
    pub c_tag: String,
    /// The variants, in declaration order.
    pub variants: Vec<EnumVariantBinding>,
}

impl EnumBinding {
    /// `true` for a rich (sum-type) enum.
    #[must_use]
    pub fn is_rich(&self) -> bool {
        self.variants.iter().any(|v| !v.fields.is_empty())
    }
}

/// One enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariantBinding {
    /// The variant name as written.
    pub name: String,
    /// The discriminant (a rich enum's buffer tag).
    pub value: i32,
    /// Documentation.
    pub doc: Option<String>,
    /// `{enum_c_tag}_{variant}`, the C constant.
    pub c_const: String,
    /// The variant's fields, in wire order; empty for a unit variant.
    pub fields: Vec<FieldBinding>,
}

/// An interface (reference-counted object type), fully lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceBinding {
    /// The interface name as written.
    pub name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// The deprecation message, if deprecated.
    pub deprecated: Option<String>,
    /// `{p}_{name}`, the opaque type.
    pub c_tag: String,
    /// Constructors.
    pub constructors: Vec<FnBinding>,
    /// Instance methods.
    pub methods: Vec<FnBinding>,
    /// Static functions.
    pub statics: Vec<FnBinding>,
    /// `{c_tag}* {c_tag}_clone(const {c_tag}* self)`: another strong
    /// reference to the same object.
    pub clone_symbol: String,
    /// `void {c_tag}_destroy({c_tag}* self)`: releases one strong reference.
    pub destroy_symbol: String,
}

impl InterfaceBinding {
    /// Every member: constructors, then methods, then statics.
    pub fn members(&self) -> impl Iterator<Item = &FnBinding> {
        self.constructors
            .iter()
            .chain(&self.methods)
            .chain(&self.statics)
    }
}

/// One method of a callback interface, lowered to its vtable entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackMethodBinding {
    /// The method name as written; also its vtable field.
    pub name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// The deprecation message, if deprecated.
    pub deprecated: Option<String>,
    /// The method's zero-based position in the vtable after the header.
    pub slot: usize,
    /// The parameters. An object parameter moves one strong reference to
    /// the consumer.
    pub params: Vec<FieldBinding>,
    /// The resolved return (`None` = void). An object or run the consumer
    /// returns moves to the producer.
    pub ret: Option<Ty>,
    /// Which failures the method may report.
    pub error: ErrorStrategy,
    /// The vtable entry: `void* ctx`, the parameters, then `{p}_error*
    /// out_err`, returning the value.
    pub abi: AbiFn,
}

/// A callback interface, fully lowered.
///
/// A parameter of this type is the `{c_tag}` struct, `{ void* ctx; const
/// {vtable_tag}* vtable; }`, passed by value. The vtable starts with a fixed
/// header (`uint32_t size`, `uint32_t flags`, `void (*free)(void* ctx)`)
/// followed by one function pointer per method in declaration order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackBinding {
    /// The callback interface name as written.
    pub name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// The deprecation message, if deprecated.
    pub deprecated: Option<String>,
    /// `{p}_{name}`, the argument struct.
    pub c_tag: String,
    /// `{c_tag}_vtable`.
    pub vtable_tag: String,
    /// The methods, in vtable order. Never empty.
    pub methods: Vec<CallbackMethodBinding>,
}

/// One error code of an error domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCodeBinding {
    /// The code name as written.
    pub name: String,
    /// The positive value carried in `{p}_error.code`.
    pub value: i32,
    /// The default message.
    pub message: String,
    /// Documentation.
    pub doc: Option<String>,
    /// `{domain_c_tag}_{name}`, the C constant.
    pub c_const: String,
    /// The payload fields, in wire order; empty when the payload is empty.
    pub fields: Vec<FieldBinding>,
}

impl ErrorCodeBinding {
    /// `{c_const}_payload`, the struct the C value-buffer header declares
    /// for the code's fields (only when there are any).
    #[must_use]
    pub fn payload_tag(&self) -> String {
        format!("{}_payload", self.c_const)
    }
}

/// An error domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBinding {
    /// The domain name as written (for example `KvError`).
    pub name: String,
    /// The domain's error type name with exactly one `Error` suffix, from
    /// [`crate::errors::type_name`].
    pub type_name: String,
    /// Documentation.
    pub doc: Option<String>,
    /// `{p}_{name}`, the C type (`typedef int32_t`) of the code constants.
    pub c_tag: String,
    /// The codes, in declaration order.
    pub codes: Vec<ErrorCodeBinding>,
}

/// The whole API, validated and lowered for code generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// The library's identity: its name, C symbol prefix, native library
    /// name, and package metadata.
    pub identity: Identity,
    /// The IR schema version of the source.
    pub version: String,
    /// The library's documentation.
    pub doc: Option<String>,
    /// Error domains, in declaration order.
    pub errors: Vec<ErrorBinding>,
    /// Enums, in declaration order.
    pub enums: Vec<EnumBinding>,
    /// Records, in declaration order.
    pub records: Vec<RecordBinding>,
    /// Interfaces, in declaration order.
    pub interfaces: Vec<InterfaceBinding>,
    /// Callback interfaces, in declaration order.
    pub callbacks: Vec<CallbackBinding>,
    /// Free functions, in declaration order.
    pub functions: Vec<FnBinding>,
    /// Every type name, mapped to its declaration.
    pub types: TypeIndex,
}

impl Model {
    /// The C symbol prefix every emitted name starts with.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.identity.prefix
    }

    /// Every callable: free functions, then each interface's constructors,
    /// methods, and statics.
    pub fn callables(&self) -> impl Iterator<Item = &FnBinding> {
        self.functions
            .iter()
            .chain(self.interfaces.iter().flat_map(InterfaceBinding::members))
    }

    /// The declaration of the user type `name`, which must be one of
    /// `kinds`.
    fn decl(&self, name: &str, kinds: &[TypeKind]) -> &TypeDecl {
        match self.types.get(name) {
            Some(d) if kinds.contains(&d.kind) => d,
            Some(d) => panic!("type '{name}' is a {:?}, not one of {kinds:?}", d.kind),
            None => panic!("type '{name}' is not declared"),
        }
    }

    /// The interface named `name`.
    ///
    /// # Panics
    ///
    /// Panics when no interface has that name, which validation rules out
    /// for every name the model carries.
    #[must_use]
    pub fn interface(&self, name: &str) -> &InterfaceBinding {
        &self.interfaces[self.decl(name, &[TypeKind::Interface]).index]
    }

    /// The callback interface named `name`.
    ///
    /// # Panics
    ///
    /// Panics when no callback interface has that name.
    #[must_use]
    pub fn callback(&self, name: &str) -> &CallbackBinding {
        &self.callbacks[self.decl(name, &[TypeKind::CallbackInterface]).index]
    }

    /// The enum (C-style or rich) named `name`.
    ///
    /// # Panics
    ///
    /// Panics when no enum has that name.
    #[must_use]
    pub fn enumeration(&self, name: &str) -> &EnumBinding {
        &self.enums[self.decl(name, &[TypeKind::Enum, TypeKind::RichEnum]).index]
    }

    /// The record named `name`.
    ///
    /// # Panics
    ///
    /// Panics when no record has that name.
    #[must_use]
    pub fn record(&self, name: &str) -> &RecordBinding {
        &self.records[self.decl(name, &[TypeKind::Record]).index]
    }

    /// The error domain named `name`.
    ///
    /// # Panics
    ///
    /// Panics when no error domain has that name.
    #[must_use]
    pub fn error_domain(&self, name: &str) -> &ErrorBinding {
        &self.errors[self.decl(name, &[TypeKind::ErrorDomain]).index]
    }

    /// The `{p}_{name}` C tag of the user type `name` (its opaque type,
    /// enum type, buffer-header struct, or callback argument struct).
    #[must_use]
    pub fn c_tag(&self, name: &str) -> String {
        format!("{}_{name}", self.prefix())
    }

    /// The C type of a value of type `ty` at `position`.
    #[must_use]
    pub fn ctype(&self, ty: &Ty, position: crate::abi::Position) -> CType {
        CType::of(ty, position)
    }

    /// Every C identifier the library's ABI declares, with the declaration
    /// that owns it. Validation rejects an API in which two share one.
    #[must_use]
    pub fn c_symbols(&self) -> Vec<Symbol> {
        symbols::collect(self)
    }

    /// `true` when any callable is `async`.
    #[must_use]
    pub fn has_async(&self) -> bool {
        self.callables().any(FnBinding::is_async)
    }

    /// `true` when any callable returns an iterator.
    #[must_use]
    pub fn has_iterators(&self) -> bool {
        self.callables().any(|f| f.iterator().is_some())
    }

    /// `true` when any callable takes a cancel token.
    #[must_use]
    pub fn has_cancellable(&self) -> bool {
        self.callables().any(FnBinding::cancellable)
    }

    /// Every distinct optional, list, and map type that crosses inside a
    /// value buffer anywhere in the API (the *composites*, which the C
    /// value-buffer header and several targets give a codec each),
    /// innermost first (a shape after the shapes it contains), each once.
    ///
    /// The order is deterministic: record fields, rich-enum variant fields,
    /// and error payload fields in declaration order, then every buffered
    /// value at a call boundary (parameters, returns, async results, iterator
    /// elements, and callback method parameters and returns).
    #[must_use]
    pub fn composites(&self) -> Vec<Ty> {
        fn collect(ty: &Ty, out: &mut Vec<Ty>, seen: &mut std::collections::HashSet<Ty>) {
            match ty {
                Ty::Optional(inner) | Ty::List(inner) => collect(inner, out, seen),
                Ty::Map(k, v) => {
                    collect(k, out, seen);
                    collect(v, out, seen);
                }
                _ => return,
            }
            if seen.insert(ty.clone()) {
                out.push(ty.clone());
            }
        }
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let fields = self
            .records
            .iter()
            .flat_map(|s| &s.fields)
            .chain(
                self.enums
                    .iter()
                    .flat_map(|e| &e.variants)
                    .flat_map(|v| &v.fields),
            )
            .chain(
                self.errors
                    .iter()
                    .flat_map(|e| &e.codes)
                    .flat_map(|c| &c.fields),
            );
        for f in fields {
            collect(&f.ty, &mut out, &mut seen);
        }
        let mut boundary = |ty: &Ty| {
            if ty.is_buffered() {
                collect(ty, &mut out, &mut seen);
            }
        };
        for f in self.callables() {
            f.params
                .iter()
                .filter_map(|p| p.ty.value())
                .for_each(&mut boundary);
            if let Some(ret) = &f.ret {
                boundary(ret.elem());
            }
        }
        for cb in &self.callbacks {
            for meth in &cb.methods {
                meth.params.iter().for_each(|p| boundary(&p.ty));
                meth.ret.iter().for_each(&mut boundary);
            }
        }
        out
    }

    /// `true` when any value anywhere in the API crosses as a value buffer
    /// (records, rich enums, error payloads, and the optionals, lists, and
    /// maps of the Run family's Buffer kind).
    #[must_use]
    pub fn has_buffers(&self) -> bool {
        let buffered = |ty: &Ty| ty.any(&Ty::is_buffered);
        let callable = |f: &FnBinding| {
            f.params.iter().any(|p| p.ty.value().is_some_and(buffered))
                || f.ret.as_ref().is_some_and(|r| buffered(r.elem()))
        };
        let method = |m: &CallbackMethodBinding| {
            m.params.iter().any(|p| buffered(&p.ty)) || m.ret.as_ref().is_some_and(buffered)
        };
        !self.records.is_empty()
            || self.enums.iter().any(EnumBinding::is_rich)
            || self
                .errors
                .iter()
                .any(|e| e.codes.iter().any(|c| !c.fields.is_empty()))
            || self.callables().any(callable)
            || self.callbacks.iter().any(|c| c.methods.iter().any(method))
    }
}
