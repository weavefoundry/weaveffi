//! The JavaScript layer shared by the [`node`](crate::targets::node) and
//! [`wasm`](crate::targets::wasm) targets.
//!
//! Both targets generate the same ES module, whose every declaration is a
//! top-level export: the same error classes (a root error class named after
//! the package, one class per error domain, one subclass per code, and
//! `CancelledError`), records as plain objects, C-style enums as frozen
//! objects, rich enums as tagged unions, interfaces as classes with
//! `close()`, `[Symbol.dispose]`, and a `FinalizationRegistry` backstop,
//! callback interfaces as any object with the methods, lazy iterators with
//! `close()`, `Promise`-returning async functions with `AbortSignal`
//! cancellation, and the same `.d.ts`. Optional scalars are a value or
//! `null`, and typed arrays cross as typed arrays (or plain arrays, checked
//! element by element) and come back as plain arrays.
//!
//! Only the transport differs: Node.js loads the library with koffi, and
//! WebAssembly stages values in a module's linear memory. Each transport
//! binds the C signatures `index.js` lists (`$sigs`) into functions that
//! take and return one JavaScript value per C value (see [`api`]), so all
//! marshalling above the C value is generated here, once.
//!
//! The fixed runtime (codec, argument checks, error classes, object and
//! iterator wrappers, callback registry, cancellation, the load-time checks)
//! is a real JavaScript file, `runtime/runtime.js`, emitted next to
//! `index.js`. The leak counters tests read are the package's `./debug`
//! export (`debug.js`), not part of its API.

mod api;
mod codec;
mod dts;
pub(crate) mod names;

use weaveffi_model::model::{Model, ABI_VERSION};
use weaveffi_model::pkg::Identity;

use crate::codegen::CodeWriter;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use serde_json::{json, Map, Value};

pub(crate) use api::{render_api, render_tables};
pub(crate) use names::ErrorNames;

/// The shared runtime, with `{{ERROR_CLASS}}` naming the root error class.
const RUNTIME_JS: &str = include_str!("runtime/runtime.js");

/// The runtime helpers the generated API may reference.
const RUNTIME_IMPORTS: &[&str] = &[
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
    "$utf8",
    "$text",
    "$bytesOf",
    "$array",
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
    "$noConstructor",
    "$impl",
    "$register",
    "$adapter",
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

/// Render `runtime.js`.
pub(crate) fn render_runtime(errors: &ErrorNames) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    w.raw(RUNTIME_JS.replace("{{ERROR_CLASS}}", &errors.root));
    w.blank();
    w.raw(render_trailer(CommentStyle::DoubleSlash, "runtime.js"));
    w.finish()
}

/// The import and re-export lines every `index.js` starts with.
pub(crate) fn render_imports(w: &mut CodeWriter, errors: &ErrorNames) {
    let root = &errors.root;
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

/// The load-time check of the ABI revision and of `$contract` (see
/// [`render_tables`]) against the loaded transport `$m`.
pub(crate) fn verify_call(model: &Model) -> String {
    format!(
        "$verify($m, {}, {ABI_VERSION}, $contract);",
        names::js_string(&model.identity.name),
    )
}

/// Render `index.d.ts`. `extra` carries the transport's own declarations.
pub(crate) fn render_dts(model: &Model, errors: &ErrorNames, extra: &str) -> String {
    let mut w = CodeWriter::two_space();
    w.raw(render_prelude(CommentStyle::DoubleSlash));
    dts::render_declarations(&mut w, model, errors, extra);
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
    w.line(" * 1 callback implementations, 2 iterators, 3 cancel tokens, 4 runs.");
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
    w.line(" * 1 callback implementations, 2 iterators, 3 cancel tokens, 4 runs.");
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
pub(crate) fn npm_metadata(identity: &Identity, name: &str) -> Map<String, Value> {
    let version = env!("CARGO_PKG_VERSION");
    let mut obj = Map::new();
    let mut put = |key: &str, value: Value| {
        obj.insert(key.to_string(), value);
    };
    put("//", json!(format!("Generated by WeaveFFI {version}")));
    put(
        "//warning",
        json!("DO NOT EDIT. Your changes will be overwritten."),
    );
    put("//regenerate", json!("To regenerate: weaveffi generate"));
    put("name", json!(name));
    put("version", json!(identity.version));
    put("description", json!(identity.description_or_default()));
    if let Some(license) = &identity.license {
        put("license", json!(license));
    }
    if let Some(author) = identity.authors.first() {
        put("author", json!(author));
    }
    if let Some(homepage) = &identity.homepage {
        put("homepage", json!(homepage));
    }
    if let Some(repository) = &identity.repository {
        put("repository", json!({ "type": "git", "url": repository }));
    }
    put("type", json!("module"));
    put("main", json!("index.js"));
    put("types", json!("index.d.ts"));
    put(
        "exports",
        json!({
            ".": { "types": "./index.d.ts", "default": "./index.js" },
            "./debug": { "types": "./debug.d.ts", "default": "./debug.js" },
        }),
    );
    obj
}

/// A small API exercising every shape both JavaScript targets render, for
/// their unit tests: two error domains (one with a payload), a C-style and
/// a rich enum, a record holding objects, a callback interface with a
/// return of every family and methods of every error strategy, an
/// interface with a canonical constructor, a method named `close`, a
/// throwing method, an iterator of objects, a nullable async result, and a
/// cancellable async method, optional scalars and typed arrays in every
/// position, and free functions (one named after a keyword, one taking an
/// optional callback).
#[cfg(test)]
pub(crate) fn test_api(name: &str) -> Model {
    const YAML: &str = r#"
version: "0.13.0"
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
records:
  - name: Bundle
    fields:
      - { name: primary, type: Store }
      - { name: extras, type: "[Store]" }
      - { name: stamp, type: i64 }
      - { name: by_mode, type: "{Mode:bool}" }
      - { name: weights, type: "[f64]" }
callbacks:
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
  - { name: count, params: [{ name: store, type: Store }], return: i64, throws: KvError }
"#;
    let api = weaveffi_model::parse::parse_api_str(YAML, "yaml").expect("test fixture parses");
    weaveffi_model::validate::validate(&api, &Identity::named(name), None)
        .expect("test fixture validates")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::contract;

    #[test]
    fn runtime_names_the_package_root_error() {
        let model = test_api("my-kv");
        let rt = render_runtime(&ErrorNames::new(&model));
        assert!(
            rt.contains("export class MyKvError extends Error {"),
            "{rt}"
        );
        assert!(rt.contains("export class CancelledError extends MyKvError {"));
        assert!(!rt.contains("{{"));
    }

    #[test]
    fn the_contract_lists_every_row() {
        let model = test_api("acme-kv");
        let mut w = CodeWriter::two_space();
        render_tables(&mut w, &model);
        let js = w.finish();
        let rows = contract::rows(&model);
        assert_eq!(js.matches("n, '").count(), rows.len());
        let first = &rows[0];
        assert!(js.contains(&format!(
            "  ['{}', {}n, '{}'], // {}\n",
            first.symbol,
            contract::hex(first.hash),
            first.path,
            first.signature
        )));
        assert_eq!(verify_call(&model), "$verify($m, 'acme-kv', 6, $contract);");
    }

    #[test]
    fn npm_tarball_names_follow_npm_pack() {
        assert_eq!(npm_tarball_name("kv", "1.0.0"), "kv-1.0.0.tgz");
        assert_eq!(npm_tarball_name("@acme/kv", "1.0.0"), "acme-kv-1.0.0.tgz");
    }
}
