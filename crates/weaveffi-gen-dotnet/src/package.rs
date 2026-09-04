//! Package-surface renderers: the `.csproj` and `.nuspec` manifests, the
//! README variants, and the NuGet package-identity resolution.
//!
//! Every interpolated user string (package id, version, description, authors,
//! license, and URLs) routes through the shared
//! [`xml_escape`](weaveffi_core::manifest::xml_escape), so markup-sensitive
//! characters can't corrupt the XML.

use weaveffi_core::manifest::xml_escape;
use weaveffi_core::package::PackageContext;
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::DotnetConfig;

/// Resolve the NuGet/package identity for the .NET target, applying the
/// namespace-as-name fallback when nothing else identifies the package.
pub(crate) fn resolve_dotnet_package(api: &ResolvedApi, config: &DotnetConfig) -> ResolvedPackage {
    let namespace = config.namespace();
    let mut p = pkg::resolve(
        api,
        config.namespace.as_deref(),
        config.input_basename.as_deref(),
    );
    // The C# namespace doubles as the file basename; when nothing identifies
    // the package, keep the PascalCase brand as the NuGet id so
    // `WeaveFFI.csproj` and `<PackageId>` stay consistent.
    if api.package().and_then(|p| p.name.as_deref()).is_none()
        && config.namespace.is_none()
        && config.input_basename.is_none()
    {
        p.name = namespace.to_string();
    }
    p
}

/// Render the `.csproj` emitted by `generate`, with no extra item groups.
pub(crate) fn render_csproj(
    package: &ResolvedPackage,
    input_basename: &str,
    filename: &str,
) -> String {
    render_csproj_with_assets(package, input_basename, filename, "")
}

/// Render the `.csproj`, optionally injecting extra `<ItemGroup>` blocks
/// (`native_assets`) after the main `<PropertyGroup>`. The `weaveffi package`
/// path passes the `runtimes/**` native-asset item group here; `generate`
/// passes an empty string.
pub(crate) fn render_csproj_with_assets(
    package: &ResolvedPackage,
    input_basename: &str,
    filename: &str,
    native_assets: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, filename);
    let id = xml_escape(&package.name);
    let version = xml_escape(&package.version);
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

/// Render the `.nuspec` package manifest.
pub(crate) fn render_nuspec(
    package: &ResolvedPackage,
    input_basename: &str,
    filename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, filename);
    let id = xml_escape(&package.name);
    let version = xml_escape(&package.version);
    let authors = if package.authors.is_empty() {
        "WeaveFFI Contributors".to_string()
    } else {
        xml_escape(&package.authors.join(", "))
    };
    let description = xml_escape(&package.description_or_default());
    let license = xml_escape(&package.license.clone().unwrap_or_else(|| "MIT".into()));
    let project_url = xml_escape(
        &package
            .homepage
            .clone()
            .or_else(|| package.repository.clone())
            .unwrap_or_else(|| "https://github.com/weavefoundry/weaveffi".into()),
    );
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

/// Render the README emitted by `generate`.
pub(crate) fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
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

/// Render the README for a packaged .NET artifact, listing the bundled
/// runtime identifiers so consumers know which platforms ship prebuilt.
/// Platforms without a NuGet RID aren't bundled and aren't listed.
pub(crate) fn render_packaged_readme(
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
        .filter_map(|p| p.nuget_rid())
        .map(|rid| format!("- `{rid}`"))
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
