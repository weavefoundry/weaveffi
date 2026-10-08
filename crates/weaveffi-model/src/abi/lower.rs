//! The structural lowering: how each [`Ty`] maps onto C ABI parameter and
//! return slots. This is the single source of truth every generator shares.
//!
//! The lowering dispatches on [`Ty::family`]:
//!
//! * [`Family::Direct`] types occupy one C slot by value: scalars, bools, and
//!   C-style enums.
//! * [`Family::String`] and [`Family::Bytes`] are a `const uint8_t*` +
//!   `size_t` pair (UTF-8 for strings, never NUL-terminated).
//! * [`Family::Buffer`] types (records, rich enums, optionals, lists, and
//!   maps) cross as one serialized value buffer: a `const uint8_t*` +
//!   `size_t` pair encoded in the WeaveFFI buffer format. A buffered
//!   parameter is borrowed for the call; a buffered return is
//!   producer-allocated and released with `{prefix}_free_bytes` after
//!   decoding.
//! * [`Family::Object`] is an interface pointer, borrowed as a parameter and
//!   one strong reference as a return; a nullable one is `Interface?`.
//! * [`Family::Callback`] is a `void* ctx` plus `const {tag}_vtable*` pair,
//!   only ever a parameter; a nullable one (`Cb?`) passes a null vtable for
//!   none.
//!
//! A callback-interface method's return lowers differently from a producer
//! return ([`lower_callback_return`]): the consumer produces the value, so a
//! string, bytes, or buffer return is a run the consumer allocates with
//! `{prefix}_alloc` and hands back through `uint8_t** out_ptr` and
//! `size_t* out_len`.

use crate::ty::{Family, Prim, Ty, TypeIndex};

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

/// The underscore-joined C path of the module declaring the user type
/// `name`.
///
/// # Panics
///
/// Panics when `name` isn't declared. Only records and rich enums may be
/// undeclared (the proc-macro assumes a name from a sibling module tree is
/// one of those), and they cross as value buffers, which never name a C
/// type.
fn owner_path(name: &str, types: &TypeIndex) -> String {
    types
        .module_path(name)
        .unwrap_or_else(|| panic!("type '{name}' is not declared"))
        .to_string()
}

/// The opaque C tag type of the interface `name`.
fn struct_tag(name: &str, types: &TypeIndex) -> CType {
    CType::StructTag {
        module: owner_path(name, types),
        name: name.to_string(),
    }
}

/// The vtable struct type of the callback interface `name`.
fn vtable_tag(name: &str, types: &TypeIndex) -> CType {
    CType::VtableTag {
        module: owner_path(name, types),
        name: name.to_string(),
    }
}

/// The by-value C type of a [`Family::Direct`] type.
fn direct_ctype(ty: &Ty, types: &TypeIndex) -> CType {
    match ty {
        Ty::Prim(p) => match p {
            Prim::I8 => CType::Int8,
            Prim::I16 => CType::Int16,
            Prim::I32 => CType::Int32,
            Prim::I64 => CType::Int64,
            Prim::U8 => CType::Uint8,
            Prim::U16 => CType::Uint16,
            Prim::U32 => CType::Uint32,
            Prim::U64 => CType::Uint64,
            Prim::F32 => CType::Float,
            Prim::F64 => CType::Double,
            Prim::Bool => CType::Bool,
            Prim::String | Prim::Bytes => unreachable!("{p} is not a direct type"),
        },
        Ty::Enum(e) => CType::Enum {
            module: owner_path(e, types),
            name: e.clone(),
        },
        other => unreachable!("{other} is not a direct type"),
    }
}

/// The two slots of a borrowed `(ptr, len)` parameter: `const uint8_t*
/// {name}_ptr` and `size_t {name}_len`. Shared by bytes and buffered values.
fn ptr_len_slots(name: &str) -> Vec<AbiParam> {
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
pub fn lower_param(name: &str, ty: &Ty, types: &TypeIndex) -> Vec<AbiParam> {
    match ty.family() {
        Family::Direct => vec![AbiParam::new(name, direct_ctype(ty, types))],
        Family::String | Family::Bytes | Family::Buffer => ptr_len_slots(name),
        // An interface parameter borrows the object for the call: the callee
        // reads through the const pointer and clones if it wants to retain
        // the object. A nullable one is the same slot with null meaning none.
        Family::Object { .. } => {
            let iface = ty
                .interface_name()
                .expect("object family names an interface");
            vec![AbiParam::new(
                name,
                CType::Ptr {
                    konst: ConstPos::West,
                    pointee: Box::new(struct_tag(iface, types)),
                },
            )]
        }
        // A callback interface is an opaque consumer context plus the
        // consumer's static vtable for the interface. A nullable one passes a
        // null vtable for none.
        Family::Callback { .. } => {
            let cb = ty
                .callback_interface_name()
                .expect("callback family names a callback interface");
            vec![
                AbiParam::new(format!("{name}_ctx"), CType::ptr(CType::Void)),
                AbiParam::new(
                    format!("{name}_vtable"),
                    CType::const_ptr(vtable_tag(cb, types)),
                ),
            ]
        }
        Family::Iterator => unreachable!("iterator not valid as parameter"),
    }
}

/// Lower a return type to its C return type plus trailing out-parameters.
///
/// # Panics
///
/// Panics on an iterator type, whose launcher is lowered by the function
/// lowering in [`crate::model`] rather than as a plain value return, and on a
/// callback interface, which validation never admits as a return.
pub fn lower_return(ty: &Ty, types: &TypeIndex) -> AbiReturn {
    let no_out = |ret| AbiReturn {
        ret,
        out_params: vec![],
    };
    match ty.family() {
        Family::Direct => no_out(direct_ctype(ty, types)),
        // String, bytes, and buffered returns are producer-allocated byte
        // runs: the caller copies or decodes them and then calls
        // `{prefix}_free_bytes(ptr, len)`.
        Family::String | Family::Bytes | Family::Buffer => AbiReturn {
            ret: CType::const_ptr(CType::Uint8),
            out_params: vec![AbiParam::new("out_len", CType::ptr(CType::Size))],
        },
        // A returned interface transfers one strong reference; a nullable one
        // may be null.
        Family::Object { .. } => {
            let iface = ty
                .interface_name()
                .expect("object family names an interface");
            no_out(CType::ptr(struct_tag(iface, types)))
        }
        Family::Callback { .. } => unreachable!("callback interfaces are never returned"),
        Family::Iterator => {
            unreachable!("iterator return handled specially by the function lowering")
        }
    }
}

/// Lower the return type of a callback-interface method to its C return type
/// plus trailing out-parameters (placed before the method's `out_err`).
///
/// A direct value is returned by value and an object as one strong
/// reference (`{tag}*`) the producer adopts. A string, bytes, or buffer
/// return is `void` with `uint8_t** out_ptr` and `size_t* out_len` slots:
/// the consumer allocates the run with `{prefix}_alloc`, writes it there,
/// and the producer adopts and frees it.
///
/// # Panics
///
/// Panics on an iterator or a callback interface, which validation never
/// admits as a callback method return.
pub fn lower_callback_return(ty: &Ty, types: &TypeIndex) -> AbiReturn {
    match ty.family() {
        Family::String | Family::Bytes | Family::Buffer => AbiReturn {
            ret: CType::Void,
            out_params: vec![
                AbiParam::new("out_ptr", CType::ptr(CType::ptr(CType::Uint8))),
                AbiParam::new("out_len", CType::ptr(CType::Size)),
            ],
        },
        Family::Callback { .. } | Family::Iterator => {
            unreachable!("{ty} is never a callback method return")
        }
        _ => lower_return(ty, types),
    }
}

/// The trailing result fields appended to an async callback after the
/// `(context, err)` prefix.
///
/// String, bytes, and buffered results are passed as an owned `ptr` + `len`
/// pair (released by the consumer with `{prefix}_free_bytes`); everything
/// else reuses its return slot type by value.
pub fn callback_result_params(ty: &Ty, types: &TypeIndex) -> Vec<AbiParam> {
    match ty.family() {
        Family::String | Family::Bytes | Family::Buffer => vec![
            AbiParam::new("result_ptr", CType::const_ptr(CType::Uint8)),
            AbiParam::new("result_len", CType::Size),
        ],
        _ => vec![AbiParam::new("result", lower_return(ty, types).ret)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::{TypeDecl, TypeKind};

    fn render(params: &[AbiParam]) -> Vec<String> {
        params
            .iter()
            .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
            .collect()
    }

    /// `Store` in `kv`, `Status` in `shared`, `Listener` in `events`.
    fn types() -> TypeIndex {
        let mut types = TypeIndex::default();
        for (module, name, kind) in [
            ("kv", "Store", TypeKind::Interface),
            ("shared", "Status", TypeKind::Enum),
            ("events", "Listener", TypeKind::CallbackInterface),
        ] {
            let module = types.push_module(module.into());
            types.declare(
                name,
                TypeDecl {
                    kind,
                    module,
                    index: 0,
                },
            );
        }
        types
    }

    const I32: Ty = Ty::Prim(Prim::I32);

    #[test]
    fn params_lower_by_family() {
        let t = types();
        assert_eq!(render(&lower_param("x", &I32, &t)), ["int32_t x"]);
        assert_eq!(
            render(&lower_param("s", &Ty::Prim(Prim::String), &t)),
            ["const uint8_t* s_ptr", "size_t s_len"]
        );
        assert_eq!(
            render(&lower_param("data", &Ty::Prim(Prim::Bytes), &t)),
            ["const uint8_t* data_ptr", "size_t data_len"]
        );
        assert_eq!(
            render(&lower_param("xs", &Ty::List(Box::new(I32)), &t)),
            ["const uint8_t* xs_ptr", "size_t xs_len"]
        );
        // An undeclared record (a proc-macro's sibling-tree type) is fine:
        // value buffers never name a C type.
        assert_eq!(
            render(&lower_param("c", &Ty::Record("Contact".into()), &t)),
            ["const uint8_t* c_ptr", "size_t c_len"]
        );
        assert_eq!(
            render(&lower_param(
                "s",
                &Ty::Optional(Box::new(Ty::Interface("Store".into()))),
                &t
            )),
            ["const weaveffi_kv_Store* s"]
        );
        assert_eq!(
            render(&lower_param("s", &Ty::Enum("Status".into()), &t)),
            ["weaveffi_shared_Status s"]
        );
        assert_eq!(
            render(&lower_param(
                "listener",
                &Ty::CallbackInterface("Listener".into()),
                &t
            )),
            [
                "void* listener_ctx",
                "const weaveffi_events_Listener_vtable* listener_vtable"
            ]
        );
    }

    #[test]
    fn returns_lower_by_family() {
        let t = types();
        let r = lower_return(&Ty::Prim(Prim::Bytes), &t);
        assert_eq!(r.ret.render_c("weaveffi"), "const uint8_t*");
        assert_eq!(render(&r.out_params), ["size_t* out_len"]);
        for ty in [
            Ty::Record("Contact".into()),
            Ty::RichEnum("Shape".into()),
            Ty::List(Box::new(Ty::Record("Contact".into()))),
            Ty::List(Box::new(Ty::Interface("Store".into()))),
            Ty::Map(Box::new(Ty::Prim(Prim::String)), Box::new(I32)),
            Ty::Optional(Box::new(Ty::Prim(Prim::I64))),
        ] {
            let r = lower_return(&ty, &t);
            assert_eq!(r.ret.render_c("weaveffi"), "const uint8_t*", "{ty}");
            assert_eq!(render(&r.out_params), ["size_t* out_len"], "{ty}");
        }
        let r = lower_return(&Ty::Optional(Box::new(Ty::Interface("Store".into()))), &t);
        assert_eq!(r.ret.render_c("weaveffi"), "weaveffi_kv_Store*");
        assert!(r.out_params.is_empty());
        assert_eq!(
            lower_return(&Ty::Enum("Status".into()), &t)
                .ret
                .render_c("weaveffi"),
            "weaveffi_shared_Status"
        );
    }

    #[test]
    fn callback_method_returns_use_consumer_allocated_runs() {
        let t = types();
        let r = lower_callback_return(&Ty::Prim(Prim::String), &t);
        assert_eq!(r.ret, CType::Void);
        assert_eq!(
            render(&r.out_params),
            ["uint8_t** out_ptr", "size_t* out_len"]
        );
        let r = lower_callback_return(&Ty::Interface("Store".into()), &t);
        assert_eq!(r.ret.render_c("weaveffi"), "weaveffi_kv_Store*");
        assert!(r.out_params.is_empty());
        assert_eq!(lower_callback_return(&I32, &t).ret, CType::Int32);
        assert_eq!(
            render(&lower_param(
                "l",
                &Ty::Optional(Box::new(Ty::CallbackInterface("Listener".into()))),
                &t
            )),
            [
                "void* l_ctx",
                "const weaveffi_events_Listener_vtable* l_vtable"
            ]
        );
    }

    #[test]
    fn callback_results() {
        let t = types();
        assert_eq!(
            render(&callback_result_params(
                &Ty::List(Box::new(Ty::Prim(Prim::String))),
                &t
            )),
            ["const uint8_t* result_ptr", "size_t result_len"]
        );
        assert_eq!(
            render(&callback_result_params(&Ty::Prim(Prim::Bytes), &t)),
            ["const uint8_t* result_ptr", "size_t result_len"]
        );
        assert_eq!(
            render(&callback_result_params(&I32, &t)),
            ["int32_t result"]
        );
    }
}
