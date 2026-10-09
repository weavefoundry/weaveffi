//! The fixed Dart runtime every generated library ships, kept as real Dart
//! source under `runtime/` and spliced in with `{{PLACEHOLDER}}`
//! substitution: the loader and load-time checks, the error and memory
//! helpers, the object base class, the value-buffer codec, async and
//! cancellation glue, the callback-interface machinery, and the iterator
//! anchor. Sections an API doesn't use are left out. The contract tables the
//! loader checks are rendered from the model.

use crate::codegen::CodeWriter;
use crate::platform::{Arch, Os, Platform};
use weaveffi_model::contract::entries;
use weaveffi_model::model::{contract_symbol, Model, ABI_VERSION};

use crate::targets::dart::types::dart_str_literal;

const LOADER: &str = include_str!("runtime/loader.dart");
const BUNDLED: &str = include_str!("runtime/bundled.dart");
const CORE: &str = include_str!("runtime/core.dart");
const OBJECT: &str = include_str!("runtime/object.dart");
const CODEC: &str = include_str!("runtime/codec.dart");
const ASYNC: &str = include_str!("runtime/async.dart");
const CANCEL: &str = include_str!("runtime/cancel.dart");
const CALLBACKS: &str = include_str!("runtime/callbacks.dart");
const ITERATOR: &str = include_str!("runtime/iterator.dart");

/// Where the loader looks for the library before the system search path.
pub(crate) enum Bundle<'a> {
    /// Nowhere: the generated (unpackaged) bindings.
    None,
    /// `native/<platform>/` under the package root (then the working
    /// directory), for the packaged library `lib` of the given platforms.
    Packaged {
        /// The library's base name (`kvstore` for `libkvstore.dylib`).
        lib: &'a str,
        /// The platforms the package bundles.
        platforms: Vec<Platform>,
    },
}

/// Whether the packaged library bundles a prebuilt binary for `platform`:
/// the loader resolves `native/<platform-id>/` for the desktop matrix only
/// (Android libraries ship through the app's own packaging, and a `.wasm`
/// module can't be opened with `DynamicLibrary`).
pub(crate) fn bundles_platform(platform: Platform) -> bool {
    platform.is_desktop()
}

/// The `dart:ffi` `Abi` constant of a desktop platform.
fn dart_abi(platform: Platform) -> Option<&'static str> {
    Some(match (platform.os(), platform.arch()) {
        (Os::MacOs, Arch::Arm64) => "Abi.macosArm64",
        (Os::MacOs, Arch::X64) => "Abi.macosX64",
        (Os::Linux, Arch::Arm64) => "Abi.linuxArm64",
        (Os::Linux, Arch::X64) => "Abi.linuxX64",
        (Os::Windows, Arch::Arm64) => "Abi.windowsArm64",
        (Os::Windows, Arch::X64) => "Abi.windowsX64",
        _ => return None,
    })
}

/// Replace every `{{KEY}}` in `template` with its value.
fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        out = out.replace(&format!("{{{{{key}}}}}"), value);
    }
    debug_assert!(!out.contains("{{"), "unfilled placeholder in:\n{out}");
    out
}

/// Render the runtime sections `model` needs into `w`; `package` is the
/// Dart package name and `bundle` says where the loader looks first.
pub(crate) fn render_runtime(w: &mut CodeWriter, model: &Model, package: &str, bundle: &Bundle) {
    let identity = &model.identity;
    let (macos, linux, windows) = identity.library_files();
    let prefix = identity.prefix.as_str();
    let env = identity.library_env_var();
    let library = dart_str_literal(&identity.library);
    let name = dart_str_literal(&identity.name);
    let abi = ABI_VERSION.to_string();
    let common = [
        ("PREFIX", prefix),
        ("LIBRARY_ENV", env.as_str()),
        ("LIBRARY", library.as_str()),
        ("NAME", name.as_str()),
    ];

    let bundled_lookup = match bundle {
        Bundle::None => "",
        Bundle::Packaged { .. } => {
            "  final bundled = _openBundled();\n  if (bundled != null) return bundled;\n"
        }
    };
    let (macos, linux, windows) = (
        dart_str_literal(&macos),
        dart_str_literal(&linux),
        dart_str_literal(&windows),
    );
    let mut vars = common.to_vec();
    vars.extend([
        ("MACOS_FILE", macos.as_str()),
        ("LINUX_FILE", linux.as_str()),
        ("WINDOWS_FILE", windows.as_str()),
        ("BUNDLED_LOOKUP", bundled_lookup),
        ("ABI_VERSION", abi.as_str()),
    ]);
    w.blank();
    w.raw(fill(LOADER, &vars));
    render_contracts(w, model);
    if let Bundle::Packaged { lib, platforms } = bundle {
        let cases: String = platforms
            .iter()
            .filter_map(|p| {
                let abi = dart_abi(*p)?;
                Some(format!(
                    "      {abi} => 'native/{}/{}',\n",
                    p.id(),
                    dart_str_literal(&p.lib_filename(lib))
                ))
            })
            .collect();
        let package = dart_str_literal(package);
        let mut vars = common.to_vec();
        vars.extend([
            ("BUNDLED_CASES", cases.as_str()),
            ("PACKAGE", package.as_str()),
        ]);
        w.raw(fill(BUNDLED, &vars));
    }

    let cancellable = model.callables().any(|(_, f)| f.cancellable);
    let sections = [
        (true, CORE),
        (model.has_interfaces(), OBJECT),
        (model.has_buffers(), CODEC),
        (model.has_async(), ASYNC),
        (cancellable, CANCEL),
        (model.has_callback_interfaces(), CALLBACKS),
        (model.has_iterators(), ITERATOR),
    ];
    for (_, source) in sections.iter().filter(|(used, _)| *used) {
        w.blank();
        w.raw(fill(source, &common));
    }
}

/// The contract tables the loader checks: for every top-level module, its
/// contract symbol and each declaration these bindings were generated with.
fn render_contracts(w: &mut CodeWriter, model: &Model) {
    w.blank();
    w.line("/// Each top-level module's contract-table symbol and the declarations");
    w.line("/// these bindings were generated with: (id, signature hash, path).");
    w.line("const List<(String, List<(int, int, String)>)> _contracts = [");
    w.scope(|w| {
        for root in model.roots() {
            w.line(format!(
                "('{}', [",
                contract_symbol(model.prefix(), &root.name)
            ));
            w.scope(|w| {
                for e in entries(model, root) {
                    w.line(format!(
                        "(0x{:016x}, 0x{:016x}, '{}'),",
                        e.id,
                        e.hash,
                        dart_str_literal(&e.path)
                    ));
                }
            });
            w.line("]),");
        }
    });
    w.line("];");
}
