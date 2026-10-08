//! Snapshot tests covering every generator against a small, feature-complete
//! IDL corpus.
//!
//! Five fixtures cover the whole IDL surface between them: `kitchen_sink`
//! (every scalar and composite type, an interface with objects in optional,
//! list, iterator, and record positions, a callback interface, an error
//! domain, iterators, async and cancellable functions, deprecation, and a
//! nested submodule), `shapes` (rich enums and the full numeric primitive
//! set), `nested_modules` (a three-deep module tree with cross-module
//! references), `docs_everywhere` (doc comments on every declaration kind),
//! and `edge_cases` (identifiers that are reserved words in some target,
//! deeply nested composites, optional parameters, interfaces and callback
//! interfaces in every legal position, async functions with non-string
//! results, type-level deprecation, and scalar and string iterators). One
//! test per generator renders all five fixtures and checks each file, in
//! sorted order, by one of three rules:
//!
//! - A file that shares its name with a C target output is that target's
//!   copy of the C header. It must be byte-equal to the C target's own
//!   header, which `snapshot_c` snapshots, so it gets no snapshot of its own.
//! - A file matching the target's entry in [`FIXED_FILES`] (fixed runtimes,
//!   package manifests, READMEs) is the same for every fixture apart from
//!   its name, so it's snapshotted once, from [`FIXED_FROM`].
//! - Every other file depends on the fixture and is snapshotted for every
//!   fixture.
//!
//! Snapshots live under `tests/snapshots/`. Regressions in any generator's
//! output fail the affected `cargo insta test` job; behavioral regressions
//! are the conformance harness's job.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use camino::Utf8Path;
use weaveffi_cli::codegen::{ConfiguredBackend, Target};
use weaveffi_cli::targets::c::{CConfig, CGenerator};
use weaveffi_cli::targets::cpp::{CppConfig, CppGenerator};
use weaveffi_cli::targets::dart::{DartConfig, DartGenerator};
use weaveffi_cli::targets::dotnet::{DotnetConfig, DotnetGenerator};
use weaveffi_cli::targets::go::{GoConfig, GoGenerator};
use weaveffi_cli::targets::kotlin::{KotlinConfig, KotlinGenerator};
use weaveffi_cli::targets::node::{NodeConfig, NodeGenerator};
use weaveffi_cli::targets::python::{PythonConfig, PythonGenerator};
use weaveffi_cli::targets::ruby::{RubyConfig, RubyGenerator};
use weaveffi_cli::targets::swift::{SwiftConfig, SwiftGenerator};
use weaveffi_cli::targets::wasm::{WasmConfig, WasmGenerator};
use weaveffi_model::model::Model;
use weaveffi_model::parse::parse_api_str;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

const FIXTURES: [&str; 5] = [
    "kitchen_sink",
    "shapes",
    "nested_modules",
    "docs_everywhere",
    "edge_cases",
];

/// The fixture that fixed files are snapshotted from. It exercises every
/// feature, so it also emits every optional fixed file (such as Kotlin's
/// `Async.kt`).
const FIXED_FROM: &str = "kitchen_sink";

/// Per target, the files whose content doesn't depend on the fixture beyond
/// its name: fixed runtimes, package manifests, and READMEs. (A README's
/// usage example names the top-level modules, and Kotlin's
/// `build.gradle.kts` adds the coroutines dependency only for async APIs;
/// [`FIXED_FROM`] covers both.) A pattern starting with `*` matches a
/// file-name suffix; any other pattern matches the whole file name. Every
/// pattern must match a [`FIXED_FROM`] output, so a renamed or removed file
/// can't leave a stale entry behind.
const FIXED_FILES: &[(&str, &[&str])] = &[
    ("c", &[]),
    ("cpp", &["CMakeLists.txt", "README.md"]),
    ("swift", &["Package.swift", "module.modulemap"]),
    (
        "kotlin",
        &[
            "Async.kt",
            "Buffers.kt",
            "CMakeLists.txt",
            "Runtime.kt",
            "build.gradle.kts",
            "consumer-rules.pro",
            "settings.gradle.kts",
        ],
    ),
    (
        "node",
        &["README.md", "binding.gyp", "package.json", "runtime.js"],
    ),
    (
        "wasm",
        &["README.md", "linear.js", "package.json", "runtime.js"],
    ),
    (
        "python",
        &["README.md", "__init__.py", "py.typed", "pyproject.toml"],
    ),
    ("dotnet", &["*.csproj", "README.md", "Runtime.cs"]),
    ("dart", &["README.md", "pubspec.yaml"]),
    ("go", &["README.md", "codec.go", "go.mod", "runtime.go"]),
    ("ruby", &["*.gemspec", "README.md", "runtime.rb"]),
];

fn fixed_patterns(target: &str) -> &'static [&'static str] {
    FIXED_FILES
        .iter()
        .find(|(name, _)| *name == target)
        .map(|(_, patterns)| *patterns)
        .unwrap_or_else(|| panic!("target {target} has no FIXED_FILES entry"))
}

fn matches_pattern(pattern: &str, file_name: &str) -> bool {
    match pattern.strip_prefix('*') {
        Some(suffix) => file_name.ends_with(suffix),
        None => file_name == pattern,
    }
}

/// The C target's outputs for `api`, keyed by file name. Other targets copy
/// the C header verbatim, under the same file name.
fn c_outputs(model: &Model) -> BTreeMap<String, String> {
    ConfiguredBackend::new(CGenerator, CConfig::default())
        .render(model, Utf8Path::new("out"))
        .into_iter()
        .map(|file| {
            let name = file.path.file_name().expect("file name").to_owned();
            (name, file.contents)
        })
        .collect()
}

fn load_model(stem: &str) -> Model {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{stem}.yml"));
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    let api = parse_api_str(&contents, "yaml")
        .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()));
    validate(&api, &Identity::named(stem), None)
        .unwrap_or_else(|e| panic!("validate fixture {}: {e}", path.display()))
}

fn sanitize(rel: &Path) -> String {
    rel.to_string_lossy().replace(['/', '\\', '.', '-'], "_")
}

/// Replace the concrete crate version in every WeaveFFI prelude
/// (`Generated by WeaveFFI 0.21.0 from ...`) with a stable `[VERSION]`
/// placeholder so a release bump does not invalidate every snapshot.
fn redact_version(s: &str) -> String {
    const MARKER: &str = "Generated by WeaveFFI ";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find(MARKER) {
        out.push_str(&rest[..idx + MARKER.len()]);
        rest = &rest[idx + MARKER.len()..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        out.push_str("[VERSION]");
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

/// Every generated file embeds the standard WeaveFFI prelude markers in its
/// first five lines. JSON files (which cannot carry comments) embed these
/// markers via the `"//"` key convention.
fn assert_prelude_present(contents: &str, file: &Path) {
    let head: String = contents.lines().take(5).collect::<Vec<_>>().join("\n");
    for marker in ["Generated by WeaveFFI", "DO NOT EDIT"] {
        assert!(
            head.contains(marker),
            "{} is missing '{marker}' in the first 5 lines:\n{head}",
            file.display(),
        );
    }
}

fn run_snapshots(target: &dyn Target) {
    let out_dir = Utf8Path::new("out");
    let fixed = fixed_patterns(target.name());
    let mut unmatched: BTreeSet<&str> = fixed.iter().copied().collect();
    for stem in FIXTURES {
        let model = load_model(stem);
        let c_headers = if target.name() == "c" {
            BTreeMap::new()
        } else {
            c_outputs(&model)
        };
        let mut files = target.render(&model, out_dir);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        assert!(
            !files.is_empty(),
            "generator {} produced no files for fixture {stem}",
            target.name(),
        );
        let root = out_dir.join(target.name());

        insta::with_settings!({
            snapshot_path => "snapshots",
            prepend_module_to_snapshot => false,
            omit_expression => true,
        }, {
            for file in files {
                assert_prelude_present(&file.contents, file.path.as_std_path());
                let file_name = file.path.file_name().expect("file name");
                if let Some(header) = c_headers.get(file_name) {
                    assert!(
                        file.contents == *header,
                        "{} in fixture {stem} isn't a byte-equal copy of the C target's {file_name}",
                        file.path,
                    );
                    continue;
                }
                if let Some(pattern) = fixed.iter().find(|p| matches_pattern(p, file_name)) {
                    if stem != FIXED_FROM {
                        continue;
                    }
                    unmatched.remove(pattern);
                }
                let rel = file
                    .path
                    .strip_prefix(&root)
                    .expect("file under generator root");
                let name = format!("{}_{stem}__{}", target.name(), sanitize(rel.as_std_path()));
                insta::assert_snapshot!(name, redact_version(&file.contents));
            }
        });
    }
    assert!(
        unmatched.is_empty(),
        "FIXED_FILES patterns for {} match no {FIXED_FROM} output: {unmatched:?}",
        target.name(),
    );
}

macro_rules! snapshot_tests {
    ($( $fn_name:ident => $gen:expr, $cfg:ty; )*) => {
        $(
            #[test]
            fn $fn_name() {
                run_snapshots(&ConfiguredBackend::new($gen, <$cfg>::default()));
            }
        )*
    };
}

snapshot_tests! {
    snapshot_c => CGenerator, CConfig;
    snapshot_cpp => CppGenerator, CppConfig;
    snapshot_swift => SwiftGenerator, SwiftConfig;
    snapshot_kotlin => KotlinGenerator, KotlinConfig;
    snapshot_node => NodeGenerator, NodeConfig;
    snapshot_wasm => WasmGenerator, WasmConfig;
    snapshot_python => PythonGenerator, PythonConfig;
    snapshot_dotnet => DotnetGenerator, DotnetConfig;
    snapshot_dart => DartGenerator, DartConfig;
    snapshot_go => GoGenerator, GoConfig;
    snapshot_ruby => RubyGenerator, RubyConfig;
}
