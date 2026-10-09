//! Model construction tests: symbols, signatures, and the passing contracts
//! stored on every binding, built from short YAML documents through the
//! validator (the only way to obtain a model).

use super::*;
use crate::ir::{Api, CURRENT_SCHEMA_VERSION};
use crate::parse::parse_api_str;
use crate::ty::Prim;
use crate::validate::{validate, validate_scoped, Options};

fn model(body: &str) -> Model {
    model_with_prefix(body, "weaveffi")
}

fn model_with_prefix(body: &str, prefix: &str) -> Model {
    let doc = format!("version: \"{CURRENT_SCHEMA_VERSION}\"\n{body}");
    let api = parse_api_str(&doc, "yaml").expect("fixture parses");
    match validate(&api, &Identity::named(prefix), None) {
        Ok(model) => model,
        Err(e) => panic!("fixture is invalid: {e}"),
    }
}

fn rendered(params: &[AbiParam]) -> Vec<String> {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
        .collect()
}

fn slots(params: Vec<&AbiParam>) -> Vec<String> {
    params
        .into_iter()
        .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
        .collect()
}

fn function<'a>(model: &'a Model, name: &str) -> &'a FnBinding {
    model
        .callables()
        .map(|(_, f)| f)
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("no callable {name}"))
}

#[test]
fn sync_function_symbol_and_sig() {
    let model = model(
        r#"
modules:
  - name: math
    functions:
      - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
"#,
    );
    let f = &model.modules[0].functions[0];
    assert_eq!(f.shape, CallShape::Sync);
    assert_eq!(f.abi.symbol, "weaveffi_math_add");
    assert_eq!(f.abi.ret, CType::Int32);
    assert_eq!(f.ret_pass, RetPass::Direct);
    assert_eq!(f.error, ErrorStrategy::Trap);
    assert!(!f.has_self());
    assert_eq!(
        rendered(&f.abi.params),
        ["int32_t a", "int32_t b", "weaveffi_error* out_err"]
    );
    assert!(matches!(&f.params[0].pass, ArgPass::Direct { slot } if slot.name == "a"));

    let model = model_with_prefix("modules: [{ name: net, functions: [{ name: f }] }]", "acme");
    assert_eq!(model.prefix(), "acme");
}

/// One document exercising OptDirect and Slice in every position.
const SHAPES: &str = r#"
modules:
  - name: s
    enums:
      - { name: Level, variants: [{ name: Low, value: 0 }, { name: High, value: 1 }] }
    callback_interfaces:
      - name: Sink
        methods:
          - { name: limit, params: [{ name: n, type: "u32?" }], return: "f64?" }
          - { name: samples, params: [{ name: xs, type: "[f32]" }], return: "[i16]" }
          - { name: level, params: [{ name: l, type: "Level?" }], return: "Level?" }
    functions:
      - name: clamp
        params: [{ name: x, type: "i32?" }, { name: on, type: "bool?" }, { name: lvl, type: "Level?" }]
        return: "i64?"
      - name: scale
        params: [{ name: xs, type: "[f64]" }, { name: ids, type: "[u64]" }]
        return: "[i32]"
      - { name: later, params: [], return: "i64?", async: true }
      - { name: later_xs, params: [], return: "[f32]", async: true, cancellable: true }
      - { name: maybe_ints, params: [], return: "iter<i32?>" }
      - { name: chunks, params: [], return: "iter<[i32]>" }
      - { name: feed, params: [{ name: sink, type: Sink }] }
"#;

#[test]
fn opt_direct_and_slice_params() {
    let model = model(SHAPES);
    let clamp = function(&model, "clamp");
    assert_eq!(
        rendered(&clamp.abi.params),
        [
            "bool has_x",
            "int32_t x",
            "bool has_on",
            "bool on",
            "bool has_lvl",
            "weaveffi_s_Level lvl",
            "int64_t* out_value",
            "weaveffi_error* out_err"
        ]
    );
    let ArgPass::OptDirect { has, value, inner } = &clamp.params[2].pass else {
        panic!("expected OptDirect: {:?}", clamp.params[2].pass);
    };
    assert_eq!((has.name.as_str(), value.name.as_str()), ("has_lvl", "lvl"));
    assert_eq!(inner, &Ty::Enum("Level".into()));

    let scale = function(&model, "scale");
    assert_eq!(
        rendered(&scale.abi.params),
        [
            "const double* xs_ptr",
            "size_t xs_len",
            "const uint64_t* ids_ptr",
            "size_t ids_len",
            "size_t* out_len",
            "weaveffi_error* out_err"
        ]
    );
    assert!(matches!(
        &scale.params[1].pass,
        ArgPass::Slice { elem: Prim::U64, ptr, len } if ptr.name == "ids_ptr" && len.name == "ids_len"
    ));
}

#[test]
fn opt_direct_and_slice_returns() {
    let model = model(SHAPES);
    let clamp = function(&model, "clamp");
    assert_eq!(clamp.abi.ret, CType::Bool);
    assert!(
        matches!(&clamp.ret_pass, RetPass::OptDirect { out_value } if out_value.name == "out_value")
    );

    let scale = function(&model, "scale");
    assert_eq!(scale.abi.ret.render_c("weaveffi"), "int32_t*");
    assert!(matches!(
        &scale.ret_pass,
        RetPass::Slice { elem: Prim::I32, out_len } if out_len.name == "out_len"
    ));
}

#[test]
fn opt_direct_and_slice_async_results() {
    let model = model(SHAPES);
    let later = function(&model, "later");
    let a = later.async_binding().expect("async");
    assert_eq!(later.ret_pass, RetPass::Void);
    assert_eq!(later.abi.ret, CType::Void);
    assert_eq!(
        rendered(&a.callback_params),
        [
            "void* context",
            "weaveffi_error* err",
            "bool has_result",
            "int64_t result"
        ]
    );
    assert!(
        matches!(&a.result, ResultPass::OptDirect { has, value } if has.name == "has_result" && value.name == "result")
    );
    assert!(!a.cancellable() && !later.cancellable());

    let xs = function(&model, "later_xs");
    let a = xs.async_binding().expect("async");
    assert_eq!(
        rendered(&a.callback_params),
        [
            "void* context",
            "weaveffi_error* err",
            "const float* result_ptr",
            "size_t result_len"
        ]
    );
    assert!(matches!(
        &a.result,
        ResultPass::Slice {
            elem: Prim::F32,
            ..
        }
    ));
    assert_eq!(a.callback_type, "weaveffi_s_later_xs_callback");
    assert!(xs.cancellable());
    assert_eq!(
        rendered(&xs.abi.params),
        [
            "weaveffi_cancel_token* cancel_token",
            "weaveffi_s_later_xs_callback callback",
            "void* context"
        ]
    );
}

#[test]
fn opt_direct_and_slice_iterator_items() {
    let model = model(SHAPES);
    let ints = function(&model, "maybe_ints");
    let it = ints.iterator().expect("iterator");
    assert_eq!(it.elem, Ty::Optional(Box::new(Ty::Prim(Prim::I32))));
    assert_eq!(
        rendered(&it.next.params),
        [
            "weaveffi_s_MaybeIntsIterator* iter",
            "bool* out_has_item",
            "int32_t* out_item",
            "weaveffi_error* out_err"
        ]
    );
    assert!(
        matches!(&it.item, ItemPass::OptDirect { out_has, out_item } if out_has.name == "out_has_item" && out_item.name == "out_item")
    );

    let chunks = function(&model, "chunks");
    let it = chunks.iterator().expect("iterator");
    assert_eq!(
        slots(it.item.slots()),
        ["int32_t** out_item", "size_t* out_len"]
    );
    assert!(matches!(
        &it.item,
        ItemPass::Slice {
            elem: Prim::I32,
            ..
        }
    ));
    assert_eq!(
        chunks.abi.ret.render_c("weaveffi"),
        "weaveffi_s_ChunksIterator*"
    );
    assert_eq!(
        chunks.ret,
        Some(RetTy::Iterator(Ty::List(Box::new(Ty::Prim(Prim::I32)))))
    );
}

#[test]
fn opt_direct_and_slice_callback_methods() {
    let model = model(SHAPES);
    let sink = model.callback_interface("Sink");
    let limit = &sink.methods[0];
    assert_eq!(limit.abi.symbol, "limit");
    assert_eq!(limit.abi.ret, CType::Bool);
    assert_eq!(
        rendered(&limit.abi.params),
        [
            "void* ctx",
            "bool has_n",
            "uint32_t n",
            "double* out_value",
            "weaveffi_error* out_err"
        ]
    );
    assert!(matches!(&limit.params[0].pass, ArgPass::OptDirect { .. }));
    assert!(
        matches!(&limit.ret_pass, CallbackRetPass::OptDirect { out_value } if out_value.name == "out_value")
    );

    let samples = &sink.methods[1];
    assert_eq!(samples.abi.ret, CType::Void);
    assert_eq!(
        rendered(&samples.abi.params),
        [
            "void* ctx",
            "const float* xs_ptr",
            "size_t xs_len",
            "int16_t** out_ptr",
            "size_t* out_len",
            "weaveffi_error* out_err"
        ]
    );
    assert!(matches!(
        &samples.params[0].pass,
        ArgPass::Slice {
            elem: Prim::F32,
            ..
        }
    ));
    assert!(matches!(
        &samples.ret_pass,
        CallbackRetPass::Slice { elem: Prim::I16, out_ptr, out_len }
            if out_ptr.name == "out_ptr" && out_len.name == "out_len"
    ));

    let level = &sink.methods[2];
    assert_eq!(
        rendered(&level.abi.params),
        [
            "void* ctx",
            "bool has_l",
            "weaveffi_s_Level l",
            "weaveffi_s_Level* out_value",
            "weaveffi_error* out_err"
        ]
    );
    // OptDirect and Slice values never need a buffer.
    assert!(!model.has_buffers());
}

#[test]
fn user_types_resolve_to_kinds_and_contracts() {
    let model = model(
        r#"
modules:
  - name: contacts
    structs:
      - name: Contact
        deprecated: use Person
        fields:
          - { name: name, type: string }
          - { name: status, type: Status }
          - { name: store, type: "Store?" }
    enums:
      - { name: Status, variants: [{ name: Ok, value: 0 }] }
    interfaces:
      - name: Store
        constructors: [{ name: open }]
        methods:
          - { name: save, params: [{ name: contact, type: Contact }], return: "[Contact]" }
  - name: ops
    functions:
      - { name: status_of, params: [{ name: store, type: Store }], return: Status }
      - { name: find, params: [], return: "Store?" }
"#,
    );
    let contacts = &model.modules[0];
    let s = &contacts.structs[0];
    assert_eq!(s.c_tag, "weaveffi_contacts_Contact");
    assert_eq!(s.deprecated.as_deref(), Some("use Person"));
    assert_eq!(s.fields[1].ty, Ty::Enum("Status".into()));
    assert_eq!(
        s.fields[2].ty,
        Ty::Optional(Box::new(Ty::Interface("Store".into())))
    );
    assert_eq!(contacts.enums[0].c_tag, "weaveffi_contacts_Status");
    assert_eq!(
        contacts.enums[0].variants[0].c_const,
        "weaveffi_contacts_Status_Ok"
    );

    let iface = &contacts.interfaces[0];
    assert_eq!(iface.c_tag, "weaveffi_contacts_Store");
    assert_eq!(iface.clone_symbol, "weaveffi_contacts_Store_clone");
    assert_eq!(iface.destroy_symbol, "weaveffi_contacts_Store_destroy");
    let open = &iface.constructors[0];
    assert_eq!(open.ret, Some(RetTy::Value(Ty::Interface("Store".into()))));
    assert_eq!(
        open.ret_pass,
        RetPass::Object {
            nullable: false,
            interface: "Store".into(),
            destroy_symbol: "weaveffi_contacts_Store_destroy".into(),
        }
    );
    assert_eq!(open.abi.symbol, "weaveffi_contacts_Store_open");
    let save = &iface.methods[0];
    assert!(save.has_self());
    assert_eq!(
        save.receiver.as_ref().map(|r| r.ty.render_c("weaveffi")),
        Some("const weaveffi_contacts_Store*".into())
    );
    assert_eq!(
        save.params[0].ty,
        ParamTy::Value(Ty::Record("Contact".into()))
    );
    assert_eq!(
        rendered(&save.abi.params),
        [
            "const weaveffi_contacts_Store* self",
            "const uint8_t* contact_ptr",
            "size_t contact_len",
            "size_t* out_len",
            "weaveffi_error* out_err"
        ]
    );
    assert_eq!(save.abi.ret.render_c("weaveffi"), "const uint8_t*");
    assert!(matches!(&save.ret_pass, RetPass::Buffer { .. }));

    let f = function(&model, "status_of");
    assert!(
        matches!(&f.params[0].pass, ArgPass::Object { nullable: false, interface, .. } if interface == "Store")
    );
    assert_eq!(f.ret, Some(RetTy::Value(Ty::Enum("Status".into()))));
    assert_eq!(
        rendered(&f.abi.params),
        [
            "const weaveffi_contacts_Store* store",
            "weaveffi_error* out_err"
        ]
    );
    assert_eq!(f.abi.ret.render_c("weaveffi"), "weaveffi_contacts_Status");
    assert!(matches!(
        &function(&model, "find").ret_pass,
        RetPass::Object { nullable: true, .. }
    ));
    assert!(model.has_buffers());
    assert!(model.has_interfaces());
    assert_eq!(model.interface("Store").c_tag, "weaveffi_contacts_Store");
    assert_eq!(model.record("Contact").c_tag, "weaveffi_contacts_Contact");
    assert_eq!(model.owner("Status").path, "contacts");
    assert_eq!(
        model.enumeration("Status").c_tag,
        "weaveffi_contacts_Status"
    );
}

#[test]
fn callback_interfaces_lower_to_vtables() {
    let model = model(
        r#"
modules:
  - name: events
    interfaces: [{ name: Store, methods: [{ name: get }] }]
    callback_interfaces:
      - name: Listener
        doc: Receives messages.
        methods:
          - { name: on_message, params: [{ name: text, type: string }, { name: from, type: Store }] }
          - { name: should_stop, return: bool, throws: any }
          - { name: store, return: "Store?" }
          - { name: label, return: string }
    functions:
      - { name: subscribe, params: [{ name: listener, type: Listener }] }
      - { name: maybe_subscribe, params: [{ name: listener, type: "Listener?" }] }
"#,
    );
    let mb = &model.modules[0];
    let cb = &mb.callback_interfaces[0];
    assert_eq!(cb.c_tag, "weaveffi_events_Listener");
    assert_eq!(cb.vtable_tag, "weaveffi_events_Listener_vtable");
    let on_message = &cb.methods[0];
    assert_eq!(
        rendered(&on_message.abi.params),
        [
            "void* ctx",
            "const uint8_t* text_ptr",
            "size_t text_len",
            "weaveffi_events_Store* from",
            "weaveffi_error* out_err"
        ]
    );
    assert_eq!(on_message.abi.ret, CType::Void);
    assert_eq!(on_message.error, ErrorStrategy::Trap);
    assert_eq!(cb.methods[1].abi.ret, CType::Bool);
    assert_eq!(cb.methods[1].error, ErrorStrategy::Untyped);
    assert!(matches!(
        &cb.methods[2].ret_pass,
        CallbackRetPass::Object { nullable: true, clone_symbol, .. } if clone_symbol == "weaveffi_events_Store_clone"
    ));
    assert_eq!(
        slots(cb.methods[3].ret_pass.out_slots()),
        ["uint8_t** out_ptr", "size_t* out_len"]
    );
    assert_eq!(model.callback_interface("Listener").c_tag, cb.c_tag);
    assert!(model.has_callback_interfaces());

    let f = &mb.functions[0];
    assert_eq!(
        f.params[0].ty,
        ParamTy::Callback {
            name: "Listener".into(),
            nullable: false
        }
    );
    assert_eq!(
        rendered(&f.abi.params),
        [
            "void* listener_ctx",
            "const weaveffi_events_Listener_vtable* listener_vtable",
            "weaveffi_error* out_err"
        ]
    );
    assert!(matches!(
        &mb.functions[1].params[0].pass,
        ArgPass::Callback { nullable: true, interface, .. } if interface == "Listener"
    ));
    assert!(!model.has_buffers());
}

#[test]
fn nested_modules_link_parents_and_children() {
    let model = model(
        r#"
modules:
  - name: outer
    doc: Outer module.
    functions: [{ name: outer_fn }]
    modules:
      - name: inner
        functions: [{ name: leaf_fn, doc: Leaf. }]
        modules:
          - name: deepest
      - name: sibling
  - name: other
"#,
    );
    let paths: Vec<&str> = model.modules.iter().map(|m| m.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "outer",
            "outer_inner",
            "outer_inner_deepest",
            "outer_sibling",
            "other"
        ]
    );
    for (i, m) in model.modules.iter().enumerate() {
        assert_eq!(m.index, i);
    }
    let roots: Vec<&str> = model.roots().map(|m| m.name.as_str()).collect();
    assert_eq!(roots, ["outer", "other"]);
    let outer = &model.modules[0];
    assert_eq!(outer.children, [1, 3]);
    let kids: Vec<&str> = model.children(outer).map(|m| m.name.as_str()).collect();
    assert_eq!(kids, ["inner", "sibling"]);
    assert_eq!(model.modules[2].parent, Some(1));
    assert_eq!(
        model.parent(&model.modules[2]).map(|m| m.name.as_str()),
        Some("inner")
    );
    assert!(model.parent(outer).is_none());
    assert_eq!(model.modules[1].dot_path, "outer.inner");
    assert_eq!(
        model.modules[1].functions[0].abi.symbol,
        "weaveffi_outer_inner_leaf_fn"
    );
    assert_eq!(outer.doc.as_deref(), Some("Outer module."));
    assert_eq!(model.modules[1].doc, None);
}

#[test]
fn error_domains_are_global_and_resolved_by_name() {
    let model = model(
        r#"
modules:
  - name: outer
    errors:
      - name: OuterErrors
        codes: [{ name: Bad, code: 1, message: bad }]
      - name: IoError
        codes: [{ name: Disk, code: 7, message: disk, fields: [{ name: path, type: string }] }]
    modules:
      - name: inner
        functions:
          - { name: fails, throws: OuterErrors }
          - { name: reads, throws: IoError }
          - { name: anything, throws: any }
          - { name: never }
  - name: other
    functions: [{ name: elsewhere, throws: IoError }]
"#,
    );
    let outer = &model.modules[0];
    assert_eq!(outer.errors.len(), 2);
    let domain = &outer.errors[0];
    assert_eq!(domain.c_tag, "weaveffi_outer_OuterErrors");
    assert_eq!(domain.type_name, "OuterError");
    assert_eq!(domain.module, "outer");
    assert_eq!(domain.owner_path, "outer");
    assert_eq!(domain.codes[0].c_const, "weaveffi_outer_OuterErrors_Bad");
    let io = model.error_domain("IoError");
    assert_eq!(io.type_name, "IoError");
    assert_eq!(
        io.codes[0].payload_tag(),
        "weaveffi_outer_IoError_Disk_payload"
    );
    assert_eq!(
        function(&model, "fails").error,
        ErrorStrategy::Domain("OuterErrors".into())
    );
    assert_eq!(model.error_domain("OuterErrors"), domain);
    assert_eq!(
        function(&model, "elsewhere").error.domain(),
        Some("IoError")
    );
    assert_eq!(function(&model, "anything").error, ErrorStrategy::Untyped);
    assert!(function(&model, "anything").error.throws());
    assert_eq!(function(&model, "never").error, ErrorStrategy::Trap);
    assert!(!function(&model, "never").error.throws());
    assert_eq!(model.error_domains().count(), 2);
    assert_eq!(model.owner("IoError").name, "outer");
    // An error payload is a value buffer.
    assert!(model.has_buffers());
}

#[test]
fn symbol_table_owners_are_typed() {
    let model = model(
        r#"
modules:
  - name: kv
    errors: [{ name: KvError, codes: [{ name: Gone, code: 1, message: gone, fields: [{ name: k, type: string }] }] }]
    structs: [{ name: Entry, fields: [{ name: k, type: string }] }]
    interfaces: [{ name: Store, methods: [{ name: keys, return: "iter<string>" }] }]
    functions: [{ name: fetch, async: true, return: i32 }]
"#,
    );
    let symbols = model.c_symbols();
    let owner = |name: &str| {
        symbols
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.owner.clone())
            .unwrap_or_else(|| panic!("no symbol {name}"))
    };
    assert_eq!(owner("weaveffi_alloc"), SymbolOwner::Runtime);
    assert_eq!(
        owner("weaveffi_kv_contract"),
        SymbolOwner::ContractTable {
            module: "kv".into()
        }
    );
    assert_eq!(
        owner("weaveffi_kv_fetch_callback"),
        SymbolOwner::AsyncCallback {
            path: "kv.fetch".into()
        }
    );
    assert_eq!(
        owner("weaveffi_kv_Store_KeysIterator_next"),
        SymbolOwner::IteratorNext {
            path: "kv.Store.keys".into()
        }
    );
    assert_eq!(
        owner("weaveffi_kv_KvError_Gone_payload"),
        SymbolOwner::ErrorPayload {
            path: "kv.KvError.Gone".into()
        }
    );
    let codec = owner("weaveffi_kv_Entry_read");
    assert_eq!(
        codec,
        SymbolOwner::Codec {
            codec: "read",
            of: Box::new(SymbolOwner::Record {
                path: "kv.Entry".into()
            })
        }
    );
    assert_eq!(
        codec.to_string(),
        "the value-buffer codec 'read' of record 'kv.Entry'"
    );
    assert_eq!(
        owner("weaveffi_kv_Store_destroy").to_string(),
        "the destructor of 'kv.Store'"
    );
}

#[test]
fn foreign_names_are_explicit_and_recorded() {
    let doc = format!(
        "version: \"{CURRENT_SCHEMA_VERSION}\"\nmodules:\n  - name: orders\n    functions:\n      - {{ name: total, params: [{{ name: p, type: Product }}], return: \"[Product]\" }}\n"
    );
    let api: Api = parse_api_str(&doc, "yaml").unwrap();
    let identity = Identity::named("shop");
    // Without the option, the name is unknown.
    assert!(validate_scoped(&api, &identity, &Options::default()).is_err());
    let options = Options {
        foreign: api.undeclared_type_names(),
    };
    let model = validate_scoped(&api, &identity, &options).unwrap();
    assert!(model.types.is_foreign("Product"));
    assert_eq!(model.types.foreign().collect::<Vec<_>>(), ["Product"]);
    assert!(model.types.get("Product").is_none());
    let total = &model.modules[0].functions[0];
    assert_eq!(
        total.params[0].ty,
        ParamTy::Value(Ty::Record("Product".into()))
    );
    assert!(matches!(total.params[0].pass, ArgPass::Buffer { .. }));
}
