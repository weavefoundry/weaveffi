//! JS/TS naming, type mapping, and API scanning.
//!
//! Everything here answers "how is this IDL name or type spelled in
//! JavaScript and TypeScript?": wasm boundary types for the README tables,
//! TS annotations, exported function and parameter names (escaped against
//! the shared JS keyword table), the error-surface naming policy, and the
//! deep type scans that decide which runtime helpers the loader embeds.

use heck::ToLowerCamelCase;
use weaveffi_core::lang;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{ErrorBinding, FnBinding, ParamBinding};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_core::utils::local_type_name;

/// The wasm value-type spelling of one IDL type at the boundary, for the
/// README's signature tables. Buffered types occupy two `i32` slots (pointer
/// plus length); everything else keeps its scalar or pointer slot.
pub(crate) fn wasm_type(ty: &Ty) -> &'static str {
    if ty.is_buffered() {
        return "i32, i32";
    }
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::Bool | Ty::Enum(_) => "i32",
        Ty::I64 | Ty::U64 | Ty::Handle => "i64",
        Ty::F32 => "f32",
        Ty::F64 => "f64",
        Ty::StringUtf8 | Ty::BorrowedStr => "i32",
        Ty::Bytes | Ty::BorrowedBytes => "i32, i32",
        Ty::TypedHandle(_) | Ty::Interface(_) | Ty::Iterator(_) => "i32",
        // Only `Interface?` reaches here: a nullable object pointer.
        Ty::Optional(_) => "i32",
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
    }
}

/// The one-line boundary note the README's signature tables attach to one
/// IDL type: how its wasm slot(s) are interpreted.
pub(crate) fn wasm_type_note(ty: &Ty) -> &'static str {
    if ty.is_buffered() {
        return "value buffer: ptr + len in linear memory";
    }
    match ty {
        Ty::I8 => "8-bit signed mapped to i32",
        Ty::I16 => "16-bit signed mapped to i32",
        Ty::I32 => "native Wasm i32",
        Ty::U8 => "8-bit unsigned mapped to i32",
        Ty::U16 => "16-bit unsigned mapped to i32",
        Ty::U32 => "unsigned mapped to i32",
        Ty::I64 => "native Wasm i64",
        Ty::U64 => "unsigned mapped to i64",
        Ty::F32 => "native Wasm f32",
        Ty::F64 => "native Wasm f64",
        Ty::Bool => "0 = false, 1 = true",
        Ty::StringUtf8 | Ty::BorrowedStr => "NUL-terminated C string pointer",
        Ty::Bytes | Ty::BorrowedBytes => "ptr + len in linear memory",
        Ty::TypedHandle(_) => "opaque pointer",
        Ty::Handle => "opaque 64-bit handle",
        Ty::Interface(_) => "opaque object pointer",
        Ty::Enum(_) => "variant discriminant",
        Ty::Iterator(_) => "opaque iterator handle",
        Ty::Optional(_) => "nullable object pointer, 0 = absent",
        Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
    }
}

/// The IDL-flavored display spelling of one type for the README's API
/// reference (`Contact?`, `[string]`, `{string:i32}`).
pub(crate) fn type_display(ty: &Ty) -> String {
    match ty {
        Ty::I8 => "i8".into(),
        Ty::I16 => "i16".into(),
        Ty::I32 => "i32".into(),
        Ty::U8 => "u8".into(),
        Ty::U16 => "u16".into(),
        Ty::U32 => "u32".into(),
        Ty::I64 => "i64".into(),
        Ty::U64 => "u64".into(),
        Ty::F32 => "f32".into(),
        Ty::F64 => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::StringUtf8 | Ty::BorrowedStr => "string".into(),
        Ty::Bytes | Ty::BorrowedBytes => "bytes".into(),
        Ty::TypedHandle(_) | Ty::Handle => "handle".into(),
        Ty::Record(n) | Ty::RichEnum(n) => local_type_name(n).to_string(),
        Ty::Enum(n) => n.clone(),
        Ty::Optional(inner) => format!("{}?", type_display(inner)),
        Ty::List(inner) => format!("[{}]", type_display(inner)),
        Ty::Iterator(inner) => format!("iter<{}>", type_display(inner)),
        Ty::Map(k, v) => format!("{{{}:{}}}", type_display(k), type_display(v)),
        Ty::Interface(n) => local_type_name(n).to_string(),
    }
}

/// The TS annotation of one IR type.
pub(crate) fn ts_type_for(ty: &Ty) -> String {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::F32 | Ty::F64 => {
            "number".into()
        }
        Ty::Bool => "boolean".into(),
        Ty::StringUtf8 | Ty::BorrowedStr => "string".into(),
        // Bytes cross the boundary as plain `Uint8Array` copies; the Node-only
        // `Buffer` type does not exist in browsers and is never returned here.
        Ty::Bytes | Ty::BorrowedBytes => "Uint8Array".into(),
        // Every 64-bit integer crosses the JS boundary as a BigInt: wasm i64
        // results arrive as BigInt and i64 arguments are BigInt-coerced.
        Ty::I64 | Ty::U64 | Ty::Handle => "bigint".into(),
        // A typed handle is an opaque i32 pointer at the ABI, surfaced as a
        // plain number.
        Ty::TypedHandle(_) => "number".into(),
        // Records, rich enums, plain enums, and interfaces surface as bare
        // local TS names; a cross-module reference (resolved to e.g.
        // `kv.Store`) must name the local `Store`, not the qualified IR name
        // which is undeclared here.
        Ty::Enum(name) | Ty::Record(name) | Ty::RichEnum(name) | Ty::Interface(name) => {
            local_type_name(name).to_string()
        }
        Ty::Optional(inner) => format!("{} | null", ts_type_for(inner)),
        Ty::List(inner) => {
            let inner_ts = ts_type_for(inner);
            if matches!(inner.as_ref(), Ty::Optional(_)) {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        // `iter<T>` streams lazily; the wrapper is a JS iterator, never a
        // drained array.
        Ty::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("IterableIterator<{t}>")
        }
        // Maps decode into a plain object, whose keys are always strings at
        // runtime; a `bigint` key would also be rejected by `Record`'s key
        // constraint, so 64-bit keys are typed as the strings they become.
        Ty::Map(k, v) => {
            let key = match k.as_ref() {
                Ty::I64 | Ty::U64 | Ty::Handle => "string".to_string(),
                other => ts_type_for(other),
            };
            format!("Record<{key}, {}>", ts_type_for(v))
        }
    }
}

/// True if `ty` is one of the UTF-8 string spellings.
pub(crate) fn is_string_type(ty: &Ty) -> bool {
    matches!(ty, Ty::StringUtf8 | Ty::BorrowedStr)
}

// ── Naming and error-surface policy ──

/// The lowerCamelCase JS name a callable is exposed under (`list_keys` becomes
/// `listKeys`). Functions are namespaced by module object, so exported names
/// never carry a module prefix in the first place.
pub(crate) fn js_fn_name(f: &FnBinding) -> String {
    f.name.to_lower_camel_case()
}

/// The camelCase JS spelling of one parameter (`ttl_seconds` becomes
/// `ttlSeconds`), escaped when the camel-cased form collides with a JS/TS
/// reserved word (a parameter named `new` becomes `new_`; property and
/// method positions never need the escape, so only parameters route through
/// here).
pub(crate) fn js_param_name(p: &ParamBinding) -> String {
    lang::escape_ident(&p.name.to_lower_camel_case(), lang::JS_KEYWORDS)
}

/// The JS class name for one error code: plain PascalCase with no forced
/// suffix (`KeyNotFound`, not `KeyNotFoundError`). Code names are validated
/// to be globally unique across domains, so the flat name cannot collide.
pub(crate) fn js_code_class_name(name: &str) -> String {
    weaveffi_core::errors::pascal(name)
}

/// `_{typeName}From` (lowerCamel): builds the domain error matching an ABI
/// code, e.g. `_kvErrorFrom`.
pub(crate) fn js_error_factory_name(eb: &ErrorBinding) -> String {
    format!("_{}From", eb.type_name.to_lower_camel_case())
}

/// `_check{TypeName}`: throws the domain error for a non-zero out-err slot,
/// e.g. `_checkKvError`.
pub(crate) fn js_error_checker_name(eb: &ErrorBinding) -> String {
    format!("_check{}", eb.type_name)
}

/// The error-check helper a callable's out-err slot routes through, per its
/// [`ErrorStrategy`]: the module domain's typed checker for
/// [`ErrorStrategy::Throws`], the generic `_checkErr` (plain `WeaveFFIError`;
/// panics and marshalling failures only) for [`ErrorStrategy::Trap`].
pub(crate) fn js_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => js_error_checker_name(eb),
        _ => "_checkErr".to_string(),
    }
}

/// The rejection factory a throwing async callable stores in its context so
/// the completion callback maps domain codes to the typed error, or `None`
/// for [`ErrorStrategy::Trap`] callables (which reject with the generic
/// brand error).
pub(crate) fn js_err_factory(f: &FnBinding, error: Option<&ErrorBinding>) -> Option<String> {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => Some(js_error_factory_name(eb)),
        _ => None,
    }
}

/// Escape a string for embedding in a double-quoted JS literal.
pub(crate) fn js_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How a generated JS callable is declared: as a property of a module object
/// literal (`name() {...},`), as an instance member of an interface class
/// (`name() {...}`), or as a static member (`static name() {...}`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsDecl {
    /// Object-literal property (module objects); comma-terminated.
    Object,
    /// Class instance method; no terminator comma.
    Method,
    /// Class static method; no terminator comma.
    Static,
}

impl JsDecl {
    /// The declaration keyword prefix (`static ` for statics).
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            JsDecl::Static => "static ",
            _ => "",
        }
    }

    /// The block terminator (object-literal members carry a trailing comma).
    pub(crate) fn close(self) -> &'static str {
        match self {
            JsDecl::Object => "},",
            _ => "}",
        }
    }
}
