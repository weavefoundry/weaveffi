//! Building the [`Model`]: index every type declaration, resolve each written
//! [`TypeRef`] to its positional type, and lower every declaration to its C
//! symbols, ABI signatures, and passing contracts in one walk.
//!
//! The build runs only on a document every validation rule accepted, so
//! each step that can fail (an undeclared name, a callback interface or an
//! iterator in a value position) is one a rule already reported. The steps
//! still return the matching [`ValidationError`] instead of panicking, so a
//! missing rule surfaces as a diagnostic, never as a crash.

use heck::ToUpperCamelCase;

use super::{
    c_tag, member_symbol, AbiFn, AsyncBinding, CallShape, CallbackInterfaceBinding,
    CallbackMethodBinding, CallbackParamBinding, EnumBinding, EnumVariantBinding, ErrorBinding,
    ErrorCodeBinding, FieldBinding, FnBinding, InterfaceBinding, IteratorBinding, Model,
    ModuleBinding, ParamBinding, StructBinding,
};
use crate::abi::lower::{Lower, ParamSite, Undeclared};
use crate::abi::{
    cancel_token_param, context_param, ctx_param, error_out_param, AbiParam, CType, ConstPos,
};
use crate::ir::{
    Api, CallbackInterfaceDef, EnumDef, ErrorDomain, Function, InterfaceDef, Module, StructField,
    Throws, TypeRef,
};
use crate::pkg::Identity;
use crate::plan::{ErrorStrategy, RetPass};
use crate::ty::{ParamTy, RetTy, Ty, TypeDecl, TypeIndex, TypeKind};
use crate::validate::ValidationError;

/// Index every type and error-domain declaration in `api`, numbering
/// modules in the depth-first pre-order [`Model::modules`] uses. The first
/// declaration of a name wins; validation reports any later one.
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
            for (i, d) in m.errors.iter().enumerate() {
                declare(&d.name, TypeKind::ErrorDomain, i);
            }
            walk(&m.modules, &segments, types);
        }
    }
    let mut types = TypeIndex::default();
    walk(&api.modules, &[], &mut types);
    types
}

/// Build the model of `api` under `identity` from an index built by
/// [`index`].
///
/// # Errors
///
/// Returns the violation a validation rule should already have reported:
/// an undeclared name, or a callback interface, error domain, or iterator
/// where a value type belongs.
pub(crate) fn build(
    api: &Api,
    identity: Identity,
    types: TypeIndex,
) -> Result<Model, ValidationError> {
    let mut modules = Vec::new();
    {
        let builder = Builder {
            lower: Lower {
                types: &types,
                prefix: &identity.prefix,
            },
        };
        for m in &api.modules {
            builder.module(m, None, &[], &mut modules)?;
        }
    }
    Ok(Model {
        version: api.version.clone(),
        modules,
        identity,
        types,
    })
}

impl From<Undeclared> for ValidationError {
    fn from(Undeclared(name): Undeclared) -> Self {
        ValidationError::UnknownTypeRef { name }
    }
}

/// The error strategy a written `throws` declares.
fn error_strategy(throws: Option<&Throws>) -> ErrorStrategy {
    match throws {
        None => ErrorStrategy::Trap,
        Some(Throws::Domain(name)) => ErrorStrategy::Domain(name.clone()),
        Some(Throws::Any) => ErrorStrategy::Untyped,
    }
}

/// The per-build context: the lowering (type index and symbol prefix).
struct Builder<'a> {
    lower: Lower<'a>,
}

impl Builder<'_> {
    fn prefix(&self) -> &str {
        self.lower.prefix
    }

    /// Resolve a written type in a value position.
    fn value(&self, ty: &TypeRef) -> Result<Ty, ValidationError> {
        Ok(match ty {
            TypeRef::Prim(p) => Ty::Prim(*p),
            TypeRef::Named(name) => {
                let name = name.clone();
                match self.lower.types.kind(&name) {
                    Some(TypeKind::Record) => Ty::Record(name),
                    Some(TypeKind::Enum) => Ty::Enum(name),
                    Some(TypeKind::RichEnum) => Ty::RichEnum(name),
                    Some(TypeKind::Interface) => Ty::Interface(name),
                    Some(TypeKind::CallbackInterface) => {
                        return Err(ValidationError::CallbackInterfaceInInvalidPosition {
                            name,
                            location: "a value position".to_string(),
                        })
                    }
                    Some(TypeKind::ErrorDomain) => {
                        return Err(ValidationError::ErrorDomainAsType { name })
                    }
                    None if self.lower.types.is_foreign(&name) => Ty::Record(name),
                    None => return Err(ValidationError::UnknownTypeRef { name }),
                }
            }
            TypeRef::Optional(inner) => Ty::Optional(Box::new(self.value(inner)?)),
            TypeRef::List(inner) => Ty::List(Box::new(self.value(inner)?)),
            TypeRef::Map(k, v) => Ty::Map(Box::new(self.value(k)?), Box::new(self.value(v)?)),
            TypeRef::Iterator(_) => {
                return Err(ValidationError::IteratorInInvalidPosition {
                    location: "a value position".to_string(),
                })
            }
        })
    }

    /// Resolve a written parameter type: a bare or optional callback
    /// interface, or a value.
    fn param_ty(&self, ty: &TypeRef) -> Result<ParamTy, ValidationError> {
        let (top, nullable) = match ty {
            TypeRef::Optional(inner) => (inner.as_ref(), true),
            other => (other, false),
        };
        if let TypeRef::Named(name) = top {
            if self.lower.types.kind(name) == Some(TypeKind::CallbackInterface) {
                return Ok(ParamTy::Callback {
                    name: name.clone(),
                    nullable,
                });
            }
        }
        Ok(ParamTy::Value(self.value(ty)?))
    }

    /// Resolve a written return type: an iterator, or a value.
    fn ret_ty(&self, ty: &TypeRef) -> Result<RetTy, ValidationError> {
        Ok(match ty {
            TypeRef::Iterator(elem) => RetTy::Iterator(self.value(elem)?),
            other => RetTy::Value(self.value(other)?),
        })
    }

    /// Recursively lower `module` and its descendants into the flat `out`
    /// list, pre-order (parent before children) so symbol declarations
    /// precede uses and positions match the [`TypeIndex`]. Returns the
    /// module's position.
    fn module(
        &self,
        module: &Module,
        parent: Option<usize>,
        parent_segments: &[String],
        out: &mut Vec<ModuleBinding>,
    ) -> Result<usize, ValidationError> {
        let mut segments = parent_segments.to_vec();
        segments.push(module.name.clone());
        let path = segments.join("_");
        let dot_path = segments.join(".");

        let binding = ModuleBinding {
            index: out.len(),
            name: module.name.clone(),
            doc: module.doc.clone(),
            parent,
            children: Vec::new(),
            errors: module
                .errors
                .iter()
                .map(|d| self.error_domain(d, &path, &dot_path))
                .collect::<Result<_, _>>()?,
            enums: module
                .enums
                .iter()
                .map(|e| self.enum_def(e, &path))
                .collect::<Result<_, _>>()?,
            structs: module
                .structs
                .iter()
                .map(|s| {
                    Ok(StructBinding {
                        name: s.name.clone(),
                        doc: s.doc.clone(),
                        deprecated: s.deprecated.clone(),
                        c_tag: c_tag(self.prefix(), &path, &s.name),
                        fields: self.fields(&s.fields)?,
                    })
                })
                .collect::<Result<_, ValidationError>>()?,
            interfaces: module
                .interfaces
                .iter()
                .map(|i| self.interface(i, &path))
                .collect::<Result<_, _>>()?,
            callback_interfaces: module
                .callback_interfaces
                .iter()
                .map(|c| self.callback_interface(c, &path))
                .collect::<Result<_, _>>()?,
            functions: module
                .functions
                .iter()
                .map(|f| self.callable(f, &path, None, None))
                .collect::<Result<_, _>>()?,
            segments: segments.clone(),
            path,
            dot_path,
        };
        let index = binding.index;
        out.push(binding);
        for child in &module.modules {
            let child = self.module(child, Some(index), &segments, out)?;
            out[index].children.push(child);
        }
        Ok(index)
    }

    fn error_domain(
        &self,
        domain: &ErrorDomain,
        path: &str,
        dot_path: &str,
    ) -> Result<ErrorBinding, ValidationError> {
        let tag = c_tag(self.prefix(), path, &domain.name);
        let codes = domain
            .codes
            .iter()
            .map(|c| {
                Ok(ErrorCodeBinding {
                    name: c.name.clone(),
                    value: c.code,
                    message: c.message.clone(),
                    doc: c.doc.clone(),
                    c_const: member_symbol(&tag, &c.name),
                    fields: self.fields(&c.fields)?,
                })
            })
            .collect::<Result<_, ValidationError>>()?;
        Ok(ErrorBinding {
            name: domain.name.clone(),
            type_name: crate::errors::type_name(&domain.name, "Error"),
            module: dot_path.to_string(),
            owner_path: path.to_string(),
            c_tag: tag,
            codes,
        })
    }

    fn fields(&self, fields: &[StructField]) -> Result<Vec<FieldBinding>, ValidationError> {
        fields
            .iter()
            .map(|f| {
                Ok(FieldBinding {
                    name: f.name.clone(),
                    doc: f.doc.clone(),
                    ty: self.value(&f.ty)?,
                })
            })
            .collect()
    }

    fn enum_def(&self, e: &EnumDef, path: &str) -> Result<EnumBinding, ValidationError> {
        let tag = c_tag(self.prefix(), path, &e.name);
        let variants = e
            .variants
            .iter()
            .map(|v| {
                Ok(EnumVariantBinding {
                    name: v.name.clone(),
                    value: v.value,
                    doc: v.doc.clone(),
                    c_const: member_symbol(&tag, &v.name),
                    fields: self.fields(&v.fields)?,
                })
            })
            .collect::<Result<_, ValidationError>>()?;
        Ok(EnumBinding {
            name: e.name.clone(),
            doc: e.doc.clone(),
            deprecated: e.deprecated.clone(),
            c_tag: tag,
            variants,
            rich: e.is_rich(),
        })
    }

    fn callback_interface(
        &self,
        c: &CallbackInterfaceDef,
        path: &str,
    ) -> Result<CallbackInterfaceBinding, ValidationError> {
        let tag = c_tag(self.prefix(), path, &c.name);
        let methods = c
            .methods
            .iter()
            .map(|m| self.callback_method(m))
            .collect::<Result<_, _>>()?;
        Ok(CallbackInterfaceBinding {
            name: c.name.clone(),
            doc: c.doc.clone(),
            deprecated: c.deprecated.clone(),
            vtable_tag: member_symbol(&tag, "vtable"),
            c_tag: tag,
            methods,
        })
    }

    /// Lower one callback-interface method to its vtable entry: `ctx`, then
    /// every parameter's slots, then the return's out slots, then `out_err`.
    fn callback_method(&self, m: &Function) -> Result<CallbackMethodBinding, ValidationError> {
        let params = m
            .params
            .iter()
            .map(|p| {
                let ty = self.value(&p.ty)?;
                Ok(CallbackParamBinding {
                    pass: self
                        .lower
                        .value_param(&p.name, &ty, ParamSite::CallbackMethod)?,
                    name: p.name.clone(),
                    ty,
                    doc: p.doc.clone(),
                })
            })
            .collect::<Result<Vec<_>, ValidationError>>()?;
        let ret = m.returns.as_ref().map(|r| self.value(r)).transpose()?;
        let (c_ret, ret_pass) = self.lower.callback_ret(ret.as_ref())?;
        let mut slots = vec![ctx_param()];
        for p in &params {
            slots.extend(p.pass.slots().into_iter().cloned());
        }
        slots.extend(ret_pass.out_slots().into_iter().cloned());
        slots.push(error_out_param());
        Ok(CallbackMethodBinding {
            name: m.name.clone(),
            doc: m.doc.clone(),
            deprecated: m.deprecated.clone(),
            abi: AbiFn {
                symbol: m.name.clone(),
                params: slots,
                ret: c_ret,
            },
            params,
            ret,
            ret_pass,
            error: error_strategy(m.throws.as_ref()),
        })
    }

    /// Lower an interface: constructors become statics returning the
    /// interface, methods gain the implicit `self` slot, and all member
    /// symbols hang off the interface's `c_tag`.
    fn interface(
        &self,
        iface: &InterfaceDef,
        path: &str,
    ) -> Result<InterfaceBinding, ValidationError> {
        let tag = c_tag(self.prefix(), path, &iface.name);
        let owner = format!("{path}_{}", iface.name);
        let receiver = AbiParam::new(
            "self",
            CType::Ptr {
                konst: ConstPos::West,
                pointee: Box::new(CType::StructTag {
                    module: path.to_string(),
                    name: iface.name.clone(),
                }),
            },
        );
        let constructed = RetTy::Value(Ty::Interface(iface.name.clone()));
        let constructors = iface
            .constructors
            .iter()
            .map(|c| self.callable(c, &owner, None, Some(constructed.clone())))
            .collect::<Result<_, _>>()?;
        let methods = iface
            .methods
            .iter()
            .map(|m| self.callable(m, &owner, Some(receiver.clone()), None))
            .collect::<Result<_, _>>()?;
        let statics = iface
            .statics
            .iter()
            .map(|s| self.callable(s, &owner, None, None))
            .collect::<Result<_, _>>()?;
        Ok(InterfaceBinding {
            name: iface.name.clone(),
            doc: iface.doc.clone(),
            deprecated: iface.deprecated.clone(),
            clone_symbol: member_symbol(&tag, "clone"),
            destroy_symbol: member_symbol(&tag, "destroy"),
            c_tag: tag,
            constructors,
            methods,
            statics,
        })
    }

    /// Lower one callable (free function or interface member).
    ///
    /// `owner` is the prefix-free C path the callable's symbols hang off:
    /// the module path for a free function, `{module path}_{Interface}` for
    /// a member. `receiver` is an instance method's `self` slot; `ret`
    /// overrides the written return (a constructor returns its interface).
    fn callable(
        &self,
        f: &Function,
        owner: &str,
        receiver: Option<AbiParam>,
        ret: Option<RetTy>,
    ) -> Result<FnBinding, ValidationError> {
        let prefix = self.prefix();
        // `CType::Named` cores render as `{prefix}_{core}`.
        let core = format!("{owner}_{}", f.name);
        let symbol = format!("{prefix}_{core}");
        let params = f
            .params
            .iter()
            .map(|p| {
                let ty = self.param_ty(&p.ty)?;
                Ok(ParamBinding {
                    pass: self.lower.param(&p.name, &ty, ParamSite::Call)?,
                    name: p.name.clone(),
                    ty,
                    doc: p.doc.clone(),
                })
            })
            .collect::<Result<Vec<_>, ValidationError>>()?;
        let ret = match ret {
            Some(ret) => Some(ret),
            None => f.returns.as_ref().map(|r| self.ret_ty(r)).transpose()?,
        };
        let mut slots: Vec<AbiParam> = receiver.iter().cloned().collect();
        for p in &params {
            slots.extend(p.pass.slots().into_iter().cloned());
        }

        let (abi, ret_pass, shape) = if f.r#async {
            let value = match &ret {
                None => None,
                Some(RetTy::Value(ty)) => Some(ty),
                Some(RetTy::Iterator(_)) => {
                    return Err(ValidationError::AsyncIteratorReturn {
                        module: owner.to_string(),
                        function: f.name.clone(),
                    })
                }
            };
            let result = self.lower.result(value)?;
            // Launcher: the input slots, the cancel token when cancellable,
            // then the completion callback and its context.
            let cancel_token = f.cancellable.then(cancel_token_param);
            slots.extend(cancel_token.iter().cloned());
            slots.push(AbiParam::new(
                "callback",
                CType::Named(format!("{core}_callback")),
            ));
            slots.push(context_param());
            let mut callback_params = vec![
                context_param(),
                AbiParam::new("err", CType::ptr(CType::Error)),
            ];
            callback_params.extend(result.slots().into_iter().cloned());
            let launch = AbiFn {
                symbol: symbol.clone(),
                params: slots,
                ret: CType::Void,
            };
            let shape = CallShape::Async(AsyncBinding {
                callback_type: format!("{symbol}_callback"),
                callback_params,
                result,
                cancel_token,
            });
            (launch, RetPass::Void, shape)
        } else if let Some(RetTy::Iterator(elem)) = &ret {
            // `{owner}_{Pascal}Iterator`.
            let iter_core = format!("{owner}_{}Iterator", f.name.to_upper_camel_case());
            let iter_tag = format!("{prefix}_{iter_core}");
            let handle = CType::ptr(CType::Named(iter_core));
            slots.push(error_out_param());
            let launch = AbiFn {
                symbol,
                params: slots,
                ret: handle.clone(),
            };
            let item = self.lower.item(elem)?;
            let mut next_params = vec![AbiParam::new("iter", handle)];
            next_params.extend(item.slots().into_iter().cloned());
            next_params.push(error_out_param());
            let iterator = IteratorBinding {
                elem: elem.clone(),
                next: AbiFn {
                    symbol: member_symbol(&iter_tag, "next"),
                    params: next_params,
                    ret: CType::Int32,
                },
                item,
                destroy_symbol: member_symbol(&iter_tag, "destroy"),
                iter_tag,
            };
            (launch, RetPass::Iterator(iterator), CallShape::Sync)
        } else {
            let (c_ret, ret_pass) = self.lower.ret(ret.as_ref().and_then(RetTy::value))?;
            slots.extend(ret_pass.out_slots().into_iter().cloned());
            slots.push(error_out_param());
            let call = AbiFn {
                symbol,
                params: slots,
                ret: c_ret,
            };
            (call, ret_pass, CallShape::Sync)
        };

        Ok(FnBinding {
            name: f.name.clone(),
            doc: f.doc.clone(),
            deprecated: f.deprecated.clone(),
            receiver,
            params,
            ret,
            ret_pass,
            error: error_strategy(f.throws.as_ref()),
            abi,
            shape,
        })
    }
}
