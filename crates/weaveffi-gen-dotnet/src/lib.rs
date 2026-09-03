//! .NET (P/Invoke) binding generator for WeaveFFI.
//!
//! Emits a C# project (`.csproj` + `.nuspec`) with P/Invoke declarations
//! and idiomatic wrappers over the C ABI. Async functions surface as
//! `Task<T>`-returning methods. Implements [`LanguageBackend`]; the shared
//! driver bridges it into the generator pipeline.
//!
//! Records, rich enums, optionals, lists, and maps are value types that
//! cross the C ABI serialized in the WeaveFFI value-buffer format (one
//! `const uint8_t*` + `size_t` pair). The generated file carries a small
//! internal writer/reader implementing the wire format, plus one
//! `WriteTo`/`ReadFrom` pair per record and rich enum.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod pinvoke;
mod runtime;
mod types;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::{BindingModel, CallShape, ErrorBinding};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::render_wrapper_class;
use crate::entities::{
    collect_typed_handles, render_enum, render_interface_class, render_rich_enum_class,
    render_struct_class, render_typed_handle_struct,
};
use crate::package::{
    render_csproj, render_csproj_with_assets, render_nuspec, render_packaged_readme, render_readme,
    resolve_dotnet_package,
};
use crate::pinvoke::render_native_methods;
use crate::runtime::{
    render_buffer_classes, render_domain_exception, render_error_struct, render_exception_class,
    render_helpers_class, render_once_enumerable_class,
};

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
        api: &ResolvedApi,
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
        api: &ResolvedApi,
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

/// Render the complete generated C# source file: the prelude, usings, the
/// shared runtime types, every module's entities, the `NativeMethods` extern
/// class, and the per-module static wrapper classes.
pub(crate) fn render_csharp(
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
    render_buffer_classes(&mut out);
    for referent in collect_typed_handles(model) {
        render_typed_handle_struct(&mut out, &referent);
    }
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
            // Rich (algebraic) enums are sum types, emitted as an abstract
            // class with one nested variant class each; only plain C-style
            // enums map to `enum`.
            if e.is_rich() {
                render_rich_enum_class(&mut out, e);
            } else {
                render_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, s);
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
