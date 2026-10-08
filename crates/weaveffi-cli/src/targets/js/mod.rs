//! The JavaScript layer shared by the [`node`](crate::targets::node) and
//! [`wasm`](crate::targets::wasm) targets.
//!
//! Both targets generate the same ES module: one exported namespace object
//! per top-level IDL module (`export const kv = { Store, get, ... }`, with
//! nested modules as nested namespaces), the same error classes (a root
//! `{Package}Error`, one `{Domain}Error` per error domain, one `{Code}Error`
//! per code, and `CancelledError`), records as plain objects, rich enums as
//! tagged unions, interfaces as classes with `close()`, `[Symbol.dispose]`,
//! and a `FinalizationRegistry` backstop, lazy iterators, `Promise`-returning
//! async functions with `AbortSignal` cancellation, and the same `.d.ts`.
//! Only the transport differs: Node.js calls an N-API addon, WebAssembly
//! stages values in linear memory. Both expose the raw calling convention
//! documented in [`api`] as an object named `$raw`.
//!
//! The fixed runtime (codec, error classes, object and iterator wrappers,
//! cancellation, the load-time checks) is a real JavaScript file,
//! `runtime/runtime.js`, emitted next to `index.js`.

mod api;
mod codec;
mod dts;
pub(crate) mod names;

use weaveffi_model::contract;
use weaveffi_model::model::{Model, ABI_VERSION};
use weaveffi_model::pkg::Identity;

use crate::codegen::CodeWriter;
use crate::manifest::{JsonObject, JsonValue};
use crate::utils::{render_prelude, render_trailer, CommentStyle};

pub(crate) use api::render_api;

/// The shared runtime, with `{{ERROR_CLASS}}` naming the root error class.
const RUNTIME_JS: &str = include_str!("runtime/runtime.js");

/// The runtime helpers the generated API may reference.
const RUNTIME_IMPORTS: &[&str] = &[
    "$Fault",
    "$fault",
    "$domain",
    "$raise",
    "$W",
    "$WK",
    "$R",
    "$encode",
    "$decode",
    "$Object",
    "$own",
    "$adopt",
    "$adoptOpt",
    "$lend",
    "$lendOpt",
    "$unlend",
    "$clone",
    "$cloneOpt",
    "$Iterator",
    "$cancellable",
    "$impl",
    "$ret",
    "$verify",
];

/// The root error class of a package: `{PascalName}Error` (`KvstoreError`).
pub(crate) fn root_error_class(identity: &Identity) -> String {
    weaveffi_model::errors::type_name(&identity.pascal_name(), "Error")
}

/// Render `runtime.js`.
pub(crate) fn render_runtime(identity: &Identity) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.raw(RUNTIME_JS.replace("{{ERROR_CLASS}}", &root_error_class(identity)));
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, "runtime.js"));
    w.finish()
}

/// The import and re-export lines every `index.js` starts with.
pub(crate) fn render_imports(w: &mut CodeWriter, identity: &Identity) {
    let root = root_error_class(identity);
    w.line("import {");
    w.scope(|w| {
        for name in RUNTIME_IMPORTS {
            w.line(format!("{name},"));
        }
        w.line(format!("{root} as $Error,"));
    });
    w.line("} from './runtime.js';");
    w.line(format!(
        "export {{ {root}, CancelledError }} from './runtime.js';"
    ));
}

/// The contract these bindings were generated with (`const $contract`):
/// for every top-level module, each declaration's `[id, hash, path]` from
/// [`contract::entries`], which the load-time check looks up in the
/// library's own tables.
pub(crate) fn render_contract(w: &mut CodeWriter, model: &Model) {
    w.line("// The declarations these bindings were generated with, per top-level");
    w.line("// module, as [id, hash, path]: the library's contract tables must hold");
    w.line("// each one (see $verify).");
    w.block("const $contract = [", "];", |w| {
        for root in model.roots() {
            w.block(
                format!("[{}, [", names::js_string(&root.name)),
                "]],",
                |w| {
                    for e in contract::entries(model, root) {
                        w.line(format!(
                            "[0x{:016x}n, 0x{:016x}n, {}],",
                            e.id,
                            e.hash,
                            names::js_string(&e.path)
                        ));
                    }
                },
            );
        }
    });
}

/// The load-time check of the ABI revision and of `$contract` (see
/// [`render_contract`]) against the library behind `$raw`.
pub(crate) fn verify_call(model: &Model, identity: &Identity) -> String {
    format!(
        "$verify($raw, {}, '{}', {ABI_VERSION}, $contract);",
        names::js_string(&identity.name),
        model.prefix(),
    )
}

/// Render `index.d.ts`. `extra` carries the transport's own declarations.
pub(crate) fn render_dts(model: &Model, identity: &Identity, extra: &str) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    dts::render_declarations(&mut w, model, &root_error_class(identity), extra);
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, "index.d.ts"));
    w.finish()
}

/// The npm metadata both `package.json` files share: the generated-file
/// notice, then name, version, description, and the optional fields.
pub(crate) fn npm_metadata(identity: &Identity, name: &str) -> JsonObject {
    let version = env!("CARGO_PKG_VERSION");
    let mut obj = JsonObject::new()
        .str_entry("//", format!("Generated by WeaveFFI {version}"))
        .str_entry(
            "//warning",
            "DO NOT EDIT. Your changes will be overwritten.",
        )
        .str_entry("//regenerate", "To regenerate: weaveffi generate")
        .str_entry("name", name)
        .str_entry("version", &identity.version)
        .str_entry("description", identity.description_or_default())
        .opt_str_entry("license", identity.license.as_deref());
    if let Some(author) = identity.authors.first() {
        obj = obj.str_entry("author", author);
    }
    obj = obj.opt_str_entry("homepage", identity.homepage.as_deref());
    if let Some(repository) = &identity.repository {
        obj = obj.entry(
            "repository",
            JsonValue::Object(
                JsonObject::new()
                    .str_entry("type", "git")
                    .str_entry("url", repository),
            ),
        );
    }
    obj.str_entry("type", "module")
        .str_entry("main", "index.js")
        .str_entry("types", "index.d.ts")
        .entry(
            "exports",
            JsonValue::Object(
                JsonObject::new().entry(
                    ".",
                    JsonValue::Object(
                        JsonObject::new()
                            .str_entry("types", "./index.d.ts")
                            .str_entry("default", "./index.js"),
                    ),
                ),
            ),
        )
}

/// A small API exercising every shape both JavaScript targets render, for
/// their unit tests: an error domain with a payload, a C-style and a rich
/// enum, a record holding objects, two callback interfaces (one with a
/// return of every family and a method that throws), an interface with a
/// canonical constructor, a method named `close`, a throwing method, an
/// iterator of objects, a nullable async result, and a cancellable async
/// method, free functions (one named after a keyword, one taking an
/// optional callback), and a nested module.
#[cfg(test)]
pub(crate) fn test_api(name: &str) -> weaveffi_model::model::Model {
    const YAML: &str = r#"
version: "0.11.0"
modules:
  - name: kv
    doc: A key-value store.
    errors:
      name: KvError
      codes:
        - { name: KEY_NOT_FOUND, code: 1, message: "key not found", fields: [{ name: key, type: string }] }
        - { name: FULL, code: 2, message: "it's full" }
    enums:
      - name: Mode
        variants:
          - { name: Fast, value: 0 }
          - { name: Safe, value: 7 }
          - { name: Off, value: -1 }
      - name: Shape
        variants:
          - { name: Empty, value: 0 }
          - { name: Circle, value: 1, fields: [{ name: radius, type: f64 }] }
    structs:
      - name: Bundle
        fields:
          - { name: primary, type: Store }
          - { name: extras, type: "[Store]" }
          - { name: stamp, type: i64 }
          - { name: by_mode, type: "{Mode:bool}" }
    callback_interfaces:
      - name: Listener
        methods:
          - name: on_message
            params: [{ name: text, type: string }, { name: weight, type: u64 }]
            return: bool
          - name: on_bundle
            params: [{ name: bundle, type: Bundle }, { name: store, type: Store }, { name: alt, type: "Store?" }]
      - name: Policy
        methods:
          - { name: admit, params: [{ name: bundle, type: Bundle }], return: Bundle, throws: true }
          - { name: label, params: [], return: string }
          - { name: blob, params: [], return: bytes }
          - { name: pick, params: [{ name: key, type: string }], return: Store }
          - { name: maybe, params: [], return: "Store?" }
          - { name: mode, params: [], return: Mode }
          - { name: weight, params: [], return: i64 }
    interfaces:
      - name: Store
        doc: A store.
        constructors:
          - { name: new, params: [{ name: path, type: string }] }
        methods:
          - { name: get, params: [{ name: key, type: string }], return: bytes, throws: true }
          - { name: close, params: [] }
          - { name: scan, params: [], return: "iter<Store>" }
          - { name: fetch, params: [{ name: path, type: string }], return: "Store?", async: true }
          - { name: wait, params: [{ name: ms, type: i64 }], return: i64, async: true, cancellable: true }
    functions:
      - { name: subscribe, params: [{ name: listener, type: Listener }, { name: mode, type: Mode }] }
      - { name: install, params: [{ name: policy, type: "Policy?" }] }
      - { name: widen, params: [{ name: n, type: i64 }, { name: m, type: u64 }], return: u64 }
      - { name: delete, params: [{ name: class, type: "Store?" }], return: bool }
      - { name: shapes, params: [], return: "iter<Shape>" }
    modules:
      - name: stats
        functions:
          - { name: count, params: [{ name: store, type: Store }], return: i64, throws: true }
"#;
    let api = weaveffi_model::parse::parse_api_str(YAML, "yaml").expect("test fixture parses");
    weaveffi_model::validate::validate(&api, &Identity::named(name), None)
        .expect("test fixture validates")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_names_the_package_root_error() {
        let rt = render_runtime(&Identity::named("my-kv"));
        assert!(
            rt.contains("export class MyKvError extends Error {"),
            "{rt}"
        );
        assert!(rt.contains("export class CancelledError extends MyKvError {"));
        assert!(!rt.contains("{{"));
        assert!(!rt.contains("checksum"));
        assert_eq!(root_error_class(&Identity::named("io_error")), "IoError");
    }

    fn api_js() -> String {
        let mut w = CodeWriter::two_space();
        render_api(&mut w, &test_api("acme-kv"));
        w.finish()
    }

    fn has(src: &str, needle: &str) {
        assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
    }

    #[test]
    fn the_contract_lists_every_declaration_with_its_path() {
        let model = test_api("acme-kv");
        let mut w = CodeWriter::two_space();
        render_contract(&mut w, &model);
        let js = w.finish();
        let root = model.roots().next().unwrap();
        let entries = contract::entries(&model, root);
        assert_eq!(js.matches("n, 0x").count(), entries.len());
        let put = entries.iter().find(|e| e.path == "kv.Store.get").unwrap();
        has(&js, "const $contract = [\n  ['kv', [\n");
        has(
            &js,
            &format!(
                "    [0x{:016x}n, 0x{:016x}n, 'kv.Store.get'],\n",
                put.id, put.hash
            ),
        );
        assert_eq!(
            verify_call(&model, &model.identity),
            "$verify($raw, 'acme-kv', 'acme_kv', 4, $contract);"
        );
    }

    #[test]
    fn modules_are_frozen_namespaces_with_escaped_members() {
        let js = api_js();
        has(&js, "export const kv = Object.freeze({");
        has(&js, "  Store: kv$Store,\n");
        has(&js, "  delete_: kv$delete_,\n");
        has(
            &js,
            "  stats: Object.freeze({\n    count: kv$stats$count,\n  }),\n});",
        );
        has(&js, "const kv$delete_ = function delete_(class_) {");
        has(&js, "  close_() {");
        has(&js, "  7: 'Safe',\n  '-1': 'Off',\n");
    }

    #[test]
    fn calls_lend_objects_and_map_faults_by_strategy() {
        let js = api_js();
        has(
            &js,
            "  get(key) {\n    const $self = $lend(this, kv$Store);\n    try {\n      return $raw.acme_kv_kv_Store_get($self, key);\n    } catch ($e) {\n      throw $from$kv$KvError($e);\n    } finally {\n      $unlend(this);\n    }\n  }",
        );
        has(&js, "const $o0 = $lendOpt(class_, kv$Store);");
        has(&js, "throw $fault($e);");
        has(
            &js,
            "return $domain(e, $codes$kv$KvError, $payloads$kv$KvError);",
        );
        has(
            &js,
            "const $payloads$kv$KvError = new Map([\n  [1, (r) => ({ key: r.readString() })],\n]);",
        );
    }

    #[test]
    fn error_codes_with_fields_take_them_first() {
        let js = api_js();
        has(
            &js,
            "  constructor(fields, message = 'key not found') {\n    super(1, message);\n    this.key = fields.key;\n  }",
        );
        has(
            &js,
            "  constructor(message = 'it\\'s full') {\n    super(2, message);\n  }",
        );
        has(
            &js,
            "const $fields$kv$KvError = new Map([\n  [1, (w, e) => { w.writeString(e.key); }],\n]);",
        );
        has(
            &js,
            "return $raise(e, kv$KvError, $codes$kv$KvError, $fields$kv$KvError);",
        );
    }

    #[test]
    fn async_and_iterators_follow_the_raw_convention() {
        let js = api_js();
        has(
            &js,
            "return await $cancellable($options?.signal, $tokens, ($t) => $raw.acme_kv_kv_Store_wait($self, ms, $t));",
        );
        has(
            &js,
            "return $adoptOpt(kv$Store, await $raw.acme_kv_kv_Store_fetch($self, path));",
        );
        has(
            &js,
            "return new $Iterator($raw.acme_kv_kv_Store_scan($self), $it$kv$Store$scan);",
        );
        has(&js, "convert: (v) => $adopt(kv$Store, v),");
        has(&js, "convert: (v) => $decode(v, $r$kv$Shape),");
    }

    #[test]
    fn adapters_convert_arguments_and_every_return_family() {
        let js = api_js();
        has(
            &js,
            "return $ret.Bool(impl.onMessage($a0, $a1), 'Listener.onMessage');",
        );
        has(&js, "impl.onBundle($decode($a0, $r$kv$Bundle), $adopt(kv$Store, $a1), $adoptOpt(kv$Store, $a2));");
        has(
            &js,
            "    admit($a0) {\n      try {\n        return $encode(impl.admit($decode($a0, $r$kv$Bundle)), $w$kv$Bundle);\n      } catch ($e) {\n        throw $raise$kv$KvError($e);\n      }\n    },",
        );
        has(&js, "return $ret.String(impl.label(), 'Policy.label');");
        has(&js, "return $ret.Bytes(impl.blob(), 'Policy.blob');");
        has(&js, "return $clone(impl.pick($a0), kv$Store);");
        has(&js, "return $cloneOpt(impl.maybe(), kv$Store);");
        has(&js, "return $ret.I32(impl.mode(), 'Policy.mode');");
        has(&js, "return $ret.I64(impl.weight(), 'Policy.weight');");
        has(
            &js,
            "$raw.acme_kv_kv_subscribe($adapt$kv$Listener(listener), mode);",
        );
        has(
            &js,
            "$raw.acme_kv_kv_install(policy == null ? null : $adapt$kv$Policy(policy));",
        );
    }

    #[test]
    fn codecs_name_one_function_per_type() {
        let js = api_js();
        has(&js, "  $w$kv$Store(w, v.primary);\n");
        has(&js, "    primary: $r$kv$Store(r),\n");
        has(&js, "  $w_list_Store(w, v.extras);\n");
        has(
            &js,
            "function $w_map_Mode_bool(w, v) {\n  w.writeMap(v, $WK.I32, $W.Bool);\n}",
        );
        has(
            &js,
            "function $w$kv$Store(w, v) {\n  w.writeU64(BigInt($clone(v, kv$Store)));\n}",
        );
        has(
            &js,
            "function $r$kv$Store(r) {\n  return $adopt(kv$Store, $token(r.readU64()));\n}",
        );
    }

    #[test]
    fn declarations_qualify_types_and_alias_the_root_error() {
        let model = test_api("kv");
        let dts = render_dts(&model, &Identity::named("kv"), "");
        has(&dts, "export declare class KvError extends Error {");
        has(&dts, "declare const $Error: typeof KvError;");
        has(&dts, "export declare namespace kv {");
        has(&dts, "  export class KvError extends $Error {");
        has(&dts, "  export class KeyNotFoundError extends kv.KvError {");
        has(&dts, "    readonly key: string;");
        has(
            &dts,
            "    constructor(fields: { key: string }, message?: string);",
        );
        has(
            &dts,
            "    wait(ms: bigint, options?: { signal?: AbortSignal }): Promise<bigint>;",
        );
        has(&dts, "    scan(): IterableIterator<kv.Store>;");
        has(&dts, "    close_(): void;");
        has(&dts, "    by_mode: Partial<Record<kv.Mode, boolean>>;");
        has(
            &dts,
            "  export function delete_(class_: kv.Store | null): boolean;",
        );
        has(
            &dts,
            "  export function install(policy: kv.Policy | null): void;",
        );
        has(&dts, "    maybe(): kv.Store | null;");
        has(&dts, "    export function count(store: kv.Store): bigint;");
    }
}
