//! `weaveffi new` scaffolds a fresh producer project: starter IDL,
//! `Cargo.toml`, `src/lib.rs` (via the scaffold renderer), and a README.

use crate::scaffold;
use camino::Utf8Path;
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use weaveffi_core::validate::validate_api;
use weaveffi_ir::parse::parse_api_str;

pub(crate) fn cmd_new(name: &str, quiet: bool) -> Result<()> {
    let project_dir = Utf8Path::new(name);
    if project_dir.exists() {
        bail!(
            "destination '{}' already exists; choose a new name or remove it",
            name
        );
    }
    std::fs::create_dir_all(project_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create project directory: {}", name))?;

    let module_name = sanitize_module_name(name);

    let idl_path = project_dir.join("weaveffi.yml");
    let idl_contents = format!(
        concat!(
            "version: \"0.4.0\"\n",
            "package:\n",
            "  name: {name}\n",
            "  version: \"0.1.0\"\n",
            "modules:\n",
            "  - name: {module}\n",
            "    structs:\n",
            "      - name: Item\n",
            "        fields:\n",
            "          - {{ name: id, type: i64 }}\n",
            "          - {{ name: name, type: string }}\n",
            "          - {{ name: description, type: \"string?\" }}\n",
            "    functions:\n",
            "      - name: create_item\n",
            "        params:\n",
            "          - {{ name: name, type: string }}\n",
            "          - {{ name: description, type: \"string?\" }}\n",
            "        return: handle\n",
            "      - name: get_item\n",
            "        params:\n",
            "          - {{ name: id, type: handle }}\n",
            "        return: Item\n",
            "      - name: list_items\n",
            "        params: []\n",
            "        return: \"[Item]\"\n",
            "      - name: delete_item\n",
            "        params:\n",
            "          - {{ name: id, type: handle }}\n",
            "        return: bool\n",
        ),
        name = name,
        module = module_name
    );
    std::fs::write(idl_path.as_std_path(), &idl_contents)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", idl_path))?;

    let mut api = parse_api_str(&idl_contents, "yaml").wrap_err("failed to parse generated IDL")?;
    validate_api(&mut api, None).wrap_err("generated IDL failed validation")?;

    let cargo_toml_path = project_dir.join("Cargo.toml");
    let cargo_toml = format!(
        concat!(
            "[package]\n",
            "name = \"{name}\"\n",
            "version = \"0.1.0\"\n",
            "edition = \"2021\"\n",
            "publish = false\n",
            "\n",
            "[lib]\n",
            "crate-type = [\"cdylib\"]\n",
            "\n",
            "[dependencies]\n",
            "weaveffi-abi = \"0.3\"\n",
            "\n",
            "[lints.rust]\n",
            "unsafe_code = \"allow\"\n",
        ),
        name = name,
    );
    std::fs::write(cargo_toml_path.as_std_path(), &cargo_toml)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", cargo_toml_path))?;

    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(src_dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create {}", src_dir))?;
    let lib_rs_path = src_dir.join("lib.rs");
    let lib_contents = scaffold::render_scaffold(&api, "weaveffi");
    std::fs::write(lib_rs_path.as_std_path(), &lib_contents)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", lib_rs_path))?;

    let readme_path = project_dir.join("README.md");
    let readme = format!(
        concat!(
            "# {name}\n\n",
            "A WeaveFFI project.\n\n",
            "## Getting Started\n\n",
            "1. Implement the `todo!()` stubs in `src/lib.rs`.\n",
            "2. Build the shared library:\n\n",
            "   ```sh\n",
            "   cargo build\n",
            "   ```\n\n",
            "3. Generate foreign-language bindings:\n\n",
            "   ```sh\n",
            "   weaveffi generate weaveffi.yml -o generated\n",
            "   ```\n\n",
            "4. Test your library:\n\n",
            "   ```sh\n",
            "   cargo test\n",
            "   ```\n\n",
            "5. Use the generated bindings from Swift, Kotlin, Node.js, Python, .NET, or WASM.\n",
        ),
        name = name,
    );
    std::fs::write(readme_path.as_std_path(), &readme)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {}", readme_path))?;

    if !quiet {
        println!("Initialized WeaveFFI project at {}", project_dir);
        println!("Next steps:");
        println!("  cd {name}");
        println!("  # Implement the todo!() stubs in src/lib.rs");
        println!("  cargo build");
        println!("  weaveffi generate weaveffi.yml -o generated");
    }
    Ok(())
}

fn sanitize_module_name(name: &str) -> String {
    let lowered = name.to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if matches!(ch, '-' | '_' | ' ') {
            out.push('_');
        }
    }
    if out.is_empty() {
        String::from("module")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_creates_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .current_dir(dir.path())
            .args(["new", "test_proj"])
            .output()
            .expect("failed to run weaveffi new");

        assert!(
            cmd.status.success(),
            "weaveffi new failed: {}",
            String::from_utf8_lossy(&cmd.stderr)
        );
        let cargo_toml = dir.path().join("test_proj/Cargo.toml");
        assert!(cargo_toml.exists(), "Cargo.toml should exist");
        let contents = std::fs::read_to_string(&cargo_toml).unwrap();
        assert!(
            contents.contains("cdylib"),
            "Cargo.toml should contain cdylib: {contents}"
        );
    }

    #[test]
    fn new_creates_lib_rs() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .current_dir(dir.path())
            .args(["new", "test_proj"])
            .output()
            .expect("failed to run weaveffi new");

        assert!(
            cmd.status.success(),
            "weaveffi new failed: {}",
            String::from_utf8_lossy(&cmd.stderr)
        );
        let lib_rs = dir.path().join("test_proj/src/lib.rs");
        assert!(lib_rs.exists(), "src/lib.rs should exist");
        let contents = std::fs::read_to_string(&lib_rs).unwrap();
        assert!(
            contents.contains("todo!()"),
            "lib.rs should contain todo!() stubs: {contents}"
        );
    }
}
