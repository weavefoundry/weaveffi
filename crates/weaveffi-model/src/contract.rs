//! The contract: one fingerprint per declaration, which a generated binding
//! checks against the library it loads before the first call.
//!
//! Every declaration has an entry: each function, interface, interface
//! member, record, enum, callback interface, callback method, error domain,
//! and error code. An entry has:
//!
//! * an **id**, [`fnv1a64`] of the declaration's dotted path (`open`,
//!   `Store.get`, `Listener.on_put`, `KvError.NotFound`);
//! * a **hash**, [`fnv1a64`] of its canonical signature string.
//!
//! A producer exports one function per entry, `uint64_t
//! {prefix}_contract_{id:016x}(void)` ([`symbol`]), returning the hash. A
//! binding resolves the function of every entry it was generated with and
//! compares the hash: a missing function means the library lacks the
//! declaration, and a different hash means it changed. Entries the library
//! has and the binding doesn't are fine, so adding a declaration (a
//! function, a method, a record, an error code, or a callback method at the
//! end of its interface) never breaks a deployed binding.
//!
//! The canonical signature is computed from resolved types: aliases are
//! substituted, `[u8]` is `bytes`, and each type is written in its IDL
//! spelling by [`canonical`] (not by `Display`, so a cosmetic change to
//! diagnostics can't change a hash). Parameter and field names, docs,
//! deprecation text, and error messages are excluded.
//!
//! | Declaration | Signature |
//! |---|---|
//! | function or interface member | `{kind} {name}({type}, ...) -> {type or void}`, then ` throws {Domain}` or ` throws any`, ` async`, ` cancellable` when set; `kind` is `function`, `constructor`, `method`, or `static` |
//! | interface | `interface {name}` |
//! | record | `record {name} {{type}, ...}` |
//! | enum | `enum {name} {{Variant} = {value}, ...}`, a variant with fields followed by ` {{type}, ...}` |
//! | callback interface | `callback {name}` |
//! | callback method | `callback_method #{slot} {name}({type}, ...) -> {type or void}`, then ` throws {Domain}` or ` throws any` when set |
//! | error domain | `errors {name}` |
//! | error code | `code {Code} = {value}`, a code with fields followed by ` {{type}, ...}` |
//!
//! [`Sig`] builds these strings from parts, so the producer macro, which
//! knows each type's spelling only at compile time, assembles exactly the
//! strings the CLI does.

use crate::model::{ErrorStrategy, FieldBinding, FnBinding, Model};
use crate::ty::{ParamTy, RetTy, Ty};

/// One declaration's contract entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractEntry {
    /// The declaration's dotted path (`Store.put`), which bindings name in
    /// their load errors.
    pub path: String,
    /// [`fnv1a64`] of [`path`](Self::path).
    pub id: u64,
    /// [`fnv1a64`] of [`signature`](Self::signature).
    pub hash: u64,
    /// The canonical signature (see the [module docs](self)).
    pub signature: String,
    /// The producer's function returning the hash, [`symbol`].
    pub symbol: String,
}

/// The 64-bit FNV-1a hash of `data`: tiny, dependency-free, and stable
/// across platforms and Rust versions.
#[must_use]
pub const fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < data.len() {
        hash ^= data[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// The producer's contract function for the declaration `path`:
/// `{prefix}_contract_{id:016x}`, the id in lower-case hex.
#[must_use]
pub fn symbol(prefix: &str, path: &str) -> String {
    format!("{prefix}_contract_{:016x}", fnv1a64(path.as_bytes()))
}

/// The canonical IDL spelling of a value type (`i32`, `[Entry?]`,
/// `{string:Kind}`).
#[must_use]
pub fn canonical(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => p.snake().to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => n.clone(),
        Ty::Optional(inner) => format!("{}?", canonical(inner)),
        Ty::List(inner) => format!("[{}]", canonical(inner)),
        Ty::Map(k, v) => format!("{{{}:{}}}", canonical(k), canonical(v)),
    }
}

fn canonical_param(ty: &ParamTy) -> String {
    match ty {
        ParamTy::Value(t) => canonical(t),
        ParamTy::Callback { name, nullable } => {
            if *nullable {
                format!("{name}?")
            } else {
                name.clone()
            }
        }
    }
}

fn canonical_ret(ty: Option<&RetTy>) -> String {
    match ty {
        None => VOID.to_string(),
        Some(RetTy::Value(t)) => canonical(t),
        Some(RetTy::Iterator(t)) => format!("iter<{}>", canonical(t)),
    }
}

/// The spelling of a missing return.
pub const VOID: &str = "void";

/// The ` throws ...` suffix of a strategy: empty, ` throws {Domain}`, or
/// ` throws any`.
#[must_use]
pub fn throws_suffix(error: &ErrorStrategy) -> String {
    match error {
        ErrorStrategy::Trap => String::new(),
        ErrorStrategy::Domain(d) => format!(" throws {d}"),
        ErrorStrategy::Untyped => " throws any".to_string(),
    }
}

/// One part of a canonical signature: literal text, or a slot whose text
/// the caller supplies (a type's spelling, a ` throws` suffix).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part<T> {
    /// Literal text.
    Lit(String),
    /// A caller-supplied piece.
    Slot(T),
}

/// A canonical signature as a sequence of [`Part`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sig<T>(pub Vec<Part<T>>);

impl<T> Sig<T> {
    fn new() -> Self {
        Sig(Vec::new())
    }

    fn lit(mut self, s: impl Into<String>) -> Self {
        let s = s.into();
        if let Some(Part::Lit(last)) = self.0.last_mut() {
            last.push_str(&s);
        } else {
            self.0.push(Part::Lit(s));
        }
        self
    }

    fn slot(mut self, t: T) -> Self {
        self.0.push(Part::Slot(t));
        self
    }

    fn list(mut self, items: Vec<T>) -> Self {
        for (i, t) in items.into_iter().enumerate() {
            if i > 0 {
                self = self.lit(", ");
            }
            self = self.slot(t);
        }
        self
    }

    /// `{kind} {name}({params}) -> {ret}{throws}[ async][ cancellable]`,
    /// where `ret` spells `void` for no return and `throws` is a
    /// [`throws_suffix`].
    #[must_use]
    pub fn callable(
        kind: &str,
        name: &str,
        params: Vec<T>,
        ret: T,
        throws: T,
        is_async: bool,
        cancellable: bool,
    ) -> Self {
        let mut sig = Self::new()
            .lit(format!("{kind} {name}("))
            .list(params)
            .lit(") -> ")
            .slot(ret)
            .slot(throws);
        if is_async {
            sig = sig.lit(" async");
        }
        if cancellable {
            sig = sig.lit(" cancellable");
        }
        sig
    }

    /// `callback_method #{slot} {name}({params}) -> {ret}{throws}`.
    #[must_use]
    pub fn callback_method(slot: usize, name: &str, params: Vec<T>, ret: T, throws: T) -> Self {
        Self::new()
            .lit(format!("callback_method #{slot} {name}("))
            .list(params)
            .lit(") -> ")
            .slot(ret)
            .slot(throws)
    }

    /// `record {name} {{fields}}`.
    #[must_use]
    pub fn record(name: &str, fields: Vec<T>) -> Self {
        Self::new()
            .lit(format!("record {name} {{"))
            .list(fields)
            .lit("}")
    }

    /// `enum {name} {{V = value[ {{types}}], ...}}`.
    #[must_use]
    pub fn enumeration(name: &str, variants: Vec<(String, i32, Vec<T>)>) -> Self {
        let mut sig = Self::new().lit(format!("enum {name} {{"));
        for (i, (variant, value, fields)) in variants.into_iter().enumerate() {
            if i > 0 {
                sig = sig.lit(", ");
            }
            sig = sig.case(&variant, value, fields);
        }
        sig.lit("}")
    }

    /// `code {name} = {value}[ {{types}}]`.
    #[must_use]
    pub fn code(name: &str, value: i32, fields: Vec<T>) -> Self {
        Self::new().lit("code ").case(name, value, fields)
    }

    fn case(self, name: &str, value: i32, fields: Vec<T>) -> Self {
        let sig = self.lit(format!("{name} = {value}"));
        if fields.is_empty() {
            sig
        } else {
            sig.lit(" {").list(fields).lit("}")
        }
    }

    /// A signature that is all literal text (`interface Store`).
    #[must_use]
    pub fn text(s: impl Into<String>) -> Self {
        Self::new().lit(s)
    }
}

impl Sig<String> {
    /// The signature's text.
    #[must_use]
    pub fn render(&self) -> String {
        self.0
            .iter()
            .map(|p| match p {
                Part::Lit(s) | Part::Slot(s) => s.as_str(),
            })
            .collect()
    }
}

/// Every contract entry of `model`, sorted ascending by id.
#[must_use]
pub fn entries(model: &Model) -> Vec<ContractEntry> {
    let prefix = model.prefix();
    let mut out = Vec::new();
    let mut push = |path: String, sig: Sig<String>| {
        let signature = sig.render();
        out.push(ContractEntry {
            id: fnv1a64(path.as_bytes()),
            hash: fnv1a64(signature.as_bytes()),
            symbol: symbol(prefix, &path),
            path,
            signature,
        });
    };
    for f in &model.functions {
        push(f.path.clone(), callable_signature(f));
    }
    for i in &model.interfaces {
        push(i.name.clone(), Sig::text(format!("interface {}", i.name)));
        for f in i.members() {
            push(f.path.clone(), callable_signature(f));
        }
    }
    for r in &model.records {
        push(r.name.clone(), Sig::record(&r.name, field_types(&r.fields)));
    }
    for e in &model.enums {
        let variants = e
            .variants
            .iter()
            .map(|v| (v.name.clone(), v.value, field_types(&v.fields)))
            .collect();
        push(e.name.clone(), Sig::enumeration(&e.name, variants));
    }
    for c in &model.callbacks {
        push(c.name.clone(), Sig::text(format!("callback {}", c.name)));
        for m in &c.methods {
            push(
                format!("{}.{}", c.name, m.name),
                Sig::callback_method(
                    m.slot,
                    &m.name,
                    field_types(&m.params),
                    m.ret.as_ref().map_or_else(|| VOID.to_string(), canonical),
                    throws_suffix(&m.error),
                ),
            );
        }
    }
    for d in &model.errors {
        push(d.name.clone(), Sig::text(format!("errors {}", d.name)));
        for c in &d.codes {
            push(
                format!("{}.{}", d.name, c.name),
                Sig::code(&c.name, c.value, field_types(&c.fields)),
            );
        }
    }
    out.sort_by_key(|e| e.id);
    out
}

fn field_types(fields: &[FieldBinding]) -> Vec<String> {
    fields.iter().map(|f| canonical(&f.ty)).collect()
}

fn callable_signature(f: &FnBinding) -> Sig<String> {
    Sig::callable(
        f.kind.keyword(),
        &f.name,
        f.params.iter().map(|p| canonical_param(&p.ty)).collect(),
        canonical_ret(f.ret.as_ref()),
        throws_suffix(&f.error),
        f.is_async(),
        f.cancellable(),
    )
}

#[cfg(all(test, feature = "idl"))]
mod tests {
    use super::*;
    use crate::ir::Api;
    use crate::pkg::Identity;

    fn model(yaml: &str) -> Model {
        let api: Api = serde_saphyr::from_str(yaml).unwrap();
        crate::validate::validate(&api, &Identity::named("kv"), None).unwrap()
    }

    const KV: &str = r#"
version: "0.13.0"
errors:
  - name: KvError
    codes:
      - { name: NotFound, code: 1, message: missing, fields: [{ name: key, type: string }] }
      - { name: Full, code: 2, message: full }
records:
  - name: Entry
    fields: [{ name: key, type: string }, { name: value, type: bytes }]
enums:
  - name: Kind
    variants: [{ name: Hot }, { name: Cold, value: 4 }]
  - name: Shape
    variants: [{ name: Dot }, { name: Circle, fields: [{ name: r, type: f64 }] }]
interfaces:
  - name: Store
    constructors: [{ name: open, params: [{ name: path, type: string }], throws: KvError }]
    methods:
      - { name: get, params: [{ name: key, type: string }], return: Entry, throws: KvError }
      - { name: sizes, params: [{ name: limit, type: "u32?" }], return: "[u64]", throws: any }
callbacks:
  - name: Listener
    methods:
      - { name: on_put, params: [{ name: entry, type: Entry }], return: string, throws: KvError }
      - { name: on_clear }
functions:
  - { name: version, return: string }
  - { name: fetch, params: [{ name: key, type: string }], return: "i64?", async: true, cancellable: true }
  - { name: watch, params: [{ name: l, type: "Listener?" }], return: "iter<[i32]>" }
"#;

    fn signature(model: &Model, path: &str) -> String {
        let entry = entries(model).into_iter().find(|e| e.path == path);
        let entry = entry.unwrap_or_else(|| panic!("no entry {path}"));
        assert_eq!(entry.id, fnv1a64(path.as_bytes()));
        assert_eq!(entry.hash, fnv1a64(entry.signature.as_bytes()));
        assert_eq!(entry.symbol, format!("kv_contract_{:016x}", entry.id));
        entry.signature
    }

    #[test]
    fn one_entry_per_declaration_member_code_and_method_sorted_by_id() {
        let model = model(KV);
        let table = entries(&model);
        assert!(table.windows(2).all(|w| w[0].id < w[1].id));
        let mut got: Vec<&str> = table.iter().map(|e| e.path.as_str()).collect();
        got.sort_unstable();
        assert_eq!(
            got,
            [
                "Entry",
                "Kind",
                "KvError",
                "KvError.Full",
                "KvError.NotFound",
                "Listener",
                "Listener.on_clear",
                "Listener.on_put",
                "Shape",
                "Store",
                "Store.get",
                "Store.open",
                "Store.sizes",
                "fetch",
                "version",
                "watch",
            ]
        );
    }

    #[test]
    fn canonical_signatures() {
        let m = model(KV);
        let cases = [
            ("version", "function version() -> string"),
            ("fetch", "function fetch(string) -> i64? async cancellable"),
            ("watch", "function watch(Listener?) -> iter<[i32]>"),
            ("Store", "interface Store"),
            (
                "Store.open",
                "constructor open(string) -> Store throws KvError",
            ),
            ("Store.get", "method get(string) -> Entry throws KvError"),
            ("Store.sizes", "method sizes(u32?) -> [u64] throws any"),
            ("Entry", "record Entry {string, bytes}"),
            ("Kind", "enum Kind {Hot = 0, Cold = 4}"),
            ("Shape", "enum Shape {Dot = 0, Circle = 1 {f64}}"),
            ("Listener", "callback Listener"),
            (
                "Listener.on_put",
                "callback_method #0 on_put(Entry) -> string throws KvError",
            ),
            ("Listener.on_clear", "callback_method #1 on_clear() -> void"),
            ("KvError", "errors KvError"),
            ("KvError.NotFound", "code NotFound = 1 {string}"),
            ("KvError.Full", "code Full = 2"),
        ];
        for (path, expected) in cases {
            assert_eq!(signature(&m, path), expected, "{path}");
        }
    }

    #[test]
    fn the_documented_example_hashes() {
        assert_eq!(
            fnv1a64(b"method get(string) -> Entry throws KvError"),
            fnv1a64("method get(string) -> Entry throws KvError".as_bytes())
        );
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    }

    #[test]
    fn slots_render_where_the_caller_puts_them() {
        let sig: Sig<&str> = Sig::callable("method", "f", vec!["A", "B"], "R", "T", true, false);
        assert_eq!(
            sig.0,
            [
                Part::Lit("method f(".into()),
                Part::Slot("A"),
                Part::Lit(", ".into()),
                Part::Slot("B"),
                Part::Lit(") -> ".into()),
                Part::Slot("R"),
                Part::Slot("T"),
                Part::Lit(" async".into()),
            ]
        );
    }
}
