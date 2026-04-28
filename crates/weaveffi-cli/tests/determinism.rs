//! Cross-generator determinism guard.
//!
//! Runs every generator on the kitchen-sink fixture twice into two separate
//! tempdirs and asserts every emitted file is byte-identical between runs.
//! Catches non-deterministic iteration (e.g. `HashMap` walks) before it can
//! flake the snapshot suite.

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

const KITCHEN_SINK: &str = "06_kitchen_sink.yml";

fn load_kitchen_sink() -> Api {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(KITCHEN_SINK);
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

fn run_into_tempdir(gen: &dyn Generator, api: &Api) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let out_dir = Utf8Path::from_path(tmp.path()).expect("utf8 tempdir");
    gen.generate(api, out_dir).expect("generator failed");
    let gen_root = out_dir.join(gen.name()).into_std_path_buf();
    (tmp, gen_root)
}

fn assert_byte_identical(gen: &dyn Generator) {
    let api = load_kitchen_sink();

    let (_tmp_a, root_a) = run_into_tempdir(gen, &api);
    let (_tmp_b, root_b) = run_into_tempdir(gen, &api);

    let files_a = collect_files_sorted(&root_a);
    let files_b = collect_files_sorted(&root_b);

    assert!(
        !files_a.is_empty(),
        "generator {} produced no files for kitchen-sink fixture",
        gen.name()
    );

    let rels_a: Vec<_> = files_a
        .iter()
        .map(|p| p.strip_prefix(&root_a).expect("under root").to_path_buf())
        .collect();
    let rels_b: Vec<_> = files_b
        .iter()
        .map(|p| p.strip_prefix(&root_b).expect("under root").to_path_buf())
        .collect();

    assert_eq!(
        rels_a,
        rels_b,
        "generator {} produced different file sets between runs",
        gen.name()
    );

    for (a, b) in files_a.iter().zip(files_b.iter()) {
        let bytes_a = fs::read(a).unwrap_or_else(|e| panic!("read {}: {e}", a.display()));
        let bytes_b = fs::read(b).unwrap_or_else(|e| panic!("read {}: {e}", b.display()));
        assert_eq!(
            bytes_a,
            bytes_b,
            "generator {} produced non-deterministic output for {}",
            gen.name(),
            a.strip_prefix(&root_a).unwrap().display()
        );
    }
}

#[test]
fn generator_output_is_byte_identical_across_runs() {
    let generators: &[&dyn Generator] = &[
        &CGenerator,
        &CppGenerator,
        &SwiftGenerator,
        &AndroidGenerator,
        &NodeGenerator,
        &WasmGenerator,
        &PythonGenerator,
        &DotnetGenerator,
        &DartGenerator,
        &GoGenerator,
        &RubyGenerator,
    ];
    for gen in generators {
        assert_byte_identical(*gen);
    }
}
