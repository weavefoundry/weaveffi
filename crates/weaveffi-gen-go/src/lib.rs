use anyhow::Result;
use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::{stamp_header, Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, Function, ListenerDef, Module, StructDef, StructField, TypeRef,
};

pub struct GoGenerator;

fn stamp_slash(body: String) -> String {
    format!("// {}\n{body}", stamp_header("go"))
}

impl GoGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        module_path: &str,
        c_prefix: &str,
    ) -> Result<()> {
        let dir = out_dir.join("go");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("weaveffi.go"),
            stamp_slash(render_go(api, c_prefix)),
        )?;
        std::fs::write(dir.join("go.mod"), stamp_slash(render_go_mod(module_path)))?;
        // go.sum is emitted empty as a placeholder; `go mod tidy` will populate
        // it with real checksums once the user adds dependencies.
        std::fs::write(dir.join("go.sum"), render_go_sum())?;
        std::fs::write(dir.join("doc.go"), stamp_slash(render_doc_go()))?;
        std::fs::write(
            dir.join("weaveffi_test.go"),
            stamp_slash(render_smoke_test()),
        )?;
        // README.md is documentation, not a source file; leave it unstamped.
        std::fs::write(dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for GoGenerator {
    fn name(&self) -> &'static str {
        "go"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "github.com/example/weaveffi", "weaveffi")
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(api, out_dir, config.go_module_path(), config.c_prefix())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("go/weaveffi.go").to_string(),
            out_dir.join("go/go.mod").to_string(),
            out_dir.join("go/go.sum").to_string(),
            out_dir.join("go/doc.go").to_string(),
            out_dir.join("go/weaveffi_test.go").to_string(),
            out_dir.join("go/README.md").to_string(),
        ]
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[
            Capability::Callbacks,
            Capability::Listeners,
            Capability::CancellableAsync,
            Capability::Iterators,
            Capability::TypedHandles,
            Capability::BorrowedTypes,
            Capability::MapTypes,
            Capability::NestedModules,
            Capability::CrossModuleTypes,
            Capability::ErrorDomains,
            Capability::DeprecatedAnnotations,
        ]
    }
}

// ── Type mapping ──

fn go_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "int32".into(),
        TypeRef::U32 => "uint32".into(),
        TypeRef::I64 | TypeRef::Handle => "int64".into(),
        TypeRef::F64 => "float64".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "[]byte".into(),
        TypeRef::TypedHandle(n) => format!("*{}", n.to_upper_camel_case()),
        TypeRef::Struct(n) => format!("*{}", local_type_name(n).to_upper_camel_case()),
        TypeRef::Enum(n) => n.to_upper_camel_case(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => go_type(inner),
            TypeRef::List(_) | TypeRef::Map(_, _) => go_type(inner),
            TypeRef::Bytes | TypeRef::BorrowedBytes => go_type(inner),
            _ => format!("*{}", go_type(inner)),
        },
        TypeRef::List(inner) => format!("[]{}", go_type(inner)),
        TypeRef::Iterator(inner) => format!("<-chan {}", go_type(inner)),
        TypeRef::Map(k, v) => format!("map[{}]{}", go_type(k), go_type(v)),
        TypeRef::Callback(name) => name.to_upper_camel_case(),
    }
}

fn go_zero(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::F64 => "0".into(),
        TypeRef::Bool => "false".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "\"\"".into(),
        TypeRef::Enum(_) => "0".into(),
        _ => "nil".into(),
    }
}

fn c_scalar_type(ty: &TypeRef, module: &str, prefix: &str) -> Option<String> {
    match ty {
        TypeRef::I32 => Some("C.int32_t".into()),
        TypeRef::U32 => Some("C.uint32_t".into()),
        TypeRef::I64 | TypeRef::Handle => Some("C.int64_t".into()),
        TypeRef::F64 => Some("C.double".into()),
        TypeRef::Bool => Some("C._Bool".into()),
        TypeRef::Enum(n) => Some(format!("C.{prefix}_{module}_{n}")),
        _ => None,
    }
}

fn c_scalar_conv(expr: &str, ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::Bool => format!("boolToC({expr})"),
        _ => {
            if let Some(ct) = c_scalar_type(ty, module, prefix) {
                format!("{ct}({expr})")
            } else {
                expr.to_string()
            }
        }
    }
}

fn go_scalar_conv(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => format!("int32({expr})"),
        TypeRef::U32 => format!("uint32({expr})"),
        TypeRef::I64 | TypeRef::Handle => format!("int64({expr})"),
        TypeRef::F64 => format!("float64({expr})"),
        TypeRef::Bool => format!("cToBool({expr})"),
        TypeRef::Enum(n) => format!("{}({expr})", n.to_upper_camel_case()),
        _ => expr.to_string(),
    }
}

fn c_opaque_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => format!("{prefix}_{module}_{n}"),
        _ => String::new(),
    }
}

// ── Import scanning ──

fn param_uses_unsafe(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => true,
        TypeRef::Bytes | TypeRef::BorrowedBytes => true,
        TypeRef::List(_) | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => param_uses_unsafe(inner),
        _ => false,
    }
}

fn return_uses_unsafe(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => true,
        TypeRef::List(_) | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => return_uses_unsafe(inner),
        _ => false,
    }
}

fn type_has_bool(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Bool => true,
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            type_has_bool(inner)
        }
        _ => false,
    }
}

fn collect_all_modules(modules: &[Module]) -> Vec<&Module> {
    let mut all = Vec::new();
    for m in modules {
        all.push(m);
        all.extend(collect_all_modules(&m.modules));
    }
    all
}

fn collect_modules_with_path(modules: &[Module]) -> Vec<(&Module, String)> {
    let mut result = Vec::new();
    for m in modules {
        collect_module_with_path(m, &m.name, &mut result);
    }
    result
}

fn collect_module_with_path<'a>(m: &'a Module, path: &str, out: &mut Vec<(&'a Module, String)>) {
    out.push((m, path.to_string()));
    for sub in &m.modules {
        collect_module_with_path(sub, &format!("{path}_{}", sub.name), out);
    }
}

fn has_any_callbacks(api: &Api) -> bool {
    collect_all_modules(&api.modules)
        .iter()
        .any(|m| !m.callbacks.is_empty())
}

fn has_any_async(api: &Api) -> bool {
    collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async))
}

fn scan_imports(api: &Api) -> (bool, bool, bool, bool, bool) {
    let has_funcs = collect_all_modules(&api.modules)
        .iter()
        .any(|m| !m.functions.is_empty());

    let has_builders = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.structs.iter().any(|s| s.builder));

    let needs_fmt = has_funcs || has_builders;

    let needs_registry = has_any_callbacks(api) || has_any_async(api);

    let needs_unsafe = needs_registry
        || collect_all_modules(&api.modules).iter().any(|m| {
            m.functions.iter().any(|f| {
                f.params.iter().any(|p| param_uses_unsafe(&p.ty))
                    || f.returns.as_ref().is_some_and(return_uses_unsafe)
            }) || m
                .structs
                .iter()
                .any(|s| s.builder && s.fields.iter().any(|fld| param_uses_unsafe(&fld.ty)))
        });

    let needs_bool = collect_all_modules(&api.modules).iter().any(|m| {
        m.functions.iter().any(|f| {
            f.params.iter().any(|p| type_has_bool(&p.ty))
                || f.returns.as_ref().is_some_and(type_has_bool)
        }) || m
            .structs
            .iter()
            .any(|s| s.fields.iter().any(|fld| type_has_bool(&fld.ty)))
    });

    let needs_runtime = collect_all_modules(&api.modules)
        .iter()
        .any(|m| !m.structs.is_empty());

    (
        needs_fmt,
        needs_unsafe,
        needs_bool,
        needs_runtime,
        needs_registry,
    )
}

// ── Packaging scaffold ──

fn render_go_mod(module_path: &str) -> String {
    format!("module {module_path}\n\ngo 1.21\n")
}

fn render_go_sum() -> String {
    // Placeholder: the generated bindings have no Go dependencies, so go.sum is
    // empty by design. It exists so `go build` inside a checked-in copy does
    // not complain about a missing checksum file; `go mod tidy` will refresh it
    // once the consumer adds dependencies.
    String::new()
}

fn render_doc_go() -> String {
    r#"// Package weaveffi exposes auto-generated CGo bindings for the WeaveFFI
// C ABI. Each top-level function wraps a `weaveffi_{module}_{function}`
// symbol from the compiled shared library, marshalling Go values to and
// from the underlying C representation.
//
// See README.md for build prerequisites (CGo, a C compiler, and the
// WeaveFFI shared library + header on the linker's search path).
package weaveffi
"#
    .into()
}

fn render_smoke_test() -> String {
    r#"package weaveffi

import "testing"

// TestPackageLoads is a smoke test that simply ensures the generated
// package compiles and links against the WeaveFFI C library. Per-API
// integration tests should live next to this file and exercise the
// individual generated wrapper functions.
func TestPackageLoads(t *testing.T) {
	t.Log("weaveffi package loaded")
}
"#
    .into()
}

fn render_readme() -> String {
    r#"# WeaveFFI Go Bindings

Auto-generated Go bindings using CGo.

## Prerequisites

- Go >= 1.21
- A C compiler (gcc or clang) accessible to CGo
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`,
  or `weaveffi.dll`) and the C header (`weaveffi.h`)

## Build

1. Place `libweaveffi.so` (or the platform-specific equivalent) and
   `weaveffi.h` where the linker and CGo can find them. For example,
   install them into `/usr/local/lib` and `/usr/local/include`, or set
   `CGO_LDFLAGS` and `CGO_CFLAGS`:

```sh
export CGO_CFLAGS="-I/path/to/headers"
export CGO_LDFLAGS="-L/path/to/lib -lweaveffi"
```

2. Build or run your Go project that imports this module:

```sh
go build ./...
```

## How It Works

The generated `weaveffi.go` file uses a CGo preamble to `#include "weaveffi.h"`
and link against `-lweaveffi`. Each API function is exposed as an idiomatic Go
function that marshals arguments to C types, calls the C ABI function, and
converts the result back to Go types. Errors are returned as Go `error` values.
"#
    .into()
}

// ── Callback emission ──

fn cb_c_elem_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::Handle => format!("{prefix}_handle_t"),
        TypeRef::Enum(n) => format!("{prefix}_{module}_{n}"),
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) => format!("{prefix}_{module}_{n}*"),
        _ => "void*".into(),
    }
}

fn cb_c_param_decl(ty: &TypeRef, name: &str, mutable: bool, module: &str, prefix: &str) -> String {
    let q = if mutable { "" } else { "const " };
    match ty {
        TypeRef::I32 => format!("int32_t {name}"),
        TypeRef::U32 => format!("uint32_t {name}"),
        TypeRef::I64 => format!("int64_t {name}"),
        TypeRef::F64 => format!("double {name}"),
        TypeRef::Bool => format!("bool {name}"),
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("{q}uint8_t* {name}_ptr, size_t {name}_len")
        }
        TypeRef::BorrowedStr => format!("{q}char* {name}"),
        TypeRef::Handle => format!("{prefix}_handle_t {name}"),
        TypeRef::Enum(n) => format!("{prefix}_{module}_{n} {name}"),
        TypeRef::TypedHandle(n) => format!("{prefix}_{module}_{n}* {name}"),
        TypeRef::Struct(n) => format!("{q}{prefix}_{module}_{n}* {name}"),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                cb_c_param_decl(inner, name, mutable, module, prefix)
            }
            _ => format!("{q}{}* {name}", cb_c_elem_type(inner, module, prefix)),
        },
        TypeRef::List(inner) => {
            let elem = cb_c_elem_type(inner, module, prefix);
            format!("{q}{elem}* {name}, size_t {name}_len")
        }
        _ => format!("{q}void* {name}"),
    }
}

fn cb_c_ret_decl(ret: &Option<TypeRef>, module: &str, prefix: &str) -> (String, Vec<String>) {
    match ret {
        None => ("void".into(), vec![]),
        Some(TypeRef::I32) => ("int32_t".into(), vec![]),
        Some(TypeRef::U32) => ("uint32_t".into(), vec![]),
        Some(TypeRef::I64) | Some(TypeRef::Handle) => ("int64_t".into(), vec![]),
        Some(TypeRef::F64) => ("double".into(), vec![]),
        Some(TypeRef::Bool) => ("bool".into(), vec![]),
        Some(TypeRef::StringUtf8) | Some(TypeRef::BorrowedStr) => ("const char*".into(), vec![]),
        Some(TypeRef::Enum(n)) => (format!("{prefix}_{module}_{n}"), vec![]),
        Some(TypeRef::TypedHandle(n)) => (format!("{prefix}_{module}_{n}*"), vec![]),
        Some(TypeRef::Struct(n)) => (format!("{prefix}_{module}_{n}*"), vec![]),
        _ => ("void*".into(), vec![]),
    }
}

fn render_callback_extern_decl(out: &mut String, cb: &CallbackDef, module: &str, prefix: &str) {
    let (ret_ty, ret_extra) = cb_c_ret_decl(&cb.returns, module, prefix);
    let mut params: Vec<String> = vec!["void* context".into()];
    params.extend(
        cb.params
            .iter()
            .map(|p| cb_c_param_decl(&p.ty, &p.name, p.mutable, module, prefix)),
    );
    params.extend(ret_extra);
    out.push_str(&format!(
        "extern {ret_ty} weaveffiGoCbTrampoline_{module}_{}({});\n",
        cb.name,
        params.join(", ")
    ));
}

fn render_callback_registry(out: &mut String) {
    out.push_str("var _callbackToken int64\n");
    out.push_str("var _callbackRegistry sync.Map\n\n");
    out.push_str("func _callbackRegister(cb interface{}) int64 {\n");
    out.push_str("\ttoken := atomic.AddInt64(&_callbackToken, 1)\n");
    out.push_str("\t_callbackRegistry.Store(token, cb)\n");
    out.push_str("\treturn token\n");
    out.push_str("}\n\n");
}

fn cb_trampoline_go_param(ty: &TypeRef, name: &str) -> String {
    let cn = name.to_lower_camel_case();
    match ty {
        TypeRef::I32 => format!("{cn} C.int32_t"),
        TypeRef::U32 => format!("{cn} C.uint32_t"),
        TypeRef::I64 | TypeRef::Handle => format!("{cn} C.int64_t"),
        TypeRef::F64 => format!("{cn} C.double"),
        TypeRef::Bool => format!("{cn} C._Bool"),
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("{cn}Ptr *C.uint8_t, {cn}Len C.size_t")
        }
        TypeRef::BorrowedStr => format!("{cn} *C.char"),
        TypeRef::Enum(_) | TypeRef::TypedHandle(_) | TypeRef::Struct(_) => {
            format!("{cn} unsafe.Pointer")
        }
        _ => format!("{cn} unsafe.Pointer"),
    }
}

fn cb_trampoline_convert(pre: &mut String, ty: &TypeRef, name: &str) -> String {
    let cn = name.to_lower_camel_case();
    match ty {
        TypeRef::I32 => format!("int32({cn})"),
        TypeRef::U32 => format!("uint32({cn})"),
        TypeRef::I64 | TypeRef::Handle => format!("int64({cn})"),
        TypeRef::F64 => format!("float64({cn})"),
        TypeRef::Bool => format!("cToBool({cn})"),
        TypeRef::StringUtf8 => {
            let v = format!("{cn}Go");
            pre.push_str(&format!(
                "\t{v} := C.GoStringN((*C.char)(unsafe.Pointer({cn}Ptr)), C.int({cn}Len))\n"
            ));
            v
        }
        TypeRef::BorrowedStr => {
            let v = format!("{cn}Go");
            pre.push_str(&format!("\t{v} := C.GoString({cn})\n"));
            v
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let v = format!("{cn}Go");
            pre.push_str(&format!(
                "\t{v} := C.GoBytes(unsafe.Pointer({cn}Ptr), C.int({cn}Len))\n"
            ));
            v
        }
        _ => cn,
    }
}

fn render_callback(out: &mut String, cb: &CallbackDef, module: &str, prefix: &str) {
    let type_name = cb.name.to_upper_camel_case();

    let go_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();
    let go_ret = match &cb.returns {
        None => String::new(),
        Some(ret) => format!(" {}", go_type(ret)),
    };

    if let Some(doc) = &cb.doc {
        out.push_str(&format!("// {type_name} -- {doc}\n"));
    }
    out.push_str(&format!(
        "type {type_name} func({}){go_ret}\n\n",
        go_params.join(", ")
    ));

    let (c_ret_ty, _) = cb_c_ret_decl(&cb.returns, module, prefix);
    let mut c_params: Vec<String> = vec!["ctx unsafe.Pointer".into()];
    c_params.extend(
        cb.params
            .iter()
            .map(|p| cb_trampoline_go_param(&p.ty, &p.name)),
    );

    let trampoline_name = format!("weaveffiGoCbTrampoline_{module}_{}", cb.name);
    let c_ret_sig = if c_ret_ty == "void" {
        String::new()
    } else {
        format!(" C.{c_ret_ty}")
    };

    out.push_str(&format!("//export {trampoline_name}\n"));
    out.push_str(&format!(
        "func {trampoline_name}({}){c_ret_sig} {{\n",
        c_params.join(", ")
    ));
    out.push_str("\tv, ok := _callbackRegistry.Load(int64(uintptr(ctx)))\n");
    out.push_str("\tif !ok {\n");
    if c_ret_ty == "void" {
        out.push_str("\t\treturn\n");
    } else {
        out.push_str(&format!("\t\tvar zero C.{c_ret_ty}\n"));
        out.push_str("\t\treturn zero\n");
    }
    out.push_str("\t}\n");
    out.push_str(&format!("\tcb, ok := v.({type_name})\n"));
    out.push_str("\tif !ok {\n");
    if c_ret_ty == "void" {
        out.push_str("\t\treturn\n");
    } else {
        out.push_str(&format!("\t\tvar zero C.{c_ret_ty}\n"));
        out.push_str("\t\treturn zero\n");
    }
    out.push_str("\t}\n");

    let mut pre = String::new();
    let call_args: Vec<String> = cb
        .params
        .iter()
        .map(|p| cb_trampoline_convert(&mut pre, &p.ty, &p.name))
        .collect();
    out.push_str(&pre);

    let call = format!("cb({})", call_args.join(", "));
    match &cb.returns {
        None => {
            out.push_str(&format!("\t{call}\n"));
        }
        Some(ret) => {
            let conv = match ret {
                TypeRef::I32 => format!("C.int32_t({call})"),
                TypeRef::U32 => format!("C.uint32_t({call})"),
                TypeRef::I64 | TypeRef::Handle => format!("C.int64_t({call})"),
                TypeRef::F64 => format!("C.double({call})"),
                TypeRef::Bool => format!("boolToC({call})"),
                _ => call,
            };
            out.push_str(&format!("\treturn {conv}\n"));
        }
    }

    out.push_str("}\n\n");
}

// ── Listener emission ──

/// Emit a Go listener wrapper. Produces an empty struct type with
/// `Register(cb CallbackType) uint64` and `Unregister(id uint64)` methods.
/// A per-listener `sync.Map` pins the callback token against the id so that
/// `Unregister` can drop the pinned entry from the shared callback registry
/// after the native `unregister_*` call returns.
fn render_listener(out: &mut String, module_path: &str, l: &ListenerDef, prefix: &str) {
    let type_name = l.name.to_upper_camel_case();
    let cb_type = l.event_callback.to_upper_camel_case();
    let pins_var = format!("_{}Pins", l.name.to_lower_camel_case());
    let c_cb_type = format!("C.{prefix}_{module_path}_{}", l.event_callback);
    let trampoline = format!(
        "C.weaveffiGoCbTrampoline_{module_path}_{}",
        l.event_callback
    );
    let reg_fn = format!("{prefix}_{module_path}_register_{}", l.name);
    let unreg_fn = format!("{prefix}_{module_path}_unregister_{}", l.name);

    if let Some(doc) = &l.doc {
        out.push_str(&format!("// {type_name} -- {doc}\n"));
    }
    out.push_str(&format!("type {type_name} struct{{}}\n\n"));
    out.push_str(&format!("var {pins_var} sync.Map\n\n"));

    out.push_str(&format!(
        "func ({type_name}) Register(cb {cb_type}) uint64 {{\n"
    ));
    out.push_str("\ttoken := _callbackRegister(cb)\n");
    out.push_str("\tctx := unsafe.Pointer(uintptr(token))\n");
    out.push_str(&format!(
        "\tfn := ({c_cb_type})(unsafe.Pointer({trampoline}))\n"
    ));
    out.push_str(&format!("\tid := uint64(C.{reg_fn}(fn, ctx))\n"));
    out.push_str(&format!("\t{pins_var}.Store(id, token)\n"));
    out.push_str("\treturn id\n");
    out.push_str("}\n\n");

    out.push_str(&format!("func ({type_name}) Unregister(id uint64) {{\n"));
    out.push_str(&format!("\tC.{unreg_fn}(C.uint64_t(id))\n"));
    out.push_str(&format!(
        "\tif token, ok := {pins_var}.LoadAndDelete(id); ok {{\n"
    ));
    out.push_str("\t\t_callbackRegistry.Delete(token)\n");
    out.push_str("\t}\n");
    out.push_str("}\n\n");
}

// ── Async emission ──

/// Go type for the `Value` field of the generated `<Name>Result` struct.
/// `None` means the function returns no value and the struct has only `Err`.
fn async_value_go_type(ret: &Option<TypeRef>) -> Option<String> {
    ret.as_ref().map(go_type)
}

/// (c_decl_type, go_cgo_type) for a type used as an element or scalar result.
/// E.g. I32 -> ("int32_t", "C.int32_t"); Struct("Foo") -> ("weaveffi_m_Foo*", "*C.weaveffi_m_Foo").
/// Strings are single-pointer element types (`const char*`).
fn async_result_types(ty: &TypeRef, module: &str, prefix: &str) -> (String, String) {
    match ty {
        TypeRef::I32 => ("int32_t".into(), "C.int32_t".into()),
        TypeRef::U32 => ("uint32_t".into(), "C.uint32_t".into()),
        TypeRef::I64 | TypeRef::Handle => ("int64_t".into(), "C.int64_t".into()),
        TypeRef::F64 => ("double".into(), "C.double".into()),
        TypeRef::Bool => ("bool".into(), "C._Bool".into()),
        TypeRef::Enum(n) => (
            format!("{prefix}_{module}_{n}"),
            format!("C.{prefix}_{module}_{n}"),
        ),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("const char*".into(), "*C.char".into()),
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => (
            format!("{prefix}_{module}_{n}*"),
            format!("*C.{prefix}_{module}_{n}"),
        ),
        _ => ("void*".into(), "unsafe.Pointer".into()),
    }
}

/// (c_param_decl, go_cgo_param_decl) pairs following `(void* context, weaveffi_error* err)`.
fn async_cb_params(ret: &Option<TypeRef>, module: &str, prefix: &str) -> Vec<(String, String)> {
    match ret {
        None => vec![],
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => vec![
            ("const uint8_t* result".into(), "result *C.uint8_t".into()),
            ("size_t result_len".into(), "resultLen C.size_t".into()),
        ],
        Some(TypeRef::List(inner)) => {
            let (c_elem, go_elem) = async_result_types(inner, module, prefix);
            vec![
                (format!("{c_elem}* result"), format!("result *{go_elem}")),
                ("size_t result_len".into(), "resultLen C.size_t".into()),
            ]
        }
        Some(TypeRef::Map(k, v)) => {
            let (kc, kg) = async_result_types(k, module, prefix);
            let (vc, vg) = async_result_types(v, module, prefix);
            vec![
                (format!("{kc}* result_keys"), format!("resultKeys *{kg}")),
                (
                    format!("{vc}* result_values"),
                    format!("resultValues *{vg}"),
                ),
                ("size_t result_len".into(), "resultLen C.size_t".into()),
            ]
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                vec![("const char* result".into(), "result *C.char".into())]
            }
            TypeRef::Struct(n) | TypeRef::TypedHandle(n) => vec![(
                format!("{prefix}_{module}_{n}* result"),
                format!("result *C.{prefix}_{module}_{n}"),
            )],
            _ => {
                let (c, g) = async_result_types(inner, module, prefix);
                vec![(format!("const {c}* result"), format!("result *{g}"))]
            }
        },
        Some(ty) => {
            let (c, g) = async_result_types(ty, module, prefix);
            vec![(format!("{c} result"), format!("result {g}"))]
        }
    }
}

fn async_cb_extern_c_params(ret: &Option<TypeRef>, module: &str, prefix: &str) -> Vec<String> {
    async_cb_params(ret, module, prefix)
        .into_iter()
        .map(|(c, _)| c)
        .collect()
}

fn async_cb_go_params(ret: &Option<TypeRef>, module: &str, prefix: &str) -> Vec<String> {
    async_cb_params(ret, module, prefix)
        .into_iter()
        .map(|(_, g)| g)
        .collect()
}

/// Just the Go cgo types (no names), matching `async_cb_go_params`, used to
/// build the type-assertion target in the trampoline body.
fn async_cb_go_param_types(ret: &Option<TypeRef>, module: &str, prefix: &str) -> Vec<String> {
    async_cb_go_params(ret, module, prefix)
        .into_iter()
        .map(|decl| {
            decl.split_once(' ')
                .map(|(_, ty)| ty.to_string())
                .unwrap_or(decl)
        })
        .collect()
}

/// Names only (matching `async_cb_go_params`), used when calling the closure
/// from the trampoline.
fn async_cb_go_param_names(ret: &Option<TypeRef>) -> Vec<String> {
    match ret {
        None => vec![],
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) | Some(TypeRef::List(_)) => {
            vec!["result".into(), "resultLen".into()]
        }
        Some(TypeRef::Map(_, _)) => vec![
            "resultKeys".into(),
            "resultValues".into(),
            "resultLen".into(),
        ],
        Some(_) => vec!["result".into()],
    }
}

/// Emit code that populates `res.Value` from the trampoline locals.
/// `indent` is the whitespace prefix for each emitted line.
fn emit_async_value_from_cb_params(
    out: &mut String,
    indent: &str,
    ret: &TypeRef,
    module: &str,
    prefix: &str,
) {
    match ret {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::F64 => {
            let conv = go_scalar_conv("result", ret);
            out.push_str(&format!("{indent}res.Value = {conv}\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{indent}res.Value = cToBool(result)\n"));
        }
        TypeRef::Enum(n) => {
            let name = n.to_upper_camel_case();
            out.push_str(&format!("{indent}res.Value = {name}(result)\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{indent}if result != nil {{\n"));
            out.push_str(&format!("{indent}\tres.Value = C.GoString(result)\n"));
            out.push_str(&format!("{indent}\tC.{prefix}_free_string(result)\n"));
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{indent}if result != nil && resultLen > 0 {{\n"));
            out.push_str(&format!(
                "{indent}\tres.Value = C.GoBytes(unsafe.Pointer(result), C.int(resultLen))\n"
            ));
            out.push_str(&format!(
                "{indent}\tC.{prefix}_free_bytes(result, resultLen)\n"
            ));
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::TypedHandle(n) => {
            let name = n.to_upper_camel_case();
            out.push_str(&format!("{indent}if result != nil {{\n"));
            out.push_str(&format!("{indent}\tres.Value = &{name}{{ptr: result}}\n"));
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::Struct(n) => {
            let name = local_type_name(n).to_upper_camel_case();
            out.push_str(&format!("{indent}res.Value = new{name}(result)\n"));
        }
        TypeRef::List(inner) => {
            out.push_str(&format!("{indent}if result != nil && resultLen > 0 {{\n"));
            out.push_str(&format!("{indent}\tcount := int(resultLen)\n"));
            let gi = go_type(inner);
            out.push_str(&format!("{indent}\tgoResult := make([]{gi}, count)\n"));
            if let Some(ct) = c_scalar_type(inner, module, prefix) {
                out.push_str(&format!(
                    "{indent}\tcSlice := unsafe.Slice((*{ct})(unsafe.Pointer(result)), count)\n"
                ));
                out.push_str(&format!("{indent}\tfor i, v := range cSlice {{\n"));
                let conv = go_scalar_conv("v", inner);
                out.push_str(&format!("{indent}\t\tgoResult[i] = {conv}\n"));
                out.push_str(&format!("{indent}\t}}\n"));
            } else if matches!(inner.as_ref(), TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
                out.push_str(&format!(
                    "{indent}\tcSlice := unsafe.Slice((**C.char)(unsafe.Pointer(result)), count)\n"
                ));
                out.push_str(&format!("{indent}\tfor i, v := range cSlice {{\n"));
                out.push_str(&format!("{indent}\t\tgoResult[i] = C.GoString(v)\n"));
                out.push_str(&format!("{indent}\t}}\n"));
            } else if let TypeRef::TypedHandle(n) = inner.as_ref() {
                let ct = format!("C.{prefix}_{module}_{n}");
                let gs = n.to_upper_camel_case();
                out.push_str(&format!(
                    "{indent}\tcSlice := unsafe.Slice((**{ct})(unsafe.Pointer(result)), count)\n"
                ));
                out.push_str(&format!("{indent}\tfor i, v := range cSlice {{\n"));
                out.push_str(&format!("{indent}\t\tgoResult[i] = &{gs}{{ptr: v}}\n"));
                out.push_str(&format!("{indent}\t}}\n"));
            } else if let TypeRef::Struct(n) = inner.as_ref() {
                let ct = format!("C.{prefix}_{module}_{n}");
                let gs = local_type_name(n).to_upper_camel_case();
                out.push_str(&format!(
                    "{indent}\tcSlice := unsafe.Slice((**{ct})(unsafe.Pointer(result)), count)\n"
                ));
                out.push_str(&format!("{indent}\tfor i, v := range cSlice {{\n"));
                out.push_str(&format!("{indent}\t\tgoResult[i] = new{gs}(v)\n"));
                out.push_str(&format!("{indent}\t}}\n"));
            }
            out.push_str(&format!("{indent}\tres.Value = goResult\n"));
            out.push_str(&format!("{indent}}}\n"));
        }
        TypeRef::Map(k, v) => {
            let gk = go_type(k);
            let gv = go_type(v);
            out.push_str(&format!("{indent}res.Value = make(map[{gk}]{gv})\n"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!("{indent}if result != nil {{\n"));
                out.push_str(&format!("{indent}\tv := C.GoString(result)\n"));
                out.push_str(&format!("{indent}\tC.{prefix}_free_string(result)\n"));
                out.push_str(&format!("{indent}\tres.Value = &v\n"));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::Struct(n) => {
                let name = local_type_name(n).to_upper_camel_case();
                out.push_str(&format!("{indent}res.Value = new{name}(result)\n"));
            }
            TypeRef::TypedHandle(n) => {
                let name = n.to_upper_camel_case();
                out.push_str(&format!("{indent}if result != nil {{\n"));
                out.push_str(&format!("{indent}\tres.Value = &{name}{{ptr: result}}\n"));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::Bool => {
                out.push_str(&format!("{indent}if result != nil {{\n"));
                out.push_str(&format!("{indent}\tv := cToBool(*result)\n"));
                out.push_str(&format!("{indent}\tres.Value = &v\n"));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::Enum(n) => {
                let name = n.to_upper_camel_case();
                out.push_str(&format!("{indent}if result != nil {{\n"));
                out.push_str(&format!("{indent}\tv := {name}(*result)\n"));
                out.push_str(&format!("{indent}\tres.Value = &v\n"));
                out.push_str(&format!("{indent}}}\n"));
            }
            _ => {
                let gi = go_type(inner);
                out.push_str(&format!("{indent}if result != nil {{\n"));
                out.push_str(&format!("{indent}\tv := {gi}(*result)\n"));
                out.push_str(&format!("{indent}\tres.Value = &v\n"));
                out.push_str(&format!("{indent}}}\n"));
            }
        },
        TypeRef::Iterator(_) => {
            unreachable!("iterator return is not valid for async functions")
        }
        TypeRef::Callback(_) => {
            unreachable!("callback return is not valid for async functions")
        }
    }
}

fn render_async_trampoline_extern_decl(out: &mut String, module: &str, f: &Function, prefix: &str) {
    let mut params = vec!["void* context".to_string(), format!("{prefix}_error* err")];
    params.extend(async_cb_extern_c_params(&f.returns, module, prefix));
    out.push_str(&format!(
        "extern void weaveffiGoAsyncTrampoline_{module}_{}({});\n",
        f.name,
        params.join(", ")
    ));
}

fn render_async_function(out: &mut String, module: &str, f: &Function, prefix: &str) {
    let c_sym = format!("{prefix}_{module}_{}", f.name);
    let go_name = format!("{}_{}", module, f.name).to_upper_camel_case();
    let result_ty = format!("{go_name}Result");
    let trampoline = format!("weaveffiGoAsyncTrampoline_{module}_{}", f.name);
    let cb_type = format!("{c_sym}_callback");

    // Result struct.
    out.push_str(&format!("type {result_ty} struct {{\n"));
    if let Some(value_ty) = async_value_go_type(&f.returns) {
        out.push_str(&format!("\tValue {value_ty}\n"));
    }
    out.push_str("\tErr error\n");
    out.push_str("}\n\n");

    // Trampoline.
    let go_param_types = async_cb_go_param_types(&f.returns, module, prefix);
    let go_param_names = async_cb_go_param_names(&f.returns);
    let cb_asserted_sig = {
        let mut parts = vec![format!("*C.{prefix}_error")];
        parts.extend(go_param_types.iter().cloned());
        format!("func({})", parts.join(", "))
    };

    let mut trampoline_params = vec![
        "ctx unsafe.Pointer".to_string(),
        format!("err *C.{prefix}_error"),
    ];
    trampoline_params.extend(async_cb_go_params(&f.returns, module, prefix));

    out.push_str(&format!("//export {trampoline}\n"));
    out.push_str(&format!(
        "func {trampoline}({}) {{\n",
        trampoline_params.join(", ")
    ));
    out.push_str("\tv, ok := _callbackRegistry.Load(int64(uintptr(ctx)))\n");
    out.push_str("\tif !ok {\n\t\treturn\n\t}\n");
    out.push_str(&format!("\tcb, ok := v.({cb_asserted_sig})\n"));
    out.push_str("\tif !ok {\n\t\treturn\n\t}\n");
    let mut call_args = vec!["err".to_string()];
    call_args.extend(go_param_names.iter().cloned());
    out.push_str(&format!("\tcb({})\n", call_args.join(", ")));
    out.push_str("}\n\n");

    // Wrapper function.
    let mut go_params: Vec<String> = Vec::new();
    if f.cancellable {
        go_params.push("ctx context.Context".into());
    }
    go_params.extend(
        f.params
            .iter()
            .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty))),
    );

    if let Some(msg) = &f.deprecated {
        out.push_str(&format!("// Deprecated: {msg}\n"));
    }

    out.push_str(&format!(
        "func {go_name}({}) <-chan {result_ty} {{\n",
        go_params.join(", ")
    ));
    out.push_str(&format!("\tch := make(chan {result_ty}, 1)\n"));
    out.push_str("\tgo func() {\n");
    out.push_str("\t\tdefer close(ch)\n");
    out.push_str("\t\tdone := make(chan struct{})\n");

    // Build the closure signature: func(err *C.{prefix}_error, <extra params>).
    let mut closure_params = vec![format!("err *C.{prefix}_error")];
    closure_params.extend(async_cb_go_params(&f.returns, module, prefix));
    out.push_str(&format!(
        "\t\tcb := func({}) {{\n",
        closure_params.join(", ")
    ));
    out.push_str(&format!("\t\t\tvar res {result_ty}\n"));
    out.push_str("\t\t\tif err != nil && err.code != 0 {\n");
    out.push_str("\t\t\t\tres.Err = fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(err.message), int(err.code))\n");
    out.push_str(&format!("\t\t\t\tC.{prefix}_error_clear(err)\n"));
    out.push_str("\t\t\t}");
    if let Some(ret) = &f.returns {
        out.push_str(" else {\n");
        emit_async_value_from_cb_params(out, "\t\t\t\t", ret, module, prefix);
        out.push_str("\t\t\t}\n");
    } else {
        out.push('\n');
    }
    out.push_str("\t\t\tch <- res\n");
    out.push_str("\t\t\tclose(done)\n");
    out.push_str("\t\t}\n");

    out.push_str("\t\ttoken := _callbackRegister(cb)\n");
    out.push_str("\t\tdefer _callbackRegistry.Delete(token)\n");
    out.push_str("\t\tcbCtx := unsafe.Pointer(uintptr(token))\n");

    // Marshal input params using the same helpers as sync functions.
    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();
    for p in &f.params {
        emit_param(
            &mut pre,
            &mut c_args,
            &p.name.to_lower_camel_case(),
            &p.ty,
            module,
            prefix,
        );
    }
    // emit_param writes with single-tab indent; reindent to two tabs for the goroutine.
    for line in pre.lines() {
        out.push('\t');
        out.push_str(line);
        out.push('\n');
    }

    out.push_str(&format!(
        "\t\tfn := (C.{cb_type})(unsafe.Pointer(C.{trampoline}))\n"
    ));

    if f.cancellable {
        // Create a native cancel token and wire ctx.Done() to
        // {prefix}_cancel_token_cancel so `context.Context` cancellation
        // propagates to the Rust side. The watcher exits when either the
        // context fires or the async callback signals completion via `done`.
        out.push_str(&format!(
            "\t\tcancelTok := C.{prefix}_cancel_token_create()\n"
        ));
        out.push_str(&format!(
            "\t\tdefer C.{prefix}_cancel_token_destroy(cancelTok)\n"
        ));
        out.push_str("\t\tgo func() {\n");
        out.push_str("\t\t\tselect {\n");
        out.push_str("\t\t\tcase <-ctx.Done():\n");
        out.push_str(&format!(
            "\t\t\t\tC.{prefix}_cancel_token_cancel(cancelTok)\n"
        ));
        out.push_str("\t\t\tcase <-done:\n");
        out.push_str("\t\t\t}\n");
        out.push_str("\t\t}()\n");
        c_args.push("cancelTok".into());
    }
    c_args.push("fn".into());
    c_args.push("cbCtx".into());
    out.push_str(&format!("\t\tC.{c_sym}_async({})\n", c_args.join(", ")));

    out.push_str("\t\t<-done\n");
    out.push_str("\t}()\n");
    out.push_str("\treturn ch\n");
    out.push_str("}\n\n");
}

// ── Top-level rendering ──

fn render_go(api: &Api, c_prefix: &str) -> String {
    let (needs_fmt, needs_unsafe, needs_bool, needs_runtime, needs_registry) = scan_imports(api);
    let needs_context = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable));
    let mut out = String::new();

    out.push_str("package weaveffi\n\n");

    out.push_str("/*\n");
    out.push_str(&format!("#cgo LDFLAGS: -l{c_prefix}\n"));
    out.push_str(&format!("#include \"{c_prefix}.h\"\n"));
    out.push_str("#include <stdlib.h>\n");
    for (m, path) in collect_modules_with_path(&api.modules) {
        for cb in &m.callbacks {
            render_callback_extern_decl(&mut out, cb, &path, c_prefix);
        }
        for f in &m.functions {
            if f.r#async {
                render_async_trampoline_extern_decl(&mut out, &path, f, c_prefix);
            }
        }
    }
    out.push_str("*/\n");
    out.push_str("import \"C\"\n");

    if needs_context || needs_fmt || needs_unsafe || needs_runtime || needs_registry {
        out.push_str("\nimport (\n");
        if needs_context {
            out.push_str("\t\"context\"\n");
        }
        if needs_fmt {
            out.push_str("\t\"fmt\"\n");
        }
        if needs_runtime {
            out.push_str("\t\"runtime\"\n");
        }
        if needs_registry {
            out.push_str("\t\"sync\"\n");
            out.push_str("\t\"sync/atomic\"\n");
        }
        if needs_unsafe {
            out.push_str("\t\"unsafe\"\n");
        }
        out.push_str(")\n");
    }
    out.push('\n');

    if needs_registry {
        render_callback_registry(&mut out);
    }

    if needs_bool {
        out.push_str("func boolToC(b bool) C._Bool {\n");
        out.push_str("\tif b {\n");
        out.push_str("\t\treturn 1\n");
        out.push_str("\t}\n");
        out.push_str("\treturn 0\n");
        out.push_str("}\n\n");
        out.push_str("func cToBool(b C._Bool) bool {\n");
        out.push_str("\treturn b != 0\n");
        out.push_str("}\n\n");
    }

    for (m, path) in collect_modules_with_path(&api.modules) {
        for e in &m.enums {
            render_enum(&mut out, e);
        }
        for cb in &m.callbacks {
            render_callback(&mut out, cb, &path, c_prefix);
        }
        for l in &m.listeners {
            render_listener(&mut out, &path, l, c_prefix);
        }
        for s in &m.structs {
            render_struct(&mut out, &path, s, c_prefix);
            if s.builder {
                render_go_builder(&mut out, &path, s, c_prefix);
            }
        }
        for f in &m.functions {
            if f.r#async {
                render_async_function(&mut out, &path, f, c_prefix);
            } else {
                render_function(&mut out, &path, f, c_prefix);
            }
        }
    }

    out
}

// ── Enums ──

fn render_enum(out: &mut String, e: &EnumDef) {
    let name = e.name.to_upper_camel_case();
    out.push_str(&format!("type {name} int32\n\n"));
    out.push_str("const (\n");
    for v in &e.variants {
        let vname = format!("{name}{}", v.name.to_upper_camel_case());
        out.push_str(&format!("\t{vname} {name} = {}\n", v.value));
    }
    out.push_str(")\n\n");
}

// ── Structs ──

fn render_struct(out: &mut String, module: &str, s: &StructDef, prefix: &str) {
    let name = s.name.to_upper_camel_case();
    let c_tag = format!("{prefix}_{}_{}", module, s.name);

    out.push_str(&format!("type {name} struct {{\n"));
    out.push_str(&format!("\tptr *C.{c_tag}\n"));
    out.push_str("}\n\n");

    out.push_str(&format!("func new{name}(ptr *C.{c_tag}) *{name} {{\n"));
    out.push_str("\tif ptr == nil {\n\t\treturn nil\n\t}\n");
    out.push_str(&format!("\ts := &{name}{{ptr: ptr}}\n"));
    out.push_str(&format!(
        "\truntime.SetFinalizer(s, func(x *{name}) {{ x.Close() }})\n"
    ));
    out.push_str("\treturn s\n");
    out.push_str("}\n\n");

    for field in &s.fields {
        render_getter(out, module, &name, &c_tag, field);
    }

    out.push_str(&format!("func (s *{name}) Close() {{\n"));
    out.push_str("\tif s.ptr != nil {\n");
    out.push_str(&format!("\t\tC.{c_tag}_destroy(s.ptr)\n"));
    out.push_str("\t\ts.ptr = nil\n");
    out.push_str("\t\truntime.SetFinalizer(s, nil)\n");
    out.push_str("\t}\n");
    out.push_str("}\n\n");
}

fn render_go_builder(out: &mut String, module: &str, s: &StructDef, prefix: &str) {
    let name = s.name.to_upper_camel_case();
    let c_tag = format!("{prefix}_{module}_{}", s.name);
    let c_builder_ty = format!("{c_tag}Builder");

    out.push_str(&format!("type {name}Builder struct {{\n"));
    out.push_str(&format!("\thandle *C.{c_builder_ty}\n"));
    out.push_str("}\n\n");

    out.push_str(&format!("func New{name}Builder() *{name}Builder {{\n"));
    out.push_str(&format!(
        "\treturn &{name}Builder{{handle: C.{c_tag}_Builder_new()}}\n"
    ));
    out.push_str("}\n\n");

    for field in &s.fields {
        let method = field.name.to_upper_camel_case();
        let gt = go_type(&field.ty);
        out.push_str(&format!(
            "func (b *{name}Builder) With{method}(value {gt}) *{name}Builder {{\n"
        ));
        let mut pre = String::new();
        let mut c_args: Vec<String> = vec!["b.handle".into()];
        emit_param(&mut pre, &mut c_args, "value", &field.ty, module, prefix);
        out.push_str(&pre);
        out.push_str(&format!(
            "\tC.{c_tag}_Builder_set_{}({})\n",
            field.name,
            c_args.join(", ")
        ));
        out.push_str("\treturn b\n");
        out.push_str("}\n\n");
    }

    out.push_str(&format!(
        "func (b *{name}Builder) Build() (*{name}, error) {{\n"
    ));
    out.push_str(&format!("\tvar cErr C.{prefix}_error\n"));
    out.push_str(&format!(
        "\tresult := C.{c_tag}_Builder_build(b.handle, &cErr)\n"
    ));
    out.push_str(&format!("\tC.{c_tag}_Builder_destroy(b.handle)\n"));
    out.push_str("\tb.handle = nil\n");
    out.push_str("\tif cErr.code != 0 {\n");
    out.push_str("\t\tgoErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(cErr.message), int(cErr.code))\n");
    out.push_str(&format!("\t\tC.{prefix}_error_clear(&cErr)\n"));
    out.push_str("\t\treturn nil, goErr\n");
    out.push_str("\t}\n");
    out.push_str(&format!("\treturn new{name}(result), nil\n"));
    out.push_str("}\n\n");

    out.push_str(&format!("func (b *{name}Builder) Close() {{\n"));
    out.push_str("\tif b.handle != nil {\n");
    out.push_str(&format!("\t\tC.{c_tag}_Builder_destroy(b.handle)\n"));
    out.push_str("\t\tb.handle = nil\n");
    out.push_str("\t}\n");
    out.push_str("}\n\n");
}

fn render_getter(
    out: &mut String,
    _module: &str,
    go_struct: &str,
    c_tag: &str,
    field: &StructField,
) {
    let method = field.name.to_upper_camel_case();
    let ret = go_type(&field.ty);
    let getter = format!("C.{c_tag}_get_{}", field.name);

    out.push_str(&format!("func (s *{go_struct}) {method}() {ret} {{\n"));

    match &field.ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::F64 => {
            let conv = go_scalar_conv(&format!("{getter}(s.ptr)"), &field.ty);
            out.push_str(&format!("\treturn {conv}\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("\treturn cToBool({getter}(s.ptr))\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("\treturn C.GoString({getter}(s.ptr))\n"));
        }
        TypeRef::Enum(_) => {
            out.push_str(&format!("\treturn {ret}({getter}(s.ptr))\n"));
        }
        TypeRef::TypedHandle(n) => {
            let inner = n.to_upper_camel_case();
            out.push_str(&format!("\treturn &{inner}{{ptr: {getter}(s.ptr)}}\n"));
        }
        TypeRef::Struct(n) => {
            let inner = local_type_name(n).to_upper_camel_case();
            out.push_str(&format!("\treturn new{inner}({getter}(s.ptr))\n"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!("\tcStr := {getter}(s.ptr)\n"));
                out.push_str("\tif cStr == nil {\n\t\treturn nil\n\t}\n");
                out.push_str("\tv := C.GoString(cStr)\n");
                out.push_str("\treturn &v\n");
            }
            TypeRef::TypedHandle(n) => {
                let inner_go = n.to_upper_camel_case();
                out.push_str(&format!("\tcPtr := {getter}(s.ptr)\n"));
                out.push_str("\tif cPtr == nil {\n\t\treturn nil\n\t}\n");
                out.push_str(&format!("\treturn &{inner_go}{{ptr: cPtr}}\n"));
            }
            TypeRef::Struct(n) => {
                let inner_go = local_type_name(n).to_upper_camel_case();
                out.push_str(&format!("\tcPtr := {getter}(s.ptr)\n"));
                out.push_str("\tif cPtr == nil {\n\t\treturn nil\n\t}\n");
                out.push_str(&format!("\treturn new{inner_go}(cPtr)\n"));
            }
            TypeRef::Bool => {
                out.push_str(&format!("\tcVal := {getter}(s.ptr)\n"));
                out.push_str("\tif cVal == nil {\n\t\treturn nil\n\t}\n");
                out.push_str("\tv := cToBool(*cVal)\n");
                out.push_str("\treturn &v\n");
            }
            _ => {
                let inner_go = go_type(inner);
                out.push_str(&format!("\tcVal := {getter}(s.ptr)\n"));
                out.push_str("\tif cVal == nil {\n\t\treturn nil\n\t}\n");
                out.push_str(&format!("\tv := {inner_go}(*cVal)\n"));
                out.push_str("\treturn &v\n");
            }
        },
        _ => {
            out.push_str(&format!("\treturn {ret}({getter}(s.ptr))\n"));
        }
    }

    out.push_str("}\n\n");
}

// ── Functions ──

fn render_function(out: &mut String, module: &str, f: &Function, prefix: &str) {
    let c_sym = format!("{prefix}_{module}_{}", f.name);
    let go_name = format!("{}_{}", module, f.name).to_upper_camel_case();

    let go_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", p.name.to_lower_camel_case(), go_type(&p.ty)))
        .collect();

    let ret_sig = match &f.returns {
        Some(ret) => format!("({}, error)", go_type(ret)),
        None => "error".into(),
    };

    if let Some(msg) = &f.deprecated {
        out.push_str(&format!("// Deprecated: {msg}\n"));
    }

    out.push_str(&format!(
        "func {go_name}({}) {ret_sig} {{\n",
        go_params.join(", ")
    ));

    let mut pre = String::new();
    let mut c_args: Vec<String> = Vec::new();

    for p in &f.params {
        emit_param(
            &mut pre,
            &mut c_args,
            &p.name.to_lower_camel_case(),
            &p.ty,
            module,
            prefix,
        );
    }

    if let Some(ref ret) = f.returns {
        emit_return_out_params(&mut pre, &mut c_args, ret);
    }

    pre.push_str(&format!("\tvar cErr C.{prefix}_error\n"));
    c_args.push("&cErr".into());

    out.push_str(&pre);

    let args = c_args.join(", ");
    let c_returns_void = matches!(&f.returns, Some(TypeRef::Map(_, _)));

    if f.returns.is_some() && !c_returns_void {
        out.push_str(&format!("\tresult := C.{c_sym}({args})\n"));
    } else {
        out.push_str(&format!("\tC.{c_sym}({args})\n"));
    }

    out.push_str("\tif cErr.code != 0 {\n");
    out.push_str("\t\tgoErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(cErr.message), int(cErr.code))\n");
    out.push_str(&format!("\t\tC.{prefix}_error_clear(&cErr)\n"));
    if let Some(ref ret) = f.returns {
        out.push_str(&format!("\t\treturn {}, goErr\n", go_zero(ret)));
    } else {
        out.push_str("\t\treturn goErr\n");
    }
    out.push_str("\t}\n");

    if let Some(ref ret) = f.returns {
        emit_return(out, ret, module, &f.name, prefix);
    } else {
        out.push_str("\treturn nil\n");
    }

    out.push_str("}\n\n");
}

// ── Parameter conversion ──

fn emit_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    ty: &TypeRef,
    module: &str,
    prefix: &str,
) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 => {
            args.push(c_scalar_conv(name, ty, module, prefix));
        }
        TypeRef::Bool => args.push(format!("boolToC({name})")),
        TypeRef::Handle => args.push(format!("C.{prefix}_handle_t({name})")),
        TypeRef::Enum(n) => args.push(format!("C.{prefix}_{module}_{n}({name})")),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => args.push(format!("{name}.ptr")),

        TypeRef::StringUtf8 => {
            let bv = format!("{name}Bytes");
            let pv = format!("c{}Ptr", name.to_upper_camel_case());
            let lv = format!("c{}Len", name.to_upper_camel_case());
            pre.push_str(&format!("\t{bv} := []byte({name})\n"));
            pre.push_str(&format!("\tvar {pv} *C.uint8_t\n"));
            pre.push_str(&format!("\t{lv} := C.size_t(len({bv}))\n"));
            pre.push_str(&format!("\tif len({bv}) > 0 {{\n"));
            pre.push_str(&format!(
                "\t\t{pv} = (*C.uint8_t)(unsafe.Pointer(&{bv}[0]))\n"
            ));
            pre.push_str("\t}\n");
            args.push(pv);
            args.push(lv);
        }

        TypeRef::BorrowedStr => {
            let cv = format!("c{}", name.to_upper_camel_case());
            pre.push_str(&format!("\t{cv} := C.CString({name})\n"));
            pre.push_str(&format!("\tdefer C.free(unsafe.Pointer({cv}))\n"));
            args.push(cv);
        }

        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let pv = format!("c{}Ptr", name.to_upper_camel_case());
            let lv = format!("c{}Len", name.to_upper_camel_case());
            pre.push_str(&format!("\tvar {pv} *C.uint8_t\n"));
            pre.push_str(&format!("\t{lv} := C.size_t(len({name}))\n"));
            pre.push_str(&format!("\tif len({name}) > 0 {{\n"));
            pre.push_str(&format!(
                "\t\t{pv} = (*C.uint8_t)(unsafe.Pointer(&{name}[0]))\n"
            ));
            pre.push_str("\t}\n");
            args.push(pv);
            args.push(lv);
        }

        TypeRef::Optional(inner) => emit_optional_param(pre, args, name, inner, module, prefix),
        TypeRef::List(inner) => emit_list_param(pre, args, name, inner, module, prefix),
        TypeRef::Map(k, v) => emit_map_param(pre, args, name, k, v, module, prefix),

        TypeRef::Callback(cb_name) => {
            let cn = name.to_upper_camel_case();
            let tok_var = format!("c{cn}Token");
            let fn_var = format!("c{cn}Fn");
            let ctx_var = format!("c{cn}Ctx");
            pre.push_str(&format!("\t{tok_var} := _callbackRegister({name})\n"));
            pre.push_str(&format!(
                "\t{fn_var} := (C.{prefix}_{module}_{cb_name})(unsafe.Pointer(C.weaveffiGoCbTrampoline_{module}_{cb_name}))\n"
            ));
            pre.push_str(&format!(
                "\t{ctx_var} := unsafe.Pointer(uintptr({tok_var}))\n"
            ));
            args.push(fn_var);
            args.push(ctx_var);
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

fn emit_optional_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    inner: &TypeRef,
    module: &str,
    prefix: &str,
) {
    let cv = format!("c{}", name.to_upper_camel_case());

    match inner {
        TypeRef::StringUtf8 => {
            let bv = format!("{name}Bytes");
            let pv = format!("c{}Ptr", name.to_upper_camel_case());
            let lv = format!("c{}Len", name.to_upper_camel_case());
            pre.push_str(&format!("\tvar {bv} []byte\n"));
            pre.push_str(&format!("\tvar {pv} *C.uint8_t\n"));
            pre.push_str(&format!("\tvar {lv} C.size_t\n"));
            pre.push_str(&format!("\tif {name} != nil {{\n"));
            pre.push_str(&format!("\t\t{bv} = []byte(*{name})\n"));
            pre.push_str(&format!("\t\t{lv} = C.size_t(len({bv}))\n"));
            pre.push_str(&format!("\t\tif len({bv}) > 0 {{\n"));
            pre.push_str(&format!(
                "\t\t\t{pv} = (*C.uint8_t)(unsafe.Pointer(&{bv}[0]))\n"
            ));
            pre.push_str("\t\t}\n");
            pre.push_str("\t}\n");
            args.push(pv);
            args.push(lv);
        }
        TypeRef::BorrowedStr => {
            pre.push_str(&format!("\tvar {cv} *C.char\n"));
            pre.push_str(&format!("\tif {name} != nil {{\n"));
            pre.push_str(&format!("\t\t{cv} = C.CString(*{name})\n"));
            pre.push_str(&format!("\t\tdefer C.free(unsafe.Pointer({cv}))\n"));
            pre.push_str("\t}\n");
            args.push(cv);
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            let ct = c_opaque_type(inner, module, prefix);
            pre.push_str(&format!("\tvar {cv} *C.{ct}\n"));
            pre.push_str(&format!("\tif {name} != nil {{\n"));
            pre.push_str(&format!("\t\t{cv} = {name}.ptr\n"));
            pre.push_str("\t}\n");
            args.push(cv);
        }
        _ => {
            if let Some(ct) = c_scalar_type(inner, module, prefix) {
                pre.push_str(&format!("\tvar {cv} *{ct}\n"));
                pre.push_str(&format!("\tif {name} != nil {{\n"));
                let conv = c_scalar_conv(&format!("*{name}"), inner, module, prefix);
                pre.push_str(&format!("\t\ttmp := {conv}\n"));
                pre.push_str(&format!("\t\t{cv} = &tmp\n"));
                pre.push_str("\t}\n");
                args.push(cv);
            } else {
                args.push(name.to_string());
            }
        }
    }
}

fn emit_list_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    inner: &TypeRef,
    module: &str,
    prefix: &str,
) {
    let cn = name.to_upper_camel_case();
    let pv = format!("c{cn}Ptr");
    let lv = format!("c{cn}Len");

    pre.push_str(&format!("\t{lv} := C.size_t(len({name}))\n"));

    if let Some(ct) = c_scalar_type(inner, module, prefix) {
        if matches!(inner, TypeRef::Bool) {
            let arr = format!("c{cn}Arr");
            pre.push_str(&format!("\t{arr} := make([]C._Bool, len({name}))\n"));
            pre.push_str(&format!("\tfor i, b := range {name} {{\n"));
            pre.push_str(&format!("\t\t{arr}[i] = boolToC(b)\n"));
            pre.push_str("\t}\n");
            pre.push_str(&format!("\tvar {pv} *C._Bool\n"));
            pre.push_str(&format!("\tif len({arr}) > 0 {{\n"));
            pre.push_str(&format!("\t\t{pv} = &{arr}[0]\n"));
            pre.push_str("\t}\n");
        } else {
            pre.push_str(&format!("\tvar {pv} *{ct}\n"));
            pre.push_str(&format!("\tif len({name}) > 0 {{\n"));
            pre.push_str(&format!("\t\t{pv} = (*{ct})(unsafe.Pointer(&{name}[0]))\n"));
            pre.push_str("\t}\n");
        }
    } else if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let arr = format!("c{cn}Arr");
        pre.push_str(&format!("\t{arr} := make([]*C.char, len({name}))\n"));
        pre.push_str(&format!("\tfor i, s := range {name} {{\n"));
        pre.push_str(&format!("\t\t{arr}[i] = C.CString(s)\n"));
        pre.push_str("\t}\n");
        pre.push_str("\tdefer func() {\n");
        pre.push_str(&format!("\t\tfor _, p := range {arr} {{\n"));
        pre.push_str("\t\t\tC.free(unsafe.Pointer(p))\n");
        pre.push_str("\t\t}\n");
        pre.push_str("\t}()\n");
        pre.push_str(&format!("\tvar {pv} **C.char\n"));
        pre.push_str(&format!("\tif len({arr}) > 0 {{\n"));
        pre.push_str(&format!(
            "\t\t{pv} = (**C.char)(unsafe.Pointer(&{arr}[0]))\n"
        ));
        pre.push_str("\t}\n");
    } else if let TypeRef::Struct(n) | TypeRef::TypedHandle(n) = inner {
        let ct = format!("C.{prefix}_{module}_{n}");
        let arr = format!("c{cn}Arr");
        pre.push_str(&format!("\t{arr} := make([]*{ct}, len({name}))\n"));
        pre.push_str(&format!("\tfor i, item := range {name} {{\n"));
        pre.push_str(&format!("\t\t{arr}[i] = item.ptr\n"));
        pre.push_str("\t}\n");
        pre.push_str(&format!("\tvar {pv} **{ct}\n"));
        pre.push_str(&format!("\tif len({arr}) > 0 {{\n"));
        pre.push_str(&format!("\t\t{pv} = (**{ct})(unsafe.Pointer(&{arr}[0]))\n"));
        pre.push_str("\t}\n");
    } else {
        pre.push_str(&format!("\tvar {pv} unsafe.Pointer\n"));
    }

    args.push(pv);
    args.push(lv);
}

fn emit_map_param(
    pre: &mut String,
    args: &mut Vec<String>,
    name: &str,
    k: &TypeRef,
    v: &TypeRef,
    module: &str,
    prefix: &str,
) {
    let cn = name.to_upper_camel_case();
    let lv = format!("c{cn}Len");
    let go_k = go_type(k);
    let go_v = go_type(v);

    pre.push_str(&format!("\t{lv} := C.size_t(len({name}))\n"));
    pre.push_str(&format!("\tkeys{cn} := make([]{go_k}, 0, len({name}))\n"));
    pre.push_str(&format!("\tvals{cn} := make([]{go_v}, 0, len({name}))\n"));
    pre.push_str(&format!("\tfor mk, mv := range {name} {{\n"));
    pre.push_str(&format!("\t\tkeys{cn} = append(keys{cn}, mk)\n"));
    pre.push_str(&format!("\t\tvals{cn} = append(vals{cn}, mv)\n"));
    pre.push_str("\t}\n");

    let kp = format!("c{cn}KeysPtr");
    emit_map_array(pre, &kp, &format!("keys{cn}"), k, module, prefix);
    args.push(kp);

    let vp = format!("c{cn}ValsPtr");
    emit_map_array(pre, &vp, &format!("vals{cn}"), v, module, prefix);
    args.push(vp);

    args.push(lv);
}

fn emit_map_array(
    pre: &mut String,
    ptr_var: &str,
    slice_name: &str,
    ty: &TypeRef,
    module: &str,
    prefix: &str,
) {
    if let Some(ct) = c_scalar_type(ty, module, prefix) {
        pre.push_str(&format!("\tvar {ptr_var} *{ct}\n"));
        pre.push_str(&format!("\tif len({slice_name}) > 0 {{\n"));
        pre.push_str(&format!(
            "\t\t{ptr_var} = (*{ct})(unsafe.Pointer(&{slice_name}[0]))\n"
        ));
        pre.push_str("\t}\n");
    } else if matches!(ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        let arr = format!("{ptr_var}Arr");
        pre.push_str(&format!("\t{arr} := make([]*C.char, len({slice_name}))\n"));
        pre.push_str(&format!("\tfor i, s := range {slice_name} {{\n"));
        pre.push_str(&format!("\t\t{arr}[i] = C.CString(s)\n"));
        pre.push_str("\t}\n");
        pre.push_str("\tdefer func() {\n");
        pre.push_str(&format!("\t\tfor _, p := range {arr} {{\n"));
        pre.push_str("\t\t\tC.free(unsafe.Pointer(p))\n");
        pre.push_str("\t\t}\n");
        pre.push_str("\t}()\n");
        pre.push_str(&format!("\tvar {ptr_var} **C.char\n"));
        pre.push_str(&format!("\tif len({arr}) > 0 {{\n"));
        pre.push_str(&format!(
            "\t\t{ptr_var} = (**C.char)(unsafe.Pointer(&{arr}[0]))\n"
        ));
        pre.push_str("\t}\n");
    } else {
        pre.push_str(&format!("\tvar {ptr_var} unsafe.Pointer\n"));
    }
}

// ── Return out-params ──

fn emit_return_out_params(pre: &mut String, args: &mut Vec<String>, ty: &TypeRef) {
    match ty {
        TypeRef::List(_) | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            pre.push_str("\tvar cOutLen C.size_t\n");
            args.push("&cOutLen".into());
        }
        TypeRef::Optional(inner) => emit_return_out_params(pre, args, inner),
        _ => {}
    }
}

// ── Return conversion ──

fn emit_return(out: &mut String, ty: &TypeRef, module: &str, func_name: &str, prefix: &str) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::F64 => {
            let conv = go_scalar_conv("result", ty);
            out.push_str(&format!("\treturn {conv}, nil\n"));
        }
        TypeRef::Bool => out.push_str("\treturn cToBool(result), nil\n"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("\tgoResult := C.GoString(result)\n");
            out.push_str(&format!("\tC.{prefix}_free_string(result)\n"));
            out.push_str("\treturn goResult, nil\n");
        }
        TypeRef::Enum(_) => {
            let conv = go_scalar_conv("result", ty);
            out.push_str(&format!("\treturn {conv}, nil\n"));
        }
        TypeRef::TypedHandle(n) => {
            let g = n.to_upper_camel_case();
            out.push_str(&format!("\treturn &{g}{{ptr: result}}, nil\n"));
        }
        TypeRef::Struct(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            out.push_str(&format!("\treturn new{g}(result), nil\n"));
        }
        TypeRef::Optional(inner) => emit_optional_return(out, inner, module, prefix),
        TypeRef::List(inner) => emit_list_return(out, inner, module, prefix),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("\tif result == nil {\n\t\treturn nil, nil\n\t}\n");
            out.push_str("\tgoResult := C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))\n");
            out.push_str(&format!("\tC.{prefix}_free_bytes(result, cOutLen)\n"));
            out.push_str("\treturn goResult, nil\n");
        }
        TypeRef::Map(k, v) => emit_map_return(out, k, v),
        TypeRef::Callback(_) => out.push_str("\treturn nil, nil\n"),
        TypeRef::Iterator(inner) => emit_iterator_return(out, inner, module, func_name, prefix),
    }
}

fn emit_optional_return(out: &mut String, inner: &TypeRef, _module: &str, prefix: &str) {
    out.push_str("\tif result == nil {\n\t\treturn nil, nil\n\t}\n");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("\tv := C.GoString(result)\n");
            out.push_str(&format!("\tC.{prefix}_free_string(result)\n"));
            out.push_str("\treturn &v, nil\n");
        }
        TypeRef::TypedHandle(n) => {
            let g = n.to_upper_camel_case();
            out.push_str(&format!("\treturn &{g}{{ptr: result}}, nil\n"));
        }
        TypeRef::Struct(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            out.push_str(&format!("\treturn new{g}(result), nil\n"));
        }
        TypeRef::Bool => {
            out.push_str("\tv := cToBool(*result)\n");
            out.push_str("\treturn &v, nil\n");
        }
        _ => {
            let gt = go_type(inner);
            out.push_str(&format!("\tv := {gt}(*result)\n"));
            out.push_str("\treturn &v, nil\n");
        }
    }
}

fn emit_list_return(out: &mut String, inner: &TypeRef, module: &str, prefix: &str) {
    out.push_str("\tcount := int(cOutLen)\n");
    out.push_str("\tif count == 0 || result == nil {\n\t\treturn nil, nil\n\t}\n");

    let gi = go_type(inner);
    out.push_str(&format!("\tgoResult := make([]{gi}, count)\n"));

    if let Some(ct) = c_scalar_type(inner, module, prefix) {
        out.push_str(&format!(
            "\tcSlice := unsafe.Slice((*{ct})(unsafe.Pointer(result)), count)\n"
        ));
        out.push_str("\tfor i, v := range cSlice {\n");
        let conv = go_scalar_conv("v", inner);
        out.push_str(&format!("\t\tgoResult[i] = {conv}\n"));
        out.push_str("\t}\n");
    } else if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        out.push_str("\tcSlice := unsafe.Slice((**C.char)(unsafe.Pointer(result)), count)\n");
        out.push_str("\tfor i, v := range cSlice {\n");
        out.push_str("\t\tgoResult[i] = C.GoString(v)\n");
        out.push_str("\t}\n");
    } else if let TypeRef::TypedHandle(n) = inner {
        let ct = format!("C.{prefix}_{module}_{n}");
        let gs = n.to_upper_camel_case();
        out.push_str(&format!(
            "\tcSlice := unsafe.Slice((**{ct})(unsafe.Pointer(result)), count)\n"
        ));
        out.push_str("\tfor i, v := range cSlice {\n");
        out.push_str(&format!("\t\tgoResult[i] = &{gs}{{ptr: v}}\n"));
        out.push_str("\t}\n");
    } else if let TypeRef::Struct(n) = inner {
        let ct = format!("C.{prefix}_{module}_{n}");
        let gs = local_type_name(n).to_upper_camel_case();
        out.push_str(&format!(
            "\tcSlice := unsafe.Slice((**{ct})(unsafe.Pointer(result)), count)\n"
        ));
        out.push_str("\tfor i, v := range cSlice {\n");
        out.push_str(&format!("\t\tgoResult[i] = new{gs}(v)\n"));
        out.push_str("\t}\n");
    }

    out.push_str("\treturn goResult, nil\n");
}

fn emit_map_return(out: &mut String, k: &TypeRef, v: &TypeRef) {
    let gk = go_type(k);
    let gv = go_type(v);
    out.push_str(&format!("\tgoResult := make(map[{gk}]{gv})\n"));
    out.push_str("\treturn goResult, nil\n");
}

// ── Iterator return ──

fn iter_out_item_c_type(inner: &TypeRef, module: &str, prefix: &str) -> String {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "*C.char".into(),
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => format!("*C.{prefix}_{module}_{n}"),
        _ => c_scalar_type(inner, module, prefix).unwrap_or_else(|| "C.int32_t".into()),
    }
}

fn iter_item_to_go(var: &str, inner: &TypeRef) -> String {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("C.GoString({var})"),
        TypeRef::Bool => format!("cToBool({var})"),
        TypeRef::Struct(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            format!("new{g}({var})")
        }
        TypeRef::TypedHandle(n) => {
            let g = n.to_upper_camel_case();
            format!("&{g}{{ptr: {var}}}")
        }
        _ => go_scalar_conv(var, inner),
    }
}

fn emit_iterator_return(
    out: &mut String,
    inner: &TypeRef,
    module: &str,
    func_name: &str,
    prefix: &str,
) {
    let pascal = func_name.to_upper_camel_case();
    let iter_tag = format!("{prefix}_{module}_{pascal}Iterator");
    let gi = go_type(inner);
    let c_item_ty = iter_out_item_c_type(inner, module, prefix);
    let conv = iter_item_to_go("outItem", inner);

    out.push_str(&format!("\tch := make(chan {gi})\n"));
    out.push_str("\tgo func() {\n");
    out.push_str("\t\tdefer close(ch)\n");
    out.push_str(&format!("\t\tdefer C.{iter_tag}_destroy(result)\n"));
    out.push_str("\t\tfor {\n");
    out.push_str(&format!("\t\t\tvar outItem {c_item_ty}\n"));
    out.push_str(&format!("\t\t\tvar itemErr C.{prefix}_error\n"));
    out.push_str(&format!(
        "\t\t\thas := C.{iter_tag}_next(result, &outItem, &itemErr)\n"
    ));
    out.push_str("\t\t\tif itemErr.code != 0 {\n");
    out.push_str(&format!("\t\t\t\tC.{prefix}_error_clear(&itemErr)\n"));
    out.push_str("\t\t\t\treturn\n");
    out.push_str("\t\t\t}\n");
    out.push_str("\t\t\tif has == 0 {\n");
    out.push_str("\t\t\t\treturn\n");
    out.push_str("\t\t\t}\n");
    if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        out.push_str(&format!("\t\t\titem := {conv}\n"));
        out.push_str(&format!("\t\t\tC.{prefix}_free_string(outItem)\n"));
        out.push_str("\t\t\tch <- item\n");
    } else {
        out.push_str(&format!("\t\t\tch <- {conv}\n"));
    }
    out.push_str("\t\t}\n");
    out.push_str("\t}()\n");
    out.push_str("\treturn ch, nil\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;
    use weaveffi_core::config::GeneratorConfig;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn calculator_api() -> Api {
        Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "calculator".into(),
                functions: vec![
                    Function {
                        name: "add".into(),
                        params: vec![
                            Param {
                                name: "a".into(),
                                ty: TypeRef::I32,
                                mutable: false,
                            },
                            Param {
                                name: "b".into(),
                                ty: TypeRef::I32,
                                mutable: false,
                            },
                        ],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "echo".into(),
                        params: vec![Param {
                            name: "msg".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        }
    }

    #[test]
    fn name_returns_go() {
        assert_eq!(GoGenerator.name(), "go");
    }

    #[test]
    fn output_files_correct() {
        let api = calculator_api();
        let out = Utf8Path::new("out");
        let files = GoGenerator.output_files(&api, out);
        assert_eq!(
            files,
            vec![
                out.join("go/weaveffi.go").to_string(),
                out.join("go/go.mod").to_string(),
                out.join("go/go.sum").to_string(),
                out.join("go/doc.go").to_string(),
                out.join("go/weaveffi_test.go").to_string(),
                out.join("go/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn go_output_files_with_config_respects_naming() {
        // `go_module_path` is only used in go.mod content, so it must not change
        // the emitted file paths.
        let api = calculator_api();
        let out = Utf8Path::new("out");

        let expected = vec![
            out.join("go/weaveffi.go").to_string(),
            out.join("go/go.mod").to_string(),
            out.join("go/go.sum").to_string(),
            out.join("go/doc.go").to_string(),
            out.join("go/weaveffi_test.go").to_string(),
            out.join("go/README.md").to_string(),
        ];

        let default_files =
            GoGenerator.output_files_with_config(&api, out, &GeneratorConfig::default());
        assert_eq!(default_files, expected);

        let config = GeneratorConfig {
            go_module_path: Some("github.com/myorg/mylib".into()),
            ..GeneratorConfig::default()
        };
        let custom_files = GoGenerator.output_files_with_config(&api, out, &config);
        assert_eq!(
            custom_files, expected,
            "go_module_path must not affect output paths"
        );
    }

    #[test]
    fn package_and_cgo_preamble() {
        let go = render_go(&calculator_api(), "weaveffi");
        assert!(go.starts_with("package weaveffi\n"), "missing package");
        assert!(
            go.contains("#cgo LDFLAGS: -lweaveffi"),
            "missing LDFLAGS: {go}"
        );
        assert!(
            go.contains("#include \"weaveffi.h\""),
            "missing weaveffi.h include: {go}"
        );
        assert!(go.contains("import \"C\""), "missing import C: {go}");
    }

    #[test]
    fn imports_fmt_and_unsafe() {
        let go = render_go(&calculator_api(), "weaveffi");
        assert!(go.contains("\"fmt\""), "missing fmt import: {go}");
        assert!(go.contains("\"unsafe\""), "missing unsafe import: {go}");
    }

    #[test]
    fn simple_i32_function() {
        let go = render_go(&calculator_api(), "weaveffi");
        assert!(
            go.contains("func CalculatorAdd(a int32, b int32) (int32, error)"),
            "missing function sig: {go}"
        );
        assert!(
            go.contains("C.weaveffi_calculator_add("),
            "missing C call: {go}"
        );
        assert!(go.contains("C.int32_t(a)"), "missing param cast: {go}");
        assert!(
            go.contains("return int32(result), nil"),
            "missing return: {go}"
        );
    }

    #[test]
    fn string_function() {
        let go = render_go(&calculator_api(), "weaveffi");
        assert!(
            go.contains("func CalculatorEcho(msg string) (string, error)"),
            "missing echo sig: {go}"
        );
        assert!(
            go.contains("msgBytes := []byte(msg)"),
            "missing []byte conversion: {go}"
        );
        assert!(
            go.contains("cMsgPtr = (*C.uint8_t)(unsafe.Pointer(&msgBytes[0]))"),
            "missing ptr from byte slice: {go}"
        );
        assert!(
            go.contains("cMsgLen := C.size_t(len(msgBytes))"),
            "missing length: {go}"
        );
        assert!(
            go.contains("C.weaveffi_calculator_echo(cMsgPtr, cMsgLen, &cErr)"),
            "string param should be passed as (ptr, len, &cErr): {go}"
        );
        assert!(go.contains("C.GoString(result)"), "missing GoString: {go}");
        assert!(
            go.contains("C.weaveffi_free_string(result)"),
            "missing free_string: {go}"
        );
    }

    #[test]
    fn error_handling() {
        let go = render_go(&calculator_api(), "weaveffi");
        assert!(
            go.contains("var cErr C.weaveffi_error"),
            "missing error var: {go}"
        );
        assert!(
            go.contains("if cErr.code != 0"),
            "missing error check: {go}"
        );
        assert!(
            go.contains("C.weaveffi_error_clear(&cErr)"),
            "missing error clear: {go}"
        );
        assert!(go.contains("fmt.Errorf("), "missing Errorf: {go}");
    }

    #[test]
    fn enum_generation() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "paint".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Color".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Red".into(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Green".into(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Blue".into(),
                            value: 2,
                            doc: None,
                        },
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("type Color int32"),
            "missing enum typedef: {go}"
        );
        assert!(
            go.contains("ColorRed Color = 0"),
            "missing Red variant: {go}"
        );
        assert!(
            go.contains("ColorGreen Color = 1"),
            "missing Green variant: {go}"
        );
        assert!(
            go.contains("ColorBlue Color = 2"),
            "missing Blue variant: {go}"
        );
    }

    #[test]
    fn struct_with_getters_and_close() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![
                        StructField {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(go.contains("type Contact struct {"), "missing struct: {go}");
        assert!(
            go.contains("ptr *C.weaveffi_contacts_Contact"),
            "missing ptr field: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Name() string"),
            "missing Name getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Age() int32"),
            "missing Age getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Close()"),
            "missing Close: {go}"
        );
        assert!(
            go.contains("C.weaveffi_contacts_Contact_destroy(s.ptr)"),
            "missing destroy call: {go}"
        );
        assert!(
            go.contains("s.ptr = nil"),
            "missing nil assignment after destroy: {go}"
        );
    }

    #[test]
    fn struct_builder_type_and_setters() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "geo".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Point".into(),
                    doc: None,
                    builder: true,
                    fields: vec![StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("type PointBuilder struct {"),
            "builder type: {go}"
        );
        assert!(
            go.contains("handle *C.weaveffi_geo_PointBuilder"),
            "typed builder handle: {go}"
        );
        assert!(
            go.contains(
                "func NewPointBuilder() *PointBuilder {\n\treturn &PointBuilder{handle: C.weaveffi_geo_Point_Builder_new()}"
            ),
            "constructor must call C Builder_new: {go}"
        );
        assert!(
            go.contains("func (b *PointBuilder) WithX(value float64) *PointBuilder"),
            "WithX: {go}"
        );
        assert!(
            go.contains("C.weaveffi_geo_Point_Builder_set_x(b.handle, C.double(value))"),
            "WithX should call C setter with typed handle: {go}"
        );
    }

    #[test]
    fn void_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "system".into(),
                functions: vec![Function {
                    name: "reset".into(),
                    params: vec![],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("func SystemReset() error"),
            "missing void function sig: {go}"
        );
        assert!(
            go.contains("return nil"),
            "missing nil return for void: {go}"
        );
    }

    #[test]
    fn handle_type() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "create".into(),
                    params: vec![Param {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("(int64, error)"),
            "handle return should be int64: {go}"
        );
        assert!(
            go.contains("return int64(result), nil"),
            "missing handle return conversion: {go}"
        );
    }

    #[test]
    fn bool_function_generates_helpers() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "logic".into(),
                functions: vec![Function {
                    name: "negate".into(),
                    params: vec![Param {
                        name: "val".into(),
                        ty: TypeRef::Bool,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Bool),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(go.contains("func boolToC("), "missing boolToC: {go}");
        assert!(go.contains("func cToBool("), "missing cToBool: {go}");
        assert!(
            go.contains("boolToC(val)"),
            "missing boolToC call for param: {go}"
        );
        assert!(
            go.contains("cToBool(result)"),
            "missing cToBool for return: {go}"
        );
    }

    #[test]
    fn enum_param_and_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "paint".into(),
                functions: vec![Function {
                    name: "mix".into(),
                    params: vec![Param {
                        name: "a".into(),
                        ty: TypeRef::Enum("Color".into()),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Enum("Color".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Color".into(),
                    doc: None,
                    variants: vec![EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                    }],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("func PaintMix(a Color) (Color, error)"),
            "missing enum function sig: {go}"
        );
        assert!(
            go.contains("C.weaveffi_paint_Color(a)"),
            "missing enum param conversion: {go}"
        );
        assert!(
            go.contains("Color(result)"),
            "missing enum return conversion: {go}"
        );
    }

    #[test]
    fn struct_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("(*Contact, error)"),
            "missing struct return type: {go}"
        );
        assert!(
            go.contains("newContact(result)"),
            "missing struct factory wrap: {go}"
        );
    }

    #[test]
    fn optional_string_param() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "query".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("query *string"),
            "optional string param should be *string: {go}"
        );
        assert!(
            go.contains("if query != nil"),
            "missing nil check for optional: {go}"
        );
        assert!(
            go.contains("queryBytes = []byte(*query)"),
            "missing []byte conversion of dereferenced optional: {go}"
        );
        assert!(
            go.contains("cQueryPtr = (*C.uint8_t)(unsafe.Pointer(&queryBytes[0]))"),
            "missing ptr from byte slice for optional: {go}"
        );
        assert!(
            go.contains("C.weaveffi_store_find(cQueryPtr, cQueryLen, &cErr)"),
            "optional string param should call C with (ptr, len, &cErr): {go}"
        );
    }

    #[test]
    fn optional_struct_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Contact".into(),
                    )))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("(*Contact, error)"),
            "optional struct return: {go}"
        );
        assert!(go.contains("if result == nil"), "missing nil check: {go}");
    }

    #[test]
    fn list_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "list_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("([]int32, error)"),
            "missing list return type: {go}"
        );
        assert!(
            go.contains("var cOutLen C.size_t"),
            "missing out_len var: {go}"
        );
        assert!(go.contains("unsafe.Slice("), "missing unsafe.Slice: {go}");
    }

    #[test]
    fn struct_list_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("([]*Contact, error)"),
            "missing struct list return: {go}"
        );
        assert!(
            go.contains("C.weaveffi_contacts_Contact"),
            "missing C struct type in list conversion: {go}"
        );
    }

    #[test]
    fn async_functions_emitted() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![Function {
                    name: "run".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("func TasksRun("),
            "async function wrapper should be emitted: {go}"
        );
    }

    #[test]
    fn go_async_returns_channel() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![
                    Function {
                        name: "run".into(),
                        params: vec![Param {
                            name: "id".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "cancellable_run".into(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: true,
                        cancellable: true,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("type TasksRunResult struct {"),
            "Result struct should be emitted: {go}"
        );
        assert!(
            go.contains("Value string"),
            "Result struct should carry value field: {go}"
        );
        assert!(
            go.contains("Err error"),
            "Result struct should carry error field: {go}"
        );
        assert!(
            go.contains("func TasksRun(id int32) <-chan TasksRunResult {"),
            "wrapper should return a receive-only channel: {go}"
        );
        assert!(
            go.contains("ch := make(chan TasksRunResult, 1)"),
            "wrapper should create a buffered channel: {go}"
        );
        assert!(
            go.contains("go func() {"),
            "wrapper should spawn a goroutine: {go}"
        );
        assert!(
            go.contains("defer close(ch)"),
            "goroutine should close the channel on exit: {go}"
        );
        assert!(
            go.contains("_callbackRegister(cb)"),
            "callback should be registered before invoking the C entry point: {go}"
        );
        assert!(
            go.contains("//export weaveffiGoAsyncTrampoline_tasks_run"),
            "exported trampoline should be emitted: {go}"
        );
        assert!(
            go.contains("C.weaveffi_tasks_run_async("),
            "the _async C entry point should be invoked: {go}"
        );
        assert!(
            go.contains("C.weaveffi_tasks_cancellable_run_async("),
            "cancellable async should still use the _async entry point: {go}"
        );
        assert!(
            go.contains("return ch"),
            "wrapper should return the channel: {go}"
        );
    }

    #[test]
    fn go_cancellable_async_wires_context_to_cancel_token() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "tasks".into(),
                functions: vec![
                    Function {
                        name: "run".into(),
                        params: vec![Param {
                            name: "id".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: true,
                        cancellable: true,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "fire".into(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: true,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");

        assert!(
            go.contains("\"context\""),
            "context import must be emitted for cancellable async: {go}"
        );
        assert!(
            go.contains("func TasksRun(ctx context.Context, id int32) <-chan TasksRunResult {"),
            "cancellable async wrapper must take context.Context first: {go}"
        );
        assert!(
            go.contains("cancelTok := C.weaveffi_cancel_token_create()"),
            "cancellable async must create a native cancel token: {go}"
        );
        assert!(
            go.contains("defer C.weaveffi_cancel_token_destroy(cancelTok)"),
            "cancellable async must destroy the native cancel token: {go}"
        );
        assert!(
            go.contains("C.weaveffi_cancel_token_cancel(cancelTok)"),
            "cancellable async must wire ctx.Done() to weaveffi_cancel_token_cancel: {go}"
        );
        assert!(
            go.contains("case <-ctx.Done():"),
            "cancellable async must select on ctx.Done(): {go}"
        );
        let run_async_call = go
            .lines()
            .find(|l| l.contains("C.weaveffi_tasks_run_async("))
            .expect("run_async call must be emitted");
        assert!(
            run_async_call.contains("cancelTok") && run_async_call.contains("cbCtx"),
            "cancellable async must forward the native token to _async: {run_async_call}"
        );

        let fire_sig = go
            .lines()
            .find(|l| l.contains("func TasksFire("))
            .expect("non-cancellable fire wrapper must still be emitted");
        assert!(
            !fire_sig.contains("context.Context"),
            "non-cancellable async must not accept context.Context: {fire_sig}"
        );
    }

    #[test]
    fn generates_file_on_disk() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        GoGenerator.generate(&api, out_dir).unwrap();

        let go_file = tmp.join("go/weaveffi.go");
        assert!(go_file.exists(), "go/weaveffi.go should exist");
        let contents = std::fs::read_to_string(&go_file).unwrap();
        assert!(
            contents.contains("package weaveffi"),
            "file should contain package declaration"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_generates_go_mod() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_mod");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator.generate(&api, out_dir).unwrap();

        let go_mod_path = tmp.join("go/go.mod");
        assert!(go_mod_path.exists(), "go/go.mod should exist");
        let go_mod = std::fs::read_to_string(&go_mod_path).unwrap();
        assert!(
            go_mod.contains("module github.com/example/weaveffi"),
            "missing module directive: {go_mod}"
        );
        assert!(go_mod.contains("go 1.21"), "missing go version: {go_mod}");

        let readme_path = tmp.join("go/README.md");
        assert!(readme_path.exists(), "go/README.md should exist");
        let readme = std::fs::read_to_string(&readme_path).unwrap();
        assert!(
            readme.contains("CGo"),
            "README should mention CGo: {readme}"
        );
        assert!(
            readme.contains("go build"),
            "README should mention go build: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn optional_i32_param() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("id *int32"),
            "optional i32 param should be *int32: {go}"
        );
        assert!(
            go.contains("var cId *C.int32_t"),
            "missing C var for optional: {go}"
        );
    }

    #[test]
    fn struct_optional_string_field() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("func (s *Contact) Email() *string"),
            "optional string getter should return *string: {go}"
        );
        assert!(
            go.contains("if cStr == nil"),
            "should check nil for optional string: {go}"
        );
    }

    #[test]
    fn no_bool_helpers_when_unneeded() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "math".into(),
                functions: vec![Function {
                    name: "add".into(),
                    params: vec![
                        Param {
                            name: "a".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "b".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            !go.contains("boolToC"),
            "should not include bool helpers: {go}"
        );
    }

    #[test]
    fn struct_enum_field_getter() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![EnumDef {
                    name: "ContactType".into(),
                    doc: None,
                    variants: vec![EnumVariant {
                        name: "Personal".into(),
                        value: 0,
                        doc: None,
                    }],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("func (s *Contact) ContactType() ContactType"),
            "missing enum field getter: {go}"
        );
    }

    #[test]
    fn generate_go_basic() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_basic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(go.contains("package weaveffi"), "missing package: {go}");
        assert!(
            go.contains("func CalculatorAdd(a int32, b int32) (int32, error)"),
            "missing add function: {go}"
        );
        assert!(
            go.contains("func CalculatorEcho(msg string) (string, error)"),
            "missing echo function: {go}"
        );

        let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
        assert!(
            go_mod.contains("module github.com/example/weaveffi"),
            "go.mod should have default module path: {go_mod}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_with_structs() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![
                        StructField {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_go_structs");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(go.contains("type Contact struct {"), "missing struct: {go}");
        assert!(
            go.contains("ptr *C.weaveffi_contacts_Contact"),
            "missing ptr field: {go}"
        );
        assert!(
            go.contains("func (s *Contact) FirstName() string"),
            "missing FirstName getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) LastName() string"),
            "missing LastName getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Age() int32"),
            "missing Age getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Close()"),
            "missing Close: {go}"
        );
        assert!(
            go.contains("(*Contact, error)"),
            "missing struct return type: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_with_enums() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "classify".into(),
                    params: vec![Param {
                        name: "ct".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Enum("ContactType".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "ContactType".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Personal".into(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Work".into(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Other".into(),
                            value: 2,
                            doc: None,
                        },
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_go_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(
            go.contains("type ContactType int32"),
            "missing enum type: {go}"
        );
        assert!(
            go.contains("ContactTypePersonal ContactType = 0"),
            "missing Personal variant: {go}"
        );
        assert!(
            go.contains("ContactTypeWork ContactType = 1"),
            "missing Work variant: {go}"
        );
        assert!(
            go.contains("ContactTypeOther ContactType = 2"),
            "missing Other variant: {go}"
        );
        assert!(
            go.contains("func ContactsClassify(ct ContactType) (ContactType, error)"),
            "missing classify function: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_error_handling() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![
                    Function {
                        name: "save".into(),
                        params: vec![Param {
                            name: "data".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "clear".into(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_go_errors");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(
            go.contains("func StoreSave(data string) (int32, error)"),
            "missing save sig: {go}"
        );
        assert!(
            go.contains("func StoreClear() error"),
            "missing void clear sig: {go}"
        );
        assert!(
            go.contains("var cErr C.weaveffi_error"),
            "missing error var: {go}"
        );
        assert!(
            go.contains("if cErr.code != 0"),
            "missing error check: {go}"
        );
        assert!(
            go.contains("C.weaveffi_error_clear(&cErr)"),
            "missing error clear: {go}"
        );
        assert!(
            go.contains("return 0, goErr"),
            "missing zero-value error return for i32: {go}"
        );
        assert!(
            go.contains("return goErr"),
            "missing void error return: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_go_full_contacts() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![
                    Function {
                        name: "create_contact".into(),
                        params: vec![
                            Param {
                                name: "first_name".into(),
                                ty: TypeRef::StringUtf8,
                                mutable: false,
                            },
                            Param {
                                name: "last_name".into(),
                                ty: TypeRef::StringUtf8,
                                mutable: false,
                            },
                            Param {
                                name: "email".into(),
                                ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                                mutable: false,
                            },
                            Param {
                                name: "contact_type".into(),
                                ty: TypeRef::Enum("ContactType".into()),
                                mutable: false,
                            },
                        ],
                        returns: Some(TypeRef::Handle),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "get_contact".into(),
                        params: vec![Param {
                            name: "id".into(),
                            ty: TypeRef::Handle,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::Struct("Contact".into())),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "list_contacts".into(),
                        params: vec![],
                        returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "delete_contact".into(),
                        params: vec![Param {
                            name: "id".into(),
                            ty: TypeRef::Handle,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::Bool),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "count_contacts".into(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![
                        StructField {
                            name: "id".into(),
                            ty: TypeRef::I64,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            doc: None,
                            default: None,
                        },
                    ],
                }],
                enums: vec![EnumDef {
                    name: "ContactType".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Personal".into(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Work".into(),
                            value: 1,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Other".into(),
                            value: 2,
                            doc: None,
                        },
                    ],
                }],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_go_full_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();

        assert!(
            go.contains("type ContactType int32"),
            "missing ContactType enum: {go}"
        );
        assert!(
            go.contains("ContactTypePersonal ContactType = 0"),
            "missing Personal: {go}"
        );
        assert!(
            go.contains("type Contact struct {"),
            "missing Contact struct: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Id() int64"),
            "missing Id getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) FirstName() string"),
            "missing FirstName getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Email() *string"),
            "missing optional Email getter: {go}"
        );
        assert!(
            go.contains("func (s *Contact) ContactType() ContactType"),
            "missing ContactType getter: {go}"
        );
        assert!(
            go.contains("func ContactsCreateContact("),
            "missing create_contact: {go}"
        );
        assert!(
            go.contains("(int64, error)"),
            "create_contact should return handle: {go}"
        );
        assert!(
            go.contains("func ContactsGetContact(id int64) (*Contact, error)"),
            "missing get_contact: {go}"
        );
        assert!(
            go.contains("func ContactsListContacts() ([]*Contact, error)"),
            "missing list_contacts: {go}"
        );
        assert!(
            go.contains("func ContactsDeleteContact(id int64) (bool, error)"),
            "missing delete_contact: {go}"
        );
        assert!(
            go.contains("func ContactsCountContacts() (int32, error)"),
            "missing count_contacts: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_custom_module_path() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_custom_mod");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        let config = GeneratorConfig {
            go_module_path: Some("github.com/myorg/mylib".into()),
            ..Default::default()
        };
        GoGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
        assert!(
            go_mod.contains("module github.com/myorg/mylib"),
            "go.mod should use custom module path: {go_mod}"
        );
        assert!(
            !go_mod.contains("module github.com/example/weaveffi"),
            "go.mod should not use default path: {go_mod}"
        );

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();
        assert!(
            go.contains("package weaveffi"),
            "Go source should still use weaveffi package: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_mod_uses_module_path() {
        // Default run must emit a real, importable module path so consumers can
        // `go get` the generated package without first editing go.mod.
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_mod_default_path");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
        assert!(
            go_mod.contains("module github.com/example/weaveffi"),
            "default go.mod should declare a real importable module path: {go_mod}"
        );

        let config = GeneratorConfig {
            go_module_path: Some("github.com/myorg/mylib".into()),
            ..Default::default()
        };
        GoGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();
        let go_mod = std::fs::read_to_string(tmp.join("go/go.mod")).unwrap();
        assert!(
            go_mod.contains("module github.com/myorg/mylib"),
            "custom go.mod must use the configured module path: {go_mod}"
        );
        assert!(
            !go_mod.contains("github.com/example/weaveffi"),
            "custom go.mod must not retain the default module path: {go_mod}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_has_doc_go() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_doc");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator.generate(&api, out_dir).unwrap();

        let doc_path = tmp.join("go/doc.go");
        assert!(doc_path.exists(), "go/doc.go should exist");
        let doc = std::fs::read_to_string(&doc_path).unwrap();
        assert!(
            doc.contains("package weaveffi"),
            "doc.go must declare the weaveffi package: {doc}"
        );
        assert!(
            doc.contains("// Package weaveffi"),
            "doc.go must carry a package-level doc comment: {doc}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_has_smoke_test() {
        let api = calculator_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_go_smoke");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator.generate(&api, out_dir).unwrap();

        let test_path = tmp.join("go/weaveffi_test.go");
        assert!(test_path.exists(), "go/weaveffi_test.go should exist");
        let test_src = std::fs::read_to_string(&test_path).unwrap();
        assert!(
            test_src.contains("package weaveffi"),
            "smoke test must live in the weaveffi package: {test_src}"
        );
        assert!(
            test_src.contains("import \"testing\""),
            "smoke test must import the testing package: {test_src}"
        );
        assert!(
            test_src.contains("func TestPackageLoads(t *testing.T)"),
            "smoke test must declare a TestPackageLoads function: {test_src}"
        );

        let go_sum = tmp.join("go/go.sum");
        assert!(go_sum.exists(), "go/go.sum placeholder should exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_cgo_respects_c_prefix() {
        let api = calculator_api();
        let config = GeneratorConfig {
            c_prefix: Some("myffi".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_go_c_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        GoGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let go = std::fs::read_to_string(tmp.join("go/weaveffi.go")).unwrap();

        assert!(
            go.contains("#cgo LDFLAGS: -lmyffi"),
            "cgo LDFLAGS must link against -lmyffi when c_prefix is set: {go}"
        );
        assert!(
            go.contains("#include \"myffi.h\""),
            "cgo preamble must include myffi.h when c_prefix is set: {go}"
        );
        assert!(
            !go.contains("-lweaveffi") && !go.contains("\"weaveffi.h\""),
            "must not retain default weaveffi library/header names when c_prefix is set: {go}"
        );

        assert!(
            go.contains("C.myffi_calculator_add("),
            "function calls must use myffi prefix: {go}"
        );
        assert!(
            go.contains("C.myffi_calculator_echo("),
            "function calls must use myffi prefix: {go}"
        );
        assert!(
            !go.contains("C.weaveffi_calculator_"),
            "function calls must not retain default weaveffi prefix: {go}"
        );

        assert!(
            go.contains("var cErr C.myffi_error"),
            "error struct type must use myffi prefix: {go}"
        );
        assert!(
            go.contains("C.myffi_error_clear(&cErr)"),
            "error_clear call must use myffi prefix: {go}"
        );
        assert!(
            go.contains("C.myffi_free_string(result)"),
            "free_string call must use myffi prefix: {go}"
        );
        assert!(
            !go.contains("C.weaveffi_error") && !go.contains("C.weaveffi_free_string"),
            "must not retain default weaveffi prefix for runtime symbols: {go}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn go_no_double_free_on_error() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                functions: vec![Function {
                    name: "find_contact".into(),
                    params: vec![Param {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let go = render_go(&api, "weaveffi");

        let fn_start = go
            .find("func ContactsFindContact(")
            .expect("ContactsFindContact wrapper");
        let fn_body = &go[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        assert!(
            !fn_text.contains("weaveffi_free_string(cName"),
            "borrowed string param must not be freed via weaveffi_free_string: {fn_text}"
        );

        let err_check = fn_text
            .find("if cErr.code != 0")
            .expect("error check in ContactsFindContact");
        let contact_wrap = fn_text
            .find("newContact(result)")
            .expect("Contact factory call in ContactsFindContact");
        assert!(
            err_check < contact_wrap,
            "error must be checked before wrapping struct return: {fn_text}"
        );

        assert!(
            go.contains("func (s *Contact) Close()")
                && go.contains("weaveffi_contacts_Contact_destroy(s.ptr)"),
            "struct return type should have Close calling destroy: {go}"
        );
    }

    #[test]
    fn go_null_check_on_optional_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "find_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                        "Contact".into(),
                    )))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let go = render_go(&api, "weaveffi");

        let fn_start = go
            .find("func ContactsFindContact(")
            .expect("ContactsFindContact wrapper");
        let fn_body = &go[fn_start..];
        let fn_end = fn_body.find("\n}\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        let null_check = fn_text
            .find("if result == nil")
            .expect("nil check in ContactsFindContact");
        let contact_wrap = fn_text
            .find("newContact(result)")
            .expect("Contact factory call in ContactsFindContact");
        assert!(
            null_check < contact_wrap,
            "optional struct return should check nil before wrapping: {fn_text}"
        );
    }

    #[test]
    fn go_string_param_uses_byteslice_pointer_and_length() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![
                    Function {
                        name: "log".into(),
                        params: vec![Param {
                            name: "msg".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        }],
                        returns: None,
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "find".into(),
                        params: vec![Param {
                            name: "query".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                        }],
                        returns: None,
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let go = render_go(&api, "weaveffi");

        let log_start = go.find("func IoLog(").expect("IoLog wrapper");
        let log_body = &go[log_start..];
        let log_text = &log_body[..log_body.find("\n}\n").unwrap()];

        assert!(
            log_text.contains("msgBytes := []byte(msg)"),
            "required string param should be converted to []byte: {log_text}"
        );
        assert!(
            log_text.contains("var cMsgPtr *C.uint8_t"),
            "required string param should declare *C.uint8_t pointer var: {log_text}"
        );
        assert!(
            log_text.contains("cMsgLen := C.size_t(len(msgBytes))"),
            "required string param should compute C.size_t length: {log_text}"
        );
        assert!(
            log_text.contains("if len(msgBytes) > 0 {"),
            "required string param should guard pointer with len > 0: {log_text}"
        );
        assert!(
            log_text.contains("cMsgPtr = (*C.uint8_t)(unsafe.Pointer(&msgBytes[0]))"),
            "required string param should compute ptr from &msgBytes[0]: {log_text}"
        );
        assert!(
            log_text.contains("C.weaveffi_io_log(cMsgPtr, cMsgLen, &cErr)"),
            "required string param should call C with (ptr, len, &cErr): {log_text}"
        );
        assert!(
            !log_text.contains("C.CString(msg)"),
            "required string param must not use C.CString: {log_text}"
        );
        assert!(
            !log_text.contains("defer C.free"),
            "required string param must not defer C.free (Go GC owns the byte slice): {log_text}"
        );

        let find_start = go.find("func IoFind(").expect("IoFind wrapper");
        let find_body = &go[find_start..];
        let find_text = &find_body[..find_body.find("\n}\n").unwrap()];

        assert!(
            find_text.contains("var queryBytes []byte"),
            "optional string param should declare empty byte slice: {find_text}"
        );
        assert!(
            find_text.contains("var cQueryPtr *C.uint8_t"),
            "optional string param should declare *C.uint8_t pointer var: {find_text}"
        );
        assert!(
            find_text.contains("var cQueryLen C.size_t"),
            "optional string param should declare C.size_t length var: {find_text}"
        );
        assert!(
            find_text.contains("if query != nil {"),
            "optional string param should guard on query != nil: {find_text}"
        );
        assert!(
            find_text.contains("queryBytes = []byte(*query)"),
            "optional string param should encode dereferenced *string: {find_text}"
        );
        assert!(
            find_text.contains("cQueryPtr = (*C.uint8_t)(unsafe.Pointer(&queryBytes[0]))"),
            "optional string param should compute ptr from &queryBytes[0]: {find_text}"
        );
        assert!(
            find_text.contains("C.weaveffi_io_find(cQueryPtr, cQueryLen, &cErr)"),
            "optional string param should call C with (ptr, len, &cErr): {find_text}"
        );
        assert!(
            !find_text.contains("C.CString(*query)"),
            "optional string param must not use C.CString: {find_text}"
        );
    }

    #[test]
    fn go_bytes_param_uses_canonical_shape() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "send".into(),
                    params: vec![Param {
                        name: "payload".into(),
                        ty: TypeRef::Bytes,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("var cPayloadPtr *C.uint8_t"),
            "Go wrapper must declare *C.uint8_t for Bytes param ptr: {go}"
        );
        assert!(
            go.contains("cPayloadLen := C.size_t(len(payload))"),
            "Go wrapper must capture payload length as C.size_t: {go}"
        );
        assert!(
            go.contains("cPayloadPtr = (*C.uint8_t)(unsafe.Pointer(&payload[0]))"),
            "Go wrapper must compute ptr from &payload[0]: {go}"
        );
        assert!(
            go.contains("C.weaveffi_io_send(cPayloadPtr, cPayloadLen, &cErr)"),
            "Go wrapper must call C with (ptr, len, &cErr) for Bytes param: {go}"
        );
    }

    #[test]
    fn go_bytes_return_uses_canonical_shape() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "read".into(),
                    params: vec![],
                    returns: Some(TypeRef::Bytes),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");
        assert!(
            go.contains("var cOutLen C.size_t"),
            "Go wrapper must declare cOutLen out-param for Bytes return: {go}"
        );
        assert!(
            go.contains("result := C.weaveffi_io_read(&cOutLen, &cErr)"),
            "Go wrapper must call C with (&cOutLen, &cErr) for Bytes return: {go}"
        );
        assert!(
            go.contains("C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))"),
            "Go wrapper must copy bytes via C.GoBytes(result, cOutLen): {go}"
        );
        assert!(
            go.contains("C.weaveffi_free_bytes(result, cOutLen)"),
            "Go wrapper must free returned bytes via C.weaveffi_free_bytes(result, cOutLen): {go}"
        );
    }

    #[test]
    fn go_wrapper_calls_weaveffi_error_clear_after_capturing_message() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "math".into(),
                functions: vec![Function {
                    name: "add".into(),
                    params: vec![
                        Param {
                            name: "a".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "b".into(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let go = render_go(&api, "weaveffi");
        let msg_pos = go
            .find("goErr := fmt.Errorf(\"weaveffi: %s (code %d)\", C.GoString(cErr.message), int(cErr.code))")
            .expect("Go wrapper must build a Go error from cErr.message before clearing");
        let clear_pos = go
            .find("C.weaveffi_error_clear(&cErr)")
            .expect("Go wrapper must call C.weaveffi_error_clear after capturing the message");
        let return_pos = go[clear_pos..]
            .find("return")
            .map(|p| p + clear_pos)
            .expect("Go wrapper must return the goErr after clearing");
        assert!(
            msg_pos < clear_pos,
            "C.weaveffi_error_clear must run AFTER capturing cErr.message: {go}"
        );
        assert!(
            clear_pos < return_pos,
            "C.weaveffi_error_clear must run BEFORE returning the goErr: {go}"
        );
    }

    #[test]
    fn go_bytes_return_calls_free_bytes() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "parity".into(),
                functions: vec![Function {
                    name: "echo".into(),
                    params: vec![Param {
                        name: "b".into(),
                        ty: TypeRef::Bytes,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Bytes),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");

        let copy_pos = go
            .find("goResult := C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))")
            .expect("Go wrapper must copy bytes into a Go []byte via C.GoBytes");
        let free_pos = go
            .find("C.weaveffi_free_bytes(result, cOutLen)")
            .expect("Go wrapper must free the returned pointer via C.weaveffi_free_bytes");
        assert!(
            copy_pos < free_pos,
            "C.weaveffi_free_bytes must run AFTER C.GoBytes has copied the payload: {go}"
        );
    }

    #[test]
    fn go_struct_wrapper_calls_destroy() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");

        assert!(
            go.contains("\"runtime\""),
            "Go output must import runtime for SetFinalizer: {go}"
        );
        assert!(
            go.contains("func newContact(ptr *C.weaveffi_contacts_Contact) *Contact"),
            "Go output must define newContact factory: {go}"
        );
        assert!(
            go.contains("runtime.SetFinalizer(s, func(x *Contact) { x.Close() })"),
            "newContact must register a finalizer: {go}"
        );
        assert!(
            go.contains("func (s *Contact) Close()"),
            "Contact must have a Close method: {go}"
        );
        assert!(
            go.contains("C.weaveffi_contacts_Contact_destroy(s.ptr)"),
            "Close must call the C destroy function: {go}"
        );
        assert!(
            go.contains("runtime.SetFinalizer(s, nil)"),
            "Close must clear the finalizer after destroying: {go}"
        );
        assert!(
            go.contains("s.ptr = nil"),
            "Close must null out the pointer: {go}"
        );
    }

    #[test]
    fn go_struct_setter_string_uses_byteslice_pointer_and_length() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "set_contact_name".into(),
                    params: vec![
                        Param {
                            name: "contact".into(),
                            ty: TypeRef::TypedHandle("Contact".into()),
                            mutable: false,
                        },
                        Param {
                            name: "new_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                    ],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let go = render_go(&api, "weaveffi");

        let fn_start = go
            .find("func ContactsSetContactName(")
            .expect("ContactsSetContactName wrapper");
        let fn_body = &go[fn_start..];
        let fn_text = &fn_body[..fn_body.find("\n}\n").unwrap()];

        assert!(
            fn_text.contains("newNameBytes := []byte(newName)"),
            "struct setter should convert string param to []byte: {fn_text}"
        );
        assert!(
            fn_text.contains("var cNewNamePtr *C.uint8_t"),
            "struct setter should declare *C.uint8_t pointer var: {fn_text}"
        );
        assert!(
            fn_text.contains("cNewNameLen := C.size_t(len(newNameBytes))"),
            "struct setter should compute C.size_t length: {fn_text}"
        );
        assert!(
            fn_text.contains("if len(newNameBytes) > 0 {"),
            "struct setter should guard pointer with len > 0: {fn_text}"
        );
        assert!(
            fn_text.contains("cNewNamePtr = (*C.uint8_t)(unsafe.Pointer(&newNameBytes[0]))"),
            "struct setter should compute ptr from &newNameBytes[0]: {fn_text}"
        );
        assert!(
            fn_text.contains(
                "C.weaveffi_contacts_set_contact_name(contact.ptr, cNewNamePtr, cNewNameLen, &cErr)"
            ),
            "struct setter should call C with (handle.ptr, ptr, len, &cErr): {fn_text}"
        );
        assert!(
            !fn_text.contains("C.CString(newName)"),
            "struct setter must not use C.CString: {fn_text}"
        );
        assert!(
            !fn_text.contains("defer C.free"),
            "struct setter must not defer C.free for the string param: {fn_text}"
        );
    }

    #[test]
    fn go_builder_setter_string_uses_byteslice_pointer_and_length() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "Contact_Builder_set_name".into(),
                    params: vec![
                        Param {
                            name: "builder".into(),
                            ty: TypeRef::Handle,
                            mutable: true,
                        },
                        Param {
                            name: "value".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                    ],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: true,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let go = render_go(&api, "weaveffi");

        let fn_start = go
            .find("func ContactsContactBuilderSetName(")
            .expect("ContactsContactBuilderSetName wrapper");
        let fn_body = &go[fn_start..];
        let fn_text = &fn_body[..fn_body.find("\n}\n").unwrap()];

        assert!(
            fn_text.contains("valueBytes := []byte(value)"),
            "builder setter should convert string param to []byte: {fn_text}"
        );
        assert!(
            fn_text.contains("var cValuePtr *C.uint8_t"),
            "builder setter should declare *C.uint8_t pointer var: {fn_text}"
        );
        assert!(
            fn_text.contains("cValueLen := C.size_t(len(valueBytes))"),
            "builder setter should compute C.size_t length: {fn_text}"
        );
        assert!(
            fn_text.contains("if len(valueBytes) > 0 {"),
            "builder setter should guard pointer with len > 0: {fn_text}"
        );
        assert!(
            fn_text.contains("cValuePtr = (*C.uint8_t)(unsafe.Pointer(&valueBytes[0]))"),
            "builder setter should compute ptr from &valueBytes[0]: {fn_text}"
        );
        assert!(
            fn_text.contains(
                "C.weaveffi_contacts_Contact_Builder_set_name(C.weaveffi_handle_t(builder), cValuePtr, cValueLen, &cErr)"
            ),
            "builder setter should call C with (handle, ptr, len, &cErr): {fn_text}"
        );
        assert!(
            !fn_text.contains("C.CString(value)"),
            "builder setter must not use C.CString: {fn_text}"
        );
        assert!(
            !fn_text.contains("defer C.free"),
            "builder setter must not defer C.free for the string param: {fn_text}"
        );
    }

    #[test]
    fn go_builder_build_calls_c_builder_build() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "geo".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Point".into(),
                    doc: None,
                    builder: true,
                    fields: vec![
                        StructField {
                            name: "x".into(),
                            ty: TypeRef::F64,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "y".into(),
                            ty: TypeRef::F64,
                            doc: None,
                            default: None,
                        },
                    ],
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");

        assert!(
            go.contains("type PointBuilder struct {\n\thandle *C.weaveffi_geo_PointBuilder\n}"),
            "builder struct must hold a typed C handle: {go}"
        );
        assert!(
            go.contains(
                "func NewPointBuilder() *PointBuilder {\n\treturn &PointBuilder{handle: C.weaveffi_geo_Point_Builder_new()}\n}"
            ),
            "constructor must call C Builder_new: {go}"
        );
        assert!(
            go.contains("C.weaveffi_geo_Point_Builder_set_x(b.handle, C.double(value))"),
            "WithX must call native Builder_set_x with (b.handle, C.double(value)): {go}"
        );
        assert!(
            go.contains("C.weaveffi_geo_Point_Builder_set_y(b.handle, C.double(value))"),
            "WithY must call native Builder_set_y with (b.handle, C.double(value)): {go}"
        );

        let build_start = go
            .find("func (b *PointBuilder) Build() (*Point, error) {")
            .expect("Build must return (*Point, error)");
        let build_body = &go[build_start..];
        let build_end = build_body.find("\n}\n").unwrap();
        let build_text = &build_body[..build_end];

        assert!(
            build_text.contains("var cErr C.weaveffi_error"),
            "Build must declare cErr: {build_text}"
        );
        assert!(
            build_text.contains("result := C.weaveffi_geo_Point_Builder_build(b.handle, &cErr)"),
            "Build must call C Builder_build with (b.handle, &cErr): {build_text}"
        );
        assert!(
            build_text.contains("C.weaveffi_geo_Point_Builder_destroy(b.handle)"),
            "Build must destroy the builder: {build_text}"
        );
        assert!(
            build_text.contains("b.handle = nil"),
            "Build must nil the handle after destroy: {build_text}"
        );
        assert!(
            build_text.contains("if cErr.code != 0 {"),
            "Build must check cErr.code: {build_text}"
        );
        assert!(
            build_text.contains("C.weaveffi_error_clear(&cErr)"),
            "Build must clear cErr after capturing: {build_text}"
        );
        assert!(
            build_text.contains("return nil, goErr"),
            "Build must return (nil, err) on error: {build_text}"
        );
        assert!(
            build_text.contains("return newPoint(result), nil"),
            "Build must wrap result with newPoint on success: {build_text}"
        );

        let close_start = go
            .find("func (b *PointBuilder) Close() {")
            .expect("Close() cleanup method must be present");
        let close_body = &go[close_start..];
        let close_text = &close_body[..close_body.find("\n}\n").unwrap()];
        assert!(
            close_text.contains("if b.handle != nil {"),
            "Close must guard on b.handle != nil: {close_text}"
        );
        assert!(
            close_text.contains("C.weaveffi_geo_Point_Builder_destroy(b.handle)"),
            "Close must call C Builder_destroy: {close_text}"
        );
        assert!(
            close_text.contains("b.handle = nil"),
            "Close must nil the handle: {close_text}"
        );
    }

    #[test]
    fn capabilities_includes_callbacks_listeners_iterators_excludes_builders_async() {
        let caps = GoGenerator.capabilities();
        assert!(
            caps.contains(&Capability::Callbacks),
            "Go generator must advertise Callbacks now that callback codegen is implemented"
        );
        assert!(
            caps.contains(&Capability::Listeners),
            "Go generator must advertise Listeners now that listener codegen is implemented"
        );
        assert!(
            caps.contains(&Capability::Iterators),
            "Go generator must advertise Iterators now that iterator codegen is implemented"
        );
        assert!(
            !caps.contains(&Capability::AsyncFunctions),
            "Go generator must not advertise AsyncFunctions until async codegen is implemented"
        );
        assert!(
            !caps.contains(&Capability::Builders),
            "Go generator must not advertise Builders until builder codegen is implemented"
        );
        for cap in Capability::ALL {
            if matches!(cap, Capability::AsyncFunctions | Capability::Builders) {
                continue;
            }
            assert!(caps.contains(cap), "Go generator must support {cap:?}");
        }
    }

    #[test]
    fn go_iterator_return_uses_channel() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "data".into(),
                functions: vec![Function {
                    name: "list_items".into(),
                    params: vec![],
                    returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");

        assert!(
            go.contains("func DataListItems() (<-chan int32, error)"),
            "iterator function must return <-chan T: {go}"
        );
        assert!(
            go.contains("ch := make(chan int32)"),
            "iterator must allocate a Go channel: {go}"
        );
        assert!(
            go.contains("go func() {"),
            "iterator must spawn a goroutine: {go}"
        );
        assert!(
            go.contains("defer close(ch)"),
            "goroutine must close the channel on done: {go}"
        );
        assert!(
            go.contains("defer C.weaveffi_data_ListItemsIterator_destroy(result)"),
            "goroutine must destroy the native iterator: {go}"
        );
        assert!(
            go.contains("C.weaveffi_data_ListItemsIterator_next(result, &outItem, &itemErr)"),
            "goroutine must call _next in the loop: {go}"
        );
        assert!(
            go.contains("if has == 0 {"),
            "goroutine must break when _next returns 0: {go}"
        );
        assert!(
            go.contains("ch <- int32(outItem)"),
            "goroutine must send converted items to the channel: {go}"
        );
        assert!(
            go.contains("return ch, nil"),
            "wrapper must return the channel and nil error on success: {go}"
        );
    }

    #[test]
    fn go_emits_callback_func_type_and_registry() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "events".into(),
                functions: vec![Function {
                    name: "subscribe".into(),
                    params: vec![Param {
                        name: "handler".into(),
                        ty: TypeRef::Callback("OnData".into()),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "OnData".into(),
                    params: vec![Param {
                        name: "payload".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");

        assert!(
            go.contains("type OnData func(payload string)"),
            "missing callback Go func type: {go}"
        );
        assert!(
            go.contains("var _callbackRegistry sync.Map"),
            "missing sync.Map registry: {go}"
        );
        assert!(
            go.contains("var _callbackToken int64"),
            "missing int64 token counter: {go}"
        );
        assert!(
            go.contains("func _callbackRegister(cb interface{}) int64 {"),
            "missing _callbackRegister helper: {go}"
        );
        assert!(
            go.contains("atomic.AddInt64(&_callbackToken, 1)"),
            "registry should use atomic.AddInt64 for tokens: {go}"
        );
        assert!(
            go.contains("_callbackRegistry.Store(token, cb)"),
            "registry should store the user callback: {go}"
        );
        assert!(go.contains("\"sync\""), "Go output must import sync: {go}");
        assert!(
            go.contains("\"sync/atomic\""),
            "Go output must import sync/atomic: {go}"
        );
        assert!(
            go.contains("//export weaveffiGoCbTrampoline_events_OnData"),
            "missing //export trampoline directive: {go}"
        );
        assert!(
            go.contains("func weaveffiGoCbTrampoline_events_OnData(ctx unsafe.Pointer,"),
            "trampoline should accept ctx unsafe.Pointer: {go}"
        );
        assert!(
            go.contains("extern void weaveffiGoCbTrampoline_events_OnData("),
            "cgo preamble must declare the trampoline as extern: {go}"
        );
        assert!(
            go.contains("_callbackRegistry.Load(int64(uintptr(ctx)))"),
            "trampoline should decode token from context: {go}"
        );
        assert!(
            go.contains("cHandlerToken := _callbackRegister(handler)"),
            "wrapper should register the callback: {go}"
        );
        assert!(
            go.contains(
                "cHandlerFn := (C.weaveffi_events_OnData)(unsafe.Pointer(C.weaveffiGoCbTrampoline_events_OnData))"
            ),
            "wrapper should cast trampoline to the C callback type: {go}"
        );
        assert!(
            go.contains("cHandlerCtx := unsafe.Pointer(uintptr(cHandlerToken))"),
            "wrapper should pass token as the context void*: {go}"
        );
        assert!(
            go.contains("C.weaveffi_events_subscribe(cHandlerFn, cHandlerCtx, &cErr)"),
            "C call should receive trampoline fn and token context: {go}"
        );
    }

    #[test]
    fn go_emits_listener_type() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "events".into(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "OnData".into(),
                    params: vec![Param {
                        name: "value".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![ListenerDef {
                    name: "data_stream".into(),
                    event_callback: "OnData".into(),
                    doc: None,
                }],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let go = render_go(&api, "weaveffi");

        assert!(
            go.contains("type DataStream struct{}"),
            "missing listener struct type: {go}"
        );
        assert!(
            go.contains("var _dataStreamPins sync.Map"),
            "listener must declare a per-listener sync.Map for pinning: {go}"
        );
        assert!(
            go.contains("func (DataStream) Register(cb OnData) uint64 {"),
            "listener must expose Register(cb CallbackType) uint64: {go}"
        );
        assert!(
            go.contains("token := _callbackRegister(cb)"),
            "Register must register the callback to get a token: {go}"
        );
        assert!(
            go.contains("fn := (C.weaveffi_events_OnData)(unsafe.Pointer(C.weaveffiGoCbTrampoline_events_OnData))"),
            "Register must cast the trampoline to the C callback type: {go}"
        );
        assert!(
            go.contains("id := uint64(C.weaveffi_events_register_data_stream(fn, ctx))"),
            "Register must call the native register symbol with (fn, ctx): {go}"
        );
        assert!(
            go.contains("_dataStreamPins.Store(id, token)"),
            "Register must pin the token under id in the listener sync.Map: {go}"
        );
        assert!(
            go.contains("func (DataStream) Unregister(id uint64) {"),
            "listener must expose Unregister(id uint64): {go}"
        );
        assert!(
            go.contains("C.weaveffi_events_unregister_data_stream(C.uint64_t(id))"),
            "Unregister must call the native unregister symbol with the id: {go}"
        );
        assert!(
            go.contains("_dataStreamPins.LoadAndDelete(id)"),
            "Unregister must remove the pin from the listener sync.Map: {go}"
        );
        assert!(
            go.contains("_callbackRegistry.Delete(token)"),
            "Unregister must drop the token from the shared callback registry: {go}"
        );
    }

    #[test]
    fn go_outputs_have_version_stamp() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_go_stamp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).unwrap();

        GoGenerator.generate(&api, out_dir).unwrap();

        for rel in ["go/weaveffi.go", "go/go.mod"] {
            let contents = std::fs::read_to_string(tmp.join(rel)).unwrap();
            assert!(
                contents.starts_with("// WeaveFFI "),
                "{rel} missing stamp: {contents}"
            );
            assert!(
                contents.contains(" go "),
                "{rel} stamp missing generator name"
            );
            assert!(
                contents.contains("DO NOT EDIT"),
                "{rel} missing DO NOT EDIT"
            );
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
