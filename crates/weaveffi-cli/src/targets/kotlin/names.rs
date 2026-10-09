//! Naming policy: the Kotlin package, the Kotlin name of every user type,
//! error domain, member, field, and module object, the public Kotlin type of
//! every IR type, and JNI name mangling.
//!
//! User types live at the top level of the package, so a type whose name is
//! a Kotlin keyword, a type Kotlin or Java imports by default (`Unit`,
//! `String`, `Result`, ...), or a name the generated runtime declares gains a
//! trailing underscore (`Unit` becomes `Unit_`). A module object whose name
//! would equal a type's gains a `Module` suffix (`kv.stats` beside a `Stats`
//! record becomes `Kv.StatsModule`). Callables, parameters, and fields are
//! lowerCamelCased (`expires_at` becomes `expiresAt`).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::codegen::common::pascal_case;
use crate::codegen::errors;
use crate::lang;
use weaveffi_model::model::{FnBinding, Model, ModuleBinding};
use weaveffi_model::plan::ErrorStrategy;
use weaveffi_model::ty::{ParamTy, Prim, RetTy, Ty};

/// Names user types must not take: Kotlin and Java types the compiler
/// imports into every file, plus the declarations of the generated runtime.
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
    "Char",
    "CharSequence",
    "Charsets",
    "Collection",
    "Comparable",
    "Deprecated",
    "Double",
    "DoubleArray",
    "Enum",
    "Error",
    "Exception",
    "FfiException",
    "Float",
    "FloatArray",
    "Function",
    "IllegalArgumentException",
    "IllegalStateException",
    "Int",
    "IntArray",
    "Iterable",
    "Iterator",
    "JniBridge",
    "JvmField",
    "JvmStatic",
    "Lazy",
    "List",
    "Long",
    "LongArray",
    "Map",
    "MutableList",
    "MutableMap",
    "MutableSet",
    "NativeBugException",
    "NativeCleaner",
    "NativeCompletion",
    "NativeHandle",
    "NativeIterator",
    "NativeLibrary",
    "NoSuchElementException",
    "Nothing",
    "Number",
    "Object",
    "Pair",
    "Result",
    "Runnable",
    "RuntimeException",
    "Sequence",
    "Set",
    "Short",
    "ShortArray",
    "String",
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

/// The generated files besides the module objects' (`{Object}.kt`): a module
/// object spelled like one of them gains a `Module` suffix, so the files
/// never collide.
const GENERATED_FILE_STEMS: &[&str] = &["Async", "Buffers", "Codecs", "JniBridge", "Runtime"];

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

/// JNI exports map a Java identifier to a C symbol by escaping `_` to `_1`
/// (plus `;`, `[`, and non-ASCII characters).
pub(crate) fn jni_mangle(ident: &str) -> String {
    let mut out = String::with_capacity(ident.len());
    for c in ident.chars() {
        match c {
            '_' => out.push_str("_1"),
            ';' => out.push_str("_2"),
            '[' => out.push_str("_3"),
            c if c.is_ascii_alphanumeric() => out.push(c),
            c => {
                let _ = write!(out, "_0{:04x}", c as u32);
            }
        }
    }
    out
}

/// The `JniBridge.error` domain of a call that can't fail: its failure
/// raises `NativeBugException`.
pub(crate) const TRAP_DOMAIN: u32 = 0;
/// The `JniBridge.error` domain of a `throws: any` call: its failure raises
/// `FfiException` with the code and message.
pub(crate) const UNTYPED_DOMAIN: u32 = 1;

/// Every name the generated Kotlin and JNI code needs, resolved once from
/// the identity, the target configuration, and the model.
pub(crate) struct Names {
    /// The Kotlin package (`kvstore`).
    pub package: String,
    /// The package as a JVM internal path (`kvstore`, `com/example/kv`).
    pub package_path: String,
    /// The C symbol prefix (`kvstore`).
    pub prefix: String,
    /// The library name the producer is loaded as and the JNI shim derives
    /// from (`kvstore`).
    pub library: String,
    types: HashMap<String, String>,
    /// Every error domain's exception class and `JniBridge.error` index, by
    /// the domain's (global) name, in declaration order.
    domains: Vec<(String, String)>,
    objects: HashMap<String, String>,
    /// Every interface's `_destroy` symbol, by interface name.
    destroys: HashMap<String, String>,
}

impl Names {
    /// Resolve every name for `model`, with `package` the Kotlin package
    /// (already validated) and `library` the producer library name.
    pub(crate) fn new(model: &Model, package: &str, library: &str) -> Self {
        let mut types = HashMap::new();
        let mut destroys = HashMap::new();
        for m in &model.modules {
            for i in &m.interfaces {
                destroys.insert(i.name.clone(), i.destroy_symbol.clone());
            }
            let declared = m
                .enums
                .iter()
                .map(|e| &e.name)
                .chain(m.structs.iter().map(|s| &s.name))
                .chain(m.interfaces.iter().map(|i| &i.name))
                .chain(m.callback_interfaces.iter().map(|c| &c.name));
            for name in declared {
                types.insert(name.clone(), kt_type_ident(name));
            }
        }
        let domains: Vec<(String, String)> = errors::tables(model, "Exception")
            .into_iter()
            .map(|t| (t.domain.name.clone(), kt_type_ident(&t.type_name)))
            .collect();
        let taken: HashSet<&String> = types
            .values()
            .chain(domains.iter().map(|(_, exc)| exc))
            .collect();
        let objects = model
            .modules
            .iter()
            .map(|m| {
                let mut name = kt_type_ident(&pascal_case(&m.name));
                if taken.contains(&name) || GENERATED_FILE_STEMS.contains(&name.as_str()) {
                    name.push_str("Module");
                }
                (m.path.clone(), name)
            })
            .collect();
        Self {
            package: package.to_string(),
            package_path: package.replace('.', "/"),
            prefix: model.prefix().to_string(),
            library: library.to_string(),
            types,
            domains,
            objects,
            destroys,
        }
    }

    /// The `_destroy` symbol of the interface `name`.
    ///
    /// # Panics
    ///
    /// Panics when no interface has that name, which validation rules out.
    pub(crate) fn destroy_symbol(&self, name: &str) -> &str {
        self.destroys
            .get(name)
            .unwrap_or_else(|| panic!("interface '{name}' is not declared"))
    }

    /// The Kotlin class of the user type `name`.
    pub(crate) fn ty(&self, name: &str) -> String {
        self.types
            .get(name)
            .cloned()
            .unwrap_or_else(|| kt_type_ident(name))
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

    /// The `JniBridge.error` domain a failure of a callable with `error`
    /// maps through: [`TRAP_DOMAIN`], [`UNTYPED_DOMAIN`], or the domain's
    /// index (from 2, in declaration order).
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

    /// Every declared domain's `JniBridge.error` index and exception class,
    /// in index order.
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

    /// The simple Kotlin name of a module's object.
    pub(crate) fn object(&self, m: &ModuleBinding) -> String {
        self.objects
            .get(&m.path)
            .cloned()
            .unwrap_or_else(|| pascal_case(&m.name))
    }

    /// The Kotlin name of a free function: its bare name (names are global).
    pub(crate) fn function(&self, f: &FnBinding) -> String {
        kt_member(&f.name)
    }

    /// The `JniBridge` member name for a C symbol: the symbol without the
    /// library prefix, unique because C symbols are.
    pub(crate) fn native(&self, symbol: &str) -> String {
        symbol
            .strip_prefix(&format!("{}_", self.prefix))
            .unwrap_or(symbol)
            .to_string()
    }

    /// The JNI export name of a `JniBridge` native.
    pub(crate) fn jni_export(&self, native: &str) -> String {
        format!("Java_{}_{}", self.jni_class(), jni_mangle(native))
    }

    /// The mangled `{package}_JniBridge` part of every JNI export name.
    pub(crate) fn jni_class(&self) -> String {
        let pkg: Vec<String> = self.package.split('.').map(jni_mangle).collect();
        format!("{}_JniBridge", pkg.join("_"))
    }

    /// The public Kotlin type of an IR value type.
    pub(crate) fn kt_type(&self, t: &Ty) -> String {
        match t {
            Ty::Prim(Prim::I8) => "Byte".into(),
            Ty::Prim(Prim::U8) => "UByte".into(),
            Ty::Prim(Prim::I16) => "Short".into(),
            Ty::Prim(Prim::U16) => "UShort".into(),
            Ty::Prim(Prim::I32) => "Int".into(),
            Ty::Prim(Prim::U32) => "UInt".into(),
            Ty::Prim(Prim::I64) => "Long".into(),
            Ty::Prim(Prim::U64) => "ULong".into(),
            Ty::Prim(Prim::F32) => "Float".into(),
            Ty::Prim(Prim::F64) => "Double".into(),
            Ty::Prim(Prim::Bool) => "Boolean".into(),
            Ty::Prim(Prim::String) => "String".into(),
            Ty::Prim(Prim::Bytes) => "ByteArray".into(),
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
    /// `NativeIterator` of the element type.
    pub(crate) fn kt_ret_type(&self, t: &RetTy) -> String {
        match t {
            RetTy::Value(t) => self.kt_type(t),
            RetTy::Iterator(elem) => format!("NativeIterator<{}>", self.kt_type(elem)),
        }
    }
}

/// For an unsigned integer type, the Kotlin conversions from its JNI carrier
/// (`toUInt`) and back (`toInt`), both of which reinterpret the bits; `None`
/// for every other type.
pub(crate) fn unsigned_conversions(t: &Ty) -> Option<(&'static str, &'static str)> {
    match t {
        Ty::Prim(Prim::U8) => Some(("toUByte", "toByte")),
        Ty::Prim(Prim::U16) => Some(("toUShort", "toShort")),
        Ty::Prim(Prim::U32) => Some(("toUInt", "toInt")),
        Ty::Prim(Prim::U64) => Some(("toULong", "toLong")),
        _ => None,
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
}
