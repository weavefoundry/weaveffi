use std::collections::{BTreeMap, BTreeSet};
use weaveffi_ir::ir::{Api, ErrorDomain, Function, Module, Param, TypeRef};

#[derive(Debug, Clone)]
pub enum ValidationWarning {
    DeprecatedHandleType { module: String, function: String },
    LargeEnumVariantCount { enum_name: String, count: usize },
    DeepNesting { location: String, depth: usize },
    EmptyModuleDoc { module: String },
}

pub fn collect_warnings(api: &Api) -> Vec<ValidationWarning> {
    let mut warnings = Vec::new();
    for module in &api.modules {
        for f in &module.functions {
            if function_uses_handle(f) {
                warnings.push(ValidationWarning::DeprecatedHandleType {
                    module: module.name.clone(),
                    function: f.name.clone(),
                });
            }
        }

        for e in &module.enums {
            if e.variants.len() > 100 {
                warnings.push(ValidationWarning::LargeEnumVariantCount {
                    enum_name: e.name.clone(),
                    count: e.variants.len(),
                });
            }
        }

        for f in &module.functions {
            for p in &f.params {
                let depth = nesting_depth(&p.ty);
                if depth > 3 {
                    warnings.push(ValidationWarning::DeepNesting {
                        location: format!("{}::{}::{}", module.name, f.name, p.name),
                        depth,
                    });
                }
            }
            if let Some(ret) = &f.returns {
                let depth = nesting_depth(ret);
                if depth > 3 {
                    warnings.push(ValidationWarning::DeepNesting {
                        location: format!("{}::{}::return", module.name, f.name),
                        depth,
                    });
                }
            }
        }
        for s in &module.structs {
            for field in &s.fields {
                let depth = nesting_depth(&field.ty);
                if depth > 3 {
                    warnings.push(ValidationWarning::DeepNesting {
                        location: format!("{}::{}::{}", module.name, s.name, field.name),
                        depth,
                    });
                }
            }
        }

        if !module.functions.is_empty() && module.functions.iter().all(|f| f.doc.is_none()) {
            warnings.push(ValidationWarning::EmptyModuleDoc {
                module: module.name.clone(),
            });
        }
    }
    warnings
}

fn function_uses_handle(f: &Function) -> bool {
    f.params.iter().any(|p| contains_handle(&p.ty))
        || f.returns.as_ref().is_some_and(|r| contains_handle(r))
}

fn contains_handle(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Handle => true,
        TypeRef::Optional(inner) | TypeRef::List(inner) => contains_handle(inner),
        TypeRef::Map(k, v) => contains_handle(k) || contains_handle(v),
        _ => false,
    }
}

fn nesting_depth(ty: &TypeRef) -> usize {
    match ty {
        TypeRef::Optional(inner) | TypeRef::List(inner) => 1 + nesting_depth(inner),
        TypeRef::Map(k, v) => nesting_depth(k).max(nesting_depth(v)),
        _ => 0,
    }
}

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
    #[error("duplicate struct name in module '{module}': {name}")]
    DuplicateStructName { module: String, name: String },
    #[error("duplicate field name in struct '{struct_name}': {field}")]
    DuplicateStructField { struct_name: String, field: String },
    #[error("empty struct in module '{module}': {name}")]
    EmptyStruct { module: String, name: String },
    #[error("duplicate enum name in module '{module}': {name}")]
    DuplicateEnumName { module: String, name: String },
    #[error("empty enum in module '{module}': {name}")]
    EmptyEnum { module: String, name: String },
    #[error("duplicate enum variant in enum '{enum_name}': {variant}")]
    DuplicateEnumVariant { enum_name: String, variant: String },
    #[error("duplicate enum value in enum '{enum_name}': {value}")]
    DuplicateEnumValue { enum_name: String, value: i32 },
    #[error("unknown type reference: {name}")]
    UnknownTypeRef { name: String },
    #[error("invalid map key type: {key_type}; only primitive types and strings are allowed as map keys")]
    InvalidMapKey { key_type: String },
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

pub fn validate_api(api: &mut Api) -> Result<(), ValidationError> {
    let mut module_names = BTreeSet::new();
    for m in &api.modules {
        if !module_names.insert(m.name.clone()) {
            return Err(ValidationError::DuplicateModuleName(m.name.clone()));
        }
        validate_module(m)?;
    }
    resolve_type_refs(api);
    Ok(())
}

pub fn resolve_type_refs(api: &mut Api) {
    for module in &mut api.modules {
        let enum_names: BTreeSet<&str> = module.enums.iter().map(|e| e.name.as_str()).collect();
        for f in &mut module.functions {
            for p in &mut f.params {
                resolve_single_type_ref(&mut p.ty, &enum_names);
            }
            if let Some(ret) = &mut f.returns {
                resolve_single_type_ref(ret, &enum_names);
            }
        }
        for s in &mut module.structs {
            for field in &mut s.fields {
                resolve_single_type_ref(&mut field.ty, &enum_names);
            }
        }
    }
}

fn resolve_single_type_ref(ty: &mut TypeRef, enum_names: &BTreeSet<&str>) {
    match ty {
        TypeRef::Struct(name) if enum_names.contains(name.as_str()) => {
            let name = std::mem::take(name);
            *ty = TypeRef::Enum(name);
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) => {
            resolve_single_type_ref(inner, enum_names);
        }
        TypeRef::Map(k, v) => {
            resolve_single_type_ref(k, enum_names);
            resolve_single_type_ref(v, enum_names);
        }
        _ => {}
    }
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

    let mut struct_names = BTreeSet::new();
    for s in &module.structs {
        check_identifier(&s.name)?;
        if !struct_names.insert(s.name.clone()) {
            return Err(ValidationError::DuplicateStructName {
                module: module.name.clone(),
                name: s.name.clone(),
            });
        }
        if s.fields.is_empty() {
            return Err(ValidationError::EmptyStruct {
                module: module.name.clone(),
                name: s.name.clone(),
            });
        }
        let mut field_names = BTreeSet::new();
        for f in &s.fields {
            check_identifier(&f.name)?;
            if !field_names.insert(f.name.clone()) {
                return Err(ValidationError::DuplicateStructField {
                    struct_name: s.name.clone(),
                    field: f.name.clone(),
                });
            }
        }
    }

    let mut enum_names = BTreeSet::new();
    for e in &module.enums {
        check_identifier(&e.name)?;
        if !enum_names.insert(e.name.clone()) {
            return Err(ValidationError::DuplicateEnumName {
                module: module.name.clone(),
                name: e.name.clone(),
            });
        }
        if e.variants.is_empty() {
            return Err(ValidationError::EmptyEnum {
                module: module.name.clone(),
                name: e.name.clone(),
            });
        }
        let mut variant_names = BTreeSet::new();
        let mut variant_values = BTreeMap::new();
        for v in &e.variants {
            check_identifier(&v.name)?;
            if !variant_names.insert(v.name.clone()) {
                return Err(ValidationError::DuplicateEnumVariant {
                    enum_name: e.name.clone(),
                    variant: v.name.clone(),
                });
            }
            if variant_values.insert(v.value, v.name.clone()).is_some() {
                return Err(ValidationError::DuplicateEnumValue {
                    enum_name: e.name.clone(),
                    value: v.value,
                });
            }
        }
    }

    let known_types: BTreeSet<&str> = struct_names
        .iter()
        .map(|s| s.as_str())
        .chain(enum_names.iter().map(|s| s.as_str()))
        .collect();
    for s in &module.structs {
        for f in &s.fields {
            validate_type_ref(&f.ty, &known_types)?;
        }
    }
    for f in &module.functions {
        for p in &f.params {
            validate_type_ref(&p.ty, &known_types)?;
        }
        if let Some(ret) = &f.returns {
            validate_type_ref(ret, &known_types)?;
        }
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

fn validate_type_ref(ty: &TypeRef, known: &BTreeSet<&str>) -> Result<(), ValidationError> {
    match ty {
        TypeRef::Struct(name) | TypeRef::Enum(name) => {
            if !known.contains(name.as_str()) {
                return Err(ValidationError::UnknownTypeRef { name: name.clone() });
            }
            Ok(())
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) => validate_type_ref(inner, known),
        TypeRef::Map(k, v) => {
            let bad_key = match k.as_ref() {
                TypeRef::Struct(name) => Some(format!("struct {name}")),
                TypeRef::List(_) => Some("list".to_string()),
                TypeRef::Map(_, _) => Some("map".to_string()),
                _ => None,
            };
            if let Some(key_type) = bad_key {
                return Err(ValidationError::InvalidMapKey { key_type });
            }
            validate_type_ref(k, known)?;
            validate_type_ref(v, known)
        }
        _ => Ok(()),
    }
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
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
        StructField, TypeRef,
    };

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
        let mut api = simple_api();
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn duplicate_module_names_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![simple_module("dup"), simple_module("dup")],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateModuleName(n) if n == "dup"
        ));
    }

    #[test]
    fn duplicate_function_names_rejected() {
        let mut api = Api {
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
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateFunctionName { .. }
        ));
    }

    #[test]
    fn reserved_keywords_rejected() {
        for kw in ["type", "async"] {
            let mut api = Api {
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
                validate_api(&mut api).is_err(),
                "Expected reserved keyword '{kw}' to be rejected"
            );
        }
    }

    #[test]
    fn invalid_identifiers_rejected() {
        for bad in ["123", "has spaces", ""] {
            let mut api = Api {
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
                validate_api(&mut api).is_err(),
                "Expected invalid identifier '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn async_functions_rejected() {
        let mut api = Api {
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
            validate_api(&mut api).unwrap_err(),
            ValidationError::AsyncNotSupported { .. }
        ));
    }

    #[test]
    fn empty_module_name_rejected() {
        let mut api = Api {
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
            validate_api(&mut api).unwrap_err(),
            ValidationError::NoModuleName
        ));
    }

    #[test]
    fn doc_example_error_domain_validates() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                functions: vec![
                    Function {
                        name: "create_contact".to_string(),
                        params: vec![
                            Param {
                                name: "name".to_string(),
                                ty: TypeRef::StringUtf8,
                            },
                            Param {
                                name: "email".to_string(),
                                ty: TypeRef::StringUtf8,
                            },
                        ],
                        returns: Some(TypeRef::Handle),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "get_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                    },
                ],
                structs: vec![],
                enums: vec![],
                errors: Some(ErrorDomain {
                    name: "ContactErrors".to_string(),
                    codes: vec![
                        ErrorCode {
                            name: "not_found".to_string(),
                            code: 1,
                            message: "Contact not found".to_string(),
                        },
                        ErrorCode {
                            name: "duplicate".to_string(),
                            code: 2,
                            message: "Contact already exists".to_string(),
                        },
                        ErrorCode {
                            name: "invalid_email".to_string(),
                            code: 3,
                            message: "Email address is invalid".to_string(),
                        },
                    ],
                }),
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn error_code_zero_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![],
                errors: Some(ErrorDomain {
                    name: "MyErrors".to_string(),
                    codes: vec![ErrorCode {
                        name: "success".to_string(),
                        code: 0,
                        message: "should fail".to_string(),
                    }],
                }),
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::InvalidErrorCode { module, name }
                if module == "mymod" && name == "success"
        ));
    }

    #[test]
    fn error_domain_name_collision_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("do_stuff")],
                structs: vec![],
                enums: vec![],
                errors: Some(ErrorDomain {
                    name: "do_stuff".to_string(),
                    codes: vec![ErrorCode {
                        name: "fail".to_string(),
                        code: 1,
                        message: "failed".to_string(),
                    }],
                }),
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::NameCollisionWithErrorDomain { module, name }
                if module == "mymod" && name == "do_stuff"
        ));
    }

    #[test]
    fn duplicate_error_names_rejected() {
        let mut api = Api {
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
                            name: "fail".to_string(),
                            code: 1,
                            message: "failed".to_string(),
                        },
                        ErrorCode {
                            name: "fail".to_string(),
                            code: 2,
                            message: "also failed".to_string(),
                        },
                    ],
                }),
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateErrorName { module, name }
                if module == "mymod" && name == "fail"
        ));
    }

    #[test]
    fn duplicate_error_codes_rejected() {
        let mut api = Api {
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
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateErrorCode { .. }
        ));
    }

    fn simple_struct(name: &str) -> StructDef {
        StructDef {
            name: name.to_string(),
            doc: None,
            fields: vec![StructField {
                name: "x".to_string(),
                ty: TypeRef::I32,
                doc: None,
            }],
        }
    }

    #[test]
    fn duplicate_struct_names_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![simple_struct("Point"), simple_struct("Point")],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateStructName { module, name }
                if module == "mymod" && name == "Point"
        ));
    }

    #[test]
    fn empty_struct_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![StructDef {
                    name: "Empty".to_string(),
                    doc: None,
                    fields: vec![],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::EmptyStruct { module, name }
                if module == "mymod" && name == "Empty"
        ));
    }

    #[test]
    fn duplicate_struct_field_names_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![StructDef {
                    name: "Point".to_string(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "x".to_string(),
                            ty: TypeRef::I32,
                            doc: None,
                        },
                        StructField {
                            name: "x".to_string(),
                            ty: TypeRef::F64,
                            doc: None,
                        },
                    ],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateStructField { struct_name, field }
                if struct_name == "Point" && field == "x"
        ));
    }

    fn simple_enum(name: &str) -> EnumDef {
        EnumDef {
            name: name.to_string(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "A".to_string(),
                    value: 0,
                    doc: None,
                },
                EnumVariant {
                    name: "B".to_string(),
                    value: 1,
                    doc: None,
                },
            ],
        }
    }

    #[test]
    fn duplicate_enum_names_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![simple_enum("Color"), simple_enum("Color")],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateEnumName { module, name }
                if module == "mymod" && name == "Color"
        ));
    }

    #[test]
    fn empty_enum_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Empty".to_string(),
                    doc: None,
                    variants: vec![],
                }],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::EmptyEnum { module, name }
                if module == "mymod" && name == "Empty"
        ));
    }

    #[test]
    fn duplicate_enum_variant_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Color".to_string(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Red".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Red".to_string(),
                            value: 1,
                            doc: None,
                        },
                    ],
                }],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateEnumVariant { enum_name, variant }
                if enum_name == "Color" && variant == "Red"
        ));
    }

    #[test]
    fn duplicate_enum_value_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Color".to_string(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Red".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Green".to_string(),
                            value: 0,
                            doc: None,
                        },
                    ],
                }],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateEnumValue { enum_name, value }
                if enum_name == "Color" && value == 0
        ));
    }

    #[test]
    fn unknown_type_ref_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "do_stuff".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::Struct("Foo".to_string()),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::UnknownTypeRef { name } if name == "Foo"
        ));
    }

    #[test]
    fn valid_struct_ref_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "do_stuff".to_string(),
                    params: vec![Param {
                        name: "p".to_string(),
                        ty: TypeRef::Struct("Point".to_string()),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![simple_struct("Point")],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn unknown_type_ref_in_optional_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "do_stuff".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Struct("Bar".to_string()))),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::UnknownTypeRef { name } if name == "Bar"
        ));
    }

    #[test]
    fn unknown_type_ref_in_list_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "do_stuff".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Baz".to_string())))),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::UnknownTypeRef { name } if name == "Baz"
        ));
    }

    #[test]
    fn struct_field_referencing_unknown_type() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![StructDef {
                    name: "Wrapper".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "inner".to_string(),
                        ty: TypeRef::Struct("Nonexistent".to_string()),
                        doc: None,
                    }],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::UnknownTypeRef { name } if name == "Nonexistent"
        ));
    }

    #[test]
    fn function_param_with_optional_struct() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "save".to_string(),
                    params: vec![Param {
                        name: "c".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Struct("Contact".to_string()))),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    }],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn function_param_with_list_of_enums() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "paint".to_string(),
                    params: vec![Param {
                        name: "colors".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::Enum("Color".to_string()))),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn nested_optional_list_validates() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "list_contacts".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                        TypeRef::Struct("Contact".to_string()),
                    ))))),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    }],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn enum_variant_value_zero_allowed() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "Status".to_string(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Unknown".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Active".to_string(),
                            value: 1,
                            doc: None,
                        },
                    ],
                }],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn valid_enum_ref_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "get_color".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Enum("Color".to_string())),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn resolve_enum_ref_in_function_param() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "paint".to_string(),
                    params: vec![Param {
                        name: "color".to_string(),
                        ty: TypeRef::Struct("Color".to_string()),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                errors: None,
            }],
        };
        validate_api(&mut api).unwrap();
        assert_eq!(
            api.modules[0].functions[0].params[0].ty,
            TypeRef::Enum("Color".to_string())
        );
    }

    #[test]
    fn resolve_enum_ref_in_optional() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "paint".to_string(),
                    params: vec![Param {
                        name: "color".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Struct("Color".to_string()))),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                errors: None,
            }],
        };
        validate_api(&mut api).unwrap();
        assert_eq!(
            api.modules[0].functions[0].params[0].ty,
            TypeRef::Optional(Box::new(TypeRef::Enum("Color".to_string())))
        );
    }

    #[test]
    fn struct_ref_not_changed() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "save".to_string(),
                    params: vec![Param {
                        name: "c".to_string(),
                        ty: TypeRef::Struct("Contact".to_string()),
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                }],
                structs: vec![simple_struct("Contact")],
                enums: vec![],
                errors: None,
            }],
        };
        validate_api(&mut api).unwrap();
        assert_eq!(
            api.modules[0].functions[0].params[0].ty,
            TypeRef::Struct("Contact".to_string())
        );
    }

    #[test]
    fn map_with_string_key_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "get_map".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::I32),
                    )),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn map_with_struct_key_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "get_map".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::Struct("Point".to_string())),
                        Box::new(TypeRef::I32),
                    )),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![simple_struct("Point")],
                enums: vec![],
                errors: None,
            }],
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::InvalidMapKey { key_type } if key_type == "struct Point"
        ));
    }

    #[test]
    fn map_with_enum_key_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "get_map".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".to_string())),
                        Box::new(TypeRef::StringUtf8),
                    )),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                errors: None,
            }],
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn warning_deprecated_handle_in_param() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "get_thing".to_string(),
                    params: vec![Param {
                        name: "h".to_string(),
                        ty: TypeRef::Handle,
                    }],
                    returns: None,
                    doc: Some("documented".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert_eq!(warnings.len(), 1);
        assert!(matches!(
            &warnings[0],
            ValidationWarning::DeprecatedHandleType { module, function }
                if module == "mymod" && function == "get_thing"
        ));
    }

    #[test]
    fn warning_deprecated_handle_in_return() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "create".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Handle),
                    doc: Some("documented".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::DeprecatedHandleType { function, .. } if function == "create"
        )));
    }

    #[test]
    fn warning_deprecated_handle_nested() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "find".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::Handle))),
                    doc: Some("documented".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::DeprecatedHandleType { function, .. } if function == "find"
        )));
    }

    #[test]
    fn warning_no_handle_no_warning() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::I32,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: Some("documented".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::DeprecatedHandleType { .. })));
    }

    #[test]
    fn warning_large_enum_variant_count() {
        let variants: Vec<EnumVariant> = (0..101)
            .map(|i| EnumVariant {
                name: format!("V{i}"),
                value: i,
                doc: None,
            })
            .collect();
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "BigEnum".to_string(),
                    doc: None,
                    variants,
                }],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::LargeEnumVariantCount { enum_name, count }
                if enum_name == "BigEnum" && *count == 101
        )));
    }

    #[test]
    fn warning_enum_at_100_no_warning() {
        let variants: Vec<EnumVariant> = (0..100)
            .map(|i| EnumVariant {
                name: format!("V{i}"),
                value: i,
                doc: None,
            })
            .collect();
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![],
                enums: vec![EnumDef {
                    name: "BigEnum".to_string(),
                    doc: None,
                    variants,
                }],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::LargeEnumVariantCount { .. })));
    }

    #[test]
    fn warning_deep_nesting_in_param() {
        let deep = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
            Box::new(TypeRef::List(Box::new(TypeRef::I32))),
        )))));
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "nested_fn".to_string(),
                    params: vec![Param {
                        name: "data".to_string(),
                        ty: deep,
                    }],
                    returns: None,
                    doc: Some("documented".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::DeepNesting { location, depth }
                if location == "mymod::nested_fn::data" && *depth == 4
        )));
    }

    #[test]
    fn warning_nesting_at_3_no_warning() {
        let nested = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
            Box::new(TypeRef::I32),
        )))));
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "ok_fn".to_string(),
                    params: vec![Param {
                        name: "data".to_string(),
                        ty: nested,
                    }],
                    returns: None,
                    doc: Some("documented".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::DeepNesting { .. })));
    }

    #[test]
    fn warning_deep_nesting_in_struct_field() {
        let deep = TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
            Box::new(TypeRef::List(Box::new(TypeRef::I32))),
        )))));
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![StructDef {
                    name: "Widget".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "data".to_string(),
                        ty: deep,
                        doc: None,
                    }],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::DeepNesting { location, .. }
                if location == "mymod::Widget::data"
        )));
    }

    #[test]
    fn warning_empty_module_doc() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "undocumented".to_string(),
                functions: vec![
                    Function {
                        name: "a".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "b".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: false,
                    },
                ],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::EmptyModuleDoc { module } if module == "undocumented"
        )));
    }

    #[test]
    fn warning_partial_docs_no_warning() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "partial".to_string(),
                functions: vec![
                    Function {
                        name: "a".to_string(),
                        params: vec![],
                        returns: None,
                        doc: Some("has doc".to_string()),
                        r#async: false,
                    },
                    Function {
                        name: "b".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: false,
                    },
                ],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::EmptyModuleDoc { .. })));
    }

    #[test]
    fn warning_no_functions_no_empty_doc_warning() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "empty".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::EmptyModuleDoc { .. })));
    }

    #[test]
    fn warning_clean_api_no_warnings() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "clean".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::I32,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: Some("Adds numbers".to_string()),
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                errors: None,
            }],
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.is_empty());
    }

    #[test]
    fn resolve_enum_ref_in_struct_field() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![simple_function("ok_fn")],
                structs: vec![StructDef {
                    name: "Widget".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "color".to_string(),
                        ty: TypeRef::Struct("Color".to_string()),
                        doc: None,
                    }],
                }],
                enums: vec![simple_enum("Color")],
                errors: None,
            }],
        };
        validate_api(&mut api).unwrap();
        assert_eq!(
            api.modules[0].structs[0].fields[0].ty,
            TypeRef::Enum("Color".to_string())
        );
    }
}
