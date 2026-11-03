//! Building the [`Model`]: index every type declaration, resolve each written
//! [`TypeRef`] to its positional type, and lower every declaration to its C
//! symbols and ABI signatures in one walk.
//!
//! The build runs only on a document every validation rule accepted, so each
//! step that can fail (an undeclared name, a callback interface or an
//! iterator in a value position) is one a rule already reported. The steps
//! still return the matching [`ValidationError`] instead of panicking, so a
//! missing rule surfaces as a diagnostic, never as a crash.

use heck::ToUpperCamelCase;

use super::{
    AsyncBinding, CallableKind, CallbackBinding, CallbackMethodBinding, EnumBinding,
    EnumVariantBinding, ErrorBinding, ErrorCodeBinding, ErrorStrategy, FieldBinding, FnBinding,
    InterfaceBinding, IteratorBinding, Model, ParamBinding, RecordBinding, Shape,
};
use crate::abi::{
    cancel_token_param, context_param, ctx_param, error_out_param, AbiFn, AbiParam, CType, Position,
};
use crate::ir::{
    Api, CallbackDef, EnumDef, ErrorDomain, Field, Function, InterfaceDef, Throws, TypeRef,
};
use crate::pkg::Identity;
use crate::ty::{ParamTy, RetTy, Ty, TypeIndex, TypeKind};
use crate::validate::ValidationError;

/// Index every type and error-domain declaration in `api`. The first
/// declaration of a name wins; validation reports any later one.
pub(crate) fn index(api: &Api) -> TypeIndex {
    let mut types = TypeIndex::default();
    for (i, r) in api.records.iter().enumerate() {
        types.declare(&r.name, TypeKind::Record, i);
    }
    for (i, e) in api.enums.iter().enumerate() {
        let kind = if e.is_rich() {
            TypeKind::RichEnum
        } else {
            TypeKind::Enum
        };
        types.declare(&e.name, kind, i);
    }
    for (i, d) in api.errors.iter().enumerate() {
        types.declare(&d.name, TypeKind::ErrorDomain, i);
    }
    for (i, iface) in api.interfaces.iter().enumerate() {
        types.declare(&iface.name, TypeKind::Interface, i);
    }
    for (i, c) in api.callbacks.iter().enumerate() {
        types.declare(&c.name, TypeKind::CallbackInterface, i);
    }
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
    let b = Builder {
        types: &types,
        prefix: &identity.prefix,
    };
    let errors = api
        .errors
        .iter()
        .map(|d| b.error_domain(d))
        .collect::<Result<_, _>>()?;
    let enums = api
        .enums
        .iter()
        .map(|e| b.enum_def(e))
        .collect::<Result<_, _>>()?;
    let records = api
        .records
        .iter()
        .map(|r| {
            Ok(RecordBinding {
                name: r.name.clone(),
                doc: r.doc.clone(),
                deprecated: r.deprecated.clone(),
                c_tag: b.tag(&r.name),
                fields: b.fields(&r.fields)?,
            })
        })
        .collect::<Result<_, ValidationError>>()?;
    let interfaces = api
        .interfaces
        .iter()
        .map(|i| b.interface(i))
        .collect::<Result<_, _>>()?;
    let callbacks = api
        .callbacks
        .iter()
        .map(|c| b.callback(c))
        .collect::<Result<_, _>>()?;
    let functions = api
        .functions
        .iter()
        .map(|f| b.callable(f, CallableKind::Function, None))
        .collect::<Result<_, _>>()?;
    Ok(Model {
        version: api.version.clone(),
        doc: api.doc.clone(),
        errors,
        enums,
        records,
        interfaces,
        callbacks,
        functions,
        identity,
        types,
    })
}

/// The error strategy a written `throws` declares.
fn error_strategy(throws: Option<&Throws>) -> ErrorStrategy {
    match throws {
        None => ErrorStrategy::Trap,
        Some(Throws::Domain(name)) => ErrorStrategy::Domain(name.clone()),
        Some(Throws::Any) => ErrorStrategy::Untyped,
    }
}

/// The per-build context: the type index and the symbol prefix.
struct Builder<'a> {
    types: &'a TypeIndex,
    prefix: &'a str,
}

impl Builder<'_> {
    /// `{prefix}_{name}`.
    fn tag(&self, name: &str) -> String {
        format!("{}_{name}", self.prefix)
    }

    /// Resolve a written type in a value position.
    fn value(&self, ty: &TypeRef) -> Result<Ty, ValidationError> {
        Ok(match ty {
            TypeRef::Prim(p) => Ty::Prim(*p),
            TypeRef::Named(name) => {
                let name = name.clone();
                match self.types.kind(&name) {
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
            if self.types.kind(name) == Some(TypeKind::CallbackInterface) {
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

    fn error_domain(&self, domain: &ErrorDomain) -> Result<ErrorBinding, ValidationError> {
        let tag = self.tag(&domain.name);
        let codes = domain
            .codes
            .iter()
            .map(|c| {
                Ok(ErrorCodeBinding {
                    name: c.name.clone(),
                    value: c.code,
                    message: c.message.clone(),
                    doc: c.doc.clone(),
                    c_const: format!("{tag}_{}", c.name),
                    fields: self.fields(&c.fields)?,
                })
            })
            .collect::<Result<_, ValidationError>>()?;
        Ok(ErrorBinding {
            name: domain.name.clone(),
            type_name: crate::errors::type_name(&domain.name, "Error"),
            doc: domain.doc.clone(),
            c_tag: tag,
            codes,
        })
    }

    fn fields(&self, fields: &[Field]) -> Result<Vec<FieldBinding>, ValidationError> {
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

    fn enum_def(&self, e: &EnumDef) -> Result<EnumBinding, ValidationError> {
        let tag = self.tag(&e.name);
        let variants = e
            .variants
            .iter()
            .zip(e.values())
            .map(|(v, value)| {
                Ok(EnumVariantBinding {
                    name: v.name.clone(),
                    value,
                    doc: v.doc.clone(),
                    c_const: format!("{tag}_{}", v.name),
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
        })
    }

    fn callback(&self, c: &CallbackDef) -> Result<CallbackBinding, ValidationError> {
        let tag = self.tag(&c.name);
        let methods = c
            .methods
            .iter()
            .enumerate()
            .map(|(slot, m)| self.callback_method(slot, m))
            .collect::<Result<_, _>>()?;
        Ok(CallbackBinding {
            name: c.name.clone(),
            doc: c.doc.clone(),
            deprecated: c.deprecated.clone(),
            vtable_tag: format!("{tag}_vtable"),
            c_tag: tag,
            methods,
        })
    }

    /// Lower one callback method to its vtable entry: `ctx`, the
    /// parameters, then `out_err`, returning the value.
    fn callback_method(
        &self,
        slot: usize,
        m: &Function,
    ) -> Result<CallbackMethodBinding, ValidationError> {
        let params = m
            .params
            .iter()
            .map(|p| {
                Ok(FieldBinding {
                    name: p.name.clone(),
                    doc: p.doc.clone(),
                    ty: self.value(&p.ty)?,
                })
            })
            .collect::<Result<Vec<_>, ValidationError>>()?;
        let ret = m.returns.as_ref().map(|r| self.value(r)).transpose()?;
        let mut slots = vec![ctx_param()];
        slots.extend(
            params
                .iter()
                .map(|p| AbiParam::new(&p.name, CType::of(&p.ty, Position::Transfer))),
        );
        slots.push(error_out_param());
        Ok(CallbackMethodBinding {
            name: m.name.clone(),
            doc: m.doc.clone(),
            deprecated: m.deprecated.clone(),
            slot,
            abi: AbiFn {
                symbol: m.name.clone(),
                params: slots,
                ret: ret
                    .as_ref()
                    .map_or(CType::Void, |t| CType::of(t, Position::Transfer)),
            },
            params,
            ret,
            error: error_strategy(m.throws.as_ref()),
        })
    }

    /// Lower an interface: constructors return the interface, methods gain
    /// the `self` slot, and every member symbol hangs off its C tag.
    fn interface(&self, iface: &InterfaceDef) -> Result<InterfaceBinding, ValidationError> {
        let tag = self.tag(&iface.name);
        let owner = Some(iface.name.as_str());
        let members = |list: &[Function], kind| {
            list.iter()
                .map(|f| self.callable(f, kind, owner))
                .collect::<Result<Vec<_>, _>>()
        };
        Ok(InterfaceBinding {
            name: iface.name.clone(),
            doc: iface.doc.clone(),
            deprecated: iface.deprecated.clone(),
            constructors: members(&iface.constructors, CallableKind::Constructor)?,
            methods: members(&iface.methods, CallableKind::Method)?,
            statics: members(&iface.statics, CallableKind::Static)?,
            clone_symbol: format!("{tag}_clone"),
            destroy_symbol: format!("{tag}_destroy"),
            c_tag: tag,
        })
    }

    /// Lower one callable (free function or interface member).
    fn callable(
        &self,
        f: &Function,
        kind: CallableKind,
        owner: Option<&str>,
    ) -> Result<FnBinding, ValidationError> {
        let prefix = self.prefix;
        // The symbol without its prefix: `name`, or `Owner_name`.
        let core = match owner {
            Some(o) => format!("{o}_{}", f.name),
            None => f.name.clone(),
        };
        let path = match owner {
            Some(o) => format!("{o}.{}", f.name),
            None => f.name.clone(),
        };
        let symbol = format!("{prefix}_{core}");
        let params = f
            .params
            .iter()
            .map(|p| {
                Ok(ParamBinding {
                    name: p.name.clone(),
                    ty: self.param_ty(&p.ty)?,
                    doc: p.doc.clone(),
                })
            })
            .collect::<Result<Vec<_>, ValidationError>>()?;
        let ret = match (kind, owner) {
            (CallableKind::Constructor, Some(o)) => {
                Some(RetTy::Value(Ty::Interface(o.to_string())))
            }
            _ => f.returns.as_ref().map(|r| self.ret_ty(r)).transpose()?,
        };
        let mut slots = Vec::new();
        if kind == CallableKind::Method {
            let o = owner.expect("a method has an owner");
            slots.push(AbiParam::new(
                "self",
                CType::const_ptr(CType::Object(o.to_string())),
            ));
        }
        for p in &params {
            slots.push(AbiParam::new(
                &p.name,
                match &p.ty {
                    ParamTy::Value(ty) => CType::of(ty, Position::Param),
                    ParamTy::Callback { name, .. } => CType::Callback(name.clone()),
                },
            ));
        }

        let (abi, shape) = if f.r#async {
            let value = match &ret {
                None => None,
                Some(RetTy::Value(ty)) => Some(ty),
                Some(RetTy::Iterator(_)) => {
                    return Err(ValidationError::AsyncIteratorReturn { function: path })
                }
            };
            if f.cancellable {
                slots.push(cancel_token_param());
            }
            let callback_core = format!("{core}_callback");
            slots.push(AbiParam::new(
                "callback",
                CType::Named(callback_core.clone()),
            ));
            slots.push(context_param());
            let mut callback_params = vec![
                context_param(),
                AbiParam::new("err", CType::ptr(CType::Error)),
            ];
            if let Some(ty) = value {
                callback_params.push(AbiParam::new("result", CType::of(ty, Position::Transfer)));
            }
            let callback_type = format!("{prefix}_{callback_core}");
            let shape = Shape::Async(AsyncBinding {
                callback: AbiFn {
                    symbol: callback_type.clone(),
                    params: callback_params,
                    ret: CType::Void,
                },
                callback_type,
                cancellable: f.cancellable,
            });
            let launch = AbiFn {
                symbol,
                params: slots,
                ret: CType::Void,
            };
            (launch, shape)
        } else if let Some(RetTy::Iterator(elem)) = &ret {
            let iter_core = match owner {
                Some(o) => format!("{o}_{}Iterator", f.name.to_upper_camel_case()),
                None => format!("{}Iterator", f.name.to_upper_camel_case()),
            };
            let iter_tag = format!("{prefix}_{iter_core}");
            let handle = CType::ptr(CType::Named(iter_core));
            slots.push(error_out_param());
            let launch = AbiFn {
                symbol,
                params: slots,
                ret: handle.clone(),
            };
            let iterator = IteratorBinding {
                elem: elem.clone(),
                next: AbiFn {
                    symbol: format!("{iter_tag}_next"),
                    params: vec![
                        AbiParam::new("iter", handle),
                        AbiParam::new("out_item", CType::ptr(CType::of(elem, Position::Transfer))),
                        error_out_param(),
                    ],
                    ret: CType::Scalar(crate::ty::Prim::I32),
                },
                destroy_symbol: format!("{iter_tag}_destroy"),
                iter_tag,
            };
            (launch, Shape::Iterator(iterator))
        } else {
            slots.push(error_out_param());
            let c_ret = ret
                .as_ref()
                .and_then(RetTy::value)
                .map_or(CType::Void, |t| CType::of(t, Position::Transfer));
            let call = AbiFn {
                symbol,
                params: slots,
                ret: c_ret,
            };
            (call, Shape::Sync)
        };

        Ok(FnBinding {
            name: f.name.clone(),
            doc: f.doc.clone(),
            deprecated: f.deprecated.clone(),
            kind,
            owner: owner.map(str::to_string),
            path,
            params,
            ret,
            error: error_strategy(f.throws.as_ref()),
            abi,
            shape,
        })
    }
}
