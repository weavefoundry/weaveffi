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
//!   (`kv.Store.put`, `kv.KvError`, `kv.stats.Summary`).
//! * The **hash** is [`fnv1a64`] of a canonical signature string computed
//!   from the resolved model, never from source spelling: type aliases are
//!   already substituted, `[u8]` is already `bytes`, and every type is its
//!   resolved IDL spelling (see [`Ty`]'s `Display`). Docs, deprecation text,
//!   and error messages are excluded, and so is the order of sibling
//!   declarations, because each entry is independent.
//!
//! The canonical strings are:
//!
//! | Declaration | Signature |
//! |---|---|
//! | function or interface member | `{kind} {name}({param}: {type}, ...) -> {type or void}`, then ` throws`, ` async`, ` cancellable` when set; `kind` is `function`, `constructor`, `method`, or `static` |
//! | record | `record {name} {{field}: {type}, ...}` |
//! | enum | `enum {name} {{Variant} = {value}, ...}`, a rich variant followed by ` {{field}: {type}, ...}` |
//! | callback interface | `callback {name} {{method}({param}: {type}, ...) -> {type or void}; ...}`, a method followed by ` throws` when set |
//! | error domain | `errors {name} {{Code} = {value}, ...}`, a code with fields followed by ` {{field}: {type}, ...}` |
//! | interface | `interface {name}` (its members have their own entries) |

use std::fmt::Write;

use crate::model::{FieldBinding, FnBinding, Model, ModuleBinding, ParamBinding};
use crate::ty::Ty;

/// One declaration's entry in a module's contract table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractEntry {
    /// The declaration's dotted path (`kv.Store.put`), which consumers name
    /// in their load errors.
    pub path: String,
    /// [`fnv1a64`] of [`path`](Self::path).
    pub id: u64,
    /// [`fnv1a64`] of the declaration's canonical signature string.
    pub hash: u64,
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

/// The contract table of the top-level module `root` (which must be one of
/// `model`'s [roots](Model::roots)): one entry per declaration in `root` and
/// its submodules, sorted ascending by id.
#[must_use]
pub fn entries(model: &Model, root: &ModuleBinding) -> Vec<ContractEntry> {
    let mut out = Vec::new();
    for m in model
        .modules
        .iter()
        .filter(|m| m.segments.first() == root.segments.first())
    {
        module_entries(m, &mut out);
    }
    out.sort_by_key(|e| e.id);
    out
}

fn entry(path: String, signature: &str) -> ContractEntry {
    ContractEntry {
        id: fnv1a64(path.as_bytes()),
        hash: fnv1a64(signature.as_bytes()),
        path,
    }
}

fn module_entries(m: &ModuleBinding, out: &mut Vec<ContractEntry>) {
    let dot = &m.dot_path;
    for f in &m.functions {
        out.push(entry(
            format!("{dot}.{}", f.name),
            &callable_signature("function", f),
        ));
    }
    for i in &m.interfaces {
        let owner = format!("{dot}.{}", i.name);
        out.push(entry(owner.clone(), &format!("interface {}", i.name)));
        for (kind, members) in [
            ("constructor", &i.constructors),
            ("method", &i.methods),
            ("static", &i.statics),
        ] {
            for f in members {
                out.push(entry(
                    format!("{owner}.{}", f.name),
                    &callable_signature(kind, f),
                ));
            }
        }
    }
    for s in &m.structs {
        out.push(entry(
            format!("{dot}.{}", s.name),
            &format!("record {} {{{}}}", s.name, fields(&s.fields)),
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
            &format!("enum {} {{{}}}", e.name, variants.join(", ")),
        ));
    }
    for c in &m.callback_interfaces {
        let methods: Vec<String> = c
            .methods
            .iter()
            .map(|cm| {
                let mut sig = format!(
                    "{}({}) -> {}",
                    cm.name,
                    params(&cm.params),
                    ret(cm.ret.as_ref())
                );
                if cm.throws {
                    sig.push_str(" throws");
                }
                sig
            })
            .collect();
        out.push(entry(
            format!("{dot}.{}", c.name),
            &format!("callback {} {{{}}}", c.name, methods.join("; ")),
        ));
    }
    if let Some(d) = &m.errors {
        let codes: Vec<String> = d
            .codes
            .iter()
            .map(|c| case(&c.name, c.value, &c.fields))
            .collect();
        out.push(entry(
            format!("{dot}.{}", d.name),
            &format!("errors {} {{{}}}", d.name, codes.join(", ")),
        ));
    }
}

fn params(params: &[ParamBinding]) -> String {
    let all: Vec<String> = params
        .iter()
        .map(|p| format!("{}: {}", p.name, p.ty))
        .collect();
    all.join(", ")
}

fn fields(fields: &[FieldBinding]) -> String {
    let all: Vec<String> = fields
        .iter()
        .map(|f| format!("{}: {}", f.name, f.ty))
        .collect();
    all.join(", ")
}

fn ret(ty: Option<&Ty>) -> String {
    ty.map_or_else(|| "void".to_string(), ToString::to_string)
}

/// `Name = value`, followed by ` {fields}` when the case carries any.
fn case(name: &str, value: i32, case_fields: &[FieldBinding]) -> String {
    let mut out = format!("{name} = {value}");
    if !case_fields.is_empty() {
        let _ = write!(out, " {{{}}}", fields(case_fields));
    }
    out
}

fn callable_signature(kind: &str, f: &FnBinding) -> String {
    let mut sig = format!(
        "{kind} {}({}) -> {}",
        f.name,
        params(&f.params),
        ret(f.ret.as_ref())
    );
    if f.throws {
        sig.push_str(" throws");
    }
    if f.is_async() {
        sig.push_str(" async");
    }
    if f.cancellable {
        sig.push_str(" cancellable");
    }
    sig
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::Api;
    use crate::pkg::Identity;

    fn model(yaml: &str) -> Model {
        let api: Api = serde_yaml::from_str(yaml).unwrap();
        crate::validate::validate(&api, &Identity::named("kv"), None).unwrap()
    }

    const KV: &str = r#"
version: "0.11.0"
modules:
  - name: kv
    doc: Key-value storage.
    errors:
      name: KvError
      codes:
        - { name: NotFound, code: 1, message: missing, fields: [{ name: key, type: string }] }
    structs:
      - name: Entry
        fields: [{ name: key, type: string }, { name: value, type: bytes }]
    interfaces:
      - name: Store
        constructors: [{ name: open, params: [{ name: path, type: string }], throws: true }]
        methods:
          - { name: get, params: [{ name: key, type: string }], return: "bytes?", throws: true }
    callback_interfaces:
      - name: Listener
        methods:
          - { name: on_put, params: [{ name: entry, type: Entry }], return: string, throws: true }
    functions:
      - { name: version, params: [], return: string, doc: The version. }
    modules:
      - name: stats
        enums:
          - name: Kind
            variants: [{ name: Hot, value: 0 }, { name: Cold, value: 1 }]
"#;

    fn paths(model: &Model) -> Vec<String> {
        let root = model.roots().next().unwrap();
        entries(model, root).into_iter().map(|e| e.path).collect()
    }

    #[test]
    fn one_entry_per_declaration_sorted_by_id() {
        let model = model(KV);
        let root = model.roots().next().unwrap();
        let table = entries(&model, root);
        let mut got = paths(&model);
        got.sort();
        assert_eq!(
            got,
            [
                "kv.Entry",
                "kv.KvError",
                "kv.Listener",
                "kv.Store",
                "kv.Store.get",
                "kv.Store.open",
                "kv.stats.Kind",
                "kv.version"
            ]
        );
        assert!(table.windows(2).all(|w| w[0].id < w[1].id));
        let get = table.iter().find(|e| e.path == "kv.Store.get").unwrap();
        assert_eq!(get.id, fnv1a64(b"kv.Store.get"));
        assert_eq!(
            get.hash,
            fnv1a64(b"method get(key: string) -> bytes? throws")
        );
        let listener = table.iter().find(|e| e.path == "kv.Listener").unwrap();
        assert_eq!(
            listener.hash,
            fnv1a64(b"callback Listener {on_put(entry: Entry) -> string throws}")
        );
    }

    fn hash_of(model: &Model, path: &str) -> u64 {
        let root = model.roots().next().unwrap();
        entries(model, root)
            .into_iter()
            .find(|e| e.path == path)
            .unwrap()
            .hash
    }

    #[test]
    fn prose_and_sibling_order_are_excluded() {
        let a = model(KV);
        let edited = KV
            .replace("doc: The version.", "doc: Changed prose.")
            .replace("message: missing", "message: gone");
        let b = model(&edited);
        assert_eq!(
            entries(&a, a.roots().next().unwrap()),
            entries(&b, b.roots().next().unwrap())
        );
        // Swapping two variants changes the enum (its own order is part of
        // the signature) but not its neighbors.
        let swapped = KV.replace(
            "[{ name: Hot, value: 0 }, { name: Cold, value: 1 }]",
            "[{ name: Cold, value: 1 }, { name: Hot, value: 0 }]",
        );
        let c = model(&swapped);
        assert_ne!(hash_of(&a, "kv.stats.Kind"), hash_of(&c, "kv.stats.Kind"));
        assert_eq!(hash_of(&a, "kv.Entry"), hash_of(&c, "kv.Entry"));
    }

    #[test]
    fn abi_changes_change_only_their_entry() {
        let a = model(KV);
        let b = model(&KV.replace(
            "return: \"bytes?\", throws: true",
            "return: bytes, throws: true",
        ));
        assert_ne!(hash_of(&a, "kv.Store.get"), hash_of(&b, "kv.Store.get"));
        assert_eq!(hash_of(&a, "kv.Store.open"), hash_of(&b, "kv.Store.open"));
        assert_eq!(hash_of(&a, "kv.Store"), hash_of(&b, "kv.Store"));
    }

    #[test]
    fn fnv_is_pinned() {
        // Changing the algorithm invalidates every deployed binding, so it
        // must be a deliberate, documented ABI change.
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
