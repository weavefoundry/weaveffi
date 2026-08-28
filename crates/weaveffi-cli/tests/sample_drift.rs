//! The sample drift gate: every macro-annotated sample crate must extract to
//! the same API shape its hand-written YAML IDL declares.
//!
//! The samples ship both authoring paths (a `#[weaveffi::module]` `lib.rs`
//! and a YAML IDL), and the conformance harness builds producers from the
//! macro path while generating consumers from the YAML path. Nothing but
//! this test forces the two views to agree, and a silent mismatch means the
//! consumers call symbols the producer never exported (or marshal a shape it
//! doesn't produce).
//!
//! The comparison is structural: module tree, function signatures and flags,
//! record fields, enum variants and values, callbacks, listeners, interfaces,
//! and error domains (names, codes, and messages, which cross the ABI).
//! Prose doc comments are deliberately not gated; they're cosmetic, differ in
//! phrasing between the two sources, and can't break a consumer. Struct
//! field defaults and standalone `since:` values are documented extract
//! gaps (see `docs/src/guides/extract.md`) and are skipped too.

use weaveffi_ir::ir::{Api, Function, Module, StructField, TypeRef};
use weaveffi_ir::parse::parse_api_str;

/// Fold a type to the shape that survives an extract-then-reparse cycle:
/// resolved kinds (`Record`, `Enum`, `Interface`, ...) re-parse as `Named`,
/// and validator-qualified cross-module names (`shared.Token`) reduce to
/// their final segment.
fn normalize_type(ty: &TypeRef) -> TypeRef {
    fn last_segment(name: &str) -> String {
        name.rsplit('.').next().unwrap_or(name).to_string()
    }
    match ty {
        TypeRef::Named(name)
        | TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Enum(name)
        | TypeRef::Interface(name) => TypeRef::Named(last_segment(name)),
        TypeRef::TypedHandle(name) => TypeRef::TypedHandle(last_segment(name)),
        TypeRef::Optional(inner) => TypeRef::Optional(Box::new(normalize_type(inner))),
        TypeRef::List(inner) => TypeRef::List(Box::new(normalize_type(inner))),
        TypeRef::Iterator(inner) => TypeRef::Iterator(Box::new(normalize_type(inner))),
        TypeRef::Map(k, v) => {
            TypeRef::Map(Box::new(normalize_type(k)), Box::new(normalize_type(v)))
        }
        other => other.clone(),
    }
}

/// Strip everything the gate doesn't compare from one function.
fn canon_function(f: &mut Function) {
    f.doc = None;
    // A standalone `since:` (without `#[deprecated]`) has no Rust spelling.
    if f.deprecated.is_none() {
        f.since = None;
    }
    for p in &mut f.params {
        p.doc = None;
        p.ty = normalize_type(&p.ty);
    }
    if let Some(ret) = &mut f.returns {
        *ret = normalize_type(ret);
    }
}

/// Strip everything the gate doesn't compare from one field.
fn canon_field(field: &mut StructField) {
    field.doc = None;
    field.ty = normalize_type(&field.ty);
}

/// Strip everything the gate doesn't compare from one module, recursively.
fn canon_module(m: &mut Module) {
    for f in &mut m.functions {
        canon_function(f);
    }
    for i in &mut m.interfaces {
        i.doc = None;
        for f in i
            .constructors
            .iter_mut()
            .chain(i.methods.iter_mut())
            .chain(i.statics.iter_mut())
        {
            canon_function(f);
        }
    }
    for s in &mut m.structs {
        s.doc = None;
        for field in &mut s.fields {
            canon_field(field);
        }
    }
    for e in &mut m.enums {
        e.doc = None;
        for v in &mut e.variants {
            v.doc = None;
            for field in &mut v.fields {
                canon_field(field);
            }
        }
    }
    for c in &mut m.callbacks {
        c.doc = None;
        for p in &mut c.params {
            p.doc = None;
            p.ty = normalize_type(&p.ty);
        }
    }
    for l in &mut m.listeners {
        l.doc = None;
    }
    if let Some(errors) = &mut m.errors {
        for code in &mut errors.codes {
            // Codes, names, and messages cross the ABI and are compared; a
            // separate `doc:` next to `message:` has no Rust spelling.
            code.doc = None;
            for field in &mut code.fields {
                canon_field(field);
            }
        }
    }
    for child in &mut m.modules {
        canon_module(child);
    }
}

/// The canonical, comparable form of an API: modules only, docs and lossy
/// fields stripped, types normalized.
fn canon_api(api: &Api) -> Vec<Module> {
    let mut modules = api.modules.clone();
    for m in &mut modules {
        canon_module(m);
    }
    modules
}

#[test]
fn samples_never_drift_from_their_idl() {
    let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let samples_dir = manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join("samples");

    let mut gated = 0usize;
    for entry in std::fs::read_dir(&samples_dir).expect("read samples dir") {
        let dir = entry.expect("sample dir entry").path();
        if !dir.is_dir() {
            continue;
        }
        let lib_rs = dir.join("src/lib.rs");
        let Ok(src) = std::fs::read_to_string(&lib_rs) else {
            continue;
        };
        if !src.contains("weaveffi::module") {
            continue;
        }
        let Some(yml) = std::fs::read_dir(&dir)
            .expect("read sample dir")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| {
                p.extension()
                    .is_some_and(|ext| ext == "yml" || ext == "yaml")
            })
        else {
            continue;
        };

        let sample = dir.file_name().unwrap().to_string_lossy().to_string();
        let idl_src = std::fs::read_to_string(&yml).expect("read sample IDL");
        let original = parse_api_str(&idl_src, "yaml").expect("parse sample IDL");

        let output = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("weaveffi binary")
            .args(["extract", lib_rs.to_str().unwrap(), "-f", "yaml"])
            .output()
            .expect("run weaveffi extract");
        assert!(
            output.status.success(),
            "[{sample}] extract failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let extracted_yaml = String::from_utf8(output.stdout).expect("extract stdout utf-8");
        let extracted = parse_api_str(&extracted_yaml, "yaml").expect("parse extracted YAML");

        assert_eq!(
            canon_api(&extracted),
            canon_api(&original),
            "[{sample}] the macro-annotated lib.rs and the YAML IDL have \
             drifted; update whichever side is stale so both declare the \
             same API shape"
        );
        gated += 1;
    }

    // If this trips, the discovery walk broke (or samples moved); the gate
    // must never silently pass by gating nothing.
    assert!(
        gated >= 7,
        "expected at least 7 gated samples, found {gated}"
    );
}
