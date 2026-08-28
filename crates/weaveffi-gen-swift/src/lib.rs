//! Swift binding generator for WeaveFFI.
//!
//! Emits a SwiftPM package containing a thin Swift wrapper over the C ABI,
//! including module map, `Package.swift`, and Swift `async/await` shims for
//! functions marked `async: true`. Implements [`LanguageBackend`]; the shared
//! driver bridges it into the generator pipeline.
//!
//! Records, rich enums, optionals, lists, and maps cross the C ABI as value
//! buffers (one `const uint8_t*` + `size_t` pair in the WeaveFFI wire format).
//! The generated wrapper ships a small private writer/reader pair
//! (`WvWriter`/`WvReader`) plus one pack and one unpack routine per record and
//! rich enum, so records surface as plain Swift structs and rich enums as
//! native Swift enums with associated values.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

mod calls;
mod codec;
mod docs;
mod entities;
mod package;
mod runtime;
#[cfg(test)]
mod tests;
mod types;

use std::collections::{HashMap, HashSet};

use camino::Utf8Path;
use heck::ToUpperCamelCase;
use serde::{Deserialize, Serialize};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::walk_modules;
use weaveffi_core::model::{BindingModel, ModuleBinding};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_ir::ir::Module;

use crate::entities::{render_swift_module_body, render_swift_module_types};
use crate::package::{
    render_modulemap, render_package_swift, render_packaged_package_swift, render_packaged_readme,
    resolve_module_name,
};
use crate::runtime::{
    render_buffer_runtime, render_continuation_ref, render_error_infra, render_listener_registry,
};
use crate::types::{enum_raw_type, SwiftCtx};

/// Per-target configuration for [`SwiftGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwiftConfig {
    /// SwiftPM module name (default `"WeaveFFI"`).
    pub module_name: Option<String>,
    /// When `true` (the default), strip the IR module path from emitted
    /// function names, so `enum Kv` exposes `openStore` rather than
    /// `kvOpenStore`. Set to `false` to restore the module-prefixed spelling.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the Swift wrappers call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    /// Populated by the CLI; not user-configurable via `[swift]`.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for SwiftConfig {
    fn default() -> Self {
        Self {
            module_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl SwiftConfig {
    /// Returns the configured SwiftPM module name, falling back to
    /// `"WeaveFFI"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("WeaveFFI")
    }

    /// Returns the configured C ABI symbol prefix, falling back to
    /// `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Each module contributes ~2KB of Swift wrapper text on average (struct
/// declarations, codec routines, async wrappers); pre-allocating from this
/// estimate reduces `String` re-allocations as the wrapper grows past 64 KB.
const SWIFT_BASE_BYTES: usize = 4096;
const SWIFT_BYTES_PER_MODULE: usize = 2048;
const SWIFT_BYTES_PER_FUNCTION: usize = 512;
const SWIFT_BYTES_PER_STRUCT: usize = 512;

/// Estimate the rendered wrapper size from the module tree, for the initial
/// `String` allocation.
fn estimate_swift_capacity(modules: &[Module]) -> usize {
    fn count(modules: &[Module]) -> (usize, usize, usize) {
        let mut m = 0;
        let mut f = 0;
        let mut s = 0;
        for module in modules {
            m += 1;
            f += module.functions.len();
            s += module.structs.len();
            let (sm, sf, ss) = count(&module.modules);
            m += sm;
            f += sf;
            s += ss;
        }
        (m, f, s)
    }
    let (mods, funcs, structs) = count(modules);
    SWIFT_BASE_BYTES
        + mods * SWIFT_BYTES_PER_MODULE
        + funcs * SWIFT_BYTES_PER_FUNCTION
        + structs * SWIFT_BYTES_PER_STRUCT
}

/// Swift backend: emits a SwiftPM package with a thin Swift wrapper (module
/// map, `Package.swift`, and `async`/`await` shims) over the C ABI.
pub struct SwiftGenerator;

impl LanguageBackend for SwiftGenerator {
    type Config = SwiftConfig;

    fn name(&self) -> &'static str {
        "swift"
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
        let module_name_owned = resolve_module_name(api, config);
        let module_name = module_name_owned.as_str();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("swift");
        let c_module = format!("C{module_name}");
        // The C shim is a SwiftPM `systemLibrary` target, so its module map
        // must live under `Sources/<target>/` for `swift build` to find it.
        let module_dir = dir.join("Sources").join(&c_module);

        let src_dir = dir.join("Sources").join(module_name);
        let swift_filename = format!("{module_name}.swift");
        vec![
            OutputFile::new(
                dir.join("Package.swift"),
                render_package_swift(module_name, &c_module, input_basename),
            ),
            OutputFile::new(
                module_dir.join("module.modulemap"),
                render_modulemap(&c_module, prefix, input_basename),
            ),
            OutputFile::new(
                src_dir.join(&swift_filename),
                render_swift_wrapper(
                    api,
                    model,
                    prefix,
                    config.strip_module_prefix,
                    input_basename,
                    &swift_filename,
                ),
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
        let module_name_owned = resolve_module_name(api, config);
        let module_name = module_name_owned.as_str();
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("swift");
        let c_module = format!("C{module_name}");
        let xcframework = format!("{c_module}.xcframework");

        let src_dir = dir.join("Sources").join(module_name);
        let swift_filename = format!("{module_name}.swift");
        let wrapper = render_swift_wrapper(
            api,
            model,
            prefix,
            config.strip_module_prefix,
            input_basename,
            &swift_filename,
        );

        let mut files = vec![
            PackagedFile::text(
                dir.join("Package.swift"),
                render_packaged_package_swift(module_name, &c_module, &xcframework, input_basename),
            ),
            PackagedFile::text(src_dir.join(&swift_filename), wrapper),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(module_name, &c_module, prefix, ctx, input_basename),
            ),
        ];
        // Bundle the prebuilt libraries as xcframework-ready slices.
        for nb in &ctx.binaries.binaries {
            let dest = dir
                .join("lib")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(SwiftGenerator);

/// Render the complete generated Swift wrapper file: prelude and imports,
/// the runtime helpers the model needs, every module's file-scope types, and
/// one namespace `enum` per top-level module.
fn render_swift_wrapper(
    api: &ResolvedApi,
    model: &BindingModel,
    c_prefix: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    filename: &str,
) -> String {
    let mut out = String::with_capacity(estimate_swift_capacity(&api.modules));
    out.push_str(&render_prelude(CommentStyle::DoubleSlash, input_basename));
    // The C shim target is `C<module_name>` and the wrapper file is always
    // `<module_name>.swift`, so the system-library module to import is the
    // file stem with a `C` prefix. Deriving it here keeps the `import` in sync
    // with the module name picked from `[swift] module_name` / the IDL package.
    let module_name = filename.strip_suffix(".swift").unwrap_or(filename);
    out.push_str(&format!("import C{module_name}\nimport Foundation\n\n"));

    // Index the flat, pre-order model by its underscore-joined symbol path so
    // the recursive IR walk below can pull each module's precomputed C symbols
    // while still emitting the nested Swift `enum` structure the IR tree drives.
    let by_path: HashMap<&str, &ModuleBinding> =
        model.modules.iter().map(|m| (m.path.as_str(), m)).collect();

    let all_mods = walk_modules(&api.modules).collect::<Vec<_>>();

    // Every module becomes a namespace `enum`; a wrapper type whose name
    // matches one of these is shadowed inside that namespace and must be
    // module-qualified at its use sites.
    let module_names: HashSet<String> = all_mods
        .iter()
        .map(|m| m.name.to_upper_camel_case())
        .collect();
    // Raw-value types of every C-style enum, so buffer codecs can emit the
    // right `i32` bit conversion for a referenced enum without re-resolving it.
    let enum_raws: HashMap<String, &'static str> = model
        .modules
        .iter()
        .flat_map(|m| m.enums.iter())
        .filter(|e| !e.is_rich())
        .map(|e| (e.name.clone(), enum_raw_type(e)))
        .collect();
    let ctx = SwiftCtx {
        c_prefix,
        swift_module: module_name,
        module_names: &module_names,
        enum_raws: &enum_raws,
    };

    render_error_infra(&mut out);
    render_buffer_runtime(&mut out);

    // Interface members can be async too, so consult every callable.
    let has_async = model
        .modules
        .iter()
        .any(|m| m.callables().any(|f| f.is_async));
    if has_async {
        render_continuation_ref(&mut out);
    }

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        render_listener_registry(&mut out);
    }

    for m in &api.modules {
        render_swift_module_types(&mut out, c_prefix, &by_path, m, &m.name, ctx);
        let type_name = m.name.to_upper_camel_case();
        out.push_str(&format!("public enum {} {{\n", type_name));
        render_swift_module_body(
            &mut out,
            c_prefix,
            &by_path,
            m,
            &m.name,
            1,
            strip_module_prefix,
            ctx,
        );
        out.push_str("}\n\n");
    }
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}
