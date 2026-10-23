//! Go identifiers: case conversion with Go's initialisms, keyword escaping,
//! and the names the flat package gives every declaration.
//!
//! The whole module tree renders into one Go package, so every exported
//! name is computed here once ([`GoNames`]) and looked up by the renderers:
//! types keep their IDL name (`Store`), constructors become factories
//! (`OpenStore`), statics are prefixed with their type (`StoreOpenMany`),
//! error domains and codes take the shared error type names (`KvError`,
//! `*KeyNotFoundError`), and a free function whose name would clash with
//! any of those keeps its module path (`KvOpenStore`).

use heck::ToSnakeCase;
use std::collections::{BTreeMap, BTreeSet};
use weaveffi_model::errors::type_name;
use weaveffi_model::model::{ErrorBinding, ErrorCodeBinding, FnBinding, Model};

use crate::lang;

/// The initialisms Go spells in one case (`UserID`, `ParseURL`,
/// `HTTPServer`), from the standard Go style guide's list.
const INITIALISMS: &[&str] = &[
    "ACL", "API", "ASCII", "CPU", "CSS", "DNS", "EOF", "GUID", "HTML", "HTTP", "HTTPS", "ID", "IP",
    "JSON", "LHS", "QPS", "RAM", "RHS", "RPC", "SLA", "SMTP", "SQL", "SSH", "TCP", "TLS", "TTL",
    "UDP", "UI", "UID", "URI", "URL", "UTF8", "UUID", "VM", "XML", "XMPP", "XSRF", "XSS",
];

/// Exported names the runtime files declare.
const RUNTIME_NAMES: &[&str] = &["Check", "DebugLive", "Error"];

/// Names a wrapper or trampoline body uses besides its parameters: the
/// packages it calls into, the receiver, and its locals. A parameter or
/// slot spelled like one of them gains a trailing underscore.
const RESERVED: &[&str] = &[
    "C", "cErr", "cOut", "cRet", "cRetLen", "cSelf", "call", "context", "err", "iter", "it", "ret",
    "runtime", "token", "unsafe",
];

/// The words of an identifier in any IDL spelling (`snake_case`,
/// `PascalCase`, `SCREAMING_CASE`), lowercased.
fn words(name: &str) -> Vec<String> {
    name.to_snake_case()
        .split('_')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// One word in exported position: an initialism in capitals, anything else
/// capitalized.
fn capitalize(word: &str) -> String {
    let upper = word.to_ascii_uppercase();
    if INITIALISMS.contains(&upper.as_str()) {
        return upper;
    }
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The exported (PascalCase) Go spelling of an IDL name, with Go's
/// initialisms: `user_id` is `UserID`, `ttl_seconds` is `TTLSeconds`.
pub(crate) fn pascal(name: &str) -> String {
    words(name).iter().map(|w| capitalize(w)).collect()
}

/// The unexported (lowerCamelCase) Go spelling of an IDL name, with Go's
/// initialisms after the first word: `user_id` is `userID`, `url` is `url`.
pub(crate) fn camel(name: &str) -> String {
    let mut out = String::new();
    for (i, w) in words(name).iter().enumerate() {
        if i == 0 {
            out.push_str(w);
        } else {
            out.push_str(&capitalize(w));
        }
    }
    out
}

/// The default Go package name for a C prefix: the prefix without its
/// underscores (`kitchen_sink` is `kitchensink`), lowercased, and escaped
/// if it's a Go keyword.
pub(crate) fn package(prefix: &str) -> String {
    let name: String = prefix
        .chars()
        .filter(|c| *c != '_')
        .flat_map(char::to_lowercase)
        .collect();
    lang::escape_ident(&name, lang::GO_KEYWORDS)
}

/// Methods every object wrapper declares: `Close` releases the wrapper's
/// reference (its unexported helpers can't collide with an exported name).
const OBJECT_METHODS: &[&str] = &["Close"];

/// The Go name of an interface method: [`pascal`], with a trailing
/// underscore when it would redeclare a method the wrapper already has
/// (`close` is `Close_`). Constructors and statics are package functions
/// prefixed or suffixed with the type name, so they can't collide.
pub(crate) fn method(name: &str) -> String {
    lang::escape_member(&pascal(name), OBJECT_METHODS)
}

/// The Go spelling of a parameter of the wrapper for `f`: lowerCamelCase,
/// escaped against Go's keywords and the names wrapper bodies use. A
/// method's receiver is `s`, and an async wrapper's leading
/// `context.Context` is `ctx`, so a parameter spelled like either is
/// escaped there too.
pub(crate) fn param(name: &str, f: &FnBinding) -> String {
    let ident = lang::escape_ident(&camel(name), lang::GO_KEYWORDS);
    let taken = RESERVED.contains(&ident.as_str())
        || (f.has_self() && ident == "s")
        || (f.is_async() && ident == "ctx");
    if taken {
        format!("{ident}_")
    } else {
        ident
    }
}

/// The Go spelling of a callback method's parameter in the interface the
/// consumer implements: lowerCamelCase, escaped against Go's keywords (a
/// method signature has no body to collide with).
pub(crate) fn method_param(name: &str) -> String {
    lang::escape_ident(&camel(name), lang::GO_KEYWORDS)
}

/// The Go spelling of one C ABI slot inside an exported trampoline: the C
/// name, escaped against Go's keywords and the names bodies use. The
/// preamble `extern` uses the same spelling so the two prototypes agree.
pub(crate) fn slot(name: &str) -> String {
    let ident = lang::escape_ident(name, lang::GO_KEYWORDS);
    if RESERVED.contains(&ident.as_str()) {
        format!("{ident}_")
    } else {
        ident
    }
}

/// The exported Go name of a struct field or variant field.
pub(crate) fn field(name: &str) -> String {
    pascal(name)
}

/// The Go name of an error code's payload field: [`field`], with a
/// trailing underscore when it would collide with the error type's
/// `Message` field or its `Code` and `Error` methods.
pub(crate) fn error_field(name: &str) -> String {
    let f = field(name);
    if matches!(f.as_str(), "Message" | "Code" | "Error") {
        format!("{f}_")
    } else {
        f
    }
}

/// The Go names of one error domain.
#[derive(Debug, Clone)]
pub(crate) struct DomainNames {
    /// The sealed interface every code type implements (`KvError`): the
    /// shared [`type_name`] in Go's casing.
    pub(crate) iface: String,
    /// The concrete type of a code these bindings don't declare
    /// (`UnknownKvError`).
    pub(crate) unknown: String,
    /// The unexported helper mapping a failure onto the domain
    /// (`wvKvError`).
    pub(crate) mapper: String,
}

/// Every exported name the generated package declares for the API, keyed by
/// the declaration it names.
pub(crate) struct GoNames {
    /// Every callable's Go name by C symbol: a free function's package
    /// function, a constructor's factory, a static's prefixed function, and
    /// a method's `Type.Method` (the spelling a doc link uses).
    callables: BTreeMap<String, String>,
    /// Error domains by IDL name.
    domains: BTreeMap<String, DomainNames>,
    /// Error-code types by C constant.
    codes: BTreeMap<String, String>,
}

impl GoNames {
    /// Name every declaration of `model`, resolving clashes in the flat
    /// package (see the module docs).
    pub(crate) fn new(model: &Model) -> Self {
        // Names the runtime declares, then every type-level name.
        let mut taken: BTreeSet<String> = RUNTIME_NAMES.iter().map(|s| s.to_string()).collect();
        let mut domains = BTreeMap::new();
        for (_, e) in model.error_domains() {
            let iface = pascal(&type_name(&e.name, "Error"));
            let names = DomainNames {
                unknown: format!("Unknown{iface}"),
                mapper: format!("wv{iface}"),
                iface,
            };
            taken.insert(names.iface.clone());
            taken.insert(names.unknown.clone());
            domains.insert(e.name.clone(), names);
        }
        let mut callables = BTreeMap::new();
        for m in &model.modules {
            for e in &m.enums {
                let name = pascal(&e.name);
                for v in &e.variants {
                    taken.insert(format!("{name}{}", pascal(&v.name)));
                }
                taken.insert(name);
            }
            for s in &m.structs {
                taken.insert(pascal(&s.name));
            }
            for cb in &m.callback_interfaces {
                taken.insert(pascal(&cb.name));
            }
            for i in &m.interfaces {
                let name = pascal(&i.name);
                for c in &i.constructors {
                    let factory = constructor(&name, c);
                    taken.insert(factory.clone());
                    callables.insert(c.abi.symbol.clone(), factory);
                }
                for f in &i.statics {
                    let go = format!("{name}{}", pascal(&f.name));
                    taken.insert(go.clone());
                    callables.insert(f.abi.symbol.clone(), go);
                }
                for f in &i.methods {
                    callables.insert(f.abi.symbol.clone(), format!("{name}.{}", method(&f.name)));
                }
                taken.insert(name);
            }
        }

        // Error codes: `{Code}Error`, or `{Domain}{Code}Error` past a clash.
        let mut codes = BTreeMap::new();
        for (_, e) in model.error_domains() {
            let domain = &domains[&e.name].iface;
            for c in &e.codes {
                let plain = pascal(&type_name(&c.name, "Error"));
                let name = if taken.contains(&plain) {
                    pascal(&type_name(
                        &format!("{}{}", domain.trim_end_matches("Error"), pascal(&c.name)),
                        "Error",
                    ))
                } else {
                    plain
                };
                taken.insert(name.clone());
                codes.insert(c.c_const.clone(), name);
            }
        }

        // Free functions: the bare name unless it clashes with a declared
        // name or another function's.
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();
        for (_, f) in model.functions() {
            *seen.entry(pascal(&f.name)).or_default() += 1;
        }
        for (m, f) in model.functions() {
            let bare = pascal(&f.name);
            let name = if taken.contains(&bare) || seen[&bare] > 1 {
                pascal(&format!("{}_{}", m.path, f.name))
            } else {
                bare
            };
            callables.insert(f.abi.symbol.clone(), name);
        }
        Self {
            callables,
            domains,
            codes,
        }
    }

    /// The Go name of the free function, constructor, or static `f`.
    pub(crate) fn function(&self, f: &FnBinding) -> &str {
        &self.callables[&f.abi.symbol]
    }

    /// The doc-link spelling of the callable whose C symbol is `symbol`
    /// (`NewOp`, `NewGadget`, `Gadget.Describe`).
    pub(crate) fn callable(&self, symbol: &str) -> Option<&str> {
        self.callables.get(symbol).map(String::as_str)
    }

    /// The Go names of the error domain `name`.
    pub(crate) fn domain(&self, name: &str) -> &DomainNames {
        &self.domains[name]
    }

    /// The Go names of the error domain `e`.
    pub(crate) fn domain_of(&self, e: &ErrorBinding) -> &DomainNames {
        self.domain(&e.name)
    }

    /// The Go type name of the error code `c`.
    pub(crate) fn code(&self, c: &ErrorCodeBinding) -> &str {
        &self.codes[&c.c_const]
    }

    /// The Go type name of the error code whose C constant is `c_const`.
    pub(crate) fn code_by_const(&self, c_const: &str) -> Option<&str> {
        self.codes.get(c_const).map(String::as_str)
    }
}

/// The factory name of the constructor `c` on the interface `iface` (Go
/// spelling): `new` on `Store` is `NewStore`, `open` is `OpenStore`.
pub(crate) fn constructor(iface: &str, c: &FnBinding) -> String {
    format!("{}{iface}", pascal(&c.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;

    #[test]
    fn initialisms_keep_one_case() {
        assert_eq!(pascal("user_id"), "UserID");
        assert_eq!(pascal("ttl_seconds"), "TTLSeconds");
        assert_eq!(pascal("http_url"), "HTTPURL");
        assert_eq!(pascal("UserId"), "UserID");
        assert_eq!(pascal("KEY_NOT_FOUND"), "KeyNotFound");
        assert_eq!(pascal("MaybeI64"), "MaybeI64");
        assert_eq!(pascal("i8_value"), "I8Value");
        assert_eq!(camel("user_id"), "userID");
        assert_eq!(camel("id"), "id");
        assert_eq!(camel("url_path"), "urlPath");
        assert_eq!(camel("ttl_seconds"), "ttlSeconds");
    }

    #[test]
    fn the_default_package_drops_underscores() {
        assert_eq!(package("kitchen_sink"), "kitchensink");
        assert_eq!(package("kv"), "kv");
        assert_eq!(package("my_Lib"), "mylib");
        assert_eq!(package("go_"), "go_");
        assert_eq!(package("ra_nge"), "range_");
    }

    #[test]
    fn parameters_avoid_keywords_and_body_names() {
        let model = test_model(
            r#"
version: "0.12.0"
modules:
  - name: m
    interfaces:
      - name: Box
        methods:
          - { name: put, params: [{ name: s, type: i32 }, { name: range, type: i32 }] }
    functions:
      - { name: plain, params: [{ name: s, type: i32 }, { name: ctx, type: i32 }] }
      - { name: later, params: [{ name: ctx, type: i32 }], return: i32, async: true }
"#,
        );
        let m = &model.modules[0];
        let (put, plain, later) = (
            &m.interfaces[0].methods[0],
            &m.functions[0],
            &m.functions[1],
        );
        assert_eq!(param("s", put), "s_");
        assert_eq!(param("range", put), "range_");
        assert_eq!(param("s", plain), "s");
        assert_eq!(param("ctx", plain), "ctx");
        assert_eq!(param("ctx", later), "ctx_");
        assert_eq!(param("context", plain), "context_");
        assert_eq!(method_param("chan"), "chan_");
        assert_eq!(method_param("item"), "item");
        assert_eq!(slot("err"), "err_");
        assert_eq!(error_field("message"), "Message_");
        assert_eq!(error_field("key"), "Key");
    }

    #[test]
    fn methods_avoid_the_wrapper_close() {
        assert_eq!(method("close"), "Close_");
        assert_eq!(method("Close"), "Close_");
        assert_eq!(method("close_all"), "CloseAll");
        assert_eq!(method("dispose"), "Dispose");
    }

    #[test]
    fn clashing_names_gain_a_qualifier() {
        let model = test_model(
            r#"
version: "0.12.0"
modules:
  - name: kv
    errors:
      - name: KvErrors
        codes:
          - { name: Store, code: 1, message: store }
          - { name: user_id_missing, code: 2, message: missing }
    interfaces:
      - name: Store
        constructors: [{ name: open, params: [] }]
        methods: [{ name: close, params: [] }]
    functions:
      - { name: open_store, params: [], return: Store }
      - { name: check, params: [] }
"#,
        );
        let names = GoNames::new(&model);
        let d = names.domain("KvErrors");
        assert_eq!(
            (d.iface.as_str(), d.unknown.as_str(), d.mapper.as_str()),
            ("KvError", "UnknownKvError", "wvKvError")
        );
        let e = &model.modules[0].errors[0];
        assert_eq!(names.code(&e.codes[0]), "StoreError");
        assert_eq!(names.code(&e.codes[1]), "UserIDMissingError");
        let f = &model.modules[0].functions;
        assert_eq!(names.function(&f[0]), "KvOpenStore");
        assert_eq!(names.function(&f[1]), "KvCheck");
        assert_eq!(names.callable("kv_kv_Store_close"), Some("Store.Close_"));
        assert_eq!(names.callable("kv_kv_Store_open"), Some("OpenStore"));
    }
}
