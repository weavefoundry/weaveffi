//! The JavaScript layer shared by the [`node`](crate::targets::node) and
//! [`wasm`](crate::targets::wasm) targets.
//!
//! Both targets generate the same ES module: one exported namespace object
//! per top-level IDL module (`export const kv = { Store, get, ... }`, with
//! nested modules as nested namespaces), the same error classes (a root
//! `{Package}Error`, one class per error domain, one subclass per code, and
//! `CancelledError`), records as plain objects, rich enums as tagged unions,
//! interfaces as classes with `close()`, `[Symbol.dispose]`, and a
//! `FinalizationRegistry` backstop, lazy iterators with `close()`,
//! `Promise`-returning async functions with `AbortSignal` cancellation, and
//! the same `.d.ts`. Optional scalars cross as a flag and a value, numeric
//! lists as typed arrays. Only the transport differs: Node.js calls an
//! N-API addon, WebAssembly stages values in linear memory. Both expose the
//! raw calling convention documented in [`api`] as an object named `$raw`.
//!
//! The fixed runtime (codec, argument checks, error classes, object and
//! iterator wrappers, cancellation, the load-time checks) is a real
//! JavaScript file, `runtime/runtime.js`, emitted next to `index.js`. The
//! leak counters tests read are the package's `./debug` export
//! (`debug.js`), not part of its API.

mod api;
mod codec;
mod dts;
pub(crate) mod names;

use weaveffi_model::model::{Model, ABI_VERSION};
use weaveffi_model::pkg::Identity;

use crate::codegen::contract;
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
    "$untyped",
    "$W",
    "$WK",
    "$R",
    "$check",
    "$opt",
    "$slice",
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
    "$verify",
    "$setLive",
];

/// The files every JavaScript package ships beside the transport's own.
pub(crate) const PACKAGE_FILES: &[&str] = &[
    "index.js",
    "index.d.ts",
    "runtime.js",
    "debug.js",
    "debug.d.ts",
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
/// for every top-level module, its table function and each declaration's
/// `[id, hash, path]` row from [`contract::tables`], which the load-time
/// check looks up in the library's own tables.
pub(crate) fn render_contract(w: &mut CodeWriter, model: &Model) {
    w.line("// The declarations these bindings were generated with, per top-level");
    w.line("// module's table function, as [id, hash, path]: the library's tables must");
    w.line("// hold each one (see $verify).");
    w.block("const $contract = [", "];", |w| {
        for table in contract::tables(model) {
            w.block(
                format!("[{}, [", names::js_string(&table.symbol)),
                "]],",
                |w| {
                    for row in &table.rows {
                        w.line(format!(
                            "[{}n, {}n, {}], // {}",
                            contract::hex(row.id),
                            contract::hex(row.hash),
                            names::js_string(&row.path),
                            row.signature
                        ));
                    }
                },
            );
        }
    });
}

/// The load-time check of the ABI revision and of `$contract` (see
/// [`render_contract`]) against the library behind `$raw`.
pub(crate) fn verify_call(model: &Model) -> String {
    format!(
        "$verify($raw, {}, '{}', {ABI_VERSION}, $contract);",
        names::js_string(&model.identity.name),
        model.prefix(),
    )
}

/// Render `index.d.ts`. `extra` carries the transport's own declarations.
pub(crate) fn render_dts(model: &Model, extra: &str) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    dts::render_declarations(&mut w, model, &root_error_class(&model.identity), extra);
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, "index.d.ts"));
    w.finish()
}

/// Render `debug.js`, the package's `./debug` export: the leak counters a
/// test reads once the bindings are loaded (after `init()` on
/// WebAssembly).
pub(crate) fn render_debug_js() -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.line("// Diagnostics for tests of these bindings; not part of their API.");
    w.line("import './index.js';");
    w.line("import { $live } from './runtime.js';");
    w.blank();
    w.line("/**");
    w.line(" * The native library's live-resource counter of `kind`: 0 objects,");
    w.line(" * 1 callback implementations, 2 iterators, 3 cancel tokens, 4 byte runs.");
    w.line(" * Kind -1 is `1n` when the library counts at all (its `leak-check`");
    w.line(" * feature); otherwise every kind is `0n`.");
    w.line(" */");
    w.block("export function debugLive(kind) {", "}", |w| {
        w.line("return $live(kind);");
    });
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, "debug.js"));
    w.finish()
}

/// Render `debug.d.ts`.
pub(crate) fn render_debug_dts() -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.line("/**");
    w.line(" * The native library's live-resource counter of `kind`: 0 objects,");
    w.line(" * 1 callback implementations, 2 iterators, 3 cancel tokens, 4 byte runs.");
    w.line(" * Kind -1 is `1n` when the library counts at all (its `leak-check`");
    w.line(" * feature); otherwise every kind is `0n`.");
    w.line(" */");
    w.line("export declare function debugLive(kind: number): bigint;");
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, "debug.d.ts"));
    w.finish()
}

/// The npm tarball file name `npm pack` gives `name` at `version`
/// (`kvstore-1.2.0.tgz`, or `acme-kv-1.2.0.tgz` for `@acme/kv`).
pub(crate) fn npm_tarball_name(name: &str, version: &str) -> String {
    format!(
        "{}-{version}.tgz",
        name.trim_start_matches('@').replace('/', "-")
    )
}

/// The npm metadata both `package.json` files share: the generated-file
/// notice, then name, version, description, the optional fields, and the
/// ES module entry points (`.` and `./debug`).
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
    let entry = |types: &str, js: &str| {
        JsonValue::Object(
            JsonObject::new()
                .str_entry("types", types)
                .str_entry("default", js),
        )
    };
    obj.str_entry("type", "module")
        .str_entry("main", "index.js")
        .str_entry("types", "index.d.ts")
        .entry(
            "exports",
            JsonValue::Object(
                JsonObject::new()
                    .entry(".", entry("./index.d.ts", "./index.js"))
                    .entry("./debug", entry("./debug.d.ts", "./debug.js")),
            ),
        )
}

/// A small API exercising every shape both JavaScript targets render, for
/// their unit tests: two error domains (one with a payload), a C-style and
/// a rich enum, a record holding objects, a callback interface with a
/// return of every family and methods of every error strategy, an
/// interface with a canonical constructor, a method named `close`, a
/// throwing method, an iterator of objects, a nullable async result, and a
/// cancellable async method, optional scalars and typed arrays in every
/// position, free functions (one named after a keyword, one taking an
/// optional callback), and a nested module.
#[cfg(test)]
pub(crate) fn test_api(name: &str) -> Model {
    const YAML: &str = r#"
version: "0.12.0"
modules:
  - name: kv
    doc: A key-value store.
    errors:
      - name: KvError
        codes:
          - { name: KEY_NOT_FOUND, code: 1, message: "key not found", fields: [{ name: key, type: string }] }
          - { name: FULL, code: 2, message: "it's full" }
      - name: Other
        codes:
          - { name: Broken, code: 1, message: "broken" }
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
          - { name: weights, type: "[f64]" }
    callback_interfaces:
      - name: Listener
        methods:
          - name: on_message
            params: [{ name: text, type: string }, { name: weight, type: u64 }, { name: level, type: "u8?" }]
            return: bool
          - name: on_bundle
            params: [{ name: bundle, type: Bundle }, { name: store, type: Store }, { name: alt, type: "Store?" }]
      - name: Policy
        methods:
          - { name: admit, params: [{ name: bundle, type: Bundle }], return: Bundle, throws: KvError }
          - { name: label, params: [], return: string, throws: any }
          - { name: blob, params: [], return: bytes }
          - { name: pick, params: [{ name: key, type: string }], return: Store }
          - { name: maybe, params: [], return: "Store?" }
          - { name: mode, params: [], return: Mode }
          - { name: weight, params: [], return: i64 }
          - { name: limit, params: [{ name: hint, type: "i32?" }], return: "i16?" }
          - { name: scores, params: [{ name: sizes, type: "[u64]" }], return: "[f32]", throws: Other }
    interfaces:
      - name: Store
        doc: A store.
        constructors:
          - { name: new, params: [{ name: path, type: string }] }
        methods:
          - { name: get, params: [{ name: key, type: string }], return: bytes, throws: KvError }
          - { name: close, params: [] }
          - { name: scan, params: [], return: "iter<Store>" }
          - { name: fetch, params: [{ name: path, type: string }], return: "Store?", async: true }
          - { name: wait, params: [{ name: ms, type: i64 }], return: i64, async: true, cancellable: true }
          - { name: ttl, params: [{ name: key, type: string }], return: "i64?", async: true }
          - { name: sizes, params: [], return: "[u32]", async: true, throws: any }
    functions:
      - { name: subscribe, params: [{ name: listener, type: Listener }, { name: mode, type: Mode }] }
      - { name: install, params: [{ name: policy, type: "Policy?" }] }
      - { name: widen, params: [{ name: n, type: i64 }, { name: m, type: u64 }, { name: b, type: i8 }], return: u64 }
      - { name: delete, params: [{ name: class, type: "Store?" }], return: bool }
      - { name: shapes, params: [], return: "iter<Shape>" }
      - { name: maybe, params: [{ name: x, type: "u16?" }, { name: mode, type: "Mode?" }], return: "f32?", throws: Other }
      - { name: totals, params: [{ name: xs, type: "[i32]" }, { name: ys, type: "[u64]" }], return: "[f64]" }
      - { name: levels, params: [], return: "iter<bool?>" }
      - { name: chunks, params: [], return: "iter<[i16]>" }
    modules:
      - name: stats
        functions:
          - { name: count, params: [{ name: store, type: Store }], return: i64, throws: KvError }
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
        assert_eq!(root_error_class(&Identity::named("io_error")), "IoError");
    }

    #[test]
    fn the_contract_lists_every_row_by_table_function() {
        let model = test_api("acme-kv");
        let mut w = CodeWriter::two_space();
        render_contract(&mut w, &model);
        let js = w.finish();
        let tables = contract::tables(&model);
        assert_eq!(js.matches("n, 0x").count(), tables[0].rows.len());
        assert!(js.contains("const $contract = [\n  ['acme_kv_kv_contract', [\n"));
        assert_eq!(
            verify_call(&model),
            "$verify($raw, 'acme-kv', 'acme_kv', 5, $contract);"
        );
    }

    #[test]
    fn npm_tarball_names_follow_npm_pack() {
        assert_eq!(npm_tarball_name("kv", "1.0.0"), "kv-1.0.0.tgz");
        assert_eq!(npm_tarball_name("@acme/kv", "1.0.0"), "acme-kv-1.0.0.tgz");
    }

    #[test]
    fn modules_are_frozen_namespaces_with_escaped_members() {
        let mut w = CodeWriter::two_space();
        render_api(&mut w, &test_api("acme-kv"));
        let js = w.finish();
        for needle in [
            "export const kv = Object.freeze({",
            "  delete_: kv$delete_,\n",
            "  stats: Object.freeze({\n    count: kv$stats$count,\n  }),\n});",
            "const kv$delete_ = function delete_(class_) {",
            "  close_() {",
            "  7: 'Safe',\n  '-1': 'Off',\n",
        ] {
            assert!(js.contains(needle), "missing `{needle}` in:\n{js}");
        }
    }
}
