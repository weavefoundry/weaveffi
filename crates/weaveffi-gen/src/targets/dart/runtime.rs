//! The fixed Dart runtime every generated library ships, kept as real Dart
//! source under `runtime/` and spliced in with `{{PLACEHOLDER}}`
//! substitution: the loader and load-time contract check, the error and
//! memory helpers, the object base class, the value-buffer codec, async and
//! cancellation glue, the callback-interface machinery, and the iterator
//! anchor. Sections an API doesn't use are left out.

use crate::platform::{Os, Platform};
use weaveffi_model::model::{checksum_symbol, BindingModel, ABI_VERSION};
use weaveffi_model::pkg::Identity;

use crate::targets::dart::types::dart_str_literal;

const LOADER: &str = include_str!("runtime/loader.dart");
const CORE: &str = include_str!("runtime/core.dart");
const OBJECT: &str = include_str!("runtime/object.dart");
const CODEC: &str = include_str!("runtime/codec.dart");
const ASYNC: &str = include_str!("runtime/async.dart");
const CANCEL: &str = include_str!("runtime/cancel.dart");
const CALLBACKS: &str = include_str!("runtime/callbacks.dart");
const ITERATOR: &str = include_str!("runtime/iterator.dart");

/// The library file names the loader tries, per OS family, in order.
pub(crate) struct LoaderCandidates {
    /// macOS candidates.
    pub(crate) macos: Vec<String>,
    /// Windows candidates.
    pub(crate) windows: Vec<String>,
    /// Linux, Android, and every other POSIX candidate.
    pub(crate) linux: Vec<String>,
}

impl LoaderCandidates {
    /// The platform file names of `library`, found on the system search
    /// path.
    pub(crate) fn system(identity: &Identity) -> Self {
        let (macos, linux, windows) = identity.library_files();
        Self {
            macos: vec![macos],
            windows: vec![windows],
            linux: vec![linux],
        }
    }

    /// The bundled `native/<platform>/` copies of `lib` (relative to the
    /// working directory) for every desktop platform of each OS family, then
    /// the system file name.
    pub(crate) fn bundled(lib: &str) -> Self {
        let family = |os: Os| {
            let mut names: Vec<String> = Platform::DESKTOP
                .iter()
                .filter(|p| p.os() == os)
                .map(|p| format!("native/{}/{}", p.id(), p.lib_filename(lib)))
                .collect();
            if let Some(p) = Platform::DESKTOP.iter().find(|p| p.os() == os) {
                names.push(p.lib_filename(lib));
            }
            names
        };
        Self {
            macos: family(Os::MacOs),
            windows: family(Os::Windows),
            linux: family(Os::Linux),
        }
    }
}

/// Whether the packaged library bundles a prebuilt binary for `platform`:
/// the loader resolves `native/<platform-id>/` for the desktop matrix only
/// (Android libraries ship through the app's own packaging, and a `.wasm`
/// module can't be opened with `DynamicLibrary`).
pub(crate) fn bundles_platform(platform: Platform) -> bool {
    platform.is_desktop()
}

/// Substitute the placeholders every runtime file may use.
fn splice(source: &str, identity: &Identity) -> String {
    source
        .replace("{{PREFIX}}", &identity.prefix)
        .replace("{{LIBRARY_ENV}}", &identity.library_env_var())
        .replace("{{LIBRARY}}", &dart_str_literal(&identity.library))
        .replace("{{NAME}}", &dart_str_literal(&identity.name))
}

/// Render the runtime sections `model` needs, with `loader` choosing where
/// the library is looked for.
pub(crate) fn render_runtime(
    out: &mut String,
    identity: &Identity,
    model: &BindingModel,
    loader: &LoaderCandidates,
) {
    let quote = |names: &[String]| {
        names
            .iter()
            .map(|n| format!("'{}'", dart_str_literal(n)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let contracts: String = model
        .roots()
        .map(|m| {
            format!(
                "  ('{}', '{}', 0x{:016x}),\n",
                m.name,
                checksum_symbol(&model.prefix, &m.name),
                m.checksum.expect("top-level modules carry a checksum")
            )
        })
        .collect();
    let loader_src = LOADER
        .replace("{{MACOS_CANDIDATES}}", &quote(&loader.macos))
        .replace("{{WINDOWS_CANDIDATES}}", &quote(&loader.windows))
        .replace("{{LINUX_CANDIDATES}}", &quote(&loader.linux))
        .replace("{{ABI_VERSION}}", &ABI_VERSION.to_string())
        .replace("{{CONTRACTS}}", &contracts);

    let cancellable = model.callables().any(|(_, f)| f.cancellable);
    let sections = [
        (true, loader_src.as_str()),
        (true, CORE),
        (model.has_interfaces(), OBJECT),
        (model.has_buffers(), CODEC),
        (model.has_async(), ASYNC),
        (cancellable, CANCEL),
        (model.has_callback_interfaces(), CALLBACKS),
        (model.has_iterators(), ITERATOR),
    ];
    for (_, source) in sections.iter().filter(|(used, _)| *used) {
        out.push('\n');
        out.push_str(&splice(source, identity));
    }
}
