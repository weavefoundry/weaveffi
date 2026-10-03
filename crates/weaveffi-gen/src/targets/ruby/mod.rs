//! Ruby (ffi gem) binding generator.
//!
//! Emits a gem that binds the C ABI (revision 3) with the `ffi` gem:
//! `lib/{prefix}.rb` (the generated bindings, required as `{prefix}`),
//! `lib/{prefix}/runtime.rb` (the fixed runtime from `runtime/runtime.rb`),
//! a `{name}.gemspec`, and a README. Everything lives in one Ruby module,
//! `PascalCase(name)` unless configured. Interfaces become reference-counted
//! wrapper classes (`close` plus a GC finalizer backstop, `dup`/`clone` for a
//! second reference), records and rich enums become value classes packed
//! into value buffers, async functions block on a queue fed by a
//! module-level completion trampoline (cancellable ones take a `cancel:`
//! token), iterators are lazy `Enumerator`s, and callback interfaces are
//! duck-typed modules backed by one static vtable of pinned `FFI::Function`
//! trampolines per interface.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::package::{PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::{checksum_symbol, BindingModel};
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::ruby::calls::{render_attach_function, render_callable, RbScope};
use crate::targets::ruby::entities::{
    render_enum, render_error, render_interface_class, render_interface_ffi,
    render_rich_enum_class, render_struct_class,
};
use crate::targets::ruby::package::{
    render_gemspec, render_packaged_gemspec, render_packaged_readme, render_readme, GemNames,
};
use crate::targets::ruby::runtime::render_runtime;

/// Per-target configuration for [`RubyGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RubyConfig {
    /// Top-level Ruby module name (default: the package name in PascalCase).
    pub module_name: Option<String>,
    /// Gem name written into the gemspec (default: the package name).
    pub gem_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted Ruby method names, so a `contacts` module exports
    /// `create_contact` rather than `contacts_create_contact`. Set to
    /// `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
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
            input_basename: None,
        }
    }
}

impl RubyConfig {
    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"api.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }

    /// The gem, module, and require names for `api`: the identity's, unless
    /// overridden here.
    fn names<'a>(&self, api: &'a ResolvedApi) -> GemNames<'a> {
        let identity = api.identity();
        let configured = |v: &Option<String>| {
            v.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        GemNames {
            identity,
            gem: configured(&self.gem_name).unwrap_or_else(|| identity.name.clone()),
            module: configured(&self.module_name).unwrap_or_else(|| identity.pascal_name()),
        }
    }
}

/// Ruby backend: emits an `ffi`-gem package binding the C ABI exposed by the
/// underlying cdylib.
pub struct RubyGenerator;

impl LanguageBackend for RubyGenerator {
    type Config = RubyConfig;

    fn name(&self) -> &'static str {
        "ruby"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let names = config.names(api);
        let input_basename = config.input_basename();
        let dir = out_dir.join("ruby");
        let lib_dir = dir.join("lib");
        let lib_file = format!("{}.rb", names.require());
        vec![
            OutputFile::new(
                lib_dir.join(&lib_file),
                render_bindings(model, &names, config, &lib_file),
            ),
            OutputFile::new(
                lib_dir.join(names.require()).join("runtime.rb"),
                render_runtime(names.identity, &names.module, input_basename, "runtime.rb"),
            ),
            OutputFile::new(
                dir.join(names.gemspec_file()),
                render_gemspec(&names, input_basename),
            ),
            OutputFile::new(dir.join("README.md"), render_readme(&names, input_basename)),
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
        let names = config.names(api);
        let input_basename = config.input_basename();
        let lib_file = format!("{}.rb", names.require());
        let bindings = render_bindings(model, &names, config, &lib_file);
        let runtime = render_runtime(names.identity, &names.module, input_basename, "runtime.rb");
        let readme = render_packaged_readme(&names, input_basename);

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
                bindings.clone(),
            ));
            files.push(PackagedFile::text(
                lib_dir.join(names.require()).join("runtime.rb"),
                runtime.clone(),
            ));
            // The runtime loader looks for the bundled library under
            // `lib/native/` by its platform file name.
            files.push(PackagedFile::copy(
                lib_dir
                    .join("native")
                    .join(platform.lib_filename(&names.identity.library)),
                nb.source.clone(),
            ));
            files.push(PackagedFile::text(
                gem_dir.join(names.gemspec_file()),
                render_packaged_gemspec(&names, ruby_platform, input_basename),
            ));
            files.push(PackagedFile::text(
                gem_dir.join("README.md"),
                readme.clone(),
            ));
        }
        Some(files)
    }
}

/// Render `lib/{prefix}.rb`: the runtime require, the contract check, then
/// each module's typed error surface, entities, codecs, FFI attachments,
/// callback interfaces, interface classes, and free functions.
fn render_bindings(
    model: &BindingModel,
    names: &GemNames,
    config: &RubyConfig,
    lib_file: &str,
) -> String {
    let module_name = &names.module;
    // Any call can reach a stored callback implementation, so an API with
    // callback interfaces releases the GVL on every call.
    let blocking = model.has_callback_interfaces();
    let mut out = render_prelude(CommentStyle::Hash, config.input_basename());
    out.push_str("# frozen_string_literal: true\n\n");
    out.push_str(&format!(
        "require_relative '{}/runtime'\n\n",
        names.require()
    ));
    out.push_str(&format!("# Ruby bindings for {}.\n", names.identity.name));
    out.push_str(&format!("module {module_name}\n"));
    out.push_str("  # Contract checksums of the top-level modules these bindings were\n");
    out.push_str("  # generated from; a library built from a different API fails to load.\n");
    out.push_str("  _wv_check_contract!(\n");
    for root in model.roots() {
        let checksum = root.checksum.expect("top-level modules carry a checksum");
        out.push_str(&format!(
            "    '{}' => [:{}, 0x{checksum:016x}],\n",
            root.name,
            checksum_symbol(&model.prefix, &root.name)
        ));
    }
    out.push_str("  )\n");
    for m in &model.modules {
        out.push_str(&format!("\n  # === Module: {} ===\n", m.dot_path));
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
        for e in m.enums.iter().filter(|e| e.is_rich()) {
            codec::render_rich_enum_codec(&mut out, e);
        }
        let attaches = !m.interfaces.is_empty() || !m.functions.is_empty();
        if attaches {
            out.push('\n');
        }
        for i in &m.interfaces {
            render_interface_ffi(&mut out, m, i, blocking);
        }
        for f in &m.functions {
            render_attach_function(&mut out, m, f, blocking);
        }
        // Callback interfaces precede interfaces and functions because their
        // static vtables are what interface members and free functions pass.
        for cb in &m.callback_interfaces {
            callbacks::render_callback_interface(&mut out, cb, &model.prefix);
        }
        for i in &m.interfaces {
            render_interface_class(&mut out, m, i, module_name);
        }
        for f in &m.functions {
            let scope = RbScope::Free {
                module_path: &m.path,
                strip_module_prefix: config.strip_module_prefix,
            };
            render_callable(&mut out, m, f, &scope);
        }
    }
    out.push_str("end\n\n");
    out.push_str(&render_trailer(CommentStyle::Hash, lib_file));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{BinarySet, Platform};
    use weaveffi_model::ir::{
        Api, CallbackInterfaceDef, Function, InterfaceDef, Module, Param, StructDef, StructField,
        TypeRef,
    };
    use weaveffi_model::pkg::Identity;

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

    /// One module exercising every object shape plus strings, an iterator,
    /// a callback interface, and a cancellable async method, under the
    /// identity `kv-store` (prefix `kv_store`, module `KvStore`).
    fn fixture() -> ResolvedApi {
        let mut compact = func("compact", vec![], Some(TypeRef::I64));
        compact.r#async = true;
        compact.cancellable = true;
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
                methods: vec![
                    func(
                        "get",
                        vec![param("key", TypeRef::StringUtf8)],
                        Some(TypeRef::StringUtf8),
                    ),
                    compact,
                ],
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
        ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![kv],
        })
        .with_identity(Identity::named("kv-store"))
    }

    fn generate(config: &RubyConfig) -> Vec<OutputFile> {
        let api = fixture();
        let model = BindingModel::build(&api);
        RubyGenerator.files(&api, &model, Utf8Path::new("out"), config)
    }

    fn file<'a>(files: &'a [OutputFile], path: &str) -> &'a str {
        &files
            .iter()
            .find(|f| f.path.as_str() == path)
            .unwrap_or_else(|| {
                panic!(
                    "no {path} in {:?}",
                    files.iter().map(|f| &f.path).collect::<Vec<_>>()
                )
            })
            .contents
    }

    fn bindings() -> String {
        file(
            &generate(&RubyConfig::default()),
            "out/ruby/lib/kv_store.rb",
        )
        .to_string()
    }

    fn assert_has(src: &str, needle: &str) {
        assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
    }

    /// Nothing but the generated-file banner mentions WeaveFFI.
    fn assert_unbranded(src: &str) {
        let banner = |l: &str| {
            l.contains("Generated by WeaveFFI") || l.contains("To regenerate: weaveffi generate")
        };
        for line in src.lines().filter(|l| !banner(l)) {
            assert!(
                !line.to_ascii_lowercase().contains("weaveffi"),
                "branded line: {line}"
            );
        }
    }

    #[test]
    fn layout_follows_the_identity() {
        let files = generate(&RubyConfig::default());
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "out/ruby/lib/kv_store.rb",
                "out/ruby/lib/kv_store/runtime.rb",
                "out/ruby/kv-store.gemspec",
                "out/ruby/README.md",
            ]
        );
        let gemspec = file(&files, "out/ruby/kv-store.gemspec");
        assert_has(gemspec, "s.name        = 'kv-store'");
        assert_has(gemspec, "s.authors     = ['kv-store']");
        let src = file(&files, "out/ruby/lib/kv_store.rb");
        assert_has(src, "require_relative 'kv_store/runtime'");
        assert_has(src, "module KvStore\n");
        for f in &files {
            assert_unbranded(&f.contents);
        }
    }

    #[test]
    fn config_overrides_gem_and_module_names() {
        let config = RubyConfig {
            gem_name: Some("acme-kv".into()),
            module_name: Some("Acme".into()),
            ..RubyConfig::default()
        };
        let files = generate(&config);
        assert_has(
            file(&files, "out/ruby/acme-kv.gemspec"),
            "s.name        = 'acme-kv'",
        );
        // The require path stays the identity prefix.
        let src = file(&files, "out/ruby/lib/kv_store.rb");
        assert_has(src, "module Acme\n");
        assert_has(src, "Acme.kv_store_kv_Store_destroy(ptr)");
        assert_has(
            file(&files, "out/ruby/lib/kv_store/runtime.rb"),
            "module Acme\n",
        );
    }

    #[test]
    fn runtime_loads_the_identity_library_and_checks_abi_3() {
        let files = generate(&RubyConfig::default());
        let rt = file(&files, "out/ruby/lib/kv_store/runtime.rb");
        assert_has(rt, "ENV['KV_STORE_LIBRARY']");
        assert_has(rt, "'libkv_store.dylib'");
        assert_has(rt, "'libkv_store.so'");
        assert_has(rt, "'kv_store.dll'");
        assert_has(rt, &format!("ABI_VERSION = {}", crate::cabi::ABI_VERSION));
        assert_has(rt, "attach_function :kv_store_abi_version, [], :uint32");
        assert_has(
            rt,
            "attach_function :kv_store_free_bytes, [:pointer, :size_t], :void",
        );
        assert_has(
            rt,
            "attach_function :kv_store_cancel_token_create, [], :pointer",
        );
        assert_has(rt, "CANCELLED_ERROR_CODE = -5");
        assert_has(rt, "class Cancelled < Error");
        assert!(!rt.contains("free_string"), "{rt}");
        assert!(!rt.contains("{{"), "unsubstituted placeholder in:\n{rt}");
    }

    #[test]
    fn bindings_check_every_root_checksum_at_load() {
        let api = fixture();
        let model = BindingModel::build(&api);
        let checksum = model.roots().next().unwrap().checksum.unwrap();
        assert_has(
            &bindings(),
            &format!("'kv' => [:kv_store_kv_checksum, 0x{checksum:016x}],"),
        );
    }

    #[test]
    fn interface_wrapper_owns_one_reference() {
        let src = bindings();
        assert_has(
            &src,
            "attach_function :kv_store_kv_Store_clone, [:pointer], :pointer",
        );
        assert_has(
            &src,
            "attach_function :kv_store_kv_Store_destroy, [:pointer], :void, blocking: true",
        );
        assert_has(&src, "class StorePtr < FFI::AutoPointer");
        assert_has(&src, "class Store < WvObject\n    WV_PTR = StorePtr\n");
        assert_has(&src, "KvStore.kv_store_kv_Store_clone(ptr)");
        // The constructor adopts the returned reference; methods pin `self`.
        assert_has(&src, "def initialize(path)");
        assert_has(&src, "_wv_init(result)");
        assert_has(&src, "KvStore._wv_pin(self) do |_wv_self|");
    }

    #[test]
    fn strings_cross_as_pointer_and_length() {
        let src = bindings();
        assert_has(&src, "key_s = KvStore._wv_str(key)");
        assert_has(
            &src,
            "result = KvStore.kv_store_kv_Store_get(_wv_self, key_s, key_s.bytesize, out_len, err)",
        );
        assert_has(
            &src,
            "KvStore._wv_take_string(result, out_len.read(:size_t))",
        );
        // Callback string arguments are borrowed (ptr, len) runs.
        assert_has(&src, "do |ctx, text_ptr, text_len, weight, out_err|");
        assert_has(
            &src,
            "text_v = _wv_borrow_bytes(text_ptr, text_len).force_encoding(Encoding::UTF_8)",
        );
    }

    #[test]
    fn nullable_objects_map_to_nil_in_both_directions() {
        let src = bindings();
        assert_has(&src, "def self.maybe_store(store)");
        assert_has(&src, "_wv_pin(store) do |_wv_store|");
        assert_has(&src, "result.null? ? nil : Store._from_ptr(result)");
    }

    #[test]
    fn records_reserve_object_tokens_and_seal_them_before_the_call() {
        let src = bindings();
        assert_has(&src, "w.write_object(v.primary)");
        assert_has(&src, "bundle_s = _wv_seal(bundle_w).first");
        assert_has(&src, "_wv_primary = Store._from_ptr(r.read_object_token)");
    }

    #[test]
    fn iterator_is_a_lazy_enumerator_adopting_objects() {
        let src = bindings();
        assert_has(&src, "Enumerator.new do |y|");
        assert_has(
            &src,
            "has_item = kv_store_kv_ScanIterator_next(iter, out_item, err)",
        );
        assert_has(&src, "value = Store._from_ptr(item)");
        assert_has(
            &src,
            "kv_store_kv_ScanIterator_destroy(iter) unless iter.null?",
        );
    }

    #[test]
    fn cancellable_async_takes_a_cancel_token() {
        let src = bindings();
        assert_has(&src, "def compact(cancel: nil)");
        assert_has(&src, "own_token = CancelToken.new if cancel.nil?");
        assert_has(
            &src,
            "KvStore.kv_store_kv_Store_compact(_wv_self, token._wv_ptr, KV_STORE_KV_STORE_COMPACT_CALLBACK, ctx)",
        );
        assert_has(&src, "KvStore._wv_async_wait(queue, token)");
        assert_has(&src, "own_token&.close");
        assert_has(
            &src,
            "attach_function :kv_store_kv_Store_compact, [:pointer, :pointer, :pointer, :pointer], :void, blocking: true",
        );
        assert_has(
            &src,
            "KV_STORE_KV_STORE_COMPACT_CALLBACK = FFI::Function.new(:void, [:pointer, :pointer, :int64]) do |ctx, err, result|",
        );
        assert_has(
            &src,
            "rescue Exception => e # rubocop:disable Lint/RescueException\n      queue << e",
        );
    }

    #[test]
    fn callback_interface_renders_module_vtable_and_trampolines() {
        let src = bindings();
        assert_has(&src, "module Listener\n");
        assert_has(&src, "raise NotImplementedError");
        assert_has(&src, "class WvListenerVtable < FFI::Struct");
        assert_has(
            &src,
            "layout :on_message, :pointer,\n           :on_bundle, :pointer,\n           :free, :pointer",
        );
        assert_has(&src, "WV_LISTENER_ON_MESSAGE = FFI::Function.new(:bool,");
        assert_has(&src, "impl.on_message(text_v, weight_v) ? true : false");
        // Objects are adopted before anything that can raise.
        let bundle = src.find("WV_LISTENER_ON_BUNDLE").unwrap();
        let tail = &src[bundle..];
        assert!(
            tail.find("store_v = Store._from_ptr(store)").unwrap()
                < tail.find("bundle_r = ").unwrap()
        );
        assert_has(&src, "alt_v = alt.null? ? nil : Store._from_ptr(alt)");
        assert_has(&src, "WV_LISTENER_VTABLE[:free] = WV_LISTENER_FREE");
        assert_eq!(src.matches("= WvListenerVtable.new").count(), 1);
        // Passing an implementation registers it right before the call.
        assert_has(&src, "listener_ctx = _wv_cb_register(listener)");
        assert_has(
            &src,
            "kv_store_kv_subscribe(listener_ctx, WV_LISTENER_VTABLE.to_ptr, err)",
        );
    }

    #[test]
    fn calls_keep_the_gvl_without_callback_interfaces() {
        let api = ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![Module {
                name: "math".into(),
                doc: None,
                functions: vec![func(
                    "add",
                    vec![param("a", TypeRef::I32), param("b", TypeRef::Bool)],
                    Some(TypeRef::I32),
                )],
                interfaces: vec![],
                callback_interfaces: vec![],
                structs: vec![],
                enums: vec![],
                errors: None,
                modules: vec![],
            }],
        })
        .with_identity(Identity::named("calc"));
        let model = BindingModel::build(&api);
        let files = RubyGenerator.files(&api, &model, Utf8Path::new("out"), &RubyConfig::default());
        let src = file(&files, "out/ruby/lib/calc.rb").to_string();
        assert_has(
            &src,
            "attach_function :calc_math_add, [:int32, :bool, :pointer], :int32\n",
        );
        assert_has(&src, "calc_math_add(a, (b ? true : false), err)");
    }

    #[test]
    fn rendering_is_deterministic() {
        assert_eq!(bindings(), bindings());
    }

    #[test]
    fn package_skips_platforms_without_a_gem_string() {
        let api = fixture();
        let model = BindingModel::build(&api);
        let mut binaries = BinarySet::new("kv_store");
        binaries.insert(Platform::MacosArm64, "/tmp/darwin-arm64/libkv_store.dylib");
        binaries.insert(Platform::AndroidArm64, "/tmp/android-arm64/libkv_store.so");
        binaries.insert(Platform::Wasm32, "/tmp/wasm32/kv_store.wasm");
        let ctx = PackageContext {
            binaries: &binaries,
            input_basename: Some("kv.yml"),
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
        assert!(paths.contains(&"out/ruby/darwin-arm64/lib/native/libkv_store.dylib"));
        assert!(paths.contains(&"out/ruby/darwin-arm64/lib/kv_store/runtime.rb"));
        let gemspec = files
            .iter()
            .find(|f| f.path.as_str().ends_with("kv-store.gemspec"))
            .unwrap();
        let crate::package::FileContent::Text(text) = &gemspec.content else {
            panic!("gemspec is text");
        };
        assert_has(text, "s.platform    = 'arm64-darwin'");
    }
}
