//! `weaveffi format`: rewrite an IDL file in canonical form (sorted keys,
//! defaults omitted), with `--check` for CI.

use camino::Utf8Path;
use miette::{IntoDiagnostic, Result, WrapErr};

pub(crate) fn cmd_format(input: &str, check: bool, quiet: bool) -> Result<()> {
    let in_path = Utf8Path::new(input);
    let format = super::input_format(in_path)?;
    let (api, contents) = super::load_validated_api(input)?;

    let formatted = match format {
        "yaml" => format_api_yaml(&api)?,
        "json" => format_api_json(&api)?,
        "toml" => toml::to_string_pretty(&api)
            .into_diagnostic()
            .wrap_err("failed to serialize API as TOML")?,
        _ => unreachable!(),
    };

    if check {
        if formatted != contents {
            if !quiet {
                eprintln!(
                    "{input} is not canonically formatted; run 'weaveffi format {input}' to fix"
                );
            }
            std::process::exit(1);
        }
        if !quiet {
            println!("{input} is canonically formatted");
        }
    } else {
        if formatted == contents {
            if !quiet {
                println!("{input} is already canonically formatted");
            }
            return Ok(());
        }
        std::fs::write(in_path.as_std_path(), &formatted)
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to write {}", input))?;
        if !quiet {
            println!("Formatted {input}");
        }
    }
    Ok(())
}

/// Serialize `api` as YAML with deterministic key ordering. `serde_yaml`
/// preserves the order yielded by the serializer, so we round-trip through
/// `serde_json::Value` whose object representation is a `BTreeMap` and
/// therefore alphabetically sorted at every level.
fn format_api_yaml(api: &weaveffi_ir::ir::Api) -> Result<String> {
    let json: serde_json::Value = serde_json::to_value(api)
        .into_diagnostic()
        .wrap_err("failed to convert API to JSON value")?;
    serde_yaml::to_string(&json)
        .into_diagnostic()
        .wrap_err("failed to serialize API as YAML")
}

/// Serialize `api` as pretty-printed JSON with sorted keys at every level by
/// going through `serde_json::Value` (whose `Object` is a `BTreeMap`).
fn format_api_json(api: &weaveffi_ir::ir::Api) -> Result<String> {
    let json: serde_json::Value = serde_json::to_value(api)
        .into_diagnostic()
        .wrap_err("failed to convert API to JSON value")?;
    let mut out = serde_json::to_string_pretty(&json)
        .into_diagnostic()
        .wrap_err("failed to serialize API as JSON")?;
    out.push('\n');
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn format_is_idempotent_and_omits_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let sample_src = format!(
            "{}/../../samples/calculator/calculator.yml",
            env!("CARGO_MANIFEST_DIR")
        );
        let target = dir.path().join("calc.yml");
        std::fs::copy(&sample_src, &target).unwrap();
        let target_str = target.to_str().unwrap();

        // First format establishes the canonical form.
        let out = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args(["--quiet", "format", target_str])
            .output()
            .expect("failed to run weaveffi format");
        assert!(
            out.status.success(),
            "format failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let formatted = std::fs::read_to_string(&target).unwrap();

        // Defaulted fields must not leak into the canonical IDL.
        for needle in ["null", "async:", "cancellable:", "mutable:", ": []"] {
            assert!(
                !formatted.contains(needle),
                "canonical IDL should omit default `{needle}`:\n{formatted}"
            );
        }

        // `--check` now passes on the canonical file (the defect being fixed).
        let check = assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args(["format", target_str, "--check"])
            .output()
            .expect("failed to run weaveffi format --check");
        assert!(
            check.status.success(),
            "format --check should pass on canonical file: {}",
            String::from_utf8_lossy(&check.stderr)
        );

        // Formatting again is a no-op (idempotent).
        assert_cmd::Command::cargo_bin("weaveffi")
            .expect("binary not found")
            .args(["--quiet", "format", target_str])
            .output()
            .expect("failed to run weaveffi format (2nd)");
        assert_eq!(
            formatted,
            std::fs::read_to_string(&target).unwrap(),
            "format must be idempotent"
        );
    }
}
