//! The **model**: the one validated, fully lowered view of an API that every
//! language backend consumes.
//!
//! [`validate`](crate::validate::validate) checks an [`Api`] and builds the
//! [`Model`] exactly once per run. The model owns everything a generator
//! reads:
//!
//! * the library's [`Identity`] (C symbol prefix, library name, package
//!   metadata) and the schema version;
//! * a flat list of [`ModuleBinding`]s in which every **type** is a resolved
//!   [`Ty`] (no unresolved names), so a backend dispatches on [`Ty::family`]
//!   and [`Ty::wire`] instead of re-deriving what a name means;
//! * every emitted **C symbol name**, precomputed once, so all backends agree
//!   by construction;
//! * every function, interface member, and callback-interface method paired
//!   with its lowered [`AbiFn`] signature (built from [`crate::abi`]), so no
//!   backend re-derives parameter arity, ordering, or `out_*`/`out_err`
//!   placement; and
//! * the [`TypeIndex`], which maps each (global) type name to its
//!   declaration and owning module, behind typed lookups such as
//!   [`Model::interface`] and [`Model::owner`].
//!
//! A backend reads the *idiomatic* shape from the retained [`Ty`]s
//! (`param.ty`, `field.ty`, ...) and the *native* shape from the [`AbiFn`]s,
//! then writes only the marshalling that bridges the two (see
//! [`crate::plan`]) in its own idioms. Generators never see the [`Api`].

mod build;

pub(crate) use build::{build_indexed, index};

use crate::abi::{AbiParam, CType};
use crate::ir::{Api, TypeRef};
use crate::pkg::Identity;
use crate::ty::{Ty, TypeDecl, TypeIndex, TypeKind};

/// The runtime symbols every producer exports (and every C header declares),
/// without the `{prefix}_` that begins each one. See the C ABI contract for
/// their signatures.
pub const RUNTIME_SYMBOLS: &[&str] = &[
    "str",
    "bytes",
    "reader",
    "writer",
    "abi_version",
    "error",
    "error_set",
    "error_set_payload",
    "error_clear",
    "error_free",
    "free_bytes",
    "cancel_token",
    "cancel_token_create",
    "cancel_token_cancel",
    "cancel_token_is_cancelled",
    "cancel_token_destroy",
    "alloc",
    "debug_live",
    "contract_entry",
];

/// Identifier families the generated C value-buffer helper header
/// (`{library}_buffer.h`) declares, without the `{prefix}_` that begins each:
/// a user declaration whose C symbol falls in one of these families would
/// collide with a helper.
pub const RESERVED_SYMBOL_FAMILIES: &[&str] = &[
    "str_", "bytes_", "reader_", "writer_", "list_", "map_", "opt_",
];

/// The codec functions the C value-buffer helper header declares for each
/// record, rich enum, and error payload struct `T`: `T_write`, `T_read`,
/// `T_decode`, and `T_free`.
pub const VALUE_CODECS: &[&str] = &["write", "read", "decode", "free"];

/// The C ABI revision this model lowers to. Producers export it from
/// `{prefix}_abi_version()` and every generated consumer checks it at load.
pub const ABI_VERSION: u32 = 4;

/// The symbol of a top-level module's contract table function,
/// `{prefix}_{module}_contract` (see [`crate::contract`]).
#[must_use]
pub fn contract_symbol(prefix: &str, module: &str) -> String {
    format!("{prefix}_{module}_contract")
}

/// The C header's `static inline` checker of a top-level module's contract,
/// `{prefix}_{module}_contract_check`.
#[must_use]
pub fn contract_check_symbol(prefix: &str, module: &str) -> String {
    format!("{prefix}_{module}_contract_check")
}

/// A single lowered C symbol: its name, ordered ABI parameter slots, and C
/// return type. This is what a backend declares to its FFI layer and calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiFn {
    /// The fully-qualified, prefixed C symbol (e.g. `weaveffi_math_add`).
    pub symbol: String,
    /// Ordered parameter slots, including any trailing `out_*` and `out_err`.
    pub params: Vec<AbiParam>,
    /// The C return type.
    pub ret: CType,
}

/// How a function crosses the boundary. Exactly one shape applies to any given
/// function: synchronous, asynchronous (callback-completed), or iterator-returning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallShape {
    /// A plain blocking call: [`AbiFn`] is the symbol to invoke.
    Sync(AbiFn),
    /// An async launcher plus its completion-callback typedef.
    Async(AsyncBinding),
    /// An iterator-returning function: an opaque handle plus `next`/`destroy`.
    Iterator(IteratorBinding),
}

/// The lowered surface of an `async` function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncBinding {
    /// The launcher: input slots, optional `cancel_token`, then `callback` and
    /// `context`. Returns `void`.
    pub launch: AbiFn,
    /// The completion-callback function-pointer typedef name
    /// (`{symbol}_callback`).
    pub callback_type: String,
    /// The callback's parameter slots: `(void* context, {prefix}_error* err,
    /// <result fields>)`.
    pub callback_params: Vec<AbiParam>,
}

/// The lowered surface of an `iter<T>`-returning function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IteratorBinding {
    /// The element type `T` of `iter<T>`.
    pub elem: Ty,
    /// The opaque iterator tag (`{prefix}_{path}_{Pascal}Iterator`).
    pub iter_tag: String,
    /// The launcher returning `{iter_tag}*`.
    pub launch: AbiFn,
    /// `int32_t {iter_tag}_next({iter_tag}* iter, T* out_item, ..., error* out_err)`.
    pub next: AbiFn,
    /// `void {iter_tag}_destroy({iter_tag}* iter)`.
    pub destroy_symbol: String,
}

impl IteratorBinding {
    /// The C type of one element: the pointee of `next`'s `T* out_item`
    /// slot.
    ///
    /// # Panics
    ///
    /// Panics if the model built `next` without its `out_item` pointer slot,
    /// which would be a bug in the model construction.
    pub fn item_ctype(&self) -> &CType {
        match &self.next.params[1].ty {
            CType::Ptr { pointee, .. } => pointee,
            other => panic!("iterator `out_item` slot is {other:?}, not a pointer"),
        }
    }
}

/// One IR parameter, retained with its lowered ABI slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamBinding {
    /// The parameter name as written in the IDL.
    pub name: String,
    /// The resolved type a backend renders the parameter as.
    pub ty: Ty,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// The ordered C ABI slots this single parameter expands into.
    pub abi: Vec<AbiParam>,
}

/// A function, fully lowered.
///
/// Free functions and interface members share this shape. For an instance
/// method, [`has_self`](Self::has_self) is `true` and every [`AbiFn`] in
/// [`shape`](Self::shape) carries an implicit leading `const {c_tag}* self`
/// slot that does **not** appear in [`params`](Self::params); a wrapper
/// passes its own native handle there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnBinding {
    /// The function name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the function is marked deprecated, else `None`.
    pub deprecated: Option<String>,
    /// Whether an async function accepts a trailing `cancel_token` slot.
    pub cancellable: bool,
    /// Whether the function reports typed domain errors. A throwing function
    /// surfaces as `throws`/`raises` in idiomatic wrappers using the error
    /// domain in scope ([`Model::error_domain`]); a non-throwing function
    /// has a plain signature, and a
    /// reported error (only ever a producer panic) surfaces as the target's
    /// unrecoverable-error idiom instead.
    pub throws: bool,
    /// `true` for an instance method: the ABI signatures carry an implicit
    /// leading `self` slot not present in [`params`](Self::params).
    pub has_self: bool,
    /// Input parameters with their lowered slots.
    pub params: Vec<ParamBinding>,
    /// The resolved return type (`None` = void). For an iterator function this
    /// is the `iter<T>` type itself; the element `T` also lives in
    /// [`IteratorBinding`]. For an interface constructor this is the
    /// constructed interface type.
    pub ret: Option<Ty>,
    /// Base C symbol (`{prefix}_{module_path}_{name}` for a free function,
    /// `{c_tag}_{name}` for an interface member) before any `_async`/iterator
    /// suffixing.
    pub c_base: String,
    /// The call shape (sync / async / iterator).
    pub shape: CallShape,
}

impl FnBinding {
    /// `true` when the function is `async` (lowered as a callback-completed
    /// launcher).
    pub fn is_async(&self) -> bool {
        matches!(self.shape, CallShape::Async(_))
    }
}

/// A field of a record, a rich-enum variant, or an error code's payload.
///
/// Records and rich enums are value types: they declare no C symbols of their
/// own and cross the ABI serialized inside a value buffer, so a field is just
/// its name and type. Field declaration order **is** the wire order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldBinding {
    /// The field name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// The resolved type of the field.
    pub ty: Ty,
}

/// A struct (record), fully lowered: a plain value type generators emit as a
/// native data class plus buffer read/write functions. No C symbols exist for
/// a record; instances cross the ABI serialized in value buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructBinding {
    /// The struct name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the struct is marked deprecated, else `None`.
    pub deprecated: Option<String>,
    /// The fields in declaration (and wire) order.
    pub fields: Vec<FieldBinding>,
}

/// An enum, fully lowered.
///
/// A *C-style* enum (every variant a bare discriminant) crosses the ABI by
/// value as an integer. An *algebraic* (rich) enum, at least one variant with
/// associated data, is a value type exactly like a struct: it crosses the ABI
/// serialized in a value buffer as an `i32` tag followed by the active
/// variant's fields in declaration order. Either way, the C header still
/// emits the discriminant constants ([`EnumVariantBinding::c_const`]) so C
/// consumers can switch on the value or tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumBinding {
    /// The enum name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the enum is marked deprecated, else `None`.
    pub deprecated: Option<String>,
    /// `{prefix}_{module_path}_{name}`.
    pub c_tag: String,
    /// Every variant, in declaration order.
    pub variants: Vec<EnumVariantBinding>,
    /// `true` when this is a rich (algebraic) sum-type enum: at least one
    /// variant carries fields, and values cross the ABI as buffers.
    pub rich: bool,
}

impl EnumBinding {
    /// `true` when this is a rich (algebraic) sum-type enum.
    pub fn is_rich(&self) -> bool {
        self.rich
    }
}

/// A single enum variant with its precomputed C constant name and any
/// associated data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariantBinding {
    /// The variant name as written in the IDL.
    pub name: String,
    /// The variant's integer discriminant. Doubles as the buffer tag for a
    /// rich enum.
    pub value: i32,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// `{enum_c_tag}_{variant}`.
    pub c_const: String,
    /// Associated data in declaration (and wire) order; empty for a unit
    /// variant or a C-style enum.
    pub fields: Vec<FieldBinding>,
}

/// An interface (reference-counted object type), fully lowered.
///
/// Constructors, methods, and statics are all [`FnBinding`]s sharing the
/// member symbol scheme `{c_tag}_{name}`. Methods additionally carry an
/// implicit leading `const {c_tag}* self` ABI slot ([`FnBinding::has_self`]).
/// A constructor's [`FnBinding::ret`] is synthesized as the interface type
/// itself, so wrappers can reuse their ordinary return-marshalling path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceBinding {
    /// The interface name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the interface is marked deprecated, else `None`.
    pub deprecated: Option<String>,
    /// `{prefix}_{module_path}_{name}`, the opaque tag.
    pub c_tag: String,
    /// Constructors, lowered as statics returning `{c_tag}*`.
    pub constructors: Vec<FnBinding>,
    /// Instance methods, each with the implicit `self` slot.
    pub methods: Vec<FnBinding>,
    /// Static functions namespaced under the interface.
    pub statics: Vec<FnBinding>,
    /// `{c_tag}* {c_tag}_clone(const {c_tag}* self)`: returns a new strong
    /// reference to the same object.
    pub clone_symbol: String,
    /// `void {c_tag}_destroy({c_tag}* self)`: releases one strong reference.
    pub destroy_symbol: String,
}

/// One method of a callback interface, lowered to its vtable entry.
///
/// The consumer implements this; the producer calls it through the vtable.
/// [`abi_params`](Self::abi_params) is the full C slot list of the vtable
/// entry (`void* ctx`, the parameter slots, the return's out slots, then
/// `{prefix}_error* out_err`) and [`abi_ret`](Self::abi_ret) its C return
/// type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackMethodBinding {
    /// The method name as written in the IDL; also the vtable field name.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the method is marked deprecated, else `None`.
    pub deprecated: Option<String>,
    /// Whether the method may report a positive code of the error domain in
    /// scope ([`Model::error_domain`]) through its `out_err`.
    pub throws: bool,
    /// Input parameters with their lowered slots.
    pub params: Vec<ParamBinding>,
    /// The resolved return type (`None` = void). Any family but iterators
    /// and callback interfaces; see
    /// [`lower_callback_return`](crate::abi::lower_callback_return).
    pub ret: Option<Ty>,
    /// The vtable entry's C parameter slots: `ctx`, then every parameter's
    /// slots, then the return's out slots (`out_ptr` and `out_len` for a
    /// string, bytes, or buffer return), then `out_err`.
    pub abi_params: Vec<AbiParam>,
    /// The vtable entry's C return type.
    pub abi_ret: CType,
}

/// A callback interface, fully lowered.
///
/// The C ABI sees a vtable struct ([`vtable_tag`](Self::vtable_tag)) that
/// starts with a fixed header, `uint32_t size` (the vtable's size as the
/// consumer compiled it), `uint32_t flags` (reserved, `0`), and
/// `void (*free)(void* ctx)`, followed by one function-pointer field per
/// method in declaration order. A parameter of this type lowers to two
/// slots, `void* {name}_ctx` and `const {vtable_tag}* {name}_vtable`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackInterfaceBinding {
    /// The callback interface name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the callback interface is marked deprecated,
    /// else `None`.
    pub deprecated: Option<String>,
    /// `{prefix}_{module_path}_{name}`, the type's C name stem.
    pub c_tag: String,
    /// `{c_tag}_vtable`, the vtable struct tag.
    pub vtable_tag: String,
    /// Methods in declaration (and vtable) order. Never empty.
    pub methods: Vec<CallbackMethodBinding>,
}

/// One error code of a module's error domain, with its C constant name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCodeBinding {
    /// The code name exactly as written in the IDL (e.g. `KEY_NOT_FOUND`).
    pub name: String,
    /// The numeric ABI code carried in `{prefix}_error.code`.
    pub value: i32,
    /// The default human-readable message for the code.
    pub message: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// `{domain_c_tag}_{name}`, the C enum constant.
    pub c_const: String,
    /// Structured payload fields this code carries, in declaration (and wire)
    /// order. When non-empty, a matching error's `payload_ptr`/`payload_len`
    /// slots hold these fields serialized in the value-buffer format; empty
    /// means the payload slots are null.
    pub fields: Vec<FieldBinding>,
}

impl ErrorCodeBinding {
    /// `{c_const}_payload`, the struct the C value-buffer header declares
    /// for the code's [`fields`](Self::fields) (only when there are any).
    pub fn payload_tag(&self) -> String {
        format!("{}_payload", self.c_const)
    }
}

/// An error domain, lowered on the module that declares it.
///
/// Every throwing function reports codes from the domain in scope for its
/// module: the module's own, or the nearest ancestor's
/// ([`Model::error_domain`]). Backends emit one error type per declaring
/// module and reference it from inheriting submodules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBinding {
    /// The domain name as written in the IDL (e.g. `KvError`).
    pub name: String,
    /// PascalCase type name with exactly one `Error` suffix (e.g. `KvError`);
    /// backends that brand exceptions swap the suffix via
    /// [`crate::errors::type_name`].
    pub type_name: String,
    /// Underscore-joined path of the module that declares the domain.
    pub owner_path: String,
    /// `{prefix}_{owner_path}_{name}`, the C tag naming the domain's code
    /// constants.
    pub c_tag: String,
    /// The domain's codes in declaration order.
    pub codes: Vec<ErrorCodeBinding>,
}

/// One module, flattened with its underscore-joined symbol path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBinding {
    /// The module name (its final path segment).
    pub name: String,
    /// Path segments from the root (e.g. `["outer", "inner"]`).
    pub segments: Vec<String>,
    /// Underscore-joined path used as the C symbol segment (e.g. `outer_inner`).
    pub path: String,
    /// Dot-joined path (e.g. `outer.inner`), the declaration path diagnostics
    /// and generated comments quote.
    pub dot_path: String,
    /// The module's doc comment, if the IDL records one.
    pub doc: Option<String>,
    /// The error domain this module declares, if any. A module without one
    /// inherits the nearest ancestor's ([`Model::error_domain`]).
    pub errors: Option<ErrorBinding>,
    /// Enums declared in this module, fully lowered.
    pub enums: Vec<EnumBinding>,
    /// Structs declared in this module, fully lowered.
    pub structs: Vec<StructBinding>,
    /// Interfaces declared in this module, fully lowered.
    pub interfaces: Vec<InterfaceBinding>,
    /// Callback interfaces declared in this module, fully lowered.
    pub callback_interfaces: Vec<CallbackInterfaceBinding>,
    /// Functions declared in this module, fully lowered.
    pub functions: Vec<FnBinding>,
}

impl ModuleBinding {
    /// Every callable in this module: free functions, then each interface's
    /// constructors, methods, and statics.
    pub fn callables(&self) -> impl Iterator<Item = &FnBinding> {
        self.functions
            .iter()
            .chain(self.interfaces.iter().flat_map(|i| {
                i.constructors
                    .iter()
                    .chain(i.methods.iter())
                    .chain(i.statics.iter())
            }))
    }

    /// `true` when any callable in this module is `async`.
    pub fn has_async(&self) -> bool {
        self.callables().any(FnBinding::is_async)
    }

    /// `true` when any callable in this module returns an iterator.
    pub fn has_iterators(&self) -> bool {
        self.callables()
            .any(|f| matches!(f.shape, CallShape::Iterator(_)))
    }
}

/// The whole API, validated and lowered for code generation.
///
/// Built once per run by [`validate`](crate::validate::validate); every
/// generator renders from it alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    /// The library's identity: its name, C symbol prefix, native library
    /// name, and package metadata.
    pub identity: Identity,
    /// The IR schema version of the source document.
    pub version: String,
    /// Modules in depth-first pre-order, each carrying its joined symbol path.
    pub modules: Vec<ModuleBinding>,
    /// Every user type name, mapped to its declaration.
    pub types: TypeIndex,
}

impl Model {
    /// Build the model without validating the document.
    ///
    /// Type names that resolve to no declaration become [`Ty::Record`]
    /// references named exactly as written. Tests use it for hand-built
    /// trees; everything else goes through
    /// [`validate`](crate::validate::validate).
    #[doc(hidden)]
    #[must_use]
    pub fn assume_valid(api: &Api, identity: Identity) -> Self {
        build::build(api, identity)
    }

    /// Resolve a written type reference against this model's declarations.
    /// A name no declaration provides resolves to a [`Ty::Record`] (see
    /// [`assume_valid`](Self::assume_valid)).
    pub fn resolve(&self, ty: &TypeRef) -> Ty {
        build::resolve(&self.types, ty)
    }

    /// The C symbol prefix every emitted name starts with.
    pub fn prefix(&self) -> &str {
        &self.identity.prefix
    }

    /// The top-level modules (those with a single path segment), in order.
    pub fn roots(&self) -> impl Iterator<Item = &ModuleBinding> {
        self.modules.iter().filter(|m| m.segments.len() == 1)
    }

    /// The direct submodules of `parent`, in declaration order. Backends that
    /// render nested namespaces recurse with this instead of re-walking the
    /// IR tree.
    pub fn children<'a>(
        &'a self,
        parent: &'a ModuleBinding,
    ) -> impl Iterator<Item = &'a ModuleBinding> + 'a {
        self.modules.iter().filter(move |m| {
            m.segments.len() == parent.segments.len() + 1
                && m.segments[..parent.segments.len()] == parent.segments[..]
        })
    }

    /// Iterate every function across all modules, paired with its module.
    pub fn functions(&self) -> impl Iterator<Item = (&ModuleBinding, &FnBinding)> {
        self.modules
            .iter()
            .flat_map(|m| m.functions.iter().map(move |f| (m, f)))
    }

    /// Iterate every callable (free functions and interface members) across
    /// all modules, paired with its module.
    pub fn callables(&self) -> impl Iterator<Item = (&ModuleBinding, &FnBinding)> {
        self.modules
            .iter()
            .flat_map(|m| m.callables().map(move |f| (m, f)))
    }

    /// Iterate every callback interface across all modules, paired with its
    /// module.
    pub fn callback_interfaces(
        &self,
    ) -> impl Iterator<Item = (&ModuleBinding, &CallbackInterfaceBinding)> {
        self.modules
            .iter()
            .flat_map(|m| m.callback_interfaces.iter().map(move |c| (m, c)))
    }

    /// The declaration of the user type `name`.
    ///
    /// # Panics
    ///
    /// Panics when `name` isn't declared, which validation rules out for
    /// every name a [`Ty`] carries.
    fn decl(&self, name: &str, kinds: &[TypeKind]) -> &TypeDecl {
        match self.types.get(name) {
            Some(d) if kinds.contains(&d.kind) => d,
            Some(d) => panic!("type '{name}' is a {:?}, not one of {kinds:?}", d.kind),
            None => panic!("type '{name}' is not declared"),
        }
    }

    /// The module declaring the user type `name`.
    ///
    /// # Panics
    ///
    /// Panics when `name` isn't declared, which validation rules out for
    /// every name a [`Ty`] carries.
    pub fn owner(&self, name: &str) -> &ModuleBinding {
        let decl = self.types.get(name);
        let decl = decl.unwrap_or_else(|| panic!("type '{name}' is not declared"));
        &self.modules[decl.module]
    }

    /// The interface named `name`, as carried by [`Ty::Interface`].
    ///
    /// # Panics
    ///
    /// Panics when no interface has that name.
    pub fn interface(&self, name: &str) -> &InterfaceBinding {
        let d = self.decl(name, &[TypeKind::Interface]);
        &self.modules[d.module].interfaces[d.index]
    }

    /// The callback interface named `name`, as carried by
    /// [`Ty::CallbackInterface`].
    ///
    /// # Panics
    ///
    /// Panics when no callback interface has that name.
    pub fn callback_interface(&self, name: &str) -> &CallbackInterfaceBinding {
        let d = self.decl(name, &[TypeKind::CallbackInterface]);
        &self.modules[d.module].callback_interfaces[d.index]
    }

    /// The enum (C-style or rich) named `name`, as carried by [`Ty::Enum`]
    /// and [`Ty::RichEnum`].
    ///
    /// # Panics
    ///
    /// Panics when no enum has that name.
    pub fn enumeration(&self, name: &str) -> &EnumBinding {
        let d = self.decl(name, &[TypeKind::Enum, TypeKind::RichEnum]);
        &self.modules[d.module].enums[d.index]
    }

    /// The error domain in scope for `module`'s throwing functions: its own,
    /// else the nearest ancestor's, else `None` (in which case validation
    /// has rejected any `throws` there).
    pub fn error_domain(&self, module: &ModuleBinding) -> Option<&ErrorBinding> {
        (1..=module.segments.len()).rev().find_map(|n| {
            let scope = &module.segments[..n];
            self.modules
                .iter()
                .find(|m| m.segments == scope)
                .and_then(|m| m.errors.as_ref())
        })
    }

    /// Every C identifier the library's ABI declares, paired with a
    /// human-readable description of the declaration that owns it, in
    /// declaration order. Validation rejects an API in which two entries share
    /// an identifier.
    ///
    /// Covers the runtime surface ([`RUNTIME_SYMBOLS`]), each top-level
    /// module's contract table function and the C header's checker for it,
    /// every callable's symbols (sync entry point, async launcher and
    /// completion type, iterator launcher, type, `_next`, and `_destroy`),
    /// interface types with their `_clone` and `_destroy`, enum types and
    /// constants (and a rich enum's tag type), error-domain types and code
    /// constants, callback-interface vtable types, and what the C
    /// value-buffer helper header declares per user type: the struct of
    /// every record, rich enum, and error code with fields
    /// ([`ErrorCodeBinding::payload_tag`]), each with its
    /// [`VALUE_CODECS`]. (The header's list, map, and optional shapes fall
    /// in [`RESERVED_SYMBOL_FAMILIES`].)
    pub fn c_symbols(&self) -> Vec<(String, String)> {
        let p = self.prefix();
        let mut out: Vec<(String, String)> = RUNTIME_SYMBOLS
            .iter()
            .map(|s| (format!("{p}_{s}"), format!("the runtime symbol '{p}_{s}'")))
            .collect();
        for m in self.roots() {
            out.push((
                contract_symbol(p, &m.name),
                format!("the contract table of module '{}'", m.name),
            ));
            out.push((
                contract_check_symbol(p, &m.name),
                format!("the contract check of module '{}'", m.name),
            ));
        }
        let callable = |out: &mut Vec<(String, String)>, f: &FnBinding, owner: &str| {
            let what = format!("'{owner}.{}'", f.name);
            match &f.shape {
                CallShape::Sync(abi) => out.push((abi.symbol.clone(), format!("function {what}"))),
                CallShape::Async(a) => {
                    out.push((a.launch.symbol.clone(), format!("async function {what}")));
                    out.push((
                        a.callback_type.clone(),
                        format!("the completion type of {what}"),
                    ));
                }
                CallShape::Iterator(it) => {
                    out.push((it.launch.symbol.clone(), format!("function {what}")));
                    out.push((it.iter_tag.clone(), format!("the iterator type of {what}")));
                    out.push((
                        it.next.symbol.clone(),
                        format!("the iterator step of {what}"),
                    ));
                    out.push((
                        it.destroy_symbol.clone(),
                        format!("the iterator destructor of {what}"),
                    ));
                }
            }
        };
        let codecs = |out: &mut Vec<(String, String)>, tag: &str, what: &str| {
            for codec in VALUE_CODECS {
                out.push((
                    format!("{tag}_{codec}"),
                    format!("the value-buffer codec '{tag}_{codec}' of {what}"),
                ));
            }
        };
        for m in &self.modules {
            let dot = &m.dot_path;
            if let Some(e) = &m.errors {
                out.push((e.c_tag.clone(), format!("error domain '{dot}.{}'", e.name)));
                for c in &e.codes {
                    let what = format!("error code '{dot}.{}.{}'", e.name, c.name);
                    out.push((c.c_const.clone(), what.clone()));
                    if !c.fields.is_empty() {
                        let payload = c.payload_tag();
                        codecs(&mut out, &payload, &format!("the payload of {what}"));
                        out.push((payload, format!("the payload struct of {what}")));
                    }
                }
            }
            for e in &m.enums {
                out.push((e.c_tag.clone(), format!("enum '{dot}.{}'", e.name)));
                if e.rich {
                    out.push((
                        format!("{}_Tag", e.c_tag),
                        format!("the tag type of enum '{dot}.{}'", e.name),
                    ));
                    codecs(&mut out, &e.c_tag, &format!("enum '{dot}.{}'", e.name));
                }
                for v in &e.variants {
                    out.push((
                        v.c_const.clone(),
                        format!("enum variant '{dot}.{}.{}'", e.name, v.name),
                    ));
                }
            }
            for s in &m.structs {
                let tag = format!("{p}_{}_{}", m.path, s.name);
                let what = format!("record '{dot}.{}'", s.name);
                codecs(&mut out, &tag, &what);
                out.push((tag, what));
            }
            for c in &m.callback_interfaces {
                out.push((
                    c.vtable_tag.clone(),
                    format!("the vtable of callback interface '{dot}.{}'", c.name),
                ));
            }
            for i in &m.interfaces {
                let owner = format!("{dot}.{}", i.name);
                out.push((i.c_tag.clone(), format!("interface '{owner}'")));
                out.push((i.clone_symbol.clone(), format!("the clone of '{owner}'")));
                out.push((
                    i.destroy_symbol.clone(),
                    format!("the destructor of '{owner}'"),
                ));
                for f in i.constructors.iter().chain(&i.methods).chain(&i.statics) {
                    callable(&mut out, f, &owner);
                }
            }
            for f in &m.functions {
                callable(&mut out, f, dot);
            }
        }
        out
    }

    /// `true` when any callable anywhere in the API is `async`.
    pub fn has_async(&self) -> bool {
        self.modules.iter().any(ModuleBinding::has_async)
    }

    /// `true` when any callable anywhere in the API returns an iterator.
    pub fn has_iterators(&self) -> bool {
        self.modules.iter().any(ModuleBinding::has_iterators)
    }

    /// `true` when the API declares any callback interface.
    pub fn has_callback_interfaces(&self) -> bool {
        self.modules
            .iter()
            .any(|m| !m.callback_interfaces.is_empty())
    }

    /// `true` when the API declares any interface.
    pub fn has_interfaces(&self) -> bool {
        self.modules.iter().any(|m| !m.interfaces.is_empty())
    }

    /// `true` when any type anywhere in the API crosses the ABI as a value
    /// buffer (records, rich enums, optionals, lists, maps, error payloads).
    pub fn has_buffers(&self) -> bool {
        let buffered = |ty: &Ty| ty.any(&Ty::is_buffered);
        let signature = |params: &[ParamBinding], ret: &Option<Ty>| {
            params.iter().any(|p| buffered(&p.ty)) || ret.as_ref().is_some_and(buffered)
        };
        self.modules.iter().any(|m| {
            !m.structs.is_empty()
                || m.enums.iter().any(|e| e.rich)
                || m.errors
                    .as_ref()
                    .is_some_and(|e| e.codes.iter().any(|c| !c.fields.is_empty()))
                || m.callables().any(|f| signature(&f.params, &f.ret))
                || m.callback_interfaces
                    .iter()
                    .any(|c| c.methods.iter().any(|f| signature(&f.params, &f.ret)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        CallbackInterfaceDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef,
        Module, Param, StructDef, StructField, TypeRef,
    };
    use crate::ty::Prim;

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            doc: None,
        }
    }

    fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
        }
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            doc: None,
            functions: vec![],
            interfaces: vec![],
            callback_interfaces: vec![],
            structs: vec![],
            enums: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn build(modules: Vec<Module>, prefix: &str) -> Model {
        let api = Api {
            version: crate::ir::CURRENT_SCHEMA_VERSION.into(),
            modules,
        };
        Model::assume_valid(&api, Identity::named(prefix))
    }

    fn rendered(abi: &AbiFn) -> Vec<String> {
        abi.params
            .iter()
            .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
            .collect()
    }

    #[test]
    fn sync_function_symbol_and_sig() {
        let m = Module {
            functions: vec![func(
                "add",
                vec![
                    param("a", TypeRef::Prim(Prim::I32)),
                    param("b", TypeRef::Prim(Prim::I32)),
                ],
                Some(TypeRef::Prim(Prim::I32)),
            )],
            ..module("math")
        };
        let model = build(vec![m], "weaveffi");
        let f = &model.modules[0].functions[0];
        assert_eq!(f.c_base, "weaveffi_math_add");
        let CallShape::Sync(abi) = &f.shape else {
            panic!("expected sync")
        };
        assert_eq!(abi.symbol, "weaveffi_math_add");
        assert_eq!(abi.ret, CType::Int32);
        assert_eq!(
            rendered(abi),
            ["int32_t a", "int32_t b", "weaveffi_error* out_err"]
        );

        let model = build(vec![module("net")], "acme");
        assert_eq!(model.prefix(), "acme");
    }

    #[test]
    fn async_function_has_launch_and_callback() {
        let m = Module {
            functions: vec![Function {
                cancellable: true,
                r#async: true,
                ..func(
                    "fetch",
                    vec![param("id", TypeRef::Prim(Prim::I64))],
                    Some(TypeRef::Prim(Prim::String)),
                )
            }],
            ..module("net")
        };
        let model = build(vec![m], "weaveffi");
        let CallShape::Async(a) = &model.modules[0].functions[0].shape else {
            panic!("expected async")
        };
        assert_eq!(a.launch.symbol, "weaveffi_net_fetch");
        assert_eq!(a.callback_type, "weaveffi_net_fetch_callback");
        let names: Vec<&str> = a.launch.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["id", "cancel_token", "callback", "context"]);
        assert_eq!(a.callback_params[0].name, "context");
        assert_eq!(a.callback_params[1].name, "err");
        assert_eq!(a.callback_params[2].name, "result_ptr");
        assert!(model.has_async());
    }

    #[test]
    fn iterator_function_has_next_and_destroy() {
        let m = Module {
            functions: vec![func(
                "get_messages",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::Prim(Prim::String)))),
            )],
            ..module("events")
        };
        let model = build(vec![m], "weaveffi");
        let CallShape::Iterator(it) = &model.modules[0].functions[0].shape else {
            panic!("expected iterator")
        };
        assert_eq!(it.iter_tag, "weaveffi_events_GetMessagesIterator");
        assert_eq!(it.launch.symbol, "weaveffi_events_get_messages");
        assert_eq!(it.next.symbol, "weaveffi_events_GetMessagesIterator_next");
        assert_eq!(
            it.destroy_symbol,
            "weaveffi_events_GetMessagesIterator_destroy"
        );
        assert_eq!(it.elem, Ty::Prim(Prim::String));
        assert_eq!(it.next.ret, CType::Int32);
        assert_eq!(it.next.params[1].ty.render_c("weaveffi"), "const uint8_t**");
        assert_eq!(it.item_ctype().render_c("weaveffi"), "const uint8_t*");
        assert!(model.has_iterators());
    }

    #[test]
    fn user_types_resolve_to_kinds_and_buffers() {
        let shared = Module {
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                deprecated: Some("use Person".into()),
                fields: vec![
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::Prim(Prim::String),
                        doc: None,
                    },
                    StructField {
                        name: "status".into(),
                        ty: TypeRef::Named("Status".into()),
                        doc: None,
                    },
                    StructField {
                        name: "store".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Named("Store".into()))),
                        doc: None,
                    },
                ],
            }],
            enums: vec![EnumDef {
                name: "Status".into(),
                doc: None,
                deprecated: None,
                variants: vec![EnumVariant {
                    name: "Ok".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                }],
            }],
            interfaces: vec![InterfaceDef {
                name: "Store".into(),
                doc: None,
                deprecated: None,
                constructors: vec![func("open", vec![], None)],
                methods: vec![func(
                    "save",
                    vec![param("contact", TypeRef::Named("Contact".into()))],
                    Some(TypeRef::List(Box::new(TypeRef::Named("Contact".into())))),
                )],
                statics: vec![],
            }],
            ..module("contacts")
        };
        let other = Module {
            functions: vec![func(
                "status_of",
                vec![param("store", TypeRef::Named("Store".into()))],
                Some(TypeRef::Named("Status".into())),
            )],
            ..module("ops")
        };
        let model = build(vec![shared, other], "weaveffi");
        let contacts = &model.modules[0];
        let s = &contacts.structs[0];
        assert_eq!(s.deprecated.as_deref(), Some("use Person"));
        assert_eq!(s.fields[1].ty, Ty::Enum("Status".into()));
        assert_eq!(
            s.fields[2].ty,
            Ty::Optional(Box::new(Ty::Interface("Store".into())))
        );
        assert_eq!(contacts.enums[0].c_tag, "weaveffi_contacts_Status");
        assert_eq!(
            contacts.enums[0].variants[0].c_const,
            "weaveffi_contacts_Status_Ok"
        );

        let iface = &contacts.interfaces[0];
        assert_eq!(iface.c_tag, "weaveffi_contacts_Store");
        assert_eq!(iface.clone_symbol, "weaveffi_contacts_Store_clone");
        assert_eq!(iface.destroy_symbol, "weaveffi_contacts_Store_destroy");
        assert_eq!(
            iface.constructors[0].ret,
            Some(Ty::Interface("Store".into()))
        );
        let save = &iface.methods[0];
        assert!(save.has_self);
        assert_eq!(save.params[0].ty, Ty::Record("Contact".into()));
        let CallShape::Sync(abi) = &save.shape else {
            panic!("expected sync")
        };
        assert_eq!(
            rendered(abi),
            [
                "const weaveffi_contacts_Store* self",
                "const uint8_t* contact_ptr",
                "size_t contact_len",
                "size_t* out_len",
                "weaveffi_error* out_err"
            ]
        );
        assert_eq!(abi.ret.render_c("weaveffi"), "const uint8_t*");

        let ops = &model.modules[1];
        let f = &ops.functions[0];
        assert_eq!(f.params[0].ty, Ty::Interface("Store".into()));
        assert_eq!(f.ret, Some(Ty::Enum("Status".into())));
        let CallShape::Sync(abi) = &f.shape else {
            panic!("expected sync")
        };
        assert_eq!(
            rendered(abi),
            [
                "const weaveffi_contacts_Store* store",
                "weaveffi_error* out_err"
            ]
        );
        assert_eq!(abi.ret.render_c("weaveffi"), "weaveffi_contacts_Status");
        assert!(model.has_buffers());
        assert!(model.has_interfaces());
        assert_eq!(model.interface("Store").c_tag, "weaveffi_contacts_Store");
        assert_eq!(model.owner("Status").path, "contacts");
        assert_eq!(
            model.enumeration("Status").c_tag,
            "weaveffi_contacts_Status"
        );
    }

    #[test]
    fn callback_interfaces_lower_to_vtables() {
        let m = Module {
            callback_interfaces: vec![CallbackInterfaceDef {
                name: "Listener".into(),
                doc: Some("Receives messages.".into()),
                deprecated: None,
                methods: vec![
                    func(
                        "on_message",
                        vec![param("text", TypeRef::Prim(Prim::String))],
                        None,
                    ),
                    func("should_stop", vec![], Some(TypeRef::Prim(Prim::Bool))),
                ],
            }],
            functions: vec![func(
                "subscribe",
                vec![param("listener", TypeRef::Named("Listener".into()))],
                None,
            )],
            ..module("events")
        };
        let model = build(vec![m], "weaveffi");
        let mb = &model.modules[0];
        let cb = &mb.callback_interfaces[0];
        assert_eq!(cb.c_tag, "weaveffi_events_Listener");
        assert_eq!(cb.vtable_tag, "weaveffi_events_Listener_vtable");
        let on_message = &cb.methods[0];
        let slots: Vec<String> = on_message
            .abi_params
            .iter()
            .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
            .collect();
        assert_eq!(
            slots,
            [
                "void* ctx",
                "const uint8_t* text_ptr",
                "size_t text_len",
                "weaveffi_error* out_err"
            ]
        );
        assert_eq!(on_message.abi_ret, CType::Void);
        assert_eq!(cb.methods[1].abi_ret, CType::Bool);
        assert_eq!(model.callback_interface("Listener").c_tag, cb.c_tag);
        assert!(model.has_callback_interfaces());

        let f = &mb.functions[0];
        assert_eq!(f.params[0].ty, Ty::CallbackInterface("Listener".into()));
        let CallShape::Sync(abi) = &f.shape else {
            panic!("expected sync")
        };
        assert_eq!(
            rendered(abi),
            [
                "void* listener_ctx",
                "const weaveffi_events_Listener_vtable* listener_vtable",
                "weaveffi_error* out_err"
            ]
        );
        assert!(!model.has_buffers());
    }

    #[test]
    fn nested_modules_flatten_pre_order_with_paths_and_docs() {
        let inner = Module {
            functions: vec![Function {
                doc: Some("Leaf.".into()),
                ..func("leaf_fn", vec![], None)
            }],
            ..module("inner")
        };
        let outer = Module {
            doc: Some("Outer module.".into()),
            functions: vec![func("outer_fn", vec![], None)],
            modules: vec![inner],
            ..module("outer")
        };
        let model = build(vec![outer], "weaveffi");
        let paths: Vec<&str> = model.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["outer", "outer_inner"]);
        assert_eq!(model.modules[1].dot_path, "outer.inner");
        assert_eq!(
            model.modules[1].functions[0].c_base,
            "weaveffi_outer_inner_leaf_fn"
        );
        assert_eq!(model.modules[0].doc.as_deref(), Some("Outer module."));
        assert_eq!(model.modules[1].doc, None);
    }

    #[test]
    fn error_domains_are_declared_once_and_inherited_by_lookup() {
        let domain = ErrorDomain {
            name: "OuterError".into(),
            codes: vec![ErrorCode {
                name: "Bad".into(),
                code: 1,
                message: "bad".into(),
                doc: None,
                fields: vec![],
            }],
        };
        let outer = Module {
            errors: Some(domain),
            modules: vec![module("inner")],
            ..module("outer")
        };
        let model = build(vec![outer, module("other")], "weaveffi");
        let [outer, inner, other] = &model.modules[..] else {
            panic!("three modules");
        };
        assert!(outer.errors.is_some() && inner.errors.is_none());
        assert_eq!(
            model.error_domain(inner).map(|e| e.c_tag.as_str()),
            Some("weaveffi_outer_OuterError")
        );
        assert_eq!(model.error_domain(outer), outer.errors.as_ref());
        assert!(model.error_domain(other).is_none());
    }
}
