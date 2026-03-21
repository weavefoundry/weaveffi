use std::collections::{BTreeMap, BTreeSet};
use weaveffi_ir::ir::{Api, ErrorDomain, Function, Module, Param};

#[derive(Debug, thiserror::Error)]
pub enum ValidationError {
    #[error("module has no name")]
    NoModuleName,
    #[error("duplicate module name: {0}")]
    DuplicateModuleName(String),
    #[error("invalid module name '{0}': {1}")]
    InvalidModuleName(String, &'static str),
    #[error("duplicate function name in module '{module}': {function}")]
    DuplicateFunctionName { module: String, function: String },
    #[error("duplicate param name in function '{function}' of module '{module}': {param}")]
    DuplicateParamName {
        module: String,
        function: String,
        param: String,
    },
    #[error("reserved keyword used: {0}")]
    ReservedKeyword(String),
    #[error("invalid identifier '{0}': {1}")]
    InvalidIdentifier(String, &'static str),
    #[error("async functions are not supported in 0.1.0: {module}::{function}")]
    AsyncNotSupported { module: String, function: String },
    #[error("error domain missing name in module '{0}'")]
    ErrorDomainMissingName(String),
    #[error("duplicate error code name in module '{module}': {name}")]
    DuplicateErrorName { module: String, name: String },
    #[error("duplicate error numeric code in module '{module}': {code}")]
    DuplicateErrorCode { module: String, code: i32 },
    #[error("invalid error code in module '{module}' for '{name}': must be non-zero")]
    InvalidErrorCode { module: String, name: String },
    #[error("function name collides with error domain name in module '{module}': {name}")]
    NameCollisionWithErrorDomain { module: String, name: String },
}

const RESERVED: &[&str] = &[
    "if", "else", "for", "while", "loop", "match", "type", "return", "async", "await", "break",
    "continue", "fn", "struct", "enum", "mod", "use",
];

fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        None => false,
        Some(c) if !(c.is_ascii_alphabetic() || c == '_') => false,
        _ => chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
    }
}

fn check_identifier(name: &str) -> Result<(), ValidationError> {
    if !is_valid_identifier(name) {
        return Err(ValidationError::InvalidIdentifier(
            name.to_string(),
            "must start with a letter or underscore and contain only alphanumeric characters or underscores",
        ));
    }
    if RESERVED.contains(&name) {
        return Err(ValidationError::ReservedKeyword(name.to_string()));
    }
    Ok(())
}

pub fn validate_api(api: &Api) -> Result<(), ValidationError> {
    let mut module_names = BTreeSet::new();
    for m in &api.modules {
        if !module_names.insert(m.name.clone()) {
            return Err(ValidationError::DuplicateModuleName(m.name.clone()));
        }
        validate_module(m)?;
    }
    Ok(())
}

fn validate_module(module: &Module) -> Result<(), ValidationError> {
    if module.name.trim().is_empty() {
        return Err(ValidationError::NoModuleName);
    }
    check_identifier(&module.name).map_err(|e| match e {
        ValidationError::ReservedKeyword(_) => {
            ValidationError::InvalidModuleName(module.name.clone(), "reserved word")
        }
        ValidationError::InvalidIdentifier(_, reason) => {
            ValidationError::InvalidModuleName(module.name.clone(), reason)
        }
        other => other,
    })?;

    let mut function_names = BTreeSet::new();
    for f in &module.functions {
        if !function_names.insert(f.name.clone()) {
            return Err(ValidationError::DuplicateFunctionName {
                module: module.name.clone(),
                function: f.name.clone(),
            });
        }
        validate_function(module, f)?;
    }

    if let Some(errors) = &module.errors {
        validate_error_domain(module, errors, &function_names)?;
    }

    Ok(())
}

fn validate_function(module: &Module, f: &Function) -> Result<(), ValidationError> {
    check_identifier(&f.name)?;
    if f.r#async {
        return Err(ValidationError::AsyncNotSupported {
            module: module.name.clone(),
            function: f.name.clone(),
        });
    }

    let mut param_names = BTreeSet::new();
    for p in &f.params {
        validate_param(p)?;
        if !param_names.insert(p.name.clone()) {
            return Err(ValidationError::DuplicateParamName {
                module: module.name.clone(),
                function: f.name.clone(),
                param: p.name.clone(),
            });
        }
    }

    Ok(())
}

fn validate_param(p: &Param) -> Result<(), ValidationError> {
    check_identifier(&p.name)?;
    Ok(())
}

fn validate_error_domain(
    module: &Module,
    errors: &ErrorDomain,
    function_names: &BTreeSet<String>,
) -> Result<(), ValidationError> {
    if errors.name.trim().is_empty() {
        return Err(ValidationError::ErrorDomainMissingName(module.name.clone()));
    }
    if function_names.contains(&errors.name) {
        return Err(ValidationError::NameCollisionWithErrorDomain {
            module: module.name.clone(),
            name: errors.name.clone(),
        });
    }

    let mut by_name: BTreeSet<String> = BTreeSet::new();
    let mut by_code: BTreeMap<i32, String> = BTreeMap::new();
    for c in &errors.codes {
        if c.code == 0 {
            return Err(ValidationError::InvalidErrorCode {
                module: module.name.clone(),
                name: c.name.clone(),
            });
        }
        if !by_name.insert(c.name.clone()) {
            return Err(ValidationError::DuplicateErrorName {
                module: module.name.clone(),
                name: c.name.clone(),
            });
        }
        if by_code.insert(c.code, c.name.clone()).is_some() {
            return Err(ValidationError::DuplicateErrorCode {
                module: module.name.clone(),
                code: c.code,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Api, ErrorCode, ErrorDomain, Function, Module, Param, TypeRef};

    fn simple_function(name: &str) -> Function {
        Function {
            name: name.to_string(),
            params: vec![Param {
                name: "x".to_string(),
                ty: TypeRef::I32,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
        }
    }

    fn simple_module(name: &str) -> Module {
        Module {
            name: name.to_string(),
            functions: vec![simple_function("do_stuff")],
            structs: vec![],
            enums: vec![],
            errors: None,
        }
    }

    fn simple_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![simple_module("mymod")],
        }
    }

    #[test]
    fn valid_api_passes() {
        assert!(validate_api(&simple_api()).is_ok());
    }

    #[test]
    fn duplicate_module_names_rejected() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![simple_module("dup"), simple_module("dup")],
        };
        assert!(matches!(
            validate_api(&api).unwrap_err(),
            ValidationError::DuplicateModuleName(n) if n == "dup"
        ));
    }

    #[test]
    fn duplicate_function_names_rejected() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("same"), simple_function("same")],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&api).unwrap_err(),
            ValidationError::DuplicateFunctionName { .. }
        ));
    }

    #[test]
    fn reserved_keywords_rejected() {
        for kw in ["type", "async"] {
            let api = Api {
                version: "0.1.0".to_string(),
                modules: vec![Module {
                    name: kw.to_string(),
                    functions: vec![simple_function("ok_fn")],
                    structs: vec![],
                    enums: vec![],
                    errors: None,
                }],
            };
            assert!(
                validate_api(&api).is_err(),
                "Expected reserved keyword '{kw}' to be rejected"
            );
        }
    }

    #[test]
    fn invalid_identifiers_rejected() {
        for bad in ["123", "has spaces", ""] {
            let api = Api {
                version: "0.1.0".to_string(),
                modules: vec![Module {
                    name: bad.to_string(),
                    functions: vec![simple_function("ok_fn")],
                    structs: vec![],
                    enums: vec![],
                    errors: None,
                }],
            };
            assert!(
                validate_api(&api).is_err(),
                "Expected invalid identifier '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn async_functions_rejected() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "do_async".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                    r#async: true,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&api).unwrap_err(),
            ValidationError::AsyncNotSupported { .. }
        ));
    }

    #[test]
    fn empty_module_name_rejected() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&api).unwrap_err(),
            ValidationError::NoModuleName
        ));
    }

    #[test]
    fn duplicate_error_codes_rejected() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![],
                errors: Some(ErrorDomain {
                    name: "MyErrors".to_string(),
                    codes: vec![
                        ErrorCode {
                            name: "not_found".to_string(),
                            code: 1,
                            message: "not found".to_string(),
                        },
                        ErrorCode {
                            name: "timeout".to_string(),
                            code: 1,
                            message: "timed out".to_string(),
                        },
                    ],
                }),
            }],
        };
        assert!(matches!(
            validate_api(&api).unwrap_err(),
            ValidationError::DuplicateErrorCode { .. }
        ));
    }
}
