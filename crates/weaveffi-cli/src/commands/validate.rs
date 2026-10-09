//! `weaveffi validate`: schema validation with human-readable or `--format
//! json` output, plus advisory warnings under `--warn`.

use miette::{Report, Result};
use weaveffi_model::validate::{ValidationError, ValidationWarning};

pub(crate) fn cmd_validate(locate: &super::Locate<'_>, warn: bool, json_mode: bool) -> Result<()> {
    let quiet = locate.quiet;
    let definition = locate.project()?.definition()?;
    match definition.validate() {
        Ok(_) => {
            let warnings = if warn {
                definition.warnings()
            } else {
                Vec::new()
            };
            let counts = Counts::of(&definition.api.modules);
            if json_mode {
                let json = serde_json::json!({
                    "ok": true,
                    "modules": counts.modules,
                    "functions": counts.functions,
                    "interfaces": counts.interfaces,
                    "callback_interfaces": counts.callback_interfaces,
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
            Ok(())
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
                std::process::exit(1);
            }
            Err(Report::new(diags))
        }
    }
}

/// Declaration counts across a module tree, for the success summary.
#[derive(Default)]
struct Counts {
    modules: usize,
    functions: usize,
    interfaces: usize,
    callback_interfaces: usize,
    records: usize,
    enums: usize,
    error_domains: usize,
}

impl Counts {
    fn of(modules: &[weaveffi_model::ir::Module]) -> Self {
        let mut c = Self::default();
        c.add(modules);
        c
    }

    fn add(&mut self, modules: &[weaveffi_model::ir::Module]) {
        for m in modules {
            self.modules += 1;
            self.functions += m.functions.len();
            self.interfaces += m.interfaces.len();
            self.callback_interfaces += m.callback_interfaces.len();
            self.records += m.structs.len();
            self.enums += m.enums.len();
            self.error_domains += usize::from(m.errors.is_some());
            self.add(&m.modules);
        }
    }

    fn summary(&self) -> String {
        let parts = [
            (self.modules, "module", "modules"),
            (self.functions, "function", "functions"),
            (self.interfaces, "interface", "interfaces"),
            (
                self.callback_interfaces,
                "callback interface",
                "callback interfaces",
            ),
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

/// Convert a [`ValidationWarning`] into a JSON object of `{ code, location,
/// message }`. Variants that do not carry an explicit `location` field
/// synthesize one from the available identifiers (e.g. `module::function`).
fn warning_to_json(w: &ValidationWarning) -> serde_json::Value {
    let (code, location) = match w {
        ValidationWarning::LargeEnumVariantCount { enum_name, .. } => {
            ("LargeEnumVariantCount", enum_name.clone())
        }
        ValidationWarning::DeepNesting { location, .. } => ("DeepNesting", location.clone()),
        ValidationWarning::EmptyModuleDoc { module } => ("EmptyModuleDoc", module.clone()),
        ValidationWarning::AsyncVoidFunction { module, function } => {
            ("AsyncVoidFunction", format!("{module}::{function}"))
        }
        ValidationWarning::DeprecatedFunction {
            module, function, ..
        } => ("DeprecatedFunction", format!("{module}::{function}")),
    };
    serde_json::json!({
        "code": code,
        "location": location,
        "message": w.to_string(),
    })
}
