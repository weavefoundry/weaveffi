//! Swift type spellings and identifier policy: how model types, user-chosen
//! names, and doc text render in Swift source.

use std::collections::HashSet;

use crate::codegen::docs::{ApiNames, Doc, IdentKind};
use crate::lang;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
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
/// package's names, namespace collisions, and every declared identifier
/// (for doc text).
pub(crate) struct SwiftCtx<'a> {
    /// The model being rendered.
    pub(crate) model: &'a Model,
    /// SwiftPM module name (e.g. `Kvstore`).
    pub(crate) swift_module: &'a str,
    /// The error type for failures outside any declared domain
    /// (`KvstoreRuntimeError`).
    pub(crate) runtime_error: String,
    /// The namespace type of the library itself (`KvstoreLibrary`), which
    /// carries `check()`.
    pub(crate) library_type: String,
    /// Every module name in the API, PascalCased: the namespace `enum`
    /// names a wrapper-type reference can be shadowed by.
    module_names: HashSet<String>,
    /// Every declared identifier, for rewriting doc text.
    names: ApiNames,
}

impl<'a> SwiftCtx<'a> {
    /// Collect the API-wide facts from the model.
    pub(crate) fn new(model: &'a Model, swift_module: &'a str) -> SwiftCtx<'a> {
        SwiftCtx {
            model,
            swift_module,
            runtime_error: runtime_error_name(swift_module),
            library_type: library_type_name(swift_module),
            module_names: model
                .modules
                .iter()
                .map(|m| m.name.to_upper_camel_case())
                .collect(),
            names: ApiNames::new(model),
        }
    }

    /// Qualify a top-level wrapper type name with the Swift module when its
    /// name collides with a namespace `enum`. Inside `enum Kv { enum Stats {
    /// ... } }` the bare name `Stats` resolves to the namespace, not the
    /// top-level type; `Kvstore.Stats` forces the type.
    pub(crate) fn ty_name(&self, local: &str) -> String {
        if self.module_names.contains(local) {
            format!("{}.{}", self.swift_module, local)
        } else {
            local.to_string()
        }
    }

    /// The Swift type of a value type.
    pub(crate) fn swift_type(&self, t: &Ty) -> String {
        match t {
            Ty::Prim(p) => prim_swift_type(*p).to_string(),
            Ty::Enum(name) | Ty::Record(name) | Ty::RichEnum(name) | Ty::Interface(name) => {
                self.ty_name(name)
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
                let ty = format!("any {}", self.ty_name(name));
                if *nullable {
                    format!("({ty})?")
                } else {
                    ty
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
            IdentKind::Module => Some(ident.to_upper_camel_case()),
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
