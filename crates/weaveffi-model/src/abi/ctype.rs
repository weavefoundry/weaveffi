//! The C-type algebra used by the ABI model.
//!
//! [`CType`] is a prefix-agnostic description of a single C type as it appears
//! in a WeaveFFI ABI signature. The canonical C rendering lives in
//! [`CType::render_c`]; every other language generator maps `CType` onto its
//! own FFI vocabulary (ctypes, P/Invoke, `dart:ffi`, ...). Because the
//! structure is shared, every target agrees on the calling convention by
//! construction.

use crate::ty::Prim;

/// Placement of a `const` qualifier on a pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConstPos {
    /// No `const` (mutable pointer): `T*`.
    None,
    /// West `const`: `const T*`.
    West,
}

/// A single C type in an ABI signature, independent of the configured symbol
/// prefix (applied at render time by [`CType::render_c`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CType {
    /// `int8_t` (the `i8` primitive).
    Int8,
    /// `int16_t` (the `i16` primitive).
    Int16,
    /// `int32_t` (the `i32` primitive).
    Int32,
    /// `int64_t` (the `i64` primitive).
    Int64,
    /// `uint8_t` as a standalone scalar (the `u8` primitive). Also used as the
    /// pointee of a byte-buffer pointer.
    Uint8,
    /// `uint16_t` (the `u16` primitive).
    Uint16,
    /// `uint32_t` (the `u32` primitive).
    Uint32,
    /// `uint64_t` (the `u64` primitive).
    Uint64,
    /// `float` (the `f32` primitive).
    Float,
    /// `double` (the `f64` primitive).
    Double,
    /// `bool` (from `<stdbool.h>`).
    Bool,
    /// `size_t`.
    Size,
    /// `char` (only appears as the pointee of a string pointer).
    Char,
    /// `void`.
    Void,
    /// `{prefix}_cancel_token`.
    CancelToken,
    /// `{prefix}_error`.
    Error,
    /// A C-style enum (or error-domain code) value type:
    /// `{prefix}_{module}_{name}`, which the header declares as
    /// `typedef int32_t`.
    Enum {
        /// Underscore-joined symbol path of the module that declares the enum.
        module: String,
        /// The enum's bare type name.
        name: String,
    },
    /// An interface's opaque object tag: `{prefix}_{module}_{name}`.
    StructTag {
        /// Underscore-joined symbol path of the module that declares the type.
        module: String,
        /// The interface's bare type name.
        name: String,
    },
    /// A callback interface's vtable struct: `{prefix}_{module}_{name}_vtable`.
    VtableTag {
        /// Underscore-joined symbol path of the module that declares the
        /// callback interface.
        module: String,
        /// The callback interface's bare type name.
        name: String,
    },
    /// A prefixed named type emitted elsewhere in the header (async callback
    /// function-pointer typedefs, iterator opaque structs, ...). Renders as
    /// `{prefix}_{core}`.
    Named(String),
    /// A pointer to `pointee` with the given `const` placement.
    Ptr {
        /// Where the `const` qualifier sits, if any.
        konst: ConstPos,
        /// The type the pointer refers to.
        pointee: Box<CType>,
    },
}

impl CType {
    /// The by-value C type of a scalar primitive (`int32_t` for `i32`,
    /// `double` for `f64`, `bool` for `bool`). For `string` and `bytes`,
    /// which cross as byte runs, it's the run's element type, `uint8_t`.
    #[must_use]
    pub fn of_prim(p: Prim) -> CType {
        match p {
            Prim::Bool => CType::Bool,
            Prim::I8 => CType::Int8,
            Prim::I16 => CType::Int16,
            Prim::I32 => CType::Int32,
            Prim::I64 => CType::Int64,
            Prim::U8 | Prim::String | Prim::Bytes => CType::Uint8,
            Prim::U16 => CType::Uint16,
            Prim::U32 => CType::Uint32,
            Prim::U64 => CType::Uint64,
            Prim::F32 => CType::Float,
            Prim::F64 => CType::Double,
        }
    }

    /// Pointer to `pointee` with no `const`.
    pub fn ptr(pointee: CType) -> CType {
        CType::Ptr {
            konst: ConstPos::None,
            pointee: Box::new(pointee),
        }
    }

    /// Pointer to `pointee` with west `const` (`const T*`).
    pub fn const_ptr(pointee: CType) -> CType {
        CType::Ptr {
            konst: ConstPos::West,
            pointee: Box::new(pointee),
        }
    }

    /// Whether this type is represented as a pointer at the C ABI boundary.
    pub fn is_pointer(&self) -> bool {
        matches!(self, CType::Ptr { .. })
    }

    /// Render this type as the Rust `extern "C"` spelling a producer cdylib
    /// uses, with `prefix` for every WeaveFFI-owned symbol.
    ///
    /// This is the Rust counterpart of [`render_c`](Self::render_c): it is the
    /// single source of truth for the `extern "C"` signatures the
    /// `#[weaveffi::module]` macro emits, so a macro-generated signature
    /// matches the generated C header by construction. Notable lowerings:
    ///
    /// * [`Char`](Self::Char) renders bare as `c_char` (the emitted code
    ///   imports `std::os::raw::c_char`), and [`Void`](Self::Void) as
    ///   `std::ffi::c_void` (only meaningful as a pointee; a bare `void`
    ///   *return* is the absence of a `-> T`, handled by the caller).
    /// * A C-style [`Enum`](Self::Enum) crosses the ABI as its discriminant,
    ///   so it lowers to `i32` (matching the header's `typedef int32_t`).
    /// * A `const` pointer is `*const`; a non-`const` pointer is `*mut`.
    pub fn render_rust(&self, prefix: &str) -> String {
        match self {
            CType::Int8 => "i8".to_string(),
            CType::Int16 => "i16".to_string(),
            CType::Int32 => "i32".to_string(),
            CType::Int64 => "i64".to_string(),
            CType::Uint8 => "u8".to_string(),
            CType::Uint16 => "u16".to_string(),
            CType::Uint32 => "u32".to_string(),
            CType::Uint64 => "u64".to_string(),
            CType::Float => "f32".to_string(),
            CType::Double => "f64".to_string(),
            CType::Bool => "bool".to_string(),
            CType::Size => "usize".to_string(),
            CType::Char => "c_char".to_string(),
            CType::Void => "std::ffi::c_void".to_string(),
            // The runtime types come from `weaveffi::abi` and keep their fixed
            // names regardless of the configured business-symbol prefix.
            CType::CancelToken => "::weaveffi::abi::FfiCancelToken".to_string(),
            CType::Error => "::weaveffi::abi::FfiError".to_string(),
            // A C-style enum is passed/returned as its int discriminant.
            CType::Enum { .. } => "i32".to_string(),
            CType::StructTag { module, name } => format!("{prefix}_{module}_{name}"),
            CType::VtableTag { module, name } => format!("{prefix}_{module}_{name}_vtable"),
            CType::Named(core) => format!("{prefix}_{core}"),
            CType::Ptr { konst, pointee } => {
                let inner = pointee.render_rust(prefix);
                match konst {
                    ConstPos::None => format!("*mut {inner}"),
                    ConstPos::West => format!("*const {inner}"),
                }
            }
        }
    }

    /// Render this type as canonical C source using `prefix` for every
    /// WeaveFFI-owned symbol.
    pub fn render_c(&self, prefix: &str) -> String {
        match self {
            CType::Int8 => "int8_t".to_string(),
            CType::Int16 => "int16_t".to_string(),
            CType::Int32 => "int32_t".to_string(),
            CType::Int64 => "int64_t".to_string(),
            CType::Uint8 => "uint8_t".to_string(),
            CType::Uint16 => "uint16_t".to_string(),
            CType::Uint32 => "uint32_t".to_string(),
            CType::Uint64 => "uint64_t".to_string(),
            CType::Float => "float".to_string(),
            CType::Double => "double".to_string(),
            CType::Bool => "bool".to_string(),
            CType::Size => "size_t".to_string(),
            CType::Char => "char".to_string(),
            CType::Void => "void".to_string(),
            CType::CancelToken => format!("{prefix}_cancel_token"),
            CType::Error => format!("{prefix}_error"),
            CType::Enum { module, name } => format!("{prefix}_{module}_{name}"),
            CType::StructTag { module, name } => format!("{prefix}_{module}_{name}"),
            CType::VtableTag { module, name } => format!("{prefix}_{module}_{name}_vtable"),
            CType::Named(core) => format!("{prefix}_{core}"),
            CType::Ptr { konst, pointee } => {
                let inner = pointee.render_c(prefix);
                match konst {
                    ConstPos::None => format!("{inner}*"),
                    ConstPos::West => format!("const {inner}*"),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_render() {
        assert_eq!(CType::Int32.render_c("weaveffi"), "int32_t");
        assert_eq!(CType::Size.render_c("weaveffi"), "size_t");
        assert_eq!(CType::Error.render_c("myffi"), "myffi_error");
    }

    #[test]
    fn string_pointer_renders_with_west_const() {
        let s = CType::const_ptr(CType::Char);
        assert_eq!(s.render_c("weaveffi"), "const char*");
        assert_eq!(CType::ptr(CType::Char).render_c("weaveffi"), "char*");
    }

    #[test]
    fn struct_and_vtable_tags_use_prefix_and_module() {
        let t = CType::StructTag {
            module: "contacts".into(),
            name: "Book".into(),
        };
        assert_eq!(t.render_c("weaveffi"), "weaveffi_contacts_Book");
        assert_eq!(
            CType::ptr(t).render_c("weaveffi"),
            "weaveffi_contacts_Book*"
        );
        let v = CType::VtableTag {
            module: "events".into(),
            name: "Listener".into(),
        };
        assert_eq!(
            CType::const_ptr(v.clone()).render_c("weaveffi"),
            "const weaveffi_events_Listener_vtable*"
        );
        assert_eq!(
            CType::const_ptr(v).render_rust("weaveffi"),
            "*const weaveffi_events_Listener_vtable"
        );
    }

    #[test]
    fn named_type_is_prefixed() {
        assert_eq!(
            CType::Named("events_fetch_callback".into()).render_c("weaveffi"),
            "weaveffi_events_fetch_callback"
        );
    }

    #[test]
    fn rust_scalars_render() {
        assert_eq!(CType::Int32.render_rust("weaveffi"), "i32");
        assert_eq!(CType::Uint8.render_rust("weaveffi"), "u8");
        assert_eq!(CType::Float.render_rust("weaveffi"), "f32");
        assert_eq!(CType::Double.render_rust("weaveffi"), "f64");
        assert_eq!(CType::Bool.render_rust("weaveffi"), "bool");
        assert_eq!(CType::Size.render_rust("weaveffi"), "usize");
    }

    #[test]
    fn rust_runtime_types_name_the_runtime_structs() {
        assert_eq!(
            CType::Error.render_rust("acme"),
            "::weaveffi::abi::FfiError"
        );
        assert_eq!(
            CType::CancelToken.render_rust("acme"),
            "::weaveffi::abi::FfiCancelToken"
        );
    }

    #[test]
    fn rust_string_param_is_const_char_pointer() {
        assert_eq!(
            CType::const_ptr(CType::Char).render_rust("weaveffi"),
            "*const c_char"
        );
        assert_eq!(
            CType::ptr(CType::Char).render_rust("weaveffi"),
            "*mut c_char"
        );
    }

    #[test]
    fn rust_c_style_enum_lowers_to_i32() {
        let e = CType::Enum {
            module: "gfx".into(),
            name: "Color".into(),
        };
        assert_eq!(e.render_rust("weaveffi"), "i32");
    }

    #[test]
    fn prims_map_to_their_scalar_types() {
        assert_eq!(CType::of_prim(Prim::F64).render_c("p"), "double");
        assert_eq!(CType::of_prim(Prim::U16).render_rust("p"), "u16");
        assert_eq!(CType::of_prim(Prim::Bool), CType::Bool);
        assert_eq!(CType::of_prim(Prim::String), CType::Uint8);
    }

    #[test]
    fn rust_void_is_only_a_pointee() {
        assert_eq!(
            CType::ptr(CType::Void).render_rust("weaveffi"),
            "*mut std::ffi::c_void"
        );
    }
}
