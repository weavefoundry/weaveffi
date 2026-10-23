//! How each value crosses the JNI boundary, read from the model's passing
//! contracts ([`ArgPass`], [`RetPass`], [`ResultPass`], [`ItemPass`],
//! [`CallbackRetPass`]), never re-derived from the type.
//!
//! A [`Carrier`] names the JNI form: a primitive (`jint`), its nullable box
//! (`java.lang.Integer`, for an optional scalar crossing out of native code),
//! a primitive array (`jintArray`, for a typed-array slice), a `ByteArray`
//! (strings, bytes, and value buffers), or an object's address (`jlong`). An
//! optional scalar parameter crosses split, as a presence flag and a value,
//! exactly like its C slots. Async results and iterator items reach Kotlin
//! as `Any?` and are cast to their carrier's object type first.

use weaveffi_model::plan::{ArgPass, CallbackRetPass, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{Prim, Ty};

/// A JNI primitive kind: the carrier of a direct value, the element of a
/// slice, and the payload of a box. Unsigned integers ride in the signed
/// kind of the same width and C-style enums in `Int`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Boolean,
    Byte,
    Short,
    Int,
    Long,
    Float,
    Double,
}

impl Kind {
    /// The kind a scalar primitive crosses as.
    pub(crate) fn of_prim(p: Prim) -> Self {
        match p {
            Prim::Bool => Self::Boolean,
            Prim::I8 | Prim::U8 => Self::Byte,
            Prim::I16 | Prim::U16 => Self::Short,
            Prim::I32 | Prim::U32 => Self::Int,
            Prim::F32 => Self::Float,
            Prim::F64 => Self::Double,
            Prim::I64 | Prim::U64 | Prim::String | Prim::Bytes => Self::Long,
        }
    }

    /// The kind a direct value (a scalar primitive or a C-style enum)
    /// crosses as.
    pub(crate) fn of(t: &Ty) -> Self {
        match t {
            Ty::Prim(p) => Self::of_prim(*p),
            _ => Self::Int,
        }
    }

    /// The Kotlin type, which is also the `Call{Kind}Method` and
    /// `New{Kind}Array` stem: `Int`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Boolean => "Boolean",
            Self::Byte => "Byte",
            Self::Short => "Short",
            Self::Int => "Int",
            Self::Long => "Long",
            Self::Float => "Float",
            Self::Double => "Double",
        }
    }

    /// The JNI C type: `jint`.
    pub(crate) fn jni(self) -> &'static str {
        match self {
            Self::Boolean => "jboolean",
            Self::Byte => "jbyte",
            Self::Short => "jshort",
            Self::Int => "jint",
            Self::Long => "jlong",
            Self::Float => "jfloat",
            Self::Double => "jdouble",
        }
    }

    /// The JVM type descriptor: `I`.
    pub(crate) fn sig(self) -> &'static str {
        match self {
            Self::Boolean => "Z",
            Self::Byte => "B",
            Self::Short => "S",
            Self::Int => "I",
            Self::Long => "J",
            Self::Float => "F",
            Self::Double => "D",
        }
    }

    /// The shim's `Jni_kind` constant: `Jni_I`.
    pub(crate) fn c_const(self) -> String {
        format!("Jni_{}", self.sig())
    }

    /// The `jvalue` member holding this kind: `i`.
    pub(crate) fn jvalue(self) -> &'static str {
        match self {
            Self::Boolean => "z",
            Self::Byte => "b",
            Self::Short => "s",
            Self::Int => "i",
            Self::Long => "j",
            Self::Float => "f",
            Self::Double => "d",
        }
    }

    /// The box class's descriptor: `Ljava/lang/Integer;`.
    pub(crate) fn box_sig(self) -> &'static str {
        match self {
            Self::Boolean => "Ljava/lang/Boolean;",
            Self::Byte => "Ljava/lang/Byte;",
            Self::Short => "Ljava/lang/Short;",
            Self::Int => "Ljava/lang/Integer;",
            Self::Long => "Ljava/lang/Long;",
            Self::Float => "Ljava/lang/Float;",
            Self::Double => "Ljava/lang/Double;",
        }
    }

    /// The Kotlin zero value of this kind, for an absent optional's value
    /// slot and a failed callback's return.
    pub(crate) fn zero(self) -> &'static str {
        match self {
            Self::Boolean => "false",
            Self::Byte => "0.toByte()",
            Self::Short => "0.toShort()",
            Self::Int => "0",
            Self::Long => "0L",
            Self::Float => "0f",
            Self::Double => "0.0",
        }
    }
}

/// The JNI form of one value in one position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Carrier {
    /// A primitive: a direct value.
    Prim(Kind),
    /// An optional scalar parameter: a presence flag, then the value (0
    /// when absent), mirroring the C slots.
    Split(Kind),
    /// A nullable box: an optional scalar crossing out of native code.
    Boxed(Kind),
    /// A primitive array: a typed-array slice.
    Array(Kind),
    /// A `ByteArray`: a string's UTF-8, bytes, or a value buffer.
    Bytes,
    /// An object's address (`0L` is none).
    Handle,
}

impl Carrier {
    /// A parameter (or callback method parameter); `None` for a callback
    /// interface, which crosses as the implementing object.
    pub(crate) fn of_arg(pass: &ArgPass) -> Option<Self> {
        Some(match pass {
            ArgPass::Direct { slot } => Self::Prim(kind_of_slot(&slot.ty)),
            ArgPass::OptDirect { inner, .. } => Self::Split(Kind::of(inner)),
            ArgPass::Slice { elem, .. } => Self::Array(Kind::of_prim(*elem)),
            ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => Self::Bytes,
            ArgPass::Object { .. } => Self::Handle,
            ArgPass::Callback { .. } => return None,
        })
    }

    /// A synchronous return (an iterator launcher's is its handle); `None`
    /// for void. `direct` is the value type of a direct return.
    pub(crate) fn of_ret(pass: &RetPass, direct: Option<&Ty>) -> Option<Self> {
        Some(match pass {
            RetPass::Void => return None,
            RetPass::Direct => Self::Prim(Kind::of(direct?)),
            RetPass::OptDirect { .. } => Self::Boxed(Kind::of(&opt_inner(direct?))),
            RetPass::Slice { elem, .. } => Self::Array(Kind::of_prim(*elem)),
            RetPass::String { .. } | RetPass::Bytes { .. } | RetPass::Buffer { .. } => Self::Bytes,
            RetPass::Object { .. } | RetPass::Iterator(_) => Self::Handle,
        })
    }

    /// An async result, delivered boxed as `Any?`; `None` for void. `ty` is
    /// the result's value type.
    pub(crate) fn of_result(pass: &ResultPass, ty: Option<&Ty>) -> Option<Self> {
        Some(match pass {
            ResultPass::Void => return None,
            ResultPass::Direct { .. } => Self::Prim(Kind::of(ty?)),
            ResultPass::OptDirect { .. } => Self::Boxed(Kind::of(&opt_inner(ty?))),
            ResultPass::Slice { elem, .. } => Self::Array(Kind::of_prim(*elem)),
            ResultPass::String { .. } | ResultPass::Bytes { .. } | ResultPass::Buffer { .. } => {
                Self::Bytes
            }
            ResultPass::Object { .. } => Self::Handle,
        })
    }

    /// An iterator item, delivered boxed as `Any?`. `elem` is the element
    /// type.
    pub(crate) fn of_item(pass: &ItemPass, elem: &Ty) -> Self {
        match pass {
            ItemPass::Direct { .. } => Self::Prim(Kind::of(elem)),
            ItemPass::OptDirect { .. } => Self::Boxed(Kind::of(&opt_inner(elem))),
            ItemPass::Slice { elem, .. } => Self::Array(Kind::of_prim(*elem)),
            ItemPass::String { .. } | ItemPass::Bytes { .. } | ItemPass::Buffer { .. } => {
                Self::Bytes
            }
            ItemPass::Object { .. } => Self::Handle,
        }
    }

    /// A callback method's return; `None` for void. `ty` is the return's
    /// value type.
    pub(crate) fn of_callback_ret(pass: &CallbackRetPass, ty: Option<&Ty>) -> Option<Self> {
        Some(match pass {
            CallbackRetPass::Void => return None,
            CallbackRetPass::Direct => Self::Prim(Kind::of(ty?)),
            CallbackRetPass::OptDirect { .. } => Self::Boxed(Kind::of(&opt_inner(ty?))),
            CallbackRetPass::Slice { elem, .. } => Self::Array(Kind::of_prim(*elem)),
            CallbackRetPass::String { .. }
            | CallbackRetPass::Bytes { .. }
            | CallbackRetPass::Buffer { .. } => Self::Bytes,
            CallbackRetPass::Object { .. } => Self::Handle,
        })
    }

    /// The Kotlin type of an `external fun` parameter or return in this
    /// form (a split optional is two parameters; this is its value's).
    pub(crate) fn kotlin(self) -> String {
        match self {
            Self::Prim(k) | Self::Split(k) => k.name().to_string(),
            Self::Boxed(k) => format!("{}?", k.name()),
            Self::Array(k) => format!("{}Array", k.name()),
            Self::Bytes => "ByteArray".to_string(),
            Self::Handle => "Long".to_string(),
        }
    }

    /// The JNI C type (a split optional's value).
    pub(crate) fn jni(self) -> String {
        match self {
            Self::Prim(k) | Self::Split(k) => k.jni().to_string(),
            Self::Boxed(_) => "jobject".to_string(),
            Self::Array(k) => format!("{}Array", k.jni()),
            Self::Bytes => "jbyteArray".to_string(),
            Self::Handle => "jlong".to_string(),
        }
    }

    /// The JVM descriptor (a split optional is its flag, then its value).
    pub(crate) fn sig(self) -> String {
        match self {
            Self::Prim(k) => k.sig().to_string(),
            Self::Split(k) => format!("Z{}", k.sig()),
            Self::Boxed(k) => k.box_sig().to_string(),
            Self::Array(k) => format!("[{}", k.sig()),
            Self::Bytes => "[B".to_string(),
            Self::Handle => "J".to_string(),
        }
    }

    /// The JNI method-call stem a value of this form comes back through
    /// (`CallStatic{stem}Method`).
    pub(crate) fn call_stem(self) -> &'static str {
        match self {
            Self::Prim(k) | Self::Split(k) => k.name(),
            Self::Handle => "Long",
            Self::Boxed(_) | Self::Array(_) | Self::Bytes => "Object",
        }
    }

    /// The array kind of a run-shaped carrier: the slice's element kind, or
    /// `Byte` for a `ByteArray`.
    pub(crate) fn run_kind(self) -> Option<Kind> {
        match self {
            Self::Array(k) => Some(k),
            Self::Bytes => Some(Kind::Byte),
            _ => None,
        }
    }
}

/// The scalar inside an optional (the type itself otherwise).
fn opt_inner(t: &Ty) -> Ty {
    match t {
        Ty::Optional(inner) => (**inner).clone(),
        other => other.clone(),
    }
}

/// The kind of a direct parameter from its C slot type (an enum slot is an
/// `int32_t` typedef).
fn kind_of_slot(t: &weaveffi_model::abi::CType) -> Kind {
    use weaveffi_model::abi::CType;
    match t {
        CType::Bool => Kind::Boolean,
        CType::Int8 | CType::Uint8 => Kind::Byte,
        CType::Int16 | CType::Uint16 => Kind::Short,
        CType::Int64 | CType::Uint64 => Kind::Long,
        CType::Float => Kind::Float,
        CType::Double => Kind::Double,
        _ => Kind::Int,
    }
}
