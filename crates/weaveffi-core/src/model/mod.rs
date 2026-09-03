//! The **binding model**: a normalized, fully-lowered view of a
//! [`ResolvedApi`] that every language backend consumes.
//!
//! [`BindingModel::build`] walks the IR exactly once and produces a flat list
//! of [`ModuleBinding`]s in which:
//!
//! * every **type** is a resolved [`Ty`] (its kind and owning module known,
//!   no unresolved names), so a backend dispatches on [`Ty::family`] and
//!   [`Ty::wire`] instead of re-deriving what a name means;
//! * every emitted **C symbol name** is precomputed once, so all backends
//!   agree by construction and a non-default prefix is honored everywhere; and
//! * every function, callback, and interface member is paired with its
//!   lowered [`AbiFn`] signature (built from [`crate::abi`]), so no backend
//!   re-derives parameter arity, ordering, or `out_*`/`out_err` placement.
//!
//! A backend reads the *idiomatic* shape from the retained [`Ty`]s
//! (`param.ty`, `field.ty`, ...) and the *native* shape from the [`AbiFn`]s,
//! then writes only the marshalling that bridges the two (see
//! [`crate::plan`]) in its own idioms. The hard, drift-prone facts live here;
//! only language syntax lives in the backends.

mod ty;

pub use ty::{Family, Prim, Ty, WireType};

use heck::ToUpperCamelCase;
use weaveffi_ir::ir::{
    CallbackDef, EnumDef, ErrorDomain, Function, InterfaceDef, Module, StructDef, StructField,
    TypeRef,
};

use crate::abi::{
    async_callback_params, async_input_params, context_param, error_out_param, lower_param,
    lower_return, sync_signature, AbiParam, CType, ConstPos,
};
use crate::resolved::ResolvedApi;

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

/// One IR parameter, retained with its lowered ABI slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamBinding {
    /// The parameter name as written in the IDL.
    pub name: String,
    /// The resolved type a backend renders the parameter as.
    pub ty: Ty,
    /// Whether the parameter is mutable (drops the `const` on its pointer slots).
    pub mutable: bool,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// The ordered C ABI slots this single parameter expands into.
    pub abi: Vec<AbiParam>,
}

impl ParamBinding {
    /// Lower one parameter declared in the module whose underscore-joined C
    /// path is `module`.
    pub fn new(
        name: impl Into<String>,
        ty: Ty,
        mutable: bool,
        doc: Option<String>,
        module: &str,
    ) -> Self {
        let name = name.into();
        let abi = lower_param(&name, &ty, module, mutable);
        Self {
            name,
            ty,
            mutable,
            doc,
            abi,
        }
    }
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
    /// The version the function was introduced, when the IDL records one.
    pub since: Option<String>,
    /// Whether an async function accepts a trailing `cancel_token` slot.
    pub cancellable: bool,
    /// Whether the function is `async` (lowered as a callback-completed launcher).
    pub is_async: bool,
    /// Whether the function reports typed domain errors. A throwing function
    /// surfaces as `throws`/`raises` in idiomatic wrappers using the module's
    /// [`ErrorBinding`]; a non-throwing function has a plain signature, and a
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

/// A callback function-pointer typedef declared at module scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackBinding {
    /// The callback name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// `{prefix}_{module_path}_{name}_fn`.
    pub c_fn_type: String,
    /// Parameters of the callback (without the trailing context).
    pub params: Vec<ParamBinding>,
    /// The full ABI slot list, including the trailing `void* context`.
    pub abi_params: Vec<AbiParam>,
}

/// A listener: a register/unregister pair bound to a callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerBinding {
    /// The listener name as written in the IDL.
    pub name: String,
    /// Optional doc comment carried from the IDL.
    pub doc: Option<String>,
    /// The callback this listener fires (name within the same module).
    pub event_callback: String,
    /// The referenced callback's `_fn` typedef name.
    pub callback_c_fn_type: String,
    /// `uint64_t {prefix}_{path}_register_{name}({cb}_fn callback, void* context)`.
    pub register_symbol: String,
    /// `void {prefix}_{path}_unregister_{name}(uint64_t id)`.
    pub unregister_symbol: String,
}

/// An interface (opaque object type), fully lowered.
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
    /// `void {c_tag}_destroy({c_tag}* self)`: releases the object reference.
    pub destroy_symbol: String,
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

/// The error domain in effect for a module: its own `errors:` block, or the
/// nearest ancestor's when the module declares none.
///
/// Every throwing function in the module reports codes from this domain.
/// Backends emit one error type per *declaring* module
/// ([`declared_here`](Self::declared_here) is `true`) and reference the
/// ancestor's type from inheriting submodules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBinding {
    /// The domain name as written in the IDL (e.g. `KvError`).
    pub name: String,
    /// PascalCase type name with exactly one `Error` suffix (e.g. `KvError`);
    /// backends that brand exceptions swap the suffix via
    /// [`crate::errors::type_name`].
    pub type_name: String,
    /// Underscore-joined path of the module that *declares* the domain.
    pub owner_path: String,
    /// `true` when this module declares the domain itself; `false` when it
    /// inherits the domain from an ancestor module.
    pub declared_here: bool,
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
    /// Dot-joined path used to qualify cross-module type names
    /// (e.g. `outer.inner`).
    pub dot_path: String,
    /// The module's doc comment when the IDL records one, else the doc of the
    /// first documented function in the module.
    pub doc: Option<String>,
    /// The error domain in effect for this module's throwing functions:
    /// its own domain, the nearest ancestor's, or `None` when no domain is in
    /// scope (in which case validation has rejected any `throws` here).
    pub error: Option<ErrorBinding>,
    /// Enums declared in this module, fully lowered.
    pub enums: Vec<EnumBinding>,
    /// Structs declared in this module, fully lowered.
    pub structs: Vec<StructBinding>,
    /// Interfaces declared in this module, fully lowered.
    pub interfaces: Vec<InterfaceBinding>,
    /// Callback typedefs declared in this module.
    pub callbacks: Vec<CallbackBinding>,
    /// Listeners declared in this module.
    pub listeners: Vec<ListenerBinding>,
    /// Functions declared in this module, fully lowered.
    pub functions: Vec<FnBinding>,
}

impl ModuleBinding {
    /// Find a callback declared in this module by name.
    pub fn callback(&self, name: &str) -> Option<&CallbackBinding> {
        self.callbacks.iter().find(|c| c.name == name)
    }

    /// True when this module declares no API surface at all.
    pub fn is_empty(&self) -> bool {
        self.enums.is_empty()
            && self.structs.is_empty()
            && self.interfaces.is_empty()
            && self.callbacks.is_empty()
            && self.listeners.is_empty()
            && self.functions.is_empty()
            && !self.declares_error()
    }

    /// True when this module declares its own error domain (as opposed to
    /// inheriting one from an ancestor).
    pub fn declares_error(&self) -> bool {
        self.error.as_ref().is_some_and(|e| e.declared_here)
    }

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
        self.callables().any(|f| f.is_async)
    }

    /// `true` when any callable in this module returns an iterator.
    pub fn has_iterators(&self) -> bool {
        self.callables()
            .any(|f| matches!(f.shape, CallShape::Iterator(_)))
    }
}

/// The whole API, normalized and lowered for code generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingModel {
    /// The C symbol prefix every emitted name is built from.
    pub prefix: String,
    /// The IR schema version of the source `Api`.
    pub version: String,
    /// Modules in depth-first pre-order, each carrying its joined symbol path.
    pub modules: Vec<ModuleBinding>,
}

impl BindingModel {
    /// Build the model from a [`ResolvedApi`], using `prefix` for every C
    /// symbol name. `prefix` is the single global ABI prefix (default
    /// `"weaveffi"`); passing the same prefix to every backend is what keeps
    /// the producer header and all consumers calling identical symbols.
    pub fn build(api: &ResolvedApi, prefix: &str) -> Self {
        let mut modules = Vec::new();
        let lowerer = Lowerer { api, prefix };
        for m in &api.modules {
            lowerer.module(m, &[], None, &mut modules);
        }
        Self {
            prefix: prefix.to_string(),
            version: api.version.clone(),
            modules,
        }
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

    /// `true` when any callable anywhere in the API is `async`.
    pub fn has_async(&self) -> bool {
        self.modules.iter().any(ModuleBinding::has_async)
    }

    /// `true` when any callable anywhere in the API returns an iterator.
    pub fn has_iterators(&self) -> bool {
        self.modules.iter().any(ModuleBinding::has_iterators)
    }

    /// `true` when the API declares any listener.
    pub fn has_listeners(&self) -> bool {
        self.modules.iter().any(|m| !m.listeners.is_empty())
    }

    /// `true` when any type anywhere in the API crosses the ABI as a value
    /// buffer (records, rich enums, optionals, lists, maps, error payloads).
    pub fn has_buffers(&self) -> bool {
        self.modules.iter().any(|m| {
            !m.structs.is_empty()
                || m.enums.iter().any(|e| e.rich)
                || m.error
                    .as_ref()
                    .is_some_and(|e| e.codes.iter().any(|c| !c.fields.is_empty()))
        }) || self.any_type(&|t| t.is_buffered())
    }

    /// Visit every boundary-crossing type in the API (callable and callback
    /// parameters and returns, iterator elements, record, variant, and error
    /// payload fields), recursing into composites, and return whether any
    /// satisfies `pred`. Backends use this to decide which runtime helpers a
    /// given API actually needs.
    pub fn any_type(&self, pred: &dyn Fn(&Ty) -> bool) -> bool {
        self.modules.iter().any(|m| {
            m.callables().any(|f| {
                f.params.iter().any(|p| p.ty.any(pred))
                    || f.ret.as_ref().is_some_and(|r| r.any(pred))
            }) || m
                .callbacks
                .iter()
                .any(|c| c.params.iter().any(|p| p.ty.any(pred)))
                || m.structs
                    .iter()
                    .any(|s| s.fields.iter().any(|f| f.ty.any(pred)))
                || m.enums.iter().any(|e| {
                    e.variants
                        .iter()
                        .any(|v| v.fields.iter().any(|f| f.ty.any(pred)))
                })
                || m.error.as_ref().is_some_and(|e| {
                    e.declared_here
                        && e.codes
                            .iter()
                            .any(|c| c.fields.iter().any(|f| f.ty.any(pred)))
                })
        })
    }
}

/// The per-build lowering context: the resolver and the symbol prefix.
struct Lowerer<'a> {
    api: &'a ResolvedApi,
    prefix: &'a str,
}

/// The per-module lowering context: where we are in the tree.
struct Scope<'a> {
    /// Underscore-joined C path (`outer_inner`).
    path: &'a str,
    /// Dot-joined qualification path (`outer.inner`).
    dot_path: &'a str,
}

impl Lowerer<'_> {
    fn ty(&self, ty: &TypeRef, scope: &Scope<'_>) -> Ty {
        self.api.resolve(ty, scope.dot_path)
    }

    /// Recursively lower `module` and its descendants into the flat `out`
    /// list, pre-order (parent before children) so symbol declarations precede
    /// uses. `inherited_error` is the nearest ancestor's error domain,
    /// threaded down so every module knows which domain its throwing
    /// functions report.
    fn module(
        &self,
        module: &Module,
        parent: &[String],
        inherited_error: Option<&ErrorBinding>,
        out: &mut Vec<ModuleBinding>,
    ) {
        let mut segments = parent.to_vec();
        segments.push(module.name.clone());
        let path = segments.join("_");
        let dot_path = segments.join(".");
        let scope = Scope {
            path: &path,
            dot_path: &dot_path,
        };
        let prefix = self.prefix;

        let error = match &module.errors {
            Some(domain) => Some(self.error_domain(domain, &scope)),
            None => inherited_error.cloned().map(|mut e| {
                e.declared_here = false;
                e
            }),
        };

        let enums = module
            .enums
            .iter()
            .map(|e| self.enum_def(e, &scope))
            .collect();
        let structs = module
            .structs
            .iter()
            .map(|s| self.struct_def(s, &scope))
            .collect();
        let interfaces = module
            .interfaces
            .iter()
            .map(|i| self.interface(i, &scope))
            .collect();
        let callbacks = module
            .callbacks
            .iter()
            .map(|c| self.callback(c, &scope))
            .collect();
        let listeners = module
            .listeners
            .iter()
            .map(|l| ListenerBinding {
                name: l.name.clone(),
                doc: l.doc.clone(),
                event_callback: l.event_callback.clone(),
                callback_c_fn_type: format!("{prefix}_{path}_{}_fn", l.event_callback),
                register_symbol: format!("{prefix}_{path}_register_{}", l.name),
                unregister_symbol: format!("{prefix}_{path}_unregister_{}", l.name),
            })
            .collect();
        let functions = module
            .functions
            .iter()
            .map(|f| {
                let c_base = format!("{prefix}_{path}_{}", f.name);
                self.callable(f, &scope, &c_base, None)
            })
            .collect();

        let doc = module
            .doc
            .clone()
            .or_else(|| module.functions.iter().find_map(|f| f.doc.clone()));

        out.push(ModuleBinding {
            name: module.name.clone(),
            segments: segments.clone(),
            path: path.clone(),
            dot_path: dot_path.clone(),
            doc,
            error: error.clone(),
            enums,
            structs,
            interfaces,
            callbacks,
            listeners,
            functions,
        });

        for child in &module.modules {
            self.module(child, &segments, error.as_ref(), out);
        }
    }

    fn error_domain(&self, domain: &ErrorDomain, scope: &Scope<'_>) -> ErrorBinding {
        let c_tag = format!("{}_{}_{}", self.prefix, scope.path, domain.name);
        ErrorBinding {
            name: domain.name.clone(),
            type_name: crate::errors::type_name(&domain.name, "Error"),
            owner_path: scope.path.to_string(),
            declared_here: true,
            c_tag: c_tag.clone(),
            codes: domain
                .codes
                .iter()
                .map(|c| ErrorCodeBinding {
                    name: c.name.clone(),
                    value: c.code,
                    message: c.message.clone(),
                    doc: c.doc.clone(),
                    c_const: format!("{c_tag}_{}", c.name),
                    fields: self.fields(&c.fields, scope),
                })
                .collect(),
        }
    }

    fn fields(&self, fields: &[StructField], scope: &Scope<'_>) -> Vec<FieldBinding> {
        fields
            .iter()
            .map(|f| FieldBinding {
                name: f.name.clone(),
                doc: f.doc.clone(),
                ty: self.ty(&f.ty, scope),
            })
            .collect()
    }

    fn struct_def(&self, s: &StructDef, scope: &Scope<'_>) -> StructBinding {
        StructBinding {
            name: s.name.clone(),
            doc: s.doc.clone(),
            deprecated: s.deprecated.clone(),
            fields: self.fields(&s.fields, scope),
        }
    }

    fn enum_def(&self, e: &EnumDef, scope: &Scope<'_>) -> EnumBinding {
        let c_tag = format!("{}_{}_{}", self.prefix, scope.path, e.name);
        let variants = e
            .variants
            .iter()
            .map(|v| EnumVariantBinding {
                name: v.name.clone(),
                value: v.value,
                doc: v.doc.clone(),
                c_const: format!("{c_tag}_{}", v.name),
                fields: self.fields(&v.fields, scope),
            })
            .collect();
        EnumBinding {
            name: e.name.clone(),
            doc: e.doc.clone(),
            deprecated: e.deprecated.clone(),
            c_tag,
            variants,
            rich: e.is_rich(),
        }
    }

    fn params(&self, params: &[weaveffi_ir::ir::Param], scope: &Scope<'_>) -> Vec<ParamBinding> {
        params
            .iter()
            .map(|p| {
                ParamBinding::new(
                    &p.name,
                    self.ty(&p.ty, scope),
                    p.mutable,
                    p.doc.clone(),
                    scope.path,
                )
            })
            .collect()
    }

    fn callback(&self, c: &CallbackDef, scope: &Scope<'_>) -> CallbackBinding {
        let params = self.params(&c.params, scope);
        let mut abi_params: Vec<AbiParam> = params.iter().flat_map(|p| p.abi.clone()).collect();
        abi_params.push(context_param());
        CallbackBinding {
            name: c.name.clone(),
            doc: c.doc.clone(),
            c_fn_type: format!("{}_{}_{}_fn", self.prefix, scope.path, c.name),
            params,
            abi_params,
        }
    }

    /// Lower an interface: constructors become statics returning the
    /// interface, methods gain the implicit `self` slot, and all member
    /// symbols hang off the interface's `c_tag`.
    fn interface(&self, iface: &InterfaceDef, scope: &Scope<'_>) -> InterfaceBinding {
        let c_tag = format!("{}_{}_{}", self.prefix, scope.path, iface.name);
        let self_slot = AbiParam::new(
            "self",
            CType::Ptr {
                konst: ConstPos::West,
                pointee: Box::new(CType::StructTag {
                    module: scope.path.to_string(),
                    name: iface.name.clone(),
                }),
            },
        );
        let member = |name: &str| format!("{c_tag}_{name}");
        let constructors = iface
            .constructors
            .iter()
            .map(|c| {
                // A constructor yields a new owned reference to the interface,
                // exactly like a static returning it.
                let mut f = c.clone();
                f.returns = Some(TypeRef::Named(iface.name.clone()));
                self.callable(&f, scope, &member(&c.name), None)
            })
            .collect();
        let methods = iface
            .methods
            .iter()
            .map(|m| self.callable(m, scope, &member(&m.name), Some(self_slot.clone())))
            .collect();
        let statics = iface
            .statics
            .iter()
            .map(|s| self.callable(s, scope, &member(&s.name), None))
            .collect();
        InterfaceBinding {
            name: iface.name.clone(),
            doc: iface.doc.clone(),
            deprecated: iface.deprecated.clone(),
            c_tag: c_tag.clone(),
            constructors,
            methods,
            statics,
            destroy_symbol: format!("{c_tag}_destroy"),
        }
    }

    /// Lower one callable (free function or interface member) whose full base
    /// C symbol is `c_base`. When `self_slot` is given (an instance method),
    /// it is prepended to every ABI signature but never appears in the
    /// retained [`ParamBinding`] list.
    fn callable(
        &self,
        f: &Function,
        scope: &Scope<'_>,
        c_base: &str,
        self_slot: Option<AbiParam>,
    ) -> FnBinding {
        let prefix = self.prefix;
        let path = scope.path;
        let params = self.params(&f.params, scope);
        let ret = f.returns.as_ref().map(|r| self.ty(r, scope));
        // The prefix-stripped spelling used for `CType::Named` cores (which
        // render as `{prefix}_{core}`), e.g. `kv_Store_scan` from
        // `weaveffi_kv_Store_scan`.
        let core_base = c_base
            .strip_prefix(&format!("{prefix}_"))
            .expect("c_base always starts with the symbol prefix")
            .to_string();
        let with_self = |mut params: Vec<AbiParam>| {
            if let Some(s) = &self_slot {
                params.insert(0, s.clone());
            }
            params
        };

        let shape = if let Some(elem) = ret.as_ref().and_then(Ty::iterator_elem) {
            let pascal = f.name.to_upper_camel_case();
            // `{owner}_{Pascal}Iterator`, where owner is the module path for a
            // free function or `{module path}_{Interface}` for a method.
            let owner = &core_base[..core_base.len() - f.name.len() - 1];
            let iter_core = format!("{owner}_{pascal}Iterator");
            let iter_tag = format!("{prefix}_{iter_core}");

            let mut launch_params: Vec<AbiParam> =
                params.iter().flat_map(|p| p.abi.iter().cloned()).collect();
            launch_params.push(error_out_param());
            let launch = AbiFn {
                symbol: c_base.to_string(),
                params: with_self(launch_params),
                ret: CType::ptr(CType::Named(iter_core.clone())),
            };

            let item = lower_return(elem, path);
            let mut next_params = vec![
                AbiParam::new("iter", CType::ptr(CType::Named(iter_core.clone()))),
                AbiParam::new("out_item", CType::ptr(item.ret)),
            ];
            next_params.extend(item.out_params);
            next_params.push(error_out_param());
            let next = AbiFn {
                symbol: format!("{iter_tag}_next"),
                params: next_params,
                ret: CType::Int32,
            };

            CallShape::Iterator(IteratorBinding {
                elem: elem.clone(),
                iter_tag: iter_tag.clone(),
                launch,
                next,
                destroy_symbol: format!("{iter_tag}_destroy"),
            })
        } else if f.r#async {
            let callback_type = format!("{c_base}_callback");
            let mut launch_params = async_input_params(&params, f.cancellable);
            launch_params.push(AbiParam::new(
                "callback",
                CType::Named(format!("{core_base}_callback")),
            ));
            launch_params.push(context_param());
            let launch = AbiFn {
                symbol: format!("{c_base}_async"),
                params: with_self(launch_params),
                ret: CType::Void,
            };
            CallShape::Async(AsyncBinding {
                launch,
                callback_type,
                callback_params: async_callback_params(ret.as_ref(), path),
            })
        } else {
            let sig = sync_signature(&params, ret.as_ref(), path);
            CallShape::Sync(AbiFn {
                symbol: c_base.to_string(),
                params: with_self(sig.params),
                ret: sig.ret,
            })
        };

        FnBinding {
            name: f.name.clone(),
            doc: f.doc.clone(),
            deprecated: f.deprecated.clone(),
            since: f.since.clone(),
            cancellable: f.cancellable,
            is_async: f.r#async,
            throws: f.throws,
            has_self: self_slot.is_some(),
            params,
            ret,
            c_base: c_base.to_string(),
            shape,
        }
    }
}

/// The element C type of an iterator's `out_item` slot (the pointee of
/// `T* out_item`). Exposed for backends that materialize iterator results.
pub fn iterator_item_ctype(elem: &Ty, module: &str) -> CType {
    lower_return(elem, module).ret
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{
        Api, CallbackDef, EnumDef, EnumVariant, Function, InterfaceDef, ListenerDef, Module, Param,
        StructDef, StructField,
    };

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            mutable: false,
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
            since: None,
        }
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            doc: None,
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api(modules: Vec<Module>) -> ResolvedApi {
        ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules,
        })
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
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
            )],
            ..module("math")
        };
        let model = BindingModel::build(&api(vec![m]), "weaveffi");
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

        let model = BindingModel::build(&api(vec![module("net")]), "acme");
        assert_eq!(model.prefix, "acme");
    }

    #[test]
    fn async_function_has_launch_and_callback() {
        let m = Module {
            functions: vec![Function {
                cancellable: true,
                r#async: true,
                ..func(
                    "fetch",
                    vec![param("id", TypeRef::I64)],
                    Some(TypeRef::StringUtf8),
                )
            }],
            ..module("net")
        };
        let model = BindingModel::build(&api(vec![m]), "weaveffi");
        let CallShape::Async(a) = &model.modules[0].functions[0].shape else {
            panic!("expected async")
        };
        assert_eq!(a.launch.symbol, "weaveffi_net_fetch_async");
        assert_eq!(a.callback_type, "weaveffi_net_fetch_callback");
        let names: Vec<&str> = a.launch.params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["id", "cancel_token", "callback", "context"]);
        assert_eq!(a.callback_params[0].name, "context");
        assert_eq!(a.callback_params[1].name, "err");
        assert_eq!(a.callback_params[2].name, "result");
        assert!(model.has_async());
    }

    #[test]
    fn iterator_function_has_next_and_destroy() {
        let m = Module {
            functions: vec![func(
                "get_messages",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            )],
            ..module("events")
        };
        let model = BindingModel::build(&api(vec![m]), "weaveffi");
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
        assert_eq!(it.elem, Ty::StringUtf8);
        assert_eq!(it.next.ret, CType::Int32);
        assert_eq!(it.next.params[1].ty.render_c("weaveffi"), "const char**");
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
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "status".into(),
                        ty: TypeRef::Named("Status".into()),
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
        let model = BindingModel::build(&api(vec![shared, other]), "weaveffi");
        let contacts = &model.modules[0];
        let s = &contacts.structs[0];
        assert_eq!(s.deprecated.as_deref(), Some("use Person"));
        assert_eq!(s.fields[1].ty, Ty::Enum("Status".into()));
        assert_eq!(contacts.enums[0].c_tag, "weaveffi_contacts_Status");
        assert_eq!(
            contacts.enums[0].variants[0].c_const,
            "weaveffi_contacts_Status_Ok"
        );

        let iface = &contacts.interfaces[0];
        assert_eq!(iface.c_tag, "weaveffi_contacts_Store");
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
        assert_eq!(f.params[0].ty, Ty::Interface("contacts.Store".into()));
        assert_eq!(f.ret, Some(Ty::Enum("contacts.Status".into())));
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
    }

    #[test]
    fn callbacks_and_listeners_are_linked() {
        let m = Module {
            callbacks: vec![CallbackDef {
                name: "on_message".into(),
                params: vec![param("text", TypeRef::StringUtf8)],
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "messages".into(),
                event_callback: "on_message".into(),
                doc: None,
            }],
            ..module("events")
        };
        let model = BindingModel::build(&api(vec![m]), "weaveffi");
        let mb = &model.modules[0];
        let cb = &mb.callbacks[0];
        assert_eq!(cb.c_fn_type, "weaveffi_events_on_message_fn");
        assert_eq!(cb.abi_params.last().unwrap().name, "context");
        let l = &mb.listeners[0];
        assert_eq!(l.register_symbol, "weaveffi_events_register_messages");
        assert_eq!(l.unregister_symbol, "weaveffi_events_unregister_messages");
        assert_eq!(l.callback_c_fn_type, "weaveffi_events_on_message_fn");
        assert!(mb.callback("on_message").is_some());
        assert!(model.has_listeners());
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
        let model = BindingModel::build(&api(vec![outer]), "weaveffi");
        let paths: Vec<&str> = model.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["outer", "outer_inner"]);
        assert_eq!(model.modules[1].dot_path, "outer.inner");
        assert_eq!(
            model.modules[1].functions[0].c_base,
            "weaveffi_outer_inner_leaf_fn"
        );
        assert_eq!(model.modules[0].doc.as_deref(), Some("Outer module."));
        assert_eq!(model.modules[1].doc.as_deref(), Some("Leaf."));
    }
}
