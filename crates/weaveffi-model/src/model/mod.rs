//! The **model**: the one validated, fully lowered view of an API that every
//! language backend consumes.
//!
//! [`validate`](crate::validate::validate) checks an [`Api`](crate::ir::Api)
//! and builds the [`Model`] exactly once per run. The model owns everything
//! a generator reads:
//!
//! * the library's [`Identity`] (C symbol prefix, library name, package
//!   metadata) and the schema version;
//! * a flat list of [`ModuleBinding`]s, linked by [`parent`] and
//!   [`children`] indices, in which every **type** is resolved
//!   ([`Ty`], [`ParamTy`], [`RetTy`]; no unresolved names);
//! * every emitted **C symbol name**, precomputed once, so all backends agree
//!   by construction, plus the typed [symbol table](Model::c_symbols);
//! * every function, interface member, and callback-interface method with
//!   its lowered [`AbiFn`] signature and its **passing contracts**
//!   ([`ArgPass`] on each parameter, [`RetPass`], [`ResultPass`],
//!   [`ItemPass`], or [`CallbackRetPass`] for what comes back, and the
//!   [`ErrorStrategy`]), with every obligation resolved (release symbols,
//!   element types, error domain names), so no backend re-derives arity,
//!   slot order, `out_*`/`out_err` placement, or ownership; and
//! * the [`TypeIndex`], which maps each (global) type name to its
//!   declaration and owning module, behind typed lookups such as
//!   [`Model::interface`], [`Model::error_domain`], and [`Model::owner`].
//!
//! A backend reads the *idiomatic* shape from the resolved types and the
//! *native* shape from the passing contracts and [`AbiFn`]s, then writes
//! only the marshalling that bridges the two in its own idioms. Generators
//! never see the [`Api`](crate::ir::Api).
//!
//! [`parent`]: ModuleBinding::parent
//! [`children`]: ModuleBinding::children
//! [`ArgPass`]: crate::plan::ArgPass
//! [`RetPass`]: crate::plan::RetPass
//! [`ResultPass`]: crate::plan::ResultPass
//! [`ItemPass`]: crate::plan::ItemPass
//! [`CallbackRetPass`]: crate::plan::CallbackRetPass
//! [`ErrorStrategy`]: crate::plan::ErrorStrategy

mod build;
mod symbols;
#[cfg(all(test, feature = "idl"))]
mod tests;

pub(crate) use build::{build, index};
pub use symbols::{Symbol, SymbolOwner};

use crate::abi::{AbiParam, CType};
use crate::pkg::Identity;
use crate::plan::{ArgPass, CallbackRetPass, ErrorStrategy, ItemPass, ResultPass, RetPass};
use crate::ty::{ParamTy, RetTy, Ty, TypeDecl, TypeIndex, TypeKind};

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
pub const ABI_VERSION: u32 = 5;

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

/// `{prefix}_{path}_{name}`: the C tag (type name stem) of a declaration
/// named `name` in the module whose underscore-joined path is `path`.
pub(crate) fn c_tag(prefix: &str, path: &str, name: &str) -> String {
    format!("{prefix}_{path}_{name}")
}

/// `{tag}_{member}`: a symbol hanging off a C tag (`_clone`, `_destroy`,
/// an interface member, an iterator's `_next`).
pub(crate) fn member_symbol(tag: &str, member: &str) -> String {
    format!("{tag}_{member}")
}

/// A single lowered C signature: its name, ordered ABI parameter slots, and
/// C return type. This is what a backend declares to its FFI layer and
/// calls (or, for a callback method, implements).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiFn {
    /// The fully qualified, prefixed C symbol (for example
    /// `weaveffi_math_add`); for a callback method, the vtable field name.
    pub symbol: String,
    /// Ordered parameter slots, including any leading `self` or `ctx` and
    /// any trailing `out_*` and `out_err`.
    pub params: Vec<AbiParam>,
    /// The C return type.
    pub ret: CType,
}

/// How a callable crosses the boundary: a blocking call or an async launch.
///
/// An iterator-returning function is a [`Sync`](Self::Sync) call whose
/// [`FnBinding::ret_pass`] is [`RetPass::Iterator`].
// A model holds a few of these per callable and never moves them in bulk,
// so boxing the large variant would only cost consumers a deref.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallShape {
    /// A blocking call: invoke [`FnBinding::abi`], then receive its return
    /// as [`FnBinding::ret_pass`] says.
    Sync,
    /// An async launch: invoke [`FnBinding::abi`] (the launcher, which
    /// returns `void`) and receive the result in the completion callback.
    Async(AsyncBinding),
}

/// The lowered completion side of an `async` callable.
///
/// The launcher is [`FnBinding::abi`]: the receiver (if any), the input
/// slots, the `cancel_token` slot when cancellable, then
/// `{callback_type} callback` and `void* context`. It returns `void` and has
/// no `out_err`; every failure arrives through the completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncBinding {
    /// The completion-callback function-pointer typedef
    /// (`{symbol}_callback`).
    pub callback_type: String,
    /// The completion callback's parameter slots, in order:
    /// `void* context`, `{prefix}_error* err`, then the result slots
    /// ([`result`](Self::result)).
    pub callback_params: Vec<AbiParam>,
    /// How the result arrives in the callback, with the release it owes.
    pub result: ResultPass,
    /// The launcher's `{prefix}_cancel_token* cancel_token` slot for a
    /// `cancellable` callable, else `None`.
    pub cancel_token: Option<AbiParam>,
}

impl AsyncBinding {
    /// `true` when the launcher takes a cancel token.
    #[must_use]
    pub fn cancellable(&self) -> bool {
        self.cancel_token.is_some()
    }
}

/// The lowered surface of an `iter<T>`-returning callable.
///
/// The launcher is [`FnBinding::abi`], which returns `{iter_tag}*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IteratorBinding {
    /// The element type `T` of `iter<T>`.
    pub elem: Ty,
    /// The opaque iterator tag (`{prefix}_{owner}_{Pascal}Iterator`).
    pub iter_tag: String,
    /// `int32_t {iter_tag}_next({iter_tag}* iter, <item slots>,
    /// {prefix}_error* out_err)`: returns `1` with an element, `0` when
    /// done.
    pub next: AbiFn,
    /// How each element arrives through `next`'s out slots.
    pub item: ItemPass,
    /// `void {iter_tag}_destroy({iter_tag}* iter)`.
    pub destroy_symbol: String,
}

/// One parameter of a callable, with its lowered passing contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamBinding {
    /// The parameter name as written in the IDL.
    pub name: String,
    /// The resolved type a backend renders the parameter as.
    pub ty: ParamTy,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// The C slots the parameter occupies and how they're filled.
    pub pass: ArgPass,
}

/// A callable (free function or interface member), fully lowered.
///
/// For an instance method, [`receiver`](Self::receiver) is the implicit
/// leading `const {c_tag}* self` slot of [`abi`](Self::abi), which does
/// **not** appear in [`params`](Self::params); a wrapper passes its own
/// native handle there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnBinding {
    /// The callable's name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the callable is marked deprecated, else
    /// `None`.
    pub deprecated: Option<String>,
    /// The `const {c_tag}* self` slot of an instance method, else `None`.
    pub receiver: Option<AbiParam>,
    /// Input parameters with their passing contracts.
    pub params: Vec<ParamBinding>,
    /// The resolved return type (`None` = void). For an interface
    /// constructor this is the constructed interface type.
    pub ret: Option<RetTy>,
    /// How [`abi`](Self::abi)'s C return and trailing out slots carry the
    /// return back: the value for a sync call, [`RetPass::Iterator`] for an
    /// iterator launcher, and [`RetPass::Void`] for an async launcher.
    pub ret_pass: RetPass,
    /// How a reported error is interpreted.
    pub error: ErrorStrategy,
    /// The symbol to call: the sync entry point, the async launcher, or the
    /// iterator launcher, with its full ordered slot list.
    pub abi: AbiFn,
    /// Whether the call blocks or completes asynchronously.
    pub shape: CallShape,
}

impl FnBinding {
    /// `true` for an instance method (one with a [`receiver`](Self::receiver)).
    #[must_use]
    pub fn has_self(&self) -> bool {
        self.receiver.is_some()
    }

    /// `true` when the callable is `async`.
    #[must_use]
    pub fn is_async(&self) -> bool {
        matches!(self.shape, CallShape::Async(_))
    }

    /// The completion side of an async callable, else `None`.
    #[must_use]
    pub fn async_binding(&self) -> Option<&AsyncBinding> {
        match &self.shape {
            CallShape::Async(a) => Some(a),
            CallShape::Sync => None,
        }
    }

    /// The iterator surface of an iterator-returning callable, else `None`.
    #[must_use]
    pub fn iterator(&self) -> Option<&IteratorBinding> {
        match &self.ret_pass {
            RetPass::Iterator(it) => Some(it),
            _ => None,
        }
    }

    /// `true` when the callable is async and takes a cancel token.
    #[must_use]
    pub fn cancellable(&self) -> bool {
        self.async_binding().is_some_and(AsyncBinding::cancellable)
    }
}

/// A field of a record, a rich-enum variant, or an error code's payload.
///
/// Records and rich enums are value types: they declare no C functions of
/// their own and cross the ABI serialized inside a value buffer, so a field
/// is just its name and type. Field declaration order **is** the wire order.
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
/// native data class plus buffer read/write functions. Instances cross the
/// ABI serialized in value buffers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructBinding {
    /// The struct name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the struct is marked deprecated, else `None`.
    pub deprecated: Option<String>,
    /// `{prefix}_{module_path}_{name}`: the struct the C value-buffer
    /// helper header declares, and the stem of its codecs.
    pub c_tag: String,
    /// The fields in declaration (and wire) order.
    pub fields: Vec<FieldBinding>,
}

/// An enum, fully lowered.
///
/// A *C-style* enum (every variant a bare discriminant) crosses the ABI by
/// value as an `int32_t`. An *algebraic* (rich) enum, at least one variant
/// with associated data, is a value type exactly like a struct: it crosses
/// the ABI serialized in a value buffer as an `i32` tag followed by the
/// active variant's fields in declaration order. Either way, the C header
/// still emits the discriminant constants
/// ([`EnumVariantBinding::c_const`]) so C consumers can switch on the value
/// or tag.
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
    #[must_use]
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
/// implicit leading `const {c_tag}* self` slot ([`FnBinding::receiver`]).
/// A constructor's [`FnBinding::ret`] is the interface type itself, so
/// wrappers can reuse their ordinary return-marshalling path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceBinding {
    /// The interface name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the interface is marked deprecated, else
    /// `None`.
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

impl InterfaceBinding {
    /// Every member: constructors, then methods, then statics.
    pub fn members(&self) -> impl Iterator<Item = &FnBinding> {
        self.constructors
            .iter()
            .chain(&self.methods)
            .chain(&self.statics)
    }
}

/// One parameter of a callback-interface method: always a value type, with
/// the slots the producer fills and the consumer's trampoline receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackParamBinding {
    /// The parameter name as written in the IDL.
    pub name: String,
    /// The resolved type.
    pub ty: Ty,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// The C slots the parameter occupies. Never [`ArgPass::Callback`]; an
    /// [`ArgPass::Object`] here transfers one strong reference to the
    /// consumer.
    pub pass: ArgPass,
}

/// One method of a callback interface, lowered to its vtable entry.
///
/// The consumer implements this; the producer calls it through the vtable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackMethodBinding {
    /// The method name as written in the IDL; also the vtable field name.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// Deprecation message when the method is marked deprecated, else `None`.
    pub deprecated: Option<String>,
    /// Input parameters with their passing contracts.
    pub params: Vec<CallbackParamBinding>,
    /// The resolved return type (`None` = void).
    pub ret: Option<Ty>,
    /// How the return crosses back to the producer.
    pub ret_pass: CallbackRetPass,
    /// Which failures the method may report.
    pub error: ErrorStrategy,
    /// The vtable entry's signature: [`symbol`](AbiFn::symbol) is the
    /// method's field name, and the slots are `void* ctx` first, then every
    /// parameter's slots, then the return's out slots, then
    /// `{prefix}_error* out_err` last.
    pub abi: AbiFn,
}

/// A callback interface, fully lowered.
///
/// The C ABI sees a vtable struct ([`vtable_tag`](Self::vtable_tag)) that
/// starts with a fixed header, `uint32_t size` (the vtable's size as the
/// consumer compiled it), `uint32_t flags`, and `void (*free)(void* ctx)`,
/// followed by one function-pointer field per method in declaration order. A
/// parameter of this type lowers to two slots, `void* {name}_ctx` and
/// `const {vtable_tag}* {name}_vtable`.
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

/// One error code of an error domain, with its C constant name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorCodeBinding {
    /// The code name exactly as written in the IDL (for example
    /// `KEY_NOT_FOUND`).
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
    #[must_use]
    pub fn payload_tag(&self) -> String {
        format!("{}_payload", self.c_const)
    }
}

/// An error domain, lowered on the module that declares it.
///
/// A callable reports a domain's codes when its
/// [`ErrorStrategy`] is [`Domain`](ErrorStrategy::Domain) naming it;
/// [`Model::error_domain`] resolves the name. Domains are open: a consumer
/// maps a positive code it doesn't know to the domain's base error type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBinding {
    /// The domain name as written in the IDL (for example `KvError`).
    pub name: String,
    /// The domain's error type name with exactly one `Error` suffix (for
    /// example `KvError`, and `KitchenError` for `KitchenErrors`), from
    /// [`crate::errors::type_name`]. Backends that brand exceptions use
    /// [`crate::errors::exception_type_name`] on [`name`](Self::name).
    pub type_name: String,
    /// Dot-joined path of the declaring module (for example `kv.stats`).
    pub module: String,
    /// Underscore-joined path of the declaring module.
    pub owner_path: String,
    /// `{prefix}_{owner_path}_{name}`, the C type (`typedef int32_t`) naming
    /// the domain's code constants.
    pub c_tag: String,
    /// The domain's codes in declaration order.
    pub codes: Vec<ErrorCodeBinding>,
}

/// One module, flattened with its underscore-joined symbol path and linked
/// to its parent and children by index into [`Model::modules`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBinding {
    /// The module's position in [`Model::modules`].
    pub index: usize,
    /// The module name (its final path segment).
    pub name: String,
    /// Path segments from the root (for example `["outer", "inner"]`).
    pub segments: Vec<String>,
    /// Underscore-joined path used as the C symbol segment (for example
    /// `outer_inner`).
    pub path: String,
    /// Dot-joined path (for example `outer.inner`), the declaration path
    /// diagnostics and generated comments quote.
    pub dot_path: String,
    /// The parent module's position in [`Model::modules`], or `None` for a
    /// top-level module.
    pub parent: Option<usize>,
    /// The direct submodules' positions in [`Model::modules`], in
    /// declaration order.
    pub children: Vec<usize>,
    /// The module's doc comment, if the IDL records one.
    pub doc: Option<String>,
    /// The error domains this module declares, in declaration order.
    pub errors: Vec<ErrorBinding>,
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
            .chain(self.interfaces.iter().flat_map(InterfaceBinding::members))
    }

    /// `true` when any callable in this module is `async`.
    #[must_use]
    pub fn has_async(&self) -> bool {
        self.callables().any(FnBinding::is_async)
    }

    /// `true` when any callable in this module returns an iterator.
    #[must_use]
    pub fn has_iterators(&self) -> bool {
        self.callables().any(|f| f.iterator().is_some())
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
    /// Modules in depth-first pre-order (a parent before its children).
    pub modules: Vec<ModuleBinding>,
    /// Every user type name, mapped to its declaration.
    pub types: TypeIndex,
}

impl Model {
    /// The C symbol prefix every emitted name starts with.
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.identity.prefix
    }

    /// The top-level modules, in declaration order.
    pub fn roots(&self) -> impl Iterator<Item = &ModuleBinding> {
        self.modules.iter().filter(|m| m.parent.is_none())
    }

    /// The direct submodules of `parent`, in declaration order.
    pub fn children<'a>(
        &'a self,
        parent: &'a ModuleBinding,
    ) -> impl Iterator<Item = &'a ModuleBinding> + 'a {
        parent.children.iter().filter_map(|&i| self.modules.get(i))
    }

    /// The parent of `module`, or `None` for a top-level module.
    #[must_use]
    pub fn parent(&self, module: &ModuleBinding) -> Option<&ModuleBinding> {
        module.parent.and_then(|i| self.modules.get(i))
    }

    /// Iterate every free function across all modules, paired with its
    /// module.
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

    /// Iterate every error domain across all modules, paired with its
    /// module.
    pub fn error_domains(&self) -> impl Iterator<Item = (&ModuleBinding, &ErrorBinding)> {
        self.modules
            .iter()
            .flat_map(|m| m.errors.iter().map(move |e| (m, e)))
    }

    /// The declaration of the user type `name`, which must be one of
    /// `kinds`.
    ///
    /// # Panics
    ///
    /// Panics when `name` isn't declared as one of `kinds`, which
    /// validation rules out for every name the model carries.
    fn decl(&self, name: &str, kinds: &[TypeKind]) -> &TypeDecl {
        match self.types.get(name) {
            Some(d) if kinds.contains(&d.kind) => d,
            Some(d) => panic!("type '{name}' is a {:?}, not one of {kinds:?}", d.kind),
            None => panic!("type '{name}' is not declared"),
        }
    }

    /// The module declaring the user type or error domain `name`.
    ///
    /// # Panics
    ///
    /// Panics when `name` isn't declared in this model (a name the model
    /// itself carries always is; a foreign record has no owner).
    #[must_use]
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
    #[must_use]
    pub fn interface(&self, name: &str) -> &InterfaceBinding {
        let d = self.decl(name, &[TypeKind::Interface]);
        &self.modules[d.module].interfaces[d.index]
    }

    /// The callback interface named `name`, as carried by
    /// [`ParamTy::Callback`].
    ///
    /// # Panics
    ///
    /// Panics when no callback interface has that name.
    #[must_use]
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
    #[must_use]
    pub fn enumeration(&self, name: &str) -> &EnumBinding {
        let d = self.decl(name, &[TypeKind::Enum, TypeKind::RichEnum]);
        &self.modules[d.module].enums[d.index]
    }

    /// The record named `name`, as carried by [`Ty::Record`].
    ///
    /// # Panics
    ///
    /// Panics when no record in this model has that name (including a
    /// foreign record, which [`TypeIndex::is_foreign`] identifies).
    #[must_use]
    pub fn record(&self, name: &str) -> &StructBinding {
        let d = self.decl(name, &[TypeKind::Record]);
        &self.modules[d.module].structs[d.index]
    }

    /// The error domain named `name`, as carried by
    /// [`ErrorStrategy::Domain`]. Domain names are global, so the domain may
    /// be declared in any module.
    ///
    /// # Panics
    ///
    /// Panics when no error domain has that name; validation guarantees one
    /// for every name an [`ErrorStrategy`] in the model carries.
    #[must_use]
    pub fn error_domain(&self, name: &str) -> &ErrorBinding {
        let d = self.decl(name, &[TypeKind::ErrorDomain]);
        &self.modules[d.module].errors[d.index]
    }

    /// Every C identifier the library's ABI declares, with the declaration
    /// that owns it, in declaration order. Validation rejects an API in
    /// which two entries share an identifier.
    ///
    /// Covers the runtime surface ([`RUNTIME_SYMBOLS`]), each top-level
    /// module's contract table function and the C header's checker for it,
    /// every callable's symbols (entry point or launcher, async completion
    /// type, iterator type, `_next`, and `_destroy`), interface types with
    /// their `_clone` and `_destroy`, enum types and constants (and a rich
    /// enum's tag type), error-domain types and code constants,
    /// callback-interface vtable types, and what the C value-buffer helper
    /// header declares per user type: the struct of every record, rich enum,
    /// and error code with fields ([`ErrorCodeBinding::payload_tag`]), each
    /// with its [`VALUE_CODECS`]. (The header's list, map, and optional
    /// shapes fall in [`RESERVED_SYMBOL_FAMILIES`].)
    #[must_use]
    pub fn c_symbols(&self) -> Vec<Symbol> {
        symbols::collect(self)
    }

    /// `true` when any callable anywhere in the API is `async`.
    #[must_use]
    pub fn has_async(&self) -> bool {
        self.modules.iter().any(ModuleBinding::has_async)
    }

    /// `true` when any callable anywhere in the API returns an iterator.
    #[must_use]
    pub fn has_iterators(&self) -> bool {
        self.modules.iter().any(ModuleBinding::has_iterators)
    }

    /// `true` when the API declares any callback interface.
    #[must_use]
    pub fn has_callback_interfaces(&self) -> bool {
        self.modules
            .iter()
            .any(|m| !m.callback_interfaces.is_empty())
    }

    /// `true` when the API declares any interface.
    #[must_use]
    pub fn has_interfaces(&self) -> bool {
        self.modules.iter().any(|m| !m.interfaces.is_empty())
    }

    /// `true` when any value anywhere in the API crosses the ABI as a value
    /// buffer (records, rich enums, error payloads, and optionals, lists,
    /// and maps outside the OptDirect and Slice families).
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
        self.modules.iter().any(|m| {
            !m.structs.is_empty()
                || m.enums.iter().any(|e| e.rich)
                || m.errors
                    .iter()
                    .any(|e| e.codes.iter().any(|c| !c.fields.is_empty()))
                || m.callables().any(callable)
                || m.callback_interfaces
                    .iter()
                    .any(|c| c.methods.iter().any(method))
        })
    }
}
