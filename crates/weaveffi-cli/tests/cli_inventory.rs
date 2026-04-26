use std::path::Path;

#[test]
fn generate_inventory_all_targets() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let repo_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
    let input = repo_root.join("samples/inventory/inventory.yml");

    let out_dir = tempfile::tempdir().expect("failed to create temp dir");
    let out_path = out_dir.path();

    assert_cmd::Command::cargo_bin("weaveffi")
        .expect("binary not found")
        .args([
            "generate",
            input.to_str().unwrap(),
            "-o",
            out_path.to_str().unwrap(),
        ])
        .assert()
        .success();

    // All target directories exist
    for dir in ["c", "swift", "android", "node", "wasm", "python", "dotnet"] {
        assert!(
            out_path.join(dir).exists(),
            "missing target directory: {dir}"
        );
    }

    // C header contains functions for both modules
    let header =
        std::fs::read_to_string(out_path.join("c/weaveffi.h")).expect("missing c/weaveffi.h");
    for prefix in ["weaveffi_products_", "weaveffi_orders_"] {
        assert!(
            header.contains(prefix),
            "c/weaveffi.h should contain {prefix} prefixed functions"
        );
    }

    // Node types.d.ts contains disposable class wrappers for all structs
    let types_dts =
        std::fs::read_to_string(out_path.join("node/types.d.ts")).expect("missing node/types.d.ts");
    for class in [
        "export declare class Product",
        "export declare class OrderItem",
        "export declare class Order",
    ] {
        assert!(
            types_dts.contains(class),
            "node/types.d.ts should contain {class}"
        );
    }
    assert!(
        types_dts.matches("dispose(): void").count() >= 3,
        "node/types.d.ts should define dispose() on all struct wrappers"
    );

    // Python weaveffi.py contains functions from both modules
    let weaveffi_py = std::fs::read_to_string(out_path.join("python/weaveffi/weaveffi.py"))
        .expect("missing python/weaveffi/weaveffi.py");
    assert!(
        weaveffi_py.contains("def products_create_product"),
        "weaveffi.py should contain products module function"
    );
    assert!(
        weaveffi_py.contains("def orders_create_order"),
        "weaveffi.py should contain orders module function"
    );
}
