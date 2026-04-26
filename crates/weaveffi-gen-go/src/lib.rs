use anyhow::Result;
use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_symbol_name, local_type_name};
use weaveffi_ir::ir::{Api, EnumDef, Function, Module, StructDef, StructField, TypeRef};

pub struct GoGenerator;

impl GoGenerator {
    fn generate_impl(&self, api: &Api, out_dir: &Utf8Path, module_path: &str) -> Result<()> {
        let dir = out_dir.join("go");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("weaveffi.go"), render_go(api))?;
        std::fs::write(dir.join("go.mod"), render_go_mod(module_path))?;
        std::fs::write(dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for GoGenerator {
    fn name(&self) -> &'static str {
        "go"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi")
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(api, out_dir, config.go_module_path())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("go/weaveffi.go").to_string(),
            out_dir.join("go/go.mod").to_string(),
            out_dir.join("go/README.md").to_string(),
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
        TypeRef::List(inner) | TypeRef::Iterator(inner) => format!("[]{}", go_type(inner)),
        TypeRef::Map(k, v) => format!("map[{}]{}", go_type(k), go_type(v)),
        TypeRef::Callback(_) => "interface{}".into(),
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

fn c_scalar_type(ty: &TypeRef, module: &str) -> Option<String> {
    match ty {
        TypeRef::I32 => Some("C.int32_t".into()),
        TypeRef::U32 => Some("C.uint32_t".into()),
        TypeRef::I64 | TypeRef::Handle => Some("C.int64_t".into()),
        TypeRef::F64 => Some("C.double".into()),
        TypeRef::Bool => Some("C._Bool".into()),
        TypeRef::Enum(n) => Some(format!("C.weaveffi_{module}_{n}")),
        _ => None,
    }
}

fn c_scalar_conv(expr: &str, ty: &TypeRef, module: &str) -> String {
    match ty {
        TypeRef::Bool => format!("boolToC({expr})"),
        _ => {
            if let Some(ct) = c_scalar_type(ty, module) {
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

fn c_opaque_type(ty: &TypeRef, module: &str) -> String {
    match ty {
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => format!("weaveffi_{module}_{n}"),
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
        TypeRef::Optional(inner) | TypeRef::List(inner) => type_has_bool(inner),
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

fn scan_imports(api: &Api) -> (bool, bool, bool) {
    let has_sync_funcs = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| !f.r#async));

    let needs_fmt = has_sync_funcs;

    let needs_unsafe = collect_all_modules(&api.modules).iter().any(|m| {
        m.functions.iter().filter(|f| !f.r#async).any(|f| {
            f.params.iter().any(|p| param_uses_unsafe(&p.ty))
                || f.returns.as_ref().is_some_and(return_uses_unsafe)
        })
    });

    let needs_bool = collect_all_modules(&api.modules).iter().any(|m| {
        m.functions.iter().filter(|f| !f.r#async).any(|f| {
            f.params.iter().any(|p| type_has_bool(&p.ty))
                || f.returns.as_ref().is_some_and(type_has_bool)
        }) || m
            .structs
            .iter()
            .any(|s| s.fields.iter().any(|fld| type_has_bool(&fld.ty)))
    });

    (needs_fmt, needs_unsafe, needs_bool)
}

// ── Packaging scaffold ──

fn render_go_mod(module_path: &str) -> String {
    format!("module {module_path}\n\ngo 1.21\n")
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

// ── Top-level rendering ──

fn render_go(api: &Api) -> String {
    let (needs_fmt, needs_unsafe, needs_bool) = scan_imports(api);
    let mut out = String::new();

    out.push_str("package weaveffi\n\n");

    out.push_str("/*\n");
    out.push_str("#cgo LDFLAGS: -lweaveffi\n");
    out.push_str("#include \"weaveffi.h\"\n");
    out.push_str("#include <stdlib.h>\n");
    out.push_str("*/\n");
    out.push_str("import \"C\"\n");

    if needs_fmt || needs_unsafe {
        out.push_str("\nimport (\n");
        if needs_fmt {
            out.push_str("\t\"fmt\"\n");
        }
        if needs_unsafe {
            out.push_str("\t\"unsafe\"\n");
        }
        out.push_str(")\n");
    }
    out.push('\n');

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
        for s in &m.structs {
            render_struct(&mut out, &path, s);
            if s.builder {
                render_go_builder(&mut out, s);
            }
        }
        for f in &m.functions {
            if !f.r#async {
                render_function(&mut out, &path, f);
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

fn render_struct(out: &mut String, module: &str, s: &StructDef) {
    let name = s.name.to_upper_camel_case();
    let c_tag = format!("weaveffi_{}_{}", module, s.name);

    out.push_str(&format!("type {name} struct {{\n"));
    out.push_str(&format!("\tptr *C.{c_tag}\n"));
    out.push_str("}\n\n");

    for field in &s.fields {
        render_getter(out, module, &name, &c_tag, field);
    }

    out.push_str(&format!("func (s *{name}) Close() {{\n"));
    out.push_str("\tif s.ptr != nil {\n");
    out.push_str(&format!("\t\tC.{c_tag}_destroy(s.ptr)\n"));
    out.push_str("\t\ts.ptr = nil\n");
    out.push_str("\t}\n");
    out.push_str("}\n\n");
}

fn render_go_builder(out: &mut String, s: &StructDef) {
    let name = s.name.to_upper_camel_case();
    out.push_str(&format!("type {name}Builder struct {{\n"));
    out.push_str("\tfields map[string]interface{}\n");
    out.push_str("}\n\n");
    out.push_str(&format!("func New{name}Builder() *{name}Builder {{\n"));
    out.push_str(&format!(
        "\treturn &{name}Builder{{fields: make(map[string]interface{{}})}}\n"
    ));
    out.push_str("}\n\n");

    for field in &s.fields {
        let method = field.name.to_upper_camel_case();
        let gt = go_type(&field.ty);
        out.push_str(&format!(
            "func (b *{name}Builder) With{method}(value {gt}) *{name}Builder {{\n"
        ));
        out.push_str(&format!("\tb.fields[\"{}\"] = value\n", field.name));
        out.push_str("\treturn b\n");
        out.push_str("}\n\n");
    }
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
            out.push_str(&format!("\treturn &{inner}{{ptr: {getter}(s.ptr)}}\n"));
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
                out.push_str(&format!("\treturn &{inner_go}{{ptr: cPtr}}\n"));
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

fn render_function(out: &mut String, module: &str, f: &Function) {
    let c_sym = c_symbol_name(module, &f.name);
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
        );
    }

    if let Some(ref ret) = f.returns {
        emit_return_out_params(&mut pre, &mut c_args, ret);
    }

    pre.push_str("\tvar cErr C.weaveffi_error\n");
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
    out.push_str("\t\tC.weaveffi_error_clear(&cErr)\n");
    if let Some(ref ret) = f.returns {
        out.push_str(&format!("\t\treturn {}, goErr\n", go_zero(ret)));
    } else {
        out.push_str("\t\treturn goErr\n");
    }
    out.push_str("\t}\n");

    if let Some(ref ret) = f.returns {
        emit_return(out, ret, module);
    } else {
        out.push_str("\treturn nil\n");
    }

    out.push_str("}\n\n");
}

// ── Parameter conversion ──

fn emit_param(pre: &mut String, args: &mut Vec<String>, name: &str, ty: &TypeRef, module: &str) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 => {
            args.push(c_scalar_conv(name, ty, module));
        }
        TypeRef::Bool => args.push(format!("boolToC({name})")),
        TypeRef::Handle => args.push(format!("C.weaveffi_handle_t({name})")),
        TypeRef::Enum(n) => args.push(format!("C.weaveffi_{module}_{n}({name})")),
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

        TypeRef::Optional(inner) => emit_optional_param(pre, args, name, inner, module),
        TypeRef::List(inner) => emit_list_param(pre, args, name, inner, module),
        TypeRef::Map(k, v) => emit_map_param(pre, args, name, k, v, module),

        TypeRef::Callback(_) => {
            args.push("nil".into());
            args.push("nil".into());
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
            let ct = c_opaque_type(inner, module);
            pre.push_str(&format!("\tvar {cv} *C.{ct}\n"));
            pre.push_str(&format!("\tif {name} != nil {{\n"));
            pre.push_str(&format!("\t\t{cv} = {name}.ptr\n"));
            pre.push_str("\t}\n");
            args.push(cv);
        }
        _ => {
            if let Some(ct) = c_scalar_type(inner, module) {
                pre.push_str(&format!("\tvar {cv} *{ct}\n"));
                pre.push_str(&format!("\tif {name} != nil {{\n"));
                let conv = c_scalar_conv(&format!("*{name}"), inner, module);
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
) {
    let cn = name.to_upper_camel_case();
    let pv = format!("c{cn}Ptr");
    let lv = format!("c{cn}Len");

    pre.push_str(&format!("\t{lv} := C.size_t(len({name}))\n"));

    if let Some(ct) = c_scalar_type(inner, module) {
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
        let ct = format!("C.weaveffi_{module}_{n}");
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
    emit_map_array(pre, &kp, &format!("keys{cn}"), k, module);
    args.push(kp);

    let vp = format!("c{cn}ValsPtr");
    emit_map_array(pre, &vp, &format!("vals{cn}"), v, module);
    args.push(vp);

    args.push(lv);
}

fn emit_map_array(pre: &mut String, ptr_var: &str, slice_name: &str, ty: &TypeRef, module: &str) {
    if let Some(ct) = c_scalar_type(ty, module) {
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
        TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            pre.push_str("\tvar cOutLen C.size_t\n");
            args.push("&cOutLen".into());
        }
        TypeRef::Optional(inner) => emit_return_out_params(pre, args, inner),
        _ => {}
    }
}

// ── Return conversion ──

fn emit_return(out: &mut String, ty: &TypeRef, module: &str) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle | TypeRef::F64 => {
            let conv = go_scalar_conv("result", ty);
            out.push_str(&format!("\treturn {conv}, nil\n"));
        }
        TypeRef::Bool => out.push_str("\treturn cToBool(result), nil\n"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("\tgoResult := C.GoString(result)\n");
            out.push_str("\tC.weaveffi_free_string(result)\n");
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
            out.push_str(&format!("\treturn &{g}{{ptr: result}}, nil\n"));
        }
        TypeRef::Optional(inner) => emit_optional_return(out, inner, module),
        TypeRef::List(inner) => emit_list_return(out, inner, module),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("\tif result == nil {\n\t\treturn nil, nil\n\t}\n");
            out.push_str("\tgoResult := C.GoBytes(unsafe.Pointer(result), C.int(cOutLen))\n");
            out.push_str("\tC.weaveffi_free_bytes(result, cOutLen)\n");
            out.push_str("\treturn goResult, nil\n");
        }
        TypeRef::Map(k, v) => emit_map_return(out, k, v),
        TypeRef::Callback(_) => out.push_str("\treturn nil, nil\n"),
        TypeRef::Iterator(inner) => emit_list_return(out, inner, module),
    }
}

fn emit_optional_return(out: &mut String, inner: &TypeRef, _module: &str) {
    out.push_str("\tif result == nil {\n\t\treturn nil, nil\n\t}\n");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("\tv := C.GoString(result)\n");
            out.push_str("\tC.weaveffi_free_string(result)\n");
            out.push_str("\treturn &v, nil\n");
        }
        TypeRef::TypedHandle(n) => {
            let g = n.to_upper_camel_case();
            out.push_str(&format!("\treturn &{g}{{ptr: result}}, nil\n"));
        }
        TypeRef::Struct(n) => {
            let g = local_type_name(n).to_upper_camel_case();
            out.push_str(&format!("\treturn &{g}{{ptr: result}}, nil\n"));
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

fn emit_list_return(out: &mut String, inner: &TypeRef, module: &str) {
    out.push_str("\tcount := int(cOutLen)\n");
    out.push_str("\tif count == 0 || result == nil {\n\t\treturn nil, nil\n\t}\n");

    let gi = go_type(inner);
    out.push_str(&format!("\tgoResult := make([]{gi}, count)\n"));

    if let Some(ct) = c_scalar_type(inner, module) {
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
        let ct = format!("C.weaveffi_{module}_{n}");
        let gs = n.to_upper_camel_case();
        out.push_str(&format!(
            "\tcSlice := unsafe.Slice((**{ct})(unsafe.Pointer(result)), count)\n"
        ));
        out.push_str("\tfor i, v := range cSlice {\n");
        out.push_str(&format!("\t\tgoResult[i] = &{gs}{{ptr: v}}\n"));
        out.push_str("\t}\n");
    } else if let TypeRef::Struct(n) = inner {
        let ct = format!("C.weaveffi_{module}_{n}");
        let gs = local_type_name(n).to_upper_camel_case();
        out.push_str(&format!(
            "\tcSlice := unsafe.Slice((**{ct})(unsafe.Pointer(result)), count)\n"
        ));
        out.push_str("\tfor i, v := range cSlice {\n");
        out.push_str(&format!("\t\tgoResult[i] = &{gs}{{ptr: v}}\n"));
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
                out.join("go/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn package_and_cgo_preamble() {
        let go = render_go(&calculator_api());
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
        let go = render_go(&calculator_api());
        assert!(go.contains("\"fmt\""), "missing fmt import: {go}");
        assert!(go.contains("\"unsafe\""), "missing unsafe import: {go}");
    }

    #[test]
    fn simple_i32_function() {
        let go = render_go(&calculator_api());
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
        let go = render_go(&calculator_api());
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
        let go = render_go(&calculator_api());
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
        assert!(
            go.contains("type PointBuilder struct {"),
            "builder type: {go}"
        );
        assert!(
            go.contains("fields map[string]interface{}"),
            "fields map: {go}"
        );
        assert!(
            go.contains("func NewPointBuilder() *PointBuilder"),
            "constructor: {go}"
        );
        assert!(
            go.contains("func (b *PointBuilder) WithX(value float64) *PointBuilder"),
            "WithX: {go}"
        );
        assert!(go.contains("b.fields[\"x\"] = value"), "field assign: {go}");
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
        assert!(
            go.contains("(*Contact, error)"),
            "missing struct return type: {go}"
        );
        assert!(
            go.contains("&Contact{ptr: result}"),
            "missing struct wrap: {go}"
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
    fn async_functions_skipped() {
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
        let go = render_go(&api);
        assert!(
            !go.contains("func TasksRun("),
            "async functions should be skipped: {go}"
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
            go_mod.contains("module weaveffi"),
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
        let go = render_go(&api);
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
            go_mod.contains("module weaveffi"),
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
            !go_mod.contains("module weaveffi"),
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

        let go = render_go(&api);

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
            .find("&Contact{ptr: result}")
            .expect("Contact wrap in ContactsFindContact");
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

        let go = render_go(&api);

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
            .find("&Contact{ptr: result}")
            .expect("Contact wrap in ContactsFindContact");
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

        let go = render_go(&api);

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
        let go = render_go(&api);
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
        let go = render_go(&api);
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

        let go = render_go(&api);
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
}
