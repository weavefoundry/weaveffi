//! The model's lowering: every declaration's C symbols and signatures.

use super::*;
use crate::abi::AbiParam;
use crate::ir::Api;
use crate::ty::{Prim, RunKind};

fn model(yaml: &str) -> Model {
    let api: Api = serde_saphyr::from_str(yaml).unwrap();
    crate::validate::validate(&api, &Identity::named("kv"), None)
        .unwrap_or_else(|e| panic!("{:?}", e.diagnostics))
}

/// `"{c type} {name}, ..."` of a signature's slots.
fn slots(params: &[AbiParam]) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c("kv"), p.name))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sig(f: &AbiFn) -> String {
    format!(
        "{} {}({})",
        f.ret.render_c("kv"),
        f.symbol,
        slots(&f.params)
    )
}

const KV: &str = r#"
version: "0.13.0"
errors:
  - name: KvError
    codes:
      - { name: NotFound, code: 1, message: missing, fields: [{ name: key, type: string }] }
records:
  - name: Entry
    fields: [{ name: key, type: string }, { name: value, type: bytes }]
enums:
  - name: Kind
    variants: [{ name: Hot }, { name: Cold }]
  - name: Shape
    variants: [{ name: Dot }, { name: Circle, fields: [{ name: r, type: f64 }] }]
interfaces:
  - name: Store
    constructors: [{ name: open, params: [{ name: path, type: string }], throws: KvError }]
    methods:
      - { name: get, params: [{ name: key, type: string }], return: Entry, throws: KvError }
      - { name: ttl, params: [{ name: limit, type: "u32?" }, { name: kind, type: Kind }], return: "i64?" }
      - { name: keys, return: "iter<string>" }
      - { name: sizes, params: [{ name: weights, type: "[f64]" }], return: "[u64]" }
      - { name: mirror, params: [{ name: other, type: "Store?" }], return: Store }
callbacks:
  - name: Listener
    methods:
      - { name: on_put, params: [{ name: entry, type: Entry }, { name: store, type: Store }], return: "bool?", throws: KvError }
      - { name: on_clear }
functions:
  - { name: version, return: string }
  - { name: fetch, params: [{ name: key, type: string }], return: "i64?", async: true, cancellable: true }
  - { name: flush, async: true }
  - { name: watch, params: [{ name: l, type: "Listener?" }], return: "iter<Store?>" }
"#;

#[test]
fn every_callable_lowers_to_one_slot_per_value() {
    let m = model(KV);
    let store = m.interface("Store");
    let sigs: Vec<String> = store.members().map(|f| sig(&f.abi)).collect();
    assert_eq!(
        sigs,
        [
            "kv_Store* kv_Store_open(kv_run path, kv_error* out_err)",
            "kv_run kv_Store_get(const kv_Store* self, kv_run key, kv_error* out_err)",
            "kv_opt_i64 kv_Store_ttl(const kv_Store* self, kv_opt_u32 limit, kv_Kind kind, kv_error* out_err)",
            "kv_Store_KeysIterator* kv_Store_keys(const kv_Store* self, kv_error* out_err)",
            "kv_run kv_Store_sizes(const kv_Store* self, kv_run weights, kv_error* out_err)",
            "kv_Store* kv_Store_mirror(const kv_Store* self, const kv_Store* other, kv_error* out_err)",
        ]
    );
    assert_eq!(store.clone_symbol, "kv_Store_clone");
    assert_eq!(store.destroy_symbol, "kv_Store_destroy");
    let get = &store.methods[0];
    assert_eq!(get.path, "Store.get");
    assert_eq!(get.kind, CallableKind::Method);
    assert_eq!(get.error, ErrorStrategy::Domain("KvError".into()));
    assert_eq!(
        store.constructors[0].ret,
        Some(RetTy::Value(Ty::Interface("Store".into())))
    );

    let keys = store.methods[2].iterator().unwrap();
    assert_eq!(keys.iter_tag, "kv_Store_KeysIterator");
    assert_eq!(
        sig(&keys.next),
        "int32_t kv_Store_KeysIterator_next(kv_Store_KeysIterator* iter, kv_run* out_item, kv_error* out_err)"
    );
    assert_eq!(keys.destroy_symbol, "kv_Store_KeysIterator_destroy");

    let watch = m.functions[3].iterator().unwrap();
    assert_eq!(
        sig(&watch.next),
        "int32_t kv_WatchIterator_next(kv_WatchIterator* iter, kv_Store** out_item, kv_error* out_err)"
    );
    assert_eq!(
        sig(&m.functions[3].abi),
        "kv_WatchIterator* kv_watch(kv_Listener l, kv_error* out_err)"
    );
}

#[test]
fn async_callables_launch_and_complete_with_one_result_slot() {
    let m = model(KV);
    let fetch = &m.functions[1];
    assert!(fetch.is_async() && fetch.cancellable());
    assert_eq!(
        sig(&fetch.abi),
        "void kv_fetch(kv_run key, kv_cancel_token* cancel_token, kv_fetch_callback callback, void* context)"
    );
    let done = fetch.async_binding().unwrap();
    assert_eq!(done.callback_type, "kv_fetch_callback");
    assert_eq!(
        slots(&done.callback.params),
        "void* context, kv_error* err, kv_opt_i64 result"
    );
    let flush = m.functions[2].async_binding().unwrap();
    assert_eq!(
        slots(&flush.callback.params),
        "void* context, kv_error* err"
    );
    assert_eq!(
        fetch.ret_value(),
        Some(&Ty::Optional(Box::new(Ty::Prim(Prim::I64))))
    );
}

#[test]
fn callback_methods_are_vtable_entries() {
    let m = model(KV);
    let l = m.callback("Listener");
    assert_eq!(l.c_tag, "kv_Listener");
    assert_eq!(l.vtable_tag, "kv_Listener_vtable");
    let on_put = &l.methods[0];
    assert_eq!(on_put.slot, 0);
    assert_eq!(
        sig(&on_put.abi),
        "kv_opt_bool on_put(void* ctx, kv_run entry, kv_Store* store, kv_error* out_err)"
    );
    assert_eq!(
        sig(&l.methods[1].abi),
        "void on_clear(void* ctx, kv_error* out_err)"
    );
    assert_eq!(l.methods[1].slot, 1);
}

#[test]
fn user_types_resolve_and_get_flat_c_names() {
    let m = model(KV);
    assert_eq!(m.record("Entry").c_tag, "kv_Entry");
    let kind = m.enumeration("Kind");
    assert!(!kind.is_rich());
    assert_eq!(kind.variants[1].c_const, "kv_Kind_Cold");
    assert_eq!(kind.variants[1].value, 1);
    assert!(m.enumeration("Shape").is_rich());
    let domain = m.error_domain("KvError");
    assert_eq!(domain.c_tag, "kv_KvError");
    assert_eq!(domain.codes[0].c_const, "kv_KvError_NotFound");
    assert_eq!(domain.codes[0].payload_tag(), "kv_KvError_NotFound_payload");
    assert_eq!(
        m.interface("Store").methods[3].params[0].ty,
        ParamTy::Value(Ty::List(Box::new(Ty::Prim(Prim::F64))))
    );
    assert_eq!(
        Ty::List(Box::new(Ty::Prim(Prim::F64))).family(),
        crate::ty::Family::Run(RunKind::Slice(Prim::F64))
    );
    assert!(m.has_async() && m.has_iterators() && m.has_cancellable() && m.has_buffers());
}

#[test]
fn symbol_table_owners_are_typed() {
    let m = model(KV);
    let symbols = m.c_symbols();
    let owner = |name: &str| {
        symbols
            .iter()
            .find(|s| s.name == name)
            .map(|s| s.owner.to_string())
            .unwrap_or_else(|| panic!("{name} missing"))
    };
    assert_eq!(owner("kv_abi_version"), "the runtime surface");
    assert_eq!(owner("kv_Store_get"), "function 'Store.get'");
    assert_eq!(owner("kv_fetch_callback"), "the completion type of 'fetch'");
    assert_eq!(
        owner("kv_Store_KeysIterator_next"),
        "the iterator step of 'Store.keys'"
    );
    assert_eq!(
        owner("kv_Entry_decode"),
        "the value-buffer codec 'decode' of record 'Entry'"
    );
    assert_eq!(owner("kv_Shape_Tag"), "the tag type of enum 'Shape'");
    assert_eq!(owner("kv_Listener"), "callback interface 'Listener'");
    assert_eq!(
        owner("kv_Listener_vtable"),
        "the vtable of callback interface 'Listener'"
    );
    assert_eq!(
        owner("kv_KvError_NotFound"),
        "error code 'KvError.NotFound'"
    );
}
