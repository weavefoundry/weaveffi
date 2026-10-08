//! Unit tests: render a small API that exercises every C ABI revision 4
//! shape the Go bindings distinguish and assert the key pieces of each
//! contract.

use weaveffi_model::contract::entries;
use weaveffi_model::ir::Api;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use super::{render_files, GoConfig};

/// One module with an error domain carrying a field; a C-style enum; a
/// record carrying objects; an interface with a `new` constructor, a
/// method, and a throwing static; nullable objects; an iterator of objects;
/// a cancellable async function; a composite map; and a callback interface
/// whose methods take every argument family and return a direct value, an
/// enum, a string, an optional record, an object, and an optional object,
/// one of them `throws`, passed both required and optional.
const FIXTURE: &str = r#"
version: "0.11.0"
modules:
  - name: shop
    errors:
      name: ShopError
      codes:
        - { name: OutOfStock, code: 1, message: "out of stock", fields: [{ name: sku, type: string }] }
        - { name: Closed, code: 2, message: "closed" }
    enums:
      - name: Mood
        variants: [{ name: Happy, value: 0 }, { name: Grumpy, value: 1 }]
    structs:
      - name: Order
        fields:
          - { name: order_id, type: u64 }
          - { name: cart, type: Cart }
          - { name: history, type: "[Cart]" }
          - { name: note, type: "string?" }
    interfaces:
      - name: Cart
        constructors:
          - { name: new, params: [{ name: owner, type: string }] }
        methods:
          - { name: add, params: [{ name: sku, type: string }], return: bool }
        statics:
          - { name: restore, params: [{ name: id, type: i64 }], return: Cart, throws: true }
    callback_interfaces:
      - name: Watcher
        methods:
          - name: on_event
            params:
              - { name: name, type: string }
              - { name: count, type: i32 }
              - { name: order, type: Order }
              - { name: cart, type: Cart }
            return: bool
          - { name: mood, return: Mood }
          - { name: label, return: string, throws: true }
          - { name: latest, return: "Order?" }
          - { name: favorite, return: Cart }
          - { name: maybe, return: "Cart?" }
          - { name: on_done }
    functions:
      - { name: find_cart, params: [{ name: current, type: "Cart?" }], return: "Cart?" }
      - { name: all_carts, return: "iter<Cart>" }
      - { name: watch, params: [{ name: watcher, type: Watcher }] }
      - { name: maybe_watch, params: [{ name: watcher, type: "Watcher?" }] }
      - { name: echo, params: [{ name: text, type: string }], return: string }
      - { name: tally, params: [{ name: counts, type: "{string:[i32?]}" }], return: "[Order]", throws: true }
      - { name: wait, params: [{ name: ms, type: i64 }], return: i64, async: true, cancellable: true }
"#;

fn model() -> Model {
    let api: Api = serde_yaml::from_str(FIXTURE).unwrap();
    validate(&api, &Identity::named("shop_kit"), None).unwrap()
}

fn files() -> Vec<(String, String)> {
    render_files(&model(), &GoConfig::default())
}

fn file(name: &str) -> String {
    files()
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, c)| c)
        .unwrap_or_else(|| panic!("no {name}"))
}

fn bindings() -> String {
    file("bindings.go")
}

#[track_caller]
fn assert_has(out: &str, needle: &str) {
    assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
}

#[test]
fn names_come_from_the_identity() {
    let files = files();
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "go.mod",
            "README.md",
            "shop_kit.h",
            "bindings.go",
            "runtime.go",
            "codec.go"
        ]
    );
    assert_has(&files[0].1, "module shop_kit\n");
    let src = &files[3].1;
    assert_has(
        src,
        "// Package shop_kit binds the shop_kit native library.\npackage shop_kit\n",
    );
    assert_has(src, "#cgo LDFLAGS: -lshop_kit\n");
    assert_has(src, "#include \"shop_kit.h\"\n");
    for (name, contents) in &files {
        if name.ends_with(".go") || name == "go.mod" {
            let body: String = contents.lines().skip(3).collect::<Vec<_>>().join("\n");
            assert!(
                !body.to_lowercase().contains("weaveffi"),
                "{name} is branded:\n{body}"
            );
            assert!(!body.contains("{{"), "{name} has a placeholder:\n{body}");
        }
    }

    let custom = render_files(
        &model(),
        &GoConfig {
            name: Some("example.com/shop".into()),
        },
    );
    assert_has(&custom[0].1, "module example.com/shop\n");
}

#[test]
fn load_checks_embed_every_contract_entry() {
    let model = model();
    let src = bindings();
    assert_has(
        &src,
        "func init() {\n\twvCheckABI()\n\tvar n C.size_t\n\ttable := C.shop_kit_shop_contract(&n)\n\twvCheckContract(table, n, []wvContractEntry{\n",
    );
    let root = model.roots().next().unwrap();
    for e in entries(&model, root) {
        assert_has(
            &src,
            &format!(
                "\t\t{{{:#018x}, {:#018x}, \"{}\"}},\n",
                e.id, e.hash, e.path
            ),
        );
    }
    let rt = file("runtime.go");
    assert_has(&rt, "const wvABIVersion uint32 = 4\n");
    assert_has(&rt, "%s is missing from the library");
    assert_has(&rt, "%s changed since these bindings were generated");
    for f in files() {
        assert!(!f.1.contains("checksum"), "{} mentions a checksum", f.0);
        assert!(!f.1.contains("_dealloc"), "{} mentions dealloc", f.0);
    }
}

#[test]
fn vtables_carry_the_header_and_every_method() {
    let src = bindings();
    assert_has(
        &src,
        "static const shop_kit_shop_Watcher_vtable wvVtable_shop_kit_shop_Watcher_vtable = {\n\t.size = sizeof(shop_kit_shop_Watcher_vtable),\n\t.flags = 0,\n\t.free = goWv_shop_kit_shop_Watcher_free,\n",
    );
    assert_has(
        &src,
        "\t.on_event = (bool (*)(void*, const uint8_t*, size_t, int32_t, const uint8_t*, size_t, shop_kit_shop_Cart*, shop_kit_error*))goWv_shop_kit_shop_Watcher_on_event,\n",
    );
    assert_has(&src, "\t.label = goWv_shop_kit_shop_Watcher_label,\n");
    assert_has(
        &src,
        "extern void goWv_shop_kit_shop_Watcher_label(void* ctx, uint8_t** out_ptr, size_t* out_len, shop_kit_error* out_err);",
    );
    assert_has(
        &src,
        "extern shop_kit_shop_Cart* goWv_shop_kit_shop_Watcher_favorite(void* ctx, shop_kit_error* out_err);",
    );
}

#[test]
fn callback_methods_return_every_family() {
    let src = bindings();
    assert_has(
        &src,
        "\tOnEvent(name string, count int32, order Order, cart *Cart) bool\n",
    );
    assert_has(&src, "\tMood() Mood\n");
    assert_has(&src, "\tLabel() (string, error)\n");
    assert_has(&src, "\tLatest() *Order\n");
    assert_has(&src, "\tFavorite() *Cart\n");
    assert_has(&src, "\tMaybe() *Cart\n");
    assert_has(&src, "\tOnDone()\n");

    // Arguments: borrowed runs are copied or decoded; an object is adopted.
    assert_has(
        &src,
        "ret := wvCallback[Watcher](ctx).OnEvent(wvBorrowString(name_ptr, name_len), int32(count), wvDecodeBorrowed(order_ptr, order_len, wvReadOrder), wvAdoptCart(cart))\n\treturn C._Bool(ret)\n",
    );
    // Direct values return by value, objects as a fresh reference, and runs
    // through the out slots as a library allocation.
    assert_has(&src, "(cRet C.shop_kit_shop_Mood) {");
    assert_has(&src, "return C.shop_kit_shop_Mood(ret)");
    assert_has(&src, "wvHandOverString(ret, out_ptr, out_len)");
    assert_has(
        &src,
        "wvHandOverBytes(wvEncode(ret, wvWriteOptOrder), out_ptr, out_len)",
    );
    assert_has(&src, "(cRet *C.shop_kit_shop_Cart) {");
    assert_has(&src, "return ret.share()");
    let rt = file("runtime.go");
    assert_has(&rt, "run := C.shop_kit_alloc(n)");
    assert_has(&src, "defer wvRecoverCallback(out_err)");
}

#[test]
fn throwing_callbacks_report_domain_codes_and_payloads() {
    let src = bindings();
    assert_has(
        &src,
        "ret, err := wvCallback[Watcher](ctx).Label()\n\tif err != nil {\n\t\twvCallbackFailed[ShopError](out_err, err)\n\t\treturn\n\t}\n",
    );
    assert_has(
        &src,
        "func (e *OutOfStockError) wvPayload() []byte {\n\tw := &wvWriter{}\n\tw.writeString(e.Sku)\n\treturn w.buf\n}",
    );
    let rt = file("runtime.go");
    assert_has(&rt, "C.shop_kit_error_set_payload(outErr, ptr, n)");
    assert_has(&rt, "wvSetError(outErr, wvCodeForeign, err.Error(), nil)");
}

#[test]
fn callback_parameters_pass_a_handle_and_the_static_vtable() {
    let src = bindings();
    assert_has(
        &src,
        "C.shop_kit_shop_watch(wvNewCallback(watcher), C.wvVtablePtr_shop_kit_shop_Watcher_vtable(), &cErr)",
    );
    // An optional callback passes a null vtable for nil.
    assert_has(
        &src,
        "\tvar cWatcherCtx unsafe.Pointer\n\tvar cWatcherVtable *C.shop_kit_shop_Watcher_vtable\n\tif watcher != nil {\n\t\tcWatcherCtx, cWatcherVtable = wvNewCallback(watcher), C.wvVtablePtr_shop_kit_shop_Watcher_vtable()\n\t}\n",
    );
    assert_has(
        &src,
        "func goWv_shop_kit_shop_Watcher_free(ctx unsafe.Pointer) {\n\twvFreeCallback(ctx)\n}",
    );
}

#[test]
fn errors_map_to_code_types() {
    let src = bindings();
    assert_has(
        &src,
        "type ShopError interface {\n\terror\n\t// Code returns the error's numeric code.\n\tCode() int32\n\tisShopError()\n}",
    );
    assert_has(&src, "type OutOfStockError struct {\n\tSku string\n");
    assert_has(&src, "func (*ClosedError) Code() int32 {\n\treturn 2\n}");
    assert_has(&src, "func (*ClosedError) isShopError() {}");
    assert_has(&src, "func wvShopError(f wvFailure) error {");
    assert_has(&src, "\t\te.Sku = r.readString()\n");
    // A throwing call returns the mapped error; any other call traps.
    assert_has(&src, "func CartRestore(id int64) (*Cart, error) {");
    assert_has(&src, "return nil, wvShopError(wvTakeError(&cErr))");
    assert_has(&src, "func Echo(text string) string {");
    assert_has(
        &src,
        "\twvTrap(&cErr)\n\treturn wvTakeString(cRet, cRetLen)\n",
    );
}

#[test]
fn composite_codecs_are_rendered_once_per_type() {
    let src = bindings();
    for stem in [
        "MapStringListOptI32",
        "ListOptI32",
        "OptI32",
        "ListOrder",
        "ListCart",
        "OptString",
        "OptOrder",
    ] {
        let def = format!("func wvWrite{stem}(w *wvWriter");
        assert_eq!(src.matches(&def).count(), 1, "{def}");
        assert_eq!(
            src.matches(&format!("func wvRead{stem}(r *wvReader)"))
                .count(),
            1,
            "{stem}"
        );
    }
    assert_has(
        &src,
        "func wvWriteMapStringListOptI32(w *wvWriter, v map[string][]*int32) {\n\twvWriteMap(w, v, (*wvWriter).writeString, wvWriteListOptI32)\n}",
    );
    assert_has(
        &src,
        "func wvReadOptOrder(r *wvReader) *Order {\n\treturn wvReadOptional(r, wvReadOrder)\n}",
    );
    assert_has(
        &src,
        "cCountsPtr, cCountsLen := wvBytes(wvEncode(counts, wvWriteMapStringListOptI32))",
    );
    assert_has(&src, "return wvDecode(cRet, cRetLen, wvReadListOrder), nil");
    // Records read and write their fields through the shared pairs.
    assert_has(&src, "\tOrderID uint64\n");
    assert_has(
        &src,
        "\twvWriteListCart(w, v.History)\n\twvWriteOptString(w, v.Note)\n",
    );
}

#[test]
fn objects_cross_as_borrowed_pointers_and_fresh_tokens() {
    let src = bindings();
    assert_has(
        &src,
        "\tvar cCurrent *C.shop_kit_shop_Cart\n\tif current != nil {\n\t\tcCurrent = current.native()\n\t\tdefer current.ref.release()\n\t}\n",
    );
    assert_has(&src, "return wvAdoptCart(cRet)");
    assert_has(
        &src,
        "func wvWriteCart(w *wvWriter, v *Cart) {\n\tw.writeU64(uint64(uintptr(unsafe.Pointer(v.share()))))\n}",
    );
    assert_has(&src, "return C.shop_kit_shop_Cart_clone(ptr)");
    assert_has(&src, "runtime.SetFinalizer(s, (*Cart).Close)");
}

#[test]
fn async_calls_block_on_a_context() {
    let src = bindings();
    assert_has(
        &src,
        "func Wait(ctx context.Context, ms int64) (int64, error) {",
    );
    assert_has(&src, "token := C.shop_kit_cancel_token_create()");
    assert_has(&src, "defer C.shop_kit_cancel_token_destroy(token)");
    assert_has(&src, "call := wvNewAsyncCall[int64]()");
    assert_has(&src, "res, fail, err := call.wait(ctx, token)");
    assert_has(
        &src,
        "wvComplete(context_, err_, func() int64 {\n\t\treturn int64(result)\n\t})",
    );
    // A non-throwing async failure is a bug: it panics.
    assert_has(&src, "\t\tpanic(fail.err())\n");
}

#[test]
fn iterators_are_lazy_sequences() {
    let src = bindings();
    assert_has(&src, "func AllCarts() iter.Seq[*Cart] {");
    assert_has(
        &src,
        "defer C.shop_kit_shop_AllCartsIterator_destroy(cIter)",
    );
    assert_has(&src, "if !yield(wvAdoptCart(cItem)) {");
}

#[test]
fn output_is_deterministic() {
    assert_eq!(files(), files());
}
