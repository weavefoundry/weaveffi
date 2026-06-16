//! The **binding model**: a normalized, fully-lowered view of an [`Api`] that
//! every language backend consumes.
//!
//! Before this module existed, each of the eleven generators re-walked the IR,
//! re-derived the C ABI calling convention, and re-invented every emitted C
//! symbol name. They drifted: iterators were lowered as lists in some targets,
//! listeners were emitted only by the C header, and a custom `c_prefix` reached
//! only the C and C++ outputs while the other nine hard-coded `weaveffi_`.
//!
//! [`BindingModel::build`] walks the IR exactly once and produces a flat list
//! of [`ModuleBinding`]s in which:
//!
//! * every emitted **C symbol name** is precomputed once (so all backends agree
//!   by construction, and a non-default prefix is honored everywhere); and
//! * every function/struct/callback is paired with its lowered [`AbiFn`]
//!   signature (built from [`crate::abi`]), so no backend re-derives parameter
//!   arity, ordering, or `out_*`/`out_err` placement.
//!
//! A backend reads the *idiomatic* shape from the retained [`TypeRef`]s
//! (`param.ty`, `field.ty`, …) and the *native* shape from the [`AbiFn`]s, then
//! writes only the marshalling that bridges the two in its own idioms. The hard,
//! drift-prone facts live here; only language syntax lives in the backends.

use heck::ToUpperCamelCase;
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, Function, ListenerDef, Module, StructDef, TypeRef,
};

use crate::abi::{
    self, async_callback_params, async_input_params, context_param, error_out_param, lower_param,
    lower_return, sync_signature, AbiParam, CType,
};

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
    pub elem: TypeRef,
    /// The opaque iterator tag (`{prefix}_{path}_{Pascal}Iterator`).
    pub iter_tag: String,
    /// The launcher returning `{iter_tag}*`.
    pub launch: AbiFn,
    /// `int32_t {iter_tag}_next({iter_tag}* iter, T* out_item, …, error* out_err)`.
    pub next: AbiFn,
    /// `void {iter_tag}_destroy({iter_tag}* iter)`.
    pub destroy_symbol: String,
}

/// One IR parameter, retained with its lowered ABI slots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamBinding {
    pub name: String,
    pub ty: TypeRef,
    pub mutable: bool,
    pub doc: Option<String>,
    /// The ordered C ABI slots this single parameter expands into.
    pub abi: Vec<AbiParam>,
}

/// A function, fully lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnBinding {
    pub name: String,
    pub doc: Option<String>,
    pub deprecated: Option<String>,
    pub since: Option<String>,
    pub cancellable: bool,
    pub is_async: bool,
    /// IR input parameters with their lowered slots.
    pub params: Vec<ParamBinding>,
    /// The IR return type (`None` = void). For an iterator function this is the
    /// `iter<T>` type itself; the element `T` also lives in [`IteratorBinding`].
    pub ret: Option<TypeRef>,
    /// Base C symbol (`{prefix}_{module_path}_{name}`) before any
    /// `_async`/iterator suffixing.
    pub c_base: String,
    /// The call shape (sync / async / iterator).
    pub shape: CallShape,
}

/// A struct field, retained with its getter symbol and lowered return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldBinding {
    pub name: String,
    pub doc: Option<String>,
    pub ty: TypeRef,
    /// `{c_tag}_get_{field}`. Receiver is an implicit `const {c_tag}* ptr`; any
    /// `out_*` slots are in [`getter_out_params`](Self::getter_out_params).
    pub getter_symbol: String,
    /// The C return type of the getter.
    pub getter_ret: CType,
    /// Trailing `out_*` slots of the getter (e.g. `size_t* out_len` for bytes).
    pub getter_out_params: Vec<AbiParam>,
    /// The ABI slots this field expands into when passed *in* (struct create,
    /// builder setter).
    pub value_params: Vec<AbiParam>,
}

/// The fluent builder lowered for a struct that opted in with `builder: true`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuilderBinding {
    /// `{c_tag}Builder`.
    pub builder_tag: String,
    /// `{c_tag}_Builder_new`.
    pub new_symbol: String,
    /// `{c_tag}_Builder_build` (carries a trailing `out_err`).
    pub build_symbol: String,
    /// `{c_tag}_Builder_destroy`.
    pub destroy_symbol: String,
    /// One `(field_name, setter_symbol)` per field; the value slots are the
    /// field's [`FieldBinding::value_params`].
    pub setters: Vec<(String, String)>,
}

/// A struct, fully lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructBinding {
    pub name: String,
    pub doc: Option<String>,
    /// `{prefix}_{module_path}_{name}` — the opaque tag.
    pub c_tag: String,
    pub fields: Vec<FieldBinding>,
    /// `{c_tag}_create(<field slots>, error* out_err) -> {c_tag}*`.
    pub create: AbiFn,
    /// `{c_tag}_destroy`.
    pub destroy_symbol: String,
    /// Present when `builder: true`.
    pub builder: Option<BuilderBinding>,
}

/// An enum, fully lowered.
///
/// A *C-style* enum (every variant a bare discriminant) carries only
/// [`variants`](Self::variants) and crosses the ABI by value as an integer. An
/// *algebraic* (sum-type) enum — at least one variant with associated data —
/// additionally carries [`rich`](Self::rich) and crosses the ABI as an opaque
/// object pointer (tag getter + per-variant constructors and field getters +
/// destructor), exactly like a struct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumBinding {
    pub name: String,
    pub doc: Option<String>,
    /// `{prefix}_{module_path}_{name}`.
    pub c_tag: String,
    /// Every variant's discriminant name/value, in declaration order. Present
    /// for both kinds (the discriminant of a rich enum is its tag value).
    pub variants: Vec<EnumVariantBinding>,
    /// `Some` iff this is a rich (algebraic) enum.
    pub rich: Option<RichEnumBinding>,
}

impl EnumBinding {
    /// `true` when this is a rich (algebraic) sum-type enum.
    pub fn is_rich(&self) -> bool {
        self.rich.is_some()
    }
}

/// A single enum variant with its precomputed C constant name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariantBinding {
    pub name: String,
    pub value: i32,
    pub doc: Option<String>,
    /// `{enum_c_tag}_{variant}`.
    pub c_const: String,
}

/// The opaque-object surface of a rich (algebraic) enum: how its tag is read,
/// how each variant is constructed and projected, and how the object is freed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichEnumBinding {
    /// `int32_t {tag_symbol}(const {c_tag}* self)` — returns the active
    /// variant's discriminant (matching the per-variant
    /// [`c_const`](EnumVariantBinding::c_const) values).
    pub tag_symbol: String,
    /// `void {destroy_symbol}({c_tag}* self)`.
    pub destroy_symbol: String,
    /// Per-variant constructors and field getters, in declaration order
    /// (parallel to [`EnumBinding::variants`]).
    pub variants: Vec<RichVariantBinding>,
}

/// One variant of a rich enum: its constructor and the getters for its
/// associated data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RichVariantBinding {
    pub name: String,
    pub doc: Option<String>,
    /// The variant's discriminant value (matches the tag getter's result).
    pub value: i32,
    /// `{enum_c_tag}_{variant}` — the discriminant constant.
    pub c_const: String,
    /// `{c_tag}_{variant}_new(<field slots>, error* out_err) -> {c_tag}*`.
    /// A unit variant's constructor takes only `out_err`.
    pub create: AbiFn,
    /// Associated data. Each field's getter is `{c_tag}_{variant}_get_{field}`
    /// with an implicit leading `const {c_tag}* self`; empty for a unit variant.
    pub fields: Vec<FieldBinding>,
}

/// A callback function-pointer typedef declared at module scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackBinding {
    pub name: String,
    pub doc: Option<String>,
    /// `{prefix}_{module_path}_{name}_fn`.
    pub c_fn_type: String,
    /// IR parameters of the callback (without the trailing context).
    pub params: Vec<ParamBinding>,
    /// The full ABI slot list, including the trailing `void* context`.
    pub abi_params: Vec<AbiParam>,
}

/// A listener: a register/unregister pair bound to a callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListenerBinding {
    pub name: String,
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

/// One module, flattened with its underscore-joined symbol path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleBinding {
    pub name: String,
    /// Path segments from the root (e.g. `["outer", "inner"]`).
    pub segments: Vec<String>,
    /// Underscore-joined path used as the C symbol segment (e.g. `outer_inner`).
    pub path: String,
    pub doc: Option<String>,
    pub enums: Vec<EnumBinding>,
    pub structs: Vec<StructBinding>,
    pub callbacks: Vec<CallbackBinding>,
    pub listeners: Vec<ListenerBinding>,
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
            && self.callbacks.is_empty()
            && self.listeners.is_empty()
            && self.functions.is_empty()
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
    /// Build the model from a validated [`Api`], using `prefix` for every C
    /// symbol name. `prefix` is the single global ABI prefix (default
    /// `"weaveffi"`); passing the same prefix to every backend is what keeps
    /// the producer header and all consumers calling identical symbols.
    pub fn build(api: &Api, prefix: &str) -> Self {
        let mut modules = Vec::new();
        for m in &api.modules {
            lower_module(m, &[], prefix, &mut modules);
        }
        Self {
            prefix: prefix.to_string(),
            version: api.version.clone(),
            modules,
        }
    }

    /// Iterate every function across all modules, paired with its module.
    pub fn functions(&self) -> impl Iterator<Item = (&ModuleBinding, &FnBinding)> {
        self.modules
            .iter()
            .flat_map(|m| m.functions.iter().map(move |f| (m, f)))
    }
}

/// Recursively lower `module` and its descendants into the flat `out` list,
/// pre-order (parent before children) so symbol declarations precede uses.
fn lower_module(module: &Module, parent: &[String], prefix: &str, out: &mut Vec<ModuleBinding>) {
    let mut segments = parent.to_vec();
    segments.push(module.name.clone());
    let path = segments.join("_");

    let enums = module
        .enums
        .iter()
        .map(|e| lower_enum(e, &path, prefix))
        .collect();
    let structs = module
        .structs
        .iter()
        .map(|s| lower_struct(s, &path, prefix))
        .collect();
    let callbacks: Vec<CallbackBinding> = module
        .callbacks
        .iter()
        .map(|c| lower_callback(c, &path, prefix))
        .collect();
    let listeners = module
        .listeners
        .iter()
        .map(|l| lower_listener(l, &path, prefix))
        .collect();
    let functions = module
        .functions
        .iter()
        .map(|f| lower_function(f, &path, prefix))
        .collect();

    // Module doc is synthesized from the first documented function, matching
    // the `EmptyModuleDoc` lint's notion of "the module is documented".
    let doc = module.functions.iter().find_map(|f| f.doc.clone());

    out.push(ModuleBinding {
        name: module.name.clone(),
        segments: segments.clone(),
        path,
        doc,
        enums,
        structs,
        callbacks,
        listeners,
        functions,
    });

    for child in &module.modules {
        lower_module(child, &segments, prefix, out);
    }
}

fn lower_param_binding(p: &weaveffi_ir::ir::Param, module: &str) -> ParamBinding {
    ParamBinding {
        name: p.name.clone(),
        ty: p.ty.clone(),
        mutable: p.mutable,
        doc: p.doc.clone(),
        abi: lower_param(&p.name, &p.ty, module, p.mutable),
    }
}

fn lower_enum(e: &EnumDef, path: &str, prefix: &str) -> EnumBinding {
    let c_tag = format!("{prefix}_{path}_{}", e.name);
    let variants = e
        .variants
        .iter()
        .map(|v| EnumVariantBinding {
            name: v.name.clone(),
            value: v.value,
            doc: v.doc.clone(),
            c_const: format!("{c_tag}_{}", v.name),
        })
        .collect();

    // A rich (algebraic) enum gains an opaque-object surface mirroring a
    // struct: a tag getter, a destructor, and per-variant constructors and
    // field getters. The variant name namespaces the per-variant symbols.
    let rich = e.is_rich().then(|| {
        let variants = e
            .variants
            .iter()
            .map(|v| {
                let fields: Vec<FieldBinding> = v
                    .fields
                    .iter()
                    .map(|f| {
                        let r = lower_return(&f.ty, path);
                        FieldBinding {
                            name: f.name.clone(),
                            doc: f.doc.clone(),
                            ty: f.ty.clone(),
                            getter_symbol: format!("{c_tag}_{}_get_{}", v.name, f.name),
                            getter_ret: r.ret,
                            getter_out_params: r.out_params,
                            value_params: lower_param(&f.name, &f.ty, path, false),
                        }
                    })
                    .collect();
                let mut create_params: Vec<AbiParam> = v
                    .fields
                    .iter()
                    .flat_map(|f| lower_param(&f.name, &f.ty, path, false))
                    .collect();
                create_params.push(error_out_param());
                let create = AbiFn {
                    symbol: format!("{c_tag}_{}_new", v.name),
                    params: create_params,
                    ret: CType::ptr(CType::Named(format!("{path}_{}", e.name))),
                };
                RichVariantBinding {
                    name: v.name.clone(),
                    doc: v.doc.clone(),
                    value: v.value,
                    c_const: format!("{c_tag}_{}", v.name),
                    create,
                    fields,
                }
            })
            .collect();
        RichEnumBinding {
            tag_symbol: format!("{c_tag}_tag"),
            destroy_symbol: format!("{c_tag}_destroy"),
            variants,
        }
    });

    EnumBinding {
        name: e.name.clone(),
        doc: e.doc.clone(),
        c_tag,
        variants,
        rich,
    }
}

fn lower_struct(s: &StructDef, path: &str, prefix: &str) -> StructBinding {
    let c_tag = format!("{prefix}_{path}_{}", s.name);

    let fields: Vec<FieldBinding> = s
        .fields
        .iter()
        .map(|f| {
            let r = lower_return(&f.ty, path);
            FieldBinding {
                name: f.name.clone(),
                doc: f.doc.clone(),
                ty: f.ty.clone(),
                getter_symbol: format!("{c_tag}_get_{}", f.name),
                getter_ret: r.ret,
                getter_out_params: r.out_params,
                value_params: lower_param(&f.name, &f.ty, path, false),
            }
        })
        .collect();

    // create: each field lowered as an input parameter, then out_err.
    let mut create_params: Vec<AbiParam> = s
        .fields
        .iter()
        .flat_map(|f| lower_param(&f.name, &f.ty, path, false))
        .collect();
    create_params.push(error_out_param());
    let create = AbiFn {
        symbol: format!("{c_tag}_create"),
        params: create_params,
        ret: CType::ptr(CType::Named(format!("{path}_{}", s.name))),
    };

    let builder = s.builder.then(|| {
        let builder_tag = format!("{c_tag}Builder");
        let setters = s
            .fields
            .iter()
            .map(|f| (f.name.clone(), format!("{c_tag}_Builder_set_{}", f.name)))
            .collect();
        BuilderBinding {
            builder_tag,
            new_symbol: format!("{c_tag}_Builder_new"),
            build_symbol: format!("{c_tag}_Builder_build"),
            destroy_symbol: format!("{c_tag}_Builder_destroy"),
            setters,
        }
    });

    StructBinding {
        name: s.name.clone(),
        doc: s.doc.clone(),
        c_tag: c_tag.clone(),
        fields,
        create,
        destroy_symbol: format!("{c_tag}_destroy"),
        builder,
    }
}

fn lower_callback(c: &CallbackDef, path: &str, prefix: &str) -> CallbackBinding {
    let params: Vec<ParamBinding> = c
        .params
        .iter()
        .map(|p| lower_param_binding(p, path))
        .collect();
    let mut abi_params: Vec<AbiParam> = params.iter().flat_map(|p| p.abi.clone()).collect();
    abi_params.push(context_param());
    CallbackBinding {
        name: c.name.clone(),
        doc: c.doc.clone(),
        c_fn_type: format!("{prefix}_{path}_{}_fn", c.name),
        params,
        abi_params,
    }
}

fn lower_listener(l: &ListenerDef, path: &str, prefix: &str) -> ListenerBinding {
    ListenerBinding {
        name: l.name.clone(),
        doc: l.doc.clone(),
        event_callback: l.event_callback.clone(),
        callback_c_fn_type: format!("{prefix}_{path}_{}_fn", l.event_callback),
        register_symbol: format!("{prefix}_{path}_register_{}", l.name),
        unregister_symbol: format!("{prefix}_{path}_unregister_{}", l.name),
    }
}

fn lower_function(f: &Function, path: &str, prefix: &str) -> FnBinding {
    let params: Vec<ParamBinding> = f
        .params
        .iter()
        .map(|p| lower_param_binding(p, path))
        .collect();
    let c_base = format!("{prefix}_{path}_{}", f.name);

    let shape = if let Some(TypeRef::Iterator(inner)) = &f.returns {
        let pascal = f.name.to_upper_camel_case();
        let iter_tag = format!("{prefix}_{path}_{pascal}Iterator");
        let iter_core = format!("{path}_{pascal}Iterator");

        // launcher: input slots + out_err, returns iter_tag*.
        let mut launch_params: Vec<AbiParam> = f
            .params
            .iter()
            .flat_map(|p| lower_param(&p.name, &p.ty, path, p.mutable))
            .collect();
        launch_params.push(error_out_param());
        let launch = AbiFn {
            symbol: c_base.clone(),
            params: launch_params,
            ret: CType::ptr(CType::Named(iter_core.clone())),
        };

        // next: (iter, out_item, <item out_params>, out_err) -> int32.
        let item = lower_return(inner, path);
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
            elem: (**inner).clone(),
            iter_tag: iter_tag.clone(),
            launch,
            next,
            destroy_symbol: format!("{iter_tag}_destroy"),
        })
    } else if f.r#async {
        let callback_type = format!("{c_base}_callback");
        let mut launch_params = async_input_params(f, path);
        launch_params.push(AbiParam::new(
            "callback",
            CType::Named(format!("{path}_{}_callback", f.name)),
        ));
        launch_params.push(context_param());
        let launch = AbiFn {
            symbol: format!("{c_base}_async"),
            params: launch_params,
            ret: CType::Void,
        };
        CallShape::Async(AsyncBinding {
            launch,
            callback_type,
            callback_params: async_callback_params(f.returns.as_ref(), path),
        })
    } else {
        let sig = sync_signature(&f.params, f.returns.as_ref(), path);
        CallShape::Sync(AbiFn {
            symbol: c_base.clone(),
            params: sig.params,
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
        params,
        ret: f.returns.clone(),
        c_base,
        shape,
    }
}

/// The element C type of an iterator's `out_item` slot (the pointee of
/// `T* out_item`). Exposed for backends that materialize iterator results.
pub fn iterator_item_ctype(elem: &TypeRef, module: &str) -> CType {
    abi::lower_return(elem, module).ret
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{
        CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
        StructField,
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
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
            modules,
            generators: None,
            package: None,
        }
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
        match &f.shape {
            CallShape::Sync(abi) => {
                assert_eq!(abi.symbol, "weaveffi_math_add");
                assert_eq!(abi.ret, CType::Int32);
                let rendered: Vec<String> = abi
                    .params
                    .iter()
                    .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
                    .collect();
                assert_eq!(
                    rendered,
                    ["int32_t a", "int32_t b", "weaveffi_error* out_err"]
                );
            }
            _ => panic!("expected sync"),
        }
    }

    #[test]
    fn prefix_is_honored_everywhere() {
        let m = Module {
            functions: vec![func("ping", vec![], None)],
            ..module("net")
        };
        let model = BindingModel::build(&api(vec![m]), "acme");
        let f = &model.modules[0].functions[0];
        assert_eq!(f.c_base, "acme_net_ping");
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
        match &model.modules[0].functions[0].shape {
            CallShape::Async(a) => {
                assert_eq!(a.launch.symbol, "weaveffi_net_fetch_async");
                assert_eq!(a.callback_type, "weaveffi_net_fetch_callback");
                let last_two: Vec<&str> = a
                    .launch
                    .params
                    .iter()
                    .rev()
                    .take(2)
                    .map(|p| p.name.as_str())
                    .collect();
                assert_eq!(last_two, ["context", "callback"]);
                // cancel_token slot is present before callback/context.
                assert!(a.launch.params.iter().any(|p| p.name == "cancel_token"));
                // callback prefix is (context, err, result).
                assert_eq!(a.callback_params[0].name, "context");
                assert_eq!(a.callback_params[1].name, "err");
            }
            _ => panic!("expected async"),
        }
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
        match &model.modules[0].functions[0].shape {
            CallShape::Iterator(it) => {
                assert_eq!(it.iter_tag, "weaveffi_events_GetMessagesIterator");
                assert_eq!(it.launch.symbol, "weaveffi_events_get_messages");
                assert_eq!(it.next.symbol, "weaveffi_events_GetMessagesIterator_next");
                assert_eq!(
                    it.destroy_symbol,
                    "weaveffi_events_GetMessagesIterator_destroy"
                );
                assert_eq!(it.next.ret, CType::Int32);
                // out_item is `const char** out_item` for a string element.
                let out_item = &it.next.params[1];
                assert_eq!(out_item.name, "out_item");
                assert_eq!(out_item.ty.render_c("weaveffi"), "const char**");
            }
            _ => panic!("expected iterator"),
        }
    }

    #[test]
    fn struct_create_getters_and_builder() {
        let m = Module {
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: true,
            }],
            ..module("contacts")
        };
        let model = BindingModel::build(&api(vec![m]), "weaveffi");
        let s = &model.modules[0].structs[0];
        assert_eq!(s.c_tag, "weaveffi_contacts_Contact");
        assert_eq!(s.create.symbol, "weaveffi_contacts_Contact_create");
        assert_eq!(s.destroy_symbol, "weaveffi_contacts_Contact_destroy");
        assert_eq!(
            s.fields[0].getter_symbol,
            "weaveffi_contacts_Contact_get_name"
        );
        let b = s.builder.as_ref().unwrap();
        assert_eq!(b.builder_tag, "weaveffi_contacts_ContactBuilder");
        assert_eq!(b.new_symbol, "weaveffi_contacts_Contact_Builder_new");
        assert_eq!(b.setters[0].1, "weaveffi_contacts_Contact_Builder_set_name");
    }

    #[test]
    fn enum_constants_are_prefixed() {
        let m = Module {
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            ..module("gfx")
        };
        let model = BindingModel::build(&api(vec![m]), "weaveffi");
        let e = &model.modules[0].enums[0];
        assert_eq!(e.c_tag, "weaveffi_gfx_Color");
        assert_eq!(e.variants[0].c_const, "weaveffi_gfx_Color_Red");
        assert_eq!(e.variants[1].c_const, "weaveffi_gfx_Color_Green");
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
        // context appended last.
        assert_eq!(cb.abi_params.last().unwrap().name, "context");
        let l = &mb.listeners[0];
        assert_eq!(l.register_symbol, "weaveffi_events_register_messages");
        assert_eq!(l.unregister_symbol, "weaveffi_events_unregister_messages");
        assert_eq!(l.callback_c_fn_type, "weaveffi_events_on_message_fn");
        assert!(mb.callback("on_message").is_some());
    }

    #[test]
    fn nested_modules_flatten_pre_order_with_paths() {
        let inner = Module {
            functions: vec![func("leaf_fn", vec![], None)],
            ..module("inner")
        };
        let outer = Module {
            functions: vec![func("outer_fn", vec![], None)],
            modules: vec![inner],
            ..module("outer")
        };
        let model = BindingModel::build(&api(vec![outer]), "weaveffi");
        let paths: Vec<&str> = model.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["outer", "outer_inner"]);
        assert_eq!(
            model.modules[1].functions[0].c_base,
            "weaveffi_outer_inner_leaf_fn"
        );
    }
}
