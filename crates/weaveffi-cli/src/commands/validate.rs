//! `weaveffi validate` and `weaveffi lint` — schema validation with
//! human-readable or `--format json` output, and advisory warnings.

use miette::{Report, Result};
use weaveffi_core::validate::{collect_warnings, validate_api, ValidationError, ValidationWarning};

pub(crate) fn cmd_validate(
    input: &str,
    warn: bool,
    format: Option<&str>,
    quiet: bool,
) -> Result<()> {
    let json_mode = format == Some("json");
    let (mut api, contents) = super::load_api(input)?;

    match validate_api(&mut api, Some((input, &contents))) {
        Ok(()) => {
            if warn && !quiet {
                for w in collect_warnings(&api) {
                    eprintln!("warning: {w}");
                }
            }
            let n_modules = api.modules.len();
            let n_functions: usize = api.modules.iter().map(|m| m.functions.len()).sum();
            let n_structs: usize = api.modules.iter().map(|m| m.structs.len()).sum();
            let n_enums: usize = api.modules.iter().map(|m| m.enums.len()).sum();
            if json_mode {
                let json = serde_json::json!({
                    "ok": true,
                    "modules": n_modules,
                    "functions": n_functions,
                    "structs": n_structs,
                    "enums": n_enums,
                });
                println!("{json}");
            } else if !quiet {
                println!("Validation passed");
                println!(
                    "  {} modules, {} functions, {} structs, {} enums",
                    n_modules, n_functions, n_structs, n_enums
                );
            }
            Ok(())
        }
        Err(diag) => {
            if json_mode {
                let json = serde_json::json!({
                    "ok": false,
                    "errors": [validation_error_to_json(&diag.error)],
                });
                println!("{json}");
                std::process::exit(1);
            }
            Err(Report::new(diag))
        }
    }
}

/// Returns `Ok(true)` when the file is clean, `Ok(false)` when warnings were found.
pub(crate) fn cmd_lint(input: &str, format: Option<&str>, quiet: bool) -> Result<bool> {
    let json_mode = format == Some("json");
    let (api, _contents) = super::load_validated_api(input)?;

    let warnings = collect_warnings(&api);
    if json_mode {
        let json = serde_json::json!({
            "ok": warnings.is_empty(),
            "warnings": warnings
                .iter()
                .map(warning_to_json)
                .collect::<Vec<_>>(),
        });
        println!("{json}");
        return Ok(warnings.is_empty());
    }

    if warnings.is_empty() {
        if !quiet {
            println!("No warnings.");
        }
        Ok(true)
    } else {
        if !quiet {
            for w in &warnings {
                eprintln!("warning: {w}");
            }
        }
        Ok(false)
    }
}

/// Stable string code for a [`ValidationError`] variant, used as the `code`
/// field in `weaveffi validate --format json` failure output. Keeping the
/// codes in lock-step with the variant identifiers makes them ergonomic to
/// match against in CI scripts.
fn validation_error_code(err: &ValidationError) -> &'static str {
    match err {
        ValidationError::NoModuleName => "NoModuleName",
        ValidationError::DuplicateModuleName(_) => "DuplicateModuleName",
        ValidationError::InvalidModuleName(_, _) => "InvalidModuleName",
        ValidationError::DuplicateFunctionName { .. } => "DuplicateFunctionName",
        ValidationError::DuplicateParamName { .. } => "DuplicateParamName",
        ValidationError::ReservedKeyword(_) => "ReservedKeyword",
        ValidationError::InvalidIdentifier(_, _) => "InvalidIdentifier",
        ValidationError::ErrorDomainMissingName(_) => "ErrorDomainMissingName",
        ValidationError::DuplicateErrorName { .. } => "DuplicateErrorName",
        ValidationError::DuplicateErrorCode { .. } => "DuplicateErrorCode",
        ValidationError::InvalidErrorCode { .. } => "InvalidErrorCode",
        ValidationError::NameCollisionWithErrorDomain { .. } => "NameCollisionWithErrorDomain",
        ValidationError::DuplicateStructName { .. } => "DuplicateStructName",
        ValidationError::DuplicateStructField { .. } => "DuplicateStructField",
        ValidationError::EmptyStruct { .. } => "EmptyStruct",
        ValidationError::DuplicateEnumName { .. } => "DuplicateEnumName",
        ValidationError::EmptyEnum { .. } => "EmptyEnum",
        ValidationError::DuplicateEnumVariant { .. } => "DuplicateEnumVariant",
        ValidationError::DuplicateEnumVariantField { .. } => "DuplicateEnumVariantField",
        ValidationError::DuplicateEnumValue { .. } => "DuplicateEnumValue",
        ValidationError::UnknownTypeRef { .. } => "UnknownTypeRef",
        ValidationError::InvalidMapKey { .. } => "InvalidMapKey",
        ValidationError::BorrowedTypeInInvalidPosition { .. } => "BorrowedTypeInInvalidPosition",
        ValidationError::DuplicateCallbackName { .. } => "DuplicateCallbackName",
        ValidationError::UnsupportedCallbackParamType { .. } => "UnsupportedCallbackParamType",
        ValidationError::ListenerCallbackNotFound { .. } => "ListenerCallbackNotFound",
        ValidationError::DuplicateListenerName { .. } => "DuplicateListenerName",
        ValidationError::IteratorInInvalidPosition { .. } => "IteratorInInvalidPosition",
        ValidationError::UnsupportedElementType { .. } => "UnsupportedElementType",
        ValidationError::AsyncIteratorReturn { .. } => "AsyncIteratorReturn",
        ValidationError::BuilderStructEmpty { .. } => "BuilderStructEmpty",
        ValidationError::UnsupportedSchemaVersion { .. } => "UnsupportedSchemaVersion",
    }
}

/// Convert a [`ValidationError`] into a JSON object with `code`, the
/// variant-specific identifying fields (`module`, `function`, `name`, …),
/// `message`, and `suggestion` derived from the [`miette::Diagnostic`] help.
fn validation_error_to_json(err: &ValidationError) -> serde_json::Value {
    use miette::Diagnostic;
    use serde_json::{Map, Value};
    let mut obj = Map::new();
    obj.insert(
        "code".into(),
        Value::String(validation_error_code(err).into()),
    );
    match err {
        ValidationError::NoModuleName => {}
        ValidationError::DuplicateModuleName(name) | ValidationError::ReservedKeyword(name) => {
            obj.insert("name".into(), Value::String(name.clone()));
        }
        ValidationError::InvalidModuleName(name, reason)
        | ValidationError::InvalidIdentifier(name, reason) => {
            obj.insert("name".into(), Value::String(name.clone()));
            obj.insert("reason".into(), Value::String((*reason).into()));
        }
        ValidationError::DuplicateFunctionName { module, function } => {
            obj.insert("module".into(), Value::String(module.clone()));
            obj.insert("function".into(), Value::String(function.clone()));
        }
        ValidationError::DuplicateParamName {
            module,
            function,
            param,
        } => {
            obj.insert("module".into(), Value::String(module.clone()));
            obj.insert("function".into(), Value::String(function.clone()));
            obj.insert("param".into(), Value::String(param.clone()));
        }
        ValidationError::ErrorDomainMissingName(module) => {
            obj.insert("module".into(), Value::String(module.clone()));
        }
        ValidationError::DuplicateErrorName { module, name }
        | ValidationError::NameCollisionWithErrorDomain { module, name }
        | ValidationError::InvalidErrorCode { module, name }
        | ValidationError::DuplicateStructName { module, name }
        | ValidationError::EmptyStruct { module, name }
        | ValidationError::DuplicateEnumName { module, name }
        | ValidationError::EmptyEnum { module, name }
        | ValidationError::DuplicateCallbackName { module, name }
        | ValidationError::DuplicateListenerName { module, name }
        | ValidationError::BuilderStructEmpty { module, name } => {
            obj.insert("module".into(), Value::String(module.clone()));
            obj.insert("name".into(), Value::String(name.clone()));
        }
        ValidationError::DuplicateErrorCode { module, code } => {
            obj.insert("module".into(), Value::String(module.clone()));
            obj.insert("error_code".into(), Value::Number((*code).into()));
        }
        ValidationError::DuplicateStructField { struct_name, field } => {
            obj.insert("struct".into(), Value::String(struct_name.clone()));
            obj.insert("field".into(), Value::String(field.clone()));
        }
        ValidationError::DuplicateEnumVariant { enum_name, variant } => {
            obj.insert("enum".into(), Value::String(enum_name.clone()));
            obj.insert("variant".into(), Value::String(variant.clone()));
        }
        ValidationError::DuplicateEnumVariantField {
            enum_name,
            variant,
            field,
        } => {
            obj.insert("enum".into(), Value::String(enum_name.clone()));
            obj.insert("variant".into(), Value::String(variant.clone()));
            obj.insert("field".into(), Value::String(field.clone()));
        }
        ValidationError::DuplicateEnumValue { enum_name, value } => {
            obj.insert("enum".into(), Value::String(enum_name.clone()));
            obj.insert("value".into(), Value::Number((*value).into()));
        }
        ValidationError::UnknownTypeRef { name } => {
            obj.insert("name".into(), Value::String(name.clone()));
        }
        ValidationError::InvalidMapKey { key_type } => {
            obj.insert("key_type".into(), Value::String(key_type.clone()));
        }
        ValidationError::BorrowedTypeInInvalidPosition { ty, location } => {
            obj.insert("type".into(), Value::String(ty.clone()));
            obj.insert("location".into(), Value::String(location.clone()));
        }
        ValidationError::ListenerCallbackNotFound {
            module,
            listener,
            callback,
        } => {
            obj.insert("module".into(), Value::String(module.clone()));
            obj.insert("listener".into(), Value::String(listener.clone()));
            obj.insert("callback".into(), Value::String(callback.clone()));
        }
        ValidationError::UnsupportedCallbackParamType {
            module,
            callback,
            param,
            ty,
        } => {
            obj.insert("module".into(), Value::String(module.clone()));
            obj.insert("callback".into(), Value::String(callback.clone()));
            obj.insert("param".into(), Value::String(param.clone()));
            obj.insert("type".into(), Value::String(ty.clone()));
        }
        ValidationError::IteratorInInvalidPosition { location } => {
            obj.insert("location".into(), Value::String(location.clone()));
        }
        ValidationError::UnsupportedElementType { location, ty } => {
            obj.insert("location".into(), Value::String(location.clone()));
            obj.insert("type".into(), Value::String(ty.clone()));
        }
        ValidationError::AsyncIteratorReturn { module, function } => {
            obj.insert("module".into(), Value::String(module.clone()));
            obj.insert("function".into(), Value::String(function.clone()));
        }
        ValidationError::UnsupportedSchemaVersion { version, supported } => {
            obj.insert("version".into(), Value::String(version.clone()));
            obj.insert("supported".into(), Value::String(supported.clone()));
        }
    }
    obj.insert("message".into(), Value::String(err.to_string()));
    if let Some(help) = err.help() {
        obj.insert("suggestion".into(), Value::String(help.to_string()));
    }
    Value::Object(obj)
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
        ValidationWarning::MutableOnValueType {
            module,
            function,
            param,
        } => (
            "MutableOnValueType",
            format!("{module}::{function}::{param}"),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lint_clean_file_succeeds() {
        let sample = format!(
            "{}/../../samples/calculator/calculator.yml",
            env!("CARGO_MANIFEST_DIR")
        );
        assert!(
            cmd_lint(&sample, None, false).unwrap(),
            "calculator sample should be lint-clean"
        );
    }
}
