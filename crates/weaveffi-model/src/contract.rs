//! Contract tables: a per-declaration fingerprint of each top-level module's
//! ABI.
//!
//! A producer exports, for every top-level module `m`,
//! `const {prefix}_contract_entry* {prefix}_{m}_contract(size_t* out_len)`:
//! a static table with one [`ContractEntry`] per declaration in `m` and its
//! submodules, sorted ascending by id. Every generated consumer checks, when
//! it loads the library, that each entry it was generated with is present in
//! the producer's table with an equal hash. Extra producer entries are fine,
//! so adding a declaration never breaks a deployed binding, while removing
//! or changing one is reported by its dotted path.
//!
//! [`entries`] is the one function that computes a table. The
//! `#[weaveffi::module]` macro, the CLI, and the C header generator all call
//! it on the validated [`Model`], so the two sides agree by construction.
//!
//! * The **id** is [`fnv1a64`] of the declaration's dotted path
//!   (`kv.Store.put`, `kv.KvError`, `kv.KvError.NotFound`,
//!   `kv.Listener.on_put`).
//! * The **hash** is [`fnv1a64`] of a canonical signature string computed
//!   from the resolved model, never from source spelling: type aliases are
//!   already substituted, `[u8]` is already `bytes`, and every type is its
//!   resolved IDL spelling (see [`Ty`]'s `Display`). Names that don't
//!   affect the ABI (parameter and field names), docs, deprecation text,
//!   and error messages are excluded, and so is the order of sibling
//!   declarations, because each entry is independent.
//!
//! Error domains and callback interfaces are **open**: each code and each
//! callback method is its own entry, so adding a code or a method never
//! breaks a deployed binding (a consumer maps a positive code it doesn't
//! know to the domain's base error type, and a vtable may grow; its `size`
//! header is still checked).
//!
//! The canonical strings are:
//!
//! | Declaration | Path | Signature |
//! |---|---|---|
//! | function or interface member | `{module}.{name}`, `{module}.{Interface}.{name}` | `{kind} {name}({type}, ...) -> {type or void}`, then ` throws {Domain}` or ` throws any`, ` async`, ` cancellable` when set; `kind` is `function`, `constructor`, `method`, or `static` |
//! | interface | `{module}.{Interface}` | `interface {name}` (its members have their own entries) |
//! | record | `{module}.{Record}` | `record {name} {{type}, ...}` (field types only, in order) |
//! | enum | `{module}.{Enum}` | `enum {name} {{Variant} = {value}, ...}`, a variant with fields followed by ` {{type}, ...}` |
//! | callback interface | `{module}.{Callback}` | `callback {name}` |
//! | callback method | `{module}.{Callback}.{method}` | `callback_method {name}({type}, ...) -> {type or void}`, then ` throws {Domain}` or ` throws any` when set |
//! | error domain | `{module}.{Domain}` | `errors {name}` |
//! | error code | `{module}.{Domain}.{Code}` | `code {Code} = {value}`, a code with fields followed by ` {{type}, ...}` |

use std::fmt::Write;

use crate::model::{FieldBinding, FnBinding, Model, ModuleBinding};
use crate::plan::ErrorStrategy;
use crate::ty::Ty;

/// One declaration's entry in a module's contract table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractEntry {
    /// The declaration's dotted path (`kv.Store.put`), which consumers name
    /// in their load errors.
    pub path: String,
    /// [`fnv1a64`] of [`path`](Self::path).
    pub id: u64,
    /// [`fnv1a64`] of [`signature`](Self::signature).
    pub hash: u64,
    /// The declaration's canonical signature string (see the table in the
    /// [module docs](self)), kept for diagnostics and generated comments.
    pub signature: String,
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

/// The contract table of the top-level module `root` (one of `model`'s
/// [roots](Model::roots)): one entry per declaration in `root` and its
/// submodules, sorted ascending by id.
#[must_use]
pub fn entries(model: &Model, root: &ModuleBinding) -> Vec<ContractEntry> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(m) = stack.pop() {
        module_entries(m, &mut out);
        stack.extend(model.children(m));
    }
    out.sort_by_key(|e| e.id);
    out
}

fn entry(path: String, signature: String) -> ContractEntry {
    ContractEntry {
        id: fnv1a64(path.as_bytes()),
        hash: fnv1a64(signature.as_bytes()),
        path,
        signature,
    }
}

fn module_entries(m: &ModuleBinding, out: &mut Vec<ContractEntry>) {
    let dot = &m.dot_path;
    for f in &m.functions {
        out.push(entry(
            format!("{dot}.{}", f.name),
            callable_signature("function", f),
        ));
    }
    for i in &m.interfaces {
        let owner = format!("{dot}.{}", i.name);
        out.push(entry(owner.clone(), format!("interface {}", i.name)));
        for (kind, members) in [
            ("constructor", &i.constructors),
            ("method", &i.methods),
            ("static", &i.statics),
        ] {
            for f in members {
                out.push(entry(
                    format!("{owner}.{}", f.name),
                    callable_signature(kind, f),
                ));
            }
        }
    }
    for s in &m.structs {
        out.push(entry(
            format!("{dot}.{}", s.name),
            format!("record {} {{{}}}", s.name, field_types(&s.fields)),
        ));
    }
    for e in &m.enums {
        let variants: Vec<String> = e
            .variants
            .iter()
            .map(|v| case(&v.name, v.value, &v.fields))
            .collect();
        out.push(entry(
            format!("{dot}.{}", e.name),
            format!("enum {} {{{}}}", e.name, variants.join(", ")),
        ));
    }
    for c in &m.callback_interfaces {
        let owner = format!("{dot}.{}", c.name);
        out.push(entry(owner.clone(), format!("callback {}", c.name)));
        for cm in &c.methods {
            let types: Vec<String> = cm.params.iter().map(|p| p.ty.to_string()).collect();
            let mut sig = format!(
                "callback_method {}({}) -> {}",
                cm.name,
                types.join(", "),
                cm.ret
                    .as_ref()
                    .map_or_else(|| "void".to_string(), Ty::to_string)
            );
            throws(&mut sig, &cm.error);
            out.push(entry(format!("{owner}.{}", cm.name), sig));
        }
    }
    for d in &m.errors {
        let owner = format!("{dot}.{}", d.name);
        out.push(entry(owner.clone(), format!("errors {}", d.name)));
        for c in &d.codes {
            out.push(entry(
                format!("{owner}.{}", c.name),
                format!("code {}", case(&c.name, c.value, &c.fields)),
            ));
        }
    }
}

/// The field types, in order, comma-separated (names are ABI-neutral).
fn field_types(fields: &[FieldBinding]) -> String {
    let all: Vec<String> = fields.iter().map(|f| f.ty.to_string()).collect();
    all.join(", ")
}

/// `Name = value`, followed by ` {types}` when the case carries fields.
fn case(name: &str, value: i32, case_fields: &[FieldBinding]) -> String {
    let mut out = format!("{name} = {value}");
    if !case_fields.is_empty() {
        let _ = write!(out, " {{{}}}", field_types(case_fields));
    }
    out
}

/// Append ` throws {Domain}` or ` throws any` for a throwing strategy.
fn throws(sig: &mut String, error: &ErrorStrategy) {
    match error {
        ErrorStrategy::Trap => {}
        ErrorStrategy::Domain(domain) => {
            let _ = write!(sig, " throws {domain}");
        }
        ErrorStrategy::Untyped => sig.push_str(" throws any"),
    }
}

fn callable_signature(kind: &str, f: &FnBinding) -> String {
    let types: Vec<String> = f.params.iter().map(|p| p.ty.to_string()).collect();
    let mut sig = format!(
        "{kind} {}({}) -> {}",
        f.name,
        types.join(", "),
        f.ret
            .as_ref()
            .map_or_else(|| "void".to_string(), ToString::to_string)
    );
    throws(&mut sig, &f.error);
    if f.is_async() {
        sig.push_str(" async");
    }
    if f.cancellable() {
        sig.push_str(" cancellable");
    }
    sig
}

#[cfg(all(test, feature = "idl"))]
mod tests {
    use super::*;
    use crate::ir::Api;
    use crate::pkg::Identity;

    fn model(yaml: &str) -> Model {
        let api: Api = serde_yaml_ng::from_str(yaml).unwrap();
        crate::validate::validate(&api, &Identity::named("kv"), None).unwrap()
    }

    const KV: &str = r#"
version: "0.12.0"
modules:
  - name: kv
    doc: Key-value storage.
    errors:
      - name: KvError
        codes:
          - { name: NotFound, code: 1, message: missing, fields: [{ name: key, type: string }] }
          - { name: Full, code: 2, message: full }
    structs:
      - name: Entry
        fields: [{ name: key, type: string }, { name: value, type: bytes }]
    interfaces:
      - name: Store
        constructors: [{ name: open, params: [{ name: path, type: string }], throws: KvError }]
        methods:
          - { name: get, params: [{ name: key, type: string }], return: Entry, throws: KvError }
          - { name: sizes, params: [{ name: limit, type: "u32?" }], return: "[u64]", throws: any }
    callback_interfaces:
      - name: Listener
        methods:
          - { name: on_put, params: [{ name: entry, type: Entry }], return: string, throws: KvError }
          - { name: on_clear, params: [] }
    functions:
      - { name: version, params: [], return: string, doc: The version. }
      - { name: fetch, params: [{ name: key, type: string }], return: "i64?", async: true, cancellable: true }
      - { name: watch, params: [{ name: l, type: "Listener?" }], return: "iter<[i32]>" }
    modules:
      - name: stats
        enums:
          - name: Kind
            variants: [{ name: Hot, value: 0 }, { name: Cold, value: 1 }]
          - name: Shape
            variants: [{ name: Dot, value: 0 }, { name: Circle, value: 1, fields: [{ name: r, type: f64 }] }]
"#;

    fn table(model: &Model) -> Vec<ContractEntry> {
        let root = model.roots().next().unwrap();
        entries(model, root)
    }

    fn signature(model: &Model, path: &str) -> String {
        let entry = table(model).into_iter().find(|e| e.path == path);
        let entry = entry.unwrap_or_else(|| panic!("no entry {path}"));
        assert_eq!(entry.id, fnv1a64(path.as_bytes()));
        assert_eq!(entry.hash, fnv1a64(entry.signature.as_bytes()));
        entry.signature
    }

    #[test]
    fn one_entry_per_declaration_code_and_callback_method_sorted_by_id() {
        let model = model(KV);
        let table = table(&model);
        let mut got: Vec<String> = table.iter().map(|e| e.path.clone()).collect();
        got.sort();
        assert_eq!(
            got,
            [
                "kv.Entry",
                "kv.KvError",
                "kv.KvError.Full",
                "kv.KvError.NotFound",
                "kv.Listener",
                "kv.Listener.on_clear",
                "kv.Listener.on_put",
                "kv.Store",
                "kv.Store.get",
                "kv.Store.open",
                "kv.Store.sizes",
                "kv.fetch",
                "kv.stats.Kind",
                "kv.stats.Shape",
                "kv.version",
                "kv.watch",
            ]
        );
        assert!(table.windows(2).all(|w| w[0].id < w[1].id));
    }

    #[test]
    fn canonical_signatures() {
        let model = model(KV);
        let cases = [
            ("kv.Store.get", "method get(string) -> Entry throws KvError"),
            (
                "kv.Store.open",
                "constructor open(string) -> Store throws KvError",
            ),
            ("kv.Store.sizes", "method sizes(u32?) -> [u64] throws any"),
            ("kv.Store", "interface Store"),
            ("kv.version", "function version() -> string"),
            (
                "kv.fetch",
                "function fetch(string) -> i64? async cancellable",
            ),
            ("kv.watch", "function watch(Listener?) -> iter<[i32]>"),
            ("kv.Entry", "record Entry {string, bytes}"),
            ("kv.stats.Kind", "enum Kind {Hot = 0, Cold = 1}"),
            ("kv.stats.Shape", "enum Shape {Dot = 0, Circle = 1 {f64}}"),
            ("kv.Listener", "callback Listener"),
            (
                "kv.Listener.on_put",
                "callback_method on_put(Entry) -> string throws KvError",
            ),
            ("kv.Listener.on_clear", "callback_method on_clear() -> void"),
            ("kv.KvError", "errors KvError"),
            ("kv.KvError.NotFound", "code NotFound = 1 {string}"),
            ("kv.KvError.Full", "code Full = 2"),
        ];
        for (path, expected) in cases {
            assert_eq!(signature(&model, path), expected, "{path}");
        }
    }

    #[test]
    fn names_prose_and_sibling_order_are_excluded() {
        let a = model(KV);
        let edited = KV
            .replace("doc: The version.", "doc: Changed prose.")
            .replace("message: missing", "message: gone")
            .replace("{ name: key, type: string }]", "{ name: k, type: string }]")
            .replace("name: value, type: bytes", "name: data, type: bytes");
        let b = model(&edited);
        assert_eq!(table(&a), table(&b));
        // Swapping two variants changes the enum (its own order is part of
        // the signature) but not its neighbors.
        let swapped = KV.replace(
            "[{ name: Hot, value: 0 }, { name: Cold, value: 1 }]",
            "[{ name: Cold, value: 1 }, { name: Hot, value: 0 }]",
        );
        let c = model(&swapped);
        assert_ne!(
            signature(&a, "kv.stats.Kind"),
            signature(&c, "kv.stats.Kind")
        );
        assert_eq!(signature(&a, "kv.Entry"), signature(&c, "kv.Entry"));
    }

    #[test]
    fn domains_and_callbacks_are_open() {
        let a = model(KV);
        let grown = model(
            &KV.replace(
                "- { name: Full, code: 2, message: full }",
                "- { name: Full, code: 2, message: full }\n          - { name: Busy, code: 3, message: busy }",
            )
            .replace(
                "- { name: on_clear, params: [] }",
                "- { name: on_clear, params: [] }\n          - { name: on_close, params: [] }",
            ),
        );
        // Every entry the old binding has is unchanged in the grown table.
        let grown_table = table(&grown);
        for e in table(&a) {
            assert!(grown_table.contains(&e), "{} changed", e.path);
        }
        assert_eq!(grown_table.len(), table(&a).len() + 2);
    }

    #[test]
    fn abi_changes_change_only_their_entry() {
        let a = model(KV);
        let b = model(&KV.replace(
            "return: Entry, throws: KvError",
            "return: \"Entry?\", throws: KvError",
        ));
        assert_ne!(signature(&a, "kv.Store.get"), signature(&b, "kv.Store.get"));
        assert_eq!(
            signature(&a, "kv.Store.open"),
            signature(&b, "kv.Store.open")
        );
        assert_eq!(signature(&a, "kv.Store"), signature(&b, "kv.Store"));
        let c = model(&KV.replace(
            "return: Entry, throws: KvError",
            "return: Entry, throws: any",
        ));
        assert_eq!(
            signature(&c, "kv.Store.get"),
            "method get(string) -> Entry throws any"
        );
    }

    #[test]
    fn fnv_is_pinned() {
        // Changing the algorithm invalidates every deployed binding, so it
        // must be a deliberate, documented ABI change.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
