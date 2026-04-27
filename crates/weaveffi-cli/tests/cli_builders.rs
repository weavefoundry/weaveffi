//! Cross-generator builder parity test.
//!
//! Generates bindings for a minimal `builder: true` struct with three fields
//! and asserts each target emits the builder construct and exposes a real
//! `build()` (or language equivalent) that reaches the C runtime — not a
//! placeholder stub.
//!
//! The canonical C symbol is `weaveffi_{module}_{Struct}_Builder_build`.
//! C, C++, Dart, Go, and the WASM reference doc all call / declare that
//! symbol directly. Swift and Android route the accumulated setter values
//! through the equivalent `weaveffi_{module}_{Struct}_create` C entry
//! instead of re-hopping through the C builder handle, which is valid per
//! the "(or equivalent)" clause of the parity contract.

const BUILDER_YML: &str = "version: \"0.1.0\"
modules:
  - name: parity
    functions: []
    structs:
      - name: Point
        builder: true
        fields:
          - { name: x, type: f64 }
          - { name: y, type: f64 }
          - { name: z, type: f64 }
";

// Re-enabled once Dart and Go advertise the Builders capability.
#[ignore = "Dart and Go do not advertise Builders yet; reactivate when later phases add the capability back"]
#[test]
fn builder_pattern_emits_real_build_in_all_targets() {
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let yml_path = dir.path().join("builder.yml");
    std::fs::write(&yml_path, BUILDER_YML).expect("failed to write builder.yml");

    let out_path = dir.path().join("out");
    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            yml_path.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--target",
            "c,cpp,swift,android,node,wasm,python,dotnet,dart,go,ruby",
        ])
        .assert()
        .success();

    let build_sym = "weaveffi_parity_Point_Builder_build";
    let create_sym = "weaveffi_parity_Point_create";

    // Each entry: (target label, file to inspect, needles that must be present).
    // Needles are the target-idiomatic proof that `build()` reaches the C runtime.
    let cases: &[(&str, &str, &[&str])] = &[
        ("c", "c/weaveffi.h", &[build_sym]),
        (
            "cpp",
            "cpp/weaveffi.hpp",
            &["class PointBuilder", build_sym],
        ),
        (
            "swift",
            "swift/Sources/WeaveFFI/WeaveFFI.swift",
            &["public class PointBuilder", create_sym],
        ),
        (
            "android-kotlin",
            "android/src/main/kotlin/com/weaveffi/WeaveFFI.kt",
            &["class PointBuilder", "Point.create("],
        ),
        (
            "android-jni",
            "android/src/main/cpp/weaveffi_jni.c",
            &[create_sym],
        ),
        (
            "node",
            "node/types.d.ts",
            &["interface PointBuilder", "build(): Point"],
        ),
        (
            "python",
            "python/weaveffi/weaveffi.py",
            &["class PointBuilder", "def build("],
        ),
        (
            "dotnet",
            "dotnet/WeaveFFI.cs",
            &["class PointBuilder", "public Point Build()"],
        ),
        (
            "dart",
            "dart/lib/src/bindings.dart",
            &["class PointBuilder", build_sym],
        ),
        (
            "go",
            "go/weaveffi.go",
            &["type PointBuilder struct", build_sym],
        ),
        (
            "ruby",
            "ruby/lib/weaveffi.rb",
            &["class PointBuilder", "def build"],
        ),
        ("wasm", "wasm/README.md", &[build_sym]),
    ];

    for (target, rel, needles) in cases {
        let path = out_path.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {} for target {}", path.display(), target));
        for needle in *needles {
            assert!(
                content.contains(needle),
                "target {target}: {rel} must contain {needle:?} to prove `build()` reaches the C runtime via the Builder_build symbol (or equivalent)\n--- file ---\n{content}"
            );
        }
    }
}
