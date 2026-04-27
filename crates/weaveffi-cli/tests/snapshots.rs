//! Snapshot tests covering every generator against a representative IDL corpus.
//!
//! Each test parses a fixture, runs a single generator into a fresh tempdir,
//! walks the resulting files in sorted order, and snapshots their contents
//! one-file-per-snapshot under `tests/snapshots/`. Regressions in any
//! generator's output cause the affected `cargo insta test` job to fail.

use std::fs;
use std::path::{Path, PathBuf};

use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_core::validate::validate_api;
use weaveffi_gen_android::AndroidGenerator;
use weaveffi_gen_c::CGenerator;
use weaveffi_gen_cpp::CppGenerator;
use weaveffi_gen_dart::DartGenerator;
use weaveffi_gen_dotnet::DotnetGenerator;
use weaveffi_gen_go::GoGenerator;
use weaveffi_gen_node::NodeGenerator;
use weaveffi_gen_python::PythonGenerator;
use weaveffi_gen_ruby::RubyGenerator;
use weaveffi_gen_swift::SwiftGenerator;
use weaveffi_gen_wasm::WasmGenerator;
use weaveffi_ir::ir::Api;
use weaveffi_ir::parse::parse_api_str;

fn fixture_path(file: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(file)
}

fn load_api(file: &str) -> Api {
    let path = fixture_path(file);
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    let mut api = parse_api_str(&contents, "yaml")
        .unwrap_or_else(|e| panic!("parse fixture {}: {e}", path.display()));
    validate_api(&mut api, None)
        .unwrap_or_else(|e| panic!("validate fixture {}: {e}", path.display()));
    api
}

fn collect_files_sorted(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

fn sanitize(rel: &Path) -> String {
    rel.to_string_lossy().replace(['/', '\\', '.', '-'], "_")
}

fn run_snapshot(gen: &dyn Generator, fixture_stem: &str, fixture_file: &str) {
    let api = load_api(fixture_file);
    let tmp = tempfile::tempdir().expect("create tempdir");
    let out_dir = Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
    gen.generate(&api, out_dir).expect("generator failed");

    let gen_root = out_dir.join(gen.name());
    let files = collect_files_sorted(gen_root.as_std_path());
    assert!(
        !files.is_empty(),
        "generator {} produced no files for fixture {}",
        gen.name(),
        fixture_file,
    );

    insta::with_settings!({
        snapshot_path => "snapshots",
        prepend_module_to_snapshot => false,
        omit_expression => true,
    }, {
        for file in files {
            let rel = file
                .strip_prefix(gen_root.as_std_path())
                .expect("file under gen root");
            let name = format!("{}_{}__{}", gen.name(), fixture_stem, sanitize(rel));
            let contents = fs::read_to_string(&file)
                .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
            insta::assert_snapshot!(name, contents);
        }
    });
}

macro_rules! snapshot_test {
    ($fn_name:ident, $gen_expr:expr, $fixture_stem:literal, $fixture_file:literal) => {
        #[test]
        fn $fn_name() {
            run_snapshot(&$gen_expr, $fixture_stem, $fixture_file);
        }
    };
}

snapshot_test!(
    snapshot_c_calculator,
    CGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_c_contacts,
    CGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_c_inventory,
    CGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_c_async_demo,
    CGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(snapshot_c_events, CGenerator, "events", "05_events.yml");
snapshot_test!(
    snapshot_c_kitchen_sink,
    CGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_cpp_calculator,
    CppGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_cpp_contacts,
    CppGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_cpp_inventory,
    CppGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_cpp_async_demo,
    CppGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(snapshot_cpp_events, CppGenerator, "events", "05_events.yml");
snapshot_test!(
    snapshot_cpp_kitchen_sink,
    CppGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_swift_calculator,
    SwiftGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_swift_contacts,
    SwiftGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_swift_inventory,
    SwiftGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_swift_async_demo,
    SwiftGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_swift_events,
    SwiftGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_swift_kitchen_sink,
    SwiftGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_android_calculator,
    AndroidGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_android_contacts,
    AndroidGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_android_inventory,
    AndroidGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_android_async_demo,
    AndroidGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_android_events,
    AndroidGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_android_kitchen_sink,
    AndroidGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_node_calculator,
    NodeGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_node_contacts,
    NodeGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_node_inventory,
    NodeGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_node_async_demo,
    NodeGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_node_events,
    NodeGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_node_kitchen_sink,
    NodeGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_wasm_calculator,
    WasmGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_wasm_contacts,
    WasmGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_wasm_inventory,
    WasmGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_wasm_async_demo,
    WasmGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_wasm_events,
    WasmGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_wasm_kitchen_sink,
    WasmGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_python_calculator,
    PythonGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_python_contacts,
    PythonGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_python_inventory,
    PythonGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_python_async_demo,
    PythonGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_python_events,
    PythonGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_python_kitchen_sink,
    PythonGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_dotnet_calculator,
    DotnetGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_dotnet_contacts,
    DotnetGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_dotnet_inventory,
    DotnetGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_dotnet_async_demo,
    DotnetGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_dotnet_events,
    DotnetGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_dotnet_kitchen_sink,
    DotnetGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_dart_calculator,
    DartGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_dart_contacts,
    DartGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_dart_inventory,
    DartGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_dart_async_demo,
    DartGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_dart_events,
    DartGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_dart_kitchen_sink,
    DartGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_go_calculator,
    GoGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_go_contacts,
    GoGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_go_inventory,
    GoGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_go_async_demo,
    GoGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(snapshot_go_events, GoGenerator, "events", "05_events.yml");
snapshot_test!(
    snapshot_go_kitchen_sink,
    GoGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);

snapshot_test!(
    snapshot_ruby_calculator,
    RubyGenerator,
    "calculator",
    "01_calculator.yml"
);
snapshot_test!(
    snapshot_ruby_contacts,
    RubyGenerator,
    "contacts",
    "02_contacts.yml"
);
snapshot_test!(
    snapshot_ruby_inventory,
    RubyGenerator,
    "inventory",
    "03_inventory.yml"
);
snapshot_test!(
    snapshot_ruby_async_demo,
    RubyGenerator,
    "async_demo",
    "04_async_demo.yml"
);
snapshot_test!(
    snapshot_ruby_events,
    RubyGenerator,
    "events",
    "05_events.yml"
);
snapshot_test!(
    snapshot_ruby_kitchen_sink,
    RubyGenerator,
    "kitchen_sink",
    "06_kitchen_sink.yml"
);
