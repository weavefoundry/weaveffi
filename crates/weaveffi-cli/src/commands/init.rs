//! `weaveffi init`: set up a project so a bare `weaveffi generate` works.
//!
//! In a Rust crate (a directory with a `Cargo.toml` that has a `[package]`),
//! it writes a `weaveffi.toml` whose `[project] input` is the crate's
//! `src/lib.rs`, then checks the three things a producer needs (a `cdylib`
//! crate type, a `weaveffi` dependency, and one `weaveffi::export_runtime!()`
//! call) and prints any that are missing. It never edits `Cargo.toml` or
//! source files. Anywhere else, it writes a starter IDL plus a
//! `weaveffi.toml` that points at it.

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use weaveffi_model::pkg::c_ident;

use crate::config::CONFIG_FILE_NAME;

/// Options for [`cmd_init`].
pub(crate) struct InitArgs<'a> {
    pub(crate) dir: &'a str,
    pub(crate) name: Option<&'a str>,
    pub(crate) force: bool,
    pub(crate) quiet: bool,
}

pub(crate) fn cmd_init(args: &InitArgs<'_>) -> Result<()> {
    let dir = Utf8PathBuf::from(args.dir);
    std::fs::create_dir_all(dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create {dir}"))?;
    let config_path = dir.join(CONFIG_FILE_NAME);
    if config_path.exists() && !args.force {
        bail!("{config_path} already exists; pass --force to overwrite it");
    }
    let manifest = dir.join("Cargo.toml");
    let crate_name = std::fs::read_to_string(manifest.as_std_path())
        .ok()
        .and_then(|text| text.parse::<toml::Table>().ok())
        .and_then(|doc| {
            doc.get("package")?
                .get("name")?
                .as_str()
                .map(str::to_string)
        });
    match crate_name {
        Some(name) => init_rust(&dir, &config_path, &name, args.quiet),
        None => init_idl(&dir, &config_path, args.name, args.quiet),
    }
}

fn write(path: &Utf8Path, contents: &str) -> Result<()> {
    std::fs::write(path.as_std_path(), contents)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write {path}"))
}

fn init_rust(dir: &Utf8Path, config_path: &Utf8Path, crate_name: &str, quiet: bool) -> Result<()> {
    write(
        config_path,
        "# WeaveFFI project configuration. See https://weaveffi.com/guides/config.html\n\
         [project]\n\
         input = \"src/lib.rs\"\n\
         out = \"bindings\"\n\
         # targets = [\"c\", \"swift\", \"kotlin\", \"python\"]\n",
    )?;
    if quiet {
        return Ok(());
    }
    println!("Wrote {config_path} for the Rust producer `{crate_name}`.");
    let manifest =
        std::fs::read_to_string(dir.join("Cargo.toml").as_std_path()).unwrap_or_default();
    let lib = std::fs::read_to_string(dir.join("src/lib.rs").as_std_path()).unwrap_or_default();
    let mut todo = Vec::new();
    if !manifest.contains("cdylib") {
        todo.push(
            "add `crate-type = [\"cdylib\"]` (and \"staticlib\" for iOS) under [lib] in Cargo.toml",
        );
    }
    if !manifest.contains("weaveffi") {
        todo.push("run `cargo add weaveffi`");
    }
    if !lib.contains("export_runtime!") {
        todo.push("call `weaveffi::export_runtime!();` once in src/lib.rs");
    }
    if !lib.contains("weaveffi::module") {
        todo.push("annotate your API module with `#[weaveffi::module]`");
    }
    if todo.is_empty() {
        println!("The crate is ready: run `weaveffi generate`.");
    } else {
        println!("Before running `weaveffi generate`:");
        for step in todo {
            println!("  - {step}");
        }
    }
    Ok(())
}

fn init_idl(dir: &Utf8Path, config_path: &Utf8Path, name: Option<&str>, quiet: bool) -> Result<()> {
    let name = match name {
        Some(n) => n.to_string(),
        None => std::path::absolute(dir.as_std_path())
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "api".to_string()),
    };
    let module = c_ident(&name);
    let idl_name = format!("{module}.yml");
    let idl_path = dir.join(&idl_name);
    if !idl_path.exists() {
        write(
            &idl_path,
            &format!(
                "# yaml-language-server: $schema=https://weaveffi.com/weaveffi.schema.json\n\
                 version: \"{version}\"\n\
                 modules:\n  \
                   - name: {module}\n    \
                     structs:\n      \
                       - name: Greeting\n        \
                         fields:\n          \
                           - {{ name: text, type: string }}\n          \
                           - {{ name: count, type: u32 }}\n    \
                     functions:\n      \
                       - name: greet\n        \
                         doc: Build a greeting for `name`.\n        \
                         params:\n          \
                           - {{ name: name, type: string }}\n        \
                         return: Greeting\n",
                version = weaveffi_model::ir::CURRENT_SCHEMA_VERSION,
            ),
        )?;
    }
    write(
        config_path,
        &format!(
            "# WeaveFFI project configuration. See https://weaveffi.com/guides/config.html\n\
             [project]\n\
             input = \"{idl_name}\"\n\
             out = \"bindings\"\n\
             \n\
             [package]\n\
             name = \"{name}\"\n\
             version = \"0.1.0\"\n"
        ),
    )?;
    if !quiet {
        println!("Wrote {idl_path} and {config_path}.");
        println!(
            "Run `weaveffi generate --target c` to produce the header your native library \
             implements, then generate the other targets you ship."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idl_project_validates_and_generates() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap().join("greeter");
        cmd_init(&InitArgs {
            dir: root.as_str(),
            name: None,
            force: false,
            quiet: true,
        })
        .unwrap();
        let (cfg, input) =
            crate::config::ProjectConfig::locate(Some(root.join(CONFIG_FILE_NAME).as_str()), None)
                .unwrap();
        assert_eq!(input, root.join("greeter.yml"));
        assert_eq!(cfg.package.name.as_deref(), Some("greeter"));
        crate::commands::load_validated_api(input.as_str()).unwrap();
        let again = cmd_init(&InitArgs {
            dir: root.as_str(),
            name: None,
            force: false,
            quiet: true,
        });
        assert!(again.is_err(), "init must not overwrite without --force");
    }

    #[test]
    fn rust_project_points_at_lib_rs() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"kv\"\n").unwrap();
        cmd_init(&InitArgs {
            dir: root.as_str(),
            name: None,
            force: false,
            quiet: true,
        })
        .unwrap();
        let cfg = crate::config::ProjectConfig::from_file(&root.join(CONFIG_FILE_NAME)).unwrap();
        assert_eq!(
            cfg.project.input.as_deref(),
            Some(Utf8Path::new("src/lib.rs"))
        );
    }
}
