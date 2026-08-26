//! The structural lowering: how each [`TypeRef`] maps onto C ABI parameter
//! and return slots. This is the single source of truth every generator
//! shares.
//!
//! The lowering splits every type into one of two families:
//!
//! * **Direct** types occupy dedicated C slots: scalars, bools, and C-style
//!   enums by value; strings as `const char*`; bytes as `ptr` + `len`;
//!   handles as `uint64_t`; interfaces and iterators as opaque pointers.
//! * **Buffered** types (records, rich enums, optionals, lists, and maps;
//!   see [`is_buffered`]) cross as one serialized value buffer: a
//!   `const uint8_t*` + `size_t` pair encoded in the WeaveFFI buffer format
//!   (`weaveffi-abi`'s `buffer` module). A buffered parameter is borrowed for
//!   the call; a buffered return is producer-allocated and released with
//!   `{prefix}_free_bytes` after decoding. The single exception is an
//!   optional interface, which stays a nullable object pointer.

use weaveffi_ir::ir::TypeRef;

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

/// `true` when `ty` crosses the C ABI as a serialized value buffer
/// (`const uint8_t*` + `size_t`) rather than as dedicated C slots.
///
/// Buffered types are records, rich enums, lists, maps, and optionals, with
/// one exception: an optional *interface* stays a nullable object pointer
/// (an object reference cannot be serialized by value).
pub fn is_buffered(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => !matches!(inner.as_ref(), TypeRef::Interface(_)),
        _ => false,
    }
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

/// Resolve an enum reference (possibly `module.Name`) to its C enum type.
fn enum_ctype(name: &str, current_module: &str) -> CType {
    let (module, name) = split_qualified(name, current_module);
    CType::Enum { module, name }
}

/// Resolve a typed-handle reference (possibly `module.Name`) to its C
/// `struct Tag*` pointer type.
fn typed_handle_ctype(name: &str, current_module: &str) -> CType {
    let (module, name) = split_qualified(name, current_module);
    CType::ptr(CType::StructTag { module, name })
}

/// Resolve an interface reference (possibly `module.Name`) to a pointer to its
/// opaque C tag.
fn interface_ptr_ctype(name: &str, current_module: &str) -> CType {
    CType::ptr(struct_tag(name, current_module))
}

/// The two slots of a borrowed buffered parameter: `const uint8_t* {name}_ptr`
/// and `size_t {name}_len`.
fn buffer_param_slots(name: &str) -> Vec<AbiParam> {
    vec![
        AbiParam::new(format!("{name}_ptr"), CType::const_ptr(CType::Uint8)),
        AbiParam::new(format!("{name}_len"), CType::Size),
    ]
}

/// Expand one IR parameter into its ordered C ABI slots.
pub fn lower_param(name: &str, ty: &TypeRef, module: &str, mutable: bool) -> Vec<AbiParam> {
    let west_if_immut = if mutable {
        ConstPos::None
    } else {
        ConstPos::West
    };
    if is_buffered(ty) {
        // A buffered parameter is always an immutable borrow of the encoded
        // value; validation rejects `mutable: true` on buffered types.
        return buffer_param_slots(name);
    }
    match ty {
        TypeRef::I8 => vec![AbiParam::new(name, CType::Int8)],
        TypeRef::I16 => vec![AbiParam::new(name, CType::Int16)],
        TypeRef::I32 => vec![AbiParam::new(name, CType::Int32)],
        TypeRef::I64 => vec![AbiParam::new(name, CType::Int64)],
        TypeRef::U8 => vec![AbiParam::new(name, CType::Uint8)],
        TypeRef::U16 => vec![AbiParam::new(name, CType::Uint16)],
        TypeRef::U32 => vec![AbiParam::new(name, CType::Uint32)],
        TypeRef::U64 => vec![AbiParam::new(name, CType::Uint64)],
        TypeRef::F32 => vec![AbiParam::new(name, CType::Float)],
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
        TypeRef::TypedHandle(n) => vec![AbiParam::new(name, typed_handle_ctype(n, module))],
        TypeRef::Named(n) => unreachable!("unresolved type reference '{n}' reached ABI lowering"),
        // An interface parameter borrows the object for the call: the callee
        // reads through the const pointer and never takes ownership.
        TypeRef::Interface(i) => vec![AbiParam::new(
            name,
            CType::Ptr {
                konst: ConstPos::West,
                pointee: Box::new(struct_tag(i, module)),
            },
        )],
        TypeRef::Enum(e) => vec![AbiParam::new(name, enum_ctype(e, module))],
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable borrowed object pointer, null meaning none.
        TypeRef::Optional(inner) => lower_param(name, inner, module, mutable),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
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
    if is_buffered(ty) {
        // A buffered return is producer-allocated, exactly like a bytes
        // return: the caller decodes it and then calls `{prefix}_free_bytes`.
        return AbiReturn {
            ret: CType::const_ptr(CType::Uint8),
            out_params: vec![AbiParam::new("out_len", CType::ptr(CType::Size))],
        };
    }
    match ty {
        TypeRef::I8 => no_out(CType::Int8),
        TypeRef::I16 => no_out(CType::Int16),
        TypeRef::I32 => no_out(CType::Int32),
        TypeRef::I64 => no_out(CType::Int64),
        TypeRef::U8 => no_out(CType::Uint8),
        TypeRef::U16 => no_out(CType::Uint16),
        TypeRef::U32 => no_out(CType::Uint32),
        TypeRef::U64 => no_out(CType::Uint64),
        TypeRef::F32 => no_out(CType::Float),
        TypeRef::F64 => no_out(CType::Double),
        TypeRef::Bool => no_out(CType::Bool),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => no_out(CType::const_ptr(CType::Char)),
        TypeRef::Bytes | TypeRef::BorrowedBytes => AbiReturn {
            ret: CType::const_ptr(CType::Uint8),
            out_params: vec![AbiParam::new("out_len", CType::ptr(CType::Size))],
        },
        TypeRef::Handle => no_out(CType::Handle),
        TypeRef::TypedHandle(n) => no_out(typed_handle_ctype(n, module)),
        TypeRef::Named(n) => unreachable!("unresolved type reference '{n}' reached ABI lowering"),
        // A returned interface transfers ownership of a new object reference.
        TypeRef::Interface(i) => no_out(interface_ptr_ctype(i, module)),
        TypeRef::Enum(e) => no_out(enum_ctype(e, module)),
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => lower_return(inner, module),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => {
            unreachable!("iterator return handled specially by the function lowering")
        }
    }
}

/// The trailing result fields appended to an async callback after the
/// `(context, err)` prefix.
///
/// Bytes and buffered results are passed as a borrowed `ptr` + `len` pair
/// (the producer owns the buffer for the callback's duration); everything
/// else reuses its return slot type by value.
pub fn callback_result_params(ty: &TypeRef, module: &str) -> Vec<AbiParam> {
    if is_buffered(ty) {
        return vec![
            AbiParam::new("result_ptr", CType::const_ptr(CType::Uint8)),
            AbiParam::new("result_len", CType::Size),
        ];
    }
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
            AbiParam::new("result", CType::const_ptr(CType::Uint8)),
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
    fn buffered_kinds_are_detected() {
        assert!(is_buffered(&TypeRef::Record("Contact".into())));
        assert!(is_buffered(&TypeRef::RichEnum("Shape".into())));
        assert!(is_buffered(&TypeRef::List(Box::new(TypeRef::I32))));
        assert!(is_buffered(&TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::I32)
        )));
        assert!(is_buffered(&TypeRef::Optional(Box::new(TypeRef::I32))));
        assert!(is_buffered(&TypeRef::Optional(Box::new(
            TypeRef::StringUtf8
        ))));
        // The one optional exception: nullable interface pointers.
        assert!(!is_buffered(&TypeRef::Optional(Box::new(
            TypeRef::Interface("Store".into())
        ))));
        assert!(!is_buffered(&TypeRef::I32));
        assert!(!is_buffered(&TypeRef::StringUtf8));
        assert!(!is_buffered(&TypeRef::Bytes));
        assert!(!is_buffered(&TypeRef::Interface("Store".into())));
        assert!(!is_buffered(&TypeRef::Enum("Color".into())));
    }

    #[test]
    fn list_param_is_one_buffer() {
        let xs = TypeRef::List(Box::new(TypeRef::I32));
        assert_eq!(
            render(&lower_param("xs", &xs, "m", false)),
            ["const uint8_t* xs_ptr", "size_t xs_len"]
        );
    }

    #[test]
    fn record_param_is_one_buffer() {
        let p = lower_param("c", &TypeRef::Record("other.Contact".into()), "ops", false);
        assert_eq!(render(&p), ["const uint8_t* c_ptr", "size_t c_len"]);
    }

    #[test]
    fn optional_scalar_is_buffered() {
        let o = TypeRef::Optional(Box::new(TypeRef::I32));
        assert_eq!(
            render(&lower_param("x", &o, "m", false)),
            ["const uint8_t* x_ptr", "size_t x_len"]
        );
    }

    #[test]
    fn optional_interface_is_nullable_pointer() {
        let o = TypeRef::Optional(Box::new(TypeRef::Interface("Store".into())));
        assert_eq!(
            render(&lower_param("s", &o, "kv", false)),
            ["const weaveffi_kv_Store* s"]
        );
        let r = lower_return(&o, "kv");
        assert_eq!(r.ret.render_c("weaveffi"), "weaveffi_kv_Store*");
        assert!(r.out_params.is_empty());
    }

    #[test]
    fn map_param_is_one_buffer() {
        let m = TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32));
        assert_eq!(
            render(&lower_param("m", &m, "mod", false)),
            ["const uint8_t* m_ptr", "size_t m_len"]
        );
    }

    #[test]
    fn bytes_return_has_out_len() {
        let r = lower_return(&TypeRef::Bytes, "m");
        assert_eq!(r.ret.render_c("weaveffi"), "const uint8_t*");
        assert_eq!(render(&r.out_params), ["size_t* out_len"]);
    }

    #[test]
    fn buffered_returns_share_the_bytes_shape() {
        for ty in [
            TypeRef::Record("Contact".into()),
            TypeRef::RichEnum("Shape".into()),
            TypeRef::List(Box::new(TypeRef::Record("Contact".into()))),
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            TypeRef::Optional(Box::new(TypeRef::I64)),
            TypeRef::List(Box::new(TypeRef::List(Box::new(TypeRef::I32)))),
        ] {
            let r = lower_return(&ty, "m");
            assert_eq!(r.ret.render_c("weaveffi"), "const uint8_t*", "{ty:?}");
            assert_eq!(render(&r.out_params), ["size_t* out_len"], "{ty:?}");
        }
    }

    #[test]
    fn callback_buffered_result_is_borrowed_pair() {
        let params = callback_result_params(&TypeRef::List(Box::new(TypeRef::StringUtf8)), "m");
        assert_eq!(
            render(&params),
            ["const uint8_t* result_ptr", "size_t result_len"]
        );
    }

    #[test]
    fn split_qualified_handles_levels() {
        // Unqualified -> belongs to current module.
        assert_eq!(
            split_qualified("Name", "current"),
            ("current".to_string(), "Name".to_string())
        );
        // Single-level qualified.
        assert_eq!(
            split_qualified("shared.Status", "orders"),
            ("shared".to_string(), "Status".to_string())
        );
        // Multi-level qualified: only the final segment is the type name; the
        // dotted module path flattens to an underscore-joined C prefix.
        assert_eq!(
            split_qualified("a.b.c.Name", "x"),
            ("a_b_c".to_string(), "Name".to_string())
        );
    }

    #[test]
    fn cross_module_enum_param_resolves_module() {
        // Regression: a sibling-module enum must render `weaveffi_<owner>_<Enum>`,
        // never `weaveffi_<current>_<owner>.<Enum>`.
        let p = lower_param("s", &TypeRef::Enum("shared.Status".into()), "orders", false);
        assert_eq!(render(&p), ["weaveffi_shared_Status s"]);
    }

    #[test]
    fn cross_module_enum_return_resolves_module() {
        let r = lower_return(&TypeRef::Enum("shared.Status".into()), "orders");
        assert_eq!(r.ret.render_c("weaveffi"), "weaveffi_shared_Status");
    }

    #[test]
    fn cross_module_typed_handle_param_resolves_module() {
        let p = lower_param(
            "h",
            &TypeRef::TypedHandle("auth.Session".into()),
            "api",
            false,
        );
        assert_eq!(render(&p), ["weaveffi_auth_Session* h"]);
    }
}
