//! .NET (P/Invoke) binding generator for WeaveFFI.
//!
//! Emits a C# project (`.csproj` + `.nuspec`) with P/Invoke declarations
//! and idiomatic wrappers over the C ABI. Async functions surface as
//! `Task<T>`-returning methods. Implements the [`Generator`] trait.

use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_symbol_name, local_type_name, wrapper_name};
use weaveffi_ir::ir::{Api, EnumDef, Function, Module, Param, StructDef, StructField, TypeRef};

pub struct DotnetGenerator;

impl DotnetGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        namespace: &str,
        strip_module_prefix: bool,
    ) -> Result<()> {
        let dir = out_dir.join("dotnet");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join(format!("{namespace}.cs")),
            render_csharp(api, namespace, strip_module_prefix),
        )?;
        std::fs::write(
            dir.join(format!("{namespace}.csproj")),
            render_csproj(namespace),
        )?;
        std::fs::write(
            dir.join(format!("{namespace}.nuspec")),
            render_nuspec(namespace),
        )?;
        std::fs::write(dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for DotnetGenerator {
    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "WeaveFFI", true)
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(
            api,
            out_dir,
            config.dotnet_namespace(),
            config.strip_module_prefix,
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("dotnet/WeaveFFI.cs").to_string(),
            out_dir.join("dotnet/WeaveFFI.csproj").to_string(),
            out_dir.join("dotnet/WeaveFFI.nuspec").to_string(),
            out_dir.join("dotnet/README.md").to_string(),
        ]
    }
}

fn cs_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "int".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Handle => "ulong".into(),
        TypeRef::TypedHandle(name) => name.clone(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "byte[]".into(),
        TypeRef::Struct(name) => local_type_name(name).into(),
        TypeRef::Enum(name) => name.clone(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::I32 => "int?".into(),
            TypeRef::U32 => "uint?".into(),
            TypeRef::I64 => "long?".into(),
            TypeRef::F64 => "double?".into(),
            TypeRef::Bool => "bool?".into(),
            TypeRef::Handle => "ulong?".into(),
            TypeRef::TypedHandle(name) => format!("{name}?"),
            TypeRef::Enum(name) => format!("{name}?"),
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string?".into(),
            TypeRef::Struct(name) => format!("{}?", local_type_name(name)),
            _ => format!("{}?", cs_type(inner)),
        },
        TypeRef::List(inner) => format!("{}[]", cs_type(inner)),
        TypeRef::Iterator(inner) => format!("IEnumerable<{}>", cs_type(inner)),
        TypeRef::Map(k, v) => format!("Dictionary<{}, {}>", cs_type(k), cs_type(v)),
    }
}

fn pinvoke_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "int".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "int".into(),
        TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Bytes
        | TypeRef::BorrowedBytes
        | TypeRef::Struct(_)
        | TypeRef::Optional(_)
        | TypeRef::List(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "IntPtr".into(),
        TypeRef::Handle => "ulong".into(),
        TypeRef::TypedHandle(_) => "IntPtr".into(),
        TypeRef::Enum(_) => "int".into(),
    }
}

fn pinvoke_param_list(p: &Param) -> Vec<String> {
    match &p.ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
            format!("IntPtr {}_ptr", p.name),
            format!("UIntPtr {}_len", p.name),
        ],
        TypeRef::List(_) => vec![
            format!("IntPtr {}", p.name),
            format!("UIntPtr {}_len", p.name),
        ],
        TypeRef::Map(_, _) => vec![
            format!("IntPtr {}_keys", p.name),
            format!("IntPtr {}_values", p.name),
            format!("UIntPtr {}_len", p.name),
        ],
        TypeRef::Optional(inner)
            if matches!(inner.as_ref(), TypeRef::Bytes | TypeRef::BorrowedBytes) =>
        {
            vec![
                format!("IntPtr {}_ptr", p.name),
                format!("UIntPtr {}_len", p.name),
            ]
        }
        _ => vec![format!("{} {}", pinvoke_type(&p.ty), p.name)],
    }
}

fn pinvoke_return_info(ty: &TypeRef) -> (String, Vec<String>) {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            ("IntPtr".into(), vec!["out UIntPtr out_len".into()])
        }
        TypeRef::List(_) | TypeRef::Iterator(_) => {
            ("IntPtr".into(), vec!["out UIntPtr out_len".into()])
        }
        TypeRef::Map(_, _) => (
            "void".into(),
            vec![
                "out IntPtr out_keys".into(),
                "out IntPtr out_values".into(),
                "out UIntPtr out_len".into(),
            ],
        ),
        TypeRef::Optional(inner)
            if matches!(inner.as_ref(), TypeRef::Bytes | TypeRef::BorrowedBytes) =>
        {
            ("IntPtr".into(), vec!["out UIntPtr out_len".into()])
        }
        _ => (pinvoke_type(ty), vec![]),
    }
}

fn render_csproj(namespace: &str) -> String {
    format!(
        r#"<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <PackageId>{namespace}</PackageId>
    <Version>0.1.0</Version>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
  </PropertyGroup>

</Project>
"#,
    )
}

fn render_nuspec(namespace: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{namespace}</id>
    <version>0.1.0</version>
    <authors>WeaveFFI Contributors</authors>
    <description>Auto-generated .NET bindings for a WeaveFFI native library.</description>
    <license type="expression">MIT</license>
    <projectUrl>https://github.com/AstroForge-Incorporated/weaveffi</projectUrl>
    <tags>ffi interop native pinvoke</tags>
  </metadata>
</package>
"#,
    )
}

fn render_readme() -> String {
    r#"# WeaveFFI .NET Bindings

Auto-generated P/Invoke bindings for the WeaveFFI native library.

## Build

```bash
dotnet build
```

## Pack

```bash
dotnet pack
```

The resulting `.nupkg` will be in `bin/Debug/` (or `bin/Release/` with `-c Release`).
"#
    .into()
}

fn collect_all_modules(modules: &[Module]) -> Vec<&Module> {
    let mut all = Vec::new();
    for m in modules {
        all.push(m);
        all.extend(collect_all_modules(&m.modules));
    }
    all
}

/// Emits a C# XML doc comment at `indent`. Single-line docs collapse to
/// `/// <summary>text</summary>`; multi-line docs expand to a `<summary>`
/// block with each input line wrapped in its own line.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    if doc.contains('\n') {
        out.push_str(indent);
        out.push_str("/// <summary>\n");
        for line in doc.lines() {
            out.push_str(indent);
            out.push_str("/// ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str("/// </summary>\n");
    } else {
        out.push_str(indent);
        out.push_str("/// <summary>");
        out.push_str(doc);
        out.push_str("</summary>\n");
    }
}

/// Emits a full XML doc block: function `<summary>` plus a `<param>` element
/// per documented parameter. Skips entirely when there is nothing to emit.
fn emit_fn_doc(
    out: &mut String,
    doc: &Option<String>,
    params: &[weaveffi_ir::ir::Param],
    indent: &str,
) {
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    let documented_params: Vec<&weaveffi_ir::ir::Param> = params
        .iter()
        .filter(|p| {
            p.doc
                .as_ref()
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if trimmed_doc.is_none() && documented_params.is_empty() {
        return;
    }
    if let Some(d) = trimmed_doc {
        if d.contains('\n') {
            out.push_str(indent);
            out.push_str("/// <summary>\n");
            for line in d.lines() {
                out.push_str(indent);
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("/// </summary>\n");
        } else {
            out.push_str(indent);
            out.push_str("/// <summary>");
            out.push_str(d);
            out.push_str("</summary>\n");
        }
    }
    for p in documented_params {
        let pdoc = p.doc.as_ref().unwrap().trim();
        let name = safe_cs_name(&p.name);
        if pdoc.contains('\n') {
            out.push_str(indent);
            out.push_str(&format!("/// <param name=\"{}\">\n", name));
            for line in pdoc.lines() {
                out.push_str(indent);
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("/// </param>\n");
        } else {
            out.push_str(indent);
            out.push_str(&format!("/// <param name=\"{}\">{}</param>\n", name, pdoc));
        }
    }
}

fn render_csharp(api: &Api, namespace: &str, strip_module_prefix: bool) -> String {
    let mut out = String::new();
    out.push_str("// Auto-generated by WeaveFFI — do not edit.\n");
    out.push_str(
        "using System;\nusing System.Collections.Generic;\nusing System.Runtime.InteropServices;\n",
    );
    let all_mods = collect_all_modules(&api.modules);
    if all_mods
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async))
    {
        out.push_str("using System.Threading.Tasks;\n");
    }
    out.push('\n');
    out.push_str(&format!("namespace {namespace}\n{{\n"));

    render_exception_class(&mut out);
    render_error_struct(&mut out);
    render_helpers_class(&mut out);

    for (m, path) in collect_modules_with_path(&api.modules) {
        for e in &m.enums {
            render_enum(&mut out, e);
        }
        for s in &m.structs {
            render_struct_class(&mut out, &path, s);
            render_builder_class(&mut out, &path, s);
        }
    }

    render_native_methods(&mut out, api);

    for m in &api.modules {
        render_wrapper_class(&mut out, m, &m.name, "    ", strip_module_prefix);
    }

    out.push_str("}\n");
    out
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

fn render_exception_class(out: &mut String) {
    out.push_str("    public class WeaveffiException : Exception\n    {\n");
    out.push_str("        public int Code { get; }\n\n");
    out.push_str("        public WeaveffiException(int code, string message) : base(message)\n");
    out.push_str("        {\n");
    out.push_str("            Code = code;\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");
}

fn render_error_struct(out: &mut String) {
    out.push_str("    [StructLayout(LayoutKind.Sequential)]\n");
    out.push_str("    internal struct WeaveffiError\n    {\n");
    out.push_str("        public int Code;\n");
    out.push_str("        public IntPtr Message;\n\n");
    out.push_str("        internal static void Check(WeaveffiError err)\n");
    out.push_str("        {\n");
    out.push_str("            if (err.Code != 0)\n");
    out.push_str("            {\n");
    out.push_str("                var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";\n");
    out.push_str("                throw new WeaveffiException(err.Code, msg);\n");
    out.push_str("            }\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");
}

fn render_helpers_class(out: &mut String) {
    out.push_str("    internal static class WeaveFFIHelpers\n    {\n");
    out.push_str("        internal static IntPtr StringToPtr(string? s)\n        {\n");
    out.push_str(
        "            return s == null ? IntPtr.Zero : Marshal.StringToCoTaskMemUTF8(s);\n",
    );
    out.push_str("        }\n\n");
    out.push_str("        internal static string? PtrToString(IntPtr ptr)\n        {\n");
    out.push_str("            return ptr == IntPtr.Zero ? null : Marshal.PtrToStringUTF8(ptr);\n");
    out.push_str("        }\n\n");
    out.push_str("        internal static void FreePtr(IntPtr ptr)\n        {\n");
    out.push_str("            Marshal.FreeCoTaskMem(ptr);\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");
}

fn render_enum(out: &mut String, e: &EnumDef) {
    emit_doc(out, &e.doc, "    ");
    out.push_str(&format!("    public enum {}\n    {{\n", e.name));
    for v in &e.variants {
        emit_doc(out, &v.doc, "        ");
        out.push_str(&format!("        {} = {},\n", v.name, v.value));
    }
    out.push_str("    }\n\n");
}

fn render_struct_class(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    emit_doc(out, &s.doc, "    ");
    out.push_str(&format!(
        "    public class {} : IDisposable\n    {{\n",
        s.name
    ));
    out.push_str("        private IntPtr _handle;\n");
    out.push_str("        private bool _disposed;\n\n");
    out.push_str(&format!(
        "        internal {}(IntPtr handle)\n        {{\n            _handle = handle;\n        }}\n\n",
        s.name
    ));
    out.push_str("        internal IntPtr Handle => _handle;\n\n");

    for field in &s.fields {
        render_struct_getter(out, &prefix, field);
    }

    out.push_str("        public void Dispose()\n        {\n");
    out.push_str("            if (!_disposed)\n            {\n");
    out.push_str(&format!(
        "                NativeMethods.{prefix}_destroy(_handle);\n"
    ));
    out.push_str("                _disposed = true;\n");
    out.push_str("            }\n");
    out.push_str("            GC.SuppressFinalize(this);\n");
    out.push_str("        }\n\n");
    out.push_str(&format!(
        "        ~{}()\n        {{\n            Dispose();\n        }}\n",
        s.name
    ));
    out.push_str("    }\n\n");
}

fn cs_type_builder_storage(ty: &TypeRef) -> String {
    let t = cs_type(ty);
    if t.ends_with('?') {
        t
    } else {
        format!("{t}?")
    }
}

fn render_builder_class(out: &mut String, module_name: &str, s: &StructDef) {
    let _ = module_name;
    if !s.builder {
        return;
    }
    let builder_name = format!("{}Builder", s.name);
    emit_doc(out, &s.doc, "    ");
    out.push_str(&format!("    public class {builder_name}\n    {{\n"));
    for field in &s.fields {
        let storage = cs_type_builder_storage(&field.ty);
        let fname = safe_cs_name(&field.name);
        out.push_str(&format!("        private {storage} _{fname};\n"));
    }
    out.push('\n');
    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let param_ty = cs_type(&field.ty);
        let fname = safe_cs_name(&field.name);
        emit_doc(out, &field.doc, "        ");
        out.push_str(&format!(
            "        public {builder_name} With{pascal}({param_ty} value)\n        {{\n            _{fname} = value;\n            return this;\n        }}\n\n"
        ));
    }
    out.push_str(&format!(
        "        public {name} Build()\n        {{\n",
        name = s.name
    ));
    for field in &s.fields {
        let fname = safe_cs_name(&field.name);
        let raw = field.name.replace('\\', "\\\\").replace('"', "\\\"");
        out.push_str(&format!(
            "            if (_{fname} == null) throw new InvalidOperationException(\"missing field: {raw}\");\n"
        ));
    }
    out.push_str(&format!(
        "            throw new NotImplementedException(\"{builder_name}.Build requires FFI backing\");\n        }}\n    }}\n\n"
    ));
}

fn render_struct_getter(out: &mut String, prefix: &str, field: &StructField) {
    let prop_name = field.name.to_upper_camel_case();
    let getter_sym = format!("{}_get_{}", prefix, field.name);
    let cs = cs_type(&field.ty);

    emit_doc(out, &field.doc, "        ");
    out.push_str(&format!(
        "        public {cs} {prop_name}\n        {{\n            get\n            {{\n"
    ));

    match &field.ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
            out.push_str(&format!(
                "                return NativeMethods.{getter_sym}(_handle);\n"
            ));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!(
                "                return new {name}(NativeMethods.{getter_sym}(_handle));\n"
            ));
        }
        TypeRef::Bool => {
            out.push_str(&format!(
                "                return NativeMethods.{getter_sym}(_handle) != 0;\n"
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "                var ptr = NativeMethods.{getter_sym}(_handle);\n"
            ));
            out.push_str("                var str = WeaveFFIHelpers.PtrToString(ptr);\n");
            out.push_str("                NativeMethods.weaveffi_free_string(ptr);\n");
            out.push_str("                return str;\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!(
                "                var ptr = NativeMethods.{getter_sym}(_handle, out var len);\n"
            ));
            out.push_str("                if (ptr == IntPtr.Zero) return Array.Empty<byte>();\n");
            out.push_str("                var arr = new byte[(int)len];\n");
            out.push_str("                Marshal.Copy(ptr, arr, 0, (int)len);\n");
            out.push_str("                NativeMethods.weaveffi_free_bytes(ptr, len);\n");
            out.push_str("                return arr;\n");
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!(
                "                return ({name})NativeMethods.{getter_sym}(_handle);\n"
            ));
        }
        TypeRef::Struct(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!(
                "                return new {cn}(NativeMethods.{getter_sym}(_handle));\n"
            ));
        }
        TypeRef::Optional(inner)
            if matches!(inner.as_ref(), TypeRef::Bytes | TypeRef::BorrowedBytes) =>
        {
            out.push_str(&format!(
                "                var ptr = NativeMethods.{getter_sym}(_handle, out var len);\n"
            ));
            out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
            out.push_str("                var arr = new byte[(int)len];\n");
            out.push_str("                Marshal.Copy(ptr, arr, 0, (int)len);\n");
            out.push_str("                NativeMethods.weaveffi_free_bytes(ptr, len);\n");
            out.push_str("                return arr;\n");
        }
        TypeRef::Optional(inner) => {
            out.push_str(&format!(
                "                var ptr = NativeMethods.{getter_sym}(_handle);\n"
            ));
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                var str = WeaveFFIHelpers.PtrToString(ptr);\n");
                    out.push_str("                NativeMethods.weaveffi_free_string(ptr);\n");
                    out.push_str("                return str;\n");
                }
                TypeRef::Struct(name) => {
                    let cn = local_type_name(name);
                    out.push_str(&format!(
                        "                return ptr == IntPtr.Zero ? null : new {cn}(ptr);\n"
                    ));
                }
                TypeRef::I32 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return Marshal.ReadInt32(ptr);\n");
                }
                TypeRef::U32 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return (uint)Marshal.ReadInt32(ptr);\n");
                }
                TypeRef::I64 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return Marshal.ReadInt64(ptr);\n");
                }
                TypeRef::F64 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return BitConverter.Int64BitsToDouble(Marshal.ReadInt64(ptr));\n");
                }
                TypeRef::Bool => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return Marshal.ReadInt32(ptr) != 0;\n");
                }
                TypeRef::Handle => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return (ulong)Marshal.ReadInt64(ptr);\n");
                }
                TypeRef::TypedHandle(name) => {
                    out.push_str(&format!(
                        "                return ptr == IntPtr.Zero ? null : new {name}(ptr);\n"
                    ));
                }
                TypeRef::Enum(name) => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str(&format!(
                        "                return ({name})Marshal.ReadInt32(ptr);\n"
                    ));
                }
                _ => {
                    out.push_str("                return ptr;\n");
                }
            }
        }
        TypeRef::List(inner) => {
            out.push_str(&format!(
                "                var ptr = NativeMethods.{getter_sym}(_handle, out var len);\n"
            ));
            render_list_unmarshal(out, inner, "                ");
        }
        TypeRef::Map(k, v) => {
            out.push_str(&format!(
                "                NativeMethods.{getter_sym}(_handle, out var outKeys, out var outValues, out var outLen);\n"
            ));
            let k_cs = cs_type(k);
            let v_cs = cs_type(v);
            out.push_str(&format!(
                "                var dict = new Dictionary<{k_cs}, {v_cs}>();\n"
            ));
            out.push_str(
                "                for (int i = 0; i < (int)outLen; i++)\n                {\n",
            );
            let key_read = marshal_read_element(k, "outKeys", "i");
            let val_read = marshal_read_element(v, "outValues", "i");
            out.push_str(&format!(
                "                    dict[{key_read}] = {val_read};\n"
            ));
            out.push_str("                }\n");
            out.push_str("                return dict;\n");
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as struct field"),
    }

    out.push_str("            }\n        }\n\n");
}

fn render_list_unmarshal(out: &mut String, inner: &TypeRef, indent: &str) {
    let elem = cs_type(inner);
    out.push_str(&format!(
        "{indent}if (ptr == IntPtr.Zero) return Array.Empty<{elem}>();\n"
    ));
    match inner {
        TypeRef::I32 => {
            out.push_str(&format!("{indent}var arr = new int[(int)len];\n"));
            out.push_str(&format!("{indent}Marshal.Copy(ptr, arr, 0, (int)len);\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::I64 => {
            out.push_str(&format!("{indent}var arr = new long[(int)len];\n"));
            out.push_str(&format!("{indent}Marshal.Copy(ptr, arr, 0, (int)len);\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::F64 => {
            out.push_str(&format!("{indent}var arr = new double[(int)len];\n"));
            out.push_str(&format!("{indent}Marshal.Copy(ptr, arr, 0, (int)len);\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::Struct(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}var arr = new {cn}[(int)len];\n"));
            out.push_str(&format!(
                "{indent}for (int i = 0; i < (int)len; i++)\n{indent}{{\n"
            ));
            out.push_str(&format!(
                "{indent}    arr[i] = new {cn}(Marshal.ReadIntPtr(ptr, i * IntPtr.Size));\n"
            ));
            out.push_str(&format!("{indent}}}\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("{indent}var arr = new {name}[(int)len];\n"));
            out.push_str(&format!(
                "{indent}for (int i = 0; i < (int)len; i++)\n{indent}{{\n"
            ));
            out.push_str(&format!(
                "{indent}    arr[i] = ({name})Marshal.ReadInt32(ptr + i * sizeof(int));\n"
            ));
            out.push_str(&format!("{indent}}}\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        _ => {
            out.push_str(&format!("{indent}return Array.Empty<{elem}>();\n"));
        }
    }
}

fn render_native_methods(out: &mut String, api: &Api) {
    out.push_str("    internal static class NativeMethods\n    {\n");
    out.push_str("        private const string LibName = \"weaveffi\";\n\n");

    out.push_str("        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]\n");
    out.push_str("        internal static extern void weaveffi_free_string(IntPtr ptr);\n\n");
    out.push_str("        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]\n");
    out.push_str(
        "        internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);\n\n",
    );

    for (m, path) in collect_modules_with_path(&api.modules) {
        for s in &m.structs {
            render_struct_pinvoke(out, &path, s);
        }
        for f in &m.functions {
            render_function_pinvoke(out, &path, f);
            if f.r#async {
                render_async_function_pinvoke(out, &path, f);
            }
        }
    }

    out.push_str("    }\n\n");
}

fn render_struct_pinvoke(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    let mut create_params: Vec<String> = s
        .fields
        .iter()
        .flat_map(|f| {
            let p = Param {
                name: f.name.clone(),
                ty: f.ty.clone(),
                mutable: false,
                doc: f.doc.clone(),
            };
            pinvoke_param_list(&p)
        })
        .collect();
    create_params.push("ref WeaveffiError err".into());

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{prefix}_create\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern IntPtr {prefix}_create({});\n\n",
        create_params.join(", ")
    ));

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{prefix}_destroy\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern void {prefix}_destroy(IntPtr ptr);\n\n"
    ));

    for field in &s.fields {
        let getter_sym = format!("{prefix}_get_{}", field.name);
        let (ret_type, extra_params) = pinvoke_return_info(&field.ty);

        out.push_str(&format!(
            "        [DllImport(LibName, EntryPoint = \"{getter_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
        ));
        let mut params = vec!["IntPtr ptr".into()];
        params.extend(extra_params);
        out.push_str(&format!(
            "        internal static extern {ret_type} {getter_sym}({});\n\n",
            params.join(", ")
        ));
    }
}

fn render_function_pinvoke(out: &mut String, module_name: &str, f: &Function) {
    let c_sym = c_symbol_name(module_name, &f.name);

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{c_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));

    let mut params: Vec<String> = f.params.iter().flat_map(pinvoke_param_list).collect();

    let ret_type = if let Some(ret) = &f.returns {
        let (ret_cs, extra) = pinvoke_return_info(ret);
        params.extend(extra);
        ret_cs
    } else {
        "void".into()
    };

    params.push("ref WeaveffiError err".into());

    out.push_str(&format!(
        "        internal static extern {ret_type} {c_sym}({});\n\n",
        params.join(", ")
    ));
}

fn async_cb_delegate_result_params(ret: &Option<TypeRef>) -> String {
    match ret {
        None => String::new(),
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Iterator(_)) => {
            ", IntPtr result, UIntPtr resultLen".into()
        }
        Some(TypeRef::Map(_, _)) => {
            ", IntPtr resultKeys, IntPtr resultValues, UIntPtr resultLen".into()
        }
        Some(ty) => format!(", {} result", pinvoke_type(ty)),
    }
}

fn async_cb_lambda_params(ret: &Option<TypeRef>) -> &'static str {
    match ret {
        None => "(context, err)",
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Iterator(_)) => {
            "(context, err, result, resultLen)"
        }
        Some(TypeRef::Map(_, _)) => "(context, err, resultKeys, resultValues, resultLen)",
        Some(_) => "(context, err, result)",
    }
}

fn render_async_function_pinvoke(out: &mut String, module_name: &str, f: &Function) {
    let c_sym = c_symbol_name(module_name, &f.name);
    let delegate_name = format!("AsyncCb_{c_sym}");
    let cb_params = async_cb_delegate_result_params(&f.returns);

    out.push_str("        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]\n");
    out.push_str(&format!(
        "        internal delegate void {delegate_name}(IntPtr context, IntPtr err{cb_params});\n\n"
    ));

    let mut params: Vec<String> = f.params.iter().flat_map(pinvoke_param_list).collect();
    if f.cancellable {
        params.push("IntPtr cancel_token".into());
    }
    params.push(format!("{delegate_name} callback"));
    params.push("IntPtr context".into());

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{c_sym}_async\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern void {c_sym}_async({});\n\n",
        params.join(", ")
    ));
}

fn render_wrapper_class(
    out: &mut String,
    m: &Module,
    module_path: &str,
    indent: &str,
    strip_module_prefix: bool,
) {
    let class_name = m.name.to_upper_camel_case();
    out.push_str(&format!(
        "{indent}public static class {class_name}\n{indent}{{\n"
    ));

    for f in &m.functions {
        let mut buf = String::new();
        render_wrapper_method(&mut buf, module_path, f, strip_module_prefix);
        reindent(out, &buf, indent.len().saturating_sub(4));
    }

    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        let inner_indent = format!("{indent}    ");
        render_wrapper_class(out, sub, &sub_path, &inner_indent, strip_module_prefix);
    }

    out.push_str(&format!("{indent}}}\n\n"));
}

fn param_needs_marshal(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            true
        }
        TypeRef::Optional(inner) => matches!(
            inner.as_ref(),
            TypeRef::StringUtf8
                | TypeRef::BorrowedStr
                | TypeRef::Bytes
                | TypeRef::BorrowedBytes
                | TypeRef::I32
                | TypeRef::U32
                | TypeRef::I64
                | TypeRef::F64
                | TypeRef::Bool
                | TypeRef::Handle
                | TypeRef::Enum(_)
        ),
        _ => false,
    }
}

fn reindent(out: &mut String, buf: &str, extra: usize) {
    if extra == 0 {
        out.push_str(buf);
        return;
    }
    let pad = " ".repeat(extra);
    for line in buf.lines() {
        if line.is_empty() {
            out.push('\n');
        } else {
            out.push_str(&pad);
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn render_wrapper_method(
    out: &mut String,
    module_path: &str,
    f: &Function,
    strip_module_prefix: bool,
) {
    if f.r#async {
        render_async_wrapper_method(out, module_path, f, strip_module_prefix);
        return;
    }
    let method_name = wrapper_name(module_path, &f.name, strip_module_prefix).to_upper_camel_case();
    let ret_cs = f
        .returns
        .as_ref()
        .map(cs_type)
        .unwrap_or_else(|| "void".into());

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    emit_fn_doc(out, &f.doc, &f.params, "        ");
    if let Some(msg) = &f.deprecated {
        out.push_str(&format!(
            "        [Obsolete(\"{}\")]\n",
            msg.replace('"', "\\\"")
        ));
    }

    out.push_str(&format!(
        "        public static {ret_cs} {method_name}({})\n        {{\n",
        params_sig.join(", ")
    ));

    out.push_str("            var err = new WeaveffiError();\n");

    let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));

    if needs_try {
        for p in &f.params {
            render_marshal_setup(out, p, "            ");
        }
        out.push_str("            try\n            {\n");
        render_pinvoke_call_and_return(out, module_path, f, "                ");
        out.push_str("            }\n            finally\n            {\n");
        for p in &f.params {
            render_marshal_cleanup(out, p, "                ");
        }
        out.push_str("            }\n");
    } else {
        render_pinvoke_call_and_return(out, module_path, f, "            ");
    }

    out.push_str("        }\n\n");
}

fn render_async_wrapper_method(
    out: &mut String,
    module_path: &str,
    f: &Function,
    strip_module_prefix: bool,
) {
    let method_name = wrapper_name(module_path, &f.name, strip_module_prefix).to_upper_camel_case();
    let c_sym = c_symbol_name(module_path, &f.name);
    let delegate_name = format!("NativeMethods.AsyncCb_{c_sym}");

    let task_ret = f
        .returns
        .as_ref()
        .map(|ty| format!("Task<{}>", cs_type(ty)))
        .unwrap_or_else(|| "Task".into());

    let tcs_type = f
        .returns
        .as_ref()
        .map(cs_type)
        .unwrap_or_else(|| "bool".into());

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    emit_fn_doc(out, &f.doc, &f.params, "        ");
    if let Some(msg) = &f.deprecated {
        out.push_str(&format!(
            "        [Obsolete(\"{}\")]\n",
            msg.replace('"', "\\\"")
        ));
    }

    out.push_str(&format!(
        "        public static async {task_ret} {method_name}({})\n        {{\n",
        params_sig.join(", ")
    ));

    out.push_str(&format!(
        "            var tcs = new TaskCompletionSource<{tcs_type}>(TaskCreationOptions.RunContinuationsAsynchronously);\n"
    ));

    let cb_lambda_params = async_cb_lambda_params(&f.returns);
    out.push_str(&format!(
        "            {delegate_name} callback = {cb_lambda_params} =>\n            {{\n"
    ));

    out.push_str("                if (err != IntPtr.Zero)\n                {\n");
    out.push_str("                    var wErr = Marshal.PtrToStructure<WeaveffiError>(err);\n");
    out.push_str("                    if (wErr.Code != 0)\n                    {\n");
    out.push_str(
        "                        var msg = Marshal.PtrToStringUTF8(wErr.Message) ?? \"\";\n",
    );
    out.push_str(
        "                        tcs.SetException(new WeaveffiException(wErr.Code, msg));\n",
    );
    out.push_str("                        return;\n");
    out.push_str("                    }\n");
    out.push_str("                }\n");

    render_async_set_result(out, &f.returns, "                ");

    out.push_str("            };\n");
    out.push_str("            var gcHandle = GCHandle.Alloc(callback);\n");

    let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
    let call_args = build_call_args(&f.params);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let cancel_arg = if f.cancellable { "IntPtr.Zero, " } else { "" };

    if needs_try {
        for p in &f.params {
            render_marshal_setup(out, p, "            ");
        }
        out.push_str("            try\n            {\n");
        out.push_str(&format!(
            "                NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, IntPtr.Zero);\n"
        ));
        out.push_str("            }\n            finally\n            {\n");
        for p in &f.params {
            render_marshal_cleanup(out, p, "                ");
        }
        out.push_str("            }\n");
    } else {
        out.push_str(&format!(
            "            NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, IntPtr.Zero);\n"
        ));
    }

    if f.returns.is_some() {
        out.push_str("            return await tcs.Task;\n");
    } else {
        out.push_str("            await tcs.Task;\n");
    }

    out.push_str("        }\n\n");
}

fn render_async_set_result(out: &mut String, ret: &Option<TypeRef>, indent: &str) {
    match ret {
        None => {
            out.push_str(&format!("{indent}tcs.SetResult(true);\n"));
        }
        Some(TypeRef::Bool) => {
            out.push_str(&format!("{indent}tcs.SetResult(result != 0);\n"));
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str(&format!(
                "{indent}var str = Marshal.PtrToStringUTF8(result);\n"
            ));
            out.push_str(&format!(
                "{indent}NativeMethods.weaveffi_free_string(result);\n"
            ));
            out.push_str(&format!("{indent}tcs.SetResult(str);\n"));
        }
        Some(TypeRef::Enum(name)) => {
            out.push_str(&format!("{indent}tcs.SetResult(({name})result);\n"));
        }
        Some(TypeRef::Struct(name)) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}tcs.SetResult(new {cn}(result));\n"));
        }
        Some(TypeRef::TypedHandle(name)) => {
            out.push_str(&format!("{indent}tcs.SetResult(new {name}(result));\n"));
        }
        _ => {
            out.push_str(&format!("{indent}tcs.SetResult(result);\n"));
        }
    }
}

fn render_marshal_setup(out: &mut String, p: &Param, indent: &str) {
    let name = safe_cs_name(&p.name);
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}var {name}Ptr = Marshal.StringToCoTaskMemUTF8({name});\n"
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!(
                "{indent}var {name}Pin = GCHandle.Alloc({name}, GCHandleType.Pinned);\n"
            ));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!(
                    "{indent}var {name}Ptr = {name} != null ? Marshal.StringToCoTaskMemUTF8({name}) : IntPtr.Zero;\n"
                ));
            }
            TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) | TypeRef::U32 => {
                out.push_str(&format!("{indent}var {name}Ptr = IntPtr.Zero;\n"));
                out.push_str(&format!("{indent}if ({name}.HasValue)\n{indent}{{\n"));
                out.push_str(&format!(
                    "{indent}    {name}Ptr = Marshal.AllocHGlobal(sizeof(int));\n"
                ));
                let val = match inner.as_ref() {
                    TypeRef::Bool => format!("{name}.Value ? 1 : 0"),
                    TypeRef::Enum(_) => format!("(int){name}.Value"),
                    TypeRef::U32 => format!("(int){name}.Value"),
                    _ => format!("{name}.Value"),
                };
                out.push_str(&format!(
                    "{indent}    Marshal.WriteInt32({name}Ptr, {val});\n"
                ));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::I64 | TypeRef::Handle | TypeRef::F64 => {
                out.push_str(&format!("{indent}var {name}Ptr = IntPtr.Zero;\n"));
                out.push_str(&format!("{indent}if ({name}.HasValue)\n{indent}{{\n"));
                out.push_str(&format!(
                    "{indent}    {name}Ptr = Marshal.AllocHGlobal(sizeof(long));\n"
                ));
                let val = match inner.as_ref() {
                    TypeRef::Handle => format!("(long){name}.Value"),
                    TypeRef::F64 => {
                        format!("BitConverter.DoubleToInt64Bits({name}.Value)")
                    }
                    _ => format!("{name}.Value"),
                };
                out.push_str(&format!(
                    "{indent}    Marshal.WriteInt64({name}Ptr, {val});\n"
                ));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!(
                    "{indent}var {name}Pin = {name} != null ? GCHandle.Alloc({name}, GCHandleType.Pinned) : default;\n"
                ));
            }
            _ => {}
        },
        _ => {}
    }
}

fn render_marshal_cleanup(out: &mut String, p: &Param, indent: &str) {
    let name = safe_cs_name(&p.name);
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{indent}Marshal.FreeCoTaskMem({name}Ptr);\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{indent}{name}Pin.Free();\n"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!(
                    "{indent}if ({name}Ptr != IntPtr.Zero) Marshal.FreeCoTaskMem({name}Ptr);\n"
                ));
            }
            TypeRef::I32
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_) => {
                out.push_str(&format!(
                    "{indent}if ({name}Ptr != IntPtr.Zero) Marshal.FreeHGlobal({name}Ptr);\n"
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("{indent}if ({name} != null) {name}Pin.Free();\n"));
            }
            _ => {}
        },
        _ => {}
    }
}

fn render_pinvoke_call_and_return(out: &mut String, module_path: &str, f: &Function, indent: &str) {
    let c_sym = c_symbol_name(module_path, &f.name);
    let call_args = build_call_args(&f.params);

    if let Some(TypeRef::Map(k, v)) = &f.returns {
        render_map_return_call(out, &c_sym, &call_args, k, v, indent);
        return;
    }

    let has_out_len = f.returns.as_ref().is_some_and(|r| {
        matches!(
            r,
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Iterator(_)
        ) || matches!(
            r,
            TypeRef::Optional(inner)
                if matches!(inner.as_ref(), TypeRef::Bytes | TypeRef::BorrowedBytes)
        )
    });

    if f.returns.is_some() {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let out_len_part = if has_out_len { "out var outLen, " } else { "" };
        out.push_str(&format!(
            "{indent}var result = NativeMethods.{c_sym}({args_part}{out_len_part}ref err);\n"
        ));
    } else {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        out.push_str(&format!(
            "{indent}NativeMethods.{c_sym}({args_part}ref err);\n"
        ));
    }

    out.push_str(&format!("{indent}WeaveffiError.Check(err);\n"));

    if let Some(ret_ty) = &f.returns {
        render_return_conversion(out, ret_ty, indent);
    }
}

fn build_call_args(params: &[Param]) -> String {
    params
        .iter()
        .flat_map(|p| {
            let name = safe_cs_name(&p.name);
            match &p.ty {
                TypeRef::Bool => vec![format!("{name} ? 1 : 0")],
                TypeRef::Enum(_) => vec![format!("(int){name}")],
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("{name}Ptr")],
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}.Handle")],
                TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
                    format!("{name}Pin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}.Length"),
                ],
                TypeRef::Optional(inner) => match inner.as_ref() {
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        vec![format!("{name}?.Handle ?? IntPtr.Zero")]
                    }
                    TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
                        format!("{name} != null ? {name}Pin.AddrOfPinnedObject() : IntPtr.Zero"),
                        format!("{name} != null ? (UIntPtr){name}.Length : UIntPtr.Zero"),
                    ],
                    TypeRef::StringUtf8
                    | TypeRef::BorrowedStr
                    | TypeRef::I32
                    | TypeRef::U32
                    | TypeRef::I64
                    | TypeRef::F64
                    | TypeRef::Bool
                    | TypeRef::Handle
                    | TypeRef::Enum(_) => vec![format!("{name}Ptr")],
                    _ => vec![name],
                },
                _ => vec![name],
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_return_conversion(out: &mut String, ty: &TypeRef, indent: &str) {
    match ty {
        TypeRef::Bool => {
            out.push_str(&format!("{indent}return result != 0;\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}var str = Marshal.PtrToStringUTF8(result);\n"
            ));
            out.push_str(&format!(
                "{indent}NativeMethods.weaveffi_free_string(result);\n"
            ));
            out.push_str(&format!("{indent}return str;\n"));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("{indent}return ({name})result;\n"));
        }
        TypeRef::Struct(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}return new {cn}(result);\n"));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("{indent}return new {name}(result);\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return Array.Empty<byte>();\n"
            ));
            out.push_str(&format!("{indent}var arr = new byte[(int)outLen];\n"));
            out.push_str(&format!(
                "{indent}Marshal.Copy(result, arr, 0, (int)outLen);\n"
            ));
            out.push_str(&format!(
                "{indent}NativeMethods.weaveffi_free_bytes(result, outLen);\n"
            ));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::Optional(inner) => {
            render_optional_return_conversion(out, inner, indent);
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            render_list_return(out, inner, indent);
        }
        TypeRef::Map(_, _) => {}
        _ => {
            out.push_str(&format!("{indent}return result;\n"));
        }
    }
}

fn render_optional_return_conversion(out: &mut String, inner: &TypeRef, indent: &str) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}var str = Marshal.PtrToStringUTF8(result);\n"
            ));
            out.push_str(&format!(
                "{indent}NativeMethods.weaveffi_free_string(result);\n"
            ));
            out.push_str(&format!("{indent}return str;\n"));
        }
        TypeRef::Struct(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!(
                "{indent}return result == IntPtr.Zero ? null : new {cn}(result);\n"
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!("{indent}var arr = new byte[(int)outLen];\n"));
            out.push_str(&format!(
                "{indent}Marshal.Copy(result, arr, 0, (int)outLen);\n"
            ));
            out.push_str(&format!(
                "{indent}NativeMethods.weaveffi_free_bytes(result, outLen);\n"
            ));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::I32 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!("{indent}return Marshal.ReadInt32(result);\n"));
        }
        TypeRef::U32 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return (uint)Marshal.ReadInt32(result);\n"
            ));
        }
        TypeRef::I64 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!("{indent}return Marshal.ReadInt64(result);\n"));
        }
        TypeRef::F64 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return BitConverter.Int64BitsToDouble(Marshal.ReadInt64(result));\n"
            ));
        }
        TypeRef::Bool => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!("{indent}return Marshal.ReadInt32(result) != 0;\n"));
        }
        TypeRef::Handle => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return (ulong)Marshal.ReadInt64(result);\n"
            ));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!(
                "{indent}return result == IntPtr.Zero ? null : new {name}(result);\n"
            ));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return ({name})Marshal.ReadInt32(result);\n"
            ));
        }
        _ => {
            out.push_str(&format!("{indent}return result;\n"));
        }
    }
}

fn render_list_return(out: &mut String, inner: &TypeRef, indent: &str) {
    let elem = cs_type(inner);
    out.push_str(&format!(
        "{indent}if (result == IntPtr.Zero) return Array.Empty<{elem}>();\n"
    ));
    match inner {
        TypeRef::I32 => {
            out.push_str(&format!("{indent}var arr = new int[(int)outLen];\n"));
            out.push_str(&format!(
                "{indent}Marshal.Copy(result, arr, 0, (int)outLen);\n"
            ));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::I64 => {
            out.push_str(&format!("{indent}var arr = new long[(int)outLen];\n"));
            out.push_str(&format!(
                "{indent}Marshal.Copy(result, arr, 0, (int)outLen);\n"
            ));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::F64 => {
            out.push_str(&format!("{indent}var arr = new double[(int)outLen];\n"));
            out.push_str(&format!(
                "{indent}Marshal.Copy(result, arr, 0, (int)outLen);\n"
            ));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::Struct(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}var arr = new {cn}[(int)outLen];\n"));
            out.push_str(&format!(
                "{indent}for (int i = 0; i < (int)outLen; i++)\n{indent}{{\n"
            ));
            out.push_str(&format!(
                "{indent}    arr[i] = new {cn}(Marshal.ReadIntPtr(result, i * IntPtr.Size));\n"
            ));
            out.push_str(&format!("{indent}}}\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("{indent}var arr = new {name}[(int)outLen];\n"));
            out.push_str(&format!(
                "{indent}for (int i = 0; i < (int)outLen; i++)\n{indent}{{\n"
            ));
            out.push_str(&format!(
                "{indent}    arr[i] = ({name})Marshal.ReadInt32(result + i * sizeof(int));\n"
            ));
            out.push_str(&format!("{indent}}}\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        _ => {
            out.push_str(&format!("{indent}return Array.Empty<{elem}>();\n"));
        }
    }
}

fn render_map_return_call(
    out: &mut String,
    c_sym: &str,
    call_args: &str,
    k: &TypeRef,
    v: &TypeRef,
    indent: &str,
) {
    let k_cs = cs_type(k);
    let v_cs = cs_type(v);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    out.push_str(&format!(
        "{indent}NativeMethods.{c_sym}({args_part}out var outKeys, out var outValues, out var outLen, ref err);\n"
    ));
    out.push_str(&format!("{indent}WeaveffiError.Check(err);\n"));
    out.push_str(&format!(
        "{indent}var dict = new Dictionary<{k_cs}, {v_cs}>();\n"
    ));
    out.push_str(&format!(
        "{indent}for (int i = 0; i < (int)outLen; i++)\n{indent}{{\n"
    ));
    let key_read = marshal_read_element(k, "outKeys", "i");
    let val_read = marshal_read_element(v, "outValues", "i");
    out.push_str(&format!("{indent}    dict[{key_read}] = {val_read};\n"));
    out.push_str(&format!("{indent}}}\n"));
    out.push_str(&format!("{indent}return dict;\n"));
}

fn marshal_read_element(ty: &TypeRef, arr: &str, idx: &str) -> String {
    match ty {
        TypeRef::I32 => format!("Marshal.ReadInt32({arr} + {idx} * sizeof(int))"),
        TypeRef::U32 => {
            format!("(uint)Marshal.ReadInt32({arr} + {idx} * sizeof(int))")
        }
        TypeRef::I64 => format!("Marshal.ReadInt64({arr} + {idx} * sizeof(long))"),
        TypeRef::F64 => format!(
            "BitConverter.Int64BitsToDouble(Marshal.ReadInt64({arr} + {idx} * sizeof(long)))"
        ),
        TypeRef::Bool => {
            format!("Marshal.ReadInt32({arr} + {idx} * sizeof(int)) != 0")
        }
        TypeRef::Handle => {
            format!("(ulong)Marshal.ReadInt64({arr} + {idx} * sizeof(long))")
        }
        TypeRef::TypedHandle(name) => {
            format!("new {name}(Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size))")
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("Marshal.PtrToStringUTF8(Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size))")
        }
        TypeRef::Enum(name) => {
            format!("({name})Marshal.ReadInt32({arr} + {idx} * sizeof(int))")
        }
        TypeRef::Struct(name) => {
            let cn = local_type_name(name);
            format!("new {cn}(Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size))")
        }
        _ => "default".into(),
    }
}

fn safe_cs_name(name: &str) -> String {
    match name {
        "string" | "int" | "long" | "double" | "float" | "bool" | "byte" | "object" | "class"
        | "struct" | "enum" | "event" | "delegate" | "namespace" | "ref" | "out" | "in"
        | "params" | "is" | "as" | "new" | "this" | "base" | "null" | "true" | "false"
        | "return" | "void" | "if" | "else" | "for" | "while" | "do" | "switch" | "case"
        | "break" | "continue" | "try" | "catch" | "finally" | "throw" | "using" | "static"
        | "const" | "readonly" | "override" | "virtual" | "abstract" | "sealed" | "public"
        | "private" | "protected" | "internal" => format!("@{name}"),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::config::GeneratorConfig;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
            generators: None,
        }
    }

    fn simple_module(functions: Vec<Function>) -> Module {
        Module {
            name: "math".into(),
            functions,
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    #[test]
    fn generator_name_is_dotnet() {
        assert_eq!(DotnetGenerator.name(), "dotnet");
    }

    #[test]
    fn output_files_lists_all() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = DotnetGenerator.output_files(&api, out);
        assert_eq!(
            files,
            vec![
                out.join("dotnet/WeaveFFI.cs").to_string(),
                out.join("dotnet/WeaveFFI.csproj").to_string(),
                out.join("dotnet/WeaveFFI.nuspec").to_string(),
                out.join("dotnet/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn generate_creates_output_file() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_gen_output");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir).unwrap();

        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(cs.contains("namespace WeaveFFI"));
        assert!(cs.contains("DllImport"));
        assert!(cs.contains("weaveffi_math_add"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dotnet_builder_generated() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
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
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        DotnetGenerator.generate(&api, out).unwrap();
        let dotnet_dir = out.join("dotnet");
        let cs_files: Vec<_> = std::fs::read_dir(&dotnet_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "cs").unwrap_or(false))
            .collect();
        assert!(!cs_files.is_empty(), "expected .cs files");
        let cs = std::fs::read_to_string(cs_files[0].path()).unwrap();
        assert!(
            cs.contains("class ContactBuilder"),
            "missing builder class: {cs}"
        );
        assert!(cs.contains("WithName("), "missing WithName: {cs}");
        assert!(cs.contains("WithAge("), "missing WithAge: {cs}");
        assert!(cs.contains("Build()"), "missing Build: {cs}");
    }

    #[test]
    fn dotnet_generates_csproj() {
        let api = make_api(vec![simple_module(vec![])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_csproj");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir).unwrap();

        let csproj_path = tmp.join("dotnet/WeaveFFI.csproj");
        assert!(csproj_path.exists(), ".csproj file must exist");
        let csproj = std::fs::read_to_string(&csproj_path).unwrap();
        assert!(
            csproj.contains(r#"Sdk="Microsoft.NET.Sdk""#),
            "missing SDK attribute: {csproj}"
        );
        assert!(
            csproj.contains("<TargetFramework>net8.0</TargetFramework>"),
            "missing target framework: {csproj}"
        );
        assert!(
            csproj.contains("<PackageId>WeaveFFI</PackageId>"),
            "missing package id: {csproj}"
        );
        assert!(
            csproj.contains("<Version>0.1.0</Version>"),
            "missing version: {csproj}"
        );

        let nuspec_path = tmp.join("dotnet/WeaveFFI.nuspec");
        assert!(nuspec_path.exists(), ".nuspec file must exist");
        let nuspec = std::fs::read_to_string(&nuspec_path).unwrap();
        assert!(
            nuspec.contains("<id>WeaveFFI</id>"),
            "missing nuspec id: {nuspec}"
        );

        let readme_path = tmp.join("dotnet/README.md");
        assert!(readme_path.exists(), "README.md must exist");
        let readme = std::fs::read_to_string(&readme_path).unwrap();
        assert!(
            readme.contains("dotnet build"),
            "missing build instructions: {readme}"
        );
        assert!(
            readme.contains("dotnet pack"),
            "missing pack instructions: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cs_type_mapping() {
        assert_eq!(cs_type(&TypeRef::I32), "int");
        assert_eq!(cs_type(&TypeRef::U32), "uint");
        assert_eq!(cs_type(&TypeRef::I64), "long");
        assert_eq!(cs_type(&TypeRef::F64), "double");
        assert_eq!(cs_type(&TypeRef::Bool), "bool");
        assert_eq!(cs_type(&TypeRef::StringUtf8), "string");
        assert_eq!(cs_type(&TypeRef::Handle), "ulong");
        assert_eq!(cs_type(&TypeRef::Bytes), "byte[]");
        assert_eq!(cs_type(&TypeRef::Struct("Foo".into())), "Foo");
        assert_eq!(cs_type(&TypeRef::Enum("Bar".into())), "Bar");
        assert_eq!(cs_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "int?");
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "string?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Struct("X".into())))),
            "X?"
        );
        assert_eq!(cs_type(&TypeRef::List(Box::new(TypeRef::I32))), "int[]");
        assert_eq!(
            cs_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "Dictionary<string, int>"
        );
    }

    #[test]
    fn pinvoke_type_mapping() {
        assert_eq!(pinvoke_type(&TypeRef::I32), "int");
        assert_eq!(pinvoke_type(&TypeRef::Bool), "int");
        assert_eq!(pinvoke_type(&TypeRef::StringUtf8), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Handle), "ulong");
        assert_eq!(pinvoke_type(&TypeRef::Bytes), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Struct("Foo".into())), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Enum("Bar".into())), "int");
        assert_eq!(
            pinvoke_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "IntPtr"
        );
    }

    #[test]
    fn simple_i32_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(cs.contains("namespace WeaveFFI"), "missing namespace: {cs}");
        assert!(cs.contains("DllImport"), "missing DllImport: {cs}");
        assert!(cs.contains("weaveffi_math_add"), "missing C symbol: {cs}");
        assert!(
            cs.contains("CallingConvention.Cdecl"),
            "missing Cdecl: {cs}"
        );
        assert!(
            cs.contains("public static int Add("),
            "missing wrapper method: {cs}"
        );
        assert!(
            cs.contains("WeaveffiError.Check(err)"),
            "missing error check: {cs}"
        );
    }

    #[test]
    fn void_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "reset".into(),
            params: vec![],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public static void Reset()"),
            "missing void wrapper: {cs}"
        );
        assert!(
            cs.contains("static extern void weaveffi_math_reset"),
            "missing void P/Invoke: {cs}"
        );
    }

    #[test]
    fn bool_uses_int_marshaling() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "is_valid".into(),
            params: vec![Param {
                name: "flag".into(),
                ty: TypeRef::Bool,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("flag ? 1 : 0"),
            "missing bool-to-int conversion: {cs}"
        );
        assert!(
            cs.contains("result != 0"),
            "missing int-to-bool conversion: {cs}"
        );
    }

    #[test]
    fn enum_generation() {
        let api = make_api(vec![Module {
            name: "paint".into(),
            functions: vec![Function {
                name: "mix".into(),
                params: vec![Param {
                    name: "a".into(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                    doc: None,
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
                doc: Some("Primary colors".into()),
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
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(cs.contains("public enum Color"), "missing enum: {cs}");
        assert!(cs.contains("Red = 0"), "missing Red: {cs}");
        assert!(cs.contains("Green = 1"), "missing Green: {cs}");
        assert!(cs.contains("Blue = 2"), "missing Blue: {cs}");
        assert!(
            cs.contains("<summary>Primary colors</summary>"),
            "missing doc: {cs}"
        );
        assert!(cs.contains("(int)a"), "missing enum-to-int cast: {cs}");
        assert!(
            cs.contains("(Color)result"),
            "missing int-to-enum cast: {cs}"
        );
    }

    #[test]
    fn struct_has_idisposable() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
                fields: vec![
                    StructField {
                        name: "first_name".into(),
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
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public class Contact : IDisposable"),
            "missing IDisposable: {cs}"
        );
        assert!(
            cs.contains("private IntPtr _handle"),
            "missing handle field: {cs}"
        );
        assert!(
            cs.contains("internal Contact(IntPtr handle)"),
            "missing constructor: {cs}"
        );
        assert!(
            cs.contains("<summary>A contact record</summary>"),
            "missing doc: {cs}"
        );
    }

    #[test]
    fn struct_has_property_getters() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "first_name".into(),
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
                    StructField {
                        name: "active".into(),
                        ty: TypeRef::Bool,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "role".into(),
                        ty: TypeRef::Enum("Role".into()),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public string FirstName"),
            "missing FirstName: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_contacts_Contact_get_first_name(_handle)"),
            "missing getter call: {cs}"
        );
        assert!(
            cs.contains("WeaveFFIHelpers.PtrToString(ptr)"),
            "missing UTF8 unmarshal: {cs}"
        );
        assert!(
            cs.contains("weaveffi_free_string(ptr)"),
            "missing free_string: {cs}"
        );
        assert!(cs.contains("public int Age"), "missing Age: {cs}");
        assert!(
            cs.contains("NativeMethods.weaveffi_contacts_Contact_get_age(_handle)"),
            "missing age getter: {cs}"
        );
        assert!(cs.contains("public bool Active"), "missing Active: {cs}");
        assert!(
            cs.contains("weaveffi_contacts_Contact_get_active(_handle) != 0"),
            "missing bool conversion: {cs}"
        );
        assert!(cs.contains("public Role Role"), "missing Role: {cs}");
        assert!(
            cs.contains("(Role)NativeMethods.weaveffi_contacts_Contact_get_role(_handle)"),
            "missing enum cast: {cs}"
        );
    }

    #[test]
    fn struct_has_dispose_and_finalizer() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public void Dispose()"),
            "missing Dispose: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_Contact_destroy(_handle)"),
            "missing destroy call: {cs}"
        );
        assert!(
            cs.contains("GC.SuppressFinalize(this)"),
            "missing SuppressFinalize: {cs}"
        );
        assert!(cs.contains("~Contact()"), "missing finalizer: {cs}");
    }

    #[test]
    fn struct_pinvoke_declarations() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "first_name".into(),
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
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("weaveffi_contacts_Contact_create("),
            "missing create P/Invoke: {cs}"
        );
        assert!(
            cs.contains("EntryPoint = \"weaveffi_contacts_Contact_create\""),
            "missing create entry point: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_Contact_destroy(IntPtr ptr)"),
            "missing destroy P/Invoke: {cs}"
        );
        assert!(
            cs.contains("IntPtr weaveffi_contacts_Contact_get_first_name(IntPtr ptr)"),
            "missing first_name getter P/Invoke: {cs}"
        );
        assert!(
            cs.contains("int weaveffi_contacts_Contact_get_age(IntPtr ptr)"),
            "missing age getter P/Invoke: {cs}"
        );
    }

    #[test]
    fn string_function_uses_utf8() {
        let api = make_api(vec![Module {
            name: "text".into(),
            functions: vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::StringUtf8),
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
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("Marshal.PtrToStringUTF8(result)"),
            "missing PtrToStringUTF8: {cs}"
        );
        assert!(
            cs.contains("Marshal.StringToCoTaskMemUTF8(msg)"),
            "missing StringToCoTaskMemUTF8: {cs}"
        );
        assert!(
            cs.contains("Marshal.FreeCoTaskMem(msgPtr)"),
            "missing FreeCoTaskMem: {cs}"
        );
        assert!(
            cs.contains("weaveffi_free_string(result)"),
            "missing free_string: {cs}"
        );
    }

    #[test]
    fn safe_cs_name_escapes_keywords() {
        assert_eq!(safe_cs_name("string"), "@string");
        assert_eq!(safe_cs_name("class"), "@class");
        assert_eq!(safe_cs_name("return"), "@return");
        assert_eq!(safe_cs_name("name"), "name");
        assert_eq!(safe_cs_name("id"), "id");
    }

    #[test]
    fn native_methods_class() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("internal static class NativeMethods"),
            "missing NativeMethods: {cs}"
        );
        assert!(
            cs.contains("weaveffi_free_string"),
            "missing free_string P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_free_bytes"),
            "missing free_bytes P/Invoke: {cs}"
        );
    }

    #[test]
    fn pinvoke_has_error_param() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("ref WeaveffiError err"),
            "missing error param in P/Invoke: {cs}"
        );
    }

    #[test]
    fn header_has_using_statements() {
        let api = make_api(vec![]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(cs.contains("using System;"), "missing System: {cs}");
        assert!(
            cs.contains("using System.Runtime.InteropServices;"),
            "missing InteropServices: {cs}"
        );
        assert!(
            cs.contains("using System.Collections.Generic;"),
            "missing Collections.Generic: {cs}"
        );
    }

    #[test]
    fn optional_types() {
        assert_eq!(cs_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "int?");
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Bool))),
            "bool?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "string?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Enum("Foo".into())))),
            "Foo?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Struct("Bar".into())))),
            "Bar?"
        );
    }

    #[test]
    fn struct_return_wraps_in_class() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
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
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("new Contact(result)"),
            "missing struct wrapping: {cs}"
        );
        assert!(
            cs.contains("public static Contact GetContact(ulong id)"),
            "missing method sig: {cs}"
        );
    }

    #[test]
    fn list_return_type() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "get_ids".into(),
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
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public static int[] GetIds()"),
            "missing list return method: {cs}"
        );
        assert!(cs.contains("out var outLen"), "missing outLen: {cs}");
        assert!(
            cs.contains("Marshal.Copy(result, arr, 0, (int)outLen)"),
            "missing Marshal.Copy: {cs}"
        );
    }

    #[test]
    fn map_return_type() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "get_scores".into(),
                params: vec![],
                returns: Some(TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64))),
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
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public static Dictionary<int, double> GetScores()"),
            "missing map return: {cs}"
        );
        assert!(cs.contains("out var outKeys"), "missing outKeys: {cs}");
        assert!(cs.contains("out var outValues"), "missing outValues: {cs}");
        assert!(
            cs.contains("new Dictionary<int, double>()"),
            "missing dict creation: {cs}"
        );
    }

    #[test]
    fn struct_optional_string_getter() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public string? Email"),
            "missing optional string getter: {cs}"
        );
        assert!(
            cs.contains("if (ptr == IntPtr.Zero) return null"),
            "missing null check: {cs}"
        );
        assert!(
            cs.contains("WeaveFFIHelpers.PtrToString(ptr)"),
            "missing UTF8 unmarshal: {cs}"
        );
    }

    #[test]
    fn optional_string_param_marshalling() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "create".into(),
                params: vec![
                    Param {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    },
                ],
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
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("StringToCoTaskMemUTF8(name)"),
            "missing name marshal: {cs}"
        );
        assert!(
            cs.contains("email != null ? Marshal.StringToCoTaskMemUTF8(email) : IntPtr.Zero"),
            "missing optional string marshal: {cs}"
        );
        assert!(
            cs.contains("FreeCoTaskMem(namePtr)"),
            "missing name cleanup: {cs}"
        );
        assert!(
            cs.contains("emailPtr != IntPtr.Zero"),
            "missing optional cleanup guard: {cs}"
        );
    }

    #[test]
    fn comprehensive_contacts_api() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
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
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
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
                builder: false,
            }],
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
                            doc: None,
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
                        doc: None,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
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
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_contacts_v2");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir).unwrap();

        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

        assert!(cs.contains("public enum ContactType"));
        assert!(cs.contains("Personal = 0"));
        assert!(cs.contains("Work = 1"));

        assert!(cs.contains("public class Contact : IDisposable"));
        assert!(cs.contains("private IntPtr _handle"));
        assert!(cs.contains("weaveffi_contacts_Contact_destroy(_handle)"));
        assert!(cs.contains("GC.SuppressFinalize(this)"));

        assert!(cs.contains("public long Id"));
        assert!(cs.contains("public string FirstName"));
        assert!(cs.contains("public string? Email"));

        assert!(cs.contains("weaveffi_contacts_Contact_create("));
        assert!(cs.contains("weaveffi_contacts_Contact_destroy(IntPtr ptr)"));
        assert!(cs.contains("weaveffi_contacts_Contact_get_id(IntPtr ptr)"));
        assert!(cs.contains("weaveffi_contacts_Contact_get_first_name(IntPtr ptr)"));

        assert!(cs.contains("weaveffi_contacts_create_contact("));
        assert!(cs.contains("weaveffi_contacts_get_contact("));
        assert!(cs.contains("weaveffi_contacts_count_contacts("));

        assert!(cs.contains("public static class Contacts"));
        assert!(cs.contains("public static ulong CreateContact("));
        assert!(cs.contains("public static Contact GetContact("));
        assert!(cs.contains("public static int CountContacts("));

        assert!(cs.contains("internal static class NativeMethods"));
        assert!(cs.contains("CallingConvention.Cdecl"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dotnet_basic() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_basic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir).unwrap();
        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

        assert!(
            cs.contains("EntryPoint = \"weaveffi_math_add\""),
            "missing P/Invoke EntryPoint: {cs}"
        );
        assert!(
            cs.contains(
                "internal static extern int weaveffi_math_add(int a, int b, ref WeaveffiError err)"
            ),
            "missing P/Invoke declaration: {cs}"
        );
        assert!(
            cs.contains("public static int Add(int a, int b)"),
            "missing wrapper method signature: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_math_add(a, b, ref err)"),
            "missing P/Invoke call in wrapper: {cs}"
        );
        assert!(
            cs.contains("WeaveffiError.Check(err)"),
            "missing error check in wrapper: {cs}"
        );
        assert!(
            cs.contains("return result;"),
            "missing return statement: {cs}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dotnet_with_structs() {
        let api = make_api(vec![Module {
            name: "crm".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Person".into(),
                doc: Some("A person record".into()),
                fields: vec![
                    StructField {
                        name: "full_name".into(),
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
                    StructField {
                        name: "score".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "active".into(),
                        ty: TypeRef::Bool,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);

        assert!(
            cs.contains("public class Person : IDisposable"),
            "missing IDisposable class: {cs}"
        );
        assert!(
            cs.contains("<summary>A person record</summary>"),
            "missing doc summary: {cs}"
        );
        assert!(
            cs.contains("internal Person(IntPtr handle)"),
            "missing internal constructor: {cs}"
        );
        assert!(
            cs.contains("internal IntPtr Handle => _handle"),
            "missing Handle property: {cs}"
        );

        assert!(
            cs.contains("public string FullName"),
            "missing FullName getter: {cs}"
        );
        assert!(cs.contains("public int Age"), "missing Age getter: {cs}");
        assert!(
            cs.contains("public double Score"),
            "missing Score getter: {cs}"
        );
        assert!(
            cs.contains("public bool Active"),
            "missing Active getter: {cs}"
        );

        assert!(
            cs.contains("weaveffi_crm_Person_get_full_name(_handle)"),
            "missing full_name native call: {cs}"
        );
        assert!(
            cs.contains("weaveffi_crm_Person_get_age(_handle)"),
            "missing age native call: {cs}"
        );
        assert!(
            cs.contains("weaveffi_crm_Person_get_active(_handle) != 0"),
            "missing bool getter conversion: {cs}"
        );

        assert!(
            cs.contains("public void Dispose()"),
            "missing Dispose: {cs}"
        );
        assert!(
            cs.contains("weaveffi_crm_Person_destroy(_handle)"),
            "missing destroy in Dispose: {cs}"
        );
        assert!(
            cs.contains("GC.SuppressFinalize(this)"),
            "missing SuppressFinalize: {cs}"
        );
        assert!(cs.contains("~Person()"), "missing finalizer: {cs}");

        assert!(
            cs.contains("weaveffi_crm_Person_create("),
            "missing create P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_crm_Person_destroy(IntPtr ptr)"),
            "missing destroy P/Invoke: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_with_enums() {
        let api = make_api(vec![Module {
            name: "status".into(),
            functions: vec![Function {
                name: "get_status".into(),
                params: vec![],
                returns: Some(TypeRef::Enum("Priority".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Priority".into(),
                doc: Some("Task priority levels".into()),
                variants: vec![
                    EnumVariant {
                        name: "Low".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Medium".into(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "High".into(),
                        value: 2,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Critical".into(),
                        value: 3,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);

        assert!(
            cs.contains("<summary>Task priority levels</summary>"),
            "missing enum doc: {cs}"
        );
        assert!(
            cs.contains("public enum Priority"),
            "missing enum declaration: {cs}"
        );
        assert!(cs.contains("Low = 0,"), "missing Low variant: {cs}");
        assert!(cs.contains("Medium = 1,"), "missing Medium variant: {cs}");
        assert!(cs.contains("High = 2,"), "missing High variant: {cs}");
        assert!(
            cs.contains("Critical = 3,"),
            "missing Critical variant: {cs}"
        );

        assert!(
            cs.contains("(Priority)result"),
            "missing enum return cast: {cs}"
        );
        assert!(
            cs.contains("public static Priority GetStatus()"),
            "missing wrapper returning enum: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_with_optionals() {
        let api = make_api(vec![Module {
            name: "config".into(),
            functions: vec![Function {
                name: "update".into(),
                params: vec![
                    Param {
                        name: "label".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "count".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I64))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Settings".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "nickname".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "max_retries".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "threshold".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::F64)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "enabled".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Bool)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);

        assert!(
            cs.contains("public static long? Update(string? label, int? count)"),
            "missing Nullable wrapper sig: {cs}"
        );
        assert!(
            cs.contains("if (result == IntPtr.Zero) return null;"),
            "missing null check for optional return: {cs}"
        );
        assert!(
            cs.contains("Marshal.ReadInt64(result)"),
            "missing ReadInt64 for optional i64 return: {cs}"
        );

        assert!(
            cs.contains("public string? Nickname"),
            "missing optional string getter: {cs}"
        );
        assert!(
            cs.contains("public int? MaxRetries"),
            "missing optional int getter: {cs}"
        );
        assert!(
            cs.contains("public double? Threshold"),
            "missing optional f64 getter: {cs}"
        );
        assert!(
            cs.contains("public bool? Enabled"),
            "missing optional bool getter: {cs}"
        );

        assert!(
            cs.contains("Marshal.ReadInt32(ptr) != 0"),
            "missing optional bool unmarshal: {cs}"
        );
        assert!(
            cs.contains("BitConverter.Int64BitsToDouble(Marshal.ReadInt64(ptr))"),
            "missing optional f64 unmarshal: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_with_lists() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![
                Function {
                    name: "get_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_values".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::F64))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_timestamps".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I64))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![StructDef {
                name: "Record".into(),
                doc: None,
                fields: vec![StructField {
                    name: "tags".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(&api, "WeaveFFI", true);

        assert!(
            cs.contains("public static int[] GetIds()"),
            "missing int[] return: {cs}"
        );
        assert!(
            cs.contains("public static double[] GetValues()"),
            "missing double[] return: {cs}"
        );
        assert!(
            cs.contains("public static long[] GetTimestamps()"),
            "missing long[] return: {cs}"
        );
        assert!(
            cs.contains("out var outLen"),
            "missing outLen parameter: {cs}"
        );
        assert!(
            cs.contains("Marshal.Copy(result, arr, 0, (int)outLen)"),
            "missing Marshal.Copy for array: {cs}"
        );
        assert!(
            cs.contains("Array.Empty<int>()"),
            "missing empty array fallback for int: {cs}"
        );

        assert!(
            cs.contains("public int[] Tags"),
            "missing list struct getter: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_full_contacts() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            enums: vec![EnumDef {
                name: "ContactType".into(),
                doc: Some("Type of contact".into()),
                variants: vec![
                    EnumVariant {
                        name: "Personal".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Business".into(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Government".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact entry".into()),
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::Handle,
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
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "active".into(),
                        ty: TypeRef::Bool,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "scores".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
                            doc: None,
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
                        doc: None,
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
                    params: vec![Param {
                        name: "contact_type".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Enum("ContactType".into()))),
                        mutable: false,
                        doc: None,
                    }],
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
                        doc: None,
                    }],
                    returns: None,
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
            errors: None,
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_full_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir).unwrap();
        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

        // Enum
        assert!(cs.contains("public enum ContactType"), "missing enum: {cs}");
        assert!(cs.contains("Personal = 0,"), "missing Personal: {cs}");
        assert!(cs.contains("Business = 1,"), "missing Business: {cs}");
        assert!(cs.contains("Government = 2,"), "missing Government: {cs}");
        assert!(
            cs.contains("<summary>Type of contact</summary>"),
            "missing enum doc: {cs}"
        );

        // Struct class with IDisposable
        assert!(
            cs.contains("public class Contact : IDisposable"),
            "missing IDisposable: {cs}"
        );
        assert!(
            cs.contains("<summary>A contact entry</summary>"),
            "missing struct doc: {cs}"
        );
        assert!(
            cs.contains("internal Contact(IntPtr handle)"),
            "missing constructor: {cs}"
        );
        assert!(cs.contains("~Contact()"), "missing finalizer: {cs}");
        assert!(
            cs.contains("weaveffi_contacts_Contact_destroy(_handle)"),
            "missing destroy: {cs}"
        );

        // Property getters
        assert!(cs.contains("public ulong Id"), "missing Id getter: {cs}");
        assert!(
            cs.contains("public string FirstName"),
            "missing FirstName: {cs}"
        );
        assert!(
            cs.contains("public string LastName"),
            "missing LastName: {cs}"
        );
        assert!(
            cs.contains("public string? Email"),
            "missing optional Email: {cs}"
        );
        assert!(cs.contains("public int Age"), "missing Age: {cs}");
        assert!(cs.contains("public bool Active"), "missing Active: {cs}");
        assert!(
            cs.contains("public ContactType ContactType"),
            "missing ContactType getter: {cs}"
        );
        assert!(
            cs.contains("public int[] Scores"),
            "missing Scores list getter: {cs}"
        );

        // P/Invoke declarations
        assert!(
            cs.contains("weaveffi_contacts_Contact_create("),
            "missing create P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_Contact_destroy(IntPtr ptr)"),
            "missing destroy P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_create_contact("),
            "missing create_contact P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_get_contact("),
            "missing get_contact P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_list_contacts("),
            "missing list_contacts P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_delete_contact("),
            "missing delete_contact P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_count_contacts("),
            "missing count_contacts P/Invoke: {cs}"
        );

        // Wrapper class
        assert!(
            cs.contains("public static class Contacts"),
            "missing Contacts wrapper class: {cs}"
        );
        assert!(
            cs.contains("public static ulong CreateContact("),
            "missing CreateContact wrapper: {cs}"
        );
        assert!(
            cs.contains("public static Contact GetContact(ulong id)"),
            "missing GetContact wrapper: {cs}"
        );
        assert!(
            cs.contains("public static Contact[] ListContacts("),
            "missing ListContacts wrapper: {cs}"
        );
        assert!(
            cs.contains("public static void DeleteContact(ulong id)"),
            "missing DeleteContact wrapper: {cs}"
        );
        assert!(
            cs.contains("public static int CountContacts()"),
            "missing CountContacts wrapper: {cs}"
        );

        // Supporting output files
        assert!(
            tmp.join("dotnet/WeaveFFI.csproj").exists(),
            ".csproj must exist"
        );
        assert!(
            tmp.join("dotnet/WeaveFFI.nuspec").exists(),
            ".nuspec must exist"
        );
        assert!(tmp.join("dotnet/README.md").exists(), "README must exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dotnet_has_memory_helpers() {
        let api = make_api(vec![simple_module(vec![])]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("internal static class WeaveFFIHelpers"),
            "missing WeaveFFIHelpers class: {cs}"
        );
        assert!(
            cs.contains("internal static IntPtr StringToPtr(string? s)"),
            "missing StringToPtr: {cs}"
        );
        assert!(
            cs.contains("internal static string? PtrToString(IntPtr ptr)"),
            "missing PtrToString: {cs}"
        );
        assert!(
            cs.contains("internal static void FreePtr(IntPtr ptr)"),
            "missing FreePtr: {cs}"
        );
        assert!(
            cs.contains("Marshal.StringToCoTaskMemUTF8(s)"),
            "missing StringToCoTaskMemUTF8 in helper: {cs}"
        );
        assert!(
            cs.contains("Marshal.FreeCoTaskMem(ptr)"),
            "missing FreeCoTaskMem in helper: {cs}"
        );
    }

    #[test]
    fn dotnet_custom_namespace() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let config = GeneratorConfig {
            dotnet_namespace: Some("MyCompany.Bindings".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_custom_ns");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let cs_path = tmp.join("dotnet/MyCompany.Bindings.cs");
        assert!(
            cs_path.exists(),
            ".cs file should use custom namespace name"
        );
        let cs = std::fs::read_to_string(&cs_path).unwrap();
        assert!(
            cs.contains("namespace MyCompany.Bindings"),
            "namespace should use custom name: {cs}"
        );

        let csproj_path = tmp.join("dotnet/MyCompany.Bindings.csproj");
        assert!(csproj_path.exists(), ".csproj should use custom namespace");
        let csproj = std::fs::read_to_string(&csproj_path).unwrap();
        assert!(
            csproj.contains("<PackageId>MyCompany.Bindings</PackageId>"),
            "PackageId should use custom namespace: {csproj}"
        );

        let nuspec_path = tmp.join("dotnet/MyCompany.Bindings.nuspec");
        assert!(nuspec_path.exists(), ".nuspec should use custom namespace");
        let nuspec = std::fs::read_to_string(&nuspec_path).unwrap();
        assert!(
            nuspec.contains("<id>MyCompany.Bindings</id>"),
            "nuspec id should use custom namespace: {nuspec}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dotnet_strip_module_prefix() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "create_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
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
        }]);

        let config = GeneratorConfig {
            strip_module_prefix: true,
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(
            cs.contains("CreateContact("),
            "stripped name should be CreateContact: {cs}"
        );
        assert!(
            !cs.contains("ContactsCreateContact("),
            "should not contain module-prefixed name: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {cs}"
        );

        let no_strip = GeneratorConfig::default();
        let tmp2 = std::env::temp_dir().join("weaveffi_test_dotnet_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        DotnetGenerator
            .generate_with_config(&api, out_dir2, &no_strip)
            .unwrap();

        let cs2 = std::fs::read_to_string(tmp2.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(
            cs2.contains("ContactsCreateContact("),
            "default should use module-prefixed name: {cs2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn dotnet_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
                    mutable: false,
                    doc: None,
                }],
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
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("Contact?[]?"),
            "should contain deeply nested optional type: {cs}"
        );
    }

    #[test]
    fn dotnet_map_of_lists() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "scores".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                    ),
                    mutable: false,
                    doc: None,
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
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("Dictionary<string, int[]>"),
            "should contain map of lists type: {cs}"
        );
    }

    #[test]
    fn dotnet_enum_keyed_map() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "contacts".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".into())),
                        Box::new(TypeRef::Struct("Contact".into())),
                    ),
                    mutable: false,
                    doc: None,
                }],
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
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
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
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("Dictionary<Color, Contact>"),
            "should contain enum-keyed map type: {cs}"
        );
    }

    #[test]
    fn dotnet_typed_handle_type() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_info".into(),
                    params: vec![Param {
                        name: "contact".into(),
                        ty: TypeRef::TypedHandle("Contact".into()),
                        mutable: false,
                        doc: None,
                    }],
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
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("Contact contact"),
            "TypedHandle should use class type not ulong: {cs}"
        );
    }

    #[test]
    fn dotnet_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
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
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("StringToCoTaskMemUTF8"),
            "string param should be marshalled to unmanaged memory: {cs}"
        );
        assert!(
            cs.contains("finally") && cs.contains("FreeCoTaskMem"),
            "marshalled string should be freed in finally (no double-free of managed string): {cs}"
        );
        let find = cs.find("FindContact").expect("FindContact wrapper");
        let slice = &cs[find..];
        let check_rel = slice
            .find("WeaveffiError.Check(err)")
            .expect("WeaveffiError.Check in FindContact");
        let ret_rel = slice
            .find("return new Contact(result)")
            .expect("return new Contact(result) in FindContact");
        assert!(
            check_rel < ret_rel,
            "error must be checked before wrapping result: {cs}"
        );
        assert!(
            cs.contains("public class Contact : IDisposable"),
            "struct return should be disposable: {cs}"
        );
        assert!(
            cs.contains("~Contact()"),
            "Contact should have finalizer for dispose pattern: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_Contact_destroy"),
            "Dispose should call native destroy: {cs}"
        );
    }

    #[test]
    fn dotnet_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
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
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("result == IntPtr.Zero ? null : new Contact(result)"),
            "optional struct return should null-check before wrap: {cs}"
        );
    }

    #[test]
    fn dotnet_async_returns_task() {
        let api = make_api(vec![Module {
            name: "tasks".into(),
            functions: vec![Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
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
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("async Task<"),
            "missing async Task< in signature: {cs}"
        );
    }

    #[test]
    fn dotnet_async_uses_tcs() {
        let api = make_api(vec![Module {
            name: "tasks".into(),
            functions: vec![Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
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
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("TaskCompletionSource"),
            "missing TaskCompletionSource: {cs}"
        );
    }

    #[test]
    fn dotnet_nested_module_output() {
        let api = make_api(vec![Module {
            name: "parent".to_string(),
            functions: vec![Function {
                name: "outer_fn".to_string(),
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
            modules: vec![Module {
                name: "child".to_string(),
                functions: vec![Function {
                    name: "inner_fn".to_string(),
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
        }]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("public static class Parent"),
            "top-level wrapper class missing: {cs}"
        );
        assert!(
            cs.contains("public static class Child"),
            "nested wrapper class missing: {cs}"
        );
        assert!(
            cs.contains("weaveffi_parent_outer_fn"),
            "parent P/Invoke missing: {cs}"
        );
        assert!(
            cs.contains("weaveffi_parent_child_inner_fn"),
            "nested child P/Invoke missing: {cs}"
        );
    }

    #[test]
    fn deprecated_function_generates_annotation() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add_old".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: Some("Use AddV2 instead".into()),
            since: Some("0.1.0".into()),
        }])]);
        let cs = render_csharp(&api, "WeaveFFI", true);
        assert!(
            cs.contains("[Obsolete(\"Use AddV2 instead\")]"),
            "missing Obsolete attribute: {cs}"
        );
    }

    fn doc_api() -> Api {
        make_api(vec![Module {
            name: "docs".into(),
            functions: vec![Function {
                name: "do_thing".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: Some("the input value".into()),
                }],
                returns: Some(TypeRef::I32),
                doc: Some("Performs a thing.".into()),
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: Some("An item we track.".into()),
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: Some("Stable id".into()),
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Kind".into(),
                doc: Some("Kind of item.".into()),
                variants: vec![EnumVariant {
                    name: "Small".into(),
                    value: 0,
                    doc: Some("A small one".into()),
                }],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn dotnet_emits_doc_on_function() {
        let cs = render_csharp(&doc_api(), "WeaveFFI", true);
        assert!(
            cs.contains("/// <summary>Performs a thing.</summary>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_emits_doc_on_struct() {
        let cs = render_csharp(&doc_api(), "WeaveFFI", true);
        assert!(
            cs.contains("/// <summary>An item we track.</summary>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_emits_doc_on_enum_variant() {
        let cs = render_csharp(&doc_api(), "WeaveFFI", true);
        assert!(cs.contains("/// <summary>Kind of item.</summary>"), "{cs}");
        assert!(cs.contains("/// <summary>A small one</summary>"), "{cs}");
    }

    #[test]
    fn dotnet_emits_doc_on_field() {
        let cs = render_csharp(&doc_api(), "WeaveFFI", true);
        assert!(cs.contains("/// <summary>Stable id</summary>"), "{cs}");
    }

    #[test]
    fn dotnet_emits_doc_on_param() {
        let cs = render_csharp(&doc_api(), "WeaveFFI", true);
        assert!(
            cs.contains("/// <param name=\"x\">the input value</param>"),
            "{cs}"
        );
    }
}
