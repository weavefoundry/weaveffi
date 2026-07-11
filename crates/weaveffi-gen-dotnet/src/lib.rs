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
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, AbiParam, CType};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FieldBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
    RichVariantBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`DotnetGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DotnetConfig {
    /// C# namespace (and on-disk basename used for `.cs`/`.csproj`/`.nuspec`).
    /// Defaults to `"WeaveFFI"`.
    pub namespace: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from emitted
    /// C# method names; the per-module static class already namespaces them.
    /// Set to `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the P/Invoke bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for DotnetConfig {
    fn default() -> Self {
        Self {
            namespace: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
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
        model: &BindingModel,
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
                    model,
                    namespace,
                    config.strip_module_prefix,
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
        model: &BindingModel,
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
            model,
            namespace,
            config.strip_module_prefix,
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
        // Records and rich enums share the opaque-object wrapper class.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => local_type_name(name).into(),
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
            TypeRef::Record(name) | TypeRef::RichEnum(name) => {
                format!("{}?", local_type_name(name))
            }
            _ => format!("{}?", cs_type(inner)),
        },
        TypeRef::List(inner) => format!("{}[]", cs_type(inner)),
        TypeRef::Iterator(inner) => format!("IEnumerable<{}>", cs_type(inner)),
        TypeRef::Map(k, v) => format!("Dictionary<{}, {}>", cs_type(k), cs_type(v)),
        // Interfaces surface as their opaque-handle wrapper class; a
        // cross-module reference (`kv.Store`) uses the bare local name.
        TypeRef::Interface(name) => local_type_name(name).into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
        // C `bool` is one byte; marshalling it as `int` would read past the
        // slot in arrays and leave garbage in the upper bits of returns.
        TypeRef::Bool => "byte".into(),
        TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Bytes
        | TypeRef::BorrowedBytes
        | TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::Interface(_)
        | TypeRef::Optional(_)
        | TypeRef::List(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "IntPtr".into(),
        TypeRef::Handle => "ulong".into(),
        TypeRef::TypedHandle(_) => "IntPtr".into(),
        TypeRef::Enum(_) => "int".into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Maps a shared ABI [`CType`] to its P/Invoke spelling. All pointers collapse
/// to `IntPtr`; `size_t` becomes `UIntPtr`. The structural lowering (which slots
/// exist, in what order) comes from [`weaveffi_core::abi`].
fn cs_pinvoke_ctype(ty: &CType) -> String {
    match ty {
        CType::Int32 | CType::Enum { .. } => "int".into(),
        // C `bool` is one byte on every supported ABI.
        CType::Bool => "byte".into(),
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

/// Emit [`emit_doc`] at the writer's current depth by rendering into a scratch
/// buffer and splicing it verbatim, so a [`CodeWriter`]-based renderer can
/// interleave XML doc comments without re-implementing their formatting.
fn writer_doc(w: &mut CodeWriter, doc: &Option<String>) {
    let mut tmp = String::new();
    emit_doc(&mut tmp, doc, &w.indent_str());
    w.raw(tmp);
}

/// Emit [`emit_fn_doc`] at the writer's current depth, splicing the rendered
/// `<summary>`/`<param>` block in verbatim. The [`CodeWriter`] companion to
/// [`emit_fn_doc`] used by the method renderers.
fn writer_fn_doc(w: &mut CodeWriter, doc: &Option<String>, params: &[ParamBinding]) {
    let mut tmp = String::new();
    emit_fn_doc(&mut tmp, doc, params, &w.indent_str());
    w.raw(tmp);
}

fn render_csharp(
    model: &BindingModel,
    namespace: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    filename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    // Opt the file into the nullable annotation context so the `string?`
    // signatures (optional strings) are valid regardless of the consuming
    // project's <Nullable> setting; without this, default projects warn CS8632.
    out.push_str("#nullable enable\n\n");
    out.push_str(
        "using System;\nusing System.Collections.Generic;\nusing System.Runtime.InteropServices;\n",
    );
    if model
        .modules
        .iter()
        .flat_map(|m| m.callables())
        .any(|f| f.is_async)
    {
        out.push_str("using System.Threading.Tasks;\n");
    }
    out.push('\n');
    out.push_str(&format!("namespace {namespace}\n{{\n"));

    // One typed exception per declaring module; inheriting submodules
    // reference the ancestor's type through `ModuleBinding::error`.
    let domains: Vec<&ErrorBinding> = model
        .modules
        .iter()
        .filter_map(|m| m.error.as_ref())
        .filter(|eb| eb.declared_here)
        .collect();

    render_exception_class(&mut out);
    for eb in &domains {
        render_domain_exception(&mut out, eb);
    }
    render_error_struct(&mut out, &domains);
    render_helpers_class(&mut out);
    if model
        .modules
        .iter()
        .flat_map(|m| m.callables())
        .any(|f| matches!(f.shape, CallShape::Iterator(_)))
    {
        render_once_enumerable_class(&mut out);
    }

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
        for i in &m.interfaces {
            render_interface_class(&mut out, i, m.error.as_ref());
        }
    }

    render_native_methods(&mut out, model);

    for m in &model.modules {
        render_wrapper_class(&mut out, m, strip_module_prefix);
    }

    out.push_str("}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// The C# exception class name for one error domain: the domain stem with
/// exactly one `Exception` suffix, so `KvError` becomes `KvException` rather
/// than `KvErrorException`.
fn dotnet_exception_name(eb: &ErrorBinding) -> String {
    errors::exception_type_name(&eb.type_name)
}

/// The per-domain error-check helper name on `WeaveFFIError`; `KvException`
/// is checked by `CheckKv`.
fn check_method_name(eb: &ErrorBinding) -> String {
    let exc = dotnet_exception_name(eb);
    let stem = exc.strip_suffix("Exception").unwrap_or(&exc).to_string();
    format!("Check{stem}")
}

/// Escapes a string for embedding in a C# string literal.
fn cs_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How a wrapper surfaces a non-zero error slot, rendering
/// [`ErrorStrategy`]: [`ErrorStrategy::Throws`] with a domain in scope raises
/// the typed domain exception; everything else (a producer trap, or a
/// throwing function without a declared domain) raises the plain
/// `WeaveFFIException`, which no domain exception check can catch by type.
#[derive(Clone, Copy)]
enum ErrCtx<'a> {
    /// Throw the generic `WeaveFFIException`.
    Generic,
    /// Throw the domain's typed exception via its `FromCode` factory.
    Domain(&'a ErrorBinding),
}

impl<'a> ErrCtx<'a> {
    /// The error context for one function: typed when the function's
    /// [`ErrorStrategy`] is `Throws` and its module has an error domain in
    /// scope, generic otherwise (including every `Trap` function, whose only
    /// failures are producer bugs and must not wear the domain type).
    fn for_fn(f: &FnBinding, error: Option<&'a ErrorBinding>) -> Self {
        match (f.error_strategy(), error) {
            (ErrorStrategy::Throws, Some(eb)) => ErrCtx::Domain(eb),
            _ => ErrCtx::Generic,
        }
    }

    /// The check statement placed after a native call writing into `err`.
    fn check_stmt(&self) -> String {
        self.check_stmt_for("err")
    }

    /// The check statement for a named `WeaveFFIError` local.
    fn check_stmt_for(&self, var: &str) -> String {
        match self {
            ErrCtx::Generic => format!("WeaveFFIError.Check({var});"),
            ErrCtx::Domain(eb) => format!("WeaveFFIError.{}({var});", check_method_name(eb)),
        }
    }

    /// The exception expression an async completion callback faults its
    /// `TaskCompletionSource` with.
    fn async_exception_expr(&self) -> String {
        match self {
            ErrCtx::Generic => "new WeaveFFIException(wErr.Code, msg)".into(),
            ErrCtx::Domain(eb) => {
                format!("{}.FromCode(wErr.Code, msg)", dotnet_exception_name(eb))
            }
        }
    }

    /// Emit the `<exception>` XML doc line for a throwing wrapper; generic
    /// wrappers document nothing extra.
    fn write_exception_doc(&self, w: &mut CodeWriter) {
        if let ErrCtx::Domain(eb) = self {
            w.line(format!(
                "/// <exception cref=\"{}\">Thrown when the call reports a {} code.</exception>",
                dotnet_exception_name(eb),
                eb.type_name
            ));
        }
    }
}

fn render_exception_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("public class WeaveFFIException : Exception");
    w.block("{", "}", |w| {
        w.line("public int Code { get; }");
        w.blank();
        w.line("public WeaveFFIException(int code, string message) : base(message)");
        w.block("{", "}", |w| {
            w.line("Code = code;");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// One typed exception class per declared error domain, extending the generic
/// brand exception. Each code surfaces as a `public const int` (PascalCase),
/// and `FromCode` maps a raw error slot to the typed exception, falling back
/// to the generic `WeaveFFIException` for unknown codes.
fn render_domain_exception(out: &mut String, eb: &ErrorBinding) {
    let exc = dotnet_exception_name(eb);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "/// <summary>Typed exception for the {} error domain (module {}).</summary>",
        eb.type_name,
        eb.owner_path.replace('_', ".")
    ));
    w.line(format!("public class {exc} : WeaveFFIException"));
    w.block("{", "}", |w| {
        for c in &eb.codes {
            if c.doc.is_some() {
                writer_doc(w, &c.doc);
            } else {
                w.line(format!("/// <summary>{}</summary>", xml_escape(&c.message)));
            }
            w.line(format!(
                "public const int {} = {};",
                errors::pascal(&c.name),
                c.value
            ));
        }
        w.blank();
        w.line(format!(
            "public {exc}(int code, string message) : base(code, message)"
        ));
        w.line("{");
        w.line("}");
        w.blank();
        w.line("/// <summary>Wraps a raw error slot in the typed exception, falling");
        w.line("/// back to <see cref=\"WeaveFFIException\"/> for unknown codes.</summary>");
        w.line("internal static WeaveFFIException FromCode(int code, string message)");
        w.block("{", "}", |w| {
            w.line("switch (code)");
            w.block("{", "}", |w| {
                for c in &eb.codes {
                    w.line(format!("case {}:", errors::pascal(&c.name)));
                    w.indent();
                    w.line(format!(
                        "return new {exc}(code, string.IsNullOrEmpty(message) ? \"{}\" : message);",
                        cs_str(&c.message)
                    ));
                    w.dedent();
                }
                w.line("default:");
                w.indent();
                w.line("return new WeaveFFIException(code, message);");
                w.dedent();
            });
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The raw error slot plus its check helpers: the generic `Check` (throws
/// `WeaveFFIException` on any non-zero code) and one `Check{Domain}` variant
/// per declared domain (throws the typed exception via `FromCode`).
fn render_error_struct(out: &mut String, domains: &[&ErrorBinding]) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("[StructLayout(LayoutKind.Sequential)]");
    w.line("internal struct WeaveFFIError");
    w.block("{", "}", |w| {
        w.line("public int Code;");
        w.line("public IntPtr Message;");
        w.blank();
        w.line("internal static void Check(WeaveFFIError err)");
        w.block("{", "}", |w| {
            w.line("if (err.Code != 0)");
            w.block("{", "}", |w| {
                w.line("var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";");
                w.line("throw new WeaveFFIException(err.Code, msg);");
            });
        });
        for eb in domains {
            let exc = dotnet_exception_name(eb);
            let check = check_method_name(eb);
            w.blank();
            w.line(format!("internal static void {check}(WeaveFFIError err)"));
            w.block("{", "}", |w| {
                w.line("if (err.Code != 0)");
                w.block("{", "}", |w| {
                    w.line("var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";");
                    w.line(format!("throw {exc}.FromCode(err.Code, msg);"));
                });
            });
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

fn render_helpers_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("internal static class WeaveFFIHelpers");
    w.block("{", "}", |w| {
        w.line("internal static IntPtr StringToPtr(string? s)");
        w.block("{", "}", |w| {
            w.line("return s == null ? IntPtr.Zero : Marshal.StringToCoTaskMemUTF8(s);");
        });
        w.blank();
        w.line("internal static string? PtrToString(IntPtr ptr)");
        w.block("{", "}", |w| {
            w.line("return ptr == IntPtr.Zero ? null : Marshal.PtrToStringUTF8(ptr);");
        });
        w.blank();
        w.line("internal static void FreePtr(IntPtr ptr)");
        w.block("{", "}", |w| {
            w.line("Marshal.FreeCoTaskMem(ptr);");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The single-use `IEnumerable<T>` wrapping every iterator return. The
/// native iterator is consumed (and destroyed) by its one enumerator, so a
/// second `GetEnumerator()` cannot yield anything; surfacing it as an
/// `InvalidOperationException` beats silently returning an empty or
/// double-destroyed sequence.
fn render_once_enumerable_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("/// <summary>A lazily streamed sequence backed by a native iterator.");
    w.line("/// It can be enumerated exactly once; enumerate it promptly (or call");
    w.line("/// a materializing operator such as ToList) and let the enumerator be");
    w.line("/// disposed to release the native iterator.</summary>");
    w.line("internal sealed class WeaveFFIOnceEnumerable<T> : IEnumerable<T>");
    w.block("{", "}", |w| {
        w.line("private IEnumerator<T>? _enumerator;");
        w.blank();
        w.line("internal WeaveFFIOnceEnumerable(IEnumerator<T> enumerator)");
        w.block("{", "}", |w| {
            w.line("_enumerator = enumerator;");
        });
        w.blank();
        w.line("public IEnumerator<T> GetEnumerator()");
        w.block("{", "}", |w| {
            w.line("var e = System.Threading.Interlocked.Exchange(ref _enumerator, null);");
            w.line("if (e == null)");
            w.block("{", "}", |w| {
                w.line(
                    "throw new InvalidOperationException(\"this sequence can be enumerated only once\");",
                );
            });
            w.line("return e;");
        });
        w.blank();
        w.line("System.Collections.IEnumerator System.Collections.IEnumerable.GetEnumerator()");
        w.block("{", "}", |w| {
            w.line("return GetEnumerator();");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

fn render_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum is not a plain C# `enum`; it surfaces as an
    // opaque-object class via `render_rich_enum_class`. Guard here so this
    // path only ever emits C-style enums.
    if e.is_rich() {
        return;
    }
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &e.doc);
    w.line(format!("public enum {}", e.name));
    w.block("{", "}", |w| {
        for v in &e.variants {
            writer_doc(w, &v.doc);
            w.line(format!("{} = {},", v.name, v.value));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

fn render_struct_class(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &s.doc);
    w.line(format!("public class {} : IDisposable", s.name));
    w.line("{");
    w.indent();
    w.line("private IntPtr _handle;");
    w.line("private bool _disposed;");
    w.blank();
    w.line(format!("internal {}(IntPtr handle)", s.name));
    w.block("{", "}", |w| {
        w.line("_handle = handle;");
    });
    w.blank();
    w.line("internal IntPtr Handle => _handle;");
    w.blank();

    for field in &s.fields {
        let mut tmp = String::new();
        render_struct_getter(&mut tmp, field);
        w.raw(tmp);
    }

    w.line("public void Dispose()");
    w.block("{", "}", |w| {
        w.line("if (!_disposed)");
        w.block("{", "}", |w| {
            w.line(format!("NativeMethods.{}(_handle);", s.destroy_symbol));
            w.line("_disposed = true;");
        });
        w.line("GC.SuppressFinalize(this);");
    });
    w.blank();
    w.line(format!("~{}()", s.name));
    w.block("{", "}", |w| {
        w.line("Dispose();");
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an opaque-object `IDisposable` class,
/// mirroring the struct wrapper: it owns the `IntPtr` handle and frees it via
/// the enum's `_destroy` (same Dispose + finalizer, so no double free). Surface:
/// a nested `enum Tag` + `GetTag()` reader, one static factory per variant
/// (`Shape.Circle(2.5)`) using the struct/builder error-handling convention,
/// and per-variant field accessors namespaced as `{Variant}{Field}` properties
/// that reuse the struct-getter marshalling. A `TypeRef::RichEnum` reference
/// shares the opaque-pointer handling of `TypeRef::Record`, so functions
/// taking/returning the enum flow through the record param/return path.
fn render_rich_enum_class(out: &mut String, e: &EnumBinding) {
    let Some(rich) = e.rich.as_ref() else {
        return;
    };
    let name = &e.name;

    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &e.doc);
    w.line(format!("public class {name} : IDisposable"));
    w.line("{");
    w.indent();
    w.line("private IntPtr _handle;");
    w.line("private bool _disposed;");
    w.blank();
    w.line(format!("internal {name}(IntPtr handle)"));
    w.block("{", "}", |w| {
        w.line("_handle = handle;");
    });
    w.blank();
    w.line("internal IntPtr Handle => _handle;");
    w.blank();

    // Nested discriminant enum + typed reader. `Tag` is a nested type, so the
    // reader is `GetTag()` (a `Tag` property would collide with the type name).
    w.line("public enum Tag");
    w.block("{", "}", |w| {
        for v in &e.variants {
            writer_doc(w, &v.doc);
            w.line(format!("{} = {},", v.name, v.value));
        }
    });
    w.blank();
    w.line("public Tag GetTag()");
    w.block("{", "}", |w| {
        w.line(format!(
            "return (Tag)NativeMethods.{}(_handle);",
            rich.tag_symbol
        ));
    });
    w.blank();

    // One static factory per variant.
    for v in &rich.variants {
        let mut tmp = String::new();
        render_rich_variant_factory(&mut tmp, name, v);
        w.raw(tmp);
    }

    // Per-variant field accessors, namespaced by variant to avoid collisions
    // (`CircleRadius`, `RectangleWidth`, ...). Same marshalling as struct fields.
    for v in &rich.variants {
        let variant_prefix = v.name.to_upper_camel_case();
        for f in &v.fields {
            let prop_name = format!("{}{}", variant_prefix, f.name.to_upper_camel_case());
            let mut tmp = String::new();
            render_field_getter(&mut tmp, &prop_name, f);
            w.raw(tmp);
        }
    }

    w.line("public void Dispose()");
    w.block("{", "}", |w| {
        w.line("if (!_disposed)");
        w.block("{", "}", |w| {
            w.line(format!("NativeMethods.{}(_handle);", rich.destroy_symbol));
            w.line("_disposed = true;");
        });
        w.line("GC.SuppressFinalize(this);");
    });
    w.blank();
    w.line(format!("~{name}()"));
    w.block("{", "}", |w| {
        w.line("Dispose();");
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
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

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_doc(&mut w, &v.doc);
    w.line(format!(
        "public static {enum_name} {}({})",
        v.name,
        params_sig.join(", ")
    ));
    w.line("{");
    w.indent();
    w.line("var err = new WeaveFFIError();");

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
            let mut tmp = String::new();
            render_marshal_setup(&mut tmp, p, "            ");
            w.raw(tmp);
        }
        w.line("try");
        w.line("{");
        w.scope(|w| {
            w.line(call.clone());
            w.line("WeaveFFIError.Check(err);");
            w.line(format!("return new {enum_name}(result);"));
        });
        w.line("}");
        w.line("finally");
        w.line("{");
        for p in &params {
            let mut tmp = String::new();
            render_marshal_cleanup(&mut tmp, p, "                ");
            w.raw(tmp);
        }
        w.line("}");
    } else {
        w.line(call);
        w.line("WeaveFFIError.Check(err);");
        w.line(format!("return new {enum_name}(result);"));
    }
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
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
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &s.doc);
    w.line(format!("public class {builder_name}"));
    w.line("{");
    w.indent();
    for field in &s.fields {
        let (storage, default) = cs_field_default(&field.ty);
        let fname = safe_cs_name(&field.name);
        w.line(format!("private {storage} _{fname} = {default};"));
    }
    w.blank();
    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let param_ty = cs_type(&field.ty);
        let fname = safe_cs_name(&field.name);
        writer_doc(&mut w, &field.doc);
        w.line(format!(
            "public {builder_name} With{pascal}({param_ty} value)"
        ));
        w.block("{", "}", |w| {
            w.line(format!("_{fname} = value;"));
            w.line("return this;");
        });
        w.blank();
    }
    // Build: marshal every field into the struct's C `create` call with the
    // same lowering used for function parameters, then wrap the handle.
    w.line(format!("public {} Build()", s.name));
    w.line("{");
    w.indent();
    w.line("var err = new WeaveFFIError();");
    let params: Vec<ParamBinding> = s.fields.iter().map(field_as_param).collect();
    for p in &params {
        let fname = safe_cs_name(&p.name);
        w.line(format!("var {fname} = _{fname};"));
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
            let mut tmp = String::new();
            render_marshal_setup(&mut tmp, p, "            ");
            w.raw(tmp);
        }
        w.line("try");
        w.line("{");
        w.scope(|w| {
            w.line(call.clone());
            w.line("WeaveFFIError.Check(err);");
            w.line(format!("return new {}(result);", s.name));
        });
        w.line("}");
        w.line("finally");
        w.line("{");
        for p in &params {
            let mut tmp = String::new();
            render_marshal_cleanup(&mut tmp, p, "                ");
            w.raw(tmp);
        }
        w.line("}");
    } else {
        w.line(call);
        w.line("WeaveFFIError.Check(err);");
        w.line(format!("return new {}(result);", s.name));
    }
    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// A copy of `f` whose parameter names are lowerCamelCase, the C# parameter
/// convention for public wrapper signatures. Only the wrapper signature and
/// its marshalling locals derive from these names; ABI slot names and the
/// P/Invoke declarations keep the IDL spelling.
fn camel_fn(f: &FnBinding) -> FnBinding {
    let mut f = f.clone();
    for p in &mut f.params {
        p.name = p.name.to_lower_camel_case();
    }
    f
}

/// Render one interface as an opaque-handle class following the struct-wrapper
/// pattern: a private `IntPtr` handle with `IDisposable` plus a finalizer
/// calling the interface's destroy symbol. The `new` constructor maps to a
/// real C# constructor, other constructors become static factories, instance
/// methods pass the handle as the leading native argument, and statics are
/// plain static methods. All member shapes reuse the free-function
/// marshalling paths.
fn render_interface_class(out: &mut String, i: &InterfaceBinding, error: Option<&ErrorBinding>) {
    let name = &i.name;
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &i.doc);
    w.line(format!("public class {name} : IDisposable"));
    w.line("{");
    w.indent();
    w.line("private IntPtr _handle;");
    w.line("private bool _disposed;");
    w.blank();
    w.line(format!("internal {name}(IntPtr handle)"));
    w.block("{", "}", |w| {
        w.line("_handle = handle;");
    });
    w.blank();
    w.line("internal IntPtr Handle => _handle;");
    w.blank();

    for c in &i.constructors {
        let err = ErrCtx::for_fn(c, error);
        let mut tmp = String::new();
        if c.name == "new" && matches!(c.shape, CallShape::Sync(_)) {
            render_interface_ctor(&mut tmp, i, c, err);
        } else {
            render_wrapper_method(&mut tmp, c, &c.name.to_upper_camel_case(), None, err);
        }
        w.raw(tmp);
    }
    for m in &i.methods {
        let err = ErrCtx::for_fn(m, error);
        let mut tmp = String::new();
        render_wrapper_method(
            &mut tmp,
            m,
            &m.name.to_upper_camel_case(),
            Some("_handle"),
            err,
        );
        w.raw(tmp);
    }
    for s in &i.statics {
        let err = ErrCtx::for_fn(s, error);
        let mut tmp = String::new();
        render_wrapper_method(&mut tmp, s, &s.name.to_upper_camel_case(), None, err);
        w.raw(tmp);
    }

    w.line("public void Dispose()");
    w.block("{", "}", |w| {
        w.line("if (!_disposed)");
        w.block("{", "}", |w| {
            w.line(format!("NativeMethods.{}(_handle);", i.destroy_symbol));
            w.line("_disposed = true;");
        });
        w.line("GC.SuppressFinalize(this);");
    });
    w.blank();
    w.line(format!("~{name}()"));
    w.block("{", "}", |w| {
        w.line("Dispose();");
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render the `new` constructor as a real C# constructor: the sync call path
/// with the checked result assigned to `_handle` instead of returned.
fn render_interface_ctor(out: &mut String, i: &InterfaceBinding, f: &FnBinding, err: ErrCtx) {
    let f = camel_fn(f);
    let c_sym = &f.c_base;
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }
    w.line(format!("public {}({})", i.name, params_sig.join(", ")));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");
        let call_args = build_call_args(&f.params);
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let call = format!("var result = NativeMethods.{c_sym}({args_part}ref err);");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(call.clone());
                w.line(err.check_stmt());
                w.line("_handle = result;");
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line(call);
            w.line(err.check_stmt());
            w.line("_handle = result;");
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
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

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_doc(&mut w, &field.doc);
    w.line(format!("public {cs} {prop_name}"));
    w.line("{");
    w.indent();
    w.line("get");
    w.line("{");
    w.indent();

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
            w.line(format!("return NativeMethods.{getter_sym}(_handle);"));
        }
        TypeRef::TypedHandle(name) => {
            let cn = local_type_name(name);
            w.line(format!(
                "return new {cn}(NativeMethods.{getter_sym}(_handle));"
            ));
        }
        TypeRef::Bool => {
            w.line(format!("return NativeMethods.{getter_sym}(_handle) != 0;"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("var ptr = NativeMethods.{getter_sym}(_handle);"));
            w.line("var str = WeaveFFIHelpers.PtrToString(ptr);");
            w.line("NativeMethods.weaveffi_free_string(ptr);");
            w.line("return str ?? \"\";");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "var ptr = NativeMethods.{getter_sym}(_handle, out var len);"
            ));
            w.line("if (ptr == IntPtr.Zero) return Array.Empty<byte>();");
            w.line("var arr = new byte[(int)len];");
            w.line("Marshal.Copy(ptr, arr, 0, (int)len);");
            w.line("NativeMethods.weaveffi_free_bytes(ptr, len);");
            w.line("return arr;");
        }
        TypeRef::Enum(name) => {
            // A cross-module enum (e.g. `graphics.Unit`) is emitted as the bare
            // top-level C# type `Unit`; the cast must use that local name, not
            // the qualified IR name (there is no `graphics` namespace).
            let cn = local_type_name(name);
            w.line(format!("return ({cn})NativeMethods.{getter_sym}(_handle);"));
        }
        // The getter returns an owned object reference; the wrapper adopts it
        // and its Dispose() calls the type's destroy symbol.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            let cn = local_type_name(name);
            w.line(format!(
                "return new {cn}(NativeMethods.{getter_sym}(_handle));"
            ));
        }
        TypeRef::Optional(inner)
            if matches!(inner.as_ref(), TypeRef::Bytes | TypeRef::BorrowedBytes) =>
        {
            w.line(format!(
                "var ptr = NativeMethods.{getter_sym}(_handle, out var len);"
            ));
            w.line("if (ptr == IntPtr.Zero) return null;");
            w.line("var arr = new byte[(int)len];");
            w.line("Marshal.Copy(ptr, arr, 0, (int)len);");
            w.line("NativeMethods.weaveffi_free_bytes(ptr, len);");
            w.line("return arr;");
        }
        TypeRef::Optional(inner) => {
            w.line(format!("var ptr = NativeMethods.{getter_sym}(_handle);"));
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    w.line("if (ptr == IntPtr.Zero) return null;");
                    w.line("var str = WeaveFFIHelpers.PtrToString(ptr);");
                    w.line("NativeMethods.weaveffi_free_string(ptr);");
                    w.line("return str;");
                }
                // Owned object pointer, adopted by its wrapper.
                TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
                    let cn = local_type_name(name);
                    w.line(format!("return ptr == IntPtr.Zero ? null : new {cn}(ptr);"));
                }
                other => {
                    // A producer-boxed scalar: dereference, then release the
                    // box (the `ReturnFree::BoxedScalar` contract).
                    let Some((read, size)) = boxed_scalar_read(other, "ptr") else {
                        unreachable!("unsupported optional field type");
                    };
                    w.line("if (ptr == IntPtr.Zero) return null;");
                    w.line(format!("var value = {read};"));
                    w.line(format!(
                        "NativeMethods.weaveffi_free_bytes(ptr, (UIntPtr){size});"
                    ));
                    w.line("return value;");
                }
            }
        }
        TypeRef::List(inner) => {
            w.line(format!(
                "var ptr = NativeMethods.{getter_sym}(_handle, out var len);"
            ));
            let mut tmp = String::new();
            render_list_unmarshal(&mut tmp, inner, "                ");
            w.raw(tmp);
        }
        TypeRef::Map(k, v) => {
            w.line(format!(
                "NativeMethods.{getter_sym}(_handle, out var outKeys, out var outValues, out var outLen);"
            ));
            let mut tmp = String::new();
            render_map_decode(
                &mut tmp,
                k,
                v,
                "outKeys",
                "outValues",
                "outLen",
                "                ",
            );
            w.raw(tmp);
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as struct field"),
        // The getter returns an owned reference; the wrapper owns and
        // disposes it like any interface return.
        TypeRef::Interface(name) => {
            let cn = local_type_name(name);
            w.line(format!(
                "return new {cn}(NativeMethods.{getter_sym}(_handle));"
            ));
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }

    w.dedent();
    w.line("}");
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

fn render_list_unmarshal(out: &mut String, inner: &TypeRef, indent: &str) {
    render_list_decode(out, inner, "ptr", "len", indent);
}

/// The C# expression dereferencing one producer-boxed optional scalar of type
/// `inner` through `ptr`, paired with the box's size in bytes (the argument
/// to the `weaveffi_free_bytes` release the `ReturnFree::BoxedScalar` plan
/// requires). Returns `None` for non-scalar inners, which are pointer
/// optionals and reuse the inner type's return plan.
fn boxed_scalar_read(inner: &TypeRef, ptr: &str) -> Option<(String, &'static str)> {
    Some(match inner {
        TypeRef::I8 => (format!("(sbyte)Marshal.ReadByte({ptr})"), "1"),
        TypeRef::U8 => (format!("Marshal.ReadByte({ptr})"), "1"),
        TypeRef::I16 => (format!("Marshal.ReadInt16({ptr})"), "2"),
        TypeRef::U16 => (format!("(ushort)Marshal.ReadInt16({ptr})"), "2"),
        TypeRef::I32 => (format!("Marshal.ReadInt32({ptr})"), "4"),
        TypeRef::U32 => (format!("(uint)Marshal.ReadInt32({ptr})"), "4"),
        TypeRef::I64 => (format!("Marshal.ReadInt64({ptr})"), "8"),
        TypeRef::U64 | TypeRef::Handle => (format!("(ulong)Marshal.ReadInt64({ptr})"), "8"),
        TypeRef::F32 => (
            format!("BitConverter.Int32BitsToSingle(Marshal.ReadInt32({ptr}))"),
            "4",
        ),
        TypeRef::F64 => (
            format!("BitConverter.Int64BitsToDouble(Marshal.ReadInt64({ptr}))"),
            "8",
        ),
        // C `bool` is one byte.
        TypeRef::Bool => (format!("Marshal.ReadByte({ptr}) != 0"), "1"),
        TypeRef::Enum(name) => (
            format!("({})Marshal.ReadInt32({ptr})", local_type_name(name)),
            "4",
        ),
        _ => return None,
    })
}

/// The size in bytes of one C array slot holding an element of type `ty`, as
/// a C# expression. Used to release producer-allocated array and map buffers
/// with `weaveffi_free_bytes(ptr, len * slot_size)`, the `ReturnFree::Array`
/// and `ReturnFree::MapBuffers` contract.
fn cs_elem_size(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 | TypeRef::U8 | TypeRef::Bool => "1",
        TypeRef::I16 | TypeRef::U16 => "2",
        TypeRef::I32 | TypeRef::U32 | TypeRef::F32 | TypeRef::Enum(_) => "4",
        TypeRef::I64 | TypeRef::U64 | TypeRef::F64 | TypeRef::Handle => "8",
        // Optional pointer elements occupy one pointer slot (null = none).
        TypeRef::Optional(inner) => cs_elem_size(inner),
        // Strings, records, rich enums, interfaces, and typed handles are all
        // pointer slots.
        _ => "IntPtr.Size",
    }
}

/// Emit the statements reading element `idx` of the producer-owned buffer
/// `arr` into a local named `var`, honoring the per-element release contract
/// (`ElemFree`): string elements are copied and then freed with
/// `weaveffi_free_string`; record/rich-enum/interface elements are adopted by
/// their wrapper (whose `Dispose` calls the destroy symbol); by-value
/// elements copy with nothing to free.
fn render_owned_element_read(w: &mut CodeWriter, ty: &TypeRef, arr: &str, idx: &str, var: &str) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "var {var}Ptr = Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size);"
            ));
            w.line(format!(
                "var {var} = Marshal.PtrToStringUTF8({var}Ptr) ?? \"\";"
            ));
            w.line(format!("NativeMethods.weaveffi_free_string({var}Ptr);"));
        }
        _ => {
            w.line(format!(
                "var {var} = {};",
                marshal_read_element(ty, arr, idx)
            ));
        }
    }
}

/// Emit the `weaveffi_free_bytes` release of a producer-allocated array
/// buffer of `len` elements of type `elem` based at `base`, after every
/// element has been copied out (and string elements individually freed).
fn write_buffer_free(w: &mut CodeWriter, base: &str, len: &str, elem: &TypeRef) {
    w.line(format!(
        "NativeMethods.weaveffi_free_bytes({base}, (UIntPtr)((int){len} * {}));",
        cs_elem_size(elem)
    ));
}

/// Decodes a producer-owned C array (`base` + `len`) into a managed `T[]`,
/// releases the producer's allocations (each string element via
/// `weaveffi_free_string`, then the array buffer via `weaveffi_free_bytes`),
/// and returns the copy. Object elements are adopted by their wrappers rather
/// than freed. Blittable scalars bulk-copy before the buffer release.
fn render_list_decode(out: &mut String, inner: &TypeRef, base: &str, len: &str, indent: &str) {
    let elem = cs_type(inner);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    w.line(format!(
        "if ({base} == IntPtr.Zero) return Array.Empty<{elem}>();"
    ));
    w.line(format!("var arr = new {elem}[(int){len}];"));
    match inner {
        TypeRef::I32 | TypeRef::I64 | TypeRef::F64 => {
            w.line(format!("Marshal.Copy({base}, arr, 0, (int){len});"));
        }
        _ => {
            w.line(format!("for (int i = 0; i < (int){len}; i++)"));
            w.block("{", "}", |w| {
                render_owned_element_read(w, inner, base, "i", "item");
                w.line("arr[i] = item;");
            });
        }
    }
    write_buffer_free(&mut w, base, len, inner);
    w.line("return arr;");
    out.push_str(&w.finish());
}

/// Decodes a producer-owned map return (parallel `keys`/`values` buffers of
/// `len` entries) into a `Dictionary`, freeing each string key/value after
/// copying and then releasing both parallel buffers with
/// `weaveffi_free_bytes` (the `ReturnFree::MapBuffers` contract), and
/// returns the copy.
fn render_map_decode(
    out: &mut String,
    k: &TypeRef,
    v: &TypeRef,
    keys: &str,
    values: &str,
    len: &str,
    indent: &str,
) {
    let k_cs = cs_type(k);
    let v_cs = cs_type(v);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    w.line(format!("var dict = new Dictionary<{k_cs}, {v_cs}>();"));
    w.line(format!(
        "if ({keys} != IntPtr.Zero && {values} != IntPtr.Zero)"
    ));
    w.block("{", "}", |w| {
        w.line(format!("for (int i = 0; i < (int){len}; i++)"));
        w.block("{", "}", |w| {
            render_owned_element_read(w, k, keys, "i", "key");
            render_owned_element_read(w, v, values, "i", "value");
            w.line("dict[key] = value;");
        });
        write_buffer_free(w, keys, len, k);
        write_buffer_free(w, values, len, v);
    });
    w.line("return dict;");
    out.push_str(&w.finish());
}

fn render_native_methods(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("internal static class NativeMethods");
    w.line("{");
    w.indent();
    w.line("private const string LibName = \"weaveffi\";");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_free_string(IntPtr ptr);");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);");
    w.blank();
    w.dedent();

    for m in &model.modules {
        for e in &m.enums {
            // Plain enums lower by value and need no P/Invoke; rich enums need
            // the opaque-object symbol set (tag, destroy, per-variant new/get).
            if e.is_rich() {
                let mut tmp = String::new();
                render_rich_enum_pinvoke(&mut tmp, e);
                w.raw(tmp);
            }
        }
        for s in &m.structs {
            let mut tmp = String::new();
            render_struct_pinvoke(&mut tmp, s);
            w.raw(tmp);
        }
        for i in &m.interfaces {
            let mut tmp = String::new();
            render_interface_pinvoke(&mut tmp, i);
            w.raw(tmp);
        }
        for cb in &m.callbacks {
            let mut tmp = String::new();
            render_callback_pinvoke(&mut tmp, cb);
            w.raw(tmp);
        }
        for l in &m.listeners {
            let mut tmp = String::new();
            render_listener_pinvoke(&mut tmp, l);
            w.raw(tmp);
        }
        for f in &m.functions {
            let mut tmp = String::new();
            render_shaped_pinvoke(&mut tmp, f);
            w.raw(tmp);
        }
    }

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
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
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]");
    w.line(format!(
        "internal delegate void {delegate_name}({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

fn render_listener_pinvoke(out: &mut String, l: &ListenerBinding) {
    let delegate_name = format!("Cb_{}", l.callback_c_fn_type);
    let register_sym = &l.register_symbol;
    let unregister_sym = &l.unregister_symbol;

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{register_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern ulong {register_sym}({delegate_name} callback, IntPtr context);"
    ));
    w.blank();

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{unregister_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {unregister_sym}(ulong id);"
    ));
    w.blank();
    out.push_str(&w.finish());
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

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{create_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern IntPtr {create_sym}({});",
        create_params.join(", ")
    ));
    w.blank();

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{destroy_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {destroy_sym}(IntPtr ptr);"
    ));
    w.blank();

    for field in &s.fields {
        let getter_sym = &field.getter_symbol;
        let (ret_type, extra_params) = pinvoke_return_info(&field.ty);

        w.line(format!(
            "[DllImport(LibName, EntryPoint = \"{getter_sym}\", CallingConvention = CallingConvention.Cdecl)]"
        ));
        let mut params = vec!["IntPtr ptr".into()];
        params.extend(extra_params);
        w.line(format!(
            "internal static extern {ret_type} {getter_sym}({});",
            params.join(", ")
        ));
        w.blank();
    }
    out.push_str(&w.finish());
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

    let mut w = CodeWriter::four_space().with_depth(2);
    let tag_sym = &rich.tag_symbol;
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{tag_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!("internal static extern int {tag_sym}(IntPtr ptr);"));
    w.blank();

    let destroy_sym = &rich.destroy_symbol;
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{destroy_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {destroy_sym}(IntPtr ptr);"
    ));
    w.blank();

    for v in &rich.variants {
        let new_sym = &v.create.symbol;
        let mut new_params: Vec<String> = v
            .fields
            .iter()
            .flat_map(|f| pinvoke_param_list(&field_as_param(f)))
            .collect();
        new_params.push("ref WeaveFFIError err".into());
        w.line(format!(
            "[DllImport(LibName, EntryPoint = \"{new_sym}\", CallingConvention = CallingConvention.Cdecl)]"
        ));
        w.line(format!(
            "internal static extern IntPtr {new_sym}({});",
            new_params.join(", ")
        ));
        w.blank();

        for f in &v.fields {
            let getter_sym = &f.getter_symbol;
            let (ret_type, extra_params) = pinvoke_return_info(&f.ty);
            w.line(format!(
                "[DllImport(LibName, EntryPoint = \"{getter_sym}\", CallingConvention = CallingConvention.Cdecl)]"
            ));
            let mut params = vec!["IntPtr ptr".into()];
            params.extend(extra_params);
            w.line(format!(
                "internal static extern {ret_type} {getter_sym}({});",
                params.join(", ")
            ));
            w.blank();
        }
    }
    out.push_str(&w.finish());
}

/// Emit the extern declaration set matching one callable's shape exactly:
/// sync, async (delegate + launcher), or iterator (constructor, `next`,
/// `destroy`). Shared by free functions and interface members.
fn render_shaped_pinvoke(out: &mut String, f: &FnBinding) {
    match &f.shape {
        CallShape::Sync(_) => render_function_pinvoke(out, f),
        CallShape::Async(_) => render_async_function_pinvoke(out, f),
        CallShape::Iterator(it) => render_iterator_pinvoke(out, it),
    }
}

/// The `[DllImport]` set backing one interface: the destroy symbol plus one
/// shape-matched extern set per member. Instance members carry the implicit
/// leading `self` slot.
fn render_interface_pinvoke(out: &mut String, i: &InterfaceBinding) {
    let destroy_sym = &i.destroy_symbol;
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{destroy_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {destroy_sym}(IntPtr self);"
    ));
    w.blank();
    out.push_str(&w.finish());

    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        render_shaped_pinvoke(out, f);
    }
}

fn render_function_pinvoke(out: &mut String, f: &FnBinding) {
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_pinvoke(out, it);
        return;
    }
    let c_sym = &f.c_base;

    let mut params: Vec<String> = Vec::new();
    if f.has_self {
        params.push("IntPtr self".into());
    }
    params.extend(f.params.iter().flat_map(pinvoke_param_list));

    let ret_type = if let Some(ret) = &f.ret {
        let (ret_cs, extra) = pinvoke_return_info(ret);
        params.extend(extra);
        ret_cs
    } else {
        "void".into()
    };

    params.push("ref WeaveFFIError err".into());

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{c_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern {ret_type} {c_sym}({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
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
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.launch.symbol
    ));
    w.line(format!(
        "internal static extern IntPtr {}({});",
        it.launch.symbol,
        launch_params.join(", ")
    ));
    w.blank();

    let next_params: Vec<String> = it.next.params.iter().map(iterator_slot_param).collect();
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.next.symbol
    ));
    w.line(format!(
        "internal static extern int {}({});",
        it.next.symbol,
        next_params.join(", ")
    ));
    w.blank();

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.destroy_symbol
    ));
    w.line(format!(
        "internal static extern void {}(IntPtr iter);",
        it.destroy_symbol
    ));
    w.blank();
    out.push_str(&w.finish());
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

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]");
    w.line(format!(
        "internal delegate void {delegate_name}(IntPtr context, IntPtr err{cb_params});"
    ));
    w.blank();

    let mut params: Vec<String> = Vec::new();
    if f.has_self {
        params.push("IntPtr self".into());
    }
    params.extend(f.params.iter().flat_map(pinvoke_param_list));
    if f.cancellable {
        params.push("IntPtr cancel_token".into());
    }
    params.push(format!("{delegate_name} callback"));
    params.push("IntPtr context".into());

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{c_sym}_async\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {c_sym}_async({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// Statements (appended to `out`) plus the expression converting one callback
/// parameter's delegate slots into the value handed to the user callback.
fn render_cb_arg(out: &mut String, p: &ParamBinding, idx: usize, indent: &str) -> String {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = safe_cs_name(&slots[0].name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    let expr = match &p.ty {
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
            w.line(format!("var {arg} = new byte[(int){len}];"));
            w.line(format!(
                "if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}, 0, (int){len});"
            ));
            arg
        }
        // Borrowed for the duration of the callback; the consumer must not
        // Dispose() the wrapper.
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            format!("new {}({n0})", local_type_name(name))
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                format!("Marshal.PtrToStringUTF8({n0})")
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let len = safe_cs_name(&slots[1].name);
                let arg = format!("arg{idx}");
                w.line(format!("byte[]? {arg} = null;"));
                w.line(format!("if ({n0} != IntPtr.Zero)"));
                w.block("{", "}", |w| {
                    w.line(format!("{arg} = new byte[(int){len}];"));
                    w.line(format!(
                        "if ((int){len} > 0) Marshal.Copy({n0}, {arg}, 0, (int){len});"
                    ));
                });
                arg
            }
            TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
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
                format!("{n0} == IntPtr.Zero ? (bool?)null : Marshal.ReadByte({n0}) != 0")
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
            w.line(format!("var {arg} = new {elem}[(int){len}];"));
            w.line(format!("if ({n0} != IntPtr.Zero)"));
            w.block("{", "}", |w| {
                w.line(format!("for (int i = 0; i < (int){len}; i++)"));
                w.block("{", "}", |w| {
                    w.line(format!(
                        "{arg}[i] = {};",
                        marshal_read_element(inner, &n0, "i")
                    ));
                });
            });
            arg
        }
        TypeRef::Map(k, v) => {
            let keys = safe_cs_name(&slots[0].name);
            let vals = safe_cs_name(&slots[1].name);
            let len = safe_cs_name(&slots[2].name);
            let arg = format!("arg{idx}");
            let (k_cs, v_cs) = (cs_type(k), cs_type(v));
            w.line(format!("var {arg} = new Dictionary<{k_cs}, {v_cs}>();"));
            w.line(format!(
                "if ({keys} != IntPtr.Zero && {vals} != IntPtr.Zero)"
            ));
            w.block("{", "}", |w| {
                w.line(format!("for (int i = 0; i < (int){len}; i++)"));
                w.block("{", "}", |w| {
                    w.line(format!(
                        "{arg}[{}] = {};",
                        marshal_read_element(k, &keys, "i"),
                        marshal_read_element(v, &vals, "i")
                    ));
                });
            });
            arg
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
        // Borrowed for the duration of the callback, like record parameters;
        // the consumer must not Dispose() the wrapper.
        TypeRef::Interface(name) => {
            format!("new {}({n0})", local_type_name(name))
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    };
    out.push_str(&w.finish());
    expr
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

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_doc(&mut w, &l.doc);
    w.line(format!(
        "/// <returns>A subscription id for {unregister_name}().</returns>"
    ));
    w.line(format!(
        "public static ulong {register_name}({action_type} callback)"
    ));
    w.line("{");
    w.scope(|w| {
        w.line(format!(
            "{delegate_name} trampoline = ({}) =>",
            lambda_formals.join(", ")
        ));
        w.line("{");
        w.scope(|w| {
            let mut stmts = String::new();
            let mut args = Vec::new();
            for (idx, p) in cb.params.iter().enumerate() {
                args.push(render_cb_arg(&mut stmts, p, idx, "                "));
            }
            w.raw(stmts);
            w.line(format!("callback({});", args.join(", ")));
        });
        w.line("};");
        w.line("ulong id;");
        w.line("lock (_listenerLock)");
        w.line("{");
        w.scope(|w| {
            w.line(format!(
                "id = NativeMethods.{}(trampoline, IntPtr.Zero);",
                l.register_symbol
            ));
            w.line("_listenerRefs[id] = trampoline;");
        });
        w.line("}");
        w.line("return id;");
    });
    w.line("}");
    w.blank();

    w.line(format!(
        "/// <summary>Unregisters a listener previously registered with {register_name}().</summary>"
    ));
    w.line(format!("public static void {unregister_name}(ulong id)"));
    w.line("{");
    w.scope(|w| {
        w.line(format!("NativeMethods.{}(id);", l.unregister_symbol));
        w.line("lock (_listenerLock)");
        w.line("{");
        w.scope(|w| {
            w.line("_listenerRefs.Remove(id);");
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Renders one module's static wrapper class. Submodules become sibling
/// classes named by their full path (`KvStats`, not a nested `Kv.Stats`):
/// flat classes keep generated type names (`Stats`) unambiguous, since a
/// nested module class with the same name as a struct wrapper would shadow it.
fn render_wrapper_class(out: &mut String, mb: &ModuleBinding, strip_module_prefix: bool) {
    let class_name: String = mb
        .segments
        .iter()
        .map(|s| s.to_upper_camel_case())
        .collect();
    out.push_str(&format!("    public static class {class_name}\n    {{\n"));

    if !mb.listeners.is_empty() {
        out.push_str("        private static readonly object _listenerLock = new object();\n");
        out.push_str(
            "        // Live listener delegates by subscription id. Holding the delegate\n",
        );
        out.push_str(
            "        // here keeps its native thunk alive until unregistered; without this\n",
        );
        out.push_str("        // the GC could collect a delegate the producer still calls.\n");
        out.push_str(
            "        private static readonly Dictionary<ulong, Delegate> _listenerRefs = new Dictionary<ulong, Delegate>();\n\n",
        );
        for l in &mb.listeners {
            render_listener_methods(out, mb, l, strip_module_prefix);
        }
    }
    for f in &mb.functions {
        let method_name =
            wrapper_name(&mb.path, &f.name, strip_module_prefix).to_upper_camel_case();
        let err = ErrCtx::for_fn(f, mb.error.as_ref());
        render_wrapper_method(out, f, &method_name, None, err);
    }

    out.push_str("    }\n\n");
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

/// Render one wrapper method (any shape) named `method_name`. `self_expr` is
/// the receiver's handle expression for interface instance methods (`None`
/// for free functions, statics, and factories, which render as `static`);
/// `err` selects the typed or generic error surface.
fn render_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    if f.is_async {
        render_async_wrapper_method(out, f, method_name, self_expr, err);
        return;
    }
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_wrapper_method(out, f, it, method_name, self_expr, err);
        return;
    }
    let f = camel_fn(f);
    let ret_cs = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "void".into());
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    w.line(format!(
        "public {staticness}{ret_cs} {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));

        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            let mut tmp = String::new();
            render_pinvoke_call_and_return(&mut tmp, &f, self_expr, err, "                ");
            w.raw(tmp);
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            let mut tmp = String::new();
            render_pinvoke_call_and_return(&mut tmp, &f, self_expr, err, "            ");
            w.raw(tmp);
        }
    });

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The statements converting one `_next` out-item into the yielded C# value,
/// freeing any producer-allocated memory along the way. Returns the expression
/// to `yield return`.
fn iterator_item_conversion(out: &mut String, elem: &TypeRef, indent: &str) -> String {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    let expr = match elem {
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
            w.line("var item = Marshal.PtrToStringUTF8(out_item) ?? \"\";");
            w.line("NativeMethods.weaveffi_free_string(out_item);");
            "item".into()
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("var item = new byte[(int)out_len];");
            w.line("if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, item, 0, (int)out_len);");
            w.line("NativeMethods.weaveffi_free_bytes(out_item, out_len);");
            "item".into()
        }
        // The consumer owns each yielded wrapper; Dispose() destroys it
        // (`ElemFree::Object`, adopted rather than freed eagerly).
        TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::TypedHandle(name)
        | TypeRef::Interface(name) => {
            format!("new {}(out_item)", local_type_name(name))
        }
        // A null slot is "none"; a non-null slot is an owned object pointer
        // adopted exactly like the non-optional case.
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_)
            ) =>
        {
            let cn = match inner.as_ref() {
                TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
                    local_type_name(name)
                }
                _ => unreachable!(),
            };
            format!("out_item == IntPtr.Zero ? null : new {cn}(out_item)")
        }
        other => unreachable!("unsupported iterator element type {other:?}"),
    };
    out.push_str(&w.finish());
    expr
}

/// An `iter<T>` function surfaces as `IEnumerable<T>`, rendering the
/// `IteratorProtocol` pull contract: an eager launcher call (so launch errors
/// throw immediately, per the function's `ErrorStrategy`), then a lazy
/// `yield return` enumerator issuing exactly one C `next` call per
/// `MoveNext`. Each yielded element is released per its `ElemFree` plan after
/// conversion, and the compiler-generated `finally` destroys the native
/// iterator exactly once, whether enumeration runs to exhaustion or is
/// abandoned early (C# `foreach` disposes the enumerator). Wrapping the
/// single enumerator in `WeaveFFIOnceEnumerable` makes a second enumeration
/// throw instead of double-destroying the consumed handle.
fn render_iterator_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    let f = camel_fn(f);
    let elem_cs = cs_type(&it.elem);
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let call_args = full_call_args(&f, self_expr);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let launch_call = format!(
        "var iter = NativeMethods.{}({args_part}ref err);",
        it.launch.symbol
    );

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    w.line("/// <remarks>Streams lazily: each element is pulled from the native");
    w.line("/// iterator on demand, and the iterator is destroyed when enumeration");
    w.line("/// completes or the enumerator is disposed (a <c>foreach</c> disposes it");
    w.line("/// automatically, including on early exit). The returned sequence can be");
    w.line("/// enumerated only once.</remarks>");
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    let wrap_return =
        format!("return new WeaveFFIOnceEnumerable<{elem_cs}>(Enumerate{method_name}(iter));");
    w.line(format!(
        "public {staticness}IEnumerable<{elem_cs}> {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(launch_call.clone());
                w.line(err.check_stmt());
                w.line(wrap_return.clone());
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line(launch_call.clone());
            w.line(err.check_stmt());
            w.line(wrap_return.clone());
        }
    });
    w.line("}");
    w.blank();

    // The `_next` out-slots after the iterator handle, excluding the error.
    let next_out_args: Vec<String> = it
        .next
        .params
        .iter()
        .skip(1)
        .filter(|slot| !is_error_slot(slot))
        .map(|slot| format!("out var {}", slot.name))
        .collect();

    // A `yield return` iterator method: the compiler emits the `finally`
    // into Dispose(), so the destroy below runs exactly once, on exhaustion
    // or when the consumer abandons enumeration early.
    w.line(format!(
        "private static IEnumerator<{elem_cs}> Enumerate{method_name}(IntPtr iter)"
    ));
    w.line("{");
    w.scope(|w| {
        w.line("try");
        w.line("{");
        w.scope(|w| {
            w.line("while (true)");
            w.line("{");
            w.scope(|w| {
                w.line("var iterErr = new WeaveFFIError();");
                w.line(format!(
                    "if (NativeMethods.{}(iter, {}, ref iterErr) == 0)",
                    it.next.symbol,
                    next_out_args.join(", ")
                ));
                w.line("{");
                w.scope(|w| {
                    w.line(err.check_stmt_for("iterErr"));
                    w.line("yield break;");
                });
                w.line("}");
                w.line(err.check_stmt_for("iterErr"));
                let mut conv = String::new();
                let item_expr =
                    iterator_item_conversion(&mut conv, &it.elem, "                    ");
                w.raw(conv);
                w.line(format!("yield return {item_expr};"));
            });
            w.line("}");
        });
        w.line("}");
        w.line("finally");
        w.line("{");
        w.scope(|w| {
            w.line(format!("NativeMethods.{}(iter);", it.destroy_symbol));
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render an async wrapper returning `Task`/`Task<T>` via a
/// `TaskCompletionSource` resolved from the native completion callback. A
/// non-zero error slot faults the task with the typed or generic exception
/// according to `err`.
fn render_async_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    let f = camel_fn(f);
    let c_sym = &f.c_base;
    let delegate_name = format!("NativeMethods.AsyncCb_{c_sym}");
    let staticness = if self_expr.is_none() { "static " } else { "" };

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

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    w.line(format!(
        "public {staticness}async {task_ret} {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line(format!(
            "var tcs = new TaskCompletionSource<{tcs_type}>(TaskCreationOptions.RunContinuationsAsynchronously);"
        ));

        let cb_lambda_params = async_cb_lambda_params(&f.ret);
        w.line(format!("{delegate_name} callback = {cb_lambda_params} =>"));
        w.line("{");
        w.scope(|w| {
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line("if (err != IntPtr.Zero)");
                w.line("{");
                w.scope(|w| {
                    w.line("var wErr = Marshal.PtrToStructure<WeaveFFIError>(err);");
                    w.line("if (wErr.Code != 0)");
                    w.line("{");
                    w.scope(|w| {
                        w.line("var msg = Marshal.PtrToStringUTF8(wErr.Message) ?? \"\";");
                        w.line(format!("tcs.SetException({});", err.async_exception_expr()));
                        w.line("return;");
                    });
                    w.line("}");
                });
                w.line("}");

                let mut tmp = String::new();
                render_async_set_result(&mut tmp, &f.ret, "                    ");
                w.raw(tmp);
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            w.scope(|w| {
                w.line("if (context != IntPtr.Zero)");
                w.line("{");
                w.scope(|w| {
                    w.line("GCHandle.FromIntPtr(context).Free();");
                });
                w.line("}");
            });
            w.line("}");
        });
        w.line("};");
        w.line("var gcHandle = GCHandle.Alloc(callback, GCHandleType.Normal);");
        w.line("var ctx = GCHandle.ToIntPtr(gcHandle);");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
        let call_args = full_call_args(&f, self_expr);
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let cancel_arg = if f.cancellable { "IntPtr.Zero, " } else { "" };

        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line("try");
                w.line("{");
                w.scope(|w| {
                    w.line(format!(
                        "NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);"
                    ));
                });
                w.line("}");
                w.line("catch");
                w.line("{");
                w.scope(|w| {
                    w.line("if (gcHandle.IsAllocated) gcHandle.Free();");
                    w.line("throw;");
                });
                w.line("}");
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(format!(
                    "NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);"
                ));
            });
            w.line("}");
            w.line("catch");
            w.line("{");
            w.scope(|w| {
                w.line("if (gcHandle.IsAllocated) gcHandle.Free();");
                w.line("throw;");
            });
            w.line("}");
        }

        if f.ret.is_some() {
            w.line("return await tcs.Task;");
        } else {
            w.line("await tcs.Task;");
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the statements resolving the `TaskCompletionSource` from the
/// completion callback's result slots, honoring the `AsyncProtocol` borrowed
/// results clause: string, bytes, array, and map buffers (and producer-boxed
/// optional scalars) are owned by the producer and valid only for the
/// callback's duration, so they are deep-copied here and never freed.
/// Owned-object results (records, rich enums, interfaces, typed handles) are
/// the exception: the callback receives ownership and the wrapper adopts the
/// pointer, as do object pointers inside a list result (its `ElemFree`
/// plan).
fn render_async_set_result(out: &mut String, ret: &Option<TypeRef>, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match ret {
        None => {
            w.line("tcs.SetResult(true);");
        }
        Some(TypeRef::Bool) => {
            w.line("tcs.SetResult(result != 0);");
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            w.line("tcs.SetResult(Marshal.PtrToStringUTF8(result) ?? \"\");");
        }
        Some(TypeRef::Enum(name)) => {
            let cn = local_type_name(name);
            w.line(format!("tcs.SetResult(({cn})result);"));
        }
        Some(
            TypeRef::Record(name)
            | TypeRef::RichEnum(name)
            | TypeRef::TypedHandle(name)
            | TypeRef::Interface(name),
        ) => {
            let cn = local_type_name(name);
            w.line(format!("tcs.SetResult(new {cn}(result));"));
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            w.line("var arr = new byte[(int)resultLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)resultLen > 0) Marshal.Copy(result, arr, 0, (int)resultLen);",
            );
            w.line("tcs.SetResult(arr);");
        }
        Some(TypeRef::List(inner)) => {
            let elem = cs_type(inner);
            w.line(format!("var arr = new {elem}[(int)resultLen];"));
            w.line("if (result != IntPtr.Zero)");
            w.block("{", "}", |w| {
                w.line("for (int i = 0; i < (int)resultLen; i++)");
                w.block("{", "}", |w| {
                    w.line(format!(
                        "arr[i] = {};",
                        marshal_read_element(inner, "result", "i")
                    ));
                });
            });
            w.line("tcs.SetResult(arr);");
        }
        Some(TypeRef::Map(k, v)) => {
            let k_cs = cs_type(k);
            let v_cs = cs_type(v);
            w.line(format!("var dict = new Dictionary<{k_cs}, {v_cs}>();"));
            w.line("if (resultKeys != IntPtr.Zero && resultValues != IntPtr.Zero)");
            w.block("{", "}", |w| {
                w.line("for (int i = 0; i < (int)resultLen; i++)");
                w.block("{", "}", |w| {
                    w.line(format!(
                        "dict[{}] = {};",
                        marshal_read_element(k, "resultKeys", "i"),
                        marshal_read_element(v, "resultValues", "i")
                    ));
                });
            });
            w.line("tcs.SetResult(dict);");
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(
                    "tcs.SetResult(result == IntPtr.Zero ? null : Marshal.PtrToStringUTF8(result));",
                );
            }
            TypeRef::Record(name)
            | TypeRef::RichEnum(name)
            | TypeRef::TypedHandle(name)
            | TypeRef::Interface(name) => {
                let cn = local_type_name(name);
                w.line(format!(
                    "tcs.SetResult(result == IntPtr.Zero ? null : new {cn}(result));"
                ));
            }
            other => {
                // A producer-boxed scalar, borrowed like every other result
                // buffer: dereference the copy, leave the box alone.
                let Some((read, _)) = boxed_scalar_read(other, "result") else {
                    unreachable!("unsupported optional async result type");
                };
                let cs = cs_type(ret.as_ref().unwrap());
                w.line(format!(
                    "tcs.SetResult(result == IntPtr.Zero ? ({cs})null : {read});"
                ));
            }
        },
        // Remaining scalars pass by value in the result slot.
        Some(_) => {
            w.line("tcs.SetResult(result);");
        }
    }
    out.push_str(&w.finish());
}

fn render_marshal_setup(out: &mut String, p: &ParamBinding, indent: &str) {
    let name = safe_cs_name(&p.name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "var {name}Ptr = Marshal.StringToCoTaskMemUTF8({name});"
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "var {name}Pin = GCHandle.Alloc({name}, GCHandleType.Pinned);"
            ));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!(
                    "var {name}Ptr = {name} != null ? Marshal.StringToCoTaskMemUTF8({name}) : IntPtr.Zero;"
                ));
            }
            // A boxed C `bool` is one byte, matching the pointee the
            // producer dereferences.
            TypeRef::Bool => {
                w.line(format!("var {name}Ptr = IntPtr.Zero;"));
                w.line(format!("if ({name}.HasValue)"));
                w.block("{", "}", |w| {
                    w.line(format!("{name}Ptr = Marshal.AllocHGlobal(sizeof(byte));"));
                    w.line(format!(
                        "Marshal.WriteByte({name}Ptr, (byte)({name}.Value ? 1 : 0));"
                    ));
                });
            }
            TypeRef::I32 | TypeRef::Enum(_) | TypeRef::U32 => {
                w.line(format!("var {name}Ptr = IntPtr.Zero;"));
                w.line(format!("if ({name}.HasValue)"));
                w.block("{", "}", |w| {
                    w.line(format!("{name}Ptr = Marshal.AllocHGlobal(sizeof(int));"));
                    let val = match inner.as_ref() {
                        TypeRef::Enum(_) => format!("(int){name}.Value"),
                        TypeRef::U32 => format!("(int){name}.Value"),
                        _ => format!("{name}.Value"),
                    };
                    w.line(format!("Marshal.WriteInt32({name}Ptr, {val});"));
                });
            }
            TypeRef::I64 | TypeRef::U64 | TypeRef::Handle | TypeRef::F64 => {
                w.line(format!("var {name}Ptr = IntPtr.Zero;"));
                w.line(format!("if ({name}.HasValue)"));
                w.block("{", "}", |w| {
                    w.line(format!("{name}Ptr = Marshal.AllocHGlobal(sizeof(long));"));
                    let val = match inner.as_ref() {
                        TypeRef::Handle => format!("(long){name}.Value"),
                        TypeRef::U64 => format!("(long){name}.Value"),
                        TypeRef::F64 => {
                            format!("BitConverter.DoubleToInt64Bits({name}.Value)")
                        }
                        _ => format!("{name}.Value"),
                    };
                    w.line(format!("Marshal.WriteInt64({name}Ptr, {val});"));
                });
            }
            TypeRef::I8 | TypeRef::U8 => {
                w.line(format!("var {name}Ptr = IntPtr.Zero;"));
                w.line(format!("if ({name}.HasValue)"));
                w.block("{", "}", |w| {
                    w.line(format!("{name}Ptr = Marshal.AllocHGlobal(sizeof(byte));"));
                    w.line(format!("Marshal.WriteByte({name}Ptr, (byte){name}.Value);"));
                });
            }
            TypeRef::I16 | TypeRef::U16 => {
                w.line(format!("var {name}Ptr = IntPtr.Zero;"));
                w.line(format!("if ({name}.HasValue)"));
                w.block("{", "}", |w| {
                    w.line(format!("{name}Ptr = Marshal.AllocHGlobal(sizeof(short));"));
                    w.line(format!(
                        "Marshal.WriteInt16({name}Ptr, (short){name}.Value);"
                    ));
                });
            }
            TypeRef::F32 => {
                w.line(format!("var {name}Ptr = IntPtr.Zero;"));
                w.line(format!("if ({name}.HasValue)"));
                w.block("{", "}", |w| {
                    w.line(format!("{name}Ptr = Marshal.AllocHGlobal(sizeof(float));"));
                    w.line(format!(
                        "Marshal.WriteInt32({name}Ptr, BitConverter.SingleToInt32Bits({name}.Value));"
                    ));
                });
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                w.line(format!(
                    "var {name}Pin = {name} != null ? GCHandle.Alloc({name}, GCHandleType.Pinned) : default;"
                ));
            }
            _ => {}
        },
        TypeRef::List(elem) => {
            let mut tmp = String::new();
            render_array_marshal_setup(&mut tmp, &name, &format!("{name}.Length"), elem, indent);
            w.raw(tmp);
        }
        TypeRef::Map(k, v) => {
            // Parallel key/value arrays in dictionary iteration order.
            let (k_arr, k_conv) = cs_elem_array_slot(k, "kv.Key");
            let (v_arr, v_conv) = cs_elem_array_slot(v, "kv.Value");
            w.line(format!("var {name}KeysArr = new {k_arr}[{name}.Count];"));
            w.line(format!("var {name}ValsArr = new {v_arr}[{name}.Count];"));
            w.line(format!("var {name}I = 0;"));
            w.line(format!("foreach (var kv in {name})"));
            w.block("{", "}", |w| {
                w.line(format!("{name}KeysArr[{name}I] = {k_conv};"));
                w.line(format!("{name}ValsArr[{name}I] = {v_conv};"));
                w.line(format!("{name}I++;"));
            });
            w.line(format!(
                "var {name}KeysPin = GCHandle.Alloc({name}KeysArr, GCHandleType.Pinned);"
            ));
            w.line(format!(
                "var {name}ValsPin = GCHandle.Alloc({name}ValsArr, GCHandleType.Pinned);"
            ));
        }
        _ => {}
    }
    out.push_str(&w.finish());
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
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match elem {
        // Blittable element arrays pin in place; no conversion copy needed.
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Handle => {
            w.line(format!("var {name}Arr = {name};"));
        }
        _ => {
            let (arr_ty, conv) = cs_elem_array_slot(elem, &format!("{name}[{name}It]"));
            w.line(format!("var {name}Arr = new {arr_ty}[{len_expr}];"));
            w.line(format!(
                "for (var {name}It = 0; {name}It < {len_expr}; {name}It++) {name}Arr[{name}It] = {conv};"
            ));
        }
    }
    w.line(format!(
        "var {name}Pin = GCHandle.Alloc({name}Arr, GCHandleType.Pinned);"
    ));
    out.push_str(&w.finish());
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
        // C `bool` array slots are one byte each.
        TypeRef::Bool => ("byte".into(), format!("(byte)({expr} ? 1 : 0)")),
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
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("Marshal.FreeCoTaskMem({name}Ptr);"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("{name}Pin.Free();"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                w.line(format!(
                    "if ({name}Ptr != IntPtr.Zero) Marshal.FreeCoTaskMem({name}Ptr);"
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
                w.line(format!(
                    "if ({name}Ptr != IntPtr.Zero) Marshal.FreeHGlobal({name}Ptr);"
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                w.line(format!("if ({name} != null) {name}Pin.Free();"));
            }
            _ => {}
        },
        TypeRef::List(elem) => {
            w.line(format!("{name}Pin.Free();"));
            if cs_elem_allocates(elem) {
                w.line(format!(
                    "foreach (var {name}P in {name}Arr) Marshal.FreeCoTaskMem({name}P);"
                ));
            }
        }
        TypeRef::Map(k, v) => {
            w.line(format!("{name}KeysPin.Free();"));
            w.line(format!("{name}ValsPin.Free();"));
            if cs_elem_allocates(k) {
                w.line(format!(
                    "foreach (var {name}KP in {name}KeysArr) Marshal.FreeCoTaskMem({name}KP);"
                ));
            }
            if cs_elem_allocates(v) {
                w.line(format!(
                    "foreach (var {name}VP in {name}ValsArr) Marshal.FreeCoTaskMem({name}VP);"
                ));
            }
        }
        _ => {}
    }
    out.push_str(&w.finish());
}

/// The joined native-call argument list: the implicit self handle (when
/// `self_expr` is given) followed by every lowered parameter slot.
fn full_call_args(f: &FnBinding, self_expr: Option<&str>) -> String {
    let args = build_call_args(&f.params);
    match self_expr {
        Some(s) if args.is_empty() => s.to_string(),
        Some(s) => format!("{s}, {args}"),
        None => args,
    }
}

fn render_pinvoke_call_and_return(
    out: &mut String,
    f: &FnBinding,
    self_expr: Option<&str>,
    err: ErrCtx,
    indent: &str,
) {
    let c_sym = &f.c_base;
    let call_args = full_call_args(f, self_expr);

    if let Some(TypeRef::Map(k, v)) = &f.ret {
        render_map_return_call(out, c_sym, &call_args, k, v, err, indent);
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

    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if f.ret.is_some() {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let out_len_part = if has_out_len { "out var outLen, " } else { "" };
        w.line(format!(
            "var result = NativeMethods.{c_sym}({args_part}{out_len_part}ref err);"
        ));
    } else {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        w.line(format!("NativeMethods.{c_sym}({args_part}ref err);"));
    }

    w.line(err.check_stmt());
    out.push_str(&w.finish());

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
                TypeRef::Bool => vec![format!("(byte)({name} ? 1 : 0)")],
                TypeRef::Enum(_) => vec![format!("(int){name}")],
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("{name}Ptr")],
                // Object parameters borrow: pass the handle, ownership
                // stays with the caller's wrapper.
                TypeRef::Record(_)
                | TypeRef::RichEnum(_)
                | TypeRef::TypedHandle(_)
                | TypeRef::Interface(_) => {
                    vec![format!("{name}.Handle")]
                }
                TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
                    format!("{name}Pin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}.Length"),
                ],
                TypeRef::Optional(inner) => match inner.as_ref() {
                    TypeRef::Record(_)
                    | TypeRef::RichEnum(_)
                    | TypeRef::TypedHandle(_)
                    | TypeRef::Interface(_) => {
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
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match ty {
        TypeRef::Bool => {
            w.line("return result != 0;");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("var str = Marshal.PtrToStringUTF8(result);");
            w.line("NativeMethods.weaveffi_free_string(result);");
            w.line("return str;");
        }
        TypeRef::Enum(name) => {
            let cn = local_type_name(name);
            w.line(format!("return ({cn})result;"));
        }
        // An owned object return (`ReturnFree::OwnedObject`): the wrapper
        // adopts the pointer and its Dispose() calls the destroy symbol.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            let cn = local_type_name(name);
            w.line(format!("return new {cn}(result);"));
        }
        TypeRef::TypedHandle(name) => {
            let cn = local_type_name(name);
            w.line(format!("return new {cn}(result);"));
        }
        // An interface return transfers ownership: wrap the pointer in a new
        // instance whose Dispose() releases it.
        TypeRef::Interface(name) => {
            let cn = local_type_name(name);
            w.line(format!("return new {cn}(result);"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("if (result == IntPtr.Zero) return Array.Empty<byte>();");
            w.line("var arr = new byte[(int)outLen];");
            w.line("Marshal.Copy(result, arr, 0, (int)outLen);");
            w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
            w.line("return arr;");
        }
        TypeRef::Optional(inner) => {
            out.push_str(&w.finish());
            render_optional_return_conversion(out, inner, indent);
            return;
        }
        TypeRef::List(inner) => {
            out.push_str(&w.finish());
            render_list_return(out, inner, indent);
            return;
        }
        TypeRef::Iterator(_) => unreachable!("iterator functions render via CallShape::Iterator"),
        TypeRef::Map(_, _) => {}
        _ => {
            w.line("return result;");
        }
    }
    out.push_str(&w.finish());
}

/// Renders an `T?` return: null pointer means none; pointer optionals reuse
/// the inner type's plan (owned strings and buffers freed after copying,
/// object pointers adopted); scalar optionals are producer-boxed
/// (`ReturnFree::BoxedScalar`), so the box is dereferenced and then released
/// with `weaveffi_free_bytes`.
fn render_optional_return_conversion(out: &mut String, inner: &TypeRef, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("if (result == IntPtr.Zero) return null;");
            w.line("var str = Marshal.PtrToStringUTF8(result);");
            w.line("NativeMethods.weaveffi_free_string(result);");
            w.line("return str;");
        }
        TypeRef::Record(name)
        | TypeRef::RichEnum(name)
        | TypeRef::Interface(name)
        | TypeRef::TypedHandle(name) => {
            let cn = local_type_name(name);
            w.line(format!(
                "return result == IntPtr.Zero ? null : new {cn}(result);"
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("if (result == IntPtr.Zero) return null;");
            w.line("var arr = new byte[(int)outLen];");
            w.line("Marshal.Copy(result, arr, 0, (int)outLen);");
            w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
            w.line("return arr;");
        }
        other => {
            let Some((read, size)) = boxed_scalar_read(other, "result") else {
                unreachable!("unsupported optional return type");
            };
            w.line("if (result == IntPtr.Zero) return null;");
            w.line(format!("var value = {read};"));
            w.line(format!(
                "NativeMethods.weaveffi_free_bytes(result, (UIntPtr){size});"
            ));
            w.line("return value;");
        }
    }
    out.push_str(&w.finish());
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
    err: ErrCtx,
    indent: &str,
) {
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    w.line(format!(
        "NativeMethods.{c_sym}({args_part}out var outKeys, out var outValues, out var outLen, ref err);"
    ));
    w.line(err.check_stmt());
    out.push_str(&w.finish());
    render_map_decode(out, k, v, "outKeys", "outValues", "outLen", indent);
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
        // C `bool` array slots are one byte each.
        TypeRef::Bool => {
            format!("Marshal.ReadByte({arr} + {idx} * 1) != 0")
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
        // Owned object pointer slots, adopted by their wrappers.
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::Interface(name) => {
            let cn = local_type_name(name);
            format!("new {cn}(Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size))")
        }
        // An optional pointer element occupies one pointer slot; null = none.
        // The double read keeps this a single expression.
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::Record(_)
                    | TypeRef::RichEnum(_)
                    | TypeRef::Interface(_)
                    | TypeRef::TypedHandle(_)
            ) =>
        {
            let cn = match inner.as_ref() {
                TypeRef::Record(name)
                | TypeRef::RichEnum(name)
                | TypeRef::Interface(name)
                | TypeRef::TypedHandle(name) => local_type_name(name),
                _ => unreachable!(),
            };
            format!(
                "Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size) == IntPtr.Zero ? null : new {cn}(Marshal.ReadIntPtr({arr}, {idx} * IntPtr.Size))"
            )
        }
        other => unreachable!("unsupported element type {other:?}"),
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

    /// Test shim matching the pre-0.5.0 signature: builds the [`BindingModel`]
    /// here so the production `render_csharp` stays model-only.
    fn render_csharp(
        api: &Api,
        namespace: &str,
        strip_module_prefix: bool,
        prefix: &str,
        input_basename: &str,
        filename: &str,
    ) -> String {
        let model = BindingModel::build(api, prefix);
        super::render_csharp(
            &model,
            namespace,
            strip_module_prefix,
            input_basename,
            filename,
        )
    }

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
            throws: false,
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
            version: "0.5.0".into(),
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
            interfaces: vec![],
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
            throws: false,
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
            interfaces: vec![],
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
            cs.contains("public static ulong RegisterMessageListener(Action<string> callback)"),
            "register wrapper missing: {cs}"
        );
        assert!(
            cs.contains("public static void UnregisterMessageListener(ulong id)"),
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
            version: "0.5.0".into(),
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
                interfaces: vec![],
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
        assert_eq!(cs_type(&TypeRef::Record("Foo".into())), "Foo");
        assert_eq!(cs_type(&TypeRef::Enum("Bar".into())), "Bar");
        assert_eq!(cs_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "int?");
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "string?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Record("X".into())))),
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
        // C `bool` is one byte, not int-widened.
        assert_eq!(pinvoke_type(&TypeRef::Bool), "byte");
        assert_eq!(pinvoke_type(&TypeRef::RichEnum("Foo".into())), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::StringUtf8), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Handle), "ulong");
        assert_eq!(pinvoke_type(&TypeRef::Bytes), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Record("Foo".into())), "IntPtr");
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
            throws: false,
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
            throws: false,
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
            throws: false,
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
                throws: false,
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
            interfaces: vec![],
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
            interfaces: vec![],
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
            interfaces: vec![],
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
            interfaces: vec![],
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
            interfaces: vec![],
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
            throws: false,
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
            throws: false,
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
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Record("Bar".into())))),
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
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
        // The producer-owned array buffer is released after the copy
        // (`ReturnFree::Array`): len * sizeof(int32).
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, (UIntPtr)((int)outLen * 4));"),
            "missing array buffer release: {cs}"
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
        // Both producer-owned parallel buffers are released after the copy
        // (`ReturnFree::MapBuffers`): i32 keys and f64 values.
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(outKeys, (UIntPtr)((int)outLen * 4));"),
            "missing key buffer release: {cs}"
        );
        assert!(
            cs.contains(
                "NativeMethods.weaveffi_free_bytes(outValues, (UIntPtr)((int)outLen * 8));"
            ),
            "missing value buffer release: {cs}"
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
            interfaces: vec![],
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
                    throws: false,
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
                    returns: Some(TypeRef::Record("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            interfaces: vec![],
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
            throws: false,
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
            interfaces: vec![],
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
                throws: false,
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
            interfaces: vec![],
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
                throws: false,
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
            interfaces: vec![],
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
            cs.contains("Marshal.ReadByte(ptr) != 0"),
            "missing optional bool unmarshal: {cs}"
        );
        assert!(
            cs.contains("BitConverter.Int64BitsToDouble(Marshal.ReadInt64(ptr))"),
            "missing optional f64 unmarshal: {cs}"
        );
        // Producer-boxed optional scalars are freed after the dereference
        // (the `ReturnFree::BoxedScalar` contract), both in field getters
        // and in optional scalar returns.
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(ptr, (UIntPtr)1);"),
            "missing boxed bool release: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, (UIntPtr)8);"),
            "missing boxed i64 return release: {cs}"
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
                    throws: false,
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
                    throws: false,
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
                    throws: false,
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
            interfaces: vec![],
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
                    throws: false,
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
                    returns: Some(TypeRef::Record("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    throws: false,
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
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            interfaces: vec![],
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
            throws: false,
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        // Stripping is the default: the per-module static class already
        // namespaces the method.
        let config = DotnetConfig::default();

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

        let no_strip = DotnetConfig {
            strip_module_prefix: false,
            ..DotnetConfig::default()
        };
        let tmp2 = std::env::temp_dir().join("weaveffi_test_dotnet_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir2, &no_strip).unwrap();

        let cs2 = std::fs::read_to_string(tmp2.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(
            cs2.contains("ContactsCreateContact("),
            "strip_module_prefix: false should restore module-prefixed names: {cs2}"
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
                        Box::new(TypeRef::Record("Contact".into())),
                    ))))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            interfaces: vec![],
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
                        Box::new(TypeRef::Record("Contact".into())),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            interfaces: vec![],
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
            version: "0.5.0".into(),
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
                    throws: false,
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
                interfaces: vec![],
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
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            interfaces: vec![],
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            interfaces: vec![],
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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

    /// A module with one async function per given return type, named `run0`,
    /// `run1`, ... in order, plus a `Contact` record for object results.
    fn async_api(returns: Vec<Option<TypeRef>>) -> Api {
        let functions = returns
            .into_iter()
            .enumerate()
            .map(|(i, ret)| Function {
                name: format!("run{i}"),
                params: vec![],
                returns: ret,
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            })
            .collect();
        make_api(vec![Module {
            name: "tasks".into(),
            functions,
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }])
    }

    /// Async result buffers are borrowed for the callback's duration
    /// (`AsyncProtocol` clause 2): strings and bytes are deep-copied inside
    /// the callback and never freed by the consumer.
    #[test]
    fn dotnet_async_borrowed_results_copied_never_freed() {
        let cs = render_csharp(
            &async_api(vec![
                Some(TypeRef::StringUtf8),
                Some(TypeRef::Bytes),
                Some(TypeRef::Optional(Box::new(TypeRef::I64))),
            ]),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // String result: copied, not freed.
        assert!(
            cs.contains("tcs.SetResult(Marshal.PtrToStringUTF8(result) ?? \"\");"),
            "async string result must copy: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_free_string(result)"),
            "async string result must not be freed by the consumer: {cs}"
        );
        // Bytes result: copied via the (result, resultLen) pair, not freed.
        assert!(
            cs.contains("Marshal.Copy(result, arr, 0, (int)resultLen);"),
            "async bytes result must copy: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_free_bytes(result"),
            "async bytes result must not be freed by the consumer: {cs}"
        );
        // Boxed optional scalar result: dereferenced, box left alone.
        assert!(
            cs.contains(
                "tcs.SetResult(result == IntPtr.Zero ? (long?)null : Marshal.ReadInt64(result));"
            ),
            "async optional scalar result must dereference the borrowed box: {cs}"
        );
    }

    /// Owned-object async results are the exception to the borrowed-results
    /// rule: the callback receives ownership and the wrapper adopts the
    /// pointer. List results copy the borrowed buffer, adopting object
    /// elements and copying string elements without freeing them.
    #[test]
    fn dotnet_async_object_and_list_results() {
        let cs = render_csharp(
            &async_api(vec![
                Some(TypeRef::Record("Contact".into())),
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
            ]),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // Record result adopted by its wrapper.
        assert!(
            cs.contains("tcs.SetResult(new Contact(result));"),
            "async record result must be adopted: {cs}"
        );
        // List results decode the two-slot (result, resultLen) pair; string
        // elements are copied without a consumer-side free, record elements
        // adopted.
        assert!(
            cs.contains("IntPtr result, UIntPtr resultLen"),
            "async list delegate must carry the length slot: {cs}"
        );
        assert!(
            cs.contains(
                "arr[i] = Marshal.PtrToStringUTF8(Marshal.ReadIntPtr(result, i * IntPtr.Size)) ?? \"\";"
            ),
            "async string list elements must copy: {cs}"
        );
        assert!(
            cs.contains("arr[i] = new Contact(Marshal.ReadIntPtr(result, i * IntPtr.Size));"),
            "async record list elements must be adopted: {cs}"
        );
        // No release calls anywhere in this API: every native buffer here is
        // an async result, borrowed for the callback's duration.
        assert!(
            !cs.contains("NativeMethods.weaveffi_free_string(")
                && !cs.contains("NativeMethods.weaveffi_free_bytes("),
            "async list/map buffers are borrowed and must not be freed: {cs}"
        );
        // Map results decode the three-slot parallel-buffer form.
        assert!(
            cs.contains("IntPtr resultKeys, IntPtr resultValues, UIntPtr resultLen"),
            "async map delegate must carry both buffers: {cs}"
        );
        assert!(
            cs.contains(
                "dict[Marshal.PtrToStringUTF8(Marshal.ReadIntPtr(resultKeys, i * IntPtr.Size)) ?? \"\"] = Marshal.ReadInt32(resultValues + i * sizeof(int));"
            ),
            "async map entries must copy: {cs}"
        );
        assert!(
            cs.contains("tcs.SetResult(dict);"),
            "async map result must resolve the task: {cs}"
        );
    }

    /// The iterator contract (`IteratorProtocol`): the sequence streams
    /// through a single `yield return` enumerator (one C `next` per
    /// `MoveNext`), frees each string element after conversion, destroys the
    /// native iterator exactly once from the compiler-generated `finally`,
    /// and refuses a second enumeration instead of double-destroying.
    #[test]
    fn iterator_streams_lazily_and_destroys_once() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // The single-use wrapper class and the wrapped return.
        assert!(
            cs.contains("internal sealed class WeaveFFIOnceEnumerable<T> : IEnumerable<T>"),
            "once-enumerable class missing: {cs}"
        );
        assert!(
            cs.contains("return new WeaveFFIOnceEnumerable<string>(EnumerateListKeys(iter));"),
            "iterator wrapper must return the once-enumerable: {cs}"
        );
        assert!(
            cs.contains("this sequence can be enumerated only once"),
            "second enumeration must throw: {cs}"
        );
        // One C next call per MoveNext, inside a lazy yield-return method.
        assert_eq!(
            cs.matches(
                "weaveffi_kv_Store_ListKeysIterator_next(iter, out var out_item, ref iterErr)"
            )
            .count(),
            1,
            "exactly one next call site expected: {cs}"
        );
        assert!(
            cs.contains("yield return item;"),
            "enumerator must stream lazily: {cs}"
        );
        // Each yielded string is freed after conversion (ElemFree::String).
        assert!(
            cs.contains("NativeMethods.weaveffi_free_string(out_item);"),
            "string elements must be freed: {cs}"
        );
        // Destroy exactly once, from the enumerator's finally (which C#'s
        // foreach reaches through Dispose() on early abandonment too).
        assert_eq!(
            cs.matches("NativeMethods.weaveffi_kv_Store_ListKeysIterator_destroy(iter);")
                .count(),
            1,
            "exactly one destroy call site expected: {cs}"
        );
        assert!(cs.contains("finally"), "destroy must run in finally: {cs}");
    }

    /// A list-of-strings return frees each element with
    /// `weaveffi_free_string` after copying, then releases the array buffer
    /// with `weaveffi_free_bytes` (`ReturnFree::Array` with
    /// `ElemFree::String`).
    #[test]
    fn string_list_return_frees_elements_and_buffer() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "names".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
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
            cs.contains("var itemPtr = Marshal.ReadIntPtr(result, i * IntPtr.Size);")
                && cs.contains("NativeMethods.weaveffi_free_string(itemPtr);"),
            "string elements must be freed after copying: {cs}"
        );
        assert!(
            cs.contains(
                "NativeMethods.weaveffi_free_bytes(result, (UIntPtr)((int)outLen * IntPtr.Size));"
            ),
            "array buffer must be released: {cs}"
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![Module {
                name: "child".to_string(),
                functions: vec![Function {
                    name: "inner_fn".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                interfaces: vec![],
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
            throws: false,
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
                throws: false,
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
            interfaces: vec![],
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
            throws: false,
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
                        ty: TypeRef::RichEnum("Shape".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "scale".into(),
                    params: vec![
                        Param {
                            name: "shape".into(),
                            ty: TypeRef::RichEnum("Shape".into()),
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
                    returns: Some(TypeRef::RichEnum("Shape".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![shape, channel],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
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

    /// A `kv` module exercising the 0.5.0 surface: a declared error domain, a
    /// `Store` interface (real ctor, named factory, sync/iterator/async
    /// methods, a static), and free functions with mixed `throws`.
    fn kv_api() -> Api {
        use weaveffi_ir::ir::{ErrorCode, ErrorDomain, InterfaceDef};
        let f = |name: &str,
                 params: Vec<Param>,
                 returns: Option<TypeRef>,
                 throws: bool,
                 is_async: bool,
                 cancellable: bool| Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws,
            r#async: is_async,
            cancellable,
            deprecated: None,
            since: None,
        };
        let p = |name: &str, ty: TypeRef| Param {
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
        };
        make_api(vec![Module {
            name: "kv".into(),
            functions: vec![
                f(
                    "lookup_store",
                    vec![p("store", TypeRef::Interface("Store".into()))],
                    Some(TypeRef::U64),
                    true,
                    false,
                    false,
                ),
                f("ping", vec![], Some(TypeRef::Bool), false, false, false),
            ],
            interfaces: vec![InterfaceDef {
                name: "Store".into(),
                doc: Some("A key-value store.".into()),
                constructors: vec![
                    f(
                        "new",
                        vec![p("path", TypeRef::StringUtf8)],
                        None,
                        true,
                        false,
                        false,
                    ),
                    f(
                        "open_readonly",
                        vec![p("path", TypeRef::StringUtf8)],
                        None,
                        false,
                        false,
                        false,
                    ),
                ],
                methods: vec![
                    f(
                        "get",
                        vec![p("store_key", TypeRef::StringUtf8)],
                        Some(TypeRef::StringUtf8),
                        true,
                        false,
                        false,
                    ),
                    f("count", vec![], Some(TypeRef::U64), false, false, false),
                    f(
                        "list_keys",
                        vec![],
                        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                        true,
                        false,
                        false,
                    ),
                    f("compact", vec![], None, true, true, true),
                ],
                statics: vec![f(
                    "default_capacity",
                    vec![],
                    Some(TypeRef::U32),
                    false,
                    false,
                    false,
                )],
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    ErrorCode {
                        name: "KEY_NOT_FOUND".into(),
                        code: 1001,
                        message: "Key not found".into(),
                        doc: None,
                    },
                    ErrorCode {
                        name: "IO_ERROR".into(),
                        code: 1004,
                        message: "I/O failure".into(),
                        doc: Some("Underlying storage failed.".into()),
                    },
                ],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn typed_exception_rendering() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // The domain exception extends the generic brand exception and drops
        // the doubled suffix (KvException, not KvErrorException).
        assert!(
            cs.contains("public class KvException : WeaveFFIException"),
            "typed exception class missing: {cs}"
        );
        assert!(
            !cs.contains("KvErrorException"),
            "doubled suffix must not appear: {cs}"
        );
        // Codes surface as PascalCase constants with their ABI values.
        assert!(
            cs.contains("public const int KeyNotFound = 1001;"),
            "code constant missing: {cs}"
        );
        assert!(
            cs.contains("public const int IoError = 1004;"),
            "code constant missing: {cs}"
        );
        // FromCode maps known codes to the typed exception and falls back to
        // the generic exception for unknown codes, with the default message
        // filling an empty slot message.
        assert!(
            cs.contains("internal static WeaveFFIException FromCode(int code, string message)"),
            "FromCode factory missing: {cs}"
        );
        assert!(
            cs.contains("case KeyNotFound:")
                && cs.contains(
                    "return new KvException(code, string.IsNullOrEmpty(message) ? \"Key not found\" : message);"
                ),
            "typed mapping missing: {cs}"
        );
        assert!(
            cs.contains("default:") && cs.contains("return new WeaveFFIException(code, message);"),
            "generic fallback missing: {cs}"
        );
        // The error-check helper gains a per-domain variant.
        assert!(
            cs.contains("internal static void CheckKv(WeaveFFIError err)")
                && cs.contains("throw KvException.FromCode(err.Code, msg);"),
            "per-domain check missing: {cs}"
        );
    }

    #[test]
    fn interface_class_rendering() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // Opaque-handle wrapper following the struct pattern.
        assert!(
            cs.contains("public class Store : IDisposable"),
            "interface class missing: {cs}"
        );
        assert!(
            cs.contains("internal Store(IntPtr handle)"),
            "internal handle ctor missing: {cs}"
        );
        assert!(
            cs.contains("internal IntPtr Handle => _handle;"),
            "Handle accessor missing: {cs}"
        );
        // The `new` constructor is a real C# constructor assigning _handle.
        assert!(
            cs.contains("public Store(string path)"),
            "real constructor missing: {cs}"
        );
        assert!(
            cs.contains("_handle = result;"),
            "constructor must assign the checked handle: {cs}"
        );
        // Other constructors become static factories wrapping the pointer.
        assert!(
            cs.contains("public static Store OpenReadonly(string path)"),
            "factory missing: {cs}"
        );
        assert!(
            cs.contains("return new Store(result);"),
            "factory must wrap the owned pointer: {cs}"
        );
        // Instance method: non-static, handle as the leading argument.
        assert!(
            cs.contains("public string Get(string storeKey)"),
            "instance method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_Store_get(_handle, storeKeyPtr, ref err);"),
            "method must pass _handle first: {cs}"
        );
        // Static member is a plain static method.
        assert!(
            cs.contains("public static uint DefaultCapacity()"),
            "interface static missing: {cs}"
        );
        // Iterator method surfaces as IEnumerable with the handle prepended.
        assert!(
            cs.contains("public IEnumerable<string> ListKeys()"),
            "iterator method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_Store_list_keys(_handle, ref err);"),
            "iterator launch must pass _handle: {cs}"
        );
        // Async method returns Task and passes the handle to the launcher.
        assert!(
            cs.contains("public async Task Compact()"),
            "async method missing: {cs}"
        );
        assert!(
            cs.contains(
                "NativeMethods.weaveffi_kv_Store_compact_async(_handle, IntPtr.Zero, callback, ctx);"
            ),
            "async launch must pass _handle and the cancel slot: {cs}"
        );
        // Disposal: Dispose + finalizer calling the destroy symbol once.
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_Store_destroy(_handle);")
                && cs.contains("~Store()"),
            "dispose/finalizer missing: {cs}"
        );
        // Externs: destroy plus shape-matched member declarations with the
        // implicit self slot on instance members.
        for sym in [
            "internal static extern void weaveffi_kv_Store_destroy(IntPtr self);",
            "internal static extern IntPtr weaveffi_kv_Store_new(IntPtr path, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_kv_Store_open_readonly(IntPtr path, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_kv_Store_get(IntPtr self, IntPtr store_key, ref WeaveFFIError err);",
            "internal static extern ulong weaveffi_kv_Store_count(IntPtr self, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_kv_Store_list_keys(IntPtr self, ref WeaveFFIError out_err);",
            "internal static extern int weaveffi_kv_Store_ListKeysIterator_next(",
            "internal static extern void weaveffi_kv_Store_ListKeysIterator_destroy(IntPtr iter);",
            "internal static extern void weaveffi_kv_Store_compact_async(IntPtr self, IntPtr cancel_token, AsyncCb_weaveffi_kv_Store_compact callback, IntPtr context);",
            "internal static extern uint weaveffi_kv_Store_default_capacity(ref WeaveFFIError err);",
        ] {
            assert!(cs.contains(sym), "missing P/Invoke `{sym}`: {cs}");
        }
        // No stray sync extern for the async-only member.
        assert!(
            !cs.contains("weaveffi_kv_Store_compact(IntPtr self, ref WeaveFFIError err)"),
            "async member must not declare a sync extern: {cs}"
        );
        // Interface parameters borrow the handle.
        assert!(
            cs.contains("public static ulong LookupStore(Store store)"),
            "interface param wrapper missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_lookup_store(store.Handle, ref err);"),
            "interface param must pass obj.Handle: {cs}"
        );
    }

    /// Extract the body of the method whose signature contains `sig`, up to
    /// the next method boundary (a blank line followed by a doc comment or
    /// declaration at the same depth). Good enough to scope error-check
    /// assertions to one wrapper.
    fn method_slice<'a>(cs: &'a str, sig: &str) -> &'a str {
        let start = cs
            .find(sig)
            .unwrap_or_else(|| panic!("signature `{sig}` not found in: {cs}"));
        let rest = &cs[start..];
        let end = rest.find("\n\n").unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn throws_split_typed_vs_generic() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // throws == true: sync method reports through the typed check.
        let get = method_slice(&cs, "public string Get(string storeKey)");
        assert!(
            get.contains("WeaveFFIError.CheckKv(err);"),
            "throwing method must use the typed check: {get}"
        );
        // throws == false: generic check only (panics/marshalling).
        let count = method_slice(&cs, "public ulong Count()");
        assert!(
            count.contains("WeaveFFIError.Check(err);") && !count.contains("CheckKv"),
            "non-throwing method must use the generic check: {count}"
        );
        // Free functions follow the same split.
        let lookup = method_slice(&cs, "public static ulong LookupStore(Store store)");
        assert!(
            lookup.contains("WeaveFFIError.CheckKv(err);"),
            "throwing free function must use the typed check: {lookup}"
        );
        let ping = method_slice(&cs, "public static bool Ping()");
        assert!(
            ping.contains("WeaveFFIError.Check(err);") && !ping.contains("CheckKv"),
            "non-throwing free function must use the generic check: {ping}"
        );
        // The real constructor throws the typed exception too.
        let ctor = method_slice(&cs, "public Store(string path)");
        assert!(
            ctor.contains("WeaveFFIError.CheckKv(err);"),
            "throwing constructor must use the typed check: {ctor}"
        );
        // Async completion faults the task with the typed exception; the
        // iterator's next-checks are typed as well.
        assert!(
            cs.contains("tcs.SetException(KvException.FromCode(wErr.Code, msg));"),
            "async throws must fault with the typed exception: {cs}"
        );
        let iter = method_slice(&cs, "private static IEnumerator<string> EnumerateListKeys(");
        assert!(
            iter.contains("WeaveFFIError.CheckKv(iterErr);"),
            "iterator next-check must be typed: {iter}"
        );
        // Throwing wrappers document the exception type.
        assert!(
            cs.contains(
                "/// <exception cref=\"KvException\">Thrown when the call reports a KvError code.</exception>"
            ),
            "exception doc missing: {cs}"
        );
    }

    #[test]
    fn wrapper_params_are_camel_case() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "create_contact".into(),
                params: vec![
                    Param {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: Some("Given name.".into()),
                    },
                    Param {
                        name: "contact_type".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::I32),
                doc: None,
                throws: false,
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
            interfaces: vec![],
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
            cs.contains("public static int CreateContact(string firstName, int contactType)"),
            "wrapper params must be camelCase: {cs}"
        );
        assert!(
            cs.contains("Marshal.StringToCoTaskMemUTF8(firstName)")
                && cs.contains("Marshal.FreeCoTaskMem(firstNamePtr);"),
            "marshalling locals must follow the camelCase name: {cs}"
        );
        assert!(
            cs.contains("/// <param name=\"firstName\">Given name.</param>"),
            "param docs must use the camelCase name: {cs}"
        );
        // The P/Invoke extern keeps the IDL spelling.
        assert!(
            cs.contains("internal static extern int weaveffi_contacts_create_contact(IntPtr first_name, int contact_type, ref WeaveFFIError err);"),
            "extern must keep IDL parameter names: {cs}"
        );
    }

    #[test]
    fn default_config_strips_module_prefix() {
        let config = DotnetConfig::default();
        assert!(
            config.strip_module_prefix,
            "strip_module_prefix must default to true"
        );
    }

    /// Parse, validate, and render a CLI fixture IDL end to end. Stands in
    /// for the CLI-driven generation while `weaveffi-cli` is blocked on other
    /// generators mid-overhaul: same parse, validation, model build, and
    /// render path the CLI runs, minus the argument plumbing.
    fn render_fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../weaveffi-cli/tests/fixtures")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
        let mut api = weaveffi_ir::parse::parse_api_str(&text, "yml").expect("fixture must parse");
        weaveffi_core::validate::validate_api(&mut api, None).expect("fixture must validate");
        render_csharp(&api, "WeaveFFI", true, "weaveffi", name, "WeaveFFI.cs")
    }

    #[test]
    fn fixture_contacts_renders_new_surface() {
        let cs = render_fixture("02_contacts.yml");
        // Interface class: real ctor for `new`, PascalCase methods with
        // camelCase parameters, disposal through the destroy symbol.
        assert!(
            cs.contains("public class ContactBook : IDisposable"),
            "ContactBook class missing: {cs}"
        );
        assert!(
            cs.contains("public ContactBook()"),
            "real constructor missing: {cs}"
        );
        assert!(
            cs.contains(
                "public Contact Add(string firstName, string lastName, string? email, \
                 ContactType contactType)"
            ),
            "Add method missing: {cs}"
        );
        assert!(
            cs.contains("public Contact Get(long id)"),
            "Get method missing: {cs}"
        );
        assert!(
            cs.contains("public Contact[] List()"),
            "List method missing: {cs}"
        );
        assert!(
            cs.contains("public bool Remove(long id)"),
            "Remove method missing: {cs}"
        );
        assert!(
            cs.contains("public int Count()"),
            "Count method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_contacts_ContactBook_destroy(_handle);")
                && cs.contains("~ContactBook()"),
            "dispose/finalizer missing: {cs}"
        );
        // Typed errors: domain exception with code constants, typed checks in
        // throwing methods, generic checks elsewhere.
        assert!(
            cs.contains("public class ContactsException : WeaveFFIException"),
            "ContactsException missing: {cs}"
        );
        assert!(
            cs.contains("public const int InvalidName = 1;")
                && cs.contains("public const int NotFound = 2;"),
            "code constants missing: {cs}"
        );
        let get = method_slice(&cs, "public Contact Get(long id)");
        assert!(
            get.contains("WeaveFFIError.CheckContacts(err);"),
            "throwing method must use the typed check: {get}"
        );
        let count = method_slice(&cs, "public int Count()");
        assert!(
            count.contains("WeaveFFIError.Check(err);") && !count.contains("CheckContacts"),
            "non-throwing method must use the generic check: {count}"
        );
    }

    #[test]
    fn fixture_inventory_renders_two_domains() {
        let cs = render_fixture("03_inventory.yml");
        // The products module owns the Catalog interface.
        assert!(
            cs.contains("public class Catalog : IDisposable"),
            "Catalog class missing: {cs}"
        );
        assert!(
            cs.contains("public Product AddProduct(string name, double price, Category category)"),
            "AddProduct method missing: {cs}"
        );
        assert!(
            cs.contains("public Product GetProduct(long id)"),
            "GetProduct method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_products_Catalog_destroy(_handle);"),
            "Catalog destroy missing: {cs}"
        );
        // Two error domains, each with its own exception and check helper.
        assert!(
            cs.contains("public class ProductsException : WeaveFFIException")
                && cs.contains("public class OrdersException : WeaveFFIException"),
            "both domain exceptions must render: {cs}"
        );
        assert!(
            cs.contains("public const int InvalidPrice = 1;")
                && cs.contains("public const int ProductNotFound = 2;")
                && cs.contains("public const int OrderNotFound = 1;")
                && cs.contains("public const int EmptyOrder = 2;"),
            "per-domain code constants missing: {cs}"
        );
        let add = method_slice(
            &cs,
            "public Product AddProduct(string name, double price, Category category)",
        );
        assert!(
            add.contains("WeaveFFIError.CheckProducts(err);"),
            "Catalog methods must use the products check: {add}"
        );
        // The orders module's free functions use their own domain.
        let create = method_slice(&cs, "public static long CreateOrder(OrderItem[] items)");
        assert!(
            create.contains("WeaveFFIError.CheckOrders(err);"),
            "orders functions must use the orders check: {create}"
        );
        let cancel = method_slice(&cs, "public static bool CancelOrder(long id)");
        assert!(
            cancel.contains("WeaveFFIError.Check(err);") && !cancel.contains("CheckOrders"),
            "non-throwing orders function must use the generic check: {cancel}"
        );
        // Per-module static classes with stripped method names.
        assert!(
            cs.contains("public static class Orders"),
            "orders wrapper class missing: {cs}"
        );
    }
}
