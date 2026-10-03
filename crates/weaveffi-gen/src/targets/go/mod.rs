//! Go (cgo) binding generator for WeaveFFI.
//!
//! Emits a self-contained Go module under `go/`: `go.mod`, a copy of the C
//! header, `bindings.go` (the API), `runtime.go` (error, string, object, and
//! async plumbing), and, when the API has buffered values, `codec.go` (the
//! value-buffer writer and reader). The module path defaults to the package
//! name and the Go package name is the C prefix; the bindings link
//! `-l{library}`. Implements [`LanguageBackend`]; the shared driver bridges
//! it into the generator pipeline.
//!
//! Records, rich enums, optionals, lists, and maps are value types that
//! cross the C ABI serialized in the value-buffer format (one
//! `const uint8_t*` + `size_t` pair), with one pack and one unpack function
//! per record and rich enum. Strings and bytes cross as borrowed
//! `(ptr, len)` views of Go memory and come back as producer allocations the
//! bindings copy and release with `{prefix}_free_bytes`.
//!
//! Interfaces are reference-counted objects: each Go wrapper holds one strong
//! reference released by `Close` or, as a backstop, by a finalizer, and an
//! object inside a value buffer crosses as a cloned-reference token.
//! Callback interfaces are Go `interface` types the consumer implements; an
//! implementation crosses as a `cgo.Handle` plus the address of one static
//! vtable per interface, filled with exported Go trampolines that recover
//! panics into the producer's error slot. Async functions take a
//! `context.Context`, and a cancellable one cancels its native token when
//! the context is done.

mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::lang;
use crate::package::{PackageContext, PackagedFile};
use crate::targets::c::render_c_header_from_model;
use crate::utils::{render_prelude, render_trailer, wrapper_name, CommentStyle};
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use weaveffi_model::model::{
    checksum_symbol, BindingModel, CallShape, CallbackInterfaceBinding, EnumBinding, ErrorBinding,
    FnBinding, InterfaceBinding, ModuleBinding, StructBinding,
};
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::go::calls::{
    collect_preamble_decls, render_async_function, render_callback_interface, render_function,
    ErrCtx,
};
use crate::targets::go::entities::{
    domain_stem, render_enum, render_error, render_interface, render_rich_enum, render_struct,
};
use crate::targets::go::package::{package_files, render_go_mod, render_readme};
use crate::targets::go::runtime::{render_codec, render_runtime, RuntimeNames};

/// Per-target configuration for [`GoGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoConfig {
    /// Go module path written to `go.mod`. Defaults to the package name.
    pub module_path: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// package-level function names, so module `kv`'s `delete` surfaces as
    /// `Delete` rather than `KvDelete`. Set to `false` to restore the
    /// module-prefixed spelling. Interface members are namespaced by their
    /// wrapper type and never carry the module prefix.
    pub strip_module_prefix: bool,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
    /// The identity-derived names the entity hooks render with, filled in by
    /// [`render_bindings`] for the duration of one render.
    #[serde(skip)]
    render: RenderNames,
}

/// What the [`LanguageBackend`] entity hooks need beyond the user's
/// configuration, carried on a per-render copy of [`GoConfig`].
#[derive(Debug, Clone, Default)]
struct RenderNames {
    /// The C symbol prefix.
    prefix: String,
    /// The Go package name.
    package: String,
    /// Base C symbols of the free functions that keep their module prefix
    /// even when `strip_module_prefix` is on (see [`colliding_functions`]).
    prefixed: BTreeSet<String>,
}

impl Default for GoConfig {
    fn default() -> Self {
        Self {
            module_path: None,
            strip_module_prefix: true,
            input_basename: None,
            render: RenderNames::default(),
        }
    }
}

impl GoConfig {
    /// The input IDL basename embedded in generated file headers, falling
    /// back to `"api.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }
}

/// Every identity-derived name the generated module uses.
pub(crate) struct Names {
    /// The Go module path (`go.mod`): the configured `module_path`, else the
    /// package name.
    pub(crate) module_path: String,
    /// The Go package name: the C prefix, escaped if it's a Go keyword.
    pub(crate) package: String,
    /// The native library base name the bindings link (`-l{library}`).
    pub(crate) library: String,
    /// The bundled C header's file name, `{library}.h`.
    pub(crate) header: String,
}

impl Names {
    pub(crate) fn new(api: &ResolvedApi, config: &GoConfig) -> Self {
        let id = api.identity();
        Self {
            module_path: config
                .module_path
                .as_deref()
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .unwrap_or(&id.name)
                .to_string(),
            package: lang::escape_ident(&id.prefix, lang::GO_KEYWORDS),
            library: id.library.clone(),
            header: crate::targets::c::header_name(api),
        }
    }
}

/// Go backend: emits a cgo module binding the C ABI exposed by the
/// underlying cdylib.
pub struct GoGenerator;

impl LanguageBackend for GoGenerator {
    type Config = GoConfig;

    fn name(&self) -> &'static str {
        "go"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn render_enum(&self, out: &mut String, e: &EnumBinding, config: &Self::Config) {
        // A plain C-style enum becomes an `int32` + constants; a rich
        // (algebraic) enum becomes a sealed sum type. Each renderer skips
        // the other kind.
        render_enum(out, e);
        render_rich_enum(out, &config.render.package, e);
    }

    fn render_struct(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        s: &StructBinding,
        _config: &Self::Config,
    ) {
        render_struct(out, s);
    }

    fn render_error(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        e: &ErrorBinding,
        _config: &Self::Config,
    ) {
        // Emitted once, in the declaring module; inheriting submodules
        // reference the ancestor's type through `wvMap{Stem}`.
        render_error(out, module, e);
    }

    fn render_callback_interface(
        &self,
        out: &mut String,
        _module: &ModuleBinding,
        cb: &CallbackInterfaceBinding,
        config: &Self::Config,
    ) {
        render_callback_interface(out, &config.render.prefix, cb);
    }

    fn render_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        i: &InterfaceBinding,
        config: &Self::Config,
    ) {
        let stem = domain_stem(module);
        let r = &config.render;
        render_interface(out, &r.prefix, &r.package, i, stem.as_deref());
    }

    fn render_function(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        f: &FnBinding,
        config: &Self::Config,
    ) {
        let strip = config.strip_module_prefix && !config.render.prefixed.contains(&f.c_base);
        let go_name = wrapper_name(&module.path, &f.name, strip).to_upper_camel_case();
        let stem = domain_stem(module);
        let err = ErrCtx::of(f, stem.as_deref());
        let prefix = &config.render.prefix;
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, f, ab, &go_name, None, err);
        } else {
            render_function(out, prefix, f, &go_name, None, err);
        }
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("go");
        render_files(api, model, config)
            .into_iter()
            .map(|(name, contents)| OutputFile::new(dir.join(name), contents))
            .collect()
    }

    fn package(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        Some(package_files(api, model, ctx, out_dir, config))
    }
}

/// Every file of the generated module, as `(file name, contents)` pairs.
pub(crate) fn render_files(
    api: &ResolvedApi,
    model: &BindingModel,
    config: &GoConfig,
) -> Vec<(String, String)> {
    let input_basename = config.input_basename();
    let names = Names::new(api, config);
    let runtime_names = RuntimeNames {
        package: &names.package,
        prefix: &model.prefix,
        header: &names.header,
    };
    let mut files = vec![
        (
            "go.mod".to_string(),
            render_go_mod(&names.module_path, input_basename),
        ),
        (
            "README.md".to_string(),
            render_readme(&names, input_basename),
        ),
        (
            names.header.clone(),
            render_c_header_from_model(model, input_basename, &names.header),
        ),
        (
            "bindings.go".to_string(),
            render_bindings(model, config, &names),
        ),
        (
            "runtime.go".to_string(),
            render_runtime(&runtime_names, input_basename),
        ),
    ];
    if model.has_buffers() {
        files.push((
            "codec.go".to_string(),
            render_codec(&runtime_names, input_basename),
        ));
    }
    files
}

// ── Name collisions ──

/// The free functions whose module-stripped Go name would clash in the one
/// flat package the module tree renders into: with a declared type,
/// constant, or interface factory or static, with the runtime's exported
/// names, or with another module's function of the same name. Those keep
/// their module prefix (`directory.card` beside a `Card` record surfaces as
/// `DirectoryCard`).
fn colliding_functions(model: &BindingModel) -> BTreeSet<String> {
    let mut taken: BTreeSet<String> = ["Error", "DebugLive"].map(String::from).into();
    for m in &model.modules {
        if let Some(e) = m.error.as_ref().filter(|e| e.declared_here) {
            taken.insert(e.type_name.clone());
            for c in &e.codes {
                let code = format!("{}{}", e.type_name, c.name.to_upper_camel_case());
                taken.insert(format!("{code}Payload"));
                taken.insert(code);
            }
        }
        for e in &m.enums {
            let name = e.name.to_upper_camel_case();
            for v in &e.variants {
                taken.insert(format!("{name}{}", v.name.to_upper_camel_case()));
            }
            taken.insert(name);
        }
        let types = m
            .structs
            .iter()
            .map(|s| &s.name)
            .chain(m.callback_interfaces.iter().map(|c| &c.name));
        taken.extend(types.map(|n| n.to_upper_camel_case()));
        for i in &m.interfaces {
            let name = i.name.to_upper_camel_case();
            for c in &i.constructors {
                taken.insert(format!("{}{name}", c.name.to_upper_camel_case()));
            }
            for f in &i.statics {
                taken.insert(format!("{name}{}", f.name.to_upper_camel_case()));
            }
            taken.insert(name);
        }
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for (_, f) in model.functions() {
        *seen.entry(f.name.to_upper_camel_case()).or_default() += 1;
    }
    model
        .functions()
        .filter(|(_, f)| {
            let name = f.name.to_upper_camel_case();
            taken.contains(&name) || seen[&name] > 1
        })
        .map(|(_, f)| f.c_base.clone())
        .collect()
}

// ── Import scanning ──

/// The standard-library packages `bindings.go` imports, computed by one pass
/// over the lowered model. Everything else lives in `runtime.go` and
/// `codec.go`, whose imports are fixed.
#[derive(Default, Clone, Copy)]
struct Imports {
    /// `context` (async wrappers take a `context.Context`).
    context: bool,
    /// `fmt` (typed error domains format their message).
    fmt: bool,
    /// `iter` (lazy sequences returned by `iter<T>` functions).
    iter: bool,
    /// `runtime` (`SetFinalizer` on object wrappers).
    runtime: bool,
    /// `runtime/cgo` (`cgo.Handle` for callback-interface implementations
    /// and async completion channels).
    cgo: bool,
    /// `unsafe` (object pointers, callback contexts, completion trampolines).
    unsafe_ptr: bool,
    /// The `wvHandlePtr` preamble helper that widens a `cgo.Handle` or an
    /// object token into the `void*` the ABI carries. Doing the integer to
    /// pointer conversion in C keeps `go vet` from flagging the generated
    /// file for a "possible misuse of unsafe.Pointer".
    handle_ptr: bool,
}

fn scan_imports(model: &BindingModel) -> Imports {
    let has_async = model.has_async();
    let has_interfaces = model.has_interfaces();
    let has_callbacks = model.has_callback_interfaces();
    Imports {
        context: has_async,
        fmt: model.modules.iter().any(ModuleBinding::declares_error),
        iter: model.has_iterators(),
        runtime: has_interfaces,
        cgo: has_callbacks || has_async,
        unsafe_ptr: has_interfaces || has_callbacks || has_async,
        handle_ptr: has_interfaces || has_callbacks || has_async,
    }
}

// ── Top-level rendering ──

/// Render `bindings.go`: the cgo preamble, imports, the load-time ABI and
/// checksum checks, and every module's entities and wrappers in the canonical
/// member order (error domain, enums, structs, callback interfaces,
/// interfaces, functions).
pub(crate) fn render_bindings(model: &BindingModel, config: &GoConfig, names: &Names) -> String {
    let prefix = model.prefix.as_str();
    let imports = scan_imports(model);
    let mut out = render_prelude(CommentStyle::DoubleSlash, config.input_basename());

    out.push_str(&format!("package {}\n\n", names.package));
    out.push_str("/*\n");
    out.push_str(&format!("#cgo LDFLAGS: -l{}\n", names.library));
    if model.callables().any(|(_, f)| f.deprecated.is_some()) {
        // The bindings call deprecated functions on the user's behalf; only
        // the user's own calls should warn, through Go's `Deprecated:` docs.
        out.push_str("#cgo CFLAGS: -Wno-deprecated-declarations\n");
    }
    out.push_str(&format!("#include \"{}\"\n", names.header));
    if imports.handle_ptr {
        // Each preamble helper is compiled into every cgo translation unit,
        // some of which never call it.
        out.push_str(
            "__attribute__((unused)) static void* wvHandlePtr(uintptr_t h) { return (void*)h; }\n",
        );
    }
    // Forward declarations for the //export trampolines below (mirroring the
    // const-free prototypes cgo emits into _cgo_export.h) and the static
    // vtable of each callback interface.
    for decl in collect_preamble_decls(model) {
        out.push_str(&decl);
        out.push('\n');
    }
    out.push_str("*/\n");
    out.push_str("import \"C\"\n");

    let packages = [
        (imports.context, "context"),
        (imports.fmt, "fmt"),
        (imports.iter, "iter"),
        (imports.runtime, "runtime"),
        (imports.cgo, "runtime/cgo"),
        (imports.unsafe_ptr, "unsafe"),
    ];
    let used: Vec<&str> = packages
        .iter()
        .filter(|(on, _)| *on)
        .map(|(_, p)| *p)
        .collect();
    if !used.is_empty() {
        out.push_str("\nimport (\n");
        for p in used {
            out.push_str(&format!("\t\"{p}\"\n"));
        }
        out.push_str(")\n");
    }
    out.push('\n');

    // Load-time checks: the ABI revision, then each top-level module's
    // contract checksum.
    out.push_str("func init() {\n\twvCheckABI()\n");
    for root in model.roots() {
        let checksum = root.checksum.expect("top-level modules carry a checksum");
        out.push_str(&format!(
            "\twvCheckModule(\"{}\", {checksum:#018x}, uint64(C.{}()))\n",
            root.name,
            checksum_symbol(prefix, &root.name)
        ));
    }
    out.push_str("}\n\n");

    let mut config = config.clone();
    config.render = RenderNames {
        prefix: prefix.to_string(),
        package: names.package.clone(),
        prefixed: colliding_functions(model),
    };
    for m in &model.modules {
        GoGenerator.emit_members(&mut out, m, &config);
    }

    // Exactly one blank line before the trailer keeps the file gofmt-clean.
    out.truncate(out.trim_end().len());
    out.push_str("\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "bindings.go"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn named(n: &str) -> TypeRef {
        TypeRef::Named(n.into())
    }

    fn optional(t: TypeRef) -> TypeRef {
        TypeRef::Optional(Box::new(t))
    }

    /// One module exercising every shape: an interface with a constructor
    /// and a method, `Interface?` in and out, a record with `Interface` and
    /// `[Interface]` fields, an iterator of interfaces, a callback interface
    /// taking a string, an i32, a record, and an object (one method returning
    /// `bool`, one returning void), a function taking that callback
    /// interface, and plain and cancellable async functions.
    fn fixture() -> ResolvedApi {
        let module = Module {
            name: "bus".into(),
            doc: None,
            functions: vec![
                func(
                    "pick",
                    vec![param("preferred", optional(named("Ticker")))],
                    Some(optional(named("Ticker"))),
                ),
                func(
                    "all_tickers",
                    vec![],
                    Some(TypeRef::Iterator(Box::new(named("Ticker")))),
                ),
                func(
                    "subscribe",
                    vec![param("subscriber", named("Subscriber"))],
                    None,
                ),
                func(
                    "echo",
                    vec![param("text", TypeRef::StringUtf8)],
                    Some(TypeRef::StringUtf8),
                ),
                Function {
                    r#async: true,
                    ..func(
                        "fetch",
                        vec![param("id", TypeRef::I32)],
                        Some(named("Ticker")),
                    )
                },
                Function {
                    r#async: true,
                    cancellable: true,
                    ..func("wait", vec![param("ctx", TypeRef::I64)], None)
                },
            ],
            interfaces: vec![InterfaceDef {
                name: "Ticker".into(),
                doc: Some("A ticking counter.".into()),
                deprecated: None,
                constructors: vec![func("new", vec![param("start", TypeRef::I64)], None)],
                methods: vec![func("value", vec![], Some(TypeRef::I64))],
                statics: vec![],
            }],
            callback_interfaces: vec![CallbackInterfaceDef {
                name: "Subscriber".into(),
                doc: Some("Receives bus events.".into()),
                deprecated: None,
                methods: vec![
                    func(
                        "on_message",
                        vec![
                            param("text", TypeRef::StringUtf8),
                            param("weight", TypeRef::I32),
                            param("envelope", named("Envelope")),
                        ],
                        None,
                    ),
                    func(
                        "on_ticker",
                        vec![param("ticker", named("Ticker"))],
                        Some(TypeRef::Bool),
                    ),
                ],
            }],
            structs: vec![StructDef {
                name: "Envelope".into(),
                doc: None,
                deprecated: None,
                fields: vec![
                    field("topic", TypeRef::StringUtf8),
                    field("primary", named("Ticker")),
                    field("others", TypeRef::List(Box::new(named("Ticker")))),
                ],
            }],
            enums: vec![],
            errors: None,
            modules: vec![],
        };
        ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module],
        })
        .with_identity(Identity::named("acme-bus"))
    }

    fn render_all(config: &GoConfig) -> Vec<(String, String)> {
        let api = fixture();
        let model = BindingModel::build(&api);
        render_files(&api, &model, config)
    }

    fn render(config: &GoConfig) -> String {
        render_all(config)
            .into_iter()
            .find(|(n, _)| n == "bindings.go")
            .map(|(_, c)| c)
            .expect("bindings.go")
    }

    fn assert_has(src: &str, needle: &str) {
        assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
    }

    #[test]
    fn identity_drives_every_name() {
        let files = render_all(&GoConfig::default());
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "go.mod",
                "README.md",
                "acme_bus.h",
                "bindings.go",
                "runtime.go",
                "codec.go"
            ]
        );
        let get = |n: &str| &files.iter().find(|(f, _)| f == n).unwrap().1;
        assert_has(get("go.mod"), "module acme-bus\n");
        let src = get("bindings.go");
        assert_has(src, "package acme_bus\n");
        assert_has(src, "#cgo LDFLAGS: -lacme_bus\n#include \"acme_bus.h\"\n");
        assert_has(src, "\twvCheckABI()\n\twvCheckModule(\"bus\", 0x");
        assert_has(src, "uint64(C.acme_bus_bus_checksum()))");
        let rt = get("runtime.go");
        assert_has(rt, "package acme_bus\n");
        assert_has(rt, "C.acme_bus_free_bytes(ptr, n)");
        assert_has(rt, "const wvABIVersion uint32 = 3\n");
        assert!(!rt.contains("{{"), "unsubstituted placeholder");
        for (name, contents) in &files {
            if name.ends_with(".go") || name == "go.mod" {
                let body: String = contents.lines().skip(3).collect::<Vec<_>>().join("\n");
                assert!(
                    !body.to_lowercase().contains("weaveffi"),
                    "{name} is branded:\n{body}"
                );
            }
        }

        let custom = render_all(&GoConfig {
            module_path: Some("example.com/bus".into()),
            ..GoConfig::default()
        });
        assert_has(&custom[0].1, "module example.com/bus\n");
    }

    #[test]
    fn preamble_declares_trampolines_and_one_static_vtable() {
        let src = render(&GoConfig::default());
        assert_has(
            &src,
            "extern void goWv_acme_bus_bus_Subscriber_on_message(void* ctx, uint8_t* text_ptr, size_t text_len, int32_t weight, uint8_t* envelope_ptr, size_t envelope_len, acme_bus_error* out_err);",
        );
        assert_has(
            &src,
            "extern bool goWv_acme_bus_bus_Subscriber_on_ticker(void* ctx, acme_bus_bus_Ticker* ticker, acme_bus_error* out_err);",
        );
        assert_has(
            &src,
            "static const acme_bus_bus_Subscriber_vtable wvVtable_acme_bus_bus_Subscriber_vtable = {",
        );
        assert_has(
            &src,
            "(void (*)(void*, const uint8_t*, size_t, int32_t, const uint8_t*, size_t, acme_bus_error*))goWv_acme_bus_bus_Subscriber_on_message,",
        );
        assert_has(&src, "    goWv_acme_bus_bus_Subscriber_on_ticker,");
        assert_has(&src, "    goWv_acme_bus_bus_Subscriber_free,\n};");
        assert_has(
            &src,
            "extern void goWv_acme_bus_bus_fetch_callback(void* context_, acme_bus_error* err, acme_bus_bus_Ticker* result);",
        );
    }

    #[test]
    fn strings_cross_as_borrowed_views_and_owned_runs() {
        let src = render(&GoConfig::default());
        assert_has(&src, "cTextPtr, cTextLen := wvStr(text)");
        assert_has(&src, "var cRetLen C.size_t");
        assert_has(
            &src,
            "cRet := C.acme_bus_bus_echo(cTextPtr, cTextLen, &cRetLen, &cErr)",
        );
        assert_has(&src, "return wvTakeString(cRet, cRetLen)");
        assert!(!src.contains("CString"), "no NUL-terminated strings");
        assert_has(&src, "arg0 := wvBorrowString(text_ptr, text_len)");
    }

    #[test]
    fn callback_interface_renders_go_interface_and_trampolines() {
        let src = render(&GoConfig::default());
        assert_has(&src, "type Subscriber interface {");
        assert_has(
            &src,
            "\tOnMessage(text string, weight int32, envelope Envelope)\n",
        );
        assert_has(&src, "\tOnTicker(ticker *Ticker) bool\n");
        assert_has(&src, "wvForeignError(out_err, r)");
        assert_has(&src, "arg1 := int32(weight)");
        assert_has(
            &src,
            "rArg2 := &wvReader{buf: wvBorrowBytes(envelope_ptr, envelope_len)}",
        );
        assert_has(&src, "arg0 := wvAdoptTicker(ticker)");
        assert_has(&src, "ret = C._Bool(impl.OnTicker(arg0))");
        assert_has(&src, "hSubscriber := cgo.NewHandle(subscriber)");
        assert_has(
            &src,
            "C.acme_bus_bus_subscribe(C.wvHandlePtr(C.uintptr_t(hSubscriber)), C.wvVtablePtr_acme_bus_bus_Subscriber_vtable(), &cErr)",
        );
    }

    #[test]
    fn interface_wrapper_guards_its_reference() {
        let src = render(&GoConfig::default());
        assert_has(&src, "type Ticker struct {\n\tref wvRef\n}");
        assert_has(&src, "s.ref.init(unsafe.Pointer(ptr), wvDestroyTicker)");
        assert_has(
            &src,
            "func (s *Ticker) Close() error {\n\truntime.SetFinalizer(s, nil)\n\ts.ref.close()\n\treturn nil\n}",
        );
        assert_has(
            &src,
            "func (s *Ticker) Value() int64 {\n\tcSelf := s.native()\n\tdefer s.ref.release()\n",
        );
        assert_has(&src, "panic(\"acme_bus: nil *Ticker\")");
        assert_has(
            &src,
            "var cPreferred *C.acme_bus_bus_Ticker\n\tif preferred != nil {\n\t\tcPreferred = preferred.native()\n\t\tdefer preferred.ref.release()\n\t}",
        );
        assert_has(&src, "v.Primary = wvUntokenTicker(r.readU64())");
    }

    #[test]
    fn async_functions_take_a_context_and_cancel() {
        let src = render(&GoConfig::default());
        assert_has(
            &src,
            "func Fetch(ctx context.Context, id int32) (*Ticker, error) {",
        );
        assert_has(&src, "res, fail, err := wvAwait(ctx, wvDone, nil)");
        assert_has(
            &src,
            "wvComplete(context_, err, func() *Ticker {\n\t\treturn wvAdoptTicker(result)",
        );
        assert_has(&src, "func Wait(ctx context.Context, ctx_ int64) error {");
        assert_has(&src, "wvToken := C.acme_bus_cancel_token_create()");
        assert_has(&src, "defer C.acme_bus_cancel_token_destroy(wvToken)");
        assert_has(
            &src,
            "C.acme_bus_bus_wait(C.int64_t(ctx_), wvToken, C.acme_bus_bus_wait_callback(unsafe.Pointer(C.goWv_acme_bus_bus_wait_callback)), C.wvHandlePtr(C.uintptr_t(wvHandle)))",
        );
        assert_has(&src, "_, fail, err := wvAwait(ctx, wvDone, wvToken)");
    }

    #[test]
    fn iterators_are_lazy_sequences() {
        let src = render(&GoConfig::default());
        assert_has(&src, "func AllTickers() iter.Seq[*Ticker] {");
        assert_has(&src, "var cItem *C.acme_bus_bus_Ticker");
        assert_has(
            &src,
            "more := C.acme_bus_bus_AllTickersIterator_next(cIter, &cItem, &cIterErr) != 0",
        );
        assert_has(&src, "item := wvAdoptTicker(cItem)");
        assert_has(
            &src,
            "defer C.acme_bus_bus_AllTickersIterator_destroy(cIter)",
        );
    }

    #[test]
    fn undocumented_enum_members_are_gofmt_aligned() {
        use weaveffi_model::ir::{EnumDef, EnumVariant};
        let variant = |name: &str, value: i32, fields: Vec<StructField>| EnumVariant {
            name: name.into(),
            value,
            doc: None,
            fields,
        };
        let module = Module {
            name: "paint".into(),
            doc: None,
            functions: vec![],
            interfaces: vec![],
            callback_interfaces: vec![],
            structs: vec![],
            enums: vec![
                EnumDef {
                    name: "Channel".into(),
                    doc: None,
                    deprecated: None,
                    variants: vec![
                        variant("red", 0, vec![]),
                        variant("green", 1, vec![]),
                        variant("blue", 2, vec![]),
                    ],
                },
                EnumDef {
                    name: "Shape".into(),
                    doc: None,
                    deprecated: None,
                    variants: vec![variant(
                        "rectangle",
                        0,
                        vec![field("width", TypeRef::F32), field("height", TypeRef::F32)],
                    )],
                },
            ],
            errors: None,
            modules: vec![],
        };
        let api = ResolvedApi::assume_valid(Api {
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module],
        });
        let config = GoConfig::default();
        let model = BindingModel::build(&api);
        let src = render_bindings(&model, &config, &Names::new(&api, &config));
        // gofmt pads a run of undocumented const and field names to one
        // column; emitting it pre-aligned keeps the file gofmt-clean.
        assert_has(
            &src,
            "const (\n\tChannelRed   Channel = 0\n\tChannelGreen Channel = 1\n\tChannelBlue  Channel = 2\n)",
        );
        assert_has(
            &src,
            "type ShapeRectangle struct {\n\tWidth  float32\n\tHeight float32\n}",
        );
    }

    #[test]
    fn output_is_deterministic() {
        assert_eq!(
            render_all(&GoConfig::default()),
            render_all(&GoConfig::default())
        );
    }
}
