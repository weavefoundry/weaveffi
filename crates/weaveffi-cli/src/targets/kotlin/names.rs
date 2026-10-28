//! Naming policy: the Kotlin package, the Kotlin name of every user type,
//! error domain, member, field, and generated internal declaration, and the
//! public Kotlin type of every IR type.
//!
//! User types live at the top level of the package, so a type whose name is
//! a Kotlin keyword, a type Kotlin or Java imports by default (`Unit`,
//! `String`, `Result`, ...), a JNA type the generated sources import
//! (`Pointer`, `Structure`), or a name the generated runtime declares gains a
//! trailing underscore (`Unit` becomes `Unit_`). The object holding the free
//! functions is named after the library (`kvstore` is `Kvstore`) and steps
//! aside the same way. Callables, parameters, and fields are lowerCamelCased
//! (`expires_at` becomes `expiresAt`).

use std::collections::{HashMap, HashSet};

use crate::codegen::common::pascal_case;
use crate::codegen::errors;
use crate::lang;
use weaveffi_model::model::{CallbackBinding, ErrorStrategy, Model};
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty};

/// Names user types must not take: Kotlin and Java types the compiler
/// imports into every file, the JNA types the generated sources import, and
/// the declarations of the generated runtime.
pub(crate) const RESERVED_TYPE_NAMES: &[&str] = &[
    "Any",
    "Array",
    "AutoCloseable",
    "Boolean",
    "BooleanArray",
    "BufferReader",
    "BufferWriter",
    "Byte",
    "ByteArray",
    "ByteByReference",
    "CLibrary",
    "CRuntime",
    "Callback",
    "CallbackArg",
    "Callbacks",
    "Char",
    "CharSequence",
    "Charsets",
    "Collection",
    "Comparable",
    "Completions",
    "ContractRow",
    "Deprecated",
    "Double",
    "DoubleArray",
    "DoubleByReference",
    "Enum",
    "Error",
    "Exception",
    "FfiException",
    "Float",
    "FloatArray",
    "FloatByReference",
    "FreeCallback",
    "Function",
    "IllegalArgumentException",
    "IllegalStateException",
    "Int",
    "IntArray",
    "IntByReference",
    "IntegerType",
    "Iterable",
    "Iterator",
    "Jna",
    "JvmField",
    "JvmStatic",
    "Lazy",
    "Lib",
    "Library",
    "List",
    "Long",
    "LongArray",
    "LongByReference",
    "Map",
    "Memory",
    "MutableList",
    "MutableMap",
    "MutableSet",
    "Native",
    "NativeBugException",
    "NativeCleaner",
    "NativeCompletion",
    "NativeError",
    "NativeHandle",
    "NativeLibrary",
    "NativeSequence",
    "NoSuchElementException",
    "Nothing",
    "Number",
    "Object",
    "OptBool",
    "OptF32",
    "OptF64",
    "OptI16",
    "OptI32",
    "OptI64",
    "OptI8",
    "OptU16",
    "OptU32",
    "OptU64",
    "OptU8",
    "Pair",
    "PendingCalls",
    "Pointer",
    "PointerByReference",
    "Result",
    "Run",
    "Runnable",
    "RuntimeException",
    "Sequence",
    "Set",
    "Short",
    "ShortArray",
    "ShortByReference",
    "SizeT",
    "String",
    "Structure",
    "Suppress",
    "System",
    "Thread",
    "Throwable",
    "Throws",
    "Triple",
    "UByte",
    "UInt",
    "ULong",
    "UShort",
    "Unit",
    "Volatile",
];

/// The generated files besides the library object's (`{Object}.kt`): an
/// object spelled like one of them gains a `Library` suffix, so the files
/// never collide.
const GENERATED_FILE_STEMS: &[&str] = &["Abi", "Async", "Buffers", "CLibrary", "Codecs", "Runtime"];

/// Member names the generated wrapper classes already use (or inherit from
/// `Any`, or from `java.lang.Object`, whose final `wait` and `notify` a
/// same-named function would accidentally override), and the companion's
/// `invoke` that a `new` constructor becomes; a method, static, or callback
/// method spelled the same gains a trailing underscore.
const RESERVED_MEMBER_NAMES: &[&str] = &[
    "cloneHandle",
    "close",
    "equals",
    "fromHandle",
    "fromHandleOrNull",
    "handle",
    "hashCode",
    "invoke",
    "notify",
    "notifyAll",
    "toString",
    "wait",
];

/// Properties every exception inherits from `Throwable` (plus the `code` of
/// `FfiException`); an error payload field spelled the same gains a trailing
/// underscore.
const THROWABLE_MEMBERS: &[&str] = &[
    "cause",
    "code",
    "localizedMessage",
    "message",
    "stackTrace",
    "suppressed",
];

/// Escape a user identifier for a Kotlin declaration or expression position:
/// a reserved word gains a trailing underscore.
pub(crate) fn kt_escape(name: &str) -> String {
    lang::escape_ident(name, lang::KOTLIN_KEYWORDS)
}

/// Lower-camelCase an identifier: `rich_variant` becomes `richVariant`.
pub(crate) fn lower_camel(s: &str) -> String {
    let pascal = pascal_case(s);
    let mut chars = pascal.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_lowercase().chain(chars).collect(),
    }
}

/// The Kotlin spelling of a parameter or field: lowerCamelCased, then
/// escaped. It never starts with `_`, which keeps every generated local (all
/// spelled `_name`) out of its way.
pub(crate) fn kt_param(name: &str) -> String {
    kt_escape(&lower_camel(name))
}

/// The Kotlin spelling of a callable or callback method: like [`kt_param`],
/// and also escaped away from the members every wrapper declares.
pub(crate) fn kt_member(name: &str) -> String {
    lang::escape_member(&kt_param(name), RESERVED_MEMBER_NAMES)
}

/// The Kotlin spellings of one declaration's fields (or parameters), in
/// order: each [`kt_param`], with a later field whose camelCase spelling an
/// earlier one already took (`foo_bar` beside `fooBar`) gaining trailing
/// underscores until it's unique. `error_payload` also escapes the
/// properties every exception inherits.
pub(crate) fn kt_fields<'a>(
    names: impl IntoIterator<Item = &'a str>,
    error_payload: bool,
) -> Vec<String> {
    let mut taken = HashSet::new();
    names
        .into_iter()
        .map(|raw| {
            let mut name = kt_param(raw);
            if error_payload {
                name = lang::escape_member(&name, THROWABLE_MEMBERS);
            }
            while !taken.insert(name.clone()) {
                name.push('_');
            }
            name
        })
        .collect()
}

/// Escape a user type name away from keywords and reserved type names.
fn kt_type_ident(name: &str) -> String {
    if lang::is_reserved(name, lang::KOTLIN_KEYWORDS) || RESERVED_TYPE_NAMES.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The `nativeError` domain of a call that can't fail: its failure raises
/// `NativeBugException`.
pub(crate) const TRAP_DOMAIN: u32 = 0;
/// The `nativeError` domain of a `throws: any` call: its failure raises
/// `FfiException` with the code and message.
pub(crate) const UNTYPED_DOMAIN: u32 = 1;

/// Every name the generated Kotlin code needs, resolved once from the
/// identity, the target configuration, and the model.
pub(crate) struct Names {
    /// The Kotlin package (`kvstore`).
    pub package: String,
    /// The package as a directory path (`kvstore`, `com/example/kv`).
    pub package_path: String,
    /// The C symbol prefix (`kvstore`).
    pub prefix: String,
    /// The object holding the free functions, and its file's stem
    /// (`Kvstore`).
    pub object: String,
    types: HashMap<String, String>,
    /// Every error domain's exception class, by the domain's name, in
    /// declaration order (a domain's `nativeError` index is its position
    /// plus 2).
    domains: Vec<(String, String)>,
    /// Every callback interface's internal vtable object, by its name.
    vtables: HashMap<String, String>,
    /// Every interface's `_clone` and `_destroy` symbols, by its name.
    objects: HashMap<String, (String, String)>,
}

impl Names {
    /// Resolve every name for `model`, with `package` the Kotlin package
    /// (already validated).
    pub(crate) fn new(model: &Model, package: &str) -> Self {
        let declared = model
            .enums
            .iter()
            .map(|e| &e.name)
            .chain(model.records.iter().map(|s| &s.name))
            .chain(model.interfaces.iter().map(|i| &i.name))
            .chain(model.callbacks.iter().map(|c| &c.name));
        let types: HashMap<String, String> = declared
            .map(|name| (name.clone(), kt_type_ident(name)))
            .collect();
        let domains: Vec<(String, String)> = errors::tables(model, "Exception")
            .into_iter()
            .map(|t| (t.domain.name.clone(), kt_type_ident(&t.type_name)))
            .collect();
        let mut taken: HashSet<String> = types
            .values()
            .cloned()
            .chain(domains.iter().map(|(_, exc)| exc.clone()))
            .collect();
        let mut unique = |mut name: String, suffix: &str| {
            if taken.contains(&name) || GENERATED_FILE_STEMS.contains(&name.as_str()) {
                name.push_str(suffix);
            }
            while taken.contains(&name) {
                name.push('_');
            }
            taken.insert(name.clone());
            name
        };
        let object = unique(kt_type_ident(&model.identity.pascal_name()), "Library");
        let vtables = model
            .callbacks
            .iter()
            .map(|cb| {
                let base = format!("{}Vtable", types[&cb.name].trim_end_matches('_'));
                (cb.name.clone(), unique(base, "_"))
            })
            .collect();
        let objects = model
            .interfaces
            .iter()
            .map(|i| {
                (
                    i.name.clone(),
                    (i.clone_symbol.clone(), i.destroy_symbol.clone()),
                )
            })
            .collect();
        Self {
            package: package.to_string(),
            package_path: package.replace('.', "/"),
            prefix: model.prefix().to_string(),
            object,
            types,
            domains,
            vtables,
            objects,
        }
    }

    /// The `_clone` and `_destroy` symbols of the interface `name`.
    ///
    /// # Panics
    ///
    /// Panics when no interface has that name, which validation rules out.
    pub(crate) fn object_symbols(&self, name: &str) -> (&str, &str) {
        let (clone, destroy) = self
            .objects
            .get(name)
            .unwrap_or_else(|| panic!("interface '{name}' is not declared"));
        (clone, destroy)
    }

    /// The Kotlin class of the user type `name`.
    pub(crate) fn ty(&self, name: &str) -> String {
        self.types
            .get(name)
            .cloned()
            .unwrap_or_else(|| kt_type_ident(name))
    }

    /// The internal object holding the vtable of the callback interface
    /// `cb` (`ListenerVtable`).
    pub(crate) fn vtable(&self, cb: &CallbackBinding) -> &str {
        &self.vtables[&cb.name]
    }

    /// The internal vtable object of the callback interface named `name`.
    pub(crate) fn vtable_named(&self, name: &str) -> &str {
        &self.vtables[name]
    }

    /// The Kotlin exception class of the error domain `domain` (its raw
    /// name): the shared exception spelling (`KvError` is `KvException`,
    /// `KitchenErrors` is `KitchenException`), escaped like a type.
    ///
    /// # Panics
    ///
    /// Panics when no domain has that name, which validation rules out.
    pub(crate) fn exception(&self, domain: &str) -> &str {
        self.domains
            .iter()
            .find(|(name, _)| name == domain)
            .map(|(_, exc)| exc.as_str())
            .unwrap_or_else(|| panic!("error domain '{domain}' is not declared"))
    }

    /// The `nativeError` domain a failure of a callable with `error` maps
    /// through: [`TRAP_DOMAIN`], [`UNTYPED_DOMAIN`], or the domain's index
    /// (from 2, in declaration order).
    pub(crate) fn domain_index(&self, error: &ErrorStrategy) -> u32 {
        match error {
            ErrorStrategy::Trap => TRAP_DOMAIN,
            ErrorStrategy::Untyped => UNTYPED_DOMAIN,
            ErrorStrategy::Domain(name) => {
                let i = self
                    .domains
                    .iter()
                    .position(|(d, _)| d == name)
                    .unwrap_or_else(|| panic!("error domain '{name}' is not declared"));
                u32::try_from(i).map_or(u32::MAX, |i| i + 2)
            }
        }
    }

    /// Every declared domain's `nativeError` index and exception class, in
    /// index order.
    pub(crate) fn domains(&self) -> impl Iterator<Item = (u32, &str)> {
        (2u32..).zip(self.domains.iter().map(|(_, exc)| exc.as_str()))
    }

    /// The exception class a callable with `error` raises, for its
    /// `@Throws` annotation: the domain's, `FfiException` for `throws: any`,
    /// or `None` for a call that can't fail.
    pub(crate) fn thrown(&self, error: &ErrorStrategy) -> Option<&str> {
        match error {
            ErrorStrategy::Trap => None,
            ErrorStrategy::Untyped => Some("FfiException"),
            ErrorStrategy::Domain(name) => Some(self.exception(name)),
        }
    }

    /// The public Kotlin type of an IR value type.
    pub(crate) fn kt_type(&self, t: &Ty) -> String {
        match t {
            Ty::Prim(p) => kt_prim(*p).to_string(),
            Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => self.ty(n),
            Ty::Optional(inner) => format!("{}?", self.kt_type(inner)),
            Ty::List(inner) => format!("List<{}>", self.kt_type(inner)),
            Ty::Map(k, v) => format!("Map<{}, {}>", self.kt_type(k), self.kt_type(v)),
        }
    }

    /// The public Kotlin type of a parameter: its value type, or the
    /// callback interface (nullable when optional).
    pub(crate) fn kt_param_type(&self, t: &ParamTy) -> String {
        match t {
            ParamTy::Value(t) => self.kt_type(t),
            ParamTy::Callback { name, nullable } => {
                format!("{}{}", self.ty(name), if *nullable { "?" } else { "" })
            }
        }
    }

    /// The public Kotlin type of a return: its value type, or a
    /// `NativeSequence` of the element type.
    pub(crate) fn kt_ret_type(&self, t: &RetTy) -> String {
        match t {
            RetTy::Value(t) => self.kt_type(t),
            RetTy::Iterator(elem) => format!("NativeSequence<{}>", self.kt_type(elem)),
        }
    }
}

/// The public Kotlin type of a primitive.
pub(crate) fn kt_prim(p: Prim) -> &'static str {
    match p {
        Prim::I8 => "Byte",
        Prim::U8 => "UByte",
        Prim::I16 => "Short",
        Prim::U16 => "UShort",
        Prim::I32 => "Int",
        Prim::U32 => "UInt",
        Prim::I64 => "Long",
        Prim::U64 => "ULong",
        Prim::F32 => "Float",
        Prim::F64 => "Double",
        Prim::Bool => "Boolean",
        Prim::String => "String",
        Prim::Bytes => "ByteArray",
    }
}

/// The Kotlin type a scalar crosses JNA as: the signed type of the same
/// width for an unsigned integer, and `Byte` for a C `bool` (JNA's own
/// `boolean` is a 4-byte `int`).
pub(crate) fn jna_prim(p: Prim) -> &'static str {
    match p {
        Prim::Bool | Prim::I8 | Prim::U8 => "Byte",
        Prim::I16 | Prim::U16 => "Short",
        Prim::I32 | Prim::U32 => "Int",
        Prim::I64 | Prim::U64 => "Long",
        Prim::F32 => "Float",
        Prim::F64 => "Double",
        Prim::String | Prim::Bytes => unreachable!("{p} is not a scalar"),
    }
}

/// Resolve the Kotlin package: the configured one, else the identity
/// prefix. A segment that is a Kotlin keyword gains a trailing underscore.
pub(crate) fn resolve_package(configured: Option<&str>, prefix: &str) -> String {
    let raw = configured
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(prefix);
    raw.split('.')
        .map(|seg| kt_escape(seg.trim()))
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn members_avoid_the_wrapper_and_object_members() {
        assert_eq!(kt_member("close"), "close_");
        assert_eq!(kt_member("to_string"), "toString_");
        assert_eq!(kt_member("wait"), "wait_");
        assert_eq!(kt_member("notify_all"), "notifyAll_");
        assert_eq!(kt_member("dispose"), "dispose");
        assert_eq!(kt_member("object"), "object_");
    }

    #[test]
    fn fields_are_camel_cased_and_unique() {
        assert_eq!(
            kt_fields(["expires_at", "expiresAt", "in", "_hidden"], false),
            ["expiresAt", "expiresAt_", "in_", "hidden"]
        );
        assert_eq!(
            kt_fields(["message", "code", "key"], true),
            ["message_", "code_", "key"]
        );
    }

    #[test]
    fn reserved_type_names_are_sorted() {
        let mut sorted = RESERVED_TYPE_NAMES.to_vec();
        sorted.sort_unstable();
        assert_eq!(sorted, RESERVED_TYPE_NAMES);
    }
}
