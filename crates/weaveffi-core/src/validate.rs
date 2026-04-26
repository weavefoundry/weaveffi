use std::collections::{BTreeMap, BTreeSet};
use weaveffi_ir::ir::{Api, CallbackDef, ErrorDomain, Function, Module, Param, TypeRef};

use crate::codegen::Capability;

#[derive(Debug, Clone)]
pub enum ValidationWarning {
    LargeEnumVariantCount {
        enum_name: String,
        count: usize,
    },
    DeepNesting {
        location: String,
        depth: usize,
    },
    EmptyModuleDoc {
        module: String,
    },
    AsyncVoidFunction {
        module: String,
        function: String,
    },
    MutableOnValueType {
        module: String,
        function: String,
        param: String,
    },
    DeprecatedFunction {
        module: String,
        function: String,
        message: String,
    },
}

impl std::fmt::Display for ValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LargeEnumVariantCount { enum_name, count } => {
                write!(f, "enum '{enum_name}' has {count} variants (>100)")
            }
            Self::DeepNesting { location, depth } => {
                write!(
                    f,
                    "deep type nesting at {location} (depth {depth}, max recommended 3)"
                )
            }
            Self::EmptyModuleDoc { module } => {
                write!(f, "module '{module}' has no doc comments on any function")
            }
            Self::AsyncVoidFunction { module, function } => {
                write!(
                    f,
                    "async function {module}::{function} has no return type; async void is unusual"
                )
            }
            Self::MutableOnValueType {
                module,
                function,
                param,
            } => {
                write!(
                    f,
                    "'mutable' on value-type parameter {module}::{function}::{param} has no effect; only meaningful for pointer/reference types (struct, string, bytes)"
                )
            }
            Self::DeprecatedFunction {
                module,
                function,
                message,
            } => {
                write!(f, "function {module}::{function} is deprecated: {message}")
            }
        }
    }
}

pub fn collect_warnings(api: &Api) -> Vec<ValidationWarning> {
    let mut warnings = Vec::new();
    for module in &api.modules {
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

        for f in &module.functions {
            if f.r#async && f.returns.is_none() {
                warnings.push(ValidationWarning::AsyncVoidFunction {
                    module: module.name.clone(),
                    function: f.name.clone(),
                });
            }
            for p in &f.params {
                if p.mutable && is_value_type(&p.ty) {
                    warnings.push(ValidationWarning::MutableOnValueType {
                        module: module.name.clone(),
                        function: f.name.clone(),
                        param: p.name.clone(),
                    });
                }
            }
        }

        for f in &module.functions {
            if let Some(msg) = &f.deprecated {
                warnings.push(ValidationWarning::DeprecatedFunction {
                    module: module.name.clone(),
                    function: f.name.clone(),
                    message: msg.clone(),
                });
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

fn is_value_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I32
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Enum(_)
            | TypeRef::Handle
    )
}

fn nesting_depth(ty: &TypeRef) -> usize {
    match ty {
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            1 + nesting_depth(inner)
        }
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
    #[error(
        "borrowed type '{ty}' is not valid in {location}; only function parameters are allowed"
    )]
    BorrowedTypeInInvalidPosition { ty: String, location: String },
    #[error("duplicate callback name in module '{module}': {name}")]
    DuplicateCallbackName { module: String, name: String },
    #[error(
        "listener '{listener}' in module '{module}' references undefined callback '{callback}'"
    )]
    ListenerCallbackNotFound {
        module: String,
        listener: String,
        callback: String,
    },
    #[error("duplicate listener name in module '{module}': {name}")]
    DuplicateListenerName { module: String, name: String },
    #[error("iterator type is only valid as a function return type, found in {location}")]
    IteratorInInvalidPosition { location: String },
    #[error("builder struct '{name}' in module '{module}' must have at least one field")]
    BuilderStructEmpty { module: String, name: String },
    #[error("target '{target}' does not support capability '{capability}' required by {location}")]
    TargetMissingCapability {
        target: String,
        capability: String,
        location: String,
    },
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
        validate_module(m, &api.modules)?;
    }
    resolve_type_refs(api);
    Ok(())
}

pub fn resolve_type_refs(api: &mut Api) {
    let mut global_types: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for module in &api.modules {
        for s in &module.structs {
            global_types
                .entry(s.name.clone())
                .or_insert((module.name.clone(), false));
        }
        for e in &module.enums {
            global_types
                .entry(e.name.clone())
                .or_insert((module.name.clone(), true));
        }
    }

    for module in &mut api.modules {
        let local_enum_names: BTreeSet<String> =
            module.enums.iter().map(|e| e.name.clone()).collect();
        let local_struct_names: BTreeSet<String> =
            module.structs.iter().map(|s| s.name.clone()).collect();
        let module_name = module.name.clone();
        for f in &mut module.functions {
            for p in &mut f.params {
                resolve_single_type_ref(
                    &mut p.ty,
                    &local_enum_names,
                    &local_struct_names,
                    &module_name,
                    &global_types,
                );
            }
            if let Some(ret) = &mut f.returns {
                resolve_single_type_ref(
                    ret,
                    &local_enum_names,
                    &local_struct_names,
                    &module_name,
                    &global_types,
                );
            }
        }
        for s in &mut module.structs {
            for field in &mut s.fields {
                resolve_single_type_ref(
                    &mut field.ty,
                    &local_enum_names,
                    &local_struct_names,
                    &module_name,
                    &global_types,
                );
            }
        }
    }
}

fn resolve_single_type_ref(
    ty: &mut TypeRef,
    local_enum_names: &BTreeSet<String>,
    local_struct_names: &BTreeSet<String>,
    current_module: &str,
    global_types: &BTreeMap<String, (String, bool)>,
) {
    match ty {
        TypeRef::Struct(name) if local_enum_names.contains(name.as_str()) => {
            let name = std::mem::take(name);
            *ty = TypeRef::Enum(name);
        }
        TypeRef::Struct(name) if !local_struct_names.contains(name.as_str()) => {
            if let Some((mod_name, is_enum)) = global_types.get(name.as_str()) {
                if mod_name != current_module {
                    let qualified = format!("{mod_name}.{name}");
                    if *is_enum {
                        *ty = TypeRef::Enum(qualified);
                    } else {
                        *name = qualified;
                    }
                }
            }
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            resolve_single_type_ref(
                inner,
                local_enum_names,
                local_struct_names,
                current_module,
                global_types,
            );
        }
        TypeRef::Map(k, v) => {
            resolve_single_type_ref(
                k,
                local_enum_names,
                local_struct_names,
                current_module,
                global_types,
            );
            resolve_single_type_ref(
                v,
                local_enum_names,
                local_struct_names,
                current_module,
                global_types,
            );
        }
        _ => {}
    }
}

pub fn find_type_in_api(api: &Api, name: &str) -> Option<(String, bool)> {
    for module in &api.modules {
        if module.structs.iter().any(|s| s.name == name) {
            return Some((module.name.clone(), false));
        }
        if module.enums.iter().any(|e| e.name == name) {
            return Some((module.name.clone(), true));
        }
    }
    None
}

fn validate_module(module: &Module, all_modules: &[Module]) -> Result<(), ValidationError> {
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
            if s.builder {
                return Err(ValidationError::BuilderStructEmpty {
                    module: module.name.clone(),
                    name: s.name.clone(),
                });
            }
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
            if let Some(ty) = contains_borrowed(&f.ty) {
                return Err(ValidationError::BorrowedTypeInInvalidPosition {
                    ty: ty.to_string(),
                    location: format!("field '{}' of struct '{}'", f.name, s.name),
                });
            }
            if contains_iterator(&f.ty) {
                return Err(ValidationError::IteratorInInvalidPosition {
                    location: format!("field '{}' of struct '{}'", f.name, s.name),
                });
            }
            validate_type_ref(&f.ty, &known_types, all_modules, &module.name)?;
        }
    }
    for f in &module.functions {
        for p in &f.params {
            if contains_iterator(&p.ty) {
                return Err(ValidationError::IteratorInInvalidPosition {
                    location: format!(
                        "param '{}' of function '{}::{}'",
                        p.name, module.name, f.name
                    ),
                });
            }
            validate_type_ref(&p.ty, &known_types, all_modules, &module.name)?;
        }
        if let Some(ret) = &f.returns {
            if let Some(ty) = contains_borrowed(ret) {
                return Err(ValidationError::BorrowedTypeInInvalidPosition {
                    ty: ty.to_string(),
                    location: format!("return type of {}::{}", module.name, f.name),
                });
            }
            validate_type_ref(ret, &known_types, all_modules, &module.name)?;
        }
    }

    let mut callback_names = BTreeSet::new();
    for cb in &module.callbacks {
        check_identifier(&cb.name)?;
        if !callback_names.insert(cb.name.clone()) {
            return Err(ValidationError::DuplicateCallbackName {
                module: module.name.clone(),
                name: cb.name.clone(),
            });
        }
        for p in &cb.params {
            validate_param(p)?;
        }
    }

    let mut listener_names = BTreeSet::new();
    for l in &module.listeners {
        check_identifier(&l.name)?;
        if !listener_names.insert(l.name.clone()) {
            return Err(ValidationError::DuplicateListenerName {
                module: module.name.clone(),
                name: l.name.clone(),
            });
        }
        if !callback_names.contains(&l.event_callback) {
            return Err(ValidationError::ListenerCallbackNotFound {
                module: module.name.clone(),
                listener: l.name.clone(),
                callback: l.event_callback.clone(),
            });
        }
    }

    if let Some(errors) = &module.errors {
        validate_error_domain(module, errors, &function_names)?;
    }

    let mut sub_module_names = BTreeSet::new();
    for sub in &module.modules {
        if !sub_module_names.insert(sub.name.clone()) {
            return Err(ValidationError::DuplicateModuleName(sub.name.clone()));
        }
        validate_module(sub, all_modules)?;
    }

    Ok(())
}

fn validate_function(module: &Module, f: &Function) -> Result<(), ValidationError> {
    check_identifier(&f.name)?;

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

fn contains_borrowed(ty: &TypeRef) -> Option<&'static str> {
    match ty {
        TypeRef::BorrowedStr => Some("&str"),
        TypeRef::BorrowedBytes => Some("&[u8]"),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            contains_borrowed(inner)
        }
        TypeRef::Map(k, v) => contains_borrowed(k).or_else(|| contains_borrowed(v)),
        _ => None,
    }
}

fn contains_iterator(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::Iterator(_) => true,
        TypeRef::Optional(inner) | TypeRef::List(inner) => contains_iterator(inner),
        TypeRef::Map(k, v) => contains_iterator(k) || contains_iterator(v),
        _ => false,
    }
}

fn validate_type_ref(
    ty: &TypeRef,
    known: &BTreeSet<&str>,
    all_modules: &[Module],
    current_module: &str,
) -> Result<(), ValidationError> {
    validate_type_ref_inner(ty, known, all_modules, current_module, &mut BTreeSet::new())
}

fn validate_type_ref_inner(
    ty: &TypeRef,
    known: &BTreeSet<&str>,
    all_modules: &[Module],
    current_module: &str,
    visited_callbacks: &mut BTreeSet<String>,
) -> Result<(), ValidationError> {
    match ty {
        TypeRef::Struct(name) | TypeRef::Enum(name) | TypeRef::TypedHandle(name) => {
            if !known.contains(name.as_str()) {
                let found_elsewhere = all_modules.iter().any(|m| {
                    m.name != current_module
                        && (m.structs.iter().any(|s| s.name == *name)
                            || m.enums.iter().any(|e| e.name == *name))
                });
                if !found_elsewhere {
                    return Err(ValidationError::UnknownTypeRef { name: name.clone() });
                }
            }
            Ok(())
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            validate_type_ref_inner(inner, known, all_modules, current_module, visited_callbacks)
        }
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
            validate_type_ref_inner(k, known, all_modules, current_module, visited_callbacks)?;
            validate_type_ref_inner(v, known, all_modules, current_module, visited_callbacks)
        }
        TypeRef::Callback(name) => {
            let Some((owner_module, cb)) = find_callback_def(name, all_modules, current_module)
            else {
                return Err(ValidationError::UnknownTypeRef { name: name.clone() });
            };
            let canonical = format!("{}.{}", owner_module.name, cb.name);
            if !visited_callbacks.insert(canonical) {
                return Ok(());
            }
            let owner_known: BTreeSet<&str> = owner_module
                .structs
                .iter()
                .map(|s| s.name.as_str())
                .chain(owner_module.enums.iter().map(|e| e.name.as_str()))
                .collect();
            for p in &cb.params {
                validate_type_ref_inner(
                    &p.ty,
                    &owner_known,
                    all_modules,
                    &owner_module.name,
                    visited_callbacks,
                )?;
            }
            if let Some(ret) = &cb.returns {
                validate_type_ref_inner(
                    ret,
                    &owner_known,
                    all_modules,
                    &owner_module.name,
                    visited_callbacks,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn find_callback_def<'a>(
    name: &str,
    all_modules: &'a [Module],
    current_module: &str,
) -> Option<(&'a Module, &'a CallbackDef)> {
    if let Some(dot) = name.find('.') {
        let mod_name = &name[..dot];
        let cb_name = &name[dot + 1..];
        let module = all_modules.iter().find(|m| m.name == mod_name)?;
        let cb = module.callbacks.iter().find(|c| c.name == cb_name)?;
        return Some((module, cb));
    }
    if let Some(m) = all_modules.iter().find(|m| m.name == current_module) {
        if let Some(cb) = m.callbacks.iter().find(|c| c.name == name) {
            return Some((m, cb));
        }
    }
    for m in all_modules {
        if m.name == current_module {
            continue;
        }
        if let Some(cb) = m.callbacks.iter().find(|c| c.name == name) {
            return Some((m, cb));
        }
    }
    None
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

fn capability_name(cap: Capability) -> &'static str {
    match cap {
        Capability::Callbacks => "callbacks",
        Capability::Listeners => "listeners",
        Capability::Iterators => "iterators",
        Capability::Builders => "builders",
        Capability::AsyncFunctions => "async_functions",
        Capability::CancellableAsync => "cancellable_async",
        Capability::TypedHandles => "typed_handles",
        Capability::BorrowedTypes => "borrowed_types",
        Capability::MapTypes => "map_types",
        Capability::NestedModules => "nested_modules",
        Capability::CrossModuleTypes => "cross_module_types",
        Capability::ErrorDomains => "error_domains",
        Capability::DeprecatedAnnotations => "deprecated_annotations",
    }
}

fn require_capability(
    generator_caps: &[(&str, &[Capability])],
    cap: Capability,
    location: &str,
) -> Result<(), ValidationError> {
    for (name, caps) in generator_caps {
        if !caps.contains(&cap) {
            return Err(ValidationError::TargetMissingCapability {
                target: (*name).to_string(),
                capability: capability_name(cap).to_string(),
                location: location.to_string(),
            });
        }
    }
    Ok(())
}

fn check_type_ref_capabilities(
    ty: &TypeRef,
    location: &str,
    generator_caps: &[(&str, &[Capability])],
) -> Result<(), ValidationError> {
    match ty {
        TypeRef::Iterator(inner) => {
            require_capability(generator_caps, Capability::Iterators, location)?;
            check_type_ref_capabilities(inner, location, generator_caps)?;
        }
        TypeRef::TypedHandle(_) => {
            require_capability(generator_caps, Capability::TypedHandles, location)?;
        }
        TypeRef::BorrowedStr | TypeRef::BorrowedBytes => {
            require_capability(generator_caps, Capability::BorrowedTypes, location)?;
        }
        TypeRef::Map(k, v) => {
            require_capability(generator_caps, Capability::MapTypes, location)?;
            check_type_ref_capabilities(k, location, generator_caps)?;
            check_type_ref_capabilities(v, location, generator_caps)?;
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) => {
            check_type_ref_capabilities(inner, location, generator_caps)?;
        }
        TypeRef::Struct(name) | TypeRef::Enum(name) => {
            if name.contains('.') {
                require_capability(generator_caps, Capability::CrossModuleTypes, location)?;
            }
        }
        TypeRef::Callback(_) => {
            require_capability(generator_caps, Capability::Callbacks, location)?;
        }
        _ => {}
    }
    Ok(())
}

fn check_module_capabilities(
    module: &Module,
    generator_caps: &[(&str, &[Capability])],
) -> Result<(), ValidationError> {
    if !module.callbacks.is_empty() {
        require_capability(
            generator_caps,
            Capability::Callbacks,
            &format!("module '{}' callbacks", module.name),
        )?;
        for cb in &module.callbacks {
            for p in &cb.params {
                check_type_ref_capabilities(
                    &p.ty,
                    &format!(
                        "param '{}' of callback '{}::{}'",
                        p.name, module.name, cb.name
                    ),
                    generator_caps,
                )?;
            }
        }
    }

    if !module.listeners.is_empty() {
        require_capability(
            generator_caps,
            Capability::Listeners,
            &format!("module '{}' listeners", module.name),
        )?;
    }

    if module.errors.is_some() {
        require_capability(
            generator_caps,
            Capability::ErrorDomains,
            &format!("module '{}' error domain", module.name),
        )?;
    }

    if !module.modules.is_empty() {
        require_capability(
            generator_caps,
            Capability::NestedModules,
            &format!("module '{}' nested modules", module.name),
        )?;
    }

    for f in &module.functions {
        if f.r#async {
            require_capability(
                generator_caps,
                Capability::AsyncFunctions,
                &format!("async function '{}::{}'", module.name, f.name),
            )?;
            if f.cancellable {
                require_capability(
                    generator_caps,
                    Capability::CancellableAsync,
                    &format!("cancellable async function '{}::{}'", module.name, f.name),
                )?;
            }
        }
        if f.deprecated.is_some() {
            require_capability(
                generator_caps,
                Capability::DeprecatedAnnotations,
                &format!("deprecated function '{}::{}'", module.name, f.name),
            )?;
        }
        for p in &f.params {
            check_type_ref_capabilities(
                &p.ty,
                &format!(
                    "param '{}' of function '{}::{}'",
                    p.name, module.name, f.name
                ),
                generator_caps,
            )?;
        }
        if let Some(ret) = &f.returns {
            check_type_ref_capabilities(
                ret,
                &format!("return of function '{}::{}'", module.name, f.name),
                generator_caps,
            )?;
        }
    }

    for s in &module.structs {
        if s.builder {
            require_capability(
                generator_caps,
                Capability::Builders,
                &format!("builder struct '{}::{}'", module.name, s.name),
            )?;
        }
        for fld in &s.fields {
            check_type_ref_capabilities(
                &fld.ty,
                &format!(
                    "field '{}' of struct '{}::{}'",
                    fld.name, module.name, s.name
                ),
                generator_caps,
            )?;
        }
    }

    for sub in &module.modules {
        check_module_capabilities(sub, generator_caps)?;
    }

    Ok(())
}

/// Validate that every IR feature used by `api` is supported by all selected generators.
///
/// `generator_caps` should contain an entry per user-selected generator, pairing the
/// generator's name with its declared capability set. Call this after [`validate_api`]
/// so type references are resolved (cross-module references are detected via the
/// qualified `module.Name` form produced by `resolve_type_refs`).
pub fn validate_capabilities(
    api: &Api,
    generator_caps: &[(&str, &[Capability])],
) -> Result<(), ValidationError> {
    for module in &api.modules {
        check_module_capabilities(module, generator_caps)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{
        Api, CallbackDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, ListenerDef,
        Module, Param, StructDef, StructField, TypeRef,
    };

    fn simple_function(name: &str) -> Function {
        Function {
            name: name.to_string(),
            params: vec![Param {
                name: "x".to_string(),
                ty: TypeRef::I32,
                mutable: false,
            }],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    fn simple_module(name: &str) -> Module {
        Module {
            name: name.to_string(),
            functions: vec![simple_function("do_stuff")],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn simple_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![simple_module("mymod")],
            generators: None,
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
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                }],
                generators: None,
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
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                }],
                generators: None,
            };
            assert!(
                validate_api(&mut api).is_err(),
                "Expected invalid identifier '{bad}' to be rejected"
            );
        }
    }

    #[test]
    fn async_function_passes_validation() {
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn async_function_with_return_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "fetch_data".to_string(),
                    params: vec![Param {
                        name: "url".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn async_void_function_emits_warning() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "fire_and_forget".to_string(),
                    params: vec![],
                    returns: None,
                    doc: Some("documented".to_string()),
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::AsyncVoidFunction { module, function }
                if module == "mymod" && function == "fire_and_forget"
        )));
    }

    #[test]
    fn async_function_with_return_no_void_warning() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "fetch".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::StringUtf8),
                    doc: Some("documented".to_string()),
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::AsyncVoidFunction { .. })));
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                                mutable: false,
                            },
                            Param {
                                name: "email".to_string(),
                                ty: TypeRef::StringUtf8,
                                mutable: false,
                            },
                        ],
                        returns: Some(TypeRef::Handle),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "get_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                            mutable: false,
                        }],
                        returns: Some(TypeRef::StringUtf8),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
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
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: Some(ErrorDomain {
                    name: "MyErrors".to_string(),
                    codes: vec![ErrorCode {
                        name: "success".to_string(),
                        code: 0,
                        message: "should fail".to_string(),
                    }],
                }),
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: Some(ErrorDomain {
                    name: "do_stuff".to_string(),
                    codes: vec![ErrorCode {
                        name: "fail".to_string(),
                        code: 1,
                        message: "failed".to_string(),
                    }],
                }),
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
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
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
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
                modules: vec![],
            }],
            generators: None,
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
                default: None,
            }],
            builder: false,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                            default: None,
                        },
                        StructField {
                            name: "x".to_string(),
                            ty: TypeRef::F64,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![simple_struct("Point")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![StructDef {
                    name: "Contact".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![simple_struct("Contact")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![simple_struct("Point")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: Some("documented".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: None,
                    doc: Some("documented".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "b".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "b".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        mutable: false,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: Some("Adds numbers".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![simple_enum("Color")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
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
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![simple_enum("Color")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        validate_api(&mut api).unwrap();
        assert_eq!(
            api.modules[0].structs[0].fields[0].ty,
            TypeRef::Enum("Color".to_string())
        );
    }

    #[test]
    fn typed_handle_valid_struct_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "get_session".to_string(),
                    params: vec![Param {
                        name: "h".to_string(),
                        ty: TypeRef::TypedHandle("Session".to_string()),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![simple_struct("Session")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn typed_handle_unknown_struct_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "mymod".to_string(),
                functions: vec![Function {
                    name: "get_session".to_string(),
                    params: vec![Param {
                        name: "h".to_string(),
                        ty: TypeRef::TypedHandle("Nonexistent".to_string()),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::UnknownTypeRef { name } if name == "Nonexistent"
        ));
    }

    #[test]
    fn borrowed_str_param_accepted() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "write".to_string(),
                    params: vec![Param {
                        name: "data".to_string(),
                        ty: TypeRef::BorrowedStr,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn borrowed_bytes_param_accepted() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "upload".to_string(),
                    params: vec![Param {
                        name: "raw".to_string(),
                        ty: TypeRef::BorrowedBytes,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn borrowed_str_in_return_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "read".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::BorrowedStr),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::BorrowedTypeInInvalidPosition { ty, location }
                if ty == "&str" && location.contains("return type")
        ));
    }

    #[test]
    fn borrowed_bytes_in_return_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "read_raw".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::BorrowedBytes),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::BorrowedTypeInInvalidPosition { ty, location }
                if ty == "&[u8]" && location.contains("return type")
        ));
    }

    #[test]
    fn borrowed_str_in_struct_field_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "data".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Msg".to_string(),
                    fields: vec![StructField {
                        name: "text".to_string(),
                        ty: TypeRef::BorrowedStr,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                    doc: None,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::BorrowedTypeInInvalidPosition { ty, location }
                if ty == "&str" && location.contains("struct")
        ));
    }

    #[test]
    fn borrowed_bytes_in_struct_field_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "data".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Blob".to_string(),
                    fields: vec![StructField {
                        name: "content".to_string(),
                        ty: TypeRef::BorrowedBytes,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                    doc: None,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::BorrowedTypeInInvalidPosition { ty, location }
                if ty == "&[u8]" && location.contains("struct")
        ));
    }

    #[test]
    fn borrowed_str_nested_in_optional_return_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "maybe_read".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::BorrowedStr))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::BorrowedTypeInInvalidPosition { ty, .. }
                if ty == "&str"
        ));
    }

    #[test]
    fn cross_module_struct_ref_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![
                Module {
                    name: "orders".to_string(),
                    functions: vec![Function {
                        name: "place_order".to_string(),
                        params: vec![Param {
                            name: "item".to_string(),
                            ty: TypeRef::Struct("Product".to_string()),
                            mutable: false,
                        }],
                        returns: None,
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
                Module {
                    name: "catalog".to_string(),
                    functions: vec![simple_function("list_products")],
                    structs: vec![simple_struct("Product")],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
            ],
            generators: None,
        };
        validate_api(&mut api).unwrap();
        assert_eq!(
            api.modules[0].functions[0].params[0].ty,
            TypeRef::Struct("catalog.Product".to_string())
        );
    }

    #[test]
    fn cross_module_enum_ref_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![
                Module {
                    name: "orders".to_string(),
                    functions: vec![Function {
                        name: "get_status".to_string(),
                        params: vec![],
                        returns: Some(TypeRef::Struct("Status".to_string())),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
                Module {
                    name: "shared".to_string(),
                    functions: vec![simple_function("noop")],
                    structs: vec![],
                    enums: vec![simple_enum("Status")],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
            ],
            generators: None,
        };
        validate_api(&mut api).unwrap();
        assert_eq!(
            api.modules[0].functions[0].returns,
            Some(TypeRef::Enum("shared.Status".to_string()))
        );
    }

    #[test]
    fn cross_module_unknown_still_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![
                Module {
                    name: "orders".to_string(),
                    functions: vec![Function {
                        name: "do_stuff".to_string(),
                        params: vec![Param {
                            name: "x".to_string(),
                            ty: TypeRef::Struct("Nonexistent".to_string()),
                            mutable: false,
                        }],
                        returns: None,
                        doc: None,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    }],
                    structs: vec![],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
                Module {
                    name: "catalog".to_string(),
                    functions: vec![simple_function("list_products")],
                    structs: vec![simple_struct("Product")],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                },
            ],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::UnknownTypeRef { name } if name == "Nonexistent"
        ));
    }

    #[test]
    fn find_type_in_api_finds_struct() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "catalog".to_string(),
                functions: vec![],
                structs: vec![simple_struct("Product")],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let result = find_type_in_api(&api, "Product");
        assert_eq!(result, Some(("catalog".to_string(), false)));
    }

    #[test]
    fn find_type_in_api_finds_enum() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "shared".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![simple_enum("Status")],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let result = find_type_in_api(&api, "Status");
        assert_eq!(result, Some(("shared".to_string(), true)));
    }

    #[test]
    fn find_type_in_api_returns_none_for_unknown() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![simple_module("mymod")],
            generators: None,
        };
        assert_eq!(find_type_in_api(&api, "Nonexistent"), None);
    }

    #[test]
    fn validate_nested_module_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "parent".to_string(),
                functions: vec![simple_function("top_fn")],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![Module {
                    name: "child".to_string(),
                    functions: vec![simple_function("inner_fn")],
                    structs: vec![],
                    enums: vec![],
                    callbacks: vec![],
                    listeners: vec![],
                    errors: None,
                    modules: vec![],
                }],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn duplicate_callback_names_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![
                    CallbackDef {
                        name: "on_data".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                    },
                    CallbackDef {
                        name: "on_data".to_string(),
                        params: vec![],
                        returns: None,
                        doc: None,
                    },
                ],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateCallbackName { module, name }
                if module == "events" && name == "on_data"
        ));
    }

    #[test]
    fn listener_referencing_undefined_callback_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![ListenerDef {
                    name: "watcher".to_string(),
                    event_callback: "nonexistent".to_string(),
                    doc: None,
                }],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::ListenerCallbackNotFound { module, listener, callback }
                if module == "events" && listener == "watcher" && callback == "nonexistent"
        ));
    }

    #[test]
    fn listener_referencing_defined_callback_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "on_data".to_string(),
                    params: vec![Param {
                        name: "payload".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![ListenerDef {
                    name: "data_stream".to_string(),
                    event_callback: "on_data".to_string(),
                    doc: Some("Subscribe to data".to_string()),
                }],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn duplicate_listener_names_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "on_data".to_string(),
                    params: vec![],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![
                    ListenerDef {
                        name: "watcher".to_string(),
                        event_callback: "on_data".to_string(),
                        doc: None,
                    },
                    ListenerDef {
                        name: "watcher".to_string(),
                        event_callback: "on_data".to_string(),
                        doc: None,
                    },
                ],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::DuplicateListenerName { module, name }
                if module == "events" && name == "watcher"
        ));
    }

    #[test]
    fn iterator_valid_as_return_type() {
        let mut api = Api {
            version: "0.2.0".to_string(),
            modules: vec![Module {
                name: "data".to_string(),
                functions: vec![Function {
                    name: "list_items".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn iterator_rejected_as_param() {
        let mut api = Api {
            version: "0.2.0".to_string(),
            modules: vec![Module {
                name: "data".to_string(),
                functions: vec![Function {
                    name: "consume".to_string(),
                    params: vec![Param {
                        name: "items".to_string(),
                        ty: TypeRef::Iterator(Box::new(TypeRef::I32)),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::IteratorInInvalidPosition { .. }
        ));
    }

    #[test]
    fn iterator_rejected_in_struct_field() {
        let mut api = Api {
            version: "0.2.0".to_string(),
            modules: vec![Module {
                name: "data".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Container".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "items".to_string(),
                        ty: TypeRef::Iterator(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::IteratorInInvalidPosition { .. }
        ));
    }

    #[test]
    fn builder_struct_empty_is_error() {
        let mut api = Api {
            version: "0.2.0".to_string(),
            modules: vec![Module {
                name: "m".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Empty".into(),
                    doc: None,
                    fields: vec![],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let err = validate_api(&mut api).unwrap_err();
        assert!(
            matches!(err, ValidationError::BuilderStructEmpty { .. }),
            "expected BuilderStructEmpty, got: {err}"
        );
    }

    #[test]
    fn warning_mutable_on_value_type() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::I32,
                        mutable: true,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: Some("add".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::MutableOnValueType {
                param,
                ..
            } if param == "x"
        )));
    }

    #[test]
    fn no_warning_mutable_on_pointer_type() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "io".to_string(),
                functions: vec![Function {
                    name: "fill".to_string(),
                    params: vec![
                        Param {
                            name: "buf".to_string(),
                            ty: TypeRef::Bytes,
                            mutable: true,
                        },
                        Param {
                            name: "msg".to_string(),
                            ty: TypeRef::StringUtf8,
                            mutable: true,
                        },
                        Param {
                            name: "obj".to_string(),
                            ty: TypeRef::Struct("Thing".into()),
                            mutable: true,
                        },
                    ],
                    returns: None,
                    doc: Some("fill".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(
            !warnings
                .iter()
                .any(|w| matches!(w, ValidationWarning::MutableOnValueType { .. })),
            "pointer types should not trigger mutable warning"
        );
    }

    #[test]
    fn no_warning_mutable_false_on_value_type() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![Param {
                        name: "x".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: Some("add".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(
            !warnings
                .iter()
                .any(|w| matches!(w, ValidationWarning::MutableOnValueType { .. })),
            "mutable=false should not trigger warning"
        );
    }

    #[test]
    fn warning_mutable_on_enum_type() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "paint".to_string(),
                functions: vec![Function {
                    name: "set_color".to_string(),
                    params: vec![Param {
                        name: "color".to_string(),
                        ty: TypeRef::Enum("Color".into()),
                        mutable: true,
                    }],
                    returns: None,
                    doc: Some("set".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::MutableOnValueType { param, .. } if param == "color"
        )));
    }

    #[test]
    fn warning_deprecated_function() {
        let api = Api {
            version: "0.2.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add_old".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: Some("old add".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: Some("Use add_v2 instead".to_string()),
                    since: Some("0.1.0".to_string()),
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(warnings.iter().any(|w| matches!(
            w,
            ValidationWarning::DeprecatedFunction { function, message, .. }
                if function == "add_old" && message == "Use add_v2 instead"
        )));
    }

    #[test]
    fn no_warning_for_non_deprecated_function() {
        let api = Api {
            version: "0.2.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: Some("add things".to_string()),
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let warnings = collect_warnings(&api);
        assert!(!warnings
            .iter()
            .any(|w| matches!(w, ValidationWarning::DeprecatedFunction { .. })));
    }

    const NODE_CAPS: &[Capability] = &[
        Capability::Iterators,
        Capability::Builders,
        Capability::AsyncFunctions,
        Capability::TypedHandles,
        Capability::BorrowedTypes,
        Capability::MapTypes,
        Capability::NestedModules,
        Capability::CrossModuleTypes,
        Capability::ErrorDomains,
        Capability::DeprecatedAnnotations,
    ];

    const GO_CAPS: &[Capability] = &[
        Capability::CancellableAsync,
        Capability::TypedHandles,
        Capability::BorrowedTypes,
        Capability::MapTypes,
        Capability::NestedModules,
        Capability::CrossModuleTypes,
        Capability::ErrorDomains,
        Capability::DeprecatedAnnotations,
    ];

    const RUBY_CAPS: &[Capability] = &[
        Capability::Builders,
        Capability::CancellableAsync,
        Capability::TypedHandles,
        Capability::BorrowedTypes,
        Capability::MapTypes,
        Capability::NestedModules,
        Capability::CrossModuleTypes,
        Capability::ErrorDomains,
        Capability::DeprecatedAnnotations,
    ];

    #[test]
    fn callback_in_api_with_node_target_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![simple_function("subscribe")],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "on_data".to_string(),
                    params: vec![Param {
                        name: "payload".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        validate_api(&mut api).unwrap();

        let caps: &[(&str, &[Capability])] = &[("node", NODE_CAPS)];
        let err = validate_capabilities(&api, caps).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::TargetMissingCapability { target, capability, .. }
                if target == "node" && capability == "callbacks"
        ));
    }

    #[test]
    fn iterator_in_api_with_go_target_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "items".to_string(),
                functions: vec![Function {
                    name: "stream".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        validate_api(&mut api).unwrap();

        let caps: &[(&str, &[Capability])] = &[("go", GO_CAPS)];
        let err = validate_capabilities(&api, caps).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::TargetMissingCapability { target, capability, .. }
                if target == "go" && capability == "iterators"
        ));
    }

    #[test]
    fn async_in_api_with_ruby_target_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "net".to_string(),
                functions: vec![Function {
                    name: "fetch".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        validate_api(&mut api).unwrap();

        let caps: &[(&str, &[Capability])] = &[("ruby", RUBY_CAPS)];
        let err = validate_capabilities(&api, caps).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::TargetMissingCapability { target, capability, .. }
                if target == "ruby" && capability == "async_functions"
        ));
    }

    #[test]
    fn callback_ref_valid_passes() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![Function {
                    name: "register".to_string(),
                    params: vec![Param {
                        name: "cb".to_string(),
                        ty: TypeRef::Callback("OnMessage".to_string()),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![CallbackDef {
                    name: "OnMessage".to_string(),
                    params: vec![Param {
                        name: "message".to_string(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                }],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(validate_api(&mut api).is_ok());
    }

    #[test]
    fn callback_ref_unknown_rejected() {
        let mut api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "events".to_string(),
                functions: vec![Function {
                    name: "register".to_string(),
                    params: vec![Param {
                        name: "cb".to_string(),
                        ty: TypeRef::Callback("Missing".to_string()),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        assert!(matches!(
            validate_api(&mut api).unwrap_err(),
            ValidationError::UnknownTypeRef { name } if name == "Missing"
        ));
    }
}
