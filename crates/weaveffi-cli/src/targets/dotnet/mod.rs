//! .NET binding generator.
//!
//! Emits a standalone C# project over the C ABI, revision 4: a `.csproj`,
//! the generated API (`{Namespace}.cs`), the fixed runtime (`Runtime.cs`,
//! from `runtime/Runtime.cs`), and a README. Implements [`LanguageBackend`].
//!
//! * Native calls use source-generated `[LibraryImport]` declarations over
//!   blittable slots, so the bindings need no runtime marshalling and are
//!   trim- and AOT-friendly. Strings cross as UTF-8 `(ptr, len)` pairs.
//! * The first native call runs `NativeMethods`' static constructor, which
//!   installs a resolver honoring `{PREFIX}_LIBRARY`, then checks the ABI
//!   revision and every top-level module's contract table against the
//!   entries the bindings were generated with.
//! * Records and rich enums are classes encoded in the value-buffer format
//!   by per-type `WriteTo`/`ReadFrom` pairs; optionals, lists, and maps by
//!   one `FfiCodecs` pair per distinct composite type.
//! * Interfaces are sealed `IDisposable` wrappers over a `SafeHandle`
//!   subclass whose `ReleaseHandle` calls `_destroy`. Every import takes the
//!   handle itself, so the interop stub keeps the object alive for the call.
//! * A throwing call raises its domain's exception hierarchy (one nested
//!   class per code, its fields as properties) or the root
//!   `NativeException` for a runtime code; a failed non-throwing call is a
//!   producer bug and raises `NativeBugException`; cancellation raises
//!   `OperationCanceledException`.
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
use crate::codegen::CodeWriter;
use crate::package::{Artifact, PackageContext, PackagedFile};
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

use crate::targets::dotnet::callbacks::render_callback_interface;
use crate::targets::dotnet::calls::render_module_class;
use crate::targets::dotnet::codec::render_codecs;
use crate::targets::dotnet::entities::{
    render_enum, render_interface, render_record, render_rich_enum,
};
use crate::targets::dotnet::package::{render_csproj, render_readme, Project, NATIVE_ASSETS};
use crate::targets::dotnet::pinvoke::render_native_methods;
use crate::targets::dotnet::runtime::{
    exception_names, render_domain_exception, render_runtime, RuntimeNames,
};
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
pub struct DotnetGenerator;

/// The text files of a generated project, at `dir`, loading `library`.
fn project_files(
    model: &Model,
    dir: &Utf8Path,
    config: &DotnetConfig,
    library: &str,
    ctx: Option<&PackageContext>,
) -> Vec<(camino::Utf8PathBuf, String)> {
    let namespace = config.namespace(model);
    let identity = &model.identity;
    let project = Project {
        identity,
        namespace: &namespace,
        library,
    };
    let (base, bug) = exception_names(model, &namespace);
    let names = RuntimeNames {
        namespace: &namespace,
        exception: &base,
        bug_exception: &bug,
        prefix: model.prefix(),
        library,
        library_env: &identity.library_env_var(),
    };
    let cs = format!("{namespace}.cs");
    let csproj = format!("{namespace}.csproj");
    let assets = if ctx.is_some() { NATIVE_ASSETS } else { "" };
    vec![
        (
            dir.join(&cs),
            render_csharp(model, &namespace, (&base, &bug), &cs),
        ),
        (dir.join("Runtime.cs"), render_runtime(&names, "Runtime.cs")),
        (dir.join(&csproj), render_csproj(&project, &csproj, assets)),
        (dir.join("README.md"), render_readme(&project, ctx)),
    ]
}

impl LanguageBackend for DotnetGenerator {
    type Config = DotnetConfig;

    fn name(&self) -> &'static str {
        "dotnet"
    }

    fn files(&self, model: &Model, out_dir: &Utf8Path, config: &Self::Config) -> Vec<OutputFile> {
        let library = &model.identity.library;
        project_files(model, &out_dir.join("dotnet"), config, library, None)
            .into_iter()
            .map(|(path, text)| OutputFile::new(path, text))
            .collect()
    }

    /// The NuGet-ready project at `dotnet/{Namespace}/`, with every desktop
    /// library under the `runtimes/<rid>/native/` layout NuGet resolves at
    /// restore time; `weaveffi package` then runs `dotnet pack` on it.
    /// Platforms without a RID (Android, iOS, `wasm32`) have no slot.
    fn package(
        &self,
        model: &Model,
        ctx: &PackageContext,
        config: &Self::Config,
    ) -> Option<Vec<Artifact>> {
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
            return Some(Vec::new());
        }
        let mut files: Vec<PackagedFile> =
            project_files(model, Utf8Path::new(""), config, library, Some(ctx))
                .into_iter()
                .map(|(path, text)| PackagedFile::text(path, text))
                .collect();
        files.extend(natives);
        Some(vec![Artifact::directory(
            format!("dotnet/{}", config.namespace(model)),
            files,
        )])
    }
}

/// Render the generated API file: typed exceptions, every module's entities,
/// the generated half of `NativeMethods`, and the per-module static classes.
pub(crate) fn render_csharp(
    model: &Model,
    namespace: &str,
    (base, bug): (&str, &str),
    filename: &str,
) -> String {
    let cx = Cx {
        ns: namespace,
        base,
        bug,
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

    for m in &model.modules {
        if let Some(eb) = &m.errors {
            render_domain_exception(&mut w, m, eb, cx);
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
            render_callback_interface(&mut w, &m.path, cb, model.error_domain(m), cx);
        }
        for i in &m.interfaces {
            render_interface(&mut w, i, model.error_domain(m), cx);
        }
    }
    for m in &model.modules {
        render_module_class(&mut w, m, model.error_domain(m), cx);
    }
    render_codecs(&mut w, cx, model);
    render_native_methods(&mut w, model);

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
