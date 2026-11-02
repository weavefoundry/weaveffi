//! Swift type spellings and identifier policy: how model types, user-chosen
//! names, and doc text render in Swift source, and how each value's one C
//! value is spelled on the Swift side of the C module.

use crate::codegen::docs::{ApiNames, Doc, IdentKind};
use crate::codegen::values::Crossing;
use crate::lang;
use heck::ToLowerCamelCase;
use weaveffi_model::errors::type_name;
use weaveffi_model::model::Model;
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty};

/// The Swift spelling of a user-chosen identifier in a lowerCamel position
/// (parameters, fields, enum cases, wrapper names): camel-cased, then
/// keyword-escaped through the shared rule, so a parameter named `in`
/// becomes `in_` rather than emitting broken Swift.
pub(crate) fn swift_ident(name: &str) -> String {
    lang::escape_ident(&name.to_lower_camel_case(), lang::SWIFT_KEYWORDS)
}

/// Members every interface wrapper class declares: the stored `ptr`, the
/// internal `clonePtr()`, and the `WvCodable` conformance's `wvRead` and
/// `wvWrite`. (`init` and `deinit` are keywords, so [`swift_ident`] already
/// escapes them.)
const OBJECT_MEMBERS: &[&str] = &["clonePtr", "ptr", "wvRead", "wvWrite"];

/// The Swift spelling of an interface member (a factory constructor,
/// method, or static): [`swift_ident`], with a trailing `_` when it would
/// redeclare a member the wrapper class declares (`clone_ptr` is
/// `clonePtr_`).
pub(crate) fn swift_member(name: &str) -> String {
    lang::escape_member(&swift_ident(name), OBJECT_MEMBERS)
}

/// Escape a string for embedding inside a Swift double-quoted literal.
pub(crate) fn swift_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The Swift spelling of a scalar, string, or bytes type.
pub(crate) fn prim_swift_type(p: Prim) -> &'static str {
    match p {
        Prim::I8 => "Int8",
        Prim::I16 => "Int16",
        Prim::I32 => "Int32",
        Prim::I64 => "Int64",
        Prim::U8 => "UInt8",
        Prim::U16 => "UInt16",
        Prim::U32 => "UInt32",
        Prim::U64 => "UInt64",
        Prim::F32 => "Float",
        Prim::F64 => "Double",
        Prim::Bool => "Bool",
        Prim::String => "String",
        Prim::Bytes => "Data",
    }
}

/// Facts about the whole API that spellings consult: the generated
/// package's names and every declared identifier (for doc text).
pub(crate) struct SwiftCtx<'a> {
    /// The model being rendered.
    pub(crate) model: &'a Model,
    /// The C symbol prefix, which names the run and optional-scalar
    /// structs the C module declares.
    pub(crate) prefix: &'a str,
    /// The error type for failures outside any declared domain
    /// (`KvstoreRuntimeError`).
    pub(crate) runtime_error: String,
    /// The namespace type of the library itself (`KvstoreLibrary`), which
    /// carries `check()`.
    pub(crate) library_type: String,
    /// Every declared identifier, for rewriting doc text.
    names: ApiNames,
}

impl<'a> SwiftCtx<'a> {
    /// Collect the API-wide facts from the model.
    pub(crate) fn new(model: &'a Model, swift_module: &str) -> SwiftCtx<'a> {
        SwiftCtx {
            model,
            prefix: model.prefix(),
            runtime_error: runtime_error_name(swift_module),
            library_type: library_type_name(swift_module),
            names: ApiNames::new(model),
        }
    }

    /// The Swift type of a value type.
    pub(crate) fn swift_type(&self, t: &Ty) -> String {
        match t {
            Ty::Prim(p) => prim_swift_type(*p).to_string(),
            Ty::Enum(name) | Ty::Record(name) | Ty::RichEnum(name) | Ty::Interface(name) => {
                name.clone()
            }
            Ty::Optional(inner) => format!("{}?", self.swift_type(inner)),
            Ty::List(inner) => format!("[{}]", self.swift_type(inner)),
            Ty::Map(k, v) => format!("[{}: {}]", self.swift_type(k), self.swift_type(v)),
        }
    }

    /// The Swift type of a parameter. A callback interface is an existential
    /// (`any Listener`, or `(any Listener)?`).
    pub(crate) fn param_type(&self, t: &ParamTy) -> String {
        match t {
            ParamTy::Value(t) => self.swift_type(t),
            ParamTy::Callback { name, nullable } => {
                if *nullable {
                    format!("(any {name})?")
                } else {
                    format!("any {name}")
                }
            }
        }
    }

    /// The Swift type of a return: an iterator is the runtime's generic
    /// `NativeSequence`.
    pub(crate) fn ret_type(&self, t: &RetTy) -> String {
        match t {
            RetTy::Value(t) => self.swift_type(t),
            RetTy::Iterator(elem) => format!("NativeSequence<{}>", self.swift_type(elem)),
        }
    }

    /// The Swift spelling of the one C value a value crosses as, as the C
    /// module imports it: the scalar (`Int32` for a C-style enum, whose C
    /// type is an `int32_t` typedef), the `{p}_opt_{prim}` or `{p}_run`
    /// struct, or a nullable `OpaquePointer` for an object.
    pub(crate) fn c_value_type(&self, crossing: &Crossing) -> String {
        match crossing {
            Crossing::Scalar(p) => prim_swift_type(*p).to_string(),
            Crossing::Enum(_) => "Int32".to_string(),
            Crossing::OptScalar(p) => format!("{}_opt_{}", self.prefix, p.snake()),
            Crossing::OptEnum(_) => format!("{}_opt_i32", self.prefix),
            Crossing::Utf8 | Crossing::Bytes | Crossing::Slice(_) | Crossing::Buffer(_) => {
                "WvRun".to_string()
            }
            Crossing::Object { .. } => "OpaquePointer?".to_string(),
        }
    }

    /// The zero value of [`c_value_type`](Self::c_value_type): what a
    /// callback method returns when it fails, and what an iterator's item
    /// slot starts as.
    pub(crate) fn c_zero(&self, crossing: &Crossing) -> String {
        match crossing {
            Crossing::Scalar(Prim::Bool) => "false".to_string(),
            Crossing::Scalar(_) | Crossing::Enum(_) => "0".to_string(),
            Crossing::Object { .. } => "nil".to_string(),
            other => format!("{}()", self.c_value_type(other)),
        }
    }

    /// The Swift spelling of an API identifier named in doc text, or `None`
    /// to keep it as written.
    fn spell(&self, ident: &str) -> Option<String> {
        if ident.contains('.') {
            return None;
        }
        match self.names.kind(ident)? {
            IdentKind::Function
            | IdentKind::Member
            | IdentKind::CallbackMethod
            | IdentKind::Param
            | IdentKind::Field
            | IdentKind::Variant
            | IdentKind::ErrorCode => Some(swift_ident(ident)),
            IdentKind::ErrorDomain => Some(type_name(ident, "Error")),
            IdentKind::Type => None,
        }
    }

    /// A doc text with every backticked API identifier in Swift spelling.
    pub(crate) fn doc(&self, doc: &Option<String>) -> Option<String> {
        Doc::new(doc, &None).text(|i| self.spell(i))
    }

    /// The `@available(*, deprecated, ...)` attribute of a deprecated
    /// declaration, its message in Swift spelling.
    pub(crate) fn deprecated_attr(&self, deprecated: &Option<String>) -> Option<String> {
        Doc::new(&None, deprecated)
            .deprecation(|i| self.spell(i))
            .map(|msg| {
                format!(
                    "@available(*, deprecated, message: \"{}\")",
                    swift_str(&msg)
                )
            })
    }

    /// The Swift type of the error domain named `domain`.
    pub(crate) fn domain_type(&self, domain: &str) -> String {
        self.model.error_domain(domain).type_name.clone()
    }
}

/// The error type for failures outside any declared domain:
/// `{Module}RuntimeError`.
pub(crate) fn runtime_error_name(swift_module: &str) -> String {
    format!("{swift_module}RuntimeError")
}

/// The namespace type of the library: `{Module}Library`.
pub(crate) fn library_type_name(swift_module: &str) -> String {
    format!("{swift_module}Library")
}

/// The internal namespace holding the process-wide vtable for the callback
/// interface named `name`: `Wv{Name}Vtable`.
pub(crate) fn callback_vtable_name(name: &str) -> String {
    format!("Wv{name}Vtable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_members_avoid_the_wrapper_members() {
        assert_eq!(swift_member("clone_ptr"), "clonePtr_");
        assert_eq!(swift_member("ptr"), "ptr_");
        assert_eq!(swift_member("wv_write"), "wvWrite_");
        assert_eq!(swift_member("init"), "init_");
        assert_eq!(swift_member("close"), "close");
    }

    #[test]
    fn string_literals_escape_quotes_and_controls() {
        assert_eq!(swift_str(r#"a "b" \c"#), r#"a \"b\" \\c"#);
        assert_eq!(swift_str("x\ny\u{1}"), "x\\ny\\u{1}");
    }
}
