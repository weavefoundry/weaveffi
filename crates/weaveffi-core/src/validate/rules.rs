//! The validation rules: per-module name/uniqueness checks, type-reference
//! existence, ABI-representability of element shapes, callback parameter
//! marshalability, and error-domain consistency.

use super::ValidationError;
use std::collections::{BTreeMap, BTreeSet};
use weaveffi_ir::ir::{ErrorDomain, Function, Module, Param, TypeRef};

/// The marshalable kernel for callback parameters. Callback arguments cross
/// the boundary *into* the foreign language without an FFI round-trip, so
/// every target must be able to deep-copy them in a C trampoline: scalars,
/// bool, enums, string, bytes, handles, structs (borrowed), optionals of
/// those, lists of scalars/strings, and maps of scalars/strings.
fn callback_param_type_supported(ty: &TypeRef) -> bool {
    fn leaf(ty: &TypeRef) -> bool {
        matches!(
            ty,
            TypeRef::I32
                | TypeRef::U32
                | TypeRef::I64
                | TypeRef::F64
                | TypeRef::Bool
                | TypeRef::Enum(_)
                | TypeRef::StringUtf8
                | TypeRef::BorrowedStr
                | TypeRef::Handle
                | TypeRef::TypedHandle(_)
                | TypeRef::Struct(_)
                | TypeRef::Bytes
                | TypeRef::BorrowedBytes
        )
    }
    fn elem(ty: &TypeRef) -> bool {
        matches!(
            ty,
            TypeRef::I32
                | TypeRef::U32
                | TypeRef::I64
                | TypeRef::F64
                | TypeRef::Bool
                | TypeRef::Enum(_)
                | TypeRef::StringUtf8
                | TypeRef::BorrowedStr
        )
    }
    match ty {
        t if leaf(t) => true,
        TypeRef::Optional(inner) => leaf(inner),
        TypeRef::List(inner) => elem(inner),
        TypeRef::Map(k, v) => elem(k) && elem(v),
        _ => false,
    }
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

pub(super) fn validate_module(
    module: &Module,
    all_modules: &[Module],
) -> Result<(), ValidationError> {
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
            validate_type_ref(&f.ty, &known_types, all_modules)?;
            check_element_shapes(&f.ty, || {
                format!("field '{}' of struct '{}'", f.name, s.name)
            })?;
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
            validate_type_ref(&p.ty, &known_types, all_modules)?;
            check_element_shapes(&p.ty, || {
                format!(
                    "param '{}' of function '{}::{}'",
                    p.name, module.name, f.name
                )
            })?;
        }
        if let Some(ret) = &f.returns {
            if let Some(ty) = contains_borrowed(ret) {
                return Err(ValidationError::BorrowedTypeInInvalidPosition {
                    ty: ty.to_string(),
                    location: format!("return type of {}::{}", module.name, f.name),
                });
            }
            // An async function completes through a one-shot callback; an
            // iterator needs a pull-based handle. The two shapes cannot
            // compose on the C ABI, so reject the combination up front
            // instead of letting backends lower it inconsistently.
            if f.r#async && contains_iterator(ret) {
                return Err(ValidationError::AsyncIteratorReturn {
                    module: module.name.clone(),
                    function: f.name.clone(),
                });
            }
            validate_type_ref(ret, &known_types, all_modules)?;
            check_element_shapes(ret, || {
                format!("return type of {}::{}", module.name, f.name)
            })?;
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
            if !callback_param_type_supported(&p.ty) {
                return Err(ValidationError::UnsupportedCallbackParamType {
                    module: module.name.clone(),
                    callback: cb.name.clone(),
                    param: p.name.clone(),
                    ty: format!("{:?}", p.ty),
                });
            }
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

/// The element shapes the C ABI can faithfully represent. Lists, maps, and
/// iterators lower to flat arrays (`T* + len`, parallel key/value arrays, or
/// a one-slot `next` out-param), so their element types must themselves be
/// single C slots. Composite elements (lists of lists, optional scalars in
/// lists, bytes elements needing a second length slot, ...) silently flatten
/// in `element_ctype` and would generate wrong code in every backend; reject
/// them up front. Returns the first offending element type, if any.
fn unsupported_element_shape(ty: &TypeRef) -> Option<&TypeRef> {
    fn slot_leaf(ty: &TypeRef) -> bool {
        matches!(
            ty,
            TypeRef::I32
                | TypeRef::U32
                | TypeRef::I64
                | TypeRef::F64
                | TypeRef::Bool
                | TypeRef::Enum(_)
                | TypeRef::StringUtf8
                | TypeRef::BorrowedStr
                | TypeRef::Handle
                | TypeRef::TypedHandle(_)
                | TypeRef::Struct(_)
        )
    }
    fn scalar_elem(ty: &TypeRef) -> bool {
        matches!(
            ty,
            TypeRef::I32
                | TypeRef::U32
                | TypeRef::I64
                | TypeRef::F64
                | TypeRef::Bool
                | TypeRef::Enum(_)
                | TypeRef::StringUtf8
                | TypeRef::BorrowedStr
        )
    }
    match ty {
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            t if slot_leaf(t) => None,
            // NULL entries in a pointer array express "none", so optional
            // structs/handles stay representable inside lists.
            TypeRef::Optional(o)
                if matches!(o.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) =>
            {
                None
            }
            other => Some(other),
        },
        TypeRef::Map(k, v) => {
            if !scalar_elem(k) {
                Some(k)
            } else if !scalar_elem(v) {
                Some(v)
            } else {
                None
            }
        }
        TypeRef::Optional(inner) => unsupported_element_shape(inner),
        _ => None,
    }
}

fn check_element_shapes(
    ty: &TypeRef,
    location: impl Fn() -> String,
) -> Result<(), ValidationError> {
    if let Some(bad) = unsupported_element_shape(ty) {
        return Err(ValidationError::UnsupportedElementType {
            location: location(),
            ty: format!("{bad:?}"),
        });
    }
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

/// Does a struct/enum named `name` exist anywhere in the module tree,
/// including nested submodules? Validation runs before reference qualification,
/// so an unqualified reference is valid if its bare name is defined anywhere;
/// the resolver later rewrites it to the owning module's full path.
fn type_exists(modules: &[Module], name: &str) -> bool {
    modules.iter().any(|m| {
        m.structs.iter().any(|s| s.name == name)
            || m.enums.iter().any(|e| e.name == name)
            || type_exists(&m.modules, name)
    })
}

fn validate_type_ref(
    ty: &TypeRef,
    known: &BTreeSet<&str>,
    all_modules: &[Module],
) -> Result<(), ValidationError> {
    match ty {
        TypeRef::Struct(name) | TypeRef::Enum(name) | TypeRef::TypedHandle(name) => {
            if !known.contains(name.as_str()) && !type_exists(all_modules, name) {
                return Err(ValidationError::UnknownTypeRef { name: name.clone() });
            }
            Ok(())
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            validate_type_ref(inner, known, all_modules)
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
            validate_type_ref(k, known, all_modules)?;
            validate_type_ref(v, known, all_modules)
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
