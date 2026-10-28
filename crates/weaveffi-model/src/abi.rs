//! The C ABI's type algebra (revision 6).
//!
//! Every value crosses a call boundary as exactly one C value, whatever its
//! position (parameter, return, async result, iterator item, or callback
//! method parameter or return), chosen by its [`Family`]:
//!
//! | Family | C type |
//! |---|---|
//! | Direct | the scalar (`int32_t`, `double`, `bool`, ...), or an enum's `{p}_{E}` (`typedef int32_t`) |
//! | OptDirect | `{p}_opt_{prim}`: `{ bool has; T value; }` (an enum's is `{p}_opt_i32`) |
//! | Run | `{p}_run`: `{ const uint8_t* ptr; size_t len; }`, `len` in bytes |
//! | Object | `const {p}_{I}*` as a callable's parameter (borrowed), else `{p}_{I}*` (one strong reference moves) |
//!
//! A callback-interface parameter is a `{p}_{Cb}` struct (`{ void* ctx;
//! const {p}_{Cb}_vtable* vtable; }`) passed by value, and an iterator
//! return is an opaque `{p}_{Iter}*`.
//!
//! [`CType`] is prefix-agnostic; [`CType::render_c`] is the canonical
//! spelling, and every other target maps it onto its own FFI vocabulary.

use crate::ty::{Family, Prim, Ty};

/// A single C type in an ABI signature, independent of the library's symbol
/// prefix (applied at render time by [`CType::render_c`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CType {
    /// A scalar primitive by value: an integer, `float`, `double`, or
    /// `bool`.
    Scalar(Prim),
    /// `size_t`.
    Size,
    /// `void` (a return, or a pointee).
    Void,
    /// A C-style enum or error-domain code type, `{p}_{name}` (declared
    /// `typedef int32_t`).
    Enum(String),
    /// An optional scalar, `{p}_opt_{prim}`.
    Opt(Prim),
    /// A byte run, `{p}_run`.
    Run,
    /// An interface's opaque object type, `{p}_{name}` (only as a pointee).
    Object(String),
    /// A callback-interface argument, the `{p}_{name}` struct of a context
    /// and a vtable pointer, passed by value.
    Callback(String),
    /// A callback interface's vtable struct, `{p}_{name}_vtable`.
    Vtable(String),
    /// A prefixed type declared elsewhere in the header (an iterator's
    /// opaque type, an async completion typedef): `{p}_{core}`.
    Named(String),
    /// The error struct, `{p}_error`.
    Error,
    /// The cancel token, `{p}_cancel_token`.
    CancelToken,
    /// A pointer.
    Ptr {
        /// `true` for `const T*`.
        konst: bool,
        /// The type pointed to.
        pointee: Box<CType>,
    },
}

/// Where a value crosses, which decides an object pointer's constness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// A callable's parameter: borrowed for the call.
    Param,
    /// Anything that moves a value to its receiver: a return, an async
    /// result, an iterator item, or a callback method's parameter or return.
    Transfer,
}

impl CType {
    /// A pointer to `pointee`.
    #[must_use]
    pub fn ptr(pointee: CType) -> CType {
        CType::Ptr {
            konst: false,
            pointee: Box::new(pointee),
        }
    }

    /// A `const` pointer to `pointee`.
    #[must_use]
    pub fn const_ptr(pointee: CType) -> CType {
        CType::Ptr {
            konst: true,
            pointee: Box::new(pointee),
        }
    }

    /// The C type of a value of type `ty` at `position`.
    #[must_use]
    // The `expect` can't fire: every type in the Direct family has a scalar.
    #[allow(clippy::missing_panics_doc)]
    pub fn of(ty: &Ty, position: Position) -> CType {
        match ty.family() {
            Family::Direct => match ty {
                Ty::Enum(name) => CType::Enum(name.clone()),
                _ => CType::Scalar(ty.scalar().expect("a Direct value has a scalar")),
            },
            Family::OptDirect => CType::Opt(ty.scalar().expect("an OptDirect value has a scalar")),
            Family::Run(_) => CType::Run,
            Family::Object { .. } => {
                let name = ty
                    .interface_name()
                    .expect("an Object value names an interface")
                    .to_string();
                CType::Ptr {
                    konst: position == Position::Param,
                    pointee: Box::new(CType::Object(name)),
                }
            }
        }
    }

    /// `true` for a pointer.
    #[must_use]
    pub fn is_pointer(&self) -> bool {
        matches!(self, CType::Ptr { .. })
    }

    /// The C spelling of a scalar primitive (`int32_t`, `double`, `bool`).
    ///
    /// # Panics
    ///
    /// Panics for `string` and `bytes`, which aren't scalars.
    #[must_use]
    pub fn scalar_c(p: Prim) -> &'static str {
        match p {
            Prim::Bool => "bool",
            Prim::I8 => "int8_t",
            Prim::I16 => "int16_t",
            Prim::I32 => "int32_t",
            Prim::I64 => "int64_t",
            Prim::U8 => "uint8_t",
            Prim::U16 => "uint16_t",
            Prim::U32 => "uint32_t",
            Prim::U64 => "uint64_t",
            Prim::F32 => "float",
            Prim::F64 => "double",
            Prim::String | Prim::Bytes => panic!("{p} is not a scalar"),
        }
    }

    /// Render this type as C source with `prefix` for every library-owned
    /// name.
    #[must_use]
    pub fn render_c(&self, prefix: &str) -> String {
        match self {
            CType::Scalar(p) => Self::scalar_c(*p).to_string(),
            CType::Size => "size_t".to_string(),
            CType::Void => "void".to_string(),
            CType::Enum(name) | CType::Object(name) | CType::Callback(name) => {
                format!("{prefix}_{name}")
            }
            CType::Opt(p) => format!("{prefix}_opt_{}", p.snake()),
            CType::Run => format!("{prefix}_run"),
            CType::Vtable(name) => format!("{prefix}_{name}_vtable"),
            CType::Named(core) => format!("{prefix}_{core}"),
            CType::Error => format!("{prefix}_error"),
            CType::CancelToken => format!("{prefix}_cancel_token"),
            CType::Ptr { konst, pointee } => {
                let inner = pointee.render_c(prefix);
                if *konst {
                    format!("const {inner}*")
                } else {
                    format!("{inner}*")
                }
            }
        }
    }
}

/// A named C parameter slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbiParam {
    /// The C parameter name (for example `key`, `out_err`, `callback`).
    pub name: String,
    /// The C type of the slot.
    pub ty: CType,
}

impl AbiParam {
    /// A slot from a name and its C type.
    pub fn new(name: impl Into<String>, ty: CType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// One lowered C function signature: its symbol, ordered parameter slots,
/// and return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AbiFn {
    /// The C symbol (for a callback method, the vtable field name).
    pub symbol: String,
    /// The parameter slots, in order.
    pub params: Vec<AbiParam>,
    /// The C return type.
    pub ret: CType,
}

/// The trailing `{p}_error* out_err` slot of every synchronous call.
#[must_use]
pub fn error_out_param() -> AbiParam {
    AbiParam::new("out_err", CType::ptr(CType::Error))
}

/// The `void* context` slot of an async launcher and its completion.
#[must_use]
pub fn context_param() -> AbiParam {
    AbiParam::new("context", CType::ptr(CType::Void))
}

/// The leading `void* ctx` slot of every callback-interface method.
#[must_use]
pub fn ctx_param() -> AbiParam {
    AbiParam::new("ctx", CType::ptr(CType::Void))
}

/// The `{p}_cancel_token* cancel_token` slot of a cancellable launcher.
#[must_use]
pub fn cancel_token_param() -> AbiParam {
    AbiParam::new("cancel_token", CType::ptr(CType::CancelToken))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_family_is_one_c_type() {
        let p = "kv";
        let render = |ty: &Ty, at| CType::of(ty, at).render_c(p);
        assert_eq!(render(&Ty::Prim(Prim::I32), Position::Param), "int32_t");
        assert_eq!(render(&Ty::Prim(Prim::Bool), Position::Param), "bool");
        assert_eq!(
            render(&Ty::Enum("Color".into()), Position::Param),
            "kv_Color"
        );
        let opt = |t| Ty::Optional(Box::new(t));
        assert_eq!(
            render(&opt(Ty::Prim(Prim::F64)), Position::Param),
            "kv_opt_f64"
        );
        assert_eq!(
            render(&opt(Ty::Enum("Color".into())), Position::Param),
            "kv_opt_i32"
        );
        for ty in [
            Ty::Prim(Prim::String),
            Ty::Prim(Prim::Bytes),
            Ty::List(Box::new(Ty::Prim(Prim::U64))),
            Ty::Record("Entry".into()),
        ] {
            assert_eq!(render(&ty, Position::Transfer), "kv_run");
        }
        let store = Ty::Interface("Store".into());
        assert_eq!(render(&store, Position::Param), "const kv_Store*");
        assert_eq!(render(&opt(store), Position::Transfer), "kv_Store*");
        assert_eq!(
            CType::const_ptr(CType::Vtable("Listener".into())).render_c(p),
            "const kv_Listener_vtable*"
        );
        assert_eq!(CType::ptr(CType::Error).render_c(p), "kv_error*");
    }
}
