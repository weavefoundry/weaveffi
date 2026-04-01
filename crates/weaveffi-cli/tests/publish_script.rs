use std::path::Path;

fn parse_crates_from_script(script: &str) -> Vec<String> {
    let start = script.find("CRATES=(").expect("CRATES=( not found");
    let after = &script[start..];
    let end = after.find(')').expect("closing ) not found");
    let block = &after[..end];

    block
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.contains("CRATES") {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

#[test]
fn publish_script_contains_all_publishable_crates() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/publish-crates.sh"))
        .expect("failed to read publish-crates.sh");

    let crates = parse_crates_from_script(&script);

    let expected = [
        "weaveffi-ir",
        "weaveffi-abi",
        "weaveffi-core",
        "weaveffi-gen-c",
        "weaveffi-gen-swift",
        "weaveffi-gen-android",
        "weaveffi-gen-node",
        "weaveffi-gen-wasm",
        "weaveffi-gen-python",
        "weaveffi-gen-dotnet",
        "weaveffi-gen-cpp",
        "weaveffi-gen-dart",
        "weaveffi-gen-go",
        "weaveffi-gen-ruby",
        "weaveffi-cli",
    ];

    assert_eq!(
        crates, expected,
        "CRATES array does not match expected order"
    );
}

#[test]
fn publish_script_cli_is_last() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/publish-crates.sh"))
        .expect("failed to read publish-crates.sh");

    let crates = parse_crates_from_script(&script);
    assert_eq!(
        crates.last().map(|s| s.as_str()),
        Some("weaveffi-cli"),
        "weaveffi-cli must be published last"
    );
}

#[test]
fn publish_script_core_before_generators() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let script = std::fs::read_to_string(repo_root.join("scripts/publish-crates.sh"))
        .expect("failed to read publish-crates.sh");

    let crates = parse_crates_from_script(&script);
    let core_pos = crates.iter().position(|c| c == "weaveffi-core");
    let gen_positions: Vec<_> = crates
        .iter()
        .enumerate()
        .filter(|(_, c)| c.starts_with("weaveffi-gen-"))
        .map(|(i, _)| i)
        .collect();

    let core_pos = core_pos.expect("weaveffi-core not found");
    for gen_pos in &gen_positions {
        assert!(
            core_pos < *gen_pos,
            "weaveffi-core (pos {core_pos}) must come before generators (pos {gen_pos})"
        );
    }
}
