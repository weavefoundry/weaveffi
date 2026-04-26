//! Cross-generator iterator parity test.
//!
//! Uses the existing `samples/events/events.yml` — which declares
//! `get_messages() -> iter<string>` — to generate bindings for every target
//! and asserts each output contains the appropriate language-idiomatic
//! streaming construct for the iterator return:
//!
//! - C: `*Iterator_next` and `*Iterator_destroy` functions in the header.
//! - C++: a `std::vector<std::string>` iterable container.
//! - Swift: `[String]` (Swift's Sequence-conforming collection), driven
//!   internally by the C iterator `_next` / `_destroy` pair.
//! - Kotlin: `Iterator<String>`.
//! - Node: an object implementing `Symbol.asyncIterator`.
//! - WASM: `get_messages` method on the module interface.
//! - Python: an iterator class with a `__next__` method.
//! - .NET: `IEnumerable<string>`.
//! - Dart: `Iterable<String>`.
//! - Go: a `<-chan string` read-only channel.
//! - Ruby: an `Enumerator`.

use std::path::Path;

#[test]
fn iterator_return_emits_streaming_in_all_targets() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let events_yml = repo_root.join("samples/events/events.yml");

    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = dir.path().join("out");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            events_yml.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--target",
            "c,cpp,swift,android,node,wasm,python,dotnet,dart,go,ruby",
        ])
        .assert()
        .success();

    let cases: &[(&str, &str, &[&str])] = &[
        (
            "c",
            "c/weaveffi.h",
            &[
                "weaveffi_events_GetMessagesIterator_next",
                "weaveffi_events_GetMessagesIterator_destroy",
            ],
        ),
        ("cpp", "cpp/weaveffi.hpp", &["std::vector<std::string>"]),
        (
            "swift",
            "swift/Sources/WeaveFFI/WeaveFFI.swift",
            &["-> [String]"],
        ),
        (
            "android",
            "android/src/main/kotlin/com/weaveffi/WeaveFFI.kt",
            &["Iterator<String>"],
        ),
        ("node", "node/index.js", &["Symbol.asyncIterator"]),
        ("wasm", "wasm/weaveffi_wasm.js", &["get_messages"]),
        ("python", "python/weaveffi/weaveffi.py", &["__next__"]),
        ("dotnet", "dotnet/WeaveFFI.cs", &["IEnumerable<string>"]),
        ("dart", "dart/lib/weaveffi.dart", &["Iterable<String>"]),
        ("go", "go/weaveffi.go", &["<-chan string"]),
        ("ruby", "ruby/lib/weaveffi.rb", &["Enumerator"]),
    ];

    for (target, rel, needles) in cases {
        let path = out_path.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {} for target {}", path.display(), target));
        for needle in *needles {
            assert!(
                content.contains(needle),
                "target {target}: {rel} should contain {needle:?} for iter<string> get_messages\n--- file ---\n{content}"
            );
        }
    }
}
