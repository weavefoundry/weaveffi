//! Swift type spellings and identifier policy: how model types and
//! user-chosen names render in Swift source.

use std::collections::HashSet;

use crate::lang;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::model::{ErrorBinding, IteratorBinding, Model};
use weaveffi_model::ty::{Prim, Ty};

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
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// The `@available(*, deprecated, ...)` attribute line for a deprecation
/// message.
pub(crate) fn deprecated_attr(msg: &str) -> String {
    format!("@available(*, deprecated, message: \"{}\")", swift_str(msg))
}

/// Facts about the whole API that type spellings and codecs consult:
/// namespace collisions, C enum type names, and which value types can be
/// `Hashable`.
pub(crate) struct SwiftCtx<'a> {
    /// The model being rendered.
    pub(crate) model: &'a Model,
    /// C ABI symbol prefix (e.g. `kvstore`).
    pub(crate) c_prefix: &'a str,
    /// SwiftPM module name (e.g. `Kvstore`).
    pub(crate) swift_module: &'a str,
    /// The error type for codes outside any declared domain
    /// (`KvstoreRuntimeError`).
    pub(crate) runtime_error: String,
    /// Every module name in the API, PascalCased: the namespace `enum`
    /// names a wrapper-type reference can be shadowed by.
    module_names: HashSet<String>,
    /// The records and rich enums that are `Hashable` (every field is).
    hashable: HashSet<String>,
    /// The declaring module paths of the error domains a callback method
    /// may throw, which need a `wvReport{Stem}` helper.
    reported: HashSet<String>,
}

impl<'a> SwiftCtx<'a> {
    /// Collect the API-wide facts from the model.
    pub(crate) fn new(model: &'a Model, swift_module: &'a str) -> SwiftCtx<'a> {
        let module_names = model
            .modules
            .iter()
            .map(|m| m.name.to_upper_camel_case())
            .collect();
        let reported = model
            .modules
            .iter()
            .filter(|m| {
                m.callback_interfaces
                    .iter()
                    .any(|cb| cb.methods.iter().any(|f| f.throws))
            })
            .filter_map(|m| model.error_domain(m))
            .map(|e| e.owner_path.clone())
            .collect();
        SwiftCtx {
            model,
            c_prefix: model.prefix(),
            swift_module,
            runtime_error: format!("{swift_module}RuntimeError"),
            module_names,
            hashable: hashable_types(model),
            reported,
        }
    }

    /// `true` when a callback method may throw the domain `eb`.
    pub(crate) fn reports(&self, eb: &ErrorBinding) -> bool {
        self.reported.contains(&eb.owner_path)
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

    /// The Swift surface type of a model type. A callback interface is an
    /// existential (`any Listener`).
    ///
    /// # Panics
    ///
    /// Panics on an iterator, which only appears as a function return and
    /// renders as its per-function sequence class instead.
    pub(crate) fn swift_type(&self, t: &Ty) -> String {
        match t {
            Ty::Enum(name) | Ty::Record(name) | Ty::RichEnum(name) | Ty::Interface(name) => {
                self.ty_name(name)
            }
            Ty::CallbackInterface(name) => format!("any {}", self.ty_name(name)),
            Ty::Optional(inner) if matches!(**inner, Ty::CallbackInterface(_)) => {
                format!("({})?", self.swift_type(inner))
            }
            Ty::Optional(inner) => format!("{}?", self.swift_type(inner)),
            Ty::List(inner) => format!("[{}]", self.swift_type(inner)),
            Ty::Map(k, v) => format!("[{}: {}]", self.swift_type(k), self.swift_type(v)),
            other => scalar_swift_type(other).to_string(),
        }
    }

    /// The imported C type of the C-style enum named `name`.
    ///
    /// # Panics
    ///
    /// Panics when no enum has that name, which validation rules out.
    pub(crate) fn c_enum_type(&self, name: &str) -> &str {
        &self.model.enumeration(name).c_tag
    }

    /// `true` when values of `t` can be `Hashable` (and so `Equatable`).
    pub(crate) fn is_hashable(&self, t: &Ty) -> bool {
        type_is_hashable(t, &self.hashable)
    }
}

/// The Swift spelling of a scalar, string, or bytes type.
///
/// # Panics
///
/// Panics on a user, container, or iterator type.
pub(crate) fn scalar_swift_type(t: &Ty) -> &'static str {
    match t {
        Ty::Prim(Prim::I8) => "Int8",
        Ty::Prim(Prim::I16) => "Int16",
        Ty::Prim(Prim::I32) => "Int32",
        Ty::Prim(Prim::I64) => "Int64",
        Ty::Prim(Prim::U8) => "UInt8",
        Ty::Prim(Prim::U16) => "UInt16",
        Ty::Prim(Prim::U32) => "UInt32",
        Ty::Prim(Prim::U64) => "UInt64",
        Ty::Prim(Prim::F32) => "Float",
        Ty::Prim(Prim::F64) => "Double",
        Ty::Prim(Prim::Bool) => "Bool",
        Ty::Prim(Prim::String) => "String",
        Ty::Prim(Prim::Bytes) => "Data",
        other => unreachable!("`{other}` isn't a scalar type"),
    }
}

fn type_is_hashable(t: &Ty, hashable: &HashSet<String>) -> bool {
    match t {
        Ty::Interface(_) | Ty::CallbackInterface(_) | Ty::Iterator(_) => false,
        Ty::Record(name) | Ty::RichEnum(name) => hashable.contains(name),
        Ty::Optional(inner) | Ty::List(inner) => type_is_hashable(inner, hashable),
        Ty::Map(k, v) => type_is_hashable(k, hashable) && type_is_hashable(v, hashable),
        _ => true,
    }
}

/// The records and rich enums whose every field is `Hashable`, as a greatest
/// fixed point so mutually referencing types resolve.
fn hashable_types(model: &Model) -> HashSet<String> {
    let mut types: Vec<(String, Vec<&Ty>)> = Vec::new();
    for m in &model.modules {
        for s in &m.structs {
            let fields = s.fields.iter().map(|f| &f.ty).collect();
            types.push((s.name.clone(), fields));
        }
        for e in m.enums.iter().filter(|e| e.is_rich()) {
            let fields = e
                .variants
                .iter()
                .flat_map(|v| v.fields.iter().map(|f| &f.ty))
                .collect();
            types.push((e.name.clone(), fields));
        }
    }
    let mut hashable: HashSet<String> = types.iter().map(|(n, _)| n.clone()).collect();
    loop {
        let before = hashable.len();
        for (name, fields) in &types {
            if hashable.contains(name) && !fields.iter().all(|t| type_is_hashable(t, &hashable)) {
                hashable.remove(name);
            }
        }
        if hashable.len() == before {
            return hashable;
        }
    }
}

/// The Swift name of the lazy sequence class emitted for one `iter<T>`
/// function: the iterator tag minus the C prefix, PascalCased
/// (`kvstore_kv_Store_ListKeysIterator` becomes `KvStoreListKeysIterator`).
pub(crate) fn iterator_class_name(it: &IteratorBinding, c_prefix: &str) -> String {
    it.iter_tag
        .strip_prefix(&format!("{c_prefix}_"))
        .unwrap_or(&it.iter_tag)
        .to_upper_camel_case()
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
}
