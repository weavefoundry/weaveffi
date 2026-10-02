//! Swift type spellings and identifier policy: how model types and
//! user-chosen names render in Swift source.

use std::collections::{HashMap, HashSet};

use crate::lang;
use crate::utils::local_type_name;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::model::{BindingModel, IteratorBinding, Ty};

/// The Swift spelling of a user-chosen identifier in a lowerCamel position
/// (parameters, fields, enum cases, wrapper names): camel-cased, then
/// keyword-escaped through the shared rule, so a parameter named `in`
/// becomes `in_` rather than emitting broken Swift.
pub(crate) fn swift_ident(name: &str) -> String {
    lang::escape_ident(&name.to_lower_camel_case(), lang::SWIFT_KEYWORDS)
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
    /// C ABI symbol prefix (e.g. `kvstore`).
    pub(crate) c_prefix: &'a str,
    /// SwiftPM module name (e.g. `Kvstore`).
    pub(crate) swift_module: &'a str,
    /// Every module name in the API, PascalCased: the namespace `enum`
    /// names a wrapper-type reference can be shadowed by.
    module_names: HashSet<String>,
    /// The C type of every C-style enum, keyed by its absolute name.
    enum_c_types: HashMap<String, String>,
    /// Absolute names of the records and rich enums that are `Hashable`
    /// (every field is).
    hashable: HashSet<String>,
}

impl<'a> SwiftCtx<'a> {
    /// Collect the API-wide facts from the binding model.
    pub(crate) fn new(model: &'a BindingModel, swift_module: &'a str) -> SwiftCtx<'a> {
        let module_names = model
            .modules
            .iter()
            .map(|m| m.name.to_upper_camel_case())
            .collect();
        let enum_c_types = model
            .modules
            .iter()
            .flat_map(|m| {
                m.enums
                    .iter()
                    .filter(|e| !e.is_rich())
                    .map(move |e| (format!("{}.{}", m.dot_path, e.name), e.c_tag.clone()))
            })
            .collect();
        SwiftCtx {
            c_prefix: model.prefix.as_str(),
            swift_module,
            module_names,
            enum_c_types,
            hashable: hashable_types(model),
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

    /// The Swift surface type of a model type.
    ///
    /// # Panics
    ///
    /// Panics on an iterator, which only appears as a function return and
    /// renders as its per-function sequence class instead.
    pub(crate) fn swift_type(&self, t: &Ty) -> String {
        match t {
            Ty::Enum(name)
            | Ty::Record(name)
            | Ty::RichEnum(name)
            | Ty::Interface(name)
            | Ty::CallbackInterface(name) => self.ty_name(local_type_name(name)),
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
    /// Panics when no C-style enum has that name, which validation rules out.
    pub(crate) fn c_enum_type(&self, name: &str) -> &str {
        self.enum_c_types
            .get(name)
            .unwrap_or_else(|| panic!("unknown C-style enum `{name}`"))
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
        Ty::I8 => "Int8",
        Ty::I16 => "Int16",
        Ty::I32 => "Int32",
        Ty::I64 => "Int64",
        Ty::U8 => "UInt8",
        Ty::U16 => "UInt16",
        Ty::U32 => "UInt32",
        Ty::U64 => "UInt64",
        Ty::F32 => "Float",
        Ty::F64 => "Double",
        Ty::Bool => "Bool",
        Ty::StringUtf8 => "String",
        Ty::Bytes => "Data",
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
fn hashable_types(model: &BindingModel) -> HashSet<String> {
    let mut types: Vec<(String, Vec<&Ty>)> = Vec::new();
    for m in &model.modules {
        for s in &m.structs {
            let fields = s.fields.iter().map(|f| &f.ty).collect();
            types.push((format!("{}.{}", m.dot_path, s.name), fields));
        }
        for e in m.enums.iter().filter(|e| e.is_rich()) {
            let fields = e
                .variants
                .iter()
                .flat_map(|v| v.fields.iter().map(|f| &f.ty))
                .collect();
            types.push((format!("{}.{}", m.dot_path, e.name), fields));
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

/// The internal box class retaining one implementation of the callback
/// interface named `name` across the C boundary: `Wv{Name}Box`.
pub(crate) fn callback_box_name(name: &str) -> String {
    format!("Wv{}Box", local_type_name(name))
}

/// The internal namespace holding the process-wide vtable for the callback
/// interface named `name`: `Wv{Name}Vtable`.
pub(crate) fn callback_vtable_name(name: &str) -> String {
    format!("Wv{}Vtable", local_type_name(name))
}
