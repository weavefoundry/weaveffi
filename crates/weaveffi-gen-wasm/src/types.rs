//! JS/TS naming, type mapping, and API scanning.
//!
//! Everything here answers "how is this IDL name or type spelled in
//! JavaScript and TypeScript?": wasm boundary types for the README tables,
//! TS annotations, exported function and parameter names (escaped against
//! the shared JS keyword table), the error-surface naming policy, and the
//! deep type scans that decide which runtime helpers the loader embeds.

use heck::ToLowerCamelCase;
use weaveffi_core::abi::is_buffered;
use weaveffi_core::lang;
use weaveffi_core::model::{ErrorBinding, FnBinding, ParamBinding};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::{Api, Module, TypeRef};

/// The wasm value-type spelling of one IDL type at the boundary, for the
/// README's signature tables. Buffered types occupy two `i32` slots (pointer
/// plus length); everything else keeps its scalar or pointer slot.
pub(crate) fn wasm_type(ty: &TypeRef) -> &'static str {
    if is_buffered(ty) {
        return "i32, i32";
    }
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::Bool
        | TypeRef::Enum(_) => "i32",
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => "i64",
        TypeRef::F32 => "f32",
        TypeRef::F64 => "f64",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "i32",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "i32, i32",
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) | TypeRef::Iterator(_) => "i32",
        // Only `Interface?` reaches here: a nullable object pointer.
        TypeRef::Optional(_) => "i32",
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The one-line boundary note the README's signature tables attach to one
/// IDL type: how its wasm slot(s) are interpreted.
pub(crate) fn wasm_type_note(ty: &TypeRef) -> &'static str {
    if is_buffered(ty) {
        return "value buffer: ptr + len in linear memory";
    }
    match ty {
        TypeRef::I8 => "8-bit signed mapped to i32",
        TypeRef::I16 => "16-bit signed mapped to i32",
        TypeRef::I32 => "native Wasm i32",
        TypeRef::U8 => "8-bit unsigned mapped to i32",
        TypeRef::U16 => "16-bit unsigned mapped to i32",
        TypeRef::U32 => "unsigned mapped to i32",
        TypeRef::I64 => "native Wasm i64",
        TypeRef::U64 => "unsigned mapped to i64",
        TypeRef::F32 => "native Wasm f32",
        TypeRef::F64 => "native Wasm f64",
        TypeRef::Bool => "0 = false, 1 = true",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "NUL-terminated C string pointer",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "ptr + len in linear memory",
        TypeRef::TypedHandle(_) => "opaque pointer",
        TypeRef::Handle => "opaque 64-bit handle",
        TypeRef::Interface(_) => "opaque object pointer",
        TypeRef::Enum(_) => "variant discriminant",
        TypeRef::Iterator(_) => "opaque iterator handle",
        TypeRef::Optional(_) => "nullable object pointer, 0 = absent",
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The IDL-flavored display spelling of one type for the README's API
/// reference (`Contact?`, `[string]`, `{string:i32}`).
pub(crate) fn type_display(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "i8".into(),
        TypeRef::I16 => "i16".into(),
        TypeRef::I32 => "i32".into(),
        TypeRef::U8 => "u8".into(),
        TypeRef::U16 => "u16".into(),
        TypeRef::U32 => "u32".into(),
        TypeRef::I64 => "i64".into(),
        TypeRef::U64 => "u64".into(),
        TypeRef::F32 => "f32".into(),
        TypeRef::F64 => "f64".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "bytes".into(),
        TypeRef::TypedHandle(_) | TypeRef::Handle => "handle".into(),
        TypeRef::Record(n) | TypeRef::RichEnum(n) => local_type_name(n).to_string(),
        TypeRef::Enum(n) => n.clone(),
        TypeRef::Optional(inner) => format!("{}?", type_display(inner)),
        TypeRef::List(inner) => format!("[{}]", type_display(inner)),
        TypeRef::Iterator(inner) => format!("iter<{}>", type_display(inner)),
        TypeRef::Map(k, v) => format!("{{{}:{}}}", type_display(k), type_display(v)),
        TypeRef::Interface(n) => local_type_name(n).to_string(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// The TS annotation of one IR type.
pub(crate) fn ts_type_for(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::F32
        | TypeRef::F64 => "number".into(),
        TypeRef::Bool => "boolean".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        // Bytes cross the boundary as plain `Uint8Array` copies; the Node-only
        // `Buffer` type does not exist in browsers and is never returned here.
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Uint8Array".into(),
        // Every 64-bit integer crosses the JS boundary as a BigInt: wasm i64
        // results arrive as BigInt and i64 arguments are BigInt-coerced.
        TypeRef::I64 | TypeRef::U64 | TypeRef::Handle => "bigint".into(),
        // A typed handle is an opaque i32 pointer at the ABI, surfaced as a
        // plain number.
        TypeRef::TypedHandle(_) => "number".into(),
        // Records, rich enums, plain enums, and interfaces surface as bare
        // local TS names; a cross-module reference (resolved to e.g.
        // `kv.Store`) must name the local `Store`, not the qualified IR name
        // which is undeclared here.
        TypeRef::Enum(name)
        | TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Interface(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{} | null", ts_type_for(inner)),
        TypeRef::List(inner) => {
            let inner_ts = ts_type_for(inner);
            if matches!(inner.as_ref(), TypeRef::Optional(_)) {
                format!("({inner_ts})[]")
            } else {
                format!("{inner_ts}[]")
            }
        }
        // `iter<T>` streams lazily; the wrapper is a JS iterator, never a
        // drained array.
        TypeRef::Iterator(inner) => {
            let t = ts_type_for(inner);
            format!("IterableIterator<{t}>")
        }
        TypeRef::Map(k, v) => format!("Record<{}, {}>", ts_type_for(k), ts_type_for(v)),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// True if `ty` is one of the UTF-8 string spellings.
pub(crate) fn is_string_type(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr)
}

/// Whether `ty` or any type nested inside it (optional payloads, list and
/// iterator elements, map keys/values) satisfies `pred`.
fn typeref_deep_any(ty: &TypeRef, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
    if pred(ty) {
        return true;
    }
    match ty {
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            typeref_deep_any(inner, pred)
        }
        TypeRef::Map(k, v) => typeref_deep_any(k, pred) || typeref_deep_any(v, pred),
        _ => false,
    }
}

/// Visit every boundary-crossing type in `api` (function, interface-member,
/// and callback params and returns; struct, variant, and error payload field
/// types), recursing into composite types, and return whether any satisfies
/// `pred`.
pub(crate) fn api_deep_any(api: &Api, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
    fn deep(ty: &TypeRef, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        typeref_deep_any(ty, pred)
    }
    fn fn_any(f: &weaveffi_ir::ir::Function, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        f.params.iter().any(|p| deep(&p.ty, pred))
            || f.returns.as_ref().is_some_and(|r| deep(r, pred))
    }
    fn module_any(m: &Module, pred: &dyn Fn(&TypeRef) -> bool) -> bool {
        m.functions.iter().any(|f| fn_any(f, pred))
            // Interface members marshal exactly like free functions.
            || m.interfaces.iter().any(|i| {
                i.constructors
                    .iter()
                    .chain(i.methods.iter())
                    .chain(i.statics.iter())
                    .any(|f| fn_any(f, pred))
            })
            || m
            .structs
            .iter()
            .any(|s| s.fields.iter().any(|f| deep(&f.ty, pred)))
            // Rich (algebraic) enums serialize their variant fields exactly
            // like struct fields, so a string/bytes/list living only inside a
            // variant payload still pulls in the corresponding helpers.
            || m.enums.iter().any(|e| {
                e.variants
                    .iter()
                    .any(|v| v.fields.iter().any(|f| deep(&f.ty, pred)))
            })
            // Callback arguments are decoded by the listener trampolines.
            || m.callbacks
                .iter()
                .any(|c| c.params.iter().any(|p| deep(&p.ty, pred)))
            // Error payload fields are decoded from the error's value buffer.
            || m.errors.as_ref().is_some_and(|d| {
                d.codes
                    .iter()
                    .any(|c| c.fields.iter().any(|f| deep(&f.ty, pred)))
            })
            || m.modules.iter().any(|sub| module_any(sub, pred))
    }
    api.modules.iter().any(|m| module_any(m, pred))
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
