//! Ruby (FFI gem) binding generator for WeaveFFI.
//!
//! Emits a Ruby gem (`.gemspec` + library) using the `ffi` gem to call
//! into the C ABI (revision 2) exposed by the underlying cdylib. Interfaces
//! become reference-counted wrapper classes (`close` plus a GC finalizer
//! backstop, `dup`/`clone` for a second reference), records and rich enums
//! become value classes packed into value buffers (with interfaces inside
//! them carried as object tokens), and callback interfaces become
//! duck-typed Ruby modules backed by one static vtable of pinned
//! `FFI::Function` trampolines per interface. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod callbacks;
mod calls;
mod codec;
mod docs;
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
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::{render_attach_function, render_callable, RbScope};
use crate::entities::{
    render_enum, render_error, render_interface_class, render_interface_ffi,
    render_rich_enum_class, render_struct_class,
};
use crate::package::{
    render_gemspec, render_packaged_gemspec, render_packaged_readme, render_readme,
};
use crate::runtime::{render_preamble, ruby_loader_packaged, RUBY_LOADER_ORIGINAL};

/// Per-target configuration for [`RubyGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RubyConfig {
    /// Top-level Ruby module name (default `"WeaveFFI"`).
    pub module_name: Option<String>,
    /// Ruby gem name written into `weaveffi.gemspec` (default `"weaveffi"`).
    pub gem_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted Ruby method names, so a `contacts` module exports
    /// `create_contact` rather than `contacts_create_contact`. Set to
    /// `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the FFI bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for RubyConfig {
    fn default() -> Self {
        Self {
            module_name: None,
            gem_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl RubyConfig {
    /// Returns the configured top-level Ruby module name, falling back to
    /// `"WeaveFFI"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("WeaveFFI")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured gem name, falling back to `"weaveffi"`.
    pub fn gem_name(&self) -> &str {
        self.gem_name.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Ruby backend: emits an `ffi`-gem package (a library module, a `.gemspec`,
/// and a README) binding the C ABI exposed by the underlying cdylib.
pub struct RubyGenerator;

impl LanguageBackend for RubyGenerator {
    type Config = RubyConfig;

    fn name(&self) -> &'static str {
        "ruby"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn render_callback_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        cb: &CallbackInterfaceBinding,
        config: &Self::Config,
    ) {
        callbacks::render_callback_interface(out, module, cb, config.prefix());
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("ruby");
        let lib_dir = dir.join("lib");
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.gem_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib_file = format!("{}.rb", package.ident_name());
        let gem_file = format!("{}.gemspec", package.name);
        vec![
            OutputFile::new(
                lib_dir.join(&lib_file),
                render_ruby_module(
                    model,
                    config.module_name(),
                    config.strip_module_prefix,
                    &lib_file,
                    input_basename,
                ),
            ),
            OutputFile::new(
                dir.join(&gem_file),
                render_gemspec(&package, &gem_file, input_basename),
            ),
            OutputFile::new(
                dir.join("README.md"),
                render_readme(&package, input_basename),
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
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.gem_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib_file = format!("{}.rb", package.ident_name());
        let gem_file = format!("{}.gemspec", package.name);

        // Render the FFI module once with the bundled-first loader.
        let module_src = render_ruby_module(
            model,
            config.module_name(),
            config.strip_module_prefix,
            &lib_file,
            input_basename,
        )
        .replace(
            RUBY_LOADER_ORIGINAL,
            &ruby_loader_packaged(&ctx.binaries.lib_name),
        );
        let readme = render_packaged_readme(&package, input_basename);

        let ruby_dir = out_dir.join("ruby");
        let mut files = Vec::new();
        for nb in &ctx.binaries.binaries {
            let platform = nb.platform;
            // RubyGems has no platform string for Android or wasm32 builds,
            // so those binaries have no gem to land in.
            let Some(ruby_platform) = platform.ruby_platform() else {
                continue;
            };
            let gem_dir = ruby_dir.join(platform.id());
            let lib_dir = gem_dir.join("lib");
            files.push(PackagedFile::text(
                lib_dir.join(&lib_file),
                module_src.clone(),
            ));
            files.push(PackagedFile::copy(
                lib_dir
                    .join("native")
                    .join(ctx.binaries.bundled_filename(platform)),
                nb.source.clone(),
            ));
            files.push(PackagedFile::text(
                gem_dir.join(&gem_file),
                render_packaged_gemspec(&package, &gem_file, ruby_platform, input_basename),
            ));
            files.push(PackagedFile::text(
                gem_dir.join("README.md"),
                readme.clone(),
            ));
        }
        Some(files)
    }
}

/// Render the primary Ruby library source: the fixed preamble, then each
/// module's typed error surface, entities, codecs, FFI attachments, callback
/// interfaces, and wrappers, in dependency order.
fn render_ruby_module(
    model: &BindingModel,
    module_name: &str,
    strip_module_prefix: bool,
    lib_filename: &str,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    render_preamble(&mut out, module_name, model.has_callback_interfaces());
    for m in &model.modules {
        out.push_str(&format!("\n  # === Module: {} ===\n", m.path));
        // The typed error surface comes first so the domain class exists
        // before any wrapper references its checker.
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error(&mut out, m, eb);
        }
        for e in &m.enums {
            // A plain C-style enum is a module of integer constants; a rich
            // (algebraic) enum is a tagged value-class hierarchy packed into
            // value buffers by the codec helpers below.
            if e.is_rich() {
                render_rich_enum_class(&mut out, e);
            } else {
                render_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, s);
        }
        // Value-buffer codecs: one pack/unpack pair per record and rich enum.
        for s in &m.structs {
            codec::render_struct_codec(&mut out, s);
        }
        for e in &m.enums {
            if e.is_rich() {
                codec::render_rich_enum_codec(&mut out, e);
            }
        }
        for i in &m.interfaces {
            render_interface_ffi(&mut out, i);
        }
        for f in &m.functions {
            render_attach_function(&mut out, f);
        }
        // Callback interfaces precede interfaces and functions because their
        // static vtables are what interface members and free functions pass.
        for cb in &m.callback_interfaces {
            callbacks::render_callback_interface(&mut out, m, cb, &model.prefix);
        }
        for i in &m.interfaces {
            render_interface_class(&mut out, m, i, module_name);
        }
        for f in &m.functions {
            let scope = RbScope::Free {
                module_path: &m.path,
                strip_module_prefix,
            };
            render_callable(&mut out, m, f, &scope);
        }
    }
    out.push_str("end\n\n");
    out.push_str(&render_trailer(CommentStyle::Hash, lib_filename));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::platform::{BinarySet, Platform};
    use weaveffi_ir::ir::{
        Api, CallbackInterfaceDef, Function, InterfaceDef, Module, Param, StructDef, StructField,
        TypeRef,
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

    /// One module exercising every revision-2 object shape: an interface
    /// with a constructor and a method, `Store?` in and out, a record holding
    /// a `Store` and a `[Store]`, an iterator of `Store`, a callback
    /// interface with string, i32, record, and object parameters (one method
    /// returning `bool`, one void), and a function taking that callback.
    fn fixture() -> BindingModel {
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
                ],
            }],
            enums: vec![],
            errors: None,
            modules: vec![],
        };
        let api = ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![kv],
        });
        BindingModel::build(&api, "weaveffi")
    }

    fn render(model: &BindingModel) -> String {
        render_ruby_module(model, "WeaveFFI", true, "weaveffi.rb", "weaveffi.yml")
    }

    fn assert_has(src: &str, needle: &str) {
        assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
    }

    #[test]
    fn runtime_binds_error_set_and_drops_legacy_symbols() {
        let src = render(&fixture());
        assert_has(
            &src,
            "attach_function :weaveffi_error_set, [:pointer, :int32, :string], :void",
        );
        assert_has(&src, "FOREIGN_ERROR_CODE = -4");
        assert_has(
            &src,
            &format!("ABI_VERSION = {}", weaveffi_core::cabi::ABI_VERSION),
        );
        for legacy in [
            "arena",
            "listener_refs",
            "handle_t",
            "register_",
            "unregister_",
        ] {
            assert!(
                !src.contains(legacy),
                "legacy `{legacy}` survived in:\n{src}"
            );
        }
    }

    #[test]
    fn interface_wrapper_owns_one_reference_and_can_clone_it() {
        let src = render(&fixture());
        assert_has(
            &src,
            "attach_function :weaveffi_kv_Store_clone, [:pointer], :pointer",
        );
        assert_has(
            &src,
            "attach_function :weaveffi_kv_Store_destroy, [:pointer], :void",
        );
        assert_has(&src, "class StorePtr < FFI::AutoPointer");
        assert_has(&src, "WeaveFFI.weaveffi_kv_Store_destroy(ptr)");
        assert_has(&src, "def close\n");
        assert_has(&src, "def closed?\n");
        assert_has(&src, "def _wv_clone_ptr\n");
        assert_has(&src, "WeaveFFI.weaveffi_kv_Store_clone(handle)");
        assert_has(&src, "def initialize_copy(other)");
        assert_has(
            &src,
            "StorePtr.new(WeaveFFI.weaveffi_kv_Store_clone(other.handle))",
        );
        // The constructor adopts the returned reference; the method borrows.
        assert_has(&src, "def initialize(path)");
        assert_has(&src, "@handle = StorePtr.new(result)");
        assert_has(
            &src,
            "result = WeaveFFI.weaveffi_kv_Store_get(handle, key, err)",
        );
    }

    #[test]
    fn nullable_objects_map_to_nil_in_both_directions() {
        let src = render(&fixture());
        assert_has(&src, "def self.maybe_store(store)");
        assert_has(&src, "result = weaveffi_kv_maybe_store(store&.handle, err)");
        assert_has(
            &src,
            "return nil if result.null?\n    Store._from_ptr(result)",
        );
    }

    #[test]
    fn records_carry_objects_as_cloned_tokens() {
        let src = render(&fixture());
        assert_has(&src, "def self._wv_write_bundle(w, v)");
        assert_has(&src, "w.write_u64(v.primary._wv_clone_ptr.address)");
        assert_has(&src, "w.write_u64(_wv_e0._wv_clone_ptr.address)");
        assert_has(&src, "def self._wv_read_bundle(r)");
        assert_has(&src, "_wv_primary = Store._from_ptr(r.read_object_token)");
        assert_has(&src, "_wv_e0 = Store._from_ptr(r.read_object_token)");
        assert_has(&src, "def read_object_token");
    }

    #[test]
    fn iterator_elements_adopt_objects() {
        let src = render(&fixture());
        assert_has(&src, "def self.scan()");
        assert_has(
            &src,
            "has_item = weaveffi_kv_ScanIterator_next(iter, out_item, item_err)",
        );
        assert_has(&src, "y << Store._from_ptr(item_ptr)");
        assert_has(
            &src,
            "weaveffi_kv_ScanIterator_destroy(iter) unless iter.null?",
        );
    }

    #[test]
    fn callback_interface_renders_module_vtable_and_trampolines() {
        let src = render(&fixture());
        // Consumer-facing module with NotImplementedError defaults.
        assert_has(&src, "module Listener\n");
        assert_has(&src, "def on_message(text, weight)");
        assert_has(&src, "def on_bundle(bundle, store, alt)");
        assert_has(&src, "raise NotImplementedError");
        // Vtable layout: methods in order, then free.
        assert_has(&src, "class WvListenerVtable < FFI::Struct");
        assert_has(
            &src,
            "layout :on_message, :pointer,\n           :on_bundle, :pointer,\n           :free, :pointer",
        );
        // Trampolines match the C vtable entry signatures.
        assert_has(
            &src,
            "WV_LISTENER_ON_MESSAGE = FFI::Function.new(:int32, [:pointer, :string, :int32, :pointer]) do |ctx, text, weight, out_err|",
        );
        assert_has(
            &src,
            "WV_LISTENER_ON_BUNDLE = FFI::Function.new(:void, [:pointer, :pointer, :size_t, :pointer, :pointer, :pointer]) do |ctx, bundle_ptr, bundle_len, store, alt, out_err|",
        );
        assert_has(&src, "impl = _wv_cb_lookup(ctx)");
        assert_has(&src, "impl.on_message(text_v, weight_v) ? 1 : 0");
        // Borrowed buffer decoded before the call; objects adopted.
        assert_has(&src, "bundle_v = _wv_read_bundle(bundle_r)");
        assert_has(&src, "bundle_r.expect_end!");
        assert_has(&src, "store_v = Store._from_ptr(store)");
        assert_has(&src, "alt_v = alt.null? ? nil : Store._from_ptr(alt)");
        // Failure path: error_set with -4 and a default return.
        assert_has(
            &src,
            "rescue Exception => e\n      _wv_cb_fail(out_err, e)\n      0\n",
        );
        assert_has(
            &src,
            "rescue Exception => e\n      _wv_cb_fail(out_err, e)\n      nil\n",
        );
        assert_has(
            &src,
            "weaveffi_error_set(out_err, FOREIGN_ERROR_CODE, message)",
        );
        // Free drops the registry entry.
        assert_has(
            &src,
            "WV_LISTENER_FREE = FFI::Function.new(:void, [:pointer]) do |ctx|",
        );
        assert_has(&src, "_wv_cb_free(ctx)");
        // One static vtable, filled with the pinned trampolines.
        assert_has(&src, "WV_LISTENER_VTABLE = WvListenerVtable.new");
        assert_has(
            &src,
            "WV_LISTENER_VTABLE[:on_message] = WV_LISTENER_ON_MESSAGE",
        );
        assert_has(
            &src,
            "WV_LISTENER_VTABLE[:on_bundle] = WV_LISTENER_ON_BUNDLE",
        );
        assert_has(&src, "WV_LISTENER_VTABLE[:free] = WV_LISTENER_FREE");
        assert_eq!(src.matches("= WvListenerVtable.new").count(), 1);
    }

    #[test]
    fn passing_a_callback_registers_ctx_and_static_vtable() {
        let src = render(&fixture());
        assert_has(&src, "def self.subscribe(listener)");
        assert_has(&src, "listener_ctx = _wv_cb_register(listener)");
        assert_has(
            &src,
            "weaveffi_kv_subscribe(listener_ctx, WV_LISTENER_VTABLE.to_ptr, err)",
        );
        assert_has(&src, "def self._wv_cb_register(impl)");
        assert_has(&src, "def self._wv_cb_lookup(ctx)");
        assert_has(&src, "def self._wv_cb_free(ctx)");
    }

    #[test]
    fn registry_is_omitted_without_callback_interfaces() {
        let api = ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: "math".into(),
                doc: None,
                functions: vec![func(
                    "add",
                    vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                    Some(TypeRef::I32),
                )],
                interfaces: vec![],
                callback_interfaces: vec![],
                structs: vec![],
                enums: vec![],
                errors: None,
                modules: vec![],
            }],
        });
        let src = render(&BindingModel::build(&api, "weaveffi"));
        assert!(!src.contains("_wv_cb_register"), "{src}");
        assert_has(&src, "attach_function :weaveffi_error_set");
    }

    /// ffi's `Pointer#read_string` and `:string` callback slots produce
    /// BINARY-encoded Strings; every C string the ABI guarantees is UTF-8 must
    /// be retagged, or non-ASCII text fails to compare equal to a literal.
    #[test]
    fn c_strings_are_retagged_as_utf8() {
        let src = render(&fixture());
        // Direct string return.
        assert_has(
            &src,
            "str = result.read_string.force_encoding(Encoding::UTF_8)",
        );
        // Callback-interface string parameter.
        assert_has(
            &src,
            "text_v = text.nil? ? '' : text.force_encoding(Encoding::UTF_8)",
        );
        // Error messages (generic checker).
        assert_has(
            &src,
            "msg = msg_ptr.null? ? '' : msg_ptr.read_string.force_encoding(Encoding::UTF_8)",
        );
        // Every remaining bare read_string is a length-delimited byte copy.
        for line in src.lines().filter(|l| l.contains(".read_string")) {
            assert!(
                line.contains(".read_string(") || line.contains("force_encoding(Encoding::UTF_8)"),
                "untagged C string read in: {line}"
            );
        }
    }

    #[test]
    fn rendering_is_deterministic() {
        let model = fixture();
        assert_eq!(render(&model), render(&model));
    }

    #[test]
    fn package_skips_platforms_without_a_gem_string() {
        let model = fixture();
        let api = ResolvedApi::assume_valid(Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![],
        });
        let mut binaries = BinarySet::new("weaveffi");
        binaries.insert(Platform::MacosArm64, "/tmp/darwin-arm64/libweaveffi.dylib");
        binaries.insert(Platform::AndroidArm64, "/tmp/android-arm64/libweaveffi.so");
        binaries.insert(Platform::Wasm32, "/tmp/wasm32/weaveffi.wasm");
        let ctx = PackageContext {
            binaries: &binaries,
            input_basename: Some("weaveffi.yml"),
        };
        let files = RubyGenerator
            .package(
                &api,
                &model,
                &ctx,
                Utf8Path::new("out"),
                &RubyConfig::default(),
            )
            .expect("ruby supports packaging");
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert!(
            paths
                .iter()
                .all(|p| p.starts_with("out/ruby/darwin-arm64/")),
            "{paths:?}"
        );
        assert!(paths.iter().any(|p| p.ends_with("weaveffi.gemspec")));
        let gemspec = files
            .iter()
            .find(|f| f.path.as_str().ends_with("weaveffi.gemspec"))
            .unwrap();
        let weaveffi_core::package::FileContent::Text(text) = &gemspec.content else {
            panic!("gemspec is text");
        };
        assert_has(text, "s.platform    = 'arm64-darwin'");
    }
}
