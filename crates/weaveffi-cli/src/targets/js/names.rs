//! JavaScript and TypeScript spellings of IDL names and types.
//!
//! Generated code keeps three kinds of names apart so none can collide:
//!
//! * **Public names** are what consumers import: `Store`, `openStore`,
//!   `KeyNotFoundError`. Identifiers that are reserved words gain a trailing
//!   underscore.
//! * **Declaration names** are the module-scope bindings of user
//!   declarations, the public name plus `$` (`Store$`, `openStore$`), which
//!   `index.js` exports under the public name. IDL identifiers never contain
//!   `$`, so no declaration can shadow a JavaScript global the generated
//!   code uses (`Map`, `Promise`, `TypeError`).
//! * **Helper names** start with `$` (`$lend`, `$w$Entry`): runtime imports
//!   and generated support code, which no IDL name can reach.

use std::collections::{BTreeMap, BTreeSet};

use heck::ToLowerCamelCase;
use weaveffi_model::errors::type_name;
use weaveffi_model::model::{ErrorStrategy, Model};
use weaveffi_model::ty::{Prim, Ty};

use crate::codegen::docs::{self, ApiNames, IdentKind};
use crate::lang;

/// Identifiers that are not JS keywords but can't name a binding or
/// parameter in strict-mode (module) code.
const STRICT_RESERVED: &[&str] = &["arguments", "eval"];

/// Public names the package itself exports beside the API's: WebAssembly's
/// `init`. Both targets reserve them, so their APIs stay the same.
const PACKAGE_EXPORTS: &[&str] = &["init"];

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
    let camel = name.to_lower_camel_case();
    if PACKAGE_EXPORTS.contains(&camel.as_str()) {
        format!("{camel}_")
    } else {
        js_ident(&camel)
    }
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

/// The declaration name of the public name `name` (`Store` becomes
/// `Store$`).
pub(crate) fn decl(name: &str) -> String {
    format!("{name}$")
}

/// A helper name derived from a user type or error domain (`$w` and
/// `Entry` give `$w$Entry`).
pub(crate) fn helper(kind: &str, name: &str) -> String {
    format!("${kind}${name}")
}

/// The class names of a package's errors: the root class, one class per
/// error domain, and one per code, each unique among the package's exports.
///
/// A domain's class is [`type_name`] of its name and a code's of its code
/// (`KvError`, `KeyNotFoundError`). A code whose class would take a name
/// already exported prefixes its domain's (`KvKvError` for a code `Kv` of
/// `KvError`). The root class is the package name's (`KvstoreError`), or,
/// when that's a domain's or type's name, the package name plus `Library`
/// (`CodecLibraryError` beside a `CodecError` domain).
#[derive(Debug, Clone)]
pub(crate) struct ErrorNames {
    /// The root error class every error extends.
    pub(crate) root: String,
    domains: BTreeMap<String, String>,
    codes: BTreeMap<String, String>,
}

impl ErrorNames {
    /// The error class names of `model`.
    #[must_use]
    pub(crate) fn new(model: &Model) -> Self {
        let mut taken: BTreeSet<String> = model
            .records
            .iter()
            .map(|r| r.name.clone())
            .chain(model.enums.iter().map(|e| e.name.clone()))
            .chain(model.interfaces.iter().map(|i| i.name.clone()))
            .chain(model.callbacks.iter().map(|c| c.name.clone()))
            .chain(["CancelledError".to_string(), "NativeIterator".to_string()])
            .collect();
        let mut domains = BTreeMap::new();
        for d in &model.errors {
            let name = type_name(&d.name, "Error");
            taken.insert(name.clone());
            domains.insert(d.name.clone(), name);
        }
        let mut codes = BTreeMap::new();
        for d in &model.errors {
            let base = domains[&d.name].trim_end_matches("Error").to_string();
            for c in &d.codes {
                let mut name = type_name(&c.name, "Error");
                if taken.contains(&name) {
                    name = type_name(
                        &format!("{base}{}", weaveffi_model::errors::pascal(&c.name)),
                        "Error",
                    );
                }
                taken.insert(name.clone());
                codes.insert(c.name.clone(), name);
            }
        }
        let pascal = model.identity.pascal_name();
        let mut root = type_name(&pascal, "Error");
        if taken.contains(&root) {
            root = type_name(&format!("{pascal}Library"), "Error");
        }
        Self {
            root,
            domains,
            codes,
        }
    }

    /// The class of error domain `name`.
    #[must_use]
    pub(crate) fn domain(&self, name: &str) -> &str {
        &self.domains[name]
    }

    /// The class of error code `name` (code names are unique across a
    /// library).
    #[must_use]
    pub(crate) fn code(&self, name: &str) -> &str {
        &self.codes[name]
    }
}

/// The function that maps a native failure of a callable with error
/// strategy `error` onto the error classes: its domain's mapper
/// (`$from$KvError`) when it throws a domain, the runtime's `$fault`
/// otherwise (a trap, or an untyped failure, is the root error class).
pub(crate) fn error_mapper(error: &ErrorStrategy) -> String {
    match error.domain() {
        Some(domain) => helper("from", domain),
        None => "$fault".to_string(),
    }
}

/// The TypeScript type of `ty`.
pub(crate) fn ts_type(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => ts_type_prim(*p).to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => n.clone(),
        Ty::Optional(inner) => format!("{} | null", ts_type(inner)),
        Ty::List(inner) => {
            let t = ts_type(inner);
            if t.contains(' ') {
                format!("({t})[]")
            } else {
                format!("{t}[]")
            }
        }
        Ty::Map(k, v) => {
            let value = ts_type(v);
            match k.as_ref() {
                Ty::Enum(e) => format!("Partial<Record<{e}, {value}>>"),
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
    format!("readonly {}[] | {}", ts_type_prim(elem), typed_array(elem))
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

/// The spelling closure doc and deprecation text is rewritten with: an
/// identifier in backticks becomes its JavaScript name (`new_op` becomes
/// `newOp`, an error domain or code its class), and each segment of a
/// dotted path (`Store.get`) is spelled on its own.
pub(crate) fn doc_spelling<'a>(
    names: &'a ApiNames,
    errors: &'a ErrorNames,
) -> impl Fn(&str) -> Option<String> + 'a {
    let one = move |ident: &str| -> Option<String> {
        Some(match names.kind(ident)? {
            IdentKind::Function => fn_name(ident),
            IdentKind::Member | IdentKind::CallbackMethod | IdentKind::Param => {
                ident.to_lower_camel_case()
            }
            IdentKind::ErrorDomain => errors.domain(ident).to_string(),
            IdentKind::ErrorCode => errors.code(ident).to_string(),
            IdentKind::Type | IdentKind::Variant | IdentKind::Field => return None,
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
pub(crate) fn rewrite_doc(names: &ApiNames, errors: &ErrorNames, text: &str) -> String {
    docs::rewrite(text, doc_spelling(names, errors))
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
    use weaveffi_model::pkg::Identity;

    #[test]
    fn reserved_words_gain_an_underscore() {
        assert_eq!(js_ident("class"), "class_");
        assert_eq!(js_ident("eval"), "eval_");
        assert_eq!(js_ident("store"), "store");
        assert_eq!(fn_name("delete"), "delete_");
        assert_eq!(fn_name("open_store"), "openStore");
        assert_eq!(fn_name("init"), "init_");
        assert_eq!(param_name("import"), "import_");
        assert_eq!(member_name("close", false), "close_");
        assert_eq!(member_name("name", true), "name_");
        assert_eq!(member_name("name", false), "name");
        assert_eq!(decl("Store"), "Store$");
        assert_eq!(helper("w", "Entry"), "$w$Entry");
    }

    /// `Entry`, `Kind`, `Store`, and the `KvErrors` domain.
    pub(crate) fn model() -> Model {
        crate::codegen::test_model(
            r#"
version: "0.13.0"
enums: [{ name: Kind, variants: [{ name: A, value: 0 }] }]
records: [{ name: Entry, fields: [{ name: k, type: i32 }] }]
interfaces: [{ name: Store, methods: [{ name: get }] }]
errors:
  - name: KvErrors
    codes: [{ name: KEY_NOT_FOUND, code: 1, message: missing }, { name: Kv, code: 2, message: kv }]
functions:
  - { name: open_store, params: [{ name: ttl_seconds, type: i64 }] }
"#,
        )
    }

    #[test]
    fn error_classes_are_unique_exports() {
        let m = model();
        let names = ErrorNames::new(&m);
        assert_eq!(names.domain("KvErrors"), "KvError");
        assert_eq!(names.code("KEY_NOT_FOUND"), "KeyNotFoundError");
        assert_eq!(names.code("Kv"), "KvKvError");
        // The package `kv`'s own name is the domain's, so the root takes
        // the `Library` spelling.
        assert_eq!(names.root, "KvLibraryError");
        let mut codec = crate::codegen::test_model(
            r#"
version: "0.13.0"
errors: [{ name: CodecError, codes: [{ name: Bad, code: 1, message: bad }] }]
"#,
        );
        codec.identity = Identity::named("codec");
        assert_eq!(ErrorNames::new(&codec).root, "CodecLibraryError");
        codec.identity = Identity::named("my-kv");
        assert_eq!(ErrorNames::new(&codec).root, "MyKvError");
    }

    #[test]
    fn ts_types_name_user_types_directly() {
        assert_eq!(ts_type(&Ty::Record("Entry".into())), "Entry");
        assert_eq!(
            ts_type(&Ty::List(Box::new(Ty::Optional(Box::new(Ty::Prim(
                Prim::I64
            )))))),
            "(bigint | null)[]"
        );
        assert_eq!(
            ts_type(&Ty::Map(
                Box::new(Ty::Enum("Kind".into())),
                Box::new(Ty::Prim(Prim::Bool))
            )),
            "Partial<Record<Kind, boolean>>"
        );
        assert_eq!(
            ts_type(&Ty::Map(
                Box::new(Ty::Prim(Prim::U64)),
                Box::new(Ty::Prim(Prim::Bytes))
            )),
            "Record<string, Uint8Array>"
        );
        assert_eq!(
            ts_slice_input(Prim::U64),
            "readonly bigint[] | BigUint64Array"
        );
    }

    #[test]
    fn doc_identifiers_take_their_javascript_spelling() {
        let m = model();
        let names = ApiNames::new(&m);
        let errors = ErrorNames::new(&m);
        assert_eq!(
            rewrite_doc(
                &names,
                &errors,
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
