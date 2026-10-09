//! Building the [`Model`]: index every type declaration, resolve each written
//! [`TypeRef`] to a [`Ty`], and lower every declaration to its C symbols and
//! ABI signatures in one walk.

use heck::ToUpperCamelCase;

use super::{
    AbiFn, AsyncBinding, CallShape, CallbackInterfaceBinding, CallbackMethodBinding, EnumBinding,
    EnumVariantBinding, ErrorBinding, ErrorCodeBinding, FieldBinding, FnBinding, InterfaceBinding,
    IteratorBinding, Model, ModuleBinding, ParamBinding, StructBinding,
};
use crate::abi::{
    callback_result_params, cancel_token_param, context_param, ctx_param, error_out_param,
    lower_callback_return, lower_param, lower_return, AbiParam, CType, ConstPos,
};
use crate::ir::{
    Api, CallbackInterfaceDef, EnumDef, ErrorDomain, Function, InterfaceDef, Module, Param,
    StructDef, StructField, TypeRef,
};
use crate::pkg::Identity;
use crate::ty::{Family, Ty, TypeDecl, TypeIndex, TypeKind};

/// Index every type declaration in `api`, numbering modules in the
/// depth-first pre-order [`Model::modules`] uses. The first declaration of a
/// name wins; validation reports any later one.
pub(crate) fn index(api: &Api) -> TypeIndex {
    fn walk(modules: &[Module], parent: &[&str], types: &mut TypeIndex) {
        for m in modules {
            let mut segments = parent.to_vec();
            segments.push(&m.name);
            let module = types.push_module(segments.join("_"));
            let mut declare = |name: &str, kind: TypeKind, index: usize| {
                types.declare(
                    name,
                    TypeDecl {
                        kind,
                        module,
                        index,
                    },
                );
            };
            for (i, s) in m.structs.iter().enumerate() {
                declare(&s.name, TypeKind::Record, i);
            }
            for (i, e) in m.enums.iter().enumerate() {
                let kind = if e.is_rich() {
                    TypeKind::RichEnum
                } else {
                    TypeKind::Enum
                };
                declare(&e.name, kind, i);
            }
            for (i, iface) in m.interfaces.iter().enumerate() {
                declare(&iface.name, TypeKind::Interface, i);
            }
            for (i, c) in m.callback_interfaces.iter().enumerate() {
                declare(&c.name, TypeKind::CallbackInterface, i);
            }
            walk(&m.modules, &segments, types);
        }
    }
    let mut types = TypeIndex::default();
    walk(&api.modules, &[], &mut types);
    types
}

/// Map a written type reference to its resolved type. A name no declaration
/// provides resolves to a [`Ty::Record`] (see [`Model::assume_valid`]).
pub(crate) fn resolve(types: &TypeIndex, ty: &TypeRef) -> Ty {
    match ty {
        TypeRef::Prim(p) => Ty::Prim(*p),
        TypeRef::Named(name) => {
            let name = name.clone();
            match types.kind(&name) {
                Some(TypeKind::Enum) => Ty::Enum(name),
                Some(TypeKind::RichEnum) => Ty::RichEnum(name),
                Some(TypeKind::Interface) => Ty::Interface(name),
                Some(TypeKind::CallbackInterface) => Ty::CallbackInterface(name),
                Some(TypeKind::Record) | None => Ty::Record(name),
            }
        }
        TypeRef::Optional(inner) => Ty::Optional(Box::new(resolve(types, inner))),
        TypeRef::List(inner) => Ty::List(Box::new(resolve(types, inner))),
        TypeRef::Map(k, v) => Ty::Map(Box::new(resolve(types, k)), Box::new(resolve(types, v))),
        TypeRef::Iterator(inner) => Ty::Iterator(Box::new(resolve(types, inner))),
    }
}

/// Build the model of `api` under `identity`, indexing its declarations
/// first.
pub(crate) fn build(api: &Api, identity: Identity) -> Model {
    let types = index(api);
    build_indexed(api, identity, types)
}

/// Build the model of `api` from an index already built by [`index`].
pub(crate) fn build_indexed(api: &Api, identity: Identity, types: TypeIndex) -> Model {
    let mut modules = Vec::new();
    let lowerer = Lowerer {
        types: &types,
        prefix: &identity.prefix,
    };
    for m in &api.modules {
        lowerer.module(m, &[], &mut modules);
    }
    Model {
        version: api.version.clone(),
        modules,
        identity,
        types,
    }
}

/// The per-build lowering context: the type index and the symbol prefix.
struct Lowerer<'a> {
    types: &'a TypeIndex,
    prefix: &'a str,
}

/// A fully assembled C signature: the ordered parameter slots and the C
/// return type.
struct AbiSig {
    params: Vec<AbiParam>,
    ret: CType,
}

impl Lowerer<'_> {
    fn ty(&self, ty: &TypeRef) -> Ty {
        resolve(self.types, ty)
    }

    /// Recursively lower `module` and its descendants into the flat `out`
    /// list, pre-order (parent before children) so symbol declarations
    /// precede uses and positions match the [`TypeIndex`].
    fn module(&self, module: &Module, parent: &[String], out: &mut Vec<ModuleBinding>) {
        let mut segments = parent.to_vec();
        segments.push(module.name.clone());
        let path = segments.join("_");
        let prefix = self.prefix;

        let functions = module
            .functions
            .iter()
            .map(|f| self.callable(f, &format!("{prefix}_{path}_{}", f.name), None))
            .collect();
        out.push(ModuleBinding {
            name: module.name.clone(),
            dot_path: segments.join("."),
            doc: module.doc.clone(),
            errors: module.errors.as_ref().map(|d| self.error_domain(d, &path)),
            enums: module
                .enums
                .iter()
                .map(|e| self.enum_def(e, &path))
                .collect(),
            structs: module.structs.iter().map(|s| self.struct_def(s)).collect(),
            interfaces: module
                .interfaces
                .iter()
                .map(|i| self.interface(i, &path))
                .collect(),
            callback_interfaces: module
                .callback_interfaces
                .iter()
                .map(|c| self.callback_interface(c, &path))
                .collect(),
            functions,
            segments: segments.clone(),
            path,
        });

        for child in &module.modules {
            self.module(child, &segments, out);
        }
    }

    fn error_domain(&self, domain: &ErrorDomain, path: &str) -> ErrorBinding {
        let c_tag = format!("{}_{path}_{}", self.prefix, domain.name);
        ErrorBinding {
            name: domain.name.clone(),
            type_name: crate::errors::type_name(&domain.name, "Error"),
            owner_path: path.to_string(),
            codes: domain
                .codes
                .iter()
                .map(|c| ErrorCodeBinding {
                    name: c.name.clone(),
                    value: c.code,
                    message: c.message.clone(),
                    doc: c.doc.clone(),
                    c_const: format!("{c_tag}_{}", c.name),
                    fields: self.fields(&c.fields),
                })
                .collect(),
            c_tag,
        }
    }

    fn fields(&self, fields: &[StructField]) -> Vec<FieldBinding> {
        fields
            .iter()
            .map(|f| FieldBinding {
                name: f.name.clone(),
                doc: f.doc.clone(),
                ty: self.ty(&f.ty),
            })
            .collect()
    }

    fn struct_def(&self, s: &StructDef) -> StructBinding {
        StructBinding {
            name: s.name.clone(),
            doc: s.doc.clone(),
            deprecated: s.deprecated.clone(),
            fields: self.fields(&s.fields),
        }
    }

    fn enum_def(&self, e: &EnumDef, path: &str) -> EnumBinding {
        let c_tag = format!("{}_{path}_{}", self.prefix, e.name);
        let variants = e
            .variants
            .iter()
            .map(|v| EnumVariantBinding {
                name: v.name.clone(),
                value: v.value,
                doc: v.doc.clone(),
                c_const: format!("{c_tag}_{}", v.name),
                fields: self.fields(&v.fields),
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

    fn params(&self, params: &[Param]) -> Vec<ParamBinding> {
        params
            .iter()
            .map(|p| {
                let ty = self.ty(&p.ty);
                ParamBinding {
                    name: p.name.clone(),
                    abi: lower_param(&p.name, &ty, self.types),
                    ty,
                    doc: p.doc.clone(),
                }
            })
            .collect()
    }

    /// Lower a return type to its C return plus out-parameters, or `void`.
    fn ret(&self, ret: Option<&Ty>) -> (CType, Vec<AbiParam>) {
        match ret {
            Some(ty) => {
                let r = lower_return(ty, self.types);
                (r.ret, r.out_params)
            }
            None => (CType::Void, vec![]),
        }
    }

    /// The signature of one callback-interface method as it appears in the
    /// vtable: `ctx`, then every parameter's slots, then the return's out
    /// slots, then `out_err` (see
    /// [`lower_callback_return`](crate::abi::lower_callback_return)).
    ///
    /// Object parameters differ from a plain call: the producer transfers one
    /// strong reference the consumer adopts, so the slot is a mutable `{tag}*`
    /// rather than the borrowed `const {tag}*` of a top-level parameter.
    fn callback_method_signature(&self, params: &[ParamBinding], ret: Option<&Ty>) -> AbiSig {
        let mut out = vec![ctx_param()];
        for p in params {
            if matches!(p.ty.family(), Family::Object { .. }) {
                let [slot] = p.abi.as_slice() else {
                    unreachable!("object parameter '{}' has one slot", p.name);
                };
                let CType::Ptr { pointee, .. } = &slot.ty else {
                    unreachable!("object slot '{}' is a pointer", p.name);
                };
                out.push(AbiParam::new(&slot.name, CType::ptr((**pointee).clone())));
            } else {
                out.extend(p.abi.iter().cloned());
            }
        }
        let ret = match ret {
            Some(ty) => {
                let r = lower_callback_return(ty, self.types);
                out.extend(r.out_params);
                r.ret
            }
            None => CType::Void,
        };
        out.push(error_out_param());
        AbiSig { params: out, ret }
    }

    /// The full C signature of a *synchronous* call: every input parameter's
    /// slots, then the return type's out-parameters, then `out_err`.
    fn sync_signature(&self, params: &[ParamBinding], ret: Option<&Ty>) -> AbiSig {
        let mut out: Vec<AbiParam> = params.iter().flat_map(|p| p.abi.iter().cloned()).collect();
        let (ret, out_params) = self.ret(ret);
        out.extend(out_params);
        out.push(error_out_param());
        AbiSig { params: out, ret }
    }

    fn callback_interface(&self, c: &CallbackInterfaceDef, path: &str) -> CallbackInterfaceBinding {
        let c_tag = format!("{}_{path}_{}", self.prefix, c.name);
        let methods = c
            .methods
            .iter()
            .map(|m| {
                let params = self.params(&m.params);
                let ret = m.returns.as_ref().map(|r| self.ty(r));
                let sig = self.callback_method_signature(&params, ret.as_ref());
                CallbackMethodBinding {
                    name: m.name.clone(),
                    doc: m.doc.clone(),
                    deprecated: m.deprecated.clone(),
                    throws: m.throws,
                    params,
                    ret,
                    abi_params: sig.params,
                    abi_ret: sig.ret,
                }
            })
            .collect();
        CallbackInterfaceBinding {
            name: c.name.clone(),
            doc: c.doc.clone(),
            deprecated: c.deprecated.clone(),
            vtable_tag: format!("{c_tag}_vtable"),
            c_tag,
            methods,
        }
    }

    /// Lower an interface: constructors become statics returning the
    /// interface, methods gain the implicit `self` slot, and all member
    /// symbols hang off the interface's `c_tag`.
    fn interface(&self, iface: &InterfaceDef, path: &str) -> InterfaceBinding {
        let c_tag = format!("{}_{path}_{}", self.prefix, iface.name);
        let self_slot = AbiParam::new(
            "self",
            CType::Ptr {
                konst: ConstPos::West,
                pointee: Box::new(CType::StructTag {
                    module: path.to_string(),
                    name: iface.name.clone(),
                }),
            },
        );
        let member = |name: &str| format!("{c_tag}_{name}");
        let constructors = iface
            .constructors
            .iter()
            .map(|c| {
                // A constructor yields a new strong reference to the
                // interface, exactly like a static returning it.
                let mut f = c.clone();
                f.returns = Some(TypeRef::Named(iface.name.clone()));
                self.callable(&f, &member(&c.name), None)
            })
            .collect();
        let methods = iface
            .methods
            .iter()
            .map(|m| self.callable(m, &member(&m.name), Some(self_slot.clone())))
            .collect();
        let statics = iface
            .statics
            .iter()
            .map(|s| self.callable(s, &member(&s.name), None))
            .collect();
        InterfaceBinding {
            name: iface.name.clone(),
            doc: iface.doc.clone(),
            deprecated: iface.deprecated.clone(),
            clone_symbol: format!("{c_tag}_clone"),
            destroy_symbol: format!("{c_tag}_destroy"),
            c_tag,
            constructors,
            methods,
            statics,
        }
    }

    /// Lower one callable (free function or interface member) whose full base
    /// C symbol is `c_base`. When `self_slot` is given (an instance method),
    /// it is prepended to every ABI signature but never appears in the
    /// retained [`ParamBinding`] list.
    fn callable(&self, f: &Function, c_base: &str, self_slot: Option<AbiParam>) -> FnBinding {
        let prefix = self.prefix;
        let params = self.params(&f.params);
        let ret = f.returns.as_ref().map(|r| self.ty(r));
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
        let inputs =
            || -> Vec<AbiParam> { params.iter().flat_map(|p| p.abi.iter().cloned()).collect() };

        let shape = if let Some(elem) = ret.as_ref().and_then(Ty::iterator_elem) {
            let pascal = f.name.to_upper_camel_case();
            // `{owner}_{Pascal}Iterator`, where owner is the module path for a
            // free function or `{module path}_{Interface}` for a method.
            let owner = &core_base[..core_base.len() - f.name.len() - 1];
            let iter_core = format!("{owner}_{pascal}Iterator");
            let iter_tag = format!("{prefix}_{iter_core}");

            let mut launch_params = inputs();
            launch_params.push(error_out_param());
            let launch = AbiFn {
                symbol: c_base.to_string(),
                params: with_self(launch_params),
                ret: CType::ptr(CType::Named(iter_core.clone())),
            };

            let item = lower_return(elem, self.types);
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
            // Launcher: the input slots, the cancel token when cancellable,
            // then the completion callback and its context.
            let mut launch_params = inputs();
            if f.cancellable {
                launch_params.push(cancel_token_param());
            }
            launch_params.push(AbiParam::new(
                "callback",
                CType::Named(format!("{core_base}_callback")),
            ));
            launch_params.push(context_param());
            // Completion callback: `(void* context, {prefix}_error* err,
            // <result fields>)`.
            let mut callback_params = vec![
                context_param(),
                AbiParam::new("err", CType::ptr(CType::Error)),
            ];
            if let Some(ret) = &ret {
                callback_params.extend(callback_result_params(ret, self.types));
            }
            CallShape::Async(AsyncBinding {
                launch: AbiFn {
                    symbol: c_base.to_string(),
                    params: with_self(launch_params),
                    ret: CType::Void,
                },
                callback_type: format!("{c_base}_callback"),
                callback_params,
            })
        } else {
            let sig = self.sync_signature(&params, ret.as_ref());
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
            cancellable: f.cancellable,
            throws: f.throws,
            has_self: self_slot.is_some(),
            params,
            ret,
            c_base: c_base.to_string(),
            shape,
        }
    }
}
