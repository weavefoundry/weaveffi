//! The C-type algebra used by the ABI model.
//!
//! [`CType`] is a prefix-agnostic description of a single C type as it appears
//! in a WeaveFFI ABI signature. The canonical C rendering lives in
//! [`CType::render_c`]; every other language generator maps `CType` onto its
//! own FFI vocabulary (ctypes, P/Invoke, `dart:ffi`, …). Because the structure
//! is shared, every target agrees on the calling convention by construction.

/// Placement of a `const` qualifier on a pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstPos {
    /// No `const` (mutable pointer): `T*`.
    None,
    /// West `const`: `const T*`.
    West,
    /// East `const`: `T const*` (used for arrays of pointer elements).
    East,
}

/// A single C type in an ABI signature, independent of the configured symbol
/// prefix (applied at render time by [`CType::render_c`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CType {
    Int32,
    Uint32,
    Int64,
    Uint64,
    Double,
    Bool,
    /// `size_t`.
    Size,
    /// `char` (only appears as the pointee of a string pointer).
    Char,
    /// `uint8_t` (byte buffers).
    Uint8,
    /// `void`.
    Void,
    /// `{prefix}_handle_t`.
    Handle,
    /// `{prefix}_cancel_token`.
    CancelToken,
    /// `{prefix}_error`.
    Error,
    /// An enum value type: `{prefix}_{module}_{name}`.
    Enum {
        module: String,
        name: String,
    },
    /// A user struct / typed-handle tag: `{prefix}_{module}_{name}`.
    StructTag {
        module: String,
        name: String,
    },
    /// A prefixed named type emitted elsewhere in the header (callback
    /// function-pointer typedefs, iterator opaque structs, …). Renders as
    /// `{prefix}_{core}`.
    Named(String),
    /// A pointer to `pointee` with the given `const` placement.
    Ptr {
        konst: ConstPos,
        pointee: Box<CType>,
    },
}

impl CType {
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
    /// Mirrors the pointer/value split the array renderer keys its east/west
    /// `const` decision on.
    pub fn is_pointer(&self) -> bool {
        matches!(self, CType::Ptr { .. })
    }

    /// Render this type as canonical C source using `prefix` for every
    /// WeaveFFI-owned symbol.
    pub fn render_c(&self, prefix: &str) -> String {
        match self {
            CType::Int32 => "int32_t".to_string(),
            CType::Uint32 => "uint32_t".to_string(),
            CType::Int64 => "int64_t".to_string(),
            CType::Uint64 => "uint64_t".to_string(),
            CType::Double => "double".to_string(),
            CType::Bool => "bool".to_string(),
            CType::Size => "size_t".to_string(),
            CType::Char => "char".to_string(),
            CType::Uint8 => "uint8_t".to_string(),
            CType::Void => "void".to_string(),
            CType::Handle => format!("{prefix}_handle_t"),
            CType::CancelToken => format!("{prefix}_cancel_token"),
            CType::Error => format!("{prefix}_error"),
            CType::Enum { module, name } => format!("{prefix}_{module}_{name}"),
            CType::StructTag { module, name } => format!("{prefix}_{module}_{name}"),
            CType::Named(core) => format!("{prefix}_{core}"),
            CType::Ptr { konst, pointee } => {
                let inner = pointee.render_c(prefix);
                match konst {
                    ConstPos::None => format!("{inner}*"),
                    ConstPos::West => format!("const {inner}*"),
                    ConstPos::East => format!("{inner} const*"),
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
        assert_eq!(CType::Handle.render_c("myffi"), "myffi_handle_t");
        assert_eq!(CType::Error.render_c("myffi"), "myffi_error");
    }

    #[test]
    fn string_pointer_renders_with_west_const() {
        let s = CType::const_ptr(CType::Char);
        assert_eq!(s.render_c("weaveffi"), "const char*");
        assert_eq!(CType::ptr(CType::Char).render_c("weaveffi"), "char*");
    }

    #[test]
    fn struct_tag_uses_prefix_and_module() {
        let t = CType::StructTag {
            module: "contacts".into(),
            name: "Contact".into(),
        };
        assert_eq!(t.render_c("weaveffi"), "weaveffi_contacts_Contact");
        assert_eq!(
            CType::ptr(t).render_c("weaveffi"),
            "weaveffi_contacts_Contact*"
        );
    }

    #[test]
    fn east_const_array_of_string_pointers() {
        // `[string]` element is `const char*`; the array pointer is east-const.
        let elem = CType::const_ptr(CType::Char);
        let arr = CType::Ptr {
            konst: ConstPos::East,
            pointee: Box::new(elem),
        };
        assert_eq!(arr.render_c("weaveffi"), "const char* const*");
    }

    #[test]
    fn named_type_is_prefixed() {
        assert_eq!(
            CType::Named("events_on_data_fn".into()).render_c("weaveffi"),
            "weaveffi_events_on_data_fn"
        );
    }
}
