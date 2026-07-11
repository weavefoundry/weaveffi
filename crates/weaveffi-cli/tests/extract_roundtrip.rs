//! Round-trip tests for `weaveffi extract`.
//!
//! `roundtrip_kitchen_sink` proves the extractor recovers the same shape
//! as the original kitchen-sink IDL when run on the hand-annotated Rust
//! file at `crates/weaveffi-cli/tests/fixtures/kitchen_sink_annotated.rs`,
//! including the 0.5.0 surface: the `Gadget` interface, the `KitchenErrors`
//! domain, and per-function `throws`. Lossy fields (struct field defaults,
//! iterator returns, standalone `since` without `#[deprecated]`, callback
//! param docs, error-code `doc:` separate from `message:`) are documented
//! in `docs/src/guides/extract.md` and skipped by name in the assertions
//! below.

use std::collections::BTreeMap;

use weaveffi_ir::ir::{Api, Function, InterfaceDef, Module, StructDef, TypeRef};
use weaveffi_ir::parse::parse_api_str;

/// The validator that runs inside `weaveffi extract` rewrites cross-module
/// struct refs (e.g. `Token` → `shared.Token`) and resolves each bare name
/// to its kind (`Record`, `RichEnum`, `Enum`, `Interface`). Neither rewrite
/// survives the YAML round-trip because a string like `"Priority"` always
/// re-parses as `Named("Priority")` regardless of which kind it was. Compare
/// types modulo those two transforms so a fresh parse matches a validated
/// one.
fn normalize(ty: &TypeRef) -> TypeRef {
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
        TypeRef::Optional(inner) => TypeRef::Optional(Box::new(normalize(inner))),
        TypeRef::List(inner) => TypeRef::List(Box::new(normalize(inner))),
        TypeRef::Iterator(inner) => TypeRef::Iterator(Box::new(normalize(inner))),
        TypeRef::Map(k, v) => TypeRef::Map(Box::new(normalize(k)), Box::new(normalize(v))),
        other => other.clone(),
    }
}

fn assert_types_equivalent(a: &TypeRef, b: &TypeRef, ctx: &str) {
    assert_eq!(normalize(a), normalize(b), "{ctx} type mismatch");
}

fn module_by_name<'a>(api: &'a Api, name: &str) -> &'a Module {
    api.modules
        .iter()
        .find(|m| m.name == name)
        .unwrap_or_else(|| panic!("module {name} missing; modules: {:?}", module_names(api)))
}

fn module_names(api: &Api) -> Vec<&str> {
    api.modules.iter().map(|m| m.name.as_str()).collect()
}

fn struct_by_name<'a>(module: &'a Module, name: &str) -> &'a StructDef {
    module
        .structs
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("struct {name} missing in module {}", module.name))
}

fn function_by_name<'a>(module: &'a Module, name: &str) -> &'a Function {
    module
        .functions
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("function {name} missing in module {}", module.name))
}

fn interface_by_name<'a>(module: &'a Module, name: &str) -> &'a InterfaceDef {
    module
        .interfaces
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("interface {name} missing in module {}", module.name))
}

/// Assert one extracted callable matches its original: name, params (name +
/// type), return, throws, async, cancellable, and doc.
fn assert_functions_equivalent(extracted: &Function, orig: &Function, ctx: &str) {
    assert_eq!(extracted.name, orig.name, "{ctx} name mismatch");
    assert_eq!(
        extracted.params.len(),
        orig.params.len(),
        "{ctx} param count mismatch"
    );
    for (a, b) in extracted.params.iter().zip(orig.params.iter()) {
        assert_eq!(a.name, b.name, "{ctx} param name mismatch");
        assert_types_equivalent(&a.ty, &b.ty, &format!("{ctx} param {}", b.name));
    }
    match (&extracted.returns, &orig.returns) {
        (Some(a), Some(b)) => assert_types_equivalent(a, b, &format!("{ctx} return")),
        (None, None) => {}
        (a, b) => panic!("{ctx} return mismatch: {a:?} vs {b:?}"),
    }
    assert_eq!(extracted.throws, orig.throws, "{ctx} throws mismatch");
    assert_eq!(extracted.r#async, orig.r#async, "{ctx} async mismatch");
    assert_eq!(
        extracted.cancellable, orig.cancellable,
        "{ctx} cancellable mismatch"
    );
    assert_eq!(extracted.doc, orig.doc, "{ctx} doc mismatch");
}

#[test]
fn roundtrip_kitchen_sink() {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let idl_path = format!("{manifest}/tests/fixtures/06_kitchen_sink.yml");
    let annotated_path = format!("{manifest}/tests/fixtures/kitchen_sink_annotated.rs");

    let original_src = std::fs::read_to_string(&idl_path).expect("read kitchen-sink IDL");
    let original = parse_api_str(&original_src, "yaml").expect("parse kitchen-sink IDL");

    let extract_output = assert_cmd::Command::cargo_bin("weaveffi")
        .expect("weaveffi binary not found")
        .args(["extract", &annotated_path, "-f", "yaml"])
        .output()
        .expect("failed to run weaveffi extract");
    assert!(
        extract_output.status.success(),
        "extract failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&extract_output.stdout),
        String::from_utf8_lossy(&extract_output.stderr),
    );
    let extracted_yaml = String::from_utf8(extract_output.stdout).expect("extract stdout utf-8");
    let extracted = parse_api_str(&extracted_yaml, "yaml").expect("parse extracted YAML");

    let original_modules: BTreeMap<&str, &Module> = original
        .modules
        .iter()
        .map(|m| (m.name.as_str(), m))
        .collect();
    let extracted_modules: BTreeMap<&str, &Module> = extracted
        .modules
        .iter()
        .map(|m| (m.name.as_str(), m))
        .collect();
    assert_eq!(
        original_modules.keys().collect::<Vec<_>>(),
        extracted_modules.keys().collect::<Vec<_>>(),
        "top-level modules differ"
    );

    // shared module
    let shared_orig = module_by_name(&original, "shared");
    let shared_ex = module_by_name(&extracted, "shared");
    let token_orig = struct_by_name(shared_orig, "Token");
    let token_ex = struct_by_name(shared_ex, "Token");
    assert_eq!(token_ex.name, token_orig.name);
    assert_eq!(token_ex.fields.len(), token_orig.fields.len());
    for (a, b) in token_ex.fields.iter().zip(token_orig.fields.iter()) {
        assert_eq!(a.name, b.name);
        assert_types_equivalent(&a.ty, &b.ty, &format!("Token.{}", b.name));
    }
    let ping_orig = function_by_name(shared_orig, "ping");
    let ping_ex = function_by_name(shared_ex, "ping");
    assert_types_equivalent(
        ping_ex.returns.as_ref().unwrap(),
        ping_orig.returns.as_ref().unwrap(),
        "ping return",
    );
    assert_eq!(ping_ex.doc, ping_orig.doc);

    // kitchen module
    let kitchen_orig = module_by_name(&original, "kitchen");
    let kitchen_ex = module_by_name(&extracted, "kitchen");

    // Enums (with variant docs)
    assert_eq!(kitchen_ex.enums.len(), kitchen_orig.enums.len());
    let prio_orig = &kitchen_orig.enums[0];
    let prio_ex = &kitchen_ex.enums[0];
    assert_eq!(prio_ex.name, prio_orig.name);
    assert_eq!(prio_ex.doc, prio_orig.doc);
    assert_eq!(prio_ex.variants.len(), prio_orig.variants.len());
    for (a, b) in prio_ex.variants.iter().zip(prio_orig.variants.iter()) {
        assert_eq!(a.name, b.name);
        assert_eq!(a.value, b.value);
        assert_eq!(a.doc, b.doc, "variant {} doc mismatch", b.name);
    }

    // Builder struct
    let item_orig = struct_by_name(kitchen_orig, "Item");
    let item_ex = struct_by_name(kitchen_ex, "Item");
    assert!(
        item_ex.builder,
        "Item builder flag should round-trip via #[weaveffi_builder]"
    );
    assert_eq!(item_ex.builder, item_orig.builder);
    assert_eq!(item_ex.doc, item_orig.doc);
    assert_eq!(item_ex.fields.len(), item_orig.fields.len());
    for (a, b) in item_ex.fields.iter().zip(item_orig.fields.iter()) {
        assert_eq!(a.name, b.name);
        assert_types_equivalent(&a.ty, &b.ty, &format!("Item.{}", b.name));
        assert_eq!(a.doc, b.doc, "Item.{} doc mismatch", b.name);
        // a.default may legitimately differ; the fixture cannot recover
        // struct field defaults from Rust syntax (documented gap).
    }

    // Callbacks
    assert_eq!(kitchen_ex.callbacks.len(), kitchen_orig.callbacks.len());
    let on_ready_orig = &kitchen_orig.callbacks[0];
    let on_ready_ex = &kitchen_ex.callbacks[0];
    assert_eq!(on_ready_ex.name, on_ready_orig.name);
    assert_eq!(on_ready_ex.doc, on_ready_orig.doc);
    assert_eq!(on_ready_ex.params.len(), on_ready_orig.params.len());
    for (a, b) in on_ready_ex.params.iter().zip(on_ready_orig.params.iter()) {
        assert_eq!(a.name, b.name);
        assert_types_equivalent(&a.ty, &b.ty, &format!("callback param {}", b.name));
    }

    // Listeners
    assert_eq!(kitchen_ex.listeners.len(), kitchen_orig.listeners.len());
    let listener_orig = &kitchen_orig.listeners[0];
    let listener_ex = &kitchen_ex.listeners[0];
    assert_eq!(listener_ex.name, listener_orig.name);
    assert_eq!(listener_ex.event_callback, listener_orig.event_callback);
    assert_eq!(listener_ex.doc, listener_orig.doc);

    // Functions: every original IDL function must reappear in extracted
    // output with matching shape, except for `stream_items` which has
    // no Rust syntax for `iter<T>` (documented gap).
    let lossy_functions: &[&str] = &["stream_items"];
    for orig in &kitchen_orig.functions {
        if lossy_functions.contains(&orig.name.as_str()) {
            continue;
        }
        let extracted = function_by_name(kitchen_ex, &orig.name);
        assert_eq!(
            extracted.params.len(),
            orig.params.len(),
            "{} param count mismatch",
            orig.name
        );
        for (a, b) in extracted.params.iter().zip(orig.params.iter()) {
            assert_eq!(a.name, b.name, "{} param name mismatch", orig.name);
            assert_types_equivalent(&a.ty, &b.ty, &format!("{} param {}", orig.name, b.name));
            assert_eq!(
                a.mutable, b.mutable,
                "{} param {} mutable mismatch",
                orig.name, b.name
            );
            // Param.doc is allowed to differ (documented gap).
        }
        match (&extracted.returns, &orig.returns) {
            (Some(a), Some(b)) => assert_types_equivalent(a, b, &format!("{} return", orig.name)),
            (None, None) => {}
            (a, b) => panic!("{} return mismatch: {:?} vs {:?}", orig.name, a, b),
        }
        assert_eq!(extracted.doc, orig.doc, "{} doc mismatch", orig.name);
        assert_eq!(
            extracted.r#async, orig.r#async,
            "{} async mismatch",
            orig.name
        );
        assert_eq!(
            extracted.cancellable, orig.cancellable,
            "{} cancellable mismatch",
            orig.name
        );
        assert_eq!(
            extracted.throws, orig.throws,
            "{} throws mismatch",
            orig.name
        );
        // `since` without an accompanying `#[deprecated(since = ...)]` is
        // not recoverable: `new_op` has `since: 0.3.0` in the IDL but no
        // way to express that in Rust syntax alone.
        if orig.deprecated.is_some() {
            assert_eq!(
                extracted.deprecated, orig.deprecated,
                "{} deprecated mismatch",
                orig.name
            );
            assert_eq!(extracted.since, orig.since, "{} since mismatch", orig.name);
        }
    }

    // Error domain: `#[weaveffi::error]` makes the domain derivable, so name,
    // code names, values, and messages must all round-trip. A code's `doc:` is
    // skipped: Rust spells the message as the first doc line, so the extracted
    // doc always includes the message and cannot match an IDL doc that omits it.
    let errors_orig = kitchen_orig
        .errors
        .as_ref()
        .expect("KitchenErrors in original");
    let errors_ex = kitchen_ex
        .errors
        .as_ref()
        .expect("KitchenErrors in extracted output");
    assert_eq!(errors_ex.name, errors_orig.name);
    assert_eq!(errors_ex.codes.len(), errors_orig.codes.len());
    for (a, b) in errors_ex.codes.iter().zip(errors_orig.codes.iter()) {
        assert_eq!(a.name, b.name, "error code name mismatch");
        assert_eq!(a.code, b.code, "error code {} value mismatch", b.name);
        assert_eq!(
            a.message, b.message,
            "error code {} message mismatch",
            b.name
        );
    }

    // The `maybe_item` throws flag rides through the loop above; assert it
    // explicitly too so a regression names the feature, not just the function.
    assert!(
        function_by_name(kitchen_ex, "maybe_item").throws,
        "maybe_item should round-trip as throws: true"
    );

    // Interface: constructors, methods, and statics with their shapes.
    let gadget_orig = interface_by_name(kitchen_orig, "Gadget");
    let gadget_ex = interface_by_name(kitchen_ex, "Gadget");
    assert_eq!(gadget_ex.doc, gadget_orig.doc, "Gadget doc mismatch");
    for (kind, ex, orig) in [
        (
            "constructor",
            &gadget_ex.constructors,
            &gadget_orig.constructors,
        ),
        ("method", &gadget_ex.methods, &gadget_orig.methods),
        ("static", &gadget_ex.statics, &gadget_orig.statics),
    ] {
        assert_eq!(
            ex.len(),
            orig.len(),
            "Gadget {kind} count mismatch: {:?} vs {:?}",
            ex.iter().map(|f| &f.name).collect::<Vec<_>>(),
            orig.iter().map(|f| &f.name).collect::<Vec<_>>(),
        );
        for (a, b) in ex.iter().zip(orig.iter()) {
            assert_functions_equivalent(a, b, &format!("Gadget {kind} {}", b.name));
        }
    }

    // Nested module
    let nested_orig = kitchen_orig
        .modules
        .iter()
        .find(|m| m.name == "nested")
        .expect("nested submodule in original");
    let nested_ex = kitchen_ex
        .modules
        .iter()
        .find(|m| m.name == "nested")
        .expect("nested submodule in extracted output");
    assert_eq!(nested_ex.functions.len(), nested_orig.functions.len());
    let hello_orig = function_by_name(nested_orig, "hello");
    let hello_ex = function_by_name(nested_ex, "hello");
    assert_types_equivalent(
        hello_ex.returns.as_ref().unwrap(),
        hello_orig.returns.as_ref().unwrap(),
        "hello return",
    );
    assert_eq!(hello_ex.doc, hello_orig.doc);
}
