//! The structural lowering: how each [`Ty`] maps onto C ABI parameter and
//! return slots. This is the single source of truth every generator shares.
//!
//! The lowering dispatches on [`Ty::family`]:
//!
//! * [`Family::Direct`] types occupy one C slot by value: scalars, bools,
//!   C-style enums, and handles.
//! * [`Family::String`] is a `char*`; [`Family::Bytes`] is a `ptr` + `len`
//!   pair.
//! * [`Family::Buffer`] types (records, rich enums, optionals, lists, and
//!   maps) cross as one serialized value buffer: a `const uint8_t*` +
//!   `size_t` pair encoded in the WeaveFFI buffer format (`weaveffi-abi`'s
//!   `buffer` module). A buffered parameter is borrowed for the call; a
//!   buffered return is producer-allocated and released with
//!   `{prefix}_free_bytes` after decoding.
//! * [`Family::Object`] is an opaque interface pointer, borrowed as a
//!   parameter and owned as a return; a nullable one is `Interface?`.

use crate::model::{Family, Ty};

use super::ctype::{CType, ConstPos};

/// A named C parameter slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiParam {
    /// The C parameter name (e.g. `out_err`, `data_ptr`, `contact_len`).
    pub name: String,
    /// The C type of the slot.
    pub ty: CType,
}

impl AbiParam {
    /// Build a parameter slot from a name and its C type.
    pub fn new(name: impl Into<String>, ty: CType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// A lowered return: the C return type plus any trailing out-parameters
/// (e.g. `size_t* out_len` for a bytes or buffered return).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiReturn {
    /// The C return type, or `void` when the value is delivered entirely
    /// through [`out_params`](Self::out_params).
    pub ret: CType,
    /// Trailing out-parameter slots appended after the function's inputs.
    pub out_params: Vec<AbiParam>,
}

/// Split a (possibly qualified) type reference into its C module-path segment
/// and bare type name.
///
/// Qualified references use dot-separated module paths (`a.b.Name`); the C ABI
/// flattens those to underscore-joined symbol prefixes (`a_b`). An unqualified
/// reference belongs to `current_module`. Using `rsplit_once` (rather than
/// `split_once`) is what makes *multi-level* nesting work: only the final
/// segment is the type name, everything before it is the module path.
///
/// * `a.b.Name`, current `x`  -> (`a_b`, `Name`)
/// * `shared.Status`, current `orders` -> (`shared`, `Status`)
/// * `Name`, current `a_b`    -> (`a_b`, `Name`)
pub fn split_qualified(name: &str, current_module: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((module_path, type_name)) => (module_path.replace('.', "_"), type_name.to_string()),
        None => (current_module.to_string(), name.to_string()),
    }
}

/// Resolve a struct/interface reference (possibly `module.Name`) to its C tag
/// type.
pub fn struct_tag(name: &str, current_module: &str) -> CType {
    let (module, name) = split_qualified(name, current_module);
    CType::StructTag { module, name }
}

/// The by-value C type of a [`Family::Direct`] type.
fn direct_ctype(ty: &Ty, module: &str) -> CType {
    match ty {
        Ty::I8 => CType::Int8,
        Ty::I16 => CType::Int16,
        Ty::I32 => CType::Int32,
        Ty::I64 => CType::Int64,
        Ty::U8 => CType::Uint8,
        Ty::U16 => CType::Uint16,
        Ty::U32 => CType::Uint32,
        Ty::U64 => CType::Uint64,
        Ty::F32 => CType::Float,
        Ty::F64 => CType::Double,
        Ty::Bool => CType::Bool,
        Ty::Handle => CType::Handle,
        Ty::TypedHandle(n) => CType::ptr(struct_tag(n, module)),
        Ty::Enum(e) => {
            let (module, name) = split_qualified(e, module);
            CType::Enum { module, name }
        }
        other => unreachable!("{other} is not a direct type"),
    }
}

/// The two slots of a borrowed buffered parameter: `const uint8_t* {name}_ptr`
/// and `size_t {name}_len`.
fn buffer_param_slots(name: &str) -> Vec<AbiParam> {
    vec![
        AbiParam::new(format!("{name}_ptr"), CType::const_ptr(CType::Uint8)),
        AbiParam::new(format!("{name}_len"), CType::Size),
    ]
}

/// Expand one parameter into its ordered C ABI slots.
///
/// # Panics
///
/// Panics on an iterator type, which validation never admits as a parameter.
pub fn lower_param(name: &str, ty: &Ty, module: &str, mutable: bool) -> Vec<AbiParam> {
    let west_if_immut = if mutable {
        ConstPos::None
    } else {
        ConstPos::West
    };
    match ty.family() {
        Family::Direct => vec![AbiParam::new(name, direct_ctype(ty, module))],
        Family::String => vec![AbiParam::new(
            name,
            CType::Ptr {
                konst: west_if_immut,
                pointee: Box::new(CType::Char),
            },
        )],
        Family::Bytes => vec![
            AbiParam::new(
                format!("{name}_ptr"),
                CType::Ptr {
                    konst: west_if_immut,
                    pointee: Box::new(CType::Uint8),
                },
            ),
            AbiParam::new(format!("{name}_len"), CType::Size),
        ],
        // A buffered parameter is always an immutable borrow of the encoded
        // value; validation rejects `mutable: true` on buffered types.
        Family::Buffer => buffer_param_slots(name),
        // An interface parameter borrows the object for the call: the callee
        // reads through the const pointer and never takes ownership. A
        // nullable one is the same slot with null meaning none.
        Family::Object { .. } => {
            let iface = ty
                .interface_name()
                .expect("object family names an interface");
            vec![AbiParam::new(
                name,
                CType::Ptr {
                    konst: ConstPos::West,
                    pointee: Box::new(struct_tag(iface, module)),
                },
            )]
        }
        Family::Iterator => unreachable!("iterator not valid as parameter"),
    }
}

/// Lower a return type to its C return type plus trailing out-parameters.
///
/// # Panics
///
/// Panics on an iterator type, whose launcher is lowered by the function
/// lowering in [`crate::model`] rather than as a plain value return.
pub fn lower_return(ty: &Ty, module: &str) -> AbiReturn {
    let no_out = |ret| AbiReturn {
        ret,
        out_params: vec![],
    };
    match ty.family() {
        Family::Direct => no_out(direct_ctype(ty, module)),
        Family::String => no_out(CType::const_ptr(CType::Char)),
        // A buffered return is producer-allocated exactly like a bytes return:
        // the caller decodes it and then calls `{prefix}_free_bytes`.
        Family::Bytes | Family::Buffer => AbiReturn {
            ret: CType::const_ptr(CType::Uint8),
            out_params: vec![AbiParam::new("out_len", CType::ptr(CType::Size))],
        },
        // A returned interface transfers ownership of a new object reference;
        // a nullable one may be null.
        Family::Object { .. } => {
            let iface = ty
                .interface_name()
                .expect("object family names an interface");
            no_out(CType::ptr(struct_tag(iface, module)))
        }
        Family::Iterator => {
            unreachable!("iterator return handled specially by the function lowering")
        }
    }
}

/// The trailing result fields appended to an async callback after the
/// `(context, err)` prefix.
///
/// Bytes and buffered results are passed as an owned `ptr` + `len` pair
/// (released by the consumer with `{prefix}_free_bytes`); everything else
/// reuses its return slot type by value.
pub fn callback_result_params(ty: &Ty, module: &str) -> Vec<AbiParam> {
    match ty.family() {
        Family::Buffer => vec![
            AbiParam::new("result_ptr", CType::const_ptr(CType::Uint8)),
            AbiParam::new("result_len", CType::Size),
        ],
        Family::Bytes => vec![
            AbiParam::new("result", CType::const_ptr(CType::Uint8)),
            AbiParam::new("result_len", CType::Size),
        ],
        _ => vec![AbiParam::new("result", lower_return(ty, module).ret)],
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
    fn params_lower_by_family() {
        assert_eq!(
            render(&lower_param("x", &Ty::I32, "m", false)),
            ["int32_t x"]
        );
        assert_eq!(
            render(&lower_param("s", &Ty::StringUtf8, "m", false)),
            ["const char* s"]
        );
        assert_eq!(
            render(&lower_param("s", &Ty::StringUtf8, "m", true)),
            ["char* s"]
        );
        assert_eq!(
            render(&lower_param("data", &Ty::Bytes, "m", false)),
            ["const uint8_t* data_ptr", "size_t data_len"]
        );
        assert_eq!(
            render(&lower_param("xs", &Ty::List(Box::new(Ty::I32)), "m", false)),
            ["const uint8_t* xs_ptr", "size_t xs_len"]
        );
        assert_eq!(
            render(&lower_param(
                "c",
                &Ty::Record("other.Contact".into()),
                "ops",
                false
            )),
            ["const uint8_t* c_ptr", "size_t c_len"]
        );
        assert_eq!(
            render(&lower_param(
                "s",
                &Ty::Optional(Box::new(Ty::Interface("Store".into()))),
                "kv",
                false
            )),
            ["const weaveffi_kv_Store* s"]
        );
        assert_eq!(
            render(&lower_param(
                "s",
                &Ty::Enum("shared.Status".into()),
                "orders",
                false
            )),
            ["weaveffi_shared_Status s"]
        );
        assert_eq!(
            render(&lower_param(
                "h",
                &Ty::TypedHandle("auth.Session".into()),
                "api",
                false
            )),
            ["weaveffi_auth_Session* h"]
        );
    }

    #[test]
    fn returns_lower_by_family() {
        let r = lower_return(&Ty::Bytes, "m");
        assert_eq!(r.ret.render_c("weaveffi"), "const uint8_t*");
        assert_eq!(render(&r.out_params), ["size_t* out_len"]);
        for ty in [
            Ty::Record("Contact".into()),
            Ty::RichEnum("Shape".into()),
            Ty::List(Box::new(Ty::Record("Contact".into()))),
            Ty::Map(Box::new(Ty::StringUtf8), Box::new(Ty::I32)),
            Ty::Optional(Box::new(Ty::I64)),
        ] {
            let r = lower_return(&ty, "m");
            assert_eq!(r.ret.render_c("weaveffi"), "const uint8_t*", "{ty}");
            assert_eq!(render(&r.out_params), ["size_t* out_len"], "{ty}");
        }
        let r = lower_return(&Ty::Optional(Box::new(Ty::Interface("Store".into()))), "kv");
        assert_eq!(r.ret.render_c("weaveffi"), "weaveffi_kv_Store*");
        assert!(r.out_params.is_empty());
        assert_eq!(
            lower_return(&Ty::Enum("shared.Status".into()), "orders")
                .ret
                .render_c("weaveffi"),
            "weaveffi_shared_Status"
        );
    }

    #[test]
    fn callback_results() {
        assert_eq!(
            render(&callback_result_params(
                &Ty::List(Box::new(Ty::StringUtf8)),
                "m"
            )),
            ["const uint8_t* result_ptr", "size_t result_len"]
        );
        assert_eq!(
            render(&callback_result_params(&Ty::Bytes, "m")),
            ["const uint8_t* result", "size_t result_len"]
        );
        assert_eq!(
            render(&callback_result_params(&Ty::I32, "m")),
            ["int32_t result"]
        );
    }

    #[test]
    fn split_qualified_handles_levels() {
        assert_eq!(
            split_qualified("Name", "current"),
            ("current".to_string(), "Name".to_string())
        );
        assert_eq!(
            split_qualified("a.b.c.Name", "x"),
            ("a_b_c".to_string(), "Name".to_string())
        );
    }
}
