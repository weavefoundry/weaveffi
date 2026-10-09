//! The fixed Dart runtime every generated library ships, kept as real Dart
//! source under `runtime/` and spliced in with `{{PLACEHOLDER}}`
//! substitution: the loader and load-time checks, the error, call-frame,
//! and memory helpers, the object base class, the value-buffer codec, the
//! typed-array helpers, async and cancellation glue, the callback-interface
//! machinery, and the iterator anchor. Each section is one part file under
//! `lib/src/runtime/`, and sections an API doesn't use are left out. The
//! contract tables the loader checks are rendered from the model into
//! `contracts.dart`.

use crate::codegen::contract::{hex, tables};
use crate::codegen::CodeWriter;
use crate::platform::{Arch, Os, Platform};
use weaveffi_model::model::{Model, ABI_VERSION};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ItemPass, ResultPass, RetPass};

use crate::targets::dart::types::dart_str_literal;

const LOADER: &str = include_str!("runtime/loader.dart");
const BUNDLED: &str = include_str!("runtime/bundled.dart");
const CORE: &str = include_str!("runtime/core.dart");
const OBJECT: &str = include_str!("runtime/object.dart");
const CODEC: &str = include_str!("runtime/codec.dart");
const ARRAYS: &str = include_str!("runtime/arrays.dart");
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

/// Whether any value in `model` crosses as a typed array (the Slice
/// transport), in any position.
fn uses_arrays(model: &Model) -> bool {
    let callables = model.callables().any(|(_, f)| {
        f.params
            .iter()
            .any(|p| matches!(p.pass, ArgPass::Slice { .. }))
            || matches!(f.ret_pass, RetPass::Slice { .. })
            || f.async_binding()
                .is_some_and(|a| matches!(a.result, ResultPass::Slice { .. }))
            || f.iterator()
                .is_some_and(|it| matches!(it.item, ItemPass::Slice { .. }))
    });
    let methods = model
        .callback_interfaces()
        .flat_map(|(_, cb)| &cb.methods)
        .any(|m| {
            m.params
                .iter()
                .any(|p| matches!(p.pass, ArgPass::Slice { .. }))
                || matches!(m.ret_pass, CallbackRetPass::Slice { .. })
        });
    callables || methods
}

/// The runtime sections `model` needs, as `(file name, source)` pairs in
/// library order; `package` is the Dart package name and `bundle` says
/// where the loader looks first.
pub(crate) fn runtime_parts(
    model: &Model,
    package: &str,
    bundle: &Bundle,
) -> Vec<(String, String)> {
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
    let mut parts = vec![
        ("loader.dart".to_string(), fill(LOADER, &vars)),
        ("contracts.dart".to_string(), render_contracts(model)),
    ];
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
        parts.push(("bundled.dart".into(), fill(BUNDLED, &vars)));
    }

    let cancellable = model.callables().any(|(_, f)| f.cancellable());
    let sections = [
        (true, "core.dart", CORE),
        (model.has_interfaces(), "object.dart", OBJECT),
        (model.has_buffers(), "codec.dart", CODEC),
        (uses_arrays(model), "arrays.dart", ARRAYS),
        (model.has_async(), "async.dart", ASYNC),
        (cancellable, "cancel.dart", CANCEL),
        (model.has_callback_interfaces(), "callbacks.dart", CALLBACKS),
        (model.has_iterators(), "iterator.dart", ITERATOR),
    ];
    for (_, file, source) in sections.iter().filter(|(used, ..)| *used) {
        parts.push(((*file).to_string(), fill(source, &common)));
    }
    parts
}

/// The contract tables the loader checks: for every top-level module, its
/// contract symbol and each declaration these bindings were generated with,
/// with its canonical signature as a comment.
fn render_contracts(model: &Model) -> String {
    let mut w = CodeWriter::two_space();
    w.line("// ── Contract tables ──");
    w.blank();
    w.line("/// Each top-level module's contract-table symbol and the declarations");
    w.line("/// these bindings were generated with: (id, signature hash, path).");
    w.line("const List<(String, List<(int, int, String)>)> _contracts = [");
    w.scope(|w| {
        for table in tables(model) {
            w.line(format!("('{}', [", table.symbol));
            w.scope(|w| {
                for row in &table.rows {
                    w.line(format!("// {}", row.signature));
                    w.line(format!(
                        "({}, {}, '{}'),",
                        hex(row.id),
                        hex(row.hash),
                        dart_str_literal(&row.path)
                    ));
                }
            });
            w.line("]),");
        }
    });
    w.line("];");
    w.finish()
}
