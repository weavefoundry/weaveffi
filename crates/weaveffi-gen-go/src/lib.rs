//! Go (cgo) binding generator for WeaveFFI.
//!
//! Emits a Go module (`go.mod` + package) with cgo bindings over the C ABI
//! (revision 2) exposed by the underlying cdylib. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
//!
//! Records, rich enums, optionals, lists, and maps are value types that
//! cross the C ABI serialized in the WeaveFFI value-buffer format (one
//! `const uint8_t*` + `size_t` pair). The generated package carries a small
//! private writer/reader implementing the wire format, plus one pack and one
//! unpack function per record and rich enum.
//!
//! Interfaces are reference-counted objects: each Go wrapper holds one strong
//! reference released by `Close` or, as a backstop, by a finalizer, and an
//! object inside a value buffer crosses as a cloned-reference token.
//! Callback interfaces are Go `interface` types the consumer implements; an
//! implementation crosses as a `cgo.Handle` plus the address of one static
//! vtable per interface, filled with exported Go trampolines that recover
//! panics into the producer's error slot.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
mod types;

use camino::Utf8Path;
use heck::ToUpperCamelCase;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, ModuleBinding, StructBinding, Ty,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg;
use weaveffi_core::plan::{elem_free, Free};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, wrapper_name, CommentStyle};

use crate::calls::{
    collect_preamble_decls, render_async_function, render_callback_interface, render_function,
    ErrCtx,
};
use crate::entities::{
    domain_stem, render_enum, render_error, render_interface, render_rich_enum, render_struct,
};
use crate::package::{package_files, render_go_mod, render_readme};
use crate::runtime::{
    render_abi_version_check, render_bool_helpers, render_buffer_runtime, render_error_infra,
    render_foreign_error,
};

/// Per-target configuration for [`GoGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoConfig {
    /// Go module path written to `go.mod` (default `"weaveffi"`).
    pub module_path: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// package-level function names, so module `kv`'s `delete` surfaces as
    /// `Delete` rather than `KvDelete`. Set to `false` to restore the
    /// module-prefixed spelling. Interface members are namespaced by their
    /// wrapper type and never carry the module prefix.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the cgo bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for GoConfig {
    fn default() -> Self {
        Self {
            module_path: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl GoConfig {
    /// Returns the configured Go module path, falling back to `"weaveffi"`.
    pub fn module_path(&self) -> &str {
        self.module_path.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Go backend: emits a cgo package (`weaveffi.go`, `go.mod`, and a README)
/// binding the C ABI exposed by the underlying cdylib.
pub struct GoGenerator;

impl LanguageBackend for GoGenerator {
    type Config = GoConfig;

    fn name(&self) -> &'static str {
        "go"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn render_enum(&self, out: &mut String, e: &EnumBinding, _config: &Self::Config) {
        // A plain C-style enum becomes an `int32` + constants; a rich
        // (algebraic) enum becomes a sealed sum type. Each renderer skips
        // the other kind.
        render_enum(out, e);
        render_rich_enum(out, e);
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
        module: &ModuleBinding,
        cb: &CallbackInterfaceBinding,
        config: &Self::Config,
    ) {
        render_callback_interface(out, config.prefix(), module, cb);
    }

    fn render_interface(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        i: &InterfaceBinding,
        config: &Self::Config,
    ) {
        let stem = domain_stem(module);
        render_interface(out, config.prefix(), module, i, stem.as_deref());
    }

    fn render_function(
        &self,
        out: &mut String,
        module: &ModuleBinding,
        f: &FnBinding,
        config: &Self::Config,
    ) {
        let go_name =
            wrapper_name(&module.path, &f.name, config.strip_module_prefix).to_upper_camel_case();
        let stem = domain_stem(module);
        let err = ErrCtx::of(f, stem.as_deref());
        let prefix = config.prefix();
        if let CallShape::Async(ab) = &f.shape {
            render_async_function(out, prefix, &module.path, f, ab, &go_name, None, err);
        } else {
            render_function(out, prefix, &module.path, f, &go_name, None, err);
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
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(dir.join("weaveffi.go"), render_go(api, model, config)),
            OutputFile::new(
                dir.join("go.mod"),
                render_go_mod(
                    &pkg::resolve(
                        api,
                        config.module_path.as_deref(),
                        config.input_basename.as_deref(),
                    )
                    .name,
                    input_basename,
                ),
            ),
            OutputFile::new(dir.join("README.md"), render_readme(input_basename)),
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
        Some(package_files(api, model, ctx, out_dir, config))
    }
}

// ── Import scanning ──

/// `true` when `ty` is a bare bool in a returned position (including an
/// iterator element), needing the `cToBool` helper.
fn ret_direct_bool(ty: &Ty) -> bool {
    match ty {
        Ty::Bool => true,
        Ty::Iterator(inner) => matches!(inner.as_ref(), Ty::Bool),
        _ => false,
    }
}

/// What the generated file's preamble must pull in, computed by one pass over
/// the lowered model.
#[derive(Default, Clone, Copy)]
struct Imports {
    /// `iter` (lazy sequences returned by `iter<T>` functions).
    iter: bool,
    /// `unsafe` (pointer staging for strings/bytes/buffers, object tokens,
    /// callback contexts).
    unsafe_ptr: bool,
    /// The `boolToC`/`cToBool` helpers.
    bool_helpers: bool,
    /// `runtime` (`SetFinalizer` and `KeepAlive` on object wrappers).
    runtime: bool,
    /// `runtime/cgo` (`cgo.Handle` for callback-interface implementations
    /// and async completion channels).
    cgo: bool,
    /// The shared error plumbing: the
    /// [`ERROR_BRAND`](weaveffi_core::errors::ERROR_BRAND) type plus the
    /// `wvTakeError`/`wvBrandError`/`wvTrap` helpers.
    err_infra: bool,
    /// The value-buffer runtime (`wvWriter`/`wvReader` and buffer copy
    /// helpers), pulling in `encoding/binary`, `math`, and `unicode/utf8`.
    buffer_runtime: bool,
    /// The `wvForeignError` reporter used by callback-interface trampolines.
    foreign_error: bool,
    /// The `wvHandlePtr` preamble helper that widens a `cgo.Handle` or an
    /// object token into the `void*` the ABI carries. Doing the integer to
    /// pointer conversion in C keeps `go vet` from flagging the generated
    /// file for a "possible misuse of unsafe.Pointer".
    handle_ptr: bool,
}

/// Scan the lowered model for everything [`Imports`] tracks. Interface
/// members participate exactly like free functions (via
/// [`weaveffi_core::model::ModuleBinding::callables`]).
fn scan_imports(model: &BindingModel) -> Imports {
    let mut any_callable = false;
    let mut has_async = false;
    let mut has_iter = false;
    let mut has_domain = false;
    let mut bool_helpers = false;
    let mut buffer_runtime = false;
    let has_interfaces = model.has_interfaces();
    let has_callback_interfaces = model.has_callback_interfaces();

    for m in &model.modules {
        has_domain |= m.declares_error();
        // Records and rich enums always carry pack/unpack functions; a
        // declared domain with payload fields decodes them through a reader.
        buffer_runtime |= !m.structs.is_empty();
        buffer_runtime |= m.enums.iter().any(|e| e.is_rich());
        if let Some(eb) = &m.error {
            buffer_runtime |= eb.declared_here && eb.codes.iter().any(|c| !c.fields.is_empty());
        }
        for f in m.callables() {
            any_callable = true;
            has_async |= f.is_async;
            has_iter |= matches!(f.shape, CallShape::Iterator(_));
            buffer_runtime |= f.params.iter().any(|p| p.ty.is_buffered());
            bool_helpers |= f.params.iter().any(|p| matches!(p.ty, Ty::Bool));
            if let Some(ret) = &f.ret {
                buffer_runtime |= ret.is_buffered();
                bool_helpers |= ret_direct_bool(ret);
            }
            if let CallShape::Iterator(ib) = &f.shape {
                // Bytes and buffered elements copy through wvCopyBuffer.
                buffer_runtime |= matches!(elem_free(&ib.elem), Free::Bytes);
            }
        }
        for cb in &m.callback_interfaces {
            for meth in &cb.methods {
                buffer_runtime |= meth.params.iter().any(|p| p.ty.is_buffered());
                bool_helpers |= meth.params.iter().any(|p| matches!(p.ty, Ty::Bool));
                bool_helpers |= matches!(meth.ret, Some(Ty::Bool));
            }
        }
    }

    // Every callable checks its error slot (returning or trapping), so any
    // callable at all pulls in the error plumbing; a declared domain also
    // needs it for the brand-error fallback of its mapping helper.
    let err_infra = any_callable || has_domain;
    // wvTakeError copies the payload through unsafe.Pointer; the buffer
    // runtime copies C buffers; object token helpers widen pointers; callback
    // contexts and async trampolines carry `void*`.
    let unsafe_ptr =
        err_infra || buffer_runtime || has_interfaces || has_callback_interfaces || has_async;

    Imports {
        iter: has_iter,
        unsafe_ptr,
        bool_helpers,
        runtime: has_interfaces,
        cgo: has_callback_interfaces || has_async,
        err_infra,
        buffer_runtime,
        foreign_error: has_callback_interfaces,
        handle_ptr: has_interfaces || has_callback_interfaces || has_async,
    }
}

// ── Top-level rendering ──

/// Render the complete generated Go source file: the cgo preamble, imports,
/// runtime prelude, and every module's entities and wrappers in the canonical
/// member order (error domain, enums, structs, callback interfaces,
/// interfaces, functions).
pub(crate) fn render_go(api: &ResolvedApi, model: &BindingModel, config: &GoConfig) -> String {
    let prefix = config.prefix();
    let input_basename = config.input_basename();
    let imports = scan_imports(model);
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);

    // The Go package clause and the linked library name follow the resolved
    // package identity (e.g. `package kvstore` / `-lkvstore`) rather than the
    // `weaveffi` brand, so the bindings link the shared library the producer
    // emits for this package. The C header keeps the ABI-prefix name.
    let resolved = pkg::resolve(api, None, Some(input_basename));
    let go_pkg = resolved.ident_name();
    let link_name = resolved.ident_name();

    out.push_str(&format!("package {go_pkg}\n\n"));

    out.push_str("/*\n");
    out.push_str(&format!("#cgo LDFLAGS: -l{link_name}\n"));
    out.push_str(&format!("#include \"{prefix}.h\"\n"));
    out.push_str("#include <stdlib.h>\n");
    if imports.handle_ptr {
        out.push_str("static void* wvHandlePtr(uintptr_t h) { return (void*)h; }\n");
    }
    // Forward declarations for the //export trampolines below (mirroring the
    // const-free prototypes cgo emits into _cgo_export.h) and the static
    // vtable of each callback interface.
    for decl in collect_preamble_decls(model, prefix) {
        out.push_str(&decl);
        out.push('\n');
    }
    out.push_str("*/\n");
    out.push_str("import \"C\"\n");

    // The ABI check in `init` formats its panic message, so `fmt` is always
    // imported; the rest of the block is driven by what the model uses.
    out.push_str("\nimport (\n");
    if imports.buffer_runtime {
        out.push_str("\t\"encoding/binary\"\n");
    }
    out.push_str("\t\"fmt\"\n");
    if imports.iter {
        out.push_str("\t\"iter\"\n");
    }
    if imports.buffer_runtime {
        out.push_str("\t\"math\"\n");
    }
    if imports.runtime {
        out.push_str("\t\"runtime\"\n");
    }
    if imports.cgo {
        out.push_str("\t\"runtime/cgo\"\n");
    }
    if imports.buffer_runtime {
        out.push_str("\t\"unicode/utf8\"\n");
    }
    if imports.unsafe_ptr {
        out.push_str("\t\"unsafe\"\n");
    }
    out.push_str(")\n\n");

    render_abi_version_check(&mut out);

    if imports.bool_helpers {
        render_bool_helpers(&mut out);
    }

    if imports.err_infra {
        render_error_infra(&mut out);
    }

    if imports.buffer_runtime {
        render_buffer_runtime(&mut out);
    }

    if imports.foreign_error {
        render_foreign_error(&mut out);
    }

    for m in &model.modules {
        GoGenerator.emit_members(&mut out, m, config);
    }

    // Exactly one blank line before the trailer keeps the file gofmt-clean.
    out.truncate(out.trim_end().len());
    out.push_str("\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi.go"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn named(n: &str) -> TypeRef {
        TypeRef::Named(n.into())
    }

    fn optional(t: TypeRef) -> TypeRef {
        TypeRef::Optional(Box::new(t))
    }

    /// One module exercising every revision-2 shape: an interface with a
    /// constructor and a method, `Interface?` in and out, a record with
    /// `Interface` and `[Interface]` fields, an iterator of interfaces, a
    /// callback interface taking a string, an i32, a record, and an object
    /// (one method returning `bool`, one returning void), and a function
    /// taking that callback interface.
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
                Function {
                    r#async: true,
                    ..func(
                        "fetch",
                        vec![param("id", TypeRef::I32)],
                        Some(named("Ticker")),
                    )
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
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module],
        })
    }

    fn render(config: &GoConfig) -> String {
        let api = fixture();
        let model = BindingModel::build(&api, config.prefix());
        render_go(&api, &model, config)
    }

    fn assert_has(src: &str, needle: &str) {
        assert!(src.contains(needle), "missing `{needle}` in:\n{src}");
    }

    #[test]
    fn preamble_declares_trampolines_and_one_static_vtable() {
        let src = render(&GoConfig::default());
        assert_has(&src, "#include \"weaveffi.h\"");
        assert_has(
            &src,
            "extern void goWv_weaveffi_bus_Subscriber_on_message(void* ctx, char* text, int32_t weight, uint8_t* envelope_ptr, size_t envelope_len, weaveffi_error* out_err);",
        );
        assert_has(
            &src,
            "extern bool goWv_weaveffi_bus_Subscriber_on_ticker(void* ctx, weaveffi_bus_Ticker* ticker, weaveffi_error* out_err);",
        );
        assert_has(
            &src,
            "extern void goWv_weaveffi_bus_Subscriber_free(void* ctx);",
        );
        assert_has(
            &src,
            "static const weaveffi_bus_Subscriber_vtable wvVtable_weaveffi_bus_Subscriber = {",
        );
        // The const-carrying entry is cast back to the vtable's field type;
        // the const-free one and `free` are used directly.
        assert_has(
            &src,
            "(void (*)(void*, const char*, int32_t, const uint8_t*, size_t, weaveffi_error*))goWv_weaveffi_bus_Subscriber_on_message,",
        );
        assert_has(&src, "    goWv_weaveffi_bus_Subscriber_on_ticker,");
        assert_has(&src, "    goWv_weaveffi_bus_Subscriber_free,\n};");
        assert_eq!(src.matches("wvVtable_weaveffi_bus_Subscriber =").count(), 1);
        // A `static` table has no external linkage for cgo to import, so a
        // static accessor hands its address to Go.
        assert_has(
            &src,
            "static const weaveffi_bus_Subscriber_vtable* wvVtablePtr_weaveffi_bus_Subscriber(void) { return &wvVtable_weaveffi_bus_Subscriber; }",
        );
        assert!(
            !src.contains("&C.wvVtable_"),
            "Go must not take a static C variable's address directly"
        );
        // The async completion trampoline is declared too.
        assert_has(
            &src,
            "extern void goWv_weaveffi_bus_fetch_callback(void* context, weaveffi_error* err, weaveffi_bus_Ticker* result);",
        );
        // Imports needed by objects and callbacks.
        assert_has(&src, "\t\"runtime\"\n");
        assert_has(&src, "\t\"runtime/cgo\"\n");
        assert_has(&src, "\t\"unsafe\"\n");
        assert!(!src.contains("\"sync\""), "the registry mutex is gone");
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

        // The void-returning trampoline: borrowed string and buffer, direct
        // i32, recover into error_set with -4.
        assert_has(
            &src,
            "//export goWv_weaveffi_bus_Subscriber_on_message\nfunc goWv_weaveffi_bus_Subscriber_on_message(ctx unsafe.Pointer, text *C.char, weight C.int32_t, envelope_ptr *C.uint8_t, envelope_len C.size_t, out_err *C.weaveffi_error) {",
        );
        assert_has(
            &src,
            "impl := cgo.Handle(uintptr(ctx)).Value().(Subscriber)",
        );
        assert_has(&src, "wvForeignError(out_err, r)");
        assert_has(&src, "C.weaveffi_error_set(outErr, -4, msg)");
        assert_has(&src, "arg0 = C.GoString(text)");
        assert_has(&src, "arg1 := int32(weight)");
        assert_has(
            &src,
            "rArg2 := &wvReader{buf: wvBorrowBuffer(envelope_ptr, envelope_len)}",
        );
        assert_has(&src, "arg2 = wvUnpackEnvelope(rArg2)");
        assert_has(&src, "impl.OnMessage(arg0, arg1, arg2)\n");
        assert!(
            !src.contains("C.weaveffi_free_bytes(envelope_ptr"),
            "borrowed callback buffers are never freed"
        );

        // The bool-returning trampoline adopts its object argument and
        // writes a direct return.
        assert_has(
            &src,
            "func goWv_weaveffi_bus_Subscriber_on_ticker(ctx unsafe.Pointer, ticker *C.weaveffi_bus_Ticker, out_err *C.weaveffi_error) (ret C._Bool) {",
        );
        assert_has(&src, "arg0 := wvAdoptTicker(ticker)");
        assert_has(&src, "ret = boolToC(impl.OnTicker(arg0))");

        // `free` deletes the handle.
        assert_has(
            &src,
            "//export goWv_weaveffi_bus_Subscriber_free\nfunc goWv_weaveffi_bus_Subscriber_free(ctx unsafe.Pointer) {\n\tcgo.Handle(uintptr(ctx)).Delete()\n}",
        );

        // Passing an implementation: handle as ctx, static vtable address.
        assert_has(&src, "func Subscribe(subscriber Subscriber) {");
        assert_has(&src, "hSubscriber := cgo.NewHandle(subscriber)");
        assert_has(
            &src,
            "C.weaveffi_bus_subscribe(C.wvHandlePtr(C.uintptr_t(hSubscriber)), C.wvVtablePtr_weaveffi_bus_Subscriber(), &cErr)",
        );
    }

    #[test]
    fn interface_wrapper_adopts_clones_and_destroys_once() {
        let src = render(&GoConfig::default());
        assert_has(
            &src,
            "type Ticker struct {\n\tptr *C.weaveffi_bus_Ticker\n}",
        );
        assert_has(
            &src,
            "func wvAdoptTicker(ptr *C.weaveffi_bus_Ticker) *Ticker {\n\tif ptr == nil {\n\t\treturn nil\n\t}\n\ts := &Ticker{ptr: ptr}\n\truntime.SetFinalizer(s, (*Ticker).Close)\n\treturn s\n}",
        );
        assert_has(
            &src,
            "func (s *Ticker) Close() {\n\tif s.ptr != nil {\n\t\tC.weaveffi_bus_Ticker_destroy(s.ptr)\n\t\ts.ptr = nil\n\t\truntime.SetFinalizer(s, nil)\n\t}\n}",
        );
        // Token helpers use clone for writing and adopt for reading.
        assert_has(
            &src,
            "return uint64(uintptr(unsafe.Pointer(C.weaveffi_bus_Ticker_clone(o.ptr))))",
        );
        assert_has(
            &src,
            "func wvUntokenTicker(token uint64) *Ticker {\n\tif token == 0 {",
        );
        assert_has(
            &src,
            "return wvAdoptTicker((*C.weaveffi_bus_Ticker)(C.wvHandlePtr(C.uintptr_t(token))))",
        );

        // Constructor and method: a returned object is adopted, the receiver
        // is kept alive across the call.
        assert_has(&src, "func NewTicker(start int64) *Ticker {");
        assert_has(&src, "return wvAdoptTicker(result)");
        assert_has(
            &src,
            "func (s *Ticker) Value() int64 {\n\tif s.ptr == nil {\n\t\tpanic(\"weaveffi: Ticker used after Close\")\n\t}\n\tdefer runtime.KeepAlive(s)\n\tvar cErr C.weaveffi_error\n\tresult := C.weaveffi_bus_Ticker_value(s.ptr, &cErr)",
        );
    }

    #[test]
    fn nullable_objects_are_nil_wrapper_pointers() {
        let src = render(&GoConfig::default());
        assert_has(&src, "func Pick(preferred *Ticker) *Ticker {");
        assert_has(&src, "defer runtime.KeepAlive(preferred)");
        assert_has(
            &src,
            "var cPreferred *C.weaveffi_bus_Ticker\n\tif preferred != nil {\n\t\tcPreferred = preferred.ptr\n\t}",
        );
        assert_has(&src, "result := C.weaveffi_bus_pick(cPreferred, &cErr)");
        // A null return adopts to nil through the same helper.
        assert_has(&src, "\treturn wvAdoptTicker(result)\n}");
    }

    #[test]
    fn records_carry_object_tokens() {
        let src = render(&GoConfig::default());
        assert_has(
            &src,
            "type Envelope struct {\n\tTopic   string\n\tPrimary *Ticker\n\tOthers  []*Ticker\n}",
        );
        assert_has(&src, "w.writeU64(wvTokenTicker(v.Primary))");
        assert_has(
            &src,
            "for _, eOthers0 := range v.Others {\n\t\tw.writeU64(wvTokenTicker(eOthers0))",
        );
        assert_has(&src, "v.Primary = wvUntokenTicker(r.readU64())");
        assert_has(&src, "v.Others[iOthers0] = wvUntokenTicker(r.readU64())");
    }

    #[test]
    fn iterator_and_async_adopt_object_elements() {
        let src = render(&GoConfig::default());
        assert_has(&src, "func AllTickers() iter.Seq[*Ticker] {");
        assert_has(&src, "var outItem *C.weaveffi_bus_Ticker");
        assert_has(
            &src,
            "ok := C.weaveffi_bus_AllTickersIterator_next(it, &outItem, &iterErr) != 0",
        );
        assert_has(&src, "item := wvAdoptTicker(outItem)");
        assert_has(&src, "defer C.weaveffi_bus_AllTickersIterator_destroy(it)");

        assert_has(&src, "func Fetch(id int32) *Ticker {");
        assert_has(&src, "h := cgo.NewHandle(ch)");
        assert_has(
            &src,
            "C.weaveffi_bus_fetch_async(C.int32_t(id), C.weaveffi_bus_fetch_callback(unsafe.Pointer(C.goWv_weaveffi_bus_fetch_callback)), C.wvHandlePtr(C.uintptr_t(h)))",
        );
        assert_has(
            &src,
            "h := cgo.Handle(uintptr(context))\n\tch := h.Value().(chan wvOutcomeBusFetch)\n\th.Delete()",
        );
        assert_has(&src, "ch <- wvOutcomeBusFetch{val: wvAdoptTicker(result)}");
    }

    #[test]
    fn custom_prefix_keeps_runtime_symbols_canonical() {
        let config = GoConfig {
            prefix: Some("acme".into()),
            ..GoConfig::default()
        };
        let src = render(&config);
        assert_has(&src, "#include \"acme.h\"");
        assert_has(&src, "C.acme_bus_Ticker_clone(o.ptr)");
        assert_has(&src, "C.wvVtablePtr_acme_bus_Subscriber()");
        assert_has(&src, "C.weaveffi_error_set(outErr, -4, msg)");
        assert_has(&src, "C.weaveffi_abi_version()");
        assert!(!src.contains("arena"), "no arena bindings remain");
        assert!(
            !src.contains("handle_t"),
            "no untyped handle typedef remains"
        );
    }

    #[test]
    fn undocumented_enum_members_are_gofmt_aligned() {
        use weaveffi_ir::ir::{EnumDef, EnumVariant};
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
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module],
        });
        let config = GoConfig::default();
        let model = BindingModel::build(&api, config.prefix());
        let src = render_go(&api, &model, &config);
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
        let a = render(&GoConfig::default());
        let b = render(&GoConfig::default());
        assert_eq!(a, b);
    }
}
