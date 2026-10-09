//! Package-surface renderers: the `.csproj` (which carries the NuGet
//! metadata `dotnet pack` needs) and the README.
//!
//! Every interpolated user string routes through the shared
//! [`xml_escape`], so markup-sensitive
//! characters can't corrupt the XML.

use crate::manifest::xml_escape;
use crate::package::PackageContext;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::pkg::Identity;

/// The names a generated .NET project is built from.
pub(crate) struct Project<'a> {
    /// The library identity.
    pub identity: &'a Identity,
    /// The C# namespace, which is also the assembly name and NuGet id.
    pub namespace: &'a str,
    /// The native library base name the bindings load.
    pub library: &'a str,
}

/// Render the `.csproj`. `extra` is spliced in after the main property group
/// (the native-asset item group of a packaged build).
pub(crate) fn render_csproj(p: &Project<'_>, filename: &str, extra: &str) -> String {
    let id = p.identity;
    let ns = xml_escape(p.namespace);
    let mut meta = format!(
        "    <Version>{}</Version>\n    <Description>{}</Description>\n",
        xml_escape(&id.version),
        xml_escape(&id.description_or_default())
    );
    if !id.authors.is_empty() {
        meta.push_str(&format!(
            "    <Authors>{}</Authors>\n",
            xml_escape(&id.authors.join(", "))
        ));
    }
    if let Some(license) = &id.license {
        meta.push_str(&format!(
            "    <PackageLicenseExpression>{}</PackageLicenseExpression>\n",
            xml_escape(license)
        ));
    }
    if let Some(url) = id.homepage.as_ref().or(id.repository.as_ref()) {
        meta.push_str(&format!(
            "    <PackageProjectUrl>{}</PackageProjectUrl>\n",
            xml_escape(url)
        ));
    }
    if let Some(url) = &id.repository {
        meta.push_str(&format!(
            "    <RepositoryUrl>{}</RepositoryUrl>\n",
            xml_escape(url)
        ));
    }
    format!(
        r#"{prelude}<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <AssemblyName>{ns}</AssemblyName>
    <RootNamespace>{ns}</RootNamespace>
    <PackageId>{ns}</PackageId>
{meta}    <Nullable>enable</Nullable>
    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
    <IsAotCompatible>true</IsAotCompatible>
    <GenerateDocumentationFile>true</GenerateDocumentationFile>
    <!-- The bindings themselves reference the API's deprecated items, and
         carry the IDL's docs, which needn't cover every member. -->
    <NoWarn>$(NoWarn);CS0618;CS1573;CS1591</NoWarn>
  </PropertyGroup>
{extra}
</Project>

{trailer}"#,
        prelude = render_prelude(CommentStyle::Xml),
        trailer = render_trailer(CommentStyle::Xml, filename),
    )
}

/// The `<ItemGroup>` shipping every bundled native library under
/// `runtimes/<rid>/native/`, where NuGet resolves it at restore time.
pub(crate) const NATIVE_ASSETS: &str = "  <PropertyGroup>
    <PackageReadmeFile>README.md</PackageReadmeFile>
  </PropertyGroup>
  <ItemGroup>
    <None Include=\"README.md\" Pack=\"true\" PackagePath=\"\\\" />
    <Content Include=\"runtimes/**\" Pack=\"true\" PackagePath=\"runtimes/\">
      <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>
    </Content>
  </ItemGroup>
";

/// Render the README. A packaged build lists its bundled runtime
/// identifiers.
pub(crate) fn render_readme(
    p: &Project<'_>,
    library_class: &str,
    ctx: Option<&PackageContext>,
) -> String {
    let ns = p.namespace;
    let env = p.identity.library_env_var();
    let (mac, linux, win) = p.identity.library_files();
    let mut out = render_prelude(CommentStyle::Xml);
    out.push_str(&format!(
        "# {ns} (.NET)\n\n\
         {desc}\n\n\
         The `{ns}` namespace binds the `{lib}` native library through\n\
         source-generated P/Invoke (`[LibraryImport]`), so it's trim- and\n\
         AOT-friendly.\n\n",
        desc = p.identity.description_or_default(),
        lib = p.library,
    ));
    match ctx {
        Some(ctx) => {
            let rids: Vec<String> = ctx
                .binaries
                .platforms()
                .filter_map(|pl| pl.nuget_rid())
                .map(|rid| format!("- `{rid}`"))
                .collect();
            out.push_str(&format!(
                "## Install\n\n```bash\ndotnet add package {ns}\n```\n\n\
                 The package bundles the native library for these runtimes:\n\n{}\n\n",
                rids.join("\n")
            ));
        }
        None => {
            out.push_str(&format!(
                "## Build\n\n```bash\ndotnet build\n```\n\n\
                 Reference this project from yours (`dotnet add reference {ns}.csproj`)\n\
                 or pack it (`dotnet pack -c Release`).\n\n"
            ));
        }
    }
    out.push_str(&format!(
        "## Loading the native library\n\n\
         The bindings load `{lib}` with the platform's normal rules\n\
         (`{mac}`, `{linux}`, or `{win}` next to the app or on the\n\
         library search path). Set `{env}` to a full path to load a\n\
         specific file instead.\n\n\
         The first call checks that the loaded library matches these\n\
         bindings (its ABI revision and every declaration's contract) and\n\
         throws `NativeLoadException` when it doesn't, as does every later\n\
         call. Call `{library_class}.Check()` at startup to run the check\n\
         up front.\n\n",
        lib = p.library,
    ));
    out.push_str(&render_trailer(CommentStyle::Xml, "README.md"));
    out
}
