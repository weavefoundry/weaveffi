//! Node.js (N-API) binding generator for WeaveFFI.
//!
//! Emits a JavaScript loader plus TypeScript type definitions for the
//! companion N-API addon (ABI revision 2). Records and rich enums are value
//! types: they cross the ABI serialized in WeaveFFI value buffers, so records
//! surface as plain JS objects, rich enums as tagged unions, and the loader
//! carries a small private buffer writer/reader plus one pack and one unpack
//! function per type. 64-bit integers are `bigint`s everywhere (parameters,
//! returns, record fields, callback arguments), so nothing above 2^53 is
//! silently rounded.
//!
//! Interfaces surface as JS classes holding one strong reference to a
//! reference-counted native object: `close()` (or `Symbol.dispose`, for
//! `using` declarations) releases it, and a `FinalizationRegistry` backstop
//! releases a wrapper that is collected unclosed, so the producer's
//! `destroy` runs exactly once. Object-typed parameters are borrowed for the
//! call; returned, awaited, and iterated objects are adopted into fresh
//! wrappers; `Interface?` maps to `Wrapper | null`; and objects inside value
//! buffers cross as freshly cloned object tokens. Callback interfaces surface
//! as TypeScript `interface`s any object with the right methods satisfies:
//! the addon keeps the implementation behind a `napi_ref`, installs one
//! static vtable per interface, and its trampolines call the implementation
//! directly on the JS thread or hop through a `napi_threadsafe_function` and
//! wait when the producer calls from another thread. Async functions surface
//! as `Promise`-returning functions, `iter<T>` functions as lazy
//! `IterableIterator<T>` wrappers that pull one element per step, and each
//! declared error domain as an `Error` subclass extending the generic
//! `WeaveFFIError` brand. Implements [`LanguageBackend`]; the shared driver
//! bridges it into the generator pipeline.
//!
//! The crate is split by emitted artifact: `types` holds JS/TS naming and
//! type mapping, `codec` the value-buffer codec emitters, `runtime` the
//! fixed JS runtime prelude, `entities` the idiomatic JS surface and the
//! `index.js` assembler, `addon` the native C addon, and `package` the
//! manifests, `types.d.ts`, and the packaged layout.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod addon;
mod codec;
mod entities;
mod package;
mod runtime;
mod types;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{BindingModel, CallbackInterfaceBinding, ModuleBinding};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::platform::Platform;
use weaveffi_core::resolved::ResolvedApi;

use crate::addon::render_addon_c;
use crate::entities::render_node_index;
use crate::package::{
    node_platform_tokens, render_binding_gyp, render_callback_interface_dts, render_node_dts,
    render_package_json, render_packaged_binding_gyp, render_packaged_package_json,
    render_packaged_readme, render_platform_package_json,
};

/// Per-target configuration for [`NodeGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct NodeConfig {
    /// npm package name (default `"weaveffi"`).
    pub package_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted JS/TS function names, so module `kv`'s `open_store` exports as
    /// `openStore` rather than `kvOpenStore`. Set to `false` to keep
    /// module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the native addon calls the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            package_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl NodeConfig {
    /// Returns the configured npm package name, falling back to `"weaveffi"`.
    pub fn package_name(&self) -> &str {
        self.package_name.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured C ABI symbol prefix, falling back to
    /// `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Node.js backend: emits a JavaScript loader and TypeScript declarations for
/// the companion N-API addon that wraps the C ABI.
pub struct NodeGenerator;

impl LanguageBackend for NodeGenerator {
    type Config = NodeConfig;

    fn name(&self) -> &'static str {
        "node"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    /// The consumer-facing TypeScript `interface` for one callback interface,
    /// the same declaration `types.d.ts` carries. The JS adapter and the
    /// native vtable are emitted by the `index.js` and addon assemblers.
    fn render_callback_interface(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        cb: &CallbackInterfaceBinding,
        _config: &Self::Config,
    ) {
        render_callback_interface_dts(out, cb);
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let strip = config.strip_module_prefix;
        vec![
            OutputFile::new(
                dir.join("index.js"),
                render_node_index(model, strip, input_basename),
            ),
            OutputFile::new(
                dir.join("types.d.ts"),
                render_node_dts(model, strip, input_basename),
            ),
            OutputFile::new(
                dir.join("package.json"),
                render_package_json(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
            OutputFile::new(dir.join("binding.gyp"), render_binding_gyp(input_basename)),
            OutputFile::new(
                dir.join("weaveffi_addon.c"),
                render_addon_c(model, strip, input_basename),
            ),
        ]
    }

    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let dir = out_dir.join("node");
        let input_basename = config.input_basename();
        let strip = config.strip_module_prefix;
        let package = pkg::resolve(
            api,
            config.package_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib = &ctx.binaries.lib_name;

        // The per-platform package names follow the esbuild/swc convention:
        // `<pkg>-<node-os>-<node-cpu>` constrained by npm `os`/`cpu`, so npm
        // installs only the matching one. Platforms Node does not run on
        // (Android, Wasm) have no npm tokens and are skipped.
        let platform_pkgs: Vec<(Platform, (&str, &str), String)> = ctx
            .binaries
            .platforms()
            .filter_map(|p| {
                let tokens = node_platform_tokens(p)?;
                let (os, cpu) = tokens;
                Some((p, tokens, format!("{}-{os}-{cpu}", package.name)))
            })
            .collect();
        let dep_pkgs: Vec<(Platform, String)> = platform_pkgs
            .iter()
            .map(|(p, _, name)| (*p, name.clone()))
            .collect();

        let mut files = vec![
            PackagedFile::text(
                dir.join("index.js"),
                render_node_index(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("types.d.ts"),
                render_node_dts(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("package.json"),
                render_packaged_package_json(&package, &dep_pkgs, input_basename),
            ),
            PackagedFile::text(
                dir.join("binding.gyp"),
                render_packaged_binding_gyp(&package.name, lib, input_basename),
            ),
            PackagedFile::text(
                dir.join("weaveffi_addon.c"),
                render_addon_c(model, strip, input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(&package, ctx, input_basename),
            ),
        ];

        // Each platform package bundles its prebuilt library and is gated by
        // npm `os`/`cpu` so only the matching one installs.
        for (platform, tokens, pkg_name) in &platform_pkgs {
            let pkg_dir = dir.join("npm").join(pkg_name);
            files.push(PackagedFile::text(
                pkg_dir.join("package.json"),
                render_platform_package_json(pkg_name, &package.version, *tokens),
            ));
            let nb = ctx.binaries.get(*platform).expect("platform has a binary");
            files.push(PackagedFile::copy(
                pkg_dir.join(ctx.binaries.bundled_filename(*platform)),
                nb.source.clone(),
            ));
        }
        Some(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{
        Api, CallbackInterfaceDef, EnumDef, EnumVariant, Function, InterfaceDef, Module, Param,
        StructDef, StructField, TypeRef,
    };

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            doc: None,
        }
    }

    fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
        }
    }

    fn field(name: &str, ty: TypeRef) -> StructField {
        StructField {
            name: name.into(),
            ty,
            doc: None,
        }
    }

    fn named(name: &str) -> TypeRef {
        TypeRef::Named(name.into())
    }

    fn variant(name: &str, value: i32, fields: Vec<StructField>) -> EnumVariant {
        EnumVariant {
            name: name.into(),
            value,
            doc: None,
            fields,
        }
    }

    /// One module exercising every revision-2 shape: an interface with a
    /// constructor and a method, `Store?` in and out, a record holding a
    /// `Store` and a `[Store]`, an iterator of `Store`, an async function
    /// returning a `Store`, a 64-bit round trip, a callback interface with
    /// string, i32, record, and object parameters (one method returning
    /// `bool`, one void), a function taking that callback, a C-style enum,
    /// and a rich enum.
    fn fixture() -> BindingModel {
        let mut fetch = func(
            "fetch",
            vec![param("path", TypeRef::StringUtf8)],
            Some(named("Store")),
        );
        fetch.r#async = true;
        let kv = Module {
            name: "kv".into(),
            doc: None,
            functions: vec![
                func(
                    "maybe_store",
                    vec![param("store", TypeRef::Optional(Box::new(named("Store"))))],
                    Some(TypeRef::Optional(Box::new(named("Store")))),
                ),
                func(
                    "scan",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(named("Store")))),
                ),
                func(
                    "subscribe",
                    vec![param("listener", named("Listener"))],
                    None,
                ),
                func(
                    "describe",
                    vec![param("bundle", named("Bundle"))],
                    Some(named("Bundle")),
                ),
                func(
                    "widen",
                    vec![param("n", TypeRef::I64), param("m", TypeRef::U64)],
                    Some(TypeRef::U64),
                ),
                fetch,
            ],
            interfaces: vec![InterfaceDef {
                name: "Store".into(),
                doc: Some("A key-value store.".into()),
                deprecated: None,
                constructors: vec![func("new", vec![param("path", TypeRef::StringUtf8)], None)],
                methods: vec![func(
                    "get",
                    vec![param("key", TypeRef::StringUtf8)],
                    Some(TypeRef::StringUtf8),
                )],
                statics: vec![],
            }],
            callback_interfaces: vec![CallbackInterfaceDef {
                name: "Listener".into(),
                doc: Some("Receives store events.".into()),
                deprecated: None,
                methods: vec![
                    func(
                        "on_message",
                        vec![
                            param("text", TypeRef::StringUtf8),
                            param("weight", TypeRef::I32),
                        ],
                        Some(TypeRef::Bool),
                    ),
                    func(
                        "on_bundle",
                        vec![
                            param("bundle", named("Bundle")),
                            param("store", named("Store")),
                            param("alt", TypeRef::Optional(Box::new(named("Store")))),
                        ],
                        None,
                    ),
                ],
            }],
            structs: vec![StructDef {
                name: "Bundle".into(),
                doc: None,
                deprecated: None,
                fields: vec![
                    field("primary", named("Store")),
                    field("extras", TypeRef::List(Box::new(named("Store")))),
                    field("stamp", TypeRef::I64),
                ],
            }],
            enums: vec![
                EnumDef {
                    name: "Mode".into(),
                    doc: None,
                    deprecated: None,
                    variants: vec![variant("Fast", 0, vec![]), variant("Safe", 7, vec![])],
                },
                EnumDef {
                    name: "Shape".into(),
                    doc: None,
                    deprecated: None,
                    variants: vec![
                        variant("Empty", 0, vec![]),
                        variant("Circle", 1, vec![field("radius", TypeRef::F64)]),
                    ],
                },
            ],
            errors: None,
            modules: vec![],
        };
        let api = ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![kv],
        });
        BindingModel::build(&api, "weaveffi")
    }

    fn index_js() -> String {
        render_node_index(&fixture(), true, "weaveffi.yml")
    }

    fn addon_c() -> String {
        render_addon_c(&fixture(), true, "weaveffi.yml")
    }

    fn dts() -> String {
        render_node_dts(&fixture(), true, "weaveffi.yml")
    }

    fn assert_has(src: &str, needle: &str) {
        assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
    }

    fn assert_lacks(src: &str, needle: &str) {
        assert!(!src.contains(needle), "unexpected `{needle}` in:\n{src}");
    }

    #[test]
    fn addon_checks_abi_revision_and_binds_runtime_symbols() {
        let c = addon_c();
        assert_has(&c, "WEAVEFFI_ABI_VERSION");
        assert_has(&c, "weaveffi_error_set(");
        assert_has(&c, "weaveffi_error_clear(&err);");
        assert_lacks(&c, "arena");
        assert_lacks(&c, "weaveffi_handle_t");
    }

    #[test]
    fn interface_wrapper_closes_exactly_once_and_exposes_clone() {
        let js = index_js();
        assert_has(&js, "class Store {");
        assert_has(
            &js,
            "this._handle = __invoke(addon.Store_new, [path], __generic);",
        );
        assert_has(&js, "Store._cleanup.register(this, this._handle, this);");
        assert_has(&js, "close() {");
        assert_has(&js, "Store._cleanup.unregister(this);");
        assert_has(&js, "addon.Store__destroy(this._handle);");
        assert_has(&js, "this._handle = 0n;");
        assert_has(
            &js,
            "Store.prototype[Symbol.dispose] = Store.prototype.close;",
        );
        assert_has(&js, "Store._cleanup = new FinalizationRegistry(");
        assert_has(&js, "return addon.Store__clone(__borrow(this, Store));");
        assert_has(&js, "Store._adopt = function (handle) {");
        assert_has(
            &js,
            "__invoke(addon.Store_get, [__borrow(this, Store), key], __generic)",
        );

        let c = addon_c();
        assert_has(&c, "static napi_value Napi_weaveffi_kv_Store_destroy(");
        assert_has(&c, "weaveffi_kv_Store_destroy(self);");
        assert_has(&c, "static napi_value Napi_weaveffi_kv_Store_clone(");
        assert_has(
            &c,
            "weaveffi_kv_Store* cloned = weaveffi_kv_Store_clone(self);",
        );
        assert_has(
            &c,
            "{ \"Store__destroy\", NULL, Napi_weaveffi_kv_Store_destroy,",
        );
        assert_has(
            &c,
            "{ \"Store__clone\", NULL, Napi_weaveffi_kv_Store_clone,",
        );
        // Methods borrow the wrapped handle as the leading const pointer.
        assert_has(&c, "(const weaveffi_kv_Store*)self_raw");

        let d = dts();
        assert_has(&d, "export class Store {");
        assert_has(&d, "close(): void;");
        assert_has(&d, "[Symbol.dispose](): void;");
        assert_lacks(&d, "destroy(): void;");
    }

    #[test]
    fn nullable_objects_map_to_wrapper_or_null() {
        let js = index_js();
        assert_has(
            &js,
            "__invoke(addon.maybeStore, [(store == null ? null : __borrow(store, Store))], __generic)",
        );
        assert_has(&js, "return (_r == null ? null : Store._adopt(_r));");

        let c = addon_c();
        assert_has(&c, "weaveffi_napi_get_handle(env, args[0], &store_raw)");
        assert_has(&c, "(const weaveffi_kv_Store*)store_raw");
        assert_has(&c, "weaveffi_napi_make_handle(env, result, &ret);");
        assert_has(&c, "if (p == NULL) {\n    return napi_get_null(env, out);");

        let d = dts();
        assert_has(
            &d,
            "export function maybeStore(store: Store | null): Store | null",
        );
    }

    #[test]
    fn objects_inside_records_clone_on_write_and_adopt_on_read() {
        let js = index_js();
        assert_has(&js, "function __packBundle(w, v) {");
        assert_has(&js, "w.u64(v.primary._cloneHandle());");
        assert_has(
            &js,
            "__wList(w, v.extras, (w, v) => w.u64(v._cloneHandle()));",
        );
        assert_has(&js, "w.i64(v.stamp);");
        assert_has(&js, "primary: Store._adopt(r.u64()),");
        assert_has(&js, "extras: __rList(r, (r) => Store._adopt(r.u64())),");
        assert_has(&js, "stamp: r.i64(),");

        let d = dts();
        assert_has(&d, "primary: Store;");
        assert_has(&d, "extras: Store[];");
        assert_has(&d, "stamp: bigint;");
    }

    #[test]
    fn iterator_and_async_object_results_are_adopted() {
        let js = index_js();
        assert_has(
            &js,
            "new WeaveFFIIterator(_it, addon.scan_iterNext, addon.scan_iterDestroy, __generic, (_e) => Store._adopt(_e));",
        );
        assert_has(
            &js,
            "__invokeAsync(addon.fetch, [path], __generic).then((_r) => Store._adopt(_r));",
        );

        let c = addon_c();
        assert_has(&c, "weaveffi_napi_make_handle(env, iter_item, &ret);");
        assert_has(&c, "weaveffi_napi_make_handle(env, ctx->result, &val);");

        let d = dts();
        assert_has(&d, "export function scan(): IterableIterator<Store>");
        assert_has(&d, "export function fetch(path: string): Promise<Store>");
    }

    #[test]
    fn sixty_four_bit_integers_are_bigints() {
        let c = addon_c();
        assert_has(&c, "napi_get_value_bigint_int64(env, v, out, &lossless)");
        assert_has(&c, "napi_get_value_bigint_uint64(env, v, out, &lossless)");
        assert_has(
            &c,
            "napi_throw_range_error(env, NULL, \"bigint does not fit",
        );
        assert_has(&c, "arg_status = weaveffi_napi_get_i64(env, args[0], &n);");
        assert_has(&c, "arg_status = weaveffi_napi_get_u64(env, args[1], &m);");
        assert_has(&c, "napi_create_bigint_uint64(env, result, &ret);");
        assert_lacks(&c, "napi_create_int64(");
        assert_lacks(&c, "napi_get_value_int64(env, args");

        let d = dts();
        assert_has(&d, "export function widen(n: bigint, m: bigint): bigint");
    }

    #[test]
    fn plain_enums_have_a_runtime_value_matching_their_declaration() {
        let js = index_js();
        // The `.d.ts` promises `export enum Mode`, so `index.js` must export
        // the value, with TypeScript's forward and reverse mappings.
        assert_has(
            &js,
            "wv.Mode = Object.freeze({\n  Fast: 0,\n  Safe: 7,\n  0: 'Fast',\n  7: 'Safe',\n});",
        );
        // A rich enum is a tagged union: pack/unpack functions, no value.
        assert_has(&js, "function __packShape(w, v) {");
        assert_lacks(&js, "wv.Shape =");

        let d = dts();
        assert_has(&d, "export enum Mode {\n  Fast = 0,\n  Safe = 7,\n}");
        assert_has(&d, "export type Shape =");
    }

    #[test]
    fn callback_interface_renders_ts_interface_and_js_adapter() {
        let d = dts();
        assert_has(&d, "export interface Listener {");
        assert_has(&d, "onMessage(text: string, weight: number): boolean;");
        assert_has(
            &d,
            "onBundle(bundle: Bundle, store: Store, alt: Store | null): void;",
        );
        assert_has(&d, "export function subscribe(listener: Listener): void");

        let mut via_trait = String::new();
        let model = fixture();
        let m = &model.modules[0];
        NodeGenerator.render_callback_interface(
            &mut via_trait,
            m,
            &m.callback_interfaces[0],
            &NodeConfig::default(),
        );
        assert_has(&via_trait, "export interface Listener {");

        let js = index_js();
        assert_has(&js, "function __adaptListener(impl) {");
        assert_has(&js, "on_message(text, weight) {");
        assert_has(&js, "return impl.onMessage(text, weight);");
        assert_has(&js, "on_bundle(bundle, store, alt) {");
        assert_has(
            &js,
            "return impl.onBundle(__decode(__unpackBundle, bundle), Store._adopt(store), (alt == null ? null : Store._adopt(alt)));",
        );
        assert_has(
            &js,
            "__invoke(addon.subscribe, [__adaptListener(listener)], __generic)",
        );
    }

    #[test]
    fn callback_interface_renders_static_vtable_and_thread_hopping_trampolines() {
        let c = addon_c();
        // Handle table entry, thread identity, and the hop machinery.
        assert_has(&c, "#include <uv.h>");
        assert_has(&c, "weaveffi_napi_js_thread = uv_thread_self();");
        assert_has(&c, "napi_create_reference(env, target, 1, &ctx->ref);");
        assert_has(&c, "napi_create_threadsafe_function(env, NULL, NULL, resource_name, 0, 1, NULL, NULL, NULL, dispatch, &ctx->tsfn);");
        assert_has(&c, "uv_cond_wait(&req->cv, &req->mu);");
        // Trampolines match the vtable signatures and route by thread.
        assert_has(
            &c,
            "static bool weaveffi_kv_Listener_on_message_tramp(void* ctx, const char* text, int32_t weight, weaveffi_error* out_err) {",
        );
        assert_has(
            &c,
            "static void weaveffi_kv_Listener_on_bundle_tramp(void* ctx, const uint8_t* bundle_ptr, size_t bundle_len, weaveffi_kv_Store* store, weaveffi_kv_Store* alt, weaveffi_error* out_err) {",
        );
        assert_has(&c, "if (weaveffi_napi_on_js_thread()) {");
        assert_has(&c, "weaveffi_napi_cb_hop(&req);");
        // The invoker looks the method up by IDL name, borrows buffers, and
        // hands object references over as handles.
        assert_has(
            &c,
            "napi_get_named_property(env, target, \"on_bundle\", &fn);",
        );
        assert_has(
            &c,
            "napi_create_buffer_copy(env, f->bundle_len, f->bundle_ptr ?",
        );
        assert_has(&c, "weaveffi_napi_make_handle(env, f->store, &argv[1]);");
        assert_has(&c, "weaveffi_napi_make_handle(env, f->alt, &argv[2]);");
        assert_has(&c, "napi_get_value_bool(env, result, &f->result);");
        // Failure path: a JS exception becomes a foreign error, never an unwind.
        assert_has(
            &c,
            "weaveffi_error_set(out_err, -4, msg[0] ? msg : fallback);",
        );
        assert_has(
            &c,
            "weaveffi_napi_cb_report(env, f->hdr.out_err, \"Listener.on_message threw\");",
        );
        // One static vtable per interface, methods in order then `free`.
        assert_has(
            &c,
            "static const weaveffi_kv_Listener_vtable weaveffi_kv_Listener_napi_vtable = { weaveffi_kv_Listener_on_message_tramp, weaveffi_kv_Listener_on_bundle_tramp, weaveffi_napi_cb_free };",
        );
        // The entry point registers the adapter and passes ctx plus vtable.
        assert_has(
            &c,
            "listener_ctx = weaveffi_napi_cb_register(env, args[0], \"weaveffi_kv_Listener\", weaveffi_kv_Listener_napi_dispatch);",
        );
        assert_has(
            &c,
            "weaveffi_kv_subscribe((void*)listener_ctx, &weaveffi_kv_Listener_napi_vtable, &err);",
        );
    }

    #[test]
    fn package_skips_platforms_node_does_not_run_on() {
        assert_eq!(
            node_platform_tokens(Platform::MacosArm64),
            Some(("darwin", "arm64"))
        );
        assert_eq!(node_platform_tokens(Platform::AndroidArm64), None);
        assert_eq!(node_platform_tokens(Platform::Wasm32), None);
    }

    #[test]
    fn legacy_features_are_gone() {
        for src in [index_js(), dts(), addon_c()] {
            assert_lacks(&src, "_napi_listener");
            assert_lacks(&src, "weaveffi_handle_t");
            assert_lacks(&src, "arena");
            assert_lacks(&src, "destroy()");
        }
    }
}
