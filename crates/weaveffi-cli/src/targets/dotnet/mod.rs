//! .NET binding generator.
//!
//! Emits a standalone C# project over the C ABI, revision 5: a `.csproj`,
//! the generated API (`{Namespace}.cs`), the fixed runtime (`Runtime.cs`,
//! from `runtime/Runtime.cs`), and a README.
//!
//! * Native calls use source-generated `[LibraryImport]` declarations over
//!   blittable slots with runtime marshalling disabled, so the bindings are
//!   trim- and AOT-friendly. Strings cross as pooled UTF-8 `(ptr, len)`
//!   pairs, typed arrays and bytes as pinned spans, and optional scalars as
//!   a presence flag plus the value.
//! * Every import resolves through a `DllImportResolver` that loads the
//!   library (honoring `{PREFIX}_LIBRARY`) and checks its ABI revision and
//!   every top-level module's contract table once; a failure is a
//!   `NativeLoadException` every call throws, and `{Namespace}Library.Check()`
//!   runs the check up front.
//! * Records are positional `sealed record`s whose list and map fields are
//!   `IReadOnlyList<T>`/`IReadOnlyDictionary<K, V>`, compared by content;
//!   rich enums are closed record hierarchies. Values are encoded by
//!   per-type `WriteTo`/`ReadFrom` pairs and the runtime's generic composite
//!   codec, with no per-shape code.
//! * Interfaces are sealed `IDisposable` wrappers over a `SafeHandle`
//!   subclass whose `ReleaseHandle` calls `_destroy`. Every import takes the
//!   handle itself, so the interop stub keeps the object alive for the call.
//! * A call's [`ErrorStrategy`](weaveffi_model::plan::ErrorStrategy) picks
//!   its exceptions: a domain's hierarchy (one nested class per code; an
//!   unknown code is the domain class itself), the root `NativeException`
//!   for `throws: any` and runtime codes, or `NativeBugException` for a call
//!   that can't fail. Cancellation raises `OperationCanceledException`.
//! * Callback interfaces are C# `interface`s; a passed implementation is
//!   pinned in a `GCHandle` and paired with one static vtable of
//!   `[UnmanagedCallersOnly]` trampolines.
//! * Async functions return `Task`/`Task<T>` completed by an
//!   `[UnmanagedCallersOnly]` completion and take a `CancellationToken`
//!   (linked to the native cancel token when the function is cancellable).
//!   `iter<T>` returns re-enumerable `IEnumerable<T>`s, one native iterator
//!   per enumeration.

mod callbacks;
mod calls;
mod codec;
mod docs;
mod entities;
mod errors;
mod pack;
mod package;
mod pinvoke;
mod runtime;
mod types;

use crate::codegen::CodeWriter;
use crate::codegen::OutputFile;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::targets::Target;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::{Utf8Path, Utf8PathBuf};
use miette::Result;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

use crate::targets::dotnet::callbacks::render_callback_interface;
use crate::targets::dotnet::calls::render_module_class;
use crate::targets::dotnet::docs::Docs;
use crate::targets::dotnet::entities::{
    render_enum, render_interface, render_record, render_rich_enum,
};
use crate::targets::dotnet::errors::render_domains;
use crate::targets::dotnet::package::{render_csproj, render_readme, Project, NATIVE_ASSETS};
use crate::targets::dotnet::pinvoke::render_native_methods;
use crate::targets::dotnet::runtime::{render_runtime, RuntimeNames};
use crate::targets::dotnet::types::Cx;

/// Per-target configuration for [`DotnetGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DotnetConfig {
    /// C# namespace, assembly name, and NuGet id. Defaults to the package
    /// name in PascalCase (`my-kv` becomes `MyKv`).
    pub name: Option<String>,
}

impl DotnetConfig {
    /// The C# namespace: the configured one, else `PascalCase(name)`.
    pub fn namespace(&self, model: &Model) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| model.identity.pascal_name())
    }
}

/// .NET backend: a C# project binding the C ABI through `[LibraryImport]`.
pub struct DotnetGenerator {
    config: DotnetConfig,
}

impl From<DotnetConfig> for DotnetGenerator {
    fn from(config: DotnetConfig) -> Self {
        Self { config }
    }
}

/// The text files of a generated project, at `dir`, loading `library`.
fn project_files(
    model: &Model,
    dir: &Utf8Path,
    config: &DotnetConfig,
    library: &str,
    ctx: Option<&PackageContext>,
) -> Vec<(Utf8PathBuf, String)> {
    let namespace = config.namespace(model);
    let project = Project {
        identity: &model.identity,
        namespace: &namespace,
        library,
    };
    let names = RuntimeNames::new(model, &namespace);
    let cs = format!("{namespace}.cs");
    let csproj = format!("{namespace}.csproj");
    let assets = if ctx.is_some() { NATIVE_ASSETS } else { "" };
    vec![
        (dir.join(&cs), render_csharp(model, &namespace, &names, &cs)),
        (
            dir.join("Runtime.cs"),
            render_runtime(model, &namespace, &names, library, "Runtime.cs"),
        ),
        (dir.join(&csproj), render_csproj(&project, &csproj, assets)),
        (
            dir.join("README.md"),
            render_readme(&project, &names.library_class, ctx),
        ),
    ]
}

impl Target for DotnetGenerator {
    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn fixed_files(&self) -> &'static [&'static str] {
        &["*.csproj", "README.md", "Runtime.cs"]
    }

    /// Run `dotnet pack` over the written project, producing the NuGet
    /// package under `dotnet/`.
    fn finish_package(
        &self,
        dist: &Utf8Path,
        artifacts: &[Artifact],
        ctx: &PackageContext<'_>,
    ) -> Result<Vec<Utf8PathBuf>> {
        pack::dotnet_pack(dist, artifacts, ctx)
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let library = &model.identity.library;
        project_files(model, &Utf8PathBuf::new(), &self.config, library, None)
            .into_iter()
            .map(|(path, text)| OutputFile::new(path, text))
            .collect()
    }

    /// The NuGet-ready project at `dotnet/{Namespace}/`, with every desktop
    /// library under the `runtimes/<rid>/native/` layout NuGet resolves at
    /// restore time; `weaveffi package` then runs `dotnet pack` on it.
    /// Platforms without a RID (Android, iOS, `wasm32`) have no slot.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let config = &self.config;
        let library = &ctx.binaries.lib_name;
        let mut natives = Vec::new();
        for nb in &ctx.binaries.binaries {
            let Some(rid) = nb.platform.nuget_rid() else {
                continue;
            };
            natives.push(PackagedFile::copy(
                format!(
                    "runtimes/{rid}/native/{}",
                    ctx.binaries.bundled_filename(nb.platform)
                ),
                nb.library.clone(),
            ));
        }
        if natives.is_empty() {
            return Ok(Vec::new());
        }
        let mut files: Vec<PackagedFile> =
            project_files(model, Utf8Path::new(""), config, library, Some(ctx))
                .into_iter()
                .map(|(path, text)| PackagedFile::text(path, text))
                .collect();
        files.extend(natives);
        Ok(vec![Artifact::directory(
            format!("dotnet/{}", config.namespace(model)),
            files,
        )])
    }
}

/// Render the generated API file: typed exceptions, every module's entities,
/// the per-module static classes, and the generated half of
/// `NativeMethods`.
pub(crate) fn render_csharp(
    model: &Model,
    namespace: &str,
    names: &RuntimeNames,
    filename: &str,
) -> String {
    let cx = Cx {
        ns: namespace,
        base: &names.exception,
        bug: &names.bug_exception,
    };
    let docs = Docs::new(model);
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

    render_domains(&mut w, model, &docs, cx);
    for m in &model.modules {
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum(&mut w, cx, &docs, e);
            } else {
                render_enum(&mut w, &docs, e);
            }
        }
        for s in &m.structs {
            render_record(&mut w, cx, &docs, s);
        }
        for cb in &m.callback_interfaces {
            render_callback_interface(&mut w, model, &docs, cb, cx);
        }
        for i in &m.interfaces {
            render_interface(&mut w, model, &docs, i, cx);
        }
    }
    for m in &model.modules {
        render_module_class(&mut w, model, &docs, m, cx);
    }
    render_native_methods(&mut w, model, namespace);

    let mut out = render_prelude(CommentStyle::DoubleSlash);
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
