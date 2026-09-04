//! The validation rules: per-module name/uniqueness checks, type-reference
//! existence, ABI-representability of element shapes, callback-interface
//! shape rules, interface positions, and error-domain consistency.
//!
//! Every rule pushes into a shared `Vec<ValidationError>` sink instead of
//! returning early, so one validation pass reports every violation in the
//! document.

use super::ValidationError;
use std::collections::{BTreeMap, BTreeSet};
use weaveffi_ir::ir::{
    CallbackInterfaceDef, ErrorDomain, Function, InterfaceDef, Module, Param, StructField, TypeRef,
};

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

fn check_identifier(name: &str, errors: &mut Vec<ValidationError>) -> bool {
    if !is_valid_identifier(name) {
        errors.push(ValidationError::InvalidIdentifier(
            name.to_string(),
            "must start with a letter or underscore and contain only alphanumeric characters or underscores",
        ));
        return false;
    }
    if RESERVED.contains(&name) {
        errors.push(ValidationError::ReservedKeyword(name.to_string()));
        return false;
    }
    true
}

/// The kinds of declarations a bare name can refer to, gathered from the
/// whole module forest before any rule runs. Validation operates on the
/// document as written (every user reference is a [`TypeRef::Named`]), so
/// positional rules consult this index to learn what a name is.
#[derive(Default)]
pub(super) struct TypeIndex {
    /// Bare names of every struct, enum, interface, and callback interface
    /// anywhere in the API.
    pub all: BTreeSet<String>,
    /// Bare names of every interface anywhere in the API.
    pub interfaces: BTreeSet<String>,
    /// Bare names of every callback interface anywhere in the API.
    pub callback_interfaces: BTreeSet<String>,
    /// Bare names of every C-style (payload-free) enum anywhere in the API.
    pub plain_enums: BTreeSet<String>,
}

impl TypeIndex {
    pub(super) fn build(modules: &[Module]) -> Self {
        let mut index = Self::default();
        index.walk(modules);
        index
    }

    fn walk(&mut self, modules: &[Module]) {
        for m in modules {
            for s in &m.structs {
                self.all.insert(s.name.clone());
            }
            for e in &m.enums {
                self.all.insert(e.name.clone());
                if !e.is_rich() {
                    self.plain_enums.insert(e.name.clone());
                }
            }
            for i in &m.interfaces {
                self.all.insert(i.name.clone());
                self.interfaces.insert(i.name.clone());
            }
            for cb in &m.callback_interfaces {
                self.all.insert(cb.name.clone());
                self.callback_interfaces.insert(cb.name.clone());
            }
            self.walk(&m.modules);
        }
    }

    /// Is `name` (bare or dot-qualified) an interface?
    fn is_interface(&self, name: &str) -> bool {
        self.interfaces.contains(bare(name))
    }

    /// Is `name` (bare or dot-qualified) a callback interface?
    fn is_callback_interface(&self, name: &str) -> bool {
        self.callback_interfaces.contains(bare(name))
    }

    /// Does a declaration with this (bare or dot-qualified) name exist
    /// anywhere in the API?
    fn exists(&self, name: &str) -> bool {
        self.all.contains(bare(name))
    }

    /// May `ty` be a map key? Every target must be able to use the key in
    /// its native dictionary idiom, so only scalars, bools, strings, and
    /// C-style enums qualify; composites, optionals, bytes, and objects are
    /// rejected.
    fn is_map_key(&self, ty: &TypeRef) -> bool {
        match ty {
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::StringUtf8 => true,
            TypeRef::Named(name) => self.plain_enums.contains(bare(name)),
            _ => false,
        }
    }

    /// May `ty` be returned from a callback-interface method? Only `void`
    /// and the direct family: scalars, bool, and C-style enums.
    fn is_direct(&self, ty: &TypeRef) -> bool {
        match ty {
            TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool => true,
            TypeRef::Named(name) => self.plain_enums.contains(bare(name)),
            _ => false,
        }
    }
}

/// The final segment of a possibly dot-qualified name.
fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Enforce global bare-name uniqueness for the type namespace: structs,
/// enums, interfaces, callback interfaces, and error domains across every
/// module (including nested submodules).
///
/// Generators emit flat per-language type names, and unqualified cross-module
/// references resolve by bare name, so two types sharing a name would collide
/// in generated code and make references ambiguous.
pub(super) fn check_global_type_names(modules: &[Module], errors: &mut Vec<ValidationError>) {
    fn walk<'a>(
        modules: &'a [Module],
        prefix: &str,
        seen: &mut BTreeMap<&'a str, String>,
        errors: &mut Vec<ValidationError>,
    ) {
        for m in modules {
            let path = if prefix.is_empty() {
                m.name.clone()
            } else {
                format!("{prefix}.{}", m.name)
            };
            let names = m
                .structs
                .iter()
                .map(|s| s.name.as_str())
                .chain(m.enums.iter().map(|e| e.name.as_str()))
                .chain(m.interfaces.iter().map(|i| i.name.as_str()))
                .chain(m.callback_interfaces.iter().map(|c| c.name.as_str()))
                .chain(m.errors.iter().map(|d| d.name.as_str()));
            for name in names {
                if let Some(first) = seen.get(name) {
                    errors.push(ValidationError::DuplicateTypeName {
                        name: name.to_string(),
                        first: first.clone(),
                        second: path.clone(),
                    });
                } else {
                    seen.insert(name, path.clone());
                }
            }
            walk(&m.modules, &path, seen, errors);
        }
    }
    let mut seen = BTreeMap::new();
    walk(modules, "", &mut seen, errors);
}

/// Enforce API-global uniqueness of error *code* names across domains.
///
/// Backends with flat namespaces derive one error class or constant per
/// code, so `NotFound` declared in two different domains would collide in
/// generated code even though each domain is internally consistent.
pub(super) fn check_global_error_code_names(modules: &[Module], errors: &mut Vec<ValidationError>) {
    fn walk<'a>(
        modules: &'a [Module],
        prefix: &str,
        seen: &mut BTreeMap<&'a str, String>,
        errors: &mut Vec<ValidationError>,
    ) {
        for m in modules {
            let path = if prefix.is_empty() {
                m.name.clone()
            } else {
                format!("{prefix}.{}", m.name)
            };
            if let Some(domain) = &m.errors {
                let owner = format!("{path}.{}", domain.name);
                for code in &domain.codes {
                    if let Some(first) = seen.get(code.name.as_str()) {
                        errors.push(ValidationError::DuplicateErrorCodeName {
                            name: code.name.clone(),
                            first: first.clone(),
                            second: owner.clone(),
                        });
                    } else {
                        seen.insert(&code.name, owner.clone());
                    }
                }
            }
            walk(&m.modules, &path, seen, errors);
        }
    }
    let mut seen = BTreeMap::new();
    walk(modules, "", &mut seen, errors);
}

pub(super) fn validate_module(
    module: &Module,
    types: &TypeIndex,
    ancestor_has_domain: bool,
    errors: &mut Vec<ValidationError>,
) {
    if module.name.trim().is_empty() {
        errors.push(ValidationError::NoModuleName);
        return;
    }
    if !is_valid_identifier(&module.name) {
        errors.push(ValidationError::InvalidModuleName(
            module.name.clone(),
            "must start with a letter or underscore and contain only alphanumeric characters or underscores",
        ));
    } else if RESERVED.contains(&module.name.as_str()) {
        errors.push(ValidationError::InvalidModuleName(
            module.name.clone(),
            "reserved word",
        ));
    }

    let has_domain = ancestor_has_domain || module.errors.is_some();

    // Every C symbol suffix a module-level callable claims: free functions,
    // interface members, and the implicit clone/destroy pair. Two entries
    // with the same suffix would produce two identical C symbols.
    let mut symbol_suffixes: BTreeSet<String> = BTreeSet::new();
    let mut claim_symbol = |suffix: String, errors: &mut Vec<ValidationError>| {
        if !symbol_suffixes.insert(suffix.clone()) {
            errors.push(ValidationError::AbiSymbolCollision {
                module: module.name.clone(),
                symbol: suffix,
            });
        }
    };

    let mut function_names = BTreeSet::new();
    for f in &module.functions {
        if !function_names.insert(f.name.clone()) {
            errors.push(ValidationError::DuplicateFunctionName {
                module: module.name.clone(),
                function: f.name.clone(),
            });
        }
        claim_symbol(f.name.clone(), errors);
        validate_function(&module.name, &f.name, f, has_domain, errors);
    }

    let mut struct_names = BTreeSet::new();
    for s in &module.structs {
        check_identifier(&s.name, errors);
        if !struct_names.insert(s.name.clone()) {
            errors.push(ValidationError::DuplicateStructName {
                module: module.name.clone(),
                name: s.name.clone(),
            });
        }
        if s.fields.is_empty() {
            errors.push(ValidationError::EmptyStruct {
                module: module.name.clone(),
                name: s.name.clone(),
            });
        }
        let mut field_names = BTreeSet::new();
        for f in &s.fields {
            check_identifier(&f.name, errors);
            if !field_names.insert(f.name.clone()) {
                errors.push(ValidationError::DuplicateStructField {
                    struct_name: s.name.clone(),
                    field: f.name.clone(),
                });
            }
        }
    }

    let mut enum_names = BTreeSet::new();
    for e in &module.enums {
        check_identifier(&e.name, errors);
        if !enum_names.insert(e.name.clone()) {
            errors.push(ValidationError::DuplicateEnumName {
                module: module.name.clone(),
                name: e.name.clone(),
            });
        }
        if e.variants.is_empty() {
            errors.push(ValidationError::EmptyEnum {
                module: module.name.clone(),
                name: e.name.clone(),
            });
        }
        let mut variant_names = BTreeSet::new();
        let mut variant_values = BTreeMap::new();
        for v in &e.variants {
            check_identifier(&v.name, errors);
            if !variant_names.insert(v.name.clone()) {
                errors.push(ValidationError::DuplicateEnumVariant {
                    enum_name: e.name.clone(),
                    variant: v.name.clone(),
                });
            }
            if variant_values.insert(v.value, v.name.clone()).is_some() {
                errors.push(ValidationError::DuplicateEnumValue {
                    enum_name: e.name.clone(),
                    value: v.value,
                });
            }
            let mut variant_field_names = BTreeSet::new();
            for f in &v.fields {
                check_identifier(&f.name, errors);
                if !variant_field_names.insert(f.name.clone()) {
                    errors.push(ValidationError::DuplicateEnumVariantField {
                        enum_name: e.name.clone(),
                        variant: v.name.clone(),
                        field: f.name.clone(),
                    });
                }
            }
        }
    }

    let mut local_interface_names = BTreeSet::new();
    for i in &module.interfaces {
        check_identifier(&i.name, errors);
        if !local_interface_names.insert(i.name.clone()) {
            errors.push(ValidationError::DuplicateInterfaceName {
                module: module.name.clone(),
                name: i.name.clone(),
            });
        }
        claim_symbol(format!("{}_clone", i.name), errors);
        claim_symbol(format!("{}_destroy", i.name), errors);
        validate_interface(module, i, has_domain, &mut claim_symbol, errors);
    }

    let mut callback_names = BTreeSet::new();
    for cb in &module.callback_interfaces {
        check_identifier(&cb.name, errors);
        if !callback_names.insert(cb.name.clone()) {
            errors.push(ValidationError::DuplicateCallbackInterfaceName {
                module: module.name.clone(),
                name: cb.name.clone(),
            });
        }
        validate_callback_interface(module, cb, types, errors);
    }

    // A field of a record, a rich-enum variant, or an error payload is
    // serialized inside a value buffer, so it obeys the buffered positional
    // rules: no iterators, no callback interfaces, no interface map keys, and
    // every reference must resolve.
    let mut check_buffered_field = |f: &StructField, location: &dyn Fn() -> String| {
        if contains_iterator(&f.ty) {
            errors.push(ValidationError::IteratorInInvalidPosition {
                location: location(),
            });
        }
        validate_type_ref(&f.ty, types, errors);
        check_interface_positions(&f.ty, types, location, errors);
        check_no_callback_interface(&f.ty, types, location, errors);
    };
    for s in &module.structs {
        for f in &s.fields {
            let location = || format!("field '{}' of struct '{}'", f.name, s.name);
            check_buffered_field(f, &location);
        }
    }
    for e in &module.enums {
        for v in &e.variants {
            for f in &v.fields {
                let location = || format!("field '{}' of variant '{}::{}'", f.name, e.name, v.name);
                check_buffered_field(f, &location);
            }
        }
    }
    if let Some(domain) = &module.errors {
        for c in &domain.codes {
            for f in &c.fields {
                let location = || {
                    format!(
                        "payload field '{}' of error code '{}::{}'",
                        f.name, domain.name, c.name
                    )
                };
                check_buffered_field(f, &location);
            }
        }
    }
    for f in &module.functions {
        validate_callable_types(&module.name, &f.name, f, types, errors);
    }
    for i in &module.interfaces {
        for f in i.constructors.iter().chain(&i.methods).chain(&i.statics) {
            let display = format!("{}.{}", i.name, f.name);
            validate_callable_types(&module.name, &display, f, types, errors);
        }
    }

    if let Some(domain) = &module.errors {
        validate_error_domain(module, domain, &function_names, errors);
    }

    let mut sub_module_names = BTreeSet::new();
    for sub in &module.modules {
        if !sub_module_names.insert(sub.name.clone()) {
            errors.push(ValidationError::DuplicateModuleName(sub.name.clone()));
        }
        validate_module(sub, types, has_domain, errors);
    }
}

/// Validate an interface's shape: unique member names across constructors,
/// methods, and statics; constructor restrictions; per-member signature
/// rules; and C symbol claims for every member.
fn validate_interface(
    module: &Module,
    iface: &InterfaceDef,
    has_domain: bool,
    claim_symbol: &mut impl FnMut(String, &mut Vec<ValidationError>),
    errors: &mut Vec<ValidationError>,
) {
    if iface.constructors.is_empty() && iface.methods.is_empty() && iface.statics.is_empty() {
        errors.push(ValidationError::EmptyInterface {
            module: module.name.clone(),
            name: iface.name.clone(),
        });
    }
    let mut member_names = BTreeSet::new();
    let mut check_member = |f: &Function, errors: &mut Vec<ValidationError>| {
        if !member_names.insert(f.name.clone()) {
            errors.push(ValidationError::DuplicateInterfaceMember {
                interface: iface.name.clone(),
                name: f.name.clone(),
            });
        }
        claim_symbol(format!("{}_{}", iface.name, f.name), errors);
        let display = format!("{}.{}", iface.name, f.name);
        validate_function(&module.name, &display, f, has_domain, errors);
    };
    for c in &iface.constructors {
        check_member(c, errors);
        if c.returns.is_some() {
            errors.push(ValidationError::ConstructorHasReturn {
                interface: iface.name.clone(),
                constructor: c.name.clone(),
            });
        }
        if c.r#async {
            errors.push(ValidationError::AsyncConstructor {
                interface: iface.name.clone(),
                constructor: c.name.clone(),
            });
        }
    }
    for m in &iface.methods {
        check_member(m, errors);
    }
    for s in &iface.statics {
        check_member(s, errors);
    }
}

/// Validate a callback interface: at least one method, unique method names,
/// and per-method restrictions. A callback method is implemented by the
/// consumer, so it is synchronous, never throws, never takes a cancel token,
/// returns nothing or a direct value, and takes no callback interface or
/// iterator as a parameter.
fn validate_callback_interface(
    module: &Module,
    cb: &CallbackInterfaceDef,
    types: &TypeIndex,
    errors: &mut Vec<ValidationError>,
) {
    if cb.methods.is_empty() {
        errors.push(ValidationError::EmptyCallbackInterface {
            module: module.name.clone(),
            name: cb.name.clone(),
        });
    }
    let mut method_names = BTreeSet::new();
    for m in &cb.methods {
        check_identifier(&m.name, errors);
        if !method_names.insert(m.name.clone()) {
            errors.push(ValidationError::DuplicateCallbackMethod {
                interface: cb.name.clone(),
                name: m.name.clone(),
            });
        }
        let reject = |reason: &'static str, errors: &mut Vec<ValidationError>| {
            errors.push(ValidationError::InvalidCallbackMethod {
                interface: cb.name.clone(),
                method: m.name.clone(),
                reason,
            });
        };
        if m.r#async {
            reject("cannot be async", errors);
        }
        if m.throws {
            reject("cannot declare throws", errors);
        }
        if m.cancellable {
            reject("cannot be cancellable", errors);
        }
        if let Some(ret) = &m.returns {
            validate_type_ref(ret, types, errors);
            if !types.is_direct(ret) {
                reject(
                    "must return nothing or a direct value (scalar, bool, or C-style enum)",
                    errors,
                );
            }
        }
        let mut param_names = BTreeSet::new();
        for p in &m.params {
            validate_param(p, errors);
            if !param_names.insert(p.name.clone()) {
                errors.push(ValidationError::DuplicateParamName {
                    module: module.name.clone(),
                    function: format!("{}.{}", cb.name, m.name),
                    param: p.name.clone(),
                });
            }
            let location = || {
                format!(
                    "param '{}' of callback interface method '{}.{}'",
                    p.name, cb.name, m.name
                )
            };
            if contains_iterator(&p.ty) {
                errors.push(ValidationError::IteratorInInvalidPosition {
                    location: location(),
                });
            }
            validate_type_ref(&p.ty, types, errors);
            check_interface_positions(&p.ty, types, location, errors);
            check_no_callback_interface(&p.ty, types, location, errors);
        }
    }
}

/// Name-level checks for one callable: a valid identifier, unique parameter
/// names, and an error domain in scope when the callable declares `throws`.
fn validate_function(
    module_name: &str,
    display_name: &str,
    f: &Function,
    has_domain: bool,
    errors: &mut Vec<ValidationError>,
) {
    check_identifier(&f.name, errors);

    if f.throws && !has_domain {
        errors.push(ValidationError::ThrowsWithoutErrorDomain {
            module: module_name.to_string(),
            function: display_name.to_string(),
        });
    }

    let mut param_names = BTreeSet::new();
    for p in &f.params {
        validate_param(p, errors);
        if !param_names.insert(p.name.clone()) {
            errors.push(ValidationError::DuplicateParamName {
                module: module_name.to_string(),
                function: display_name.to_string(),
                param: p.name.clone(),
            });
        }
    }
}

/// Type-level checks for one callable's parameters and return: iterator
/// positions, async-iterator exclusion, reference existence, element shapes,
/// interface map keys, and callback-interface positions.
fn validate_callable_types(
    module_name: &str,
    display_name: &str,
    f: &Function,
    types: &TypeIndex,
    errors: &mut Vec<ValidationError>,
) {
    for p in &f.params {
        let location = || {
            format!(
                "param '{}' of function '{module_name}::{display_name}'",
                p.name
            )
        };
        if contains_iterator(&p.ty) {
            errors.push(ValidationError::IteratorInInvalidPosition {
                location: location(),
            });
        }
        validate_type_ref(&p.ty, types, errors);
        check_interface_positions(&p.ty, types, location, errors);
        // A bare callback interface is the one legal callback position;
        // anything nested (`Listener?`, `[Listener]`) is not.
        match &p.ty {
            TypeRef::Named(name) if types.is_callback_interface(name) => {}
            other => check_no_callback_interface(other, types, location, errors),
        }
    }
    if let Some(ret) = &f.returns {
        let location = || format!("return type of {module_name}::{display_name}");
        // An async function completes through a one-shot callback; an
        // iterator needs a pull-based handle. The two shapes cannot
        // compose on the C ABI, so reject the combination up front
        // instead of letting backends lower it inconsistently.
        if f.r#async && contains_iterator(ret) {
            errors.push(ValidationError::AsyncIteratorReturn {
                module: module_name.to_string(),
                function: display_name.to_string(),
            });
        }
        // An iterator is a pull handle, valid only as the outermost return
        // shape: `[iter<T>]`, `iter<T>?`, `iter<iter<T>>`, and iterators
        // inside a map have no lowering.
        let nested_iterator = match ret {
            TypeRef::Iterator(elem) => contains_iterator(elem),
            other => contains_iterator(other),
        };
        if nested_iterator {
            errors.push(ValidationError::IteratorInInvalidPosition {
                location: format!("nested inside the {}", location()),
            });
        }
        validate_type_ref(ret, types, errors);
        check_interface_positions(ret, types, location, errors);
        check_no_callback_interface(ret, types, location, errors);
    }
}

fn validate_param(p: &Param, errors: &mut Vec<ValidationError>) {
    check_identifier(&p.name, errors);
}

/// Enforce the one place an interface reference may not appear: as a map
/// key. Objects are reference-counted tokens, so they compose with every
/// other buffered shape (fields, elements, map values, optionals), but no
/// target can hash an object by identity in a way that survives the ABI.
fn check_interface_positions(
    ty: &TypeRef,
    types: &TypeIndex,
    location: impl Fn() -> String + Copy,
    errors: &mut Vec<ValidationError>,
) {
    ty.walk(&mut |t| {
        if let TypeRef::Map(k, _) = t {
            if let TypeRef::Named(name) = &**k {
                if types.is_interface(name) {
                    errors.push(ValidationError::InterfaceInInvalidPosition {
                        name: name.clone(),
                        location: format!("map key of {}", location()),
                    });
                }
            }
        }
    });
}

/// Reject any callback-interface reference reachable from `ty`. Callers that
/// permit a bare top-level callback interface unwrap it before calling.
fn check_no_callback_interface(
    ty: &TypeRef,
    types: &TypeIndex,
    location: impl Fn() -> String + Copy,
    errors: &mut Vec<ValidationError>,
) {
    ty.walk(&mut |t| {
        if let TypeRef::Named(name) = t {
            if types.is_callback_interface(name) {
                errors.push(ValidationError::CallbackInterfaceInInvalidPosition {
                    name: name.clone(),
                    location: location(),
                });
            }
        }
    });
}

fn contains_iterator(ty: &TypeRef) -> bool {
    let mut found = false;
    ty.walk(&mut |t| {
        if matches!(t, TypeRef::Iterator(_)) {
            found = true;
        }
    });
    found
}

fn validate_type_ref(ty: &TypeRef, types: &TypeIndex, errors: &mut Vec<ValidationError>) {
    match ty {
        TypeRef::Named(name) => {
            if !types.exists(name) {
                errors.push(ValidationError::UnknownTypeRef { name: name.clone() });
            }
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            validate_type_ref(inner, types, errors);
        }
        TypeRef::Map(k, v) => {
            if !types.is_map_key(k) {
                errors.push(ValidationError::InvalidMapKey {
                    key_type: k.to_string(),
                });
            }
            validate_type_ref(k, types, errors);
            validate_type_ref(v, types, errors);
        }
        _ => {}
    }
}

fn validate_error_domain(
    module: &Module,
    domain: &ErrorDomain,
    function_names: &BTreeSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    if domain.name.trim().is_empty() {
        errors.push(ValidationError::ErrorDomainMissingName(module.name.clone()));
        return;
    }
    check_identifier(&domain.name, errors);
    if function_names.contains(&domain.name) {
        errors.push(ValidationError::NameCollisionWithErrorDomain {
            module: module.name.clone(),
            name: domain.name.clone(),
        });
    }

    let mut by_name: BTreeSet<String> = BTreeSet::new();
    let mut by_code: BTreeMap<i32, String> = BTreeMap::new();
    for c in &domain.codes {
        check_identifier(&c.name, errors);
        // 0 means success and the whole negative range is reserved for the
        // runtime (-1 generic error, -2 panic, -3 marshalling failure, -4
        // foreign callback error, and room to grow), so domain codes must be
        // positive.
        if c.code <= 0 {
            errors.push(ValidationError::InvalidErrorCode {
                module: module.name.clone(),
                name: c.name.clone(),
            });
        }
        if !by_name.insert(c.name.clone()) {
            errors.push(ValidationError::DuplicateErrorName {
                module: module.name.clone(),
                name: c.name.clone(),
            });
        }
        if by_code.insert(c.code, c.name.clone()).is_some() {
            errors.push(ValidationError::DuplicateErrorCode {
                module: module.name.clone(),
                code: c.code,
            });
        }
    }
}
