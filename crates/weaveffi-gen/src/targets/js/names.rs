//! JavaScript and TypeScript spellings of IDL names and types.
//!
//! Generated code keeps three kinds of names apart so none can collide:
//!
//! * **Public names** are what consumers write: `kv.Store`, `kv.openStore`.
//!   Identifiers that are reserved words gain a trailing underscore.
//! * **Declaration names** are the module-scope bindings of user
//!   declarations, the dotted path joined with `$` (`kv$Store`,
//!   `kv$stats$getStats`). IDL identifiers never contain `$`, so two modules
//!   declaring the same name never clash, and no declaration can shadow a
//!   JavaScript global.
//! * **Helper names** start with `$` (`$lend`, `$w$kv$Entry`): runtime
//!   imports and generated support code, which no IDL name can reach.

use heck::ToLowerCamelCase;
use weaveffi_model::model::{ErrorBinding, FnBinding, ModuleBinding, Prim, Ty, WireType};

use crate::lang;
use crate::utils::local_type_name;

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

/// The name of an interface member. Instance members can't be named
/// `constructor` or `close` (the wrapper's own release method), and statics
/// can't replace the class's `name`, `length`, or `prototype`; those gain a
/// trailing underscore.
pub(crate) fn member_name(name: &str, is_static: bool) -> String {
    let camel = name.to_lower_camel_case();
    let clash = if is_static {
        matches!(camel.as_str(), "name" | "length" | "prototype" | "caller")
    } else {
        matches!(camel.as_str(), "constructor" | "close")
    };
    if clash {
        format!("{camel}_")
    } else {
        camel
    }
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

/// The declaration name of a user type from its absolute dotted path
/// (`kv.Store` becomes `kv$Store`).
pub(crate) fn type_decl(dotted: &str) -> String {
    dotted.replace('.', "$")
}

/// A helper name derived from a user type (`$w` and `kv.Entry` give
/// `$w$kv$Entry`).
pub(crate) fn helper(kind: &str, dotted: &str) -> String {
    format!("${kind}${}", type_decl(dotted))
}

/// The public name of a module (its namespace object).
pub(crate) fn module_name(m: &ModuleBinding) -> String {
    js_ident(&m.name)
}

/// The TypeScript path of a user type from its absolute dotted path, each
/// module segment escaped like the namespace it names (`kv.Store`).
pub(crate) fn ts_path(dotted: &str) -> String {
    let local = local_type_name(dotted);
    match dotted.rsplit_once('.') {
        Some((modules, _)) => {
            let mut out: Vec<String> = modules.split('.').map(js_ident).collect();
            out.push(local.to_string());
            out.join(".")
        }
        None => local.to_string(),
    }
}

/// The error class name of an error code (`KEY_NOT_FOUND` becomes
/// `KeyNotFoundError`).
pub(crate) fn code_class(code: &str) -> String {
    weaveffi_model::errors::type_name(code, "Error")
}

/// The module that declares error domain `eb`.
pub(crate) fn error_owner<'a>(
    modules: &'a [ModuleBinding],
    eb: &ErrorBinding,
) -> &'a ModuleBinding {
    modules
        .iter()
        .find(|m| m.path == eb.owner_path && m.declares_error())
        .expect("an error binding's owner module declares the domain")
}

/// The function that maps a native failure of callable `f` (declared in a
/// module whose domain is `error`) onto the error classes: the domain's
/// mapper for a throwing callable, the runtime's `$fault` otherwise.
pub(crate) fn error_mapper(
    modules: &[ModuleBinding],
    f: &FnBinding,
    error: Option<&ErrorBinding>,
) -> String {
    match error {
        Some(eb) if f.throws => {
            let owner = error_owner(modules, eb);
            format!("$from${}", decl(&owner.segments, &eb.name))
        }
        _ => "$fault".to_string(),
    }
}

/// The TypeScript type of `ty`.
pub(crate) fn ts_type(ty: &Ty) -> String {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::F32 | Ty::F64 => {
            "number".into()
        }
        Ty::I64 | Ty::U64 => "bigint".into(),
        Ty::Bool => "boolean".into(),
        Ty::StringUtf8 => "string".into(),
        Ty::Bytes => "Uint8Array".into(),
        Ty::Record(n)
        | Ty::RichEnum(n)
        | Ty::Enum(n)
        | Ty::Interface(n)
        | Ty::CallbackInterface(n) => ts_path(n),
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
                Ty::Enum(e) => format!("Partial<Record<{}, {value}>>", ts_path(e)),
                Ty::I8 | Ty::I16 | Ty::I32 | Ty::U8 | Ty::U16 | Ty::U32 => {
                    format!("Record<number, {value}>")
                }
                _ => format!("Record<string, {value}>"),
            }
        }
        Ty::Iterator(inner) => format!("IterableIterator<{}>", ts_type(inner)),
    }
}

/// The wire primitive a direct-family type is read and written as (C-style
/// enums travel as their `i32` discriminant).
///
/// # Panics
///
/// Panics on a type outside the direct family.
pub(crate) fn direct_prim(ty: &Ty) -> Prim {
    match ty.wire() {
        WireType::Prim(p) => p,
        WireType::Enum(_) => Prim::I32,
        other => unreachable!("not a direct type: {other:?}"),
    }
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
mod tests {
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

    #[test]
    fn declaration_names_join_the_path_with_dollars() {
        let segs = vec!["a".to_string(), "b".to_string()];
        assert_eq!(decl(&segs, "C"), "a$b$C");
        assert_eq!(type_decl("a.b.C"), "a$b$C");
        assert_eq!(helper("w", "kv.Entry"), "$w$kv$Entry");
        assert_eq!(ts_path("class.inner.Widget"), "class_.inner.Widget");
    }

    #[test]
    fn ts_types_qualify_user_types_from_the_root() {
        assert_eq!(ts_type(&Ty::Record("kv.Entry".into())), "kv.Entry");
        assert_eq!(
            ts_type(&Ty::List(Box::new(Ty::Optional(Box::new(Ty::I64))))),
            "(bigint | null)[]"
        );
        assert_eq!(
            ts_type(&Ty::Map(
                Box::new(Ty::Enum("kv.Kind".into())),
                Box::new(Ty::Bool)
            )),
            "Partial<Record<kv.Kind, boolean>>"
        );
        assert_eq!(
            ts_type(&Ty::Map(Box::new(Ty::U64), Box::new(Ty::Bytes))),
            "Record<string, Uint8Array>"
        );
    }

    #[test]
    fn string_literals_escape_quotes_and_controls() {
        assert_eq!(js_string("it's\n\u{1}"), "'it\\'s\\n\\x01'");
    }
}
