//! JavaScript and TypeScript spellings of IDL names and types.
//!
//! Generated code keeps three kinds of names apart so none can collide:
//!
//! * **Public names** are what consumers write: `kv.Store`, `kv.openStore`.
//!   Identifiers that are reserved words gain a trailing underscore.
//! * **Declaration names** are the module-scope bindings of user
//!   declarations, the declaring module's path and the name joined with `$`
//!   (`kv$Store`, `kv$stats$getStats`). IDL identifiers never contain `$`,
//!   so two modules declaring the same name never clash, and no declaration
//!   can shadow a JavaScript global.
//! * **Helper names** start with `$` (`$lend`, `$w$kv$Entry`): runtime
//!   imports and generated support code, which no IDL name can reach.

use heck::ToLowerCamelCase;
use weaveffi_model::model::Model;
use weaveffi_model::plan::ErrorStrategy;
use weaveffi_model::ty::{Prim, Ty, WireType};

use crate::codegen::docs::{self, ApiNames, IdentKind};
use crate::lang;

/// Identifiers that are not JS keywords but can't name a binding or
/// parameter in strict-mode (module) code.
const STRICT_RESERVED: &[&str] = &["arguments", "eval"];

/// Escape `name` with a trailing underscore when it can't be a JavaScript
/// binding (a keyword or a strict-mode reserved identifier).
pub(crate) fn js_ident(name: &str) -> String {
    if lang::is_reserved(name, lang::JS_KEYWORDS) || STRICT_RESERVED.contains(&name) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The public name of a free function (`open_store` becomes `openStore`).
pub(crate) fn fn_name(name: &str) -> String {
    js_ident(&name.to_lower_camel_case())
}

/// Instance members every interface class declares: the JavaScript
/// `constructor` and the runtime `$Object`'s `close`. (Its other members
/// start with `$` or are symbols, which no IDL name can reach.)
const OBJECT_MEMBERS: &[&str] = &["close", "constructor"];

/// Static members every class has (`Function`'s own properties), which a
/// static can't replace.
const CLASS_MEMBERS: &[&str] = &["caller", "length", "name", "prototype"];

/// The name of an interface member: lowerCamelCase, with a trailing
/// underscore when an instance member would replace one of
/// [`OBJECT_MEMBERS`] or a static (or factory) one of [`CLASS_MEMBERS`].
/// Property names may be reserved words, so no keyword escape applies.
pub(crate) fn member_name(name: &str, is_static: bool) -> String {
    let members = if is_static {
        CLASS_MEMBERS
    } else {
        OBJECT_MEMBERS
    };
    lang::escape_member(&name.to_lower_camel_case(), members)
}

/// The method a callback-interface implementation provides (`on_evict`
/// becomes `onEvict`).
pub(crate) fn callback_method_name(name: &str) -> String {
    name.to_lower_camel_case()
}

/// The parameter name of `name` (`ttl_seconds` becomes `ttlSeconds`).
pub(crate) fn param_name(name: &str) -> String {
    js_ident(&name.to_lower_camel_case())
}

/// The declaration name of `name` declared in module `segments`.
pub(crate) fn decl(segments: &[String], name: &str) -> String {
    format!("{}${name}", segments.join("$"))
}

/// The declaration name of the user type or error domain `name`, from its
/// declaring module's path (`Store` in `kv` becomes `kv$Store`).
pub(crate) fn type_decl(model: &Model, name: &str) -> String {
    decl(&model.owner(name).segments, name)
}

/// A helper name derived from a user type or error domain (`$w` and
/// `Entry` in `kv` give `$w$kv$Entry`).
pub(crate) fn helper(model: &Model, kind: &str, name: &str) -> String {
    format!("${kind}${}", type_decl(model, name))
}

/// The public name of a module (its namespace object).
pub(crate) fn module_name(name: &str) -> String {
    js_ident(name)
}

/// The TypeScript path of `name` declared in module `segments`, each module
/// segment escaped like the namespace it names (`kv.Store`).
pub(crate) fn ts_path_in(segments: &[String], name: &str) -> String {
    let mut out: Vec<String> = segments.iter().map(|s| js_ident(s)).collect();
    out.push(name.to_string());
    out.join(".")
}

/// The TypeScript path of the user type `name` (`kv.Store`).
pub(crate) fn ts_path(model: &Model, name: &str) -> String {
    ts_path_in(&model.owner(name).segments, name)
}

/// The class name of error domain `name` or of an error code `name`
/// (`KvError` stays `KvError`, `KEY_NOT_FOUND` becomes `KeyNotFoundError`).
pub(crate) fn error_class(name: &str) -> String {
    weaveffi_model::errors::type_name(name, "Error")
}

/// The TypeScript path of error domain `name`'s class (`kv.KvError`).
pub(crate) fn ts_domain(model: &Model, name: &str) -> String {
    ts_path_in(&model.owner(name).segments, &error_class(name))
}

/// The function that maps a native failure of a callable with error
/// strategy `error` onto the error classes: its domain's mapper
/// (`$from$kv$KvError`) when it throws a domain, the runtime's `$fault`
/// otherwise (a trap, or an untyped failure, is the root error class).
pub(crate) fn error_mapper(model: &Model, error: &ErrorStrategy) -> String {
    match error.domain() {
        Some(domain) => helper(model, "from", domain),
        None => "$fault".to_string(),
    }
}

/// The TypeScript type of `ty`.
pub(crate) fn ts_type(model: &Model, ty: &Ty) -> String {
    match ty {
        Ty::Prim(
            Prim::I8
            | Prim::I16
            | Prim::I32
            | Prim::U8
            | Prim::U16
            | Prim::U32
            | Prim::F32
            | Prim::F64,
        ) => "number".into(),
        Ty::Prim(Prim::I64 | Prim::U64) => "bigint".into(),
        Ty::Prim(Prim::Bool) => "boolean".into(),
        Ty::Prim(Prim::String) => "string".into(),
        Ty::Prim(Prim::Bytes) => "Uint8Array".into(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => ts_path(model, n),
        Ty::Optional(inner) => format!("{} | null", ts_type(model, inner)),
        Ty::List(inner) => {
            let t = ts_type(model, inner);
            if t.contains(' ') {
                format!("({t})[]")
            } else {
                format!("{t}[]")
            }
        }
        Ty::Map(k, v) => {
            let value = ts_type(model, v);
            match k.as_ref() {
                Ty::Enum(e) => format!("Partial<Record<{}, {value}>>", ts_path(model, e)),
                Ty::Prim(Prim::I8 | Prim::I16 | Prim::I32 | Prim::U8 | Prim::U16 | Prim::U32) => {
                    format!("Record<number, {value}>")
                }
                _ => format!("Record<string, {value}>"),
            }
        }
    }
}

/// The TypeScript type a typed-array (Slice) value of element `elem`
/// accepts where the consumer supplies it (an argument, or a callback
/// method's return): a plain array or the matching typed array
/// (`readonly number[] | Float64Array`).
pub(crate) fn ts_slice_input(elem: Prim) -> String {
    let scalar = ts_type_prim(elem);
    format!("readonly {scalar}[] | {}", typed_array(elem))
}

/// The TypeScript type of primitive `p`.
fn ts_type_prim(p: Prim) -> &'static str {
    match p {
        Prim::I64 | Prim::U64 => "bigint",
        Prim::Bool => "boolean",
        Prim::String => "string",
        Prim::Bytes => "Uint8Array",
        _ => "number",
    }
}

/// The typed array class a Slice of `elem` crosses as (`Float64Array` for
/// `f64`, `BigInt64Array` for `i64`).
///
/// # Panics
///
/// Panics on a primitive that never crosses as a typed array (`bool`,
/// `u8`, `string`, `bytes`).
pub(crate) fn typed_array(elem: Prim) -> &'static str {
    match elem {
        Prim::I8 => "Int8Array",
        Prim::I16 => "Int16Array",
        Prim::I32 => "Int32Array",
        Prim::I64 => "BigInt64Array",
        Prim::U16 => "Uint16Array",
        Prim::U32 => "Uint32Array",
        Prim::U64 => "BigUint64Array",
        Prim::F32 => "Float32Array",
        Prim::F64 => "Float64Array",
        Prim::Bool | Prim::U8 | Prim::String | Prim::Bytes => {
            unreachable!("{elem} never crosses as a typed array")
        }
    }
}

/// The runtime's name for the scalar a direct-family type is checked and
/// buffered as (`I32`, `U64`, `Bool`; C-style enums travel as their `i32`
/// discriminant): the key into `$W`, `$R`, `$ret`, and `$check`.
///
/// # Panics
///
/// Panics on a type that isn't a scalar or a C-style enum.
pub(crate) fn scalar_kind(ty: &Ty) -> &'static str {
    match ty.wire() {
        WireType::Prim(p) if p.is_scalar() => p.pascal(),
        WireType::Enum(_) => "I32",
        other => unreachable!("not a scalar: {other:?}"),
    }
}

/// The spelling closure doc and deprecation text is rewritten with: an
/// identifier in backticks becomes its JavaScript name (`new_op` becomes
/// `newOp`, an error domain or code its class), and each segment of a
/// dotted path (`Store.get`) is spelled on its own.
pub(crate) fn doc_spelling(names: &ApiNames) -> impl Fn(&str) -> Option<String> + '_ {
    let one = move |ident: &str| -> Option<String> {
        Some(match names.kind(ident)? {
            IdentKind::Function => fn_name(ident),
            IdentKind::Member | IdentKind::CallbackMethod | IdentKind::Param => {
                ident.to_lower_camel_case()
            }
            IdentKind::ErrorDomain | IdentKind::ErrorCode => error_class(ident),
            IdentKind::Module | IdentKind::Type | IdentKind::Variant | IdentKind::Field => {
                return None
            }
        })
    };
    move |path: &str| {
        let parts: Vec<String> = path
            .split('.')
            .map(|p| one(p).unwrap_or_else(|| p.to_string()))
            .collect();
        let spelled = parts.join(".");
        (spelled != path).then_some(spelled)
    }
}

/// `text` rewritten with [`doc_spelling`].
pub(crate) fn rewrite_doc(names: &ApiNames, text: &str) -> String {
    docs::rewrite(text, doc_spelling(names))
}

/// A single-quoted JavaScript string literal of `s`.
pub(crate) fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn reserved_words_gain_an_underscore() {
        assert_eq!(js_ident("class"), "class_");
        assert_eq!(js_ident("eval"), "eval_");
        assert_eq!(js_ident("store"), "store");
        assert_eq!(fn_name("delete"), "delete_");
        assert_eq!(fn_name("open_store"), "openStore");
        assert_eq!(param_name("import"), "import_");
        assert_eq!(member_name("close", false), "close_");
        assert_eq!(member_name("name", true), "name_");
        assert_eq!(member_name("name", false), "name");
    }

    /// `Entry` and `Kind` in `kv`, `C` in `a.b`, `Widget` in `class.inner`.
    pub(crate) fn model() -> Model {
        crate::codegen::test_model(
            r#"
version: "0.12.0"
modules:
  - name: kv
    enums: [{ name: Kind, variants: [{ name: A, value: 0 }] }]
    structs: [{ name: Entry, fields: [{ name: k, type: i32 }] }]
    interfaces: [{ name: Store, methods: [{ name: get }] }]
    errors:
      - name: KvErrors
        codes: [{ name: KEY_NOT_FOUND, code: 1, message: missing }]
    functions:
      - { name: open_store, params: [{ name: ttl_seconds, type: i64 }] }
  - name: a
    modules: [{ name: b, structs: [{ name: C, fields: [{ name: x, type: i32 }] }] }]
  - name: class
    modules: [{ name: inner, structs: [{ name: Widget, fields: [{ name: x, type: i32 }] }] }]
"#,
        )
    }

    #[test]
    fn declaration_names_join_the_path_with_dollars() {
        let m = model();
        let segs = vec!["a".to_string(), "b".to_string()];
        assert_eq!(decl(&segs, "C"), "a$b$C");
        assert_eq!(type_decl(&m, "C"), "a$b$C");
        assert_eq!(helper(&m, "w", "Entry"), "$w$kv$Entry");
        assert_eq!(helper(&m, "from", "KvErrors"), "$from$kv$KvErrors");
        assert_eq!(ts_path(&m, "Widget"), "class_.inner.Widget");
        assert_eq!(ts_domain(&m, "KvErrors"), "kv.KvError");
        assert_eq!(error_class("KEY_NOT_FOUND"), "KeyNotFoundError");
    }

    #[test]
    fn ts_types_qualify_user_types_from_the_root() {
        let m = model();
        assert_eq!(ts_type(&m, &Ty::Record("Entry".into())), "kv.Entry");
        assert_eq!(
            ts_type(
                &m,
                &Ty::List(Box::new(Ty::Optional(Box::new(Ty::Prim(Prim::I64)))))
            ),
            "(bigint | null)[]"
        );
        assert_eq!(
            ts_type(
                &m,
                &Ty::Map(
                    Box::new(Ty::Enum("Kind".into())),
                    Box::new(Ty::Prim(Prim::Bool))
                )
            ),
            "Partial<Record<kv.Kind, boolean>>"
        );
        assert_eq!(
            ts_type(
                &m,
                &Ty::Map(
                    Box::new(Ty::Prim(Prim::U64)),
                    Box::new(Ty::Prim(Prim::Bytes))
                )
            ),
            "Record<string, Uint8Array>"
        );
        assert_eq!(
            ts_slice_input(Prim::U64),
            "readonly bigint[] | BigUint64Array"
        );
        assert_eq!(scalar_kind(&Ty::Enum("Kind".into())), "I32");
    }

    #[test]
    fn doc_identifiers_take_their_javascript_spelling() {
        let m = model();
        let names = ApiNames::new(&m);
        assert_eq!(
            rewrite_doc(
                &names,
                "Use `open_store` with `ttl_seconds`, or catch `KvErrors` (`KEY_NOT_FOUND`) from `Store.get`; see `Entry`."
            ),
            "Use `openStore` with `ttlSeconds`, or catch `KvError` (`KeyNotFoundError`) from `Store.get`; see `Entry`."
        );
    }

    #[test]
    fn string_literals_escape_quotes_and_controls() {
        assert_eq!(js_string("it's\n\u{1}"), "'it\\'s\\n\\x01'");
    }
}
