//! Naming policy: the Kotlin package, the Kotlin name of every user type,
//! error domain, and module object, the Kotlin and JNI types each IR type
//! crosses as, and JNI name mangling.
//!
//! User types live at the top level of the package, so a type whose name is
//! a Kotlin keyword, a type Kotlin or Java imports by default (`Unit`,
//! `String`, `Result`, ...), or a name the generated runtime declares gains a
//! trailing underscore (`Unit` becomes `Unit_`). A module object whose name
//! would equal a type's gains a `Module` suffix (`kv.stats` beside a `Stats`
//! record becomes `Kv.StatsModule`).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use crate::codegen::common::pascal_case;
use crate::lang;
use crate::utils::{local_type_name, wrapper_name};
use weaveffi_model::errors;
use weaveffi_model::model::{BindingModel, ErrorBinding, FnBinding, ModuleBinding, Ty};

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
    "JvmStatic",
    "Lazy",
    "List",
    "Long",
    "LongArray",
    "Map",
    "MutableList",
    "MutableMap",
    "MutableSet",
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
    "Triple",
    "Unit",
    "Volatile",
];

/// Member names the generated wrapper classes already use (or inherit from
/// `Any`); a method, static, or callback method spelled the same gains a
/// trailing underscore.
const RESERVED_MEMBER_NAMES: &[&str] = &[
    "cloneHandle",
    "close",
    "equals",
    "fromHandle",
    "fromHandleOrNull",
    "handle",
    "hashCode",
    "toString",
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

/// The Kotlin spelling of a parameter: lowerCamelCased, then escaped. It
/// never starts with `_`, which keeps every generated local (all spelled
/// `_name`) out of its way.
pub(crate) fn kt_param(name: &str) -> String {
    kt_escape(&lower_camel(name))
}

/// The Kotlin spelling of a method, static, or callback method: like
/// [`kt_param`], and also escaped away from the members every wrapper
/// declares.
pub(crate) fn kt_member(name: &str) -> String {
    let n = kt_param(name);
    if RESERVED_MEMBER_NAMES.contains(&n.as_str()) {
        format!("{n}_")
    } else {
        n
    }
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
    /// Whether free-function names drop their module path.
    pub strip_module_prefix: bool,
    types: HashMap<String, String>,
    exceptions: HashMap<String, String>,
    domains: HashMap<String, u32>,
    objects: HashMap<String, String>,
}

impl Names {
    /// Resolve every name for `model`, with `package` the Kotlin package
    /// (already validated) and `library` the producer library name.
    pub(crate) fn new(
        model: &BindingModel,
        package: &str,
        library: &str,
        strip_module_prefix: bool,
    ) -> Self {
        let mut types = HashMap::new();
        let mut exceptions = HashMap::new();
        let mut domains = HashMap::new();
        for m in &model.modules {
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
            if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
                exceptions.insert(
                    eb.c_tag.clone(),
                    kt_type_ident(&errors::exception_type_name(&eb.name)),
                );
                let next = u32::try_from(domains.len() + 1).unwrap_or(u32::MAX);
                domains.insert(eb.c_tag.clone(), next);
            }
        }
        let taken: HashSet<&String> = types.values().chain(exceptions.values()).collect();
        let objects = model
            .modules
            .iter()
            .map(|m| {
                let mut name = kt_type_ident(&pascal_case(&m.name));
                if taken.contains(&name) {
                    name.push_str("Module");
                }
                (m.path.clone(), name)
            })
            .collect();
        Self {
            package: package.to_string(),
            package_path: package.replace('.', "/"),
            prefix: model.prefix.clone(),
            library: library.to_string(),
            strip_module_prefix,
            types,
            exceptions,
            domains,
            objects,
        }
    }

    /// The Kotlin class of a user type named `ir_name` (bare or dotted).
    pub(crate) fn ty(&self, ir_name: &str) -> String {
        let local = local_type_name(ir_name);
        self.types
            .get(local)
            .cloned()
            .unwrap_or_else(|| kt_type_ident(local))
    }

    /// The Kotlin exception class of an error domain.
    pub(crate) fn exception(&self, eb: &ErrorBinding) -> String {
        self.exceptions
            .get(&eb.c_tag)
            .cloned()
            .unwrap_or_else(|| kt_type_ident(&errors::exception_type_name(&eb.name)))
    }

    /// The domain index `JniBridge.error` maps a failure of `f` through:
    /// the module's domain for a throwing callable, else 0 (generic).
    pub(crate) fn domain(&self, f: &FnBinding, error: Option<&ErrorBinding>) -> u32 {
        match error {
            Some(eb) if f.throws => self.domains.get(&eb.c_tag).copied().unwrap_or(0),
            _ => 0,
        }
    }

    /// Every declared domain with its index, in index order.
    pub(crate) fn domains<'a>(&self, model: &'a BindingModel) -> Vec<(u32, &'a ErrorBinding)> {
        let mut out: Vec<(u32, &ErrorBinding)> = model
            .modules
            .iter()
            .filter_map(|m| m.error.as_ref().filter(|e| e.declared_here))
            .filter_map(|eb| self.domains.get(&eb.c_tag).map(|i| (*i, eb)))
            .collect();
        out.sort_by_key(|(i, _)| *i);
        out
    }

    /// The simple Kotlin name of a module's object.
    pub(crate) fn object(&self, m: &ModuleBinding) -> String {
        self.objects
            .get(&m.path)
            .cloned()
            .unwrap_or_else(|| pascal_case(&m.name))
    }

    /// The Kotlin name of a free function.
    pub(crate) fn function(&self, m: &ModuleBinding, f: &FnBinding) -> String {
        kt_member(&wrapper_name(&m.path, &f.name, self.strip_module_prefix))
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

    /// The public Kotlin type of an IR type.
    pub(crate) fn kt_type(&self, t: &Ty) -> String {
        match t {
            Ty::I8 | Ty::U8 => "Byte".into(),
            Ty::I16 | Ty::U16 => "Short".into(),
            Ty::I32 => "Int".into(),
            Ty::U32 | Ty::I64 | Ty::U64 => "Long".into(),
            Ty::F32 => "Float".into(),
            Ty::F64 => "Double".into(),
            Ty::Bool => "Boolean".into(),
            Ty::StringUtf8 => "String".into(),
            Ty::Bytes => "ByteArray".into(),
            Ty::Record(n)
            | Ty::RichEnum(n)
            | Ty::Enum(n)
            | Ty::Interface(n)
            | Ty::CallbackInterface(n) => self.ty(n),
            Ty::Optional(inner) => format!("{}?", self.kt_type(inner)),
            Ty::List(inner) => format!("List<{}>", self.kt_type(inner)),
            Ty::Map(k, v) => format!("Map<{}, {}>", self.kt_type(k), self.kt_type(v)),
            Ty::Iterator(inner) => format!("NativeIterator<{}>", self.kt_type(inner)),
        }
    }

    /// The Kotlin type a value crosses the JNI boundary as: strings, bytes,
    /// and value buffers as `ByteArray`, enums as `Int`, objects and
    /// iterators as their address (`0L` is none), and a callback interface
    /// as the implementing object.
    pub(crate) fn jni_type(&self, t: &Ty) -> String {
        match t {
            Ty::Enum(_) => "Int".into(),
            Ty::StringUtf8 | Ty::Bytes => "ByteArray".into(),
            Ty::Interface(_) | Ty::Iterator(_) => "Long".into(),
            Ty::CallbackInterface(n) => self.ty(n),
            _ if t.is_buffered() => "ByteArray".into(),
            Ty::Optional(_) => "Long".into(),
            other => self.kt_type(other),
        }
    }

    /// The JVM type descriptor of [`Names::jni_type`], for the method IDs
    /// the shim caches.
    pub(crate) fn jni_descriptor(&self, t: Option<&Ty>) -> String {
        let Some(t) = t else {
            return "V".into();
        };
        match self.jni_type(t).as_str() {
            "Byte" => "B".into(),
            "Short" => "S".into(),
            "Int" => "I".into(),
            "Long" => "J".into(),
            "Float" => "F".into(),
            "Double" => "D".into(),
            "Boolean" => "Z".into(),
            "ByteArray" => "[B".into(),
            class => format!("L{}/{class};", self.package_path),
        }
    }
}

/// The JNI C type of [`Names::jni_type`].
pub(crate) fn jni_c_type(t: &Ty) -> &'static str {
    match t {
        Ty::I8 | Ty::U8 => "jbyte",
        Ty::I16 | Ty::U16 => "jshort",
        Ty::I32 | Ty::Enum(_) => "jint",
        Ty::U32 | Ty::I64 | Ty::U64 | Ty::Interface(_) | Ty::Iterator(_) => "jlong",
        Ty::F32 => "jfloat",
        Ty::F64 => "jdouble",
        Ty::Bool => "jboolean",
        Ty::StringUtf8 | Ty::Bytes => "jbyteArray",
        Ty::CallbackInterface(_) => "jobject",
        _ if t.is_buffered() => "jbyteArray",
        Ty::Optional(_) => "jlong",
        _ => unreachable!("every type is covered above"),
    }
}

/// The `Call{Kind}Method` / `on{Kind}` stem for a direct-family or object
/// value: `Boolean`, `Byte`, `Short`, `Int`, `Long`, `Float`, or `Double`.
pub(crate) fn jni_kind(t: &Ty) -> &'static str {
    match t {
        Ty::Bool => "Boolean",
        Ty::I8 | Ty::U8 => "Byte",
        Ty::I16 | Ty::U16 => "Short",
        Ty::I32 | Ty::Enum(_) => "Int",
        Ty::F32 => "Float",
        Ty::F64 => "Double",
        _ => "Long",
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
