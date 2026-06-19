//! .NET (P/Invoke) binding generator for WeaveFFI.
//!
//! Emits a C# project (`.csproj` + `.nuspec`) with P/Invoke declarations
//! and idiomatic wrappers over the C ABI. Async functions surface as
//! `Task<T>`-returning methods. Implements [`LanguageBackend`]; the shared
//! driver bridges it into the generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::ToUpperCamelCase;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use weaveffi_core::abi::{self, AbiParam, CType};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, FieldBinding, FnBinding,
    IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, RichVariantBinding,
    StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, Module, TypeRef};

/// Per-target configuration for [`DotnetGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DotnetConfig {
    /// C# namespace (and on-disk basename used for `.cs`/`.csproj`/`.nuspec`).
    /// Defaults to `"WeaveFFI"`.
    pub namespace: Option<String>,
    /// When `true`, strip the IR module name prefix from emitted C# method
    /// names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the P/Invoke bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl DotnetConfig {
    /// Returns the configured C# namespace, falling back to `"WeaveFFI"`.
    pub fn namespace(&self) -> &str {
        self.namespace.as_deref().unwrap_or("WeaveFFI")
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

/// .NET backend: emits a C# project (`.csproj` and `.nuspec`) of P/Invoke
/// declarations and idiomatic wrappers over the C ABI exposed by the
/// underlying cdylib.
pub struct DotnetGenerator;

impl LanguageBackend for DotnetGenerator {
    type Config = DotnetConfig;

    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        api: &Api,
        _model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let namespace = config.namespace();
        let input_basename = config.input_basename();
        let package = resolve_dotnet_package(api, config);
        let dir = out_dir.join("dotnet");
        let cs_filename = format!("{namespace}.cs");
        let csproj_filename = format!("{namespace}.csproj");
        let nuspec_filename = format!("{namespace}.nuspec");
        vec![
            OutputFile::new(
                dir.join(&cs_filename),
                render_csharp(
                    api,
                    namespace,
                    config.strip_module_prefix,
                    config.prefix(),
                    input_basename,
                    &cs_filename,
                ),
            ),
            OutputFile::new(
                dir.join(&csproj_filename),
                render_csproj(&package, input_basename, &csproj_filename),
            ),
            OutputFile::new(
                dir.join(&nuspec_filename),
                render_nuspec(&package, input_basename, &nuspec_filename),
            ),
            OutputFile::new(
                dir.join("README.md"),
                render_readme(&package, input_basename),
            ),
        ]
    }

    fn package(
        &self,
        api: &Api,
        _model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let namespace = config.namespace();
        let input_basename = config.input_basename();
        let package = resolve_dotnet_package(api, config);
        let dir = out_dir.join("dotnet");
        let lib_name = &ctx.binaries.lib_name;

        let cs_filename = format!("{namespace}.cs");
        let csproj_filename = format!("{namespace}.csproj");
        let nuspec_filename = format!("{namespace}.nuspec");

        // Rebind the P/Invoke library name from the WeaveFFI brand to the
        // bundled library's base name so `[DllImport]` resolves the file we
        // ship under `runtimes/<rid>/native/`.
        let cs = render_csharp(
            api,
            namespace,
            config.strip_module_prefix,
            config.prefix(),
            input_basename,
            &cs_filename,
        )
        .replace(
            "private const string LibName = \"weaveffi\";",
            &format!("private const string LibName = \"{lib_name}\";"),
        );

        let native_assets = "  <ItemGroup>\n    \
             <Content Include=\"runtimes/**\" Pack=\"true\" PackagePath=\"runtimes/\">\n      \
             <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>\n    \
             </Content>\n  </ItemGroup>\n";

        let mut files = vec![
            PackagedFile::text(dir.join(&cs_filename), cs),
            PackagedFile::text(
                dir.join(&csproj_filename),
                render_csproj_with_assets(
                    &package,
                    input_basename,
                    &csproj_filename,
                    native_assets,
                ),
            ),
            PackagedFile::text(
                dir.join(&nuspec_filename),
                render_nuspec(&package, input_basename, &nuspec_filename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(&package, ctx, input_basename),
            ),
        ];

        // Bundle each prebuilt library under the NuGet `runtimes/<rid>/native/`
        // layout NuGet auto-resolves at restore time.
        for nb in &ctx.binaries.binaries {
            let dest = dir
                .join("runtimes")
                .join(nb.platform.nuget_rid())
                .join("native")
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }

        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(DotnetGenerator);

/// Resolve the NuGet/package identity for the .NET target, applying the
/// namespace-as-name fallback when nothing else identifies the package.
fn resolve_dotnet_package(api: &Api, config: &DotnetConfig) -> ResolvedPackage {
    let namespace = config.namespace();
    let mut p = pkg::resolve(
        api,
        config.namespace.as_deref(),
        config.input_basename.as_deref(),
    );
    // The C# namespace doubles as the file basename; when nothing identifies
    // the package, keep the PascalCase brand as the NuGet id so
    // `WeaveFFI.csproj` and `<PackageId>` stay consistent.
    if api.package.is_none() && config.namespace.is_none() && config.input_basename.is_none() {
        p.name = namespace.to_string();
    }
    p
}

/// Render the README for a packaged .NET artifact, listing the bundled
/// runtime identifiers so consumers know which platforms ship prebuilt.
fn render_packaged_readme(
    package: &ResolvedPackage,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let rids: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `{}`", p.nuget_rid()))
        .collect();
    let rid_list = rids.join("\n");
    format!(
        r#"{prelude}# {name} (.NET)

Auto-generated P/Invoke bindings for the WeaveFFI native library, with a
prebuilt native library bundled for each supported runtime under `runtimes/`.

## Install

```bash
dotnet add package {name}
```

The native library loads automatically; no extra setup is required on a
bundled platform.

## Bundled runtimes

{rid_list}

## Pack

```bash
dotnet pack -c Release
```

{trailer}"#,
    )
}

fn cs_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "sbyte".into(),
        TypeRef::I16 => "short".into(),
        TypeRef::I32 => "int".into(),
        TypeRef::U8 => "byte".into(),
        TypeRef::U16 => "ushort".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::U64 => "ulong".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Handle => "ulong".into(),
        // Cross-module typed handles/enums (e.g. `kv.Store`) must surface as the
        // bare local class; the qualified IR name is not a C# type here.
        TypeRef::TypedHandle(name) => local_type_name(name).into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "byte[]".into(),
        TypeRef::Struct(name) => local_type_name(name).into(),
        TypeRef::Enum(name) => local_type_name(name).into(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::I8 => "sbyte?".into(),
            TypeRef::I16 => "short?".into(),
            TypeRef::I32 => "int?".into(),
            TypeRef::U8 => "byte?".into(),
            TypeRef::U16 => "ushort?".into(),
            TypeRef::U32 => "uint?".into(),
            TypeRef::I64 => "long?".into(),
            TypeRef::U64 => "ulong?".into(),
            TypeRef::F32 => "float?".into(),
            TypeRef::F64 => "double?".into(),
            TypeRef::Bool => "bool?".into(),
            TypeRef::Handle => "ulong?".into(),
            TypeRef::TypedHandle(name) => format!("{}?", local_type_name(name)),
            TypeRef::Enum(name) => format!("{}?", local_type_name(name)),
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
        TypeRef::I8 => "sbyte".into(),
        TypeRef::I16 => "short".into(),
        TypeRef::I32 => "int".into(),
        TypeRef::U8 => "byte".into(),
        TypeRef::U16 => "ushort".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::U64 => "ulong".into(),
        TypeRef::F32 => "float".into(),
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

/// Maps a shared ABI [`CType`] to its P/Invoke spelling. All pointers collapse
/// to `IntPtr`; `size_t` becomes `UIntPtr`. The structural lowering (which slots
/// exist, in what order) comes from [`weaveffi_core::abi`].
fn cs_pinvoke_ctype(ty: &CType) -> String {
    match ty {
        CType::Int32 | CType::Bool | CType::Enum { .. } => "int".into(),
        CType::Uint32 => "uint".into(),
        CType::Int64 => "long".into(),
        CType::Uint64 | CType::Handle => "ulong".into(),
        CType::Double => "double".into(),
        CType::Float => "float".into(),
        CType::Size => "UIntPtr".into(),
        CType::Void => "void".into(),
        CType::Int8 => "sbyte".into(),
        CType::Int16 => "short".into(),
        CType::Uint8 => "byte".into(),
        CType::Uint16 => "ushort".into(),
        CType::Char => "sbyte".into(),
        CType::Ptr { .. }
        | CType::StructTag { .. }
        | CType::CancelToken
        | CType::Error
        | CType::Named(_) => "IntPtr".into(),
    }
}

/// Renders a return out-param. C# expresses the trailing pointer level of a
/// `T*` out-slot with the `out` keyword on the pointee value type.
fn cs_out_param(p: &AbiParam) -> String {
    let pointee = match &p.ty {
        CType::Ptr { pointee, .. } => cs_pinvoke_ctype(pointee),
        other => cs_pinvoke_ctype(other),
    };
    format!("out {} {}", pointee, p.name)
}

fn pinvoke_param_list(p: &ParamBinding) -> Vec<String> {
    abi::lower_param(&p.name, &p.ty, "", false)
        .iter()
        .map(|slot| format!("{} {}", cs_pinvoke_ctype(&slot.ty), slot.name))
        .collect()
}

fn pinvoke_return_info(ty: &TypeRef) -> (String, Vec<String>) {
    match ty {
        // Map returns use an array-base `out IntPtr` convention distinct from
        // the element-typed out-slots of the shared model.
        TypeRef::Map(_, _) => (
            "void".into(),
            vec![
                "out IntPtr out_keys".into(),
                "out IntPtr out_values".into(),
                "out UIntPtr out_len".into(),
            ],
        ),
        _ => {
            let r = abi::lower_return(ty, "");
            (
                cs_pinvoke_ctype(&r.ret),
                r.out_params.iter().map(cs_out_param).collect(),
            )
        }
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn render_csproj(package: &ResolvedPackage, input_basename: &str, filename: &str) -> String {
    render_csproj_with_assets(package, input_basename, filename, "")
}

/// Render the `.csproj`, optionally injecting extra `<ItemGroup>` blocks
/// (`native_assets`) after the main `<PropertyGroup>`. The `weaveffi package`
/// path passes the `runtimes/**` native-asset item group here; `generate`
/// passes an empty string.
fn render_csproj_with_assets(
    package: &ResolvedPackage,
    input_basename: &str,
    filename: &str,
    native_assets: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, filename);
    let id = &package.name;
    let version = &package.version;
    let description = xml_escape(&package.description_or_default());
    let mut extra = format!("    <Description>{description}</Description>\n");
    if !package.authors.is_empty() {
        extra.push_str(&format!(
            "    <Authors>{}</Authors>\n",
            xml_escape(&package.authors.join(", "))
        ));
    }
    if let Some(license) = &package.license {
        extra.push_str(&format!(
            "    <PackageLicenseExpression>{}</PackageLicenseExpression>\n",
            xml_escape(license)
        ));
    }
    if let Some(url) = package.homepage.as_ref().or(package.repository.as_ref()) {
        extra.push_str(&format!(
            "    <PackageProjectUrl>{}</PackageProjectUrl>\n",
            xml_escape(url)
        ));
    }
    format!(
        r#"{prelude}<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <PackageId>{id}</PackageId>
    <Version>{version}</Version>
{extra}    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
  </PropertyGroup>
{native_assets}
</Project>

{trailer}"#,
    )
}

fn render_nuspec(package: &ResolvedPackage, input_basename: &str, filename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, filename);
    let id = &package.name;
    let version = &package.version;
    let authors = if package.authors.is_empty() {
        "WeaveFFI Contributors".to_string()
    } else {
        xml_escape(&package.authors.join(", "))
    };
    let description = xml_escape(&package.description_or_default());
    let license = package.license.clone().unwrap_or_else(|| "MIT".into());
    let project_url = package
        .homepage
        .clone()
        .or_else(|| package.repository.clone())
        .unwrap_or_else(|| "https://github.com/weavefoundry/weaveffi".into());
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
{prelude}<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{id}</id>
    <version>{version}</version>
    <authors>{authors}</authors>
    <description>{description}</description>
    <license type="expression">{license}</license>
    <projectUrl>{project_url}</projectUrl>
    <tags>ffi interop native pinvoke</tags>
  </metadata>
</package>

{trailer}"#,
    )
}

fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    format!(
        r#"{prelude}# {name} (.NET)

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

{trailer}"#,
    )
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
fn emit_fn_doc(out: &mut String, doc: &Option<String>, params: &[ParamBinding], indent: &str) {
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    let documented_params: Vec<&ParamBinding> = params
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

fn render_csharp(
    api: &Api,
    namespace: &str,
    strip_module_prefix: bool,
    prefix: &str,
    input_basename: &str,
    filename: &str,
) -> String {
    let model = BindingModel::build(api, prefix);
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    // Opt the file into the nullable annotation context so the `string?`
    // signatures (optional strings) are valid regardless of the consuming
    // project's <Nullable> setting; without this, default projects warn CS8632.
    out.push_str("#nullable enable\n\n");
    out.push_str(
        "using System;\nusing System.Collections.Generic;\nusing System.Runtime.InteropServices;\n",
    );
    if model.functions().any(|(_, f)| f.is_async) {
        out.push_str("using System.Threading.Tasks;\n");
    }
    out.push('\n');
    out.push_str(&format!("namespace {namespace}\n{{\n"));

    render_exception_class(&mut out);
    render_error_struct(&mut out);
    render_helpers_class(&mut out);

    for m in &model.modules {
        for e in &m.enums {
            // Rich (algebraic) enums are opaque-object wrappers, emitted as a
            // class mirroring a struct; only plain C-style enums map to `enum`.
            if e.is_rich() {
                render_rich_enum_class(&mut out, e);
            } else {
                render_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, s);
            render_builder_class(&mut out, s);
        }
    }

    render_native_methods(&mut out, &model);

    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();
    for m in &api.modules {
        let class_name = m.name.to_upper_camel_case();
        render_wrapper_class(
            &mut out,
            &by_path,
            m,
            &m.name,
            &class_name,
            "    ",
            strip_module_prefix,
        );
    }

    out.push_str("}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

fn render_exception_class(out: &mut String) {
    out.push_str("    public class WeaveFFIException : Exception\n    {\n");
    out.push_str("        public int Code { get; }\n\n");
    out.push_str("        public WeaveFFIException(int code, string message) : base(message)\n");
    out.push_str("        {\n");
    out.push_str("            Code = code;\n");
    out.push_str("        }\n");
    out.push_str("    }\n\n");
}

fn render_error_struct(out: &mut String) {
    out.push_str("    [StructLayout(LayoutKind.Sequential)]\n");
    out.push_str("    internal struct WeaveFFIError\n    {\n");
    out.push_str("        public int Code;\n");
    out.push_str("        public IntPtr Message;\n\n");
    out.push_str("        internal static void Check(WeaveFFIError err)\n");
    out.push_str("        {\n");
    out.push_str("            if (err.Code != 0)\n");
    out.push_str("            {\n");
    out.push_str("                var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";\n");
    out.push_str("                throw new WeaveFFIException(err.Code, msg);\n");
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

fn render_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum is not a plain C# `enum`; it surfaces as an
    // opaque-object class via `render_rich_enum_class`. Guard here so this
    // path only ever emits C-style enums.
    if e.is_rich() {
        return;
    }
    emit_doc(out, &e.doc, "    ");
    out.push_str(&format!("    public enum {}\n    {{\n", e.name));
    for v in &e.variants {
        emit_doc(out, &v.doc, "        ");
        out.push_str(&format!("        {} = {},\n", v.name, v.value));
    }
    out.push_str("    }\n\n");
}

fn render_struct_class(out: &mut String, s: &StructBinding) {
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
        render_struct_getter(out, field);
    }

    out.push_str("        public void Dispose()\n        {\n");
    out.push_str("            if (!_disposed)\n            {\n");
    out.push_str(&format!(
        "                NativeMethods.{}(_handle);\n",
        s.destroy_symbol
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

/// Render a rich (algebraic) enum as an opaque-object `IDisposable` class,
/// mirroring the struct wrapper: it owns the `IntPtr` handle and frees it via
/// the enum's `_destroy` (same Dispose + finalizer, so no double free). Surface:
/// a nested `enum Tag` + `GetTag()` reader, one static factory per variant
/// (`Shape.Circle(2.5)`) using the struct/builder error-handling convention,
/// and per-variant field accessors namespaced as `{Variant}{Field}` properties
/// that reuse the struct-getter marshalling. Because resolution leaves a
/// rich-enum reference as `TypeRef::Struct`, functions taking/returning the
/// enum flow through the existing struct param/return path unchanged.
fn render_rich_enum_class(out: &mut String, e: &EnumBinding) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };
    let name = &e.name;

    emit_doc(out, &e.doc, "    ");
    out.push_str(&format!("    public class {name} : IDisposable\n    {{\n"));
    out.push_str("        private IntPtr _handle;\n");
    out.push_str("        private bool _disposed;\n\n");
    out.push_str(&format!(
        "        internal {name}(IntPtr handle)\n        {{\n            _handle = handle;\n        }}\n\n"
    ));
    out.push_str("        internal IntPtr Handle => _handle;\n\n");

    // Nested discriminant enum + typed reader. `Tag` is a nested type, so the
    // reader is `GetTag()` (a `Tag` property would collide with the type name).
    out.push_str("        public enum Tag\n        {\n");
    for v in &e.variants {
        emit_doc(out, &v.doc, "            ");
        out.push_str(&format!("            {} = {},\n", v.name, v.value));
    }
    out.push_str("        }\n\n");
    out.push_str("        public Tag GetTag()\n        {\n");
    out.push_str(&format!(
        "            return (Tag)NativeMethods.{}(_handle);\n",
        rich.tag_symbol
    ));
    out.push_str("        }\n\n");

    // One static factory per variant.
    for v in &rich.variants {
        render_rich_variant_factory(out, name, v);
    }

    // Per-variant field accessors, namespaced by variant to avoid collisions
    // (`CircleRadius`, `RectangleWidth`, ...). Same marshalling as struct fields.
    for v in &rich.variants {
        let variant_prefix = v.name.to_upper_camel_case();
        for f in &v.fields {
            let prop_name = format!("{}{}", variant_prefix, f.name.to_upper_camel_case());
            render_field_getter(out, &prop_name, f);
        }
    }

    out.push_str("        public void Dispose()\n        {\n");
    out.push_str("            if (!_disposed)\n            {\n");
    out.push_str(&format!(
        "                NativeMethods.{}(_handle);\n",
        rich.destroy_symbol
    ));
    out.push_str("                _disposed = true;\n");
    out.push_str("            }\n");
    out.push_str("            GC.SuppressFinalize(this);\n");
    out.push_str("        }\n\n");
    out.push_str(&format!(
        "        ~{name}()\n        {{\n            Dispose();\n        }}\n"
    ));
    out.push_str("    }\n\n");
}

/// One static factory for a rich-enum variant (`Shape.Circle(double radius)`).
/// Marshals each payload field through the same helpers as a struct's `create`
/// (builder `Build()`), invoking `{tag}_{V}_new(<slots>, ref err)` and wrapping
/// the returned handle. A unit variant takes no parameters.
fn render_rich_variant_factory(out: &mut String, enum_name: &str, v: &RichVariantBinding) {
    let params: Vec<ParamBinding> = v.fields.iter().map(field_as_param).collect();
    let params_sig: Vec<String> = params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    emit_doc(out, &v.doc, "        ");
    out.push_str(&format!(
        "        public static {enum_name} {}({})\n        {{\n",
        v.name,
        params_sig.join(", ")
    ));
    out.push_str("            var err = new WeaveFFIError();\n");

    let needs_try = params.iter().any(|p| param_needs_marshal(&p.ty));
    let call_args = build_call_args(&params);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let call = format!(
        "var result = NativeMethods.{}({args_part}ref err);",
        v.create.symbol
    );

    if needs_try {
        for p in &params {
            render_marshal_setup(out, p, "            ");
        }
        out.push_str("            try\n            {\n");
        out.push_str(&format!("                {call}\n"));
        out.push_str("                WeaveFFIError.Check(err);\n");
        out.push_str(&format!(
            "                return new {enum_name}(result);\n"
        ));
        out.push_str("            }\n            finally\n            {\n");
        for p in &params {
            render_marshal_cleanup(out, p, "                ");
        }
        out.push_str("            }\n");
    } else {
        out.push_str(&format!("            {call}\n"));
        out.push_str("            WeaveFFIError.Check(err);\n");
        out.push_str(&format!("            return new {enum_name}(result);\n"));
    }
    out.push_str("        }\n\n");
}

/// The builder slot's storage type and zero-value default. Scalars start at
/// 0/false/""/empty, collections empty, optionals absent, the same contract
/// as the other backends, so unset fields lower to valid C arguments.
fn cs_field_default(ty: &TypeRef) -> (String, String) {
    let t = cs_type(ty);
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::Handle => (t, "0".into()),
        TypeRef::F32 | TypeRef::F64 => (t, "0.0".into()),
        TypeRef::Bool => (t, "false".into()),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => (t, "\"\"".into()),
        TypeRef::Bytes | TypeRef::BorrowedBytes => (t, "Array.Empty<byte>()".into()),
        TypeRef::List(inner) => {
            let elem = cs_type(inner);
            (t, format!("Array.Empty<{elem}>()"))
        }
        TypeRef::Map(_, _) => {
            let init = format!("new {t}()");
            (t, init)
        }
        TypeRef::Optional(_) => (t, "null".into()),
        TypeRef::Enum(_) => (t, "default".into()),
        // No synthesizable zero value; the With setter is the only path.
        _ => (format!("{t}?"), "null".into()),
    }
}

/// A synthetic [`ParamBinding`] so builder fields reuse the function-parameter
/// marshalling helpers (`render_marshal_setup` / `build_call_args` / cleanup).
fn field_as_param(field: &FieldBinding) -> ParamBinding {
    ParamBinding {
        name: field.name.clone(),
        ty: field.ty.clone(),
        mutable: false,
        doc: None,
        abi: Vec::new(),
    }
}

fn render_builder_class(out: &mut String, s: &StructBinding) {
    if s.builder.is_none() {
        return;
    }
    let builder_name = format!("{}Builder", s.name);
    emit_doc(out, &s.doc, "    ");
    out.push_str(&format!("    public class {builder_name}\n    {{\n"));
    for field in &s.fields {
        let (storage, default) = cs_field_default(&field.ty);
        let fname = safe_cs_name(&field.name);
        out.push_str(&format!(
            "        private {storage} _{fname} = {default};\n"
        ));
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
    // Build: marshal every field into the struct's C `create` call with the
    // same lowering used for function parameters, then wrap the handle.
    out.push_str(&format!(
        "        public {name} Build()\n        {{\n",
        name = s.name
    ));
    out.push_str("            var err = new WeaveFFIError();\n");
    let params: Vec<ParamBinding> = s.fields.iter().map(field_as_param).collect();
    for p in &params {
        let fname = safe_cs_name(&p.name);
        out.push_str(&format!("            var {fname} = _{fname};\n"));
    }
    let needs_try = params.iter().any(|p| param_needs_marshal(&p.ty));
    let call_args = build_call_args(&params);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let call = format!(
        "var result = NativeMethods.{}({args_part}ref err);",
        s.create.symbol
    );
    if needs_try {
        for p in &params {
            render_marshal_setup(out, p, "            ");
        }
        out.push_str("            try\n            {\n");
        out.push_str(&format!("                {call}\n"));
        out.push_str("                WeaveFFIError.Check(err);\n");
        out.push_str(&format!("                return new {}(result);\n", s.name));
        out.push_str("            }\n            finally\n            {\n");
        for p in &params {
            render_marshal_cleanup(out, p, "                ");
        }
        out.push_str("            }\n");
    } else {
        out.push_str(&format!("            {call}\n"));
        out.push_str("            WeaveFFIError.Check(err);\n");
        out.push_str(&format!("            return new {}(result);\n", s.name));
    }
    out.push_str("        }\n    }\n\n");
}

fn render_struct_getter(out: &mut String, field: &FieldBinding) {
    let prop_name = field.name.to_upper_camel_case();
    render_field_getter(out, &prop_name, field);
}

/// Emit a `public {T} {prop_name} { get { ... } }` property reading one C
/// getter (`field.getter_symbol`) over the implicit `_handle`, applying the
/// marshal-and-free convention each field type requires. Shared by struct field
/// getters and rich-enum per-variant accessors (which pass a variant-namespaced
/// `prop_name`), so both project associated data identically.
fn render_field_getter(out: &mut String, prop_name: &str, field: &FieldBinding) {
    let getter_sym = &field.getter_symbol;
    let cs = cs_type(&field.ty);

    emit_doc(out, &field.doc, "        ");
    out.push_str(&format!(
        "        public {cs} {prop_name}\n        {{\n            get\n            {{\n"
    ));

    match &field.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => {
            out.push_str(&format!(
                "                return NativeMethods.{getter_sym}(_handle);\n"
            ));
        }
        TypeRef::TypedHandle(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!(
                "                return new {cn}(NativeMethods.{getter_sym}(_handle));\n"
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
            out.push_str("                return str ?? \"\";\n");
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
            // A cross-module enum (e.g. `graphics.Unit`) is emitted as the bare
            // top-level C# type `Unit`; the cast must use that local name, not
            // the qualified IR name (there is no `graphics` namespace).
            let cn = local_type_name(name);
            out.push_str(&format!(
                "                return ({cn})NativeMethods.{getter_sym}(_handle);\n"
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
                TypeRef::I8 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return (sbyte)Marshal.ReadByte(ptr);\n");
                }
                TypeRef::U8 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return (byte)Marshal.ReadByte(ptr);\n");
                }
                TypeRef::I16 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return Marshal.ReadInt16(ptr);\n");
                }
                TypeRef::U16 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return (ushort)Marshal.ReadInt16(ptr);\n");
                }
                TypeRef::U64 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return (ulong)Marshal.ReadInt64(ptr);\n");
                }
                TypeRef::F32 => {
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str("                return BitConverter.Int32BitsToSingle(Marshal.ReadInt32(ptr));\n");
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
                    let cn = local_type_name(name);
                    out.push_str(&format!(
                        "                return ptr == IntPtr.Zero ? null : new {cn}(ptr);\n"
                    ));
                }
                TypeRef::Enum(name) => {
                    let cn = local_type_name(name);
                    out.push_str("                if (ptr == IntPtr.Zero) return null;\n");
                    out.push_str(&format!(
                        "                return ({cn})Marshal.ReadInt32(ptr);\n"
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
    render_list_decode(out, inner, "ptr", "len", indent);
}

/// Decodes a C parallel array (`base` + `len`) into a managed `T[]` and
/// returns it. Blittable scalars bulk-copy; everything else element-reads via
/// [`marshal_read_element`].
fn render_list_decode(out: &mut String, inner: &TypeRef, base: &str, len: &str, indent: &str) {
    let elem = cs_type(inner);
    out.push_str(&format!(
        "{indent}if ({base} == IntPtr.Zero) return Array.Empty<{elem}>();\n"
    ));
    match inner {
        TypeRef::I32 | TypeRef::I64 | TypeRef::F64 => {
            out.push_str(&format!("{indent}var arr = new {elem}[(int){len}];\n"));
            out.push_str(&format!(
                "{indent}Marshal.Copy({base}, arr, 0, (int){len});\n"
            ));
            out.push_str(&format!("{indent}return arr;\n"));
        }
        _ => {
            let read = marshal_read_element(inner, base, "i");
            out.push_str(&format!("{indent}var arr = new {elem}[(int){len}];\n"));
            out.push_str(&format!(
                "{indent}for (int i = 0; i < (int){len}; i++)\n{indent}{{\n"
            ));
            out.push_str(&format!("{indent}    arr[i] = {read};\n"));
            out.push_str(&format!("{indent}}}\n"));
            out.push_str(&format!("{indent}return arr;\n"));
        }
    }
}

fn render_native_methods(out: &mut String, model: &BindingModel) {
    out.push_str("    internal static class NativeMethods\n    {\n");
    out.push_str("        private const string LibName = \"weaveffi\";\n\n");

    out.push_str("        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]\n");
    out.push_str("        internal static extern void weaveffi_free_string(IntPtr ptr);\n\n");
    out.push_str("        [DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]\n");
    out.push_str(
        "        internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);\n\n",
    );

    for m in &model.modules {
        for e in &m.enums {
            // Plain enums lower by value and need no P/Invoke; rich enums need
            // the opaque-object symbol set (tag, destroy, per-variant new/get).
            if e.is_rich() {
                render_rich_enum_pinvoke(out, e);
            }
        }
        for s in &m.structs {
            render_struct_pinvoke(out, s);
        }
        for cb in &m.callbacks {
            render_callback_pinvoke(out, cb);
        }
        for l in &m.listeners {
            render_listener_pinvoke(out, l);
        }
        for f in &m.functions {
            render_function_pinvoke(out, f);
            if f.is_async {
                render_async_function_pinvoke(out, f);
            }
        }
    }

    out.push_str("    }\n\n");
}

/// The unmanaged delegate type for one module callback declaration, shared by
/// every listener that fires it.
fn render_callback_pinvoke(out: &mut String, cb: &CallbackBinding) {
    let delegate_name = format!("Cb_{}", cb.c_fn_type);
    let params: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| format!("{} {}", cs_pinvoke_ctype(&slot.ty), slot.name))
        .collect();
    out.push_str("        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]\n");
    out.push_str(&format!(
        "        internal delegate void {delegate_name}({});\n\n",
        params.join(", ")
    ));
}

fn render_listener_pinvoke(out: &mut String, l: &ListenerBinding) {
    let delegate_name = format!("Cb_{}", l.callback_c_fn_type);
    let register_sym = &l.register_symbol;
    let unregister_sym = &l.unregister_symbol;

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{register_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern ulong {register_sym}({delegate_name} callback, IntPtr context);\n\n"
    ));

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{unregister_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern void {unregister_sym}(ulong id);\n\n"
    ));
}

fn render_struct_pinvoke(out: &mut String, s: &StructBinding) {
    let create_sym = &s.create.symbol;
    let destroy_sym = &s.destroy_symbol;

    let mut create_params: Vec<String> = s
        .fields
        .iter()
        .flat_map(|f| {
            let p = ParamBinding {
                name: f.name.clone(),
                ty: f.ty.clone(),
                mutable: false,
                doc: f.doc.clone(),
                abi: Vec::new(),
            };
            pinvoke_param_list(&p)
        })
        .collect();
    create_params.push("ref WeaveFFIError err".into());

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{create_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern IntPtr {create_sym}({});\n\n",
        create_params.join(", ")
    ));

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{destroy_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern void {destroy_sym}(IntPtr ptr);\n\n"
    ));

    for field in &s.fields {
        let getter_sym = &field.getter_symbol;
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

/// Emit the `[DllImport]` set backing a rich (algebraic) enum, mirroring
/// `render_struct_pinvoke`: the `_tag` reader, the `_destroy`, and per variant a
/// `_{V}_new` constructor (field slots + `ref err`, returns the opaque `IntPtr`)
/// plus a `_{V}_get_{f}` getter per field (return/extra slots lowered exactly
/// like struct field getters: string -> `IntPtr`, bytes/list add `out UIntPtr`).
fn render_rich_enum_pinvoke(out: &mut String, e: &EnumBinding) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };

    let tag_sym = &rich.tag_symbol;
    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{tag_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern int {tag_sym}(IntPtr ptr);\n\n"
    ));

    let destroy_sym = &rich.destroy_symbol;
    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{destroy_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));
    out.push_str(&format!(
        "        internal static extern void {destroy_sym}(IntPtr ptr);\n\n"
    ));

    for v in &rich.variants {
        let new_sym = &v.create.symbol;
        let mut new_params: Vec<String> = v
            .fields
            .iter()
            .flat_map(|f| pinvoke_param_list(&field_as_param(f)))
            .collect();
        new_params.push("ref WeaveFFIError err".into());
        out.push_str(&format!(
            "        [DllImport(LibName, EntryPoint = \"{new_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
        ));
        out.push_str(&format!(
            "        internal static extern IntPtr {new_sym}({});\n\n",
            new_params.join(", ")
        ));

        for f in &v.fields {
            let getter_sym = &f.getter_symbol;
            let (ret_type, extra_params) = pinvoke_return_info(&f.ty);
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
}

fn render_function_pinvoke(out: &mut String, f: &FnBinding) {
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_pinvoke(out, it);
        return;
    }
    let c_sym = &f.c_base;

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{c_sym}\", CallingConvention = CallingConvention.Cdecl)]\n"
    ));

    let mut params: Vec<String> = f.params.iter().flat_map(pinvoke_param_list).collect();

    let ret_type = if let Some(ret) = &f.ret {
        let (ret_cs, extra) = pinvoke_return_info(ret);
        params.extend(extra);
        ret_cs
    } else {
        "void".into()
    };

    params.push("ref WeaveFFIError err".into());

    out.push_str(&format!(
        "        internal static extern {ret_type} {c_sym}({});\n\n",
        params.join(", ")
    ));
}

/// Whether an ABI slot is the trailing `{prefix}_error* out_err`.
fn is_error_slot(slot: &AbiParam) -> bool {
    matches!(&slot.ty, CType::Ptr { pointee, .. } if matches!(pointee.as_ref(), CType::Error))
}

/// One P/Invoke parameter for an iterator-shape ABI slot: the trailing error
/// slot becomes `ref WeaveFFIError`, `out_*` pointer slots become `out`
/// pointee values, everything else is passed by value.
fn iterator_slot_param(slot: &AbiParam) -> String {
    if is_error_slot(slot) {
        return format!("ref WeaveFFIError {}", slot.name);
    }
    match &slot.ty {
        CType::Ptr { .. } if slot.name.starts_with("out_") => cs_out_param(slot),
        ty => format!("{} {}", cs_pinvoke_ctype(ty), slot.name),
    }
}

/// The three entry points behind one `iter<T>` function: the constructor
/// returning the opaque iterator handle, `_next`, and `_destroy`.
fn render_iterator_pinvoke(out: &mut String, it: &IteratorBinding) {
    let launch_params: Vec<String> = it.launch.params.iter().map(iterator_slot_param).collect();
    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]\n",
        it.launch.symbol
    ));
    out.push_str(&format!(
        "        internal static extern IntPtr {}({});\n\n",
        it.launch.symbol,
        launch_params.join(", ")
    ));

    let next_params: Vec<String> = it.next.params.iter().map(iterator_slot_param).collect();
    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]\n",
        it.next.symbol
    ));
    out.push_str(&format!(
        "        internal static extern int {}({});\n\n",
        it.next.symbol,
        next_params.join(", ")
    ));

    out.push_str(&format!(
        "        [DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]\n",
        it.destroy_symbol
    ));
    out.push_str(&format!(
        "        internal static extern void {}(IntPtr iter);\n\n",
        it.destroy_symbol
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

fn render_async_function_pinvoke(out: &mut String, f: &FnBinding) {
    let c_sym = &f.c_base;
    let delegate_name = format!("AsyncCb_{c_sym}");
    let cb_params = async_cb_delegate_result_params(&f.ret);

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

/// Statements (appended to `out`) plus the expression converting one callback
/// parameter's delegate slots into the value handed to the user callback.
fn render_cb_arg(out: &mut String, p: &ParamBinding, idx: usize, indent: &str) -> String {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = safe_cs_name(&slots[0].name);
    match &p.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64 => n0,
        TypeRef::Handle => n0,
        TypeRef::Bool => format!("{n0} != 0"),
        TypeRef::Enum(name) => format!("({}){n0}", local_type_name(name)),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("Marshal.PtrToStringUTF8({n0}) ?? \"\"")
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let len = safe_cs_name(&slots[1].name);
            let arg = format!("arg{idx}");
            out.push_str(&format!(
                "{indent}var {arg} = new byte[(int){len}];\n"
            ));
            out.push_str(&format!(
                "{indent}if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}, 0, (int){len});\n"
            ));
            arg
        }
        // Borrowed for the duration of the callback; the consumer must not
        // Dispose() the wrapper.
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            format!("new {}({n0})", local_type_name(name))
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                format!("Marshal.PtrToStringUTF8({n0})")
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let len = safe_cs_name(&slots[1].name);
                let arg = format!("arg{idx}");
                out.push_str(&format!("{indent}byte[]? {arg} = null;\n"));
                out.push_str(&format!(
                    "{indent}if ({n0} != IntPtr.Zero)\n{indent}{{\n"
                ));
                out.push_str(&format!(
                    "{indent}    {arg} = new byte[(int){len}];\n"
                ));
                out.push_str(&format!(
                    "{indent}    if ((int){len} > 0) Marshal.Copy({n0}, {arg}, 0, (int){len});\n"
                ));
                out.push_str(&format!("{indent}}}\n"));
                arg
            }
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let cn = local_type_name(name);
                format!("{n0} == IntPtr.Zero ? null : new {cn}({n0})")
            }
            TypeRef::I32 => format!("{n0} == IntPtr.Zero ? (int?)null : Marshal.ReadInt32({n0})"),
            TypeRef::U32 => format!(
                "{n0} == IntPtr.Zero ? (uint?)null : (uint)Marshal.ReadInt32({n0})"
            ),
            TypeRef::I64 => {
                format!("{n0} == IntPtr.Zero ? (long?)null : Marshal.ReadInt64({n0})")
            }
            TypeRef::Handle => format!(
                "{n0} == IntPtr.Zero ? (ulong?)null : (ulong)Marshal.ReadInt64({n0})"
            ),
            TypeRef::F64 => format!(
                "{n0} == IntPtr.Zero ? (double?)null : BitConverter.Int64BitsToDouble(Marshal.ReadInt64({n0}))"
            ),
            TypeRef::I8 => {
                format!("{n0} == IntPtr.Zero ? (sbyte?)null : (sbyte)Marshal.ReadByte({n0})")
            }
            TypeRef::U8 => {
                format!("{n0} == IntPtr.Zero ? (byte?)null : (byte)Marshal.ReadByte({n0})")
            }
            TypeRef::I16 => {
                format!("{n0} == IntPtr.Zero ? (short?)null : Marshal.ReadInt16({n0})")
            }
            TypeRef::U16 => format!(
                "{n0} == IntPtr.Zero ? (ushort?)null : (ushort)Marshal.ReadInt16({n0})"
            ),
            TypeRef::U64 => format!(
                "{n0} == IntPtr.Zero ? (ulong?)null : (ulong)Marshal.ReadInt64({n0})"
            ),
            TypeRef::F32 => format!(
                "{n0} == IntPtr.Zero ? (float?)null : BitConverter.Int32BitsToSingle(Marshal.ReadInt32({n0}))"
            ),
            TypeRef::Bool => {
                format!("{n0} == IntPtr.Zero ? (bool?)null : Marshal.ReadInt32({n0}) != 0")
            }
            TypeRef::Enum(name) => {
                let cn = local_type_name(name);
                format!("{n0} == IntPtr.Zero ? ({cn}?)null : ({cn})Marshal.ReadInt32({n0})")
            }
            _ => "default".into(),
        },
        TypeRef::List(inner) => {
            let len = safe_cs_name(&slots[1].name);
            let arg = format!("arg{idx}");
            let elem = cs_type(inner);
            out.push_str(&format!(
                "{indent}var {arg} = new {elem}[(int){len}];\n"
            ));
            out.push_str(&format!(
                "{indent}if ({n0} != IntPtr.Zero)\n{indent}{{\n"
            ));
            out.push_str(&format!(
                "{indent}    for (int i = 0; i < (int){len}; i++)\n{indent}    {{\n"
            ));
            out.push_str(&format!(
                "{indent}        {arg}[i] = {};\n",
                marshal_read_element(inner, &n0, "i")
            ));
            out.push_str(&format!("{indent}    }}\n{indent}}}\n"));
            arg
        }
        TypeRef::Map(k, v) => {
            let keys = safe_cs_name(&slots[0].name);
            let vals = safe_cs_name(&slots[1].name);
            let len = safe_cs_name(&slots[2].name);
            let arg = format!("arg{idx}");
            let (k_cs, v_cs) = (cs_type(k), cs_type(v));
            out.push_str(&format!(
                "{indent}var {arg} = new Dictionary<{k_cs}, {v_cs}>();\n"
            ));
            out.push_str(&format!(
                "{indent}if ({keys} != IntPtr.Zero && {vals} != IntPtr.Zero)\n{indent}{{\n"
            ));
            out.push_str(&format!(
                "{indent}    for (int i = 0; i < (int){len}; i++)\n{indent}    {{\n"
            ));
            out.push_str(&format!(
                "{indent}        {arg}[{}] = {};\n",
                marshal_read_element(k, &keys, "i"),
                marshal_read_element(v, &vals, "i")
            ));
            out.push_str(&format!("{indent}    }}\n{indent}}}\n"));
            arg
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    }
}

/// The register/unregister method pair for one listener, emitted into the
/// module's wrapper class alongside `_listenerRefs`.
fn render_listener_methods(
    out: &mut String,
    mb: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = mb.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_name = wrapper_name(
        &mb.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let unregister_name = wrapper_name(
        &mb.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let delegate_name = format!("NativeMethods.Cb_{}", cb.c_fn_type);

    let action_type = if cb.params.is_empty() {
        "Action".to_string()
    } else {
        format!(
            "Action<{}>",
            cb.params
                .iter()
                .map(|p| cs_type(&p.ty))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let lambda_formals: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| safe_cs_name(&slot.name))
        .collect();

    emit_doc(out, &l.doc, "        ");
    out.push_str(&format!(
        "        /// <returns>A subscription id for {unregister_name}().</returns>\n"
    ));
    out.push_str(&format!(
        "        public static ulong {register_name}({action_type} callback)\n        {{\n"
    ));
    out.push_str(&format!(
        "            {delegate_name} trampoline = ({}) =>\n            {{\n",
        lambda_formals.join(", ")
    ));
    let mut stmts = String::new();
    let mut args = Vec::new();
    for (idx, p) in cb.params.iter().enumerate() {
        args.push(render_cb_arg(&mut stmts, p, idx, "                "));
    }
    out.push_str(&stmts);
    out.push_str(&format!("                callback({});\n", args.join(", ")));
    out.push_str("            };\n");
    out.push_str("            ulong id;\n");
    out.push_str("            lock (_listenerLock)\n            {\n");
    out.push_str(&format!(
        "                id = NativeMethods.{}(trampoline, IntPtr.Zero);\n",
        l.register_symbol
    ));
    out.push_str("                _listenerRefs[id] = trampoline;\n");
    out.push_str("            }\n");
    out.push_str("            return id;\n");
    out.push_str("        }\n\n");

    out.push_str(&format!(
        "        /// <summary>Unregisters a listener previously registered with {register_name}().</summary>\n"
    ));
    out.push_str(&format!(
        "        public static void {unregister_name}(ulong id)\n        {{\n"
    ));
    out.push_str(&format!(
        "            NativeMethods.{}(id);\n",
        l.unregister_symbol
    ));
    out.push_str("            lock (_listenerLock)\n            {\n");
    out.push_str("                _listenerRefs.Remove(id);\n");
    out.push_str("            }\n");
    out.push_str("        }\n\n");
}

/// Renders one module's static wrapper class, then its submodules as sibling
/// classes named by their full path (`KvStats`, not a nested `Kv.Stats`).
/// Flat classes keep generated type names (`Stats`) unambiguous: a nested
/// module class with the same name as a struct wrapper would shadow it.
fn render_wrapper_class(
    out: &mut String,
    by_path: &HashMap<&str, &ModuleBinding>,
    m: &Module,
    module_path: &str,
    class_name: &str,
    indent: &str,
    strip_module_prefix: bool,
) {
    out.push_str(&format!(
        "{indent}public static class {class_name}\n{indent}{{\n"
    ));

    let mb = by_path[module_path];
    if !mb.listeners.is_empty() {
        let mut buf = String::new();
        buf.push_str("        private static readonly object _listenerLock = new object();\n");
        buf.push_str(
            "        // Live listener delegates by subscription id. Holding the delegate\n",
        );
        buf.push_str(
            "        // here keeps its native thunk alive until unregistered; without this\n",
        );
        buf.push_str("        // the GC could collect a delegate the producer still calls.\n");
        buf.push_str(
            "        private static readonly Dictionary<ulong, Delegate> _listenerRefs = new Dictionary<ulong, Delegate>();\n\n",
        );
        for l in &mb.listeners {
            render_listener_methods(&mut buf, mb, l, strip_module_prefix);
        }
        reindent(out, &buf, indent.len().saturating_sub(4));
    }
    for f in &mb.functions {
        let mut buf = String::new();
        render_wrapper_method(&mut buf, module_path, f, strip_module_prefix);
        reindent(out, &buf, indent.len().saturating_sub(4));
    }

    out.push_str(&format!("{indent}}}\n\n"));

    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        let sub_class = format!("{class_name}{}", sub.name.to_upper_camel_case());
        render_wrapper_class(
            out,
            by_path,
            sub,
            &sub_path,
            &sub_class,
            indent,
            strip_module_prefix,
        );
    }
}

fn param_needs_marshal(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            true
        }
        TypeRef::List(_) | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => matches!(
            inner.as_ref(),
            TypeRef::StringUtf8
                | TypeRef::BorrowedStr
                | TypeRef::Bytes
                | TypeRef::BorrowedBytes
                | TypeRef::I8
                | TypeRef::I16
                | TypeRef::I32
                | TypeRef::U8
                | TypeRef::U16
                | TypeRef::U32
                | TypeRef::I64
                | TypeRef::U64
                | TypeRef::F32
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
    f: &FnBinding,
    strip_module_prefix: bool,
) {
    if f.is_async {
        render_async_wrapper_method(out, module_path, f, strip_module_prefix);
        return;
    }
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_wrapper_method(out, module_path, f, it, strip_module_prefix);
        return;
    }
    let method_name = wrapper_name(module_path, &f.name, strip_module_prefix).to_upper_camel_case();
    let ret_cs = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "void".into());

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

    out.push_str("            var err = new WeaveFFIError();\n");

    let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));

    if needs_try {
        for p in &f.params {
            render_marshal_setup(out, p, "            ");
        }
        out.push_str("            try\n            {\n");
        render_pinvoke_call_and_return(out, f, "                ");
        out.push_str("            }\n            finally\n            {\n");
        for p in &f.params {
            render_marshal_cleanup(out, p, "                ");
        }
        out.push_str("            }\n");
    } else {
        render_pinvoke_call_and_return(out, f, "            ");
    }

    out.push_str("        }\n\n");
}

/// The statements converting one `_next` out-item into the yielded C# value,
/// freeing any producer-allocated memory along the way. Returns the expression
/// to `yield return`.
fn iterator_item_conversion(out: &mut String, elem: &TypeRef, indent: &str) -> String {
    match elem {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => "out_item".into(),
        TypeRef::Bool => "out_item != 0".into(),
        TypeRef::Enum(name) => format!("({})out_item", local_type_name(name)),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}var item = Marshal.PtrToStringUTF8(out_item) ?? \"\";\n"
            ));
            out.push_str(&format!(
                "{indent}NativeMethods.weaveffi_free_string(out_item);\n"
            ));
            "item".into()
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{indent}var item = new byte[(int)out_len];\n"));
            out.push_str(&format!(
                "{indent}if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, item, 0, (int)out_len);\n"
            ));
            out.push_str(&format!(
                "{indent}NativeMethods.weaveffi_free_bytes(out_item, out_len);\n"
            ));
            "item".into()
        }
        // The consumer owns each yielded wrapper; Dispose() destroys it.
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            format!("new {}(out_item)", local_type_name(name))
        }
        _ => "default!".into(),
    }
}

/// An `iter<T>` function surfaces as `IEnumerable<T>`: an eager constructor
/// call (so launch errors throw immediately), then a lazy enumerator that
/// drives `_next` and destroys the handle when enumeration ends or the
/// enumerator is disposed.
fn render_iterator_wrapper_method(
    out: &mut String,
    module_path: &str,
    f: &FnBinding,
    it: &IteratorBinding,
    strip_module_prefix: bool,
) {
    let method_name = wrapper_name(module_path, &f.name, strip_module_prefix).to_upper_camel_case();
    let elem_cs = cs_type(&it.elem);

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
        "        public static IEnumerable<{elem_cs}> {method_name}({})\n        {{\n",
        params_sig.join(", ")
    ));
    out.push_str("            var err = new WeaveFFIError();\n");

    let call_args = build_call_args(&f.params);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let launch_call = format!(
        "var iter = NativeMethods.{}({args_part}ref err);",
        it.launch.symbol
    );

    let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
    if needs_try {
        for p in &f.params {
            render_marshal_setup(out, p, "            ");
        }
        out.push_str("            try\n            {\n");
        out.push_str(&format!("                {launch_call}\n"));
        out.push_str("                WeaveFFIError.Check(err);\n");
        out.push_str(&format!(
            "                return Enumerate{method_name}(iter);\n"
        ));
        out.push_str("            }\n            finally\n            {\n");
        for p in &f.params {
            render_marshal_cleanup(out, p, "                ");
        }
        out.push_str("            }\n");
    } else {
        out.push_str(&format!("            {launch_call}\n"));
        out.push_str("            WeaveFFIError.Check(err);\n");
        out.push_str(&format!(
            "            return Enumerate{method_name}(iter);\n"
        ));
    }
    out.push_str("        }\n\n");

    // The `_next` out-slots after the iterator handle, excluding the error.
    let next_out_args: Vec<String> = it
        .next
        .params
        .iter()
        .skip(1)
        .filter(|slot| !is_error_slot(slot))
        .map(|slot| format!("out var {}", slot.name))
        .collect();

    out.push_str(&format!(
        "        private static IEnumerable<{elem_cs}> Enumerate{method_name}(IntPtr iter)\n        {{\n"
    ));
    out.push_str("            try\n            {\n");
    out.push_str("                while (true)\n                {\n");
    out.push_str("                    var iterErr = new WeaveFFIError();\n");
    out.push_str(&format!(
        "                    if (NativeMethods.{}(iter, {}, ref iterErr) == 0)\n",
        it.next.symbol,
        next_out_args.join(", ")
    ));
    out.push_str("                    {\n");
    out.push_str("                        WeaveFFIError.Check(iterErr);\n");
    out.push_str("                        yield break;\n");
    out.push_str("                    }\n");
    out.push_str("                    WeaveFFIError.Check(iterErr);\n");
    let mut conv = String::new();
    let item_expr = iterator_item_conversion(&mut conv, &it.elem, "                    ");
    out.push_str(&conv);
    out.push_str(&format!("                    yield return {item_expr};\n"));
    out.push_str("                }\n");
    out.push_str("            }\n");
    out.push_str("            finally\n            {\n");
    out.push_str(&format!(
        "                NativeMethods.{}(iter);\n",
        it.destroy_symbol
    ));
    out.push_str("            }\n");
    out.push_str("        }\n\n");
}

fn render_async_wrapper_method(
    out: &mut String,
    module_path: &str,
    f: &FnBinding,
    strip_module_prefix: bool,
) {
    let method_name = wrapper_name(module_path, &f.name, strip_module_prefix).to_upper_camel_case();
    let c_sym = &f.c_base;
    let delegate_name = format!("NativeMethods.AsyncCb_{c_sym}");

    let task_ret = f
        .ret
        .as_ref()
        .map(|ty| format!("Task<{}>", cs_type(ty)))
        .unwrap_or_else(|| "Task".into());

    let tcs_type = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "bool".into());

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

    let cb_lambda_params = async_cb_lambda_params(&f.ret);
    out.push_str(&format!(
        "            {delegate_name} callback = {cb_lambda_params} =>\n            {{\n"
    ));

    out.push_str("                try\n                {\n");
    out.push_str("                    if (err != IntPtr.Zero)\n                    {\n");
    out.push_str(
        "                        var wErr = Marshal.PtrToStructure<WeaveFFIError>(err);\n",
    );
    out.push_str("                        if (wErr.Code != 0)\n                        {\n");
    out.push_str(
        "                            var msg = Marshal.PtrToStringUTF8(wErr.Message) ?? \"\";\n",
    );
    out.push_str(
        "                            tcs.SetException(new WeaveFFIException(wErr.Code, msg));\n",
    );
    out.push_str("                            return;\n");
    out.push_str("                        }\n");
    out.push_str("                    }\n");

    render_async_set_result(out, &f.ret, "                    ");

    out.push_str("                }\n");
    out.push_str("                finally\n                {\n");
    out.push_str("                    if (context != IntPtr.Zero)\n");
    out.push_str("                    {\n");
    out.push_str("                        GCHandle.FromIntPtr(context).Free();\n");
    out.push_str("                    }\n");
    out.push_str("                }\n");

    out.push_str("            };\n");
    out.push_str("            var gcHandle = GCHandle.Alloc(callback, GCHandleType.Normal);\n");
    out.push_str("            var ctx = GCHandle.ToIntPtr(gcHandle);\n");

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
        out.push_str("                try\n                {\n");
        out.push_str(&format!(
            "                    NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);\n"
        ));
        out.push_str("                }\n");
        out.push_str("                catch\n                {\n");
        out.push_str("                    if (gcHandle.IsAllocated) gcHandle.Free();\n");
        out.push_str("                    throw;\n");
        out.push_str("                }\n");
        out.push_str("            }\n            finally\n            {\n");
        for p in &f.params {
            render_marshal_cleanup(out, p, "                ");
        }
        out.push_str("            }\n");
    } else {
        out.push_str("            try\n            {\n");
        out.push_str(&format!(
            "                NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);\n"
        ));
        out.push_str("            }\n");
        out.push_str("            catch\n            {\n");
        out.push_str("                if (gcHandle.IsAllocated) gcHandle.Free();\n");
        out.push_str("                throw;\n");
        out.push_str("            }\n");
    }

    if f.ret.is_some() {
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
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}tcs.SetResult(({cn})result);\n"));
        }
        Some(TypeRef::Struct(name)) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}tcs.SetResult(new {cn}(result));\n"));
        }
        Some(TypeRef::TypedHandle(name)) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}tcs.SetResult(new {cn}(result));\n"));
        }
        _ => {
            out.push_str(&format!("{indent}tcs.SetResult(result);\n"));
        }
    }
}

fn render_marshal_setup(out: &mut String, p: &ParamBinding, indent: &str) {
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
            TypeRef::I64 | TypeRef::U64 | TypeRef::Handle | TypeRef::F64 => {
                out.push_str(&format!("{indent}var {name}Ptr = IntPtr.Zero;\n"));
                out.push_str(&format!("{indent}if ({name}.HasValue)\n{indent}{{\n"));
                out.push_str(&format!(
                    "{indent}    {name}Ptr = Marshal.AllocHGlobal(sizeof(long));\n"
                ));
                let val = match inner.as_ref() {
                    TypeRef::Handle => format!("(long){name}.Value"),
                    TypeRef::U64 => format!("(long){name}.Value"),
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
            TypeRef::I8 | TypeRef::U8 => {
                out.push_str(&format!("{indent}var {name}Ptr = IntPtr.Zero;\n"));
                out.push_str(&format!("{indent}if ({name}.HasValue)\n{indent}{{\n"));
                out.push_str(&format!(
                    "{indent}    {name}Ptr = Marshal.AllocHGlobal(sizeof(byte));\n"
                ));
                out.push_str(&format!(
                    "{indent}    Marshal.WriteByte({name}Ptr, (byte){name}.Value);\n"
                ));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::I16 | TypeRef::U16 => {
                out.push_str(&format!("{indent}var {name}Ptr = IntPtr.Zero;\n"));
                out.push_str(&format!("{indent}if ({name}.HasValue)\n{indent}{{\n"));
                out.push_str(&format!(
                    "{indent}    {name}Ptr = Marshal.AllocHGlobal(sizeof(short));\n"
                ));
                out.push_str(&format!(
                    "{indent}    Marshal.WriteInt16({name}Ptr, (short){name}.Value);\n"
                ));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::F32 => {
                out.push_str(&format!("{indent}var {name}Ptr = IntPtr.Zero;\n"));
                out.push_str(&format!("{indent}if ({name}.HasValue)\n{indent}{{\n"));
                out.push_str(&format!(
                    "{indent}    {name}Ptr = Marshal.AllocHGlobal(sizeof(float));\n"
                ));
                out.push_str(&format!(
                    "{indent}    Marshal.WriteInt32({name}Ptr, BitConverter.SingleToInt32Bits({name}.Value));\n"
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
        TypeRef::List(elem) => {
            render_array_marshal_setup(out, &name, &format!("{name}.Length"), elem, indent);
        }
        TypeRef::Map(k, v) => {
            // Parallel key/value arrays in dictionary iteration order.
            let (k_arr, k_conv) = cs_elem_array_slot(k, "kv.Key");
            let (v_arr, v_conv) = cs_elem_array_slot(v, "kv.Value");
            out.push_str(&format!(
                "{indent}var {name}KeysArr = new {k_arr}[{name}.Count];\n"
            ));
            out.push_str(&format!(
                "{indent}var {name}ValsArr = new {v_arr}[{name}.Count];\n"
            ));
            out.push_str(&format!("{indent}var {name}I = 0;\n"));
            out.push_str(&format!("{indent}foreach (var kv in {name})\n{indent}{{\n"));
            out.push_str(&format!("{indent}    {name}KeysArr[{name}I] = {k_conv};\n"));
            out.push_str(&format!("{indent}    {name}ValsArr[{name}I] = {v_conv};\n"));
            out.push_str(&format!("{indent}    {name}I++;\n"));
            out.push_str(&format!("{indent}}}\n"));
            out.push_str(&format!(
                "{indent}var {name}KeysPin = GCHandle.Alloc({name}KeysArr, GCHandleType.Pinned);\n"
            ));
            out.push_str(&format!(
                "{indent}var {name}ValsPin = GCHandle.Alloc({name}ValsArr, GCHandleType.Pinned);\n"
            ));
        }
        _ => {}
    }
}

/// One pinned native array for a list parameter: `{name}Arr` (the converted
/// element array) and `{name}Pin` (the pin). String elements become
/// CoTaskMem-allocated UTF-8 pointers freed in cleanup.
fn render_array_marshal_setup(
    out: &mut String,
    name: &str,
    len_expr: &str,
    elem: &TypeRef,
    indent: &str,
) {
    match elem {
        // Blittable element arrays pin in place; no conversion copy needed.
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
            out.push_str(&format!("{indent}var {name}Arr = {name};\n"));
        }
        _ => {
            let (arr_ty, conv) = cs_elem_array_slot(elem, &format!("{name}[{name}It]"));
            out.push_str(&format!(
                "{indent}var {name}Arr = new {arr_ty}[{len_expr}];\n"
            ));
            out.push_str(&format!(
                "{indent}for (var {name}It = 0; {name}It < {len_expr}; {name}It++) {name}Arr[{name}It] = {conv};\n"
            ));
        }
    }
    out.push_str(&format!(
        "{indent}var {name}Pin = GCHandle.Alloc({name}Arr, GCHandleType.Pinned);\n"
    ));
}

/// The native array slot type and per-element conversion expression for one
/// list/map element.
fn cs_elem_array_slot(elem: &TypeRef, expr: &str) -> (String, String) {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
            "IntPtr".into(),
            format!("Marshal.StringToCoTaskMemUTF8({expr})"),
        ),
        TypeRef::Enum(_) => ("int".into(), format!("(int){expr}")),
        TypeRef::Bool => ("int".into(), format!("{expr} ? 1 : 0")),
        TypeRef::I8 => ("sbyte".into(), expr.into()),
        TypeRef::I16 => ("short".into(), expr.into()),
        TypeRef::I32 => ("int".into(), expr.into()),
        TypeRef::U8 => ("byte".into(), expr.into()),
        TypeRef::U16 => ("ushort".into(), expr.into()),
        TypeRef::U32 => ("uint".into(), expr.into()),
        TypeRef::I64 => ("long".into(), expr.into()),
        TypeRef::U64 => ("ulong".into(), expr.into()),
        TypeRef::F32 => ("float".into(), expr.into()),
        TypeRef::F64 => ("double".into(), expr.into()),
        TypeRef::Handle => ("ulong".into(), expr.into()),
        // Validation (`UnsupportedElementType`) rejects other element shapes.
        _ => ("IntPtr".into(), "IntPtr.Zero".into()),
    }
}

/// True when the element conversion CoTaskMem-allocates per element (strings),
/// requiring a matching per-element free in cleanup.
fn cs_elem_allocates(elem: &TypeRef) -> bool {
    matches!(elem, TypeRef::StringUtf8 | TypeRef::BorrowedStr)
}

fn render_marshal_cleanup(out: &mut String, p: &ParamBinding, indent: &str) {
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
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::F32
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
        TypeRef::List(elem) => {
            out.push_str(&format!("{indent}{name}Pin.Free();\n"));
            if cs_elem_allocates(elem) {
                out.push_str(&format!(
                    "{indent}foreach (var {name}P in {name}Arr) Marshal.FreeCoTaskMem({name}P);\n"
                ));
            }
        }
        TypeRef::Map(k, v) => {
            out.push_str(&format!("{indent}{name}KeysPin.Free();\n"));
            out.push_str(&format!("{indent}{name}ValsPin.Free();\n"));
            if cs_elem_allocates(k) {
                out.push_str(&format!(
                    "{indent}foreach (var {name}KP in {name}KeysArr) Marshal.FreeCoTaskMem({name}KP);\n"
                ));
            }
            if cs_elem_allocates(v) {
                out.push_str(&format!(
                    "{indent}foreach (var {name}VP in {name}ValsArr) Marshal.FreeCoTaskMem({name}VP);\n"
                ));
            }
        }
        _ => {}
    }
}

fn render_pinvoke_call_and_return(out: &mut String, f: &FnBinding, indent: &str) {
    let c_sym = &f.c_base;
    let call_args = build_call_args(&f.params);

    if let Some(TypeRef::Map(k, v)) = &f.ret {
        render_map_return_call(out, c_sym, &call_args, k, v, indent);
        return;
    }

    let has_out_len = f.ret.as_ref().is_some_and(|r| {
        matches!(
            r,
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_)
        ) || matches!(
            r,
            TypeRef::Optional(inner)
                if matches!(inner.as_ref(), TypeRef::Bytes | TypeRef::BorrowedBytes)
        )
    });

    if f.ret.is_some() {
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

    out.push_str(&format!("{indent}WeaveFFIError.Check(err);\n"));

    if let Some(ret_ty) = &f.ret {
        render_return_conversion(out, ret_ty, indent);
    }
}

fn build_call_args(params: &[ParamBinding]) -> String {
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
                    | TypeRef::I8
                    | TypeRef::I16
                    | TypeRef::I32
                    | TypeRef::U8
                    | TypeRef::U16
                    | TypeRef::U32
                    | TypeRef::I64
                    | TypeRef::U64
                    | TypeRef::F32
                    | TypeRef::F64
                    | TypeRef::Bool
                    | TypeRef::Handle
                    | TypeRef::Enum(_) => vec![format!("{name}Ptr")],
                    _ => vec![name],
                },
                TypeRef::List(_) => vec![
                    format!("{name}.Length == 0 ? IntPtr.Zero : {name}Pin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}.Length"),
                ],
                TypeRef::Map(_, _) => vec![
                    format!("{name}.Count == 0 ? IntPtr.Zero : {name}KeysPin.AddrOfPinnedObject()"),
                    format!("{name}.Count == 0 ? IntPtr.Zero : {name}ValsPin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}.Count"),
                ],
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
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}return ({cn})result;\n"));
        }
        TypeRef::Struct(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}return new {cn}(result);\n"));
        }
        TypeRef::TypedHandle(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!("{indent}return new {cn}(result);\n"));
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
        TypeRef::List(inner) => {
            render_list_return(out, inner, indent);
        }
        TypeRef::Iterator(_) => unreachable!("iterator functions render via CallShape::Iterator"),
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
        TypeRef::I8 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return (sbyte)Marshal.ReadByte(result);\n"
            ));
        }
        TypeRef::U8 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!("{indent}return (byte)Marshal.ReadByte(result);\n"));
        }
        TypeRef::I16 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!("{indent}return Marshal.ReadInt16(result);\n"));
        }
        TypeRef::U16 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return (ushort)Marshal.ReadInt16(result);\n"
            ));
        }
        TypeRef::U64 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return (ulong)Marshal.ReadInt64(result);\n"
            ));
        }
        TypeRef::F32 => {
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return BitConverter.Int32BitsToSingle(Marshal.ReadInt32(result));\n"
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
            let cn = local_type_name(name);
            out.push_str(&format!(
                "{indent}return result == IntPtr.Zero ? null : new {cn}(result);\n"
            ));
        }
        TypeRef::Enum(name) => {
            let cn = local_type_name(name);
            out.push_str(&format!(
                "{indent}if (result == IntPtr.Zero) return null;\n"
            ));
            out.push_str(&format!(
                "{indent}return ({cn})Marshal.ReadInt32(result);\n"
            ));
        }
        _ => {
            out.push_str(&format!("{indent}return result;\n"));
        }
    }
}

fn render_list_return(out: &mut String, inner: &TypeRef, indent: &str) {
    render_list_decode(out, inner, "result", "outLen", indent);
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
    out.push_str(&format!("{indent}WeaveFFIError.Check(err);\n"));
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
        TypeRef::I8 => format!("(sbyte)Marshal.ReadByte({arr} + {idx} * 1)"),
        TypeRef::U8 => format!("(byte)Marshal.ReadByte({arr} + {idx} * 1)"),
        TypeRef::I16 => format!("Marshal.ReadInt16({arr} + {idx} * sizeof(short))"),
        TypeRef::U16 => {
            format!("(ushort)Marshal.ReadInt16({arr} + {idx} * sizeof(short))")
        }
        TypeRef::I32 => format!("Marshal.ReadInt32({arr} + {idx} * sizeof(int))"),
        TypeRef::U32 => {
            format!("(uint)Marshal.ReadInt32({arr} + {idx} * sizeof(int))")
        }
        TypeRef::I64 => format!("Marshal.ReadInt64({arr} + {idx} * sizeof(long))"),
        TypeRef::U64 => {
            format!("(ulong)Marshal.ReadInt64({arr} + {idx} * sizeof(long))")
        }
        TypeRef::F32 => format!(
            "BitConverter.Int32BitsToSingle(Marshal.ReadInt32({arr} + {idx} * sizeof(int)))"
        ),
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
            let cn = local_type_name(name);
            format!("new {cn}(Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size))")
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!(
                "Marshal.PtrToStringUTF8(Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size)) ?? \"\""
            )
        }
        TypeRef::Enum(name) => {
            let cn = local_type_name(name);
            format!("({cn})Marshal.ReadInt32({arr} + {idx} * sizeof(int))")
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
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    #[test]
    fn package_emits_runtimes_and_rebinds_libname() {
        use camino::Utf8Path;
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = make_api(vec![simple_module(vec![Function {
            name: "ping".into(),
            params: vec![],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::WindowsX64, "/s/windows-x64/calculator.dll");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &DotnetGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &DotnetConfig::default(),
        )
        .expect("dotnet supports packaging");

        // NuGet `runtimes/<rid>/native/` layout.
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("runtimes/osx-arm64/native/libcalculator.dylib")));
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("runtimes/win-x64/native/calculator.dll")));
        // The P/Invoke library name is rebound to the bundled base name.
        let cs = files
            .iter()
            .find(|f| f.path.as_str().ends_with(".cs"))
            .expect("C# source present");
        let FileContent::Text(src) = &cs.content else {
            panic!("C# source is text");
        };
        assert!(
            src.contains("private const string LibName = \"calculator\";"),
            "DllImport name not rebound: {src}"
        );
        let csproj = files
            .iter()
            .find(|f| f.path.as_str().ends_with(".csproj"))
            .expect("csproj present");
        let FileContent::Text(proj) = &csproj.content else {
            panic!("csproj is text");
        };
        assert!(
            proj.contains("runtimes/**"),
            "native asset item group missing: {proj}"
        );
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
            modules,
            generators: None,
            package: None,
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
        assert_eq!(Generator::name(&DotnetGenerator), "dotnet");
    }

    #[test]
    fn output_files_lists_all() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = DotnetGenerator.output_files(&api, out, &DotnetConfig::default());
        assert_eq!(
            files,
            vec![
                format!("{out}/dotnet/README.md"),
                format!("{out}/dotnet/WeaveFFI.cs"),
                format!("{out}/dotnet/WeaveFFI.csproj"),
                format!("{out}/dotnet/WeaveFFI.nuspec"),
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

        DotnetGenerator
            .generate(&api, out_dir, &DotnetConfig::default())
            .unwrap();

        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(cs.contains("namespace WeaveFFI"));
        assert!(cs.contains("DllImport"));
        assert!(cs.contains("weaveffi_math_add"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn listeners_generate_register_unregister() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnMessage".into(),
                doc: None,
                params: vec![Param {
                    name: "message".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
            }],
            listeners: vec![ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }]);
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        DotnetGenerator
            .generate(&api, out, &DotnetConfig::default())
            .unwrap();
        let cs = std::fs::read_to_string(dir.path().join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(
            cs.contains("internal delegate void Cb_weaveffi_events_OnMessage_fn"),
            "unmanaged delegate type must be declared: {cs}"
        );
        assert!(
            cs.contains("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]"),
            "delegate must use cdecl: {cs}"
        );
        assert!(
            cs.contains("internal static extern ulong weaveffi_events_register_message_listener"),
            "register pinvoke missing: {cs}"
        );
        assert!(
            cs.contains(
                "public static ulong EventsRegisterMessageListener(Action<string> callback)"
            ),
            "register wrapper missing: {cs}"
        );
        assert!(
            cs.contains("public static void EventsUnregisterMessageListener(ulong id)"),
            "unregister wrapper missing: {cs}"
        );
        assert!(
            cs.contains("_listenerRefs[id] = trampoline;"),
            "delegate must be pinned in the registry: {cs}"
        );
        assert!(
            cs.contains("Marshal.PtrToStringUTF8(message) ?? \"\""),
            "string arg must be marshaled: {cs}"
        );
    }

    #[test]
    fn dotnet_builder_generated() {
        let api = Api {
            version: "0.4.0".into(),
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
            package: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        DotnetGenerator
            .generate(&api, out, &DotnetConfig::default())
            .unwrap();
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
        // Build is FFI-backed: it calls the C create symbol, checks the
        // error, and wraps the returned handle. Unset fields default to zero
        // values rather than throwing.
        assert!(
            cs.contains("NativeMethods.weaveffi_contacts_Contact_create("),
            "missing create call: {cs}"
        );
        assert!(
            cs.contains("return new Contact(result);"),
            "missing handle wrap: {cs}"
        );
        assert!(
            cs.contains("private string _name = \"\";") && cs.contains("private int _age = 0;"),
            "missing zero defaults: {cs}"
        );
        assert!(
            !cs.contains("NotImplementedException"),
            "stub must be gone: {cs}"
        );
    }

    #[test]
    fn dotnet_generates_csproj() {
        let api = make_api(vec![simple_module(vec![])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_csproj");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate(&api, out_dir, &DotnetConfig::default())
            .unwrap();

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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
            cs.contains("WeaveFFIError.Check(err)"),
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("ref WeaveFFIError err"),
            "missing error param in P/Invoke: {cs}"
        );
    }

    #[test]
    fn header_has_using_statements() {
        let api = make_api(vec![]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Work".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
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

        DotnetGenerator
            .generate(
                &api,
                out_dir,
                &DotnetConfig {
                    strip_module_prefix: true,
                    ..DotnetConfig::default()
                },
            )
            .unwrap();

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

        DotnetGenerator
            .generate(
                &api,
                out_dir,
                &DotnetConfig {
                    strip_module_prefix: true,
                    ..DotnetConfig::default()
                },
            )
            .unwrap();
        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

        assert!(
            cs.contains("EntryPoint = \"weaveffi_math_add\""),
            "missing P/Invoke EntryPoint: {cs}"
        );
        assert!(
            cs.contains(
                "internal static extern int weaveffi_math_add(int a, int b, ref WeaveFFIError err)"
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
            cs.contains("WeaveFFIError.Check(err)"),
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Medium".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "High".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Critical".into(),
                        value: 3,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Business".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Government".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
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

        DotnetGenerator
            .generate(
                &api,
                out_dir,
                &DotnetConfig {
                    strip_module_prefix: true,
                    ..DotnetConfig::default()
                },
            )
            .unwrap();
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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

        let config = DotnetConfig {
            namespace: Some("MyCompany.Bindings".into()),
            ..DotnetConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_custom_ns");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir, &config).unwrap();

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

        let config = DotnetConfig {
            strip_module_prefix: true,
            ..DotnetConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir, &config).unwrap();

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

        let no_strip = DotnetConfig::default();
        let tmp2 = std::env::temp_dir().join("weaveffi_test_dotnet_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir2, &no_strip).unwrap();

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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("Dictionary<Color, Contact>"),
            "should contain enum-keyed map type: {cs}"
        );
    }

    #[test]
    fn dotnet_typed_handle_type() {
        let api = Api {
            version: "0.4.0".into(),
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
            package: None,
        };
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
            .find("WeaveFFIError.Check(err)")
            .expect("WeaveFFIError.Check in FindContact");
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("TaskCompletionSource"),
            "missing TaskCompletionSource: {cs}"
        );
    }

    /// `GCHandle.Alloc(callback, GCHandleType.Normal)` (the .NET equivalent
    /// of pinning the delegate so the GC won't reclaim it while the C side
    /// owns a function pointer to it) must be balanced by exactly one
    /// `GCHandle.FromIntPtr(context).Free()` in the C callback after the
    /// `TaskCompletionSource` is resolved.
    #[test]
    fn dotnet_async_pins_callback_for_lifetime() {
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("GCHandle.Alloc(callback, GCHandleType.Normal)"),
            "missing GCHandle.Alloc(..., Normal): {cs}"
        );
        assert!(
            cs.contains("GCHandle.ToIntPtr(gcHandle)"),
            "GCHandle must be passed as the C context: {cs}"
        );
        assert!(
            cs.contains("GCHandle.FromIntPtr(context).Free()"),
            "missing GCHandle.Free in callback: {cs}"
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public static class Parent"),
            "top-level wrapper class missing: {cs}"
        );
        assert!(
            cs.contains("public static class ParentChild"),
            "submodule wrapper class must be flattened to its full path: {cs}"
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
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
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
                    fields: vec![],
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
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("/// <summary>Performs a thing.</summary>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_emits_doc_on_struct() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("/// <summary>An item we track.</summary>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_emits_doc_on_enum_variant() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(cs.contains("/// <summary>Kind of item.</summary>"), "{cs}");
        assert!(cs.contains("/// <summary>A small one</summary>"), "{cs}");
    }

    #[test]
    fn dotnet_emits_doc_on_field() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(cs.contains("/// <summary>Stable id</summary>"), "{cs}");
    }

    #[test]
    fn dotnet_emits_doc_on_param() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("/// <param name=\"x\">the input value</param>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_custom_prefix_threads_to_user_symbols() {
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

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "myffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

        // User symbols pick up the configured ABI prefix...
        assert!(
            cs.contains("myffi_math_add"),
            "user symbol must honor the custom prefix: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_math_add"),
            "user symbol must not retain the default prefix: {cs}"
        );
        // ...while runtime ABI helpers stay literally `weaveffi_*`.
        assert!(
            cs.contains("weaveffi_free_string"),
            "runtime ABI helper must stay literal: {cs}"
        );
    }

    fn shapes_api() -> Api {
        let shape = EnumDef {
            name: "Shape".into(),
            doc: Some("An algebraic shape".into()),
            variants: vec![
                EnumVariant {
                    name: "Empty".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Circle".into(),
                    value: 1,
                    doc: None,
                    fields: vec![StructField {
                        name: "radius".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    }],
                },
                EnumVariant {
                    name: "Rectangle".into(),
                    value: 2,
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "width".into(),
                            ty: TypeRef::F32,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "height".into(),
                            ty: TypeRef::F32,
                            doc: None,
                            default: None,
                        },
                    ],
                },
                EnumVariant {
                    name: "Labeled".into(),
                    value: 3,
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "label".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "count".into(),
                            ty: TypeRef::U8,
                            doc: None,
                            default: None,
                        },
                    ],
                },
            ],
        };
        let channel = EnumDef {
            name: "Channel".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Red".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                },
                EnumVariant {
                    name: "Green".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        };
        make_api(vec![Module {
            name: "shapes".into(),
            functions: vec![
                Function {
                    name: "describe".into(),
                    params: vec![Param {
                        name: "shape".into(),
                        ty: TypeRef::Struct("Shape".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "scale".into(),
                    params: vec![
                        Param {
                            name: "shape".into(),
                            ty: TypeRef::Struct("Shape".into()),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "factor".into(),
                            ty: TypeRef::F64,
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Struct("Shape".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "sum_bytes".into(),
                    params: vec![Param {
                        name: "values".into(),
                        ty: TypeRef::List(Box::new(TypeRef::U8)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::U64),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![shape, channel],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn rich_enum_generates_opaque_wrapper() {
        let cs = render_csharp(
            &shapes_api(),
            "Shapes",
            false,
            "weaveffi",
            "shapes.yml",
            "Shapes.cs",
        );

        // Rich enum becomes an IDisposable opaque-object class, not a C# enum.
        assert!(
            cs.contains("public class Shape : IDisposable"),
            "rich enum must be a class: {cs}"
        );
        assert!(
            !cs.contains("public enum Shape"),
            "rich enum must not be a plain enum: {cs}"
        );
        // Plain enum is still a value enum.
        assert!(
            cs.contains("public enum Channel"),
            "plain enum must stay an enum: {cs}"
        );

        // Nested discriminant enum + typed reader.
        assert!(cs.contains("public enum Tag"), "nested Tag enum: {cs}");
        assert!(
            cs.contains("Empty = 0,") && cs.contains("Labeled = 3,"),
            "Tag values: {cs}"
        );
        assert!(
            cs.contains("public Tag GetTag()")
                && cs.contains("return (Tag)NativeMethods.weaveffi_shapes_Shape_tag(_handle);"),
            "tag reader: {cs}"
        );

        // Static factories per variant with the struct error convention.
        assert!(
            cs.contains("public static Shape Empty()"),
            "Empty factory: {cs}"
        );
        assert!(
            cs.contains("public static Shape Circle(double radius)"),
            "Circle factory: {cs}"
        );
        assert!(
            cs.contains("public static Shape Rectangle(float width, float height)"),
            "Rectangle factory: {cs}"
        );
        assert!(
            cs.contains("public static Shape Labeled(string label, byte count)"),
            "Labeled factory: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_shapes_Shape_Circle_new(radius, ref err);")
                && cs.contains("return new Shape(result);"),
            "Circle ctor call + wrap: {cs}"
        );
        // String payload factory marshals + frees in try/finally.
        assert!(
            cs.contains("Marshal.StringToCoTaskMemUTF8(label)")
                && cs.contains("Marshal.FreeCoTaskMem(labelPtr);"),
            "Labeled string marshalling: {cs}"
        );

        // Per-variant accessors, namespaced by variant.
        assert!(
            cs.contains("public double CircleRadius"),
            "CircleRadius: {cs}"
        );
        assert!(
            cs.contains("public float RectangleWidth")
                && cs.contains("public float RectangleHeight"),
            "Rectangle accessors: {cs}"
        );
        assert!(
            cs.contains("public string LabeledLabel"),
            "LabeledLabel: {cs}"
        );
        assert!(
            cs.contains("public byte LabeledCount"),
            "LabeledCount: {cs}"
        );
        // String getter frees the producer-owned buffer.
        assert!(
            cs.contains("NativeMethods.weaveffi_free_string(ptr);"),
            "string getter free: {cs}"
        );

        // Disposal frees via the enum's destroy, with a finalizer (no double free).
        assert!(
            cs.contains("NativeMethods.weaveffi_shapes_Shape_destroy(_handle);")
                && cs.contains("GC.SuppressFinalize(this);")
                && cs.contains("~Shape()"),
            "dispose + finalizer: {cs}"
        );

        // P/Invoke declarations for the full symbol set.
        for sym in [
            "internal static extern int weaveffi_shapes_Shape_tag(IntPtr ptr);",
            "internal static extern void weaveffi_shapes_Shape_destroy(IntPtr ptr);",
            "internal static extern IntPtr weaveffi_shapes_Shape_Empty_new(ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_shapes_Shape_Circle_new(double radius, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_shapes_Shape_Rectangle_new(float width, float height, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_shapes_Shape_Labeled_new(IntPtr label, byte count, ref WeaveFFIError err);",
            "internal static extern double weaveffi_shapes_Shape_Circle_get_radius(IntPtr ptr);",
            "internal static extern float weaveffi_shapes_Shape_Rectangle_get_width(IntPtr ptr);",
            "internal static extern byte weaveffi_shapes_Shape_Labeled_get_count(IntPtr ptr);",
            "internal static extern IntPtr weaveffi_shapes_Shape_Labeled_get_label(IntPtr ptr);",
        ] {
            assert!(cs.contains(sym), "missing P/Invoke `{sym}`: {cs}");
        }

        // Functions taking/returning the enum flow through the struct path:
        // pass `.Handle`, wrap returns in `new Shape(...)`.
        assert!(
            cs.contains("public static string ShapesDescribe(Shape shape)")
                && cs.contains("weaveffi_shapes_describe(shape.Handle, ref err)"),
            "describe via struct path: {cs}"
        );
        assert!(
            cs.contains("public static Shape ShapesScale(Shape shape, double factor)"),
            "scale via struct path: {cs}"
        );
        // Numerics smoke: list<u8> in, u64 out (plain function path).
        assert!(
            cs.contains("public static ulong ShapesSumBytes(byte[] values)"),
            "sum_bytes wrapper: {cs}"
        );
    }
}
