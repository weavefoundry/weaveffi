//! .NET binding generator.
//!
//! Emits a standalone C# project over the C ABI, revision 3: a `.csproj`,
//! the generated API (`{Namespace}.cs`), the fixed runtime (`Runtime.cs`,
//! from `runtime/Runtime.cs`), and a README. Implements [`LanguageBackend`].
//!
//! * Native calls use source-generated `[LibraryImport]` declarations over
//!   blittable slots, so the bindings need no runtime marshalling and are
//!   trim- and AOT-friendly. Strings cross as UTF-8 `(ptr, len)` pairs.
//! * The first native call runs `NativeMethods`' static constructor, which
//!   installs a resolver honoring `{PREFIX}_LIBRARY`, then checks the ABI
//!   revision and every top-level module's contract checksum.
//! * Records, rich enums, optionals, lists, and maps are value types encoded
//!   in the value-buffer format by per-type `WriteTo`/`ReadFrom` pairs.
//! * Interfaces are sealed `IDisposable` wrappers over a `SafeHandle`
//!   subclass whose `ReleaseHandle` calls `_destroy`. Every import takes the
//!   handle itself, so the interop stub keeps the object alive for the call.
//! * Callback interfaces are C# `interface`s; a passed implementation is
//!   pinned in a `GCHandle` and paired with one static vtable of
//!   `[UnmanagedCallersOnly]` trampolines.
//! * Async functions return `Task`/`Task<T>` completed by an
//!   `[UnmanagedCallersOnly]` completion; cancellable ones take a
//!   `CancellationToken` linked to the native cancel token. `iter<T>`
//!   returns are single-use lazily streamed `IEnumerable<T>`s.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod pinvoke;
mod runtime;
mod types;

use crate::backend::{LanguageBackend, OutputFile};
use crate::capabilities::TargetCapabilities;
use crate::codegen::CodeWriter;
use crate::package::{PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::BindingModel;
use weaveffi_model::resolved::ResolvedApi;

use crate::targets::dotnet::callbacks::render_callback_interface;
use crate::targets::dotnet::calls::render_module_class;
use crate::targets::dotnet::entities::{
    render_enum, render_interface, render_record, render_rich_enum,
};
use crate::targets::dotnet::package::{render_csproj, render_readme, Project, NATIVE_ASSETS};
use crate::targets::dotnet::pinvoke::render_native_methods;
use crate::targets::dotnet::runtime::{
    base_exception_name, render_domain_exception, render_runtime, RuntimeNames,
};
use crate::targets::dotnet::types::Cx;

/// Per-target configuration for [`DotnetGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DotnetConfig {
    /// C# namespace, assembly name, and NuGet id. Defaults to the package
    /// name in PascalCase (`my-kv` becomes `MyKv`).
    pub namespace: Option<String>,
    /// When `true` (the default), strip the module name from wrapper method
    /// names; the per-module static class already namespaces them.
    pub strip_module_prefix: bool,
    /// Basename of the IDL the CLI was invoked with. Set by the CLI.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for DotnetConfig {
    fn default() -> Self {
        Self {
            namespace: None,
            strip_module_prefix: true,
            input_basename: None,
        }
    }
}

impl DotnetConfig {
    /// The C# namespace: the configured one, else `PascalCase(name)`.
    pub fn namespace(&self, api: &ResolvedApi) -> String {
        self.namespace
            .clone()
            .unwrap_or_else(|| api.identity().pascal_name())
    }

    /// The IDL basename for generated-file preludes.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("api.yml")
    }
}

/// .NET backend: a C# project binding the C ABI through `[LibraryImport]`.
pub struct DotnetGenerator;

/// The text files of a generated project, at `dir`, loading `library`.
fn project_files(
    api: &ResolvedApi,
    model: &BindingModel,
    dir: &Utf8Path,
    config: &DotnetConfig,
    library: &str,
    ctx: Option<&PackageContext>,
) -> Vec<(camino::Utf8PathBuf, String)> {
    let namespace = config.namespace(api);
    let identity = api.identity();
    let input = config.input_basename();
    let project = Project {
        identity,
        namespace: &namespace,
        library,
        input_basename: input,
    };
    let base = base_exception_name(model, &namespace);
    let names = RuntimeNames {
        namespace: &namespace,
        exception: &base,
        prefix: &model.prefix,
        library,
        library_env: &identity.library_env_var(),
    };
    let cs = format!("{namespace}.cs");
    let csproj = format!("{namespace}.csproj");
    let assets = if ctx.is_some() { NATIVE_ASSETS } else { "" };
    vec![
        (
            dir.join(&cs),
            render_csharp(
                model,
                &namespace,
                &base,
                config.strip_module_prefix,
                input,
                &cs,
            ),
        ),
        (
            dir.join("Runtime.cs"),
            render_runtime(&names, input, "Runtime.cs"),
        ),
        (dir.join(&csproj), render_csproj(&project, &csproj, assets)),
        (dir.join("README.md"), render_readme(&project, ctx)),
    ]
}

impl LanguageBackend for DotnetGenerator {
    type Config = DotnetConfig;

    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn capabilities(&self, _config: &Self::Config) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn files(
        &self,
        api: &ResolvedApi,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let library = &api.identity().library;
        project_files(api, model, &out_dir.join("dotnet"), config, library, None)
            .into_iter()
            .map(|(path, text)| OutputFile::new(path, text))
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
        let dir = out_dir.join("dotnet");
        let library = &ctx.binaries.lib_name;
        let mut files: Vec<PackagedFile> =
            project_files(api, model, &dir, config, library, Some(ctx))
                .into_iter()
                .map(|(path, text)| PackagedFile::text(path, text))
                .collect();
        // Bundle each prebuilt library under the `runtimes/<rid>/native/`
        // layout NuGet resolves at restore time. Platforms without a RID
        // (Android, wasm32) have no slot in the package.
        for nb in &ctx.binaries.binaries {
            let Some(rid) = nb.platform.nuget_rid() else {
                continue;
            };
            let dest = dir
                .join("runtimes")
                .join(rid)
                .join("native")
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

/// Render the generated API file: typed exceptions, every module's entities,
/// the generated half of `NativeMethods`, and the per-module static classes.
pub(crate) fn render_csharp(
    model: &BindingModel,
    namespace: &str,
    base: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    filename: &str,
) -> String {
    let cx = Cx {
        ns: namespace,
        base,
    };
    let mut w = CodeWriter::four_space();
    w.line("#nullable enable");
    w.blank();
    for u in [
        "System",
        "System.Collections.Generic",
        "System.Runtime.CompilerServices",
        "System.Runtime.InteropServices",
        "System.Threading",
        "System.Threading.Tasks",
    ] {
        w.line(format!("using {u};"));
    }
    w.blank();
    w.line(format!("namespace {namespace};"));
    w.blank();

    for m in model.modules.iter() {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_domain_exception(&mut w, eb, cx);
        }
    }
    for m in &model.modules {
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum(&mut w, cx, e);
            } else {
                render_enum(&mut w, e);
            }
        }
        for s in &m.structs {
            render_record(&mut w, cx, s);
        }
        for cb in &m.callback_interfaces {
            render_callback_interface(&mut w, m, cb, cx);
        }
        for i in &m.interfaces {
            render_interface(&mut w, i, m.error.as_ref(), cx);
        }
    }
    for m in &model.modules {
        render_module_class(&mut w, m, strip_module_prefix, cx);
    }
    render_native_methods(&mut w, model);

    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str(&tidy(&w.finish()));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// Drop blank lines directly before a closing brace.
fn tidy(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = String::with_capacity(text.len());
    for (i, line) in lines.iter().enumerate() {
        let next_closes = lines
            .get(i + 1)
            .is_some_and(|n| n.trim_start().starts_with('}'));
        if line.is_empty() && next_closes {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests;
