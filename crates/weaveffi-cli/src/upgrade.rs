//! `weaveffi upgrade` — migrate an IDL/IR document from an older schema
//! version to [`CURRENT_SCHEMA_VERSION`].
//!
//! The migration walks the parsed document tree (YAML/JSON/TOML) and applies
//! the version-specific transforms (currently: stripping callback-typed
//! params introduced before `0.2.0`), then rewrites the `version` field.
//! `--check` reports whether a migration *would* change the file without
//! writing it, exiting non-zero so CI can gate on out-of-date IDLs.

use camino::Utf8Path;
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use weaveffi_ir::ir::{CURRENT_SCHEMA_VERSION, SUPPORTED_VERSIONS};

pub(crate) fn cmd_upgrade(
    input: &str,
    output: Option<&str>,
    check: bool,
    quiet: bool,
) -> Result<()> {
    let in_path = Utf8Path::new(input);
    let ext = in_path.extension().unwrap_or("");
    if ext.is_empty() {
        bail!("input file has no extension (expected yml|yaml|json|toml)");
    }
    let format = match ext {
        "yml" | "yaml" => "yaml",
        "json" => "json",
        "toml" => "toml",
        other => bail!(
            "unsupported input format: {} (expected yml|yaml|json|toml)",
            other
        ),
    };
    let contents = std::fs::read_to_string(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read input file: {}", input))?;

    let outcome = match format {
        "yaml" => upgrade_yaml(&contents, quiet)?,
        "json" => upgrade_json(&contents, quiet)?,
        "toml" => upgrade_toml(&contents, quiet)?,
        _ => unreachable!(),
    };

    match outcome {
        UpgradeOutcome::AlreadyCurrent(v) => {
            if !quiet {
                println!("Already up to date (version {v}).");
            }
            Ok(())
        }
        UpgradeOutcome::Migrated {
            from,
            contents: new,
        } => {
            if check {
                if new != contents {
                    if !quiet {
                        eprintln!(
                            "{input} is outdated (version {from}); run 'weaveffi upgrade {input}' to migrate"
                        );
                    }
                    std::process::exit(2);
                }
                return Ok(());
            }
            let dest = output.unwrap_or(input);
            std::fs::write(dest, &new)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to write output file: {}", dest))?;
            if !quiet {
                println!("Upgraded {dest} from {from} to {CURRENT_SCHEMA_VERSION}");
            }
            Ok(())
        }
    }
}

enum UpgradeOutcome {
    AlreadyCurrent(String),
    Migrated { from: String, contents: String },
}

fn read_version_str<F>(get: F) -> Result<String>
where
    F: FnOnce() -> Option<String>,
{
    get().ok_or_else(|| {
        miette!("missing or non-string 'version' field; cannot determine schema version to migrate from")
    })
}

fn ensure_supported(version: &str) -> Result<()> {
    if !SUPPORTED_VERSIONS.contains(&version) {
        bail!(
            "unsupported source version '{}'; supported: {}",
            version,
            SUPPORTED_VERSIONS.join(", ")
        );
    }
    Ok(())
}

fn upgrade_yaml(input: &str, quiet: bool) -> Result<UpgradeOutcome> {
    let mut value: serde_yaml::Value = serde_yaml::from_str(input)
        .into_diagnostic()
        .wrap_err("failed to parse YAML")?;
    let version = read_version_str(|| {
        value
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })?;
    if version == CURRENT_SCHEMA_VERSION {
        return Ok(UpgradeOutcome::AlreadyCurrent(version));
    }
    ensure_supported(&version)?;
    if let serde_yaml::Value::Mapping(map) = &mut value {
        if let Some(modules) = map.get_mut("modules") {
            yaml_strip_callback_params(modules, "", &version, quiet);
        }
        map.insert(
            serde_yaml::Value::String("version".into()),
            serde_yaml::Value::String(CURRENT_SCHEMA_VERSION.into()),
        );
    }
    let new_contents = serde_yaml::to_string(&value)
        .into_diagnostic()
        .wrap_err("failed to serialize YAML")?;
    Ok(UpgradeOutcome::Migrated {
        from: version,
        contents: new_contents,
    })
}

fn yaml_strip_callback_params(
    modules: &mut serde_yaml::Value,
    parent_path: &str,
    from_version: &str,
    quiet: bool,
) {
    if from_version == "0.1.0" {
        return;
    }
    let serde_yaml::Value::Sequence(mods) = modules else {
        return;
    };
    for module in mods.iter_mut() {
        let serde_yaml::Value::Mapping(map) = module else {
            continue;
        };
        let module_name = map
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let qualified = if parent_path.is_empty() {
            module_name
        } else {
            format!("{parent_path}.{module_name}")
        };
        if let Some(serde_yaml::Value::Sequence(fns)) = map.get_mut("functions") {
            for func in fns.iter_mut() {
                let serde_yaml::Value::Mapping(fmap) = func else {
                    continue;
                };
                let fname = fmap
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                if let Some(serde_yaml::Value::Sequence(p)) = fmap.get_mut("params") {
                    p.retain(|param| {
                        let serde_yaml::Value::Mapping(pmap) = param else {
                            return true;
                        };
                        let ty = pmap.get("type").and_then(|v| v.as_str());
                        if ty == Some("callback") {
                            let pname = pmap
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)");
                            if !quiet {
                                eprintln!(
                                    "warning: removed callback-typed param '{pname}' from function '{qualified}::{fname}'"
                                );
                            }
                            return false;
                        }
                        true
                    });
                }
            }
        }
        if let Some(submodules) = map.get_mut("modules") {
            yaml_strip_callback_params(submodules, &qualified, from_version, quiet);
        }
    }
}

fn upgrade_json(input: &str, quiet: bool) -> Result<UpgradeOutcome> {
    let mut value: serde_json::Value = serde_json::from_str(input)
        .into_diagnostic()
        .wrap_err("failed to parse JSON")?;
    let version = read_version_str(|| {
        value
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })?;
    if version == CURRENT_SCHEMA_VERSION {
        return Ok(UpgradeOutcome::AlreadyCurrent(version));
    }
    ensure_supported(&version)?;
    if let serde_json::Value::Object(map) = &mut value {
        if let Some(modules) = map.get_mut("modules") {
            json_strip_callback_params(modules, "", &version, quiet);
        }
        map.insert(
            "version".to_string(),
            serde_json::Value::String(CURRENT_SCHEMA_VERSION.into()),
        );
    }
    let new_contents = serde_json::to_string_pretty(&value)
        .into_diagnostic()
        .wrap_err("failed to serialize JSON")?;
    Ok(UpgradeOutcome::Migrated {
        from: version,
        contents: new_contents,
    })
}

fn json_strip_callback_params(
    modules: &mut serde_json::Value,
    parent_path: &str,
    from_version: &str,
    quiet: bool,
) {
    if from_version == "0.1.0" {
        return;
    }
    let serde_json::Value::Array(mods) = modules else {
        return;
    };
    for module in mods.iter_mut() {
        let serde_json::Value::Object(map) = module else {
            continue;
        };
        let module_name = map
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let qualified = if parent_path.is_empty() {
            module_name
        } else {
            format!("{parent_path}.{module_name}")
        };
        if let Some(serde_json::Value::Array(fns)) = map.get_mut("functions") {
            for func in fns.iter_mut() {
                let serde_json::Value::Object(fmap) = func else {
                    continue;
                };
                let fname = fmap
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                if let Some(serde_json::Value::Array(p)) = fmap.get_mut("params") {
                    p.retain(|param| {
                        let serde_json::Value::Object(pmap) = param else {
                            return true;
                        };
                        let ty = pmap.get("type").and_then(|v| v.as_str());
                        if ty == Some("callback") {
                            let pname = pmap
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)");
                            if !quiet {
                                eprintln!(
                                    "warning: removed callback-typed param '{pname}' from function '{qualified}::{fname}'"
                                );
                            }
                            return false;
                        }
                        true
                    });
                }
            }
        }
        if let Some(submodules) = map.get_mut("modules") {
            json_strip_callback_params(submodules, &qualified, from_version, quiet);
        }
    }
}

fn upgrade_toml(input: &str, quiet: bool) -> Result<UpgradeOutcome> {
    let mut value: toml::Value = toml::from_str(input)
        .into_diagnostic()
        .wrap_err("failed to parse TOML")?;
    let version = read_version_str(|| {
        value
            .get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    })?;
    if version == CURRENT_SCHEMA_VERSION {
        return Ok(UpgradeOutcome::AlreadyCurrent(version));
    }
    ensure_supported(&version)?;
    if let toml::Value::Table(map) = &mut value {
        if let Some(modules) = map.get_mut("modules") {
            toml_strip_callback_params(modules, "", &version, quiet);
        }
        map.insert(
            "version".to_string(),
            toml::Value::String(CURRENT_SCHEMA_VERSION.into()),
        );
    }
    let new_contents = toml::to_string_pretty(&value)
        .into_diagnostic()
        .wrap_err("failed to serialize TOML")?;
    Ok(UpgradeOutcome::Migrated {
        from: version,
        contents: new_contents,
    })
}

fn toml_strip_callback_params(
    modules: &mut toml::Value,
    parent_path: &str,
    from_version: &str,
    quiet: bool,
) {
    if from_version == "0.1.0" {
        return;
    }
    let toml::Value::Array(mods) = modules else {
        return;
    };
    for module in mods.iter_mut() {
        let toml::Value::Table(map) = module else {
            continue;
        };
        let module_name = map
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("(unnamed)")
            .to_string();
        let qualified = if parent_path.is_empty() {
            module_name
        } else {
            format!("{parent_path}.{module_name}")
        };
        if let Some(toml::Value::Array(fns)) = map.get_mut("functions") {
            for func in fns.iter_mut() {
                let toml::Value::Table(fmap) = func else {
                    continue;
                };
                let fname = fmap
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                if let Some(toml::Value::Array(p)) = fmap.get_mut("params") {
                    p.retain(|param| {
                        let toml::Value::Table(pmap) = param else {
                            return true;
                        };
                        let ty = pmap.get("type").and_then(|v| v.as_str());
                        if ty == Some("callback") {
                            let pname = pmap
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("(unnamed)");
                            if !quiet {
                                eprintln!(
                                    "warning: removed callback-typed param '{pname}' from function '{qualified}::{fname}'"
                                );
                            }
                            return false;
                        }
                        true
                    });
                }
            }
        }
        if let Some(submodules) = map.get_mut("modules") {
            toml_strip_callback_params(submodules, &qualified, from_version, quiet);
        }
    }
}
