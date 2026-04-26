//! Cross-generator parity test for listeners.
//!
//! Uses the existing `samples/events/events.yml` to generate bindings and
//! asserts each of the 11 target outputs contains the idiomatic listener
//! registration helper for `message_listener`.
//!
//! `events.yml` also declares `get_messages() -> iter<string>`. The Go and
//! Ruby generators do not yet advertise the `Iterators` capability (that
//! support ships in the next phase), so those two targets are generated from
//! a listener-only variant of the sample that drops the iterator function
//! but keeps the callback + listener definitions byte-identical.

use std::path::Path;

/// Listener-only subset of `samples/events/events.yml` for targets that do
/// not yet advertise iterator support. The `events` module, `OnMessage`
/// callback, and `message_listener` listener match the sample exactly.
const EVENTS_LISTENER_ONLY_YML: &str = r#"version: "0.1.0"
modules:
  - name: events
    callbacks:
      - name: OnMessage
        params:
          - { name: message, type: string }
    listeners:
      - name: message_listener
        event_callback: OnMessage
    functions:
      - name: send_message
        params:
          - { name: text, type: string }
"#;

#[test]
fn listener_register_unregister_emitted_for_all_targets() {
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
            "c,cpp,swift,android,node,wasm,python,dotnet,dart",
        ])
        .assert()
        .success();

    let listener_only_yml = dir.path().join("events_listener_only.yml");
    std::fs::write(&listener_only_yml, EVENTS_LISTENER_ONLY_YML)
        .expect("failed to write listener-only yml");

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            listener_only_yml.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
            "--target",
            "go,ruby",
        ])
        .assert()
        .success();

    let cases: &[(&str, &str, &str)] = &[
        (
            "c",
            "c/weaveffi.h",
            "weaveffi_events_register_message_listener",
        ),
        (
            "cpp",
            "cpp/weaveffi.hpp",
            "weaveffi_events_register_message_listener",
        ),
        (
            "swift",
            "swift/Sources/WeaveFFI/WeaveFFI.swift",
            "weaveffi_events_register_message_listener",
        ),
        (
            "android",
            "android/src/main/kotlin/com/weaveffi/WeaveFFI.kt",
            "fun register(callback: OnMessage)",
        ),
        ("node", "node/types.d.ts", "register(callback: OnMessage)"),
        (
            "wasm",
            "wasm/weaveffi_wasm.js",
            "weaveffi_events_register_message_listener",
        ),
        (
            "python",
            "python/weaveffi/weaveffi.py",
            "weaveffi_events_register_message_listener",
        ),
        (
            "dotnet",
            "dotnet/WeaveFFI.cs",
            "weaveffi_events_register_message_listener",
        ),
        (
            "dart",
            "dart/lib/weaveffi.dart",
            "weaveffi_events_register_message_listener",
        ),
        (
            "go",
            "go/weaveffi.go",
            "weaveffi_events_register_message_listener",
        ),
        (
            "ruby",
            "ruby/lib/weaveffi.rb",
            "weaveffi_events_register_message_listener",
        ),
    ];

    for (target, rel, needle) in cases {
        let path = out_path.join(rel);
        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {} for target {}", path.display(), target));
        assert!(
            content.contains(needle),
            "target {target}: {} should contain {needle:?} for listener message_listener\n--- file ---\n{content}",
            rel
        );
    }
}
