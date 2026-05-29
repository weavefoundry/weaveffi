//! The structural lowering: how each [`TypeRef`] maps onto C ABI parameter
//! and return slots. This is the single source of truth every generator
//! shares; it replaces the per-generator `*_param_argtypes` / `*_return_info`
//! / `*_element_type` copies that used to drift apart.

use weaveffi_ir::ir::TypeRef;

use super::ctype::{CType, ConstPos};
use crate::codegen::common::is_c_pointer_type;

/// A named C parameter slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiParam {
    pub name: String,
    pub ty: CType,
}

impl AbiParam {
    pub fn new(name: impl Into<String>, ty: CType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// A lowered return: the C return type plus any trailing out-parameters
/// (e.g. `size_t* out_len`, or the `out_keys`/`out_values`/`out_len` triple
/// for maps).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiReturn {
    pub ret: CType,
    pub out_params: Vec<AbiParam>,
}

/// Resolve a struct reference (possibly `module.Name`) to its C tag type.
pub fn struct_tag(name: &str, current_module: &str) -> CType {
    if let Some((module, type_name)) = name.split_once('.') {
        CType::StructTag {
            module: module.to_string(),
            name: type_name.to_string(),
        }
    } else {
        CType::StructTag {
            module: current_module.to_string(),
            name: name.to_string(),
        }
    }
}

/// The C "element" type used in pointer/array contexts. Composite shapes
/// collapse to their innermost element; maps collapse to `void*`.
pub fn element_ctype(ty: &TypeRef, module: &str) -> CType {
    match ty {
        TypeRef::I32 => CType::Int32,
        TypeRef::U32 => CType::Uint32,
        TypeRef::I64 => CType::Int64,
        TypeRef::F64 => CType::Double,
        TypeRef::Bool => CType::Bool,
        TypeRef::Handle => CType::Handle,
        TypeRef::TypedHandle(n) => CType::ptr(CType::StructTag {
            module: module.to_string(),
            name: n.clone(),
        }),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => CType::const_ptr(CType::Char),
        TypeRef::Bytes | TypeRef::BorrowedBytes => CType::const_ptr(CType::Uint8),
        TypeRef::Struct(s) => CType::ptr(struct_tag(s, module)),
        TypeRef::Enum(e) => CType::Enum {
            module: module.to_string(),
            name: e.clone(),
        },
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            element_ctype(inner, module)
        }
        TypeRef::Map(_, _) => CType::ptr(CType::Void),
    }
}

/// Expand one IR parameter into its ordered C ABI slots.
pub fn lower_param(name: &str, ty: &TypeRef, module: &str, mutable: bool) -> Vec<AbiParam> {
    let west_if_immut = if mutable {
        ConstPos::None
    } else {
        ConstPos::West
    };
    match ty {
        TypeRef::I32 => vec![AbiParam::new(name, CType::Int32)],
        TypeRef::U32 => vec![AbiParam::new(name, CType::Uint32)],
        TypeRef::I64 => vec![AbiParam::new(name, CType::Int64)],
        TypeRef::F64 => vec![AbiParam::new(name, CType::Double)],
        TypeRef::Bool => vec![AbiParam::new(name, CType::Bool)],
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![AbiParam::new(
            name,
            CType::Ptr {
                konst: west_if_immut,
                pointee: Box::new(CType::Char),
            },
        )],
        TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
            AbiParam::new(
                format!("{name}_ptr"),
                CType::Ptr {
                    konst: west_if_immut,
                    pointee: Box::new(CType::Uint8),
                },
            ),
            AbiParam::new(format!("{name}_len"), CType::Size),
        ],
        TypeRef::Handle => vec![AbiParam::new(name, CType::Handle)],
        TypeRef::TypedHandle(n) => vec![AbiParam::new(
            name,
            CType::ptr(CType::StructTag {
                module: module.to_string(),
                name: n.clone(),
            }),
        )],
        TypeRef::Struct(s) => vec![AbiParam::new(
            name,
            CType::Ptr {
                konst: west_if_immut,
                pointee: Box::new(struct_tag(s, module)),
            },
        )],
        TypeRef::Enum(e) => vec![AbiParam::new(
            name,
            CType::Enum {
                module: module.to_string(),
                name: e.clone(),
            },
        )],
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                lower_param(name, inner, module, mutable)
            } else {
                vec![AbiParam::new(
                    name,
                    CType::Ptr {
                        konst: west_if_immut,
                        pointee: Box::new(element_ctype(inner, module)),
                    },
                )]
            }
        }
        TypeRef::List(inner) => {
            let elem = element_ctype(inner, module);
            let konst = if mutable {
                ConstPos::None
            } else if is_c_pointer_type(inner) {
                ConstPos::East
            } else {
                ConstPos::West
            };
            vec![
                AbiParam::new(
                    name,
                    CType::Ptr {
                        konst,
                        pointee: Box::new(elem),
                    },
                ),
                AbiParam::new(format!("{name}_len"), CType::Size),
            ]
        }
        TypeRef::Map(k, v) => {
            let key_elem = element_ctype(k, module);
            let val_elem = element_ctype(v, module);
            let key_konst = if mutable {
                ConstPos::None
            } else if is_c_pointer_type(k) {
                ConstPos::East
            } else {
                ConstPos::West
            };
            let val_konst = if mutable {
                ConstPos::None
            } else if is_c_pointer_type(v) {
                ConstPos::East
            } else {
                ConstPos::West
            };
            vec![
                AbiParam::new(
                    format!("{name}_keys"),
                    CType::Ptr {
                        konst: key_konst,
                        pointee: Box::new(key_elem),
                    },
                ),
                AbiParam::new(
                    format!("{name}_values"),
                    CType::Ptr {
                        konst: val_konst,
                        pointee: Box::new(val_elem),
                    },
                ),
                AbiParam::new(format!("{name}_len"), CType::Size),
            ]
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

/// Lower a return type to its C return type plus trailing out-parameters.
pub fn lower_return(ty: &TypeRef, module: &str) -> AbiReturn {
    let no_out = |ret| AbiReturn {
        ret,
        out_params: vec![],
    };
    match ty {
        TypeRef::I32 => no_out(CType::Int32),
        TypeRef::U32 => no_out(CType::Uint32),
        TypeRef::I64 => no_out(CType::Int64),
        TypeRef::F64 => no_out(CType::Double),
        TypeRef::Bool => no_out(CType::Bool),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => no_out(CType::const_ptr(CType::Char)),
        TypeRef::Bytes | TypeRef::BorrowedBytes => AbiReturn {
            ret: CType::const_ptr(CType::Uint8),
            out_params: vec![AbiParam::new("out_len", CType::ptr(CType::Size))],
        },
        TypeRef::Handle => no_out(CType::Handle),
        TypeRef::TypedHandle(n) => no_out(CType::ptr(CType::StructTag {
            module: module.to_string(),
            name: n.clone(),
        })),
        TypeRef::Struct(s) => no_out(CType::ptr(struct_tag(s, module))),
        TypeRef::Enum(e) => no_out(CType::Enum {
            module: module.to_string(),
            name: e.clone(),
        }),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                lower_return(inner, module)
            } else {
                no_out(CType::ptr(element_ctype(inner, module)))
            }
        }
        TypeRef::List(inner) => AbiReturn {
            ret: CType::ptr(element_ctype(inner, module)),
            out_params: vec![AbiParam::new("out_len", CType::ptr(CType::Size))],
        },
        TypeRef::Map(k, v) => AbiReturn {
            ret: CType::Void,
            out_params: vec![
                AbiParam::new("out_keys", CType::ptr(element_ctype(k, module))),
                AbiParam::new("out_values", CType::ptr(element_ctype(v, module))),
                AbiParam::new("out_len", CType::ptr(CType::Size)),
            ],
        },
        TypeRef::Iterator(_) => {
            unreachable!("iterator return handled specially by the function lowering")
        }
    }
}

/// The trailing result fields appended to an async callback after the
/// `(context, err)` prefix.
pub fn callback_result_params(ty: &TypeRef, module: &str) -> Vec<AbiParam> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
            AbiParam::new("result", CType::const_ptr(CType::Uint8)),
            AbiParam::new("result_len", CType::Size),
        ],
        TypeRef::List(inner) => vec![
            AbiParam::new("result", CType::ptr(element_ctype(inner, module))),
            AbiParam::new("result_len", CType::Size),
        ],
        TypeRef::Map(k, v) => vec![
            AbiParam::new("result_keys", CType::ptr(element_ctype(k, module))),
            AbiParam::new("result_values", CType::ptr(element_ctype(v, module))),
            AbiParam::new("result_len", CType::Size),
        ],
        _ => {
            let ret = lower_return(ty, module).ret;
            vec![AbiParam::new("result", ret)]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(params: &[AbiParam]) -> Vec<String> {
        params
            .iter()
            .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
            .collect()
    }

    #[test]
    fn scalar_param() {
        assert_eq!(
            render(&lower_param("x", &TypeRef::I32, "m", false)),
            ["int32_t x"]
        );
    }

    #[test]
    fn string_param_is_const_unless_mutable() {
        assert_eq!(
            render(&lower_param("s", &TypeRef::StringUtf8, "m", false)),
            ["const char* s"]
        );
        assert_eq!(
            render(&lower_param("s", &TypeRef::StringUtf8, "m", true)),
            ["char* s"]
        );
    }

    #[test]
    fn bytes_param_expands_to_ptr_and_len() {
        assert_eq!(
            render(&lower_param("data", &TypeRef::Bytes, "m", false)),
            ["const uint8_t* data_ptr", "size_t data_len"]
        );
    }

    #[test]
    fn list_of_scalar_uses_west_const() {
        let xs = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(
            render(&lower_param("xs", &xs, "m", false)),
            ["const int32_t* xs", "size_t xs_len"]
        );
    }

    #[test]
    fn list_of_string_uses_east_const() {
        let xs = TypeRef::List(Box::new(TypeRef::StringUtf8));
        assert_eq!(
            render(&lower_param("xs", &xs, "m", false)),
            ["const char* const* xs", "size_t xs_len"]
        );
    }

    #[test]
    fn optional_scalar_is_pointer() {
        let o = TypeRef::Optional(Box::new(TypeRef::I32));
        assert_eq!(
            render(&lower_param("x", &o, "m", false)),
            ["const int32_t* x"]
        );
    }

    #[test]
    fn optional_string_is_just_the_pointer() {
        let o = TypeRef::Optional(Box::new(TypeRef::StringUtf8));
        assert_eq!(render(&lower_param("s", &o, "m", false)), ["const char* s"]);
    }

    #[test]
    fn map_param_is_parallel_arrays() {
        let m = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        assert_eq!(
            render(&lower_param("m", &m, "mod", false)),
            [
                "const char* const* m_keys",
                "const int32_t* m_values",
                "size_t m_len"
            ]
        );
    }

    #[test]
    fn bytes_return_has_out_len() {
        let r = lower_return(&TypeRef::Bytes, "m");
        assert_eq!(r.ret.render_c("weaveffi"), "const uint8_t*");
        assert_eq!(render(&r.out_params), ["size_t* out_len"]);
    }

    #[test]
    fn map_return_is_void_with_triple_out() {
        let m = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        let r = lower_return(&m, "mod");
        assert_eq!(r.ret, CType::Void);
        assert_eq!(
            render(&r.out_params),
            [
                "const char** out_keys",
                "int32_t* out_values",
                "size_t* out_len"
            ]
        );
    }

    #[test]
    fn struct_return_is_pointer() {
        let r = lower_return(&TypeRef::Struct("Contact".into()), "contacts");
        assert_eq!(r.ret.render_c("weaveffi"), "weaveffi_contacts_Contact*");
    }

    #[test]
    fn cross_module_struct_param_resolves_module() {
        let p = lower_param("c", &TypeRef::Struct("other.Contact".into()), "ops", false);
        assert_eq!(render(&p), ["const weaveffi_other_Contact* c"]);
    }
}
