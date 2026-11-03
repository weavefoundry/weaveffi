//! `weaveffi validate`: schema validation with human-readable or `--format
//! json` output, plus advisory warnings under `--warn`.

use std::process::ExitCode;

use miette::{Report, Result};
use weaveffi_model::validate::{ValidationError, ValidationWarning};

/// Run `weaveffi validate`, printing one JSON object with `json_mode`
/// (also when the project can't be loaded at all).
pub fn cmd_validate(locate: &super::Locate<'_>, warn: bool, json_mode: bool) -> Result<ExitCode> {
    let quiet = locate.quiet;
    let definition = match locate.project().and_then(|p| p.definition()) {
        Ok(definition) => definition,
        Err(report) if json_mode => {
            let json = serde_json::json!({
                "ok": false,
                "errors": [{
                    "code": report.code().map_or_else(|| "LoadError".to_string(), |c| c.to_string()),
                    "message": format!("{report:#}"),
                }],
            });
            println!("{json}");
            return Ok(ExitCode::FAILURE);
        }
        Err(report) => return Err(report),
    };
    match definition.validate() {
        Ok(_) => {
            let warnings = if warn {
                definition.warnings()
            } else {
                Vec::new()
            };
            let counts = Counts::of(&definition.api);
            if json_mode {
                let json = serde_json::json!({
                    "ok": true,
                    "functions": counts.functions,
                    "interfaces": counts.interfaces,
                    "callbacks": counts.callbacks,
                    "records": counts.records,
                    "enums": counts.enums,
                    "error_domains": counts.error_domains,
                    "warnings": warnings.iter().map(warning_to_json).collect::<Vec<_>>(),
                });
                println!("{json}");
            } else if !quiet {
                for w in &warnings {
                    eprintln!("warning: {w}");
                }
                println!("Validation passed");
                println!("  {}", counts.summary());
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(diags) => {
            if json_mode {
                let json = serde_json::json!({
                    "ok": false,
                    "errors": diags
                        .diagnostics
                        .iter()
                        .map(|d| validation_error_to_json(&d.error))
                        .collect::<Vec<_>>(),
                });
                println!("{json}");
                return Ok(ExitCode::FAILURE);
            }
            Err(Report::new(diags))
        }
    }
}

/// Declaration counts, for the success summary.
#[derive(Default)]
struct Counts {
    functions: usize,
    interfaces: usize,
    callbacks: usize,
    records: usize,
    enums: usize,
    error_domains: usize,
}

impl Counts {
    fn of(api: &weaveffi_model::ir::Api) -> Self {
        Self {
            functions: api.functions.len(),
            interfaces: api.interfaces.len(),
            callbacks: api.callbacks.len(),
            records: api.records.len(),
            enums: api.enums.len(),
            error_domains: api.errors.len(),
        }
    }

    fn summary(&self) -> String {
        let parts = [
            (self.functions, "function", "functions"),
            (self.interfaces, "interface", "interfaces"),
            (self.callbacks, "callback interface", "callback interfaces"),
            (self.records, "record", "records"),
            (self.enums, "enum", "enums"),
            (self.error_domains, "error domain", "error domains"),
        ];
        parts
            .iter()
            .filter(|(n, ..)| *n > 0)
            .map(|(n, one, many)| format!("{n} {}", if *n == 1 { one } else { many }))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Convert a [`ValidationError`] into a JSON object: its `code` (the variant
/// name) and identifying fields from its serialized form, plus `message` and
/// a `suggestion` derived from the [`miette::Diagnostic`] help.
fn validation_error_to_json(err: &ValidationError) -> serde_json::Value {
    use miette::Diagnostic;
    let mut obj = match serde_json::to_value(err) {
        Ok(serde_json::Value::Object(obj)) => obj,
        _ => serde_json::Map::new(),
    };
    obj.insert("message".into(), err.to_string().into());
    if let Some(help) = err.help() {
        obj.insert("suggestion".into(), help.to_string().into());
    }
    serde_json::Value::Object(obj)
}

/// Convert a [`ValidationWarning`] into a JSON object: its serialized form
/// (`code` and fields) plus `message`.
fn warning_to_json(w: &ValidationWarning) -> serde_json::Value {
    let mut obj = match serde_json::to_value(w) {
        Ok(serde_json::Value::Object(obj)) => obj,
        _ => serde_json::Map::new(),
    };
    obj.insert("code".into(), w.code().into());
    obj.insert("message".into(), w.to_string().into());
    serde_json::Value::Object(obj)
}
