//! The validation rules: per-module name/uniqueness checks, type-reference
//! existence, ABI-representability of element shapes, callback parameter
//! marshalability, interface shape rules, and error-domain consistency.
//!
//! Every rule pushes into a shared `Vec<ValidationError>` sink instead of
//! returning early, so one validation pass reports every violation in the
//! document.

use super::ValidationError;
use std::collections::{BTreeMap, BTreeSet};
use weaveffi_ir::ir::{ErrorDomain, Function, InterfaceDef, Module, Param, TypeRef};

/// The marshalable kernel for callback parameters. Callback arguments cross
/// the boundary *into* the foreign language without an FFI round-trip, so
/// every target must be able to deep-copy them in a C trampoline: scalars,
/// bool, enums, string, bytes, handles, structs (borrowed), optionals of
/// those, lists of scalars/strings, and maps of scalars/strings.
fn callback_param_type_supported(ty: &TypeRef) -> bool {
    fn leaf(ty: &TypeRef) -> bool {
        matches!(
            ty,
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
    match ty {
        t if leaf(t) => true,
        TypeRef::Optional(inner) => leaf(inner),
        TypeRef::List(inner) => scalar_element(inner),
        TypeRef::Map(k, v) => scalar_element(k) && scalar_element(v),
        _ => false,
    }
}

/// The single-slot *by-value-ish* element kernel: types that lower to one C
/// array slot with no auxiliary length and no ownership transfer beyond a
/// deep copy. These are the only shapes allowed as map keys/values and as
/// list elements inside callback parameters.
fn scalar_element(ty: &TypeRef) -> bool {
    matches!(
        ty,
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
            | TypeRef::Enum(_)
            | TypeRef::StringUtf8
            | TypeRef::BorrowedStr
    )
}

/// The single-slot element kernel for lists and iterators: everything in
/// [`scalar_element`] plus handles and struct pointers, which also occupy
/// exactly one array slot.
fn slot_element(ty: &TypeRef) -> bool {
    scalar_element(ty)
        || matches!(
            ty,
            TypeRef::Handle | TypeRef::TypedHandle(_) | TypeRef::Struct(_)
        )
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

/// Enforce global bare-name uniqueness for the type namespace: structs,
/// enums, interfaces, and error domains across every module (including
/// nested submodules).
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

/// Collect the bare names of every interface declared anywhere in the module
/// tree. Validation runs before reference resolution, so an interface
/// reference is still spelled `TypeRef::Struct(name)` and positional rules
/// need this set to recognize one.
pub(super) fn collect_interface_names(modules: &[Module], out: &mut BTreeSet<String>) {
    for m in modules {
        for i in &m.interfaces {
            out.insert(i.name.clone());
        }
        collect_interface_names(&m.modules, out);
    }
}

pub(super) fn validate_module(
    module: &Module,
    all_modules: &[Module],
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

    let mut interface_names = BTreeSet::new();
    collect_interface_names(all_modules, &mut interface_names);
    let has_domain = ancestor_has_domain || module.errors.is_some();

    // Every C symbol suffix a module-level callable claims: free functions,
    // interface members, and implicit destructors. Two entries with the same
    // suffix would produce two identical C symbols.
    let mut symbol_suffixes: BTreeMap<String, ()> = BTreeMap::new();
    let mut claim_symbol = |suffix: String, errors: &mut Vec<ValidationError>| {
        if symbol_suffixes.insert(suffix.clone(), ()).is_some() {
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
            if s.builder {
                errors.push(ValidationError::BuilderStructEmpty {
                    module: module.name.clone(),
                    name: s.name.clone(),
                });
            } else {
                errors.push(ValidationError::EmptyStruct {
                    module: module.name.clone(),
                    name: s.name.clone(),
                });
            }
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
        claim_symbol(format!("{}_destroy", i.name), errors);
        validate_interface(module, i, has_domain, &mut claim_symbol, errors);
    }

    let known_types: BTreeSet<&str> = struct_names
        .iter()
        .map(|s| s.as_str())
        .chain(enum_names.iter().map(|s| s.as_str()))
        .chain(local_interface_names.iter().map(|s| s.as_str()))
        .collect();
    let ctx = TypeCtx {
        known: &known_types,
        all_modules,
        interfaces: &interface_names,
    };

    for s in &module.structs {
        for f in &s.fields {
            let location = || format!("field '{}' of struct '{}'", f.name, s.name);
            if let Some(ty) = contains_borrowed(&f.ty) {
                errors.push(ValidationError::BorrowedTypeInInvalidPosition {
                    ty: ty.to_string(),
                    location: location(),
                });
            }
            if contains_iterator(&f.ty) {
                errors.push(ValidationError::IteratorInInvalidPosition {
                    location: location(),
                });
            }
            validate_type_ref(&f.ty, &ctx, errors);
            check_element_shapes(&f.ty, location, errors);
            check_interface_positions(&f.ty, &ctx, false, location, errors);
        }
    }
    // Rich-enum variant fields carry associated data and lower exactly like
    // struct fields (by value across the C ABI), so they obey the same
    // positional rules: no borrowed views, no iterators, no interfaces, and
    // only ABI-representable element shapes.
    for e in &module.enums {
        for v in &e.variants {
            for f in &v.fields {
                let location = || format!("field '{}' of variant '{}::{}'", f.name, e.name, v.name);
                if let Some(ty) = contains_borrowed(&f.ty) {
                    errors.push(ValidationError::BorrowedTypeInInvalidPosition {
                        ty: ty.to_string(),
                        location: location(),
                    });
                }
                if contains_iterator(&f.ty) {
                    errors.push(ValidationError::IteratorInInvalidPosition {
                        location: location(),
                    });
                }
                validate_type_ref(&f.ty, &ctx, errors);
                check_element_shapes(&f.ty, location, errors);
                check_interface_positions(&f.ty, &ctx, false, location, errors);
            }
        }
    }
    for f in &module.functions {
        validate_callable_types(&module.name, &f.name, f, &ctx, errors);
    }
    for i in &module.interfaces {
        for c in &i.constructors {
            let display = format!("{}.{}", i.name, c.name);
            validate_callable_types(&module.name, &display, c, &ctx, errors);
        }
        for m in &i.methods {
            let display = format!("{}.{}", i.name, m.name);
            validate_callable_types(&module.name, &display, m, &ctx, errors);
        }
        for s in &i.statics {
            let display = format!("{}.{}", i.name, s.name);
            validate_callable_types(&module.name, &display, s, &ctx, errors);
        }
    }

    let mut callback_names = BTreeSet::new();
    for cb in &module.callbacks {
        check_identifier(&cb.name, errors);
        if !callback_names.insert(cb.name.clone()) {
            errors.push(ValidationError::DuplicateCallbackName {
                module: module.name.clone(),
                name: cb.name.clone(),
            });
        }
        for p in &cb.params {
            validate_param(p, errors);
            if references_interface(&p.ty, &ctx) {
                errors.push(ValidationError::InterfaceInInvalidPosition {
                    name: type_ref_display(&p.ty),
                    location: format!("param '{}' of callback '{}'", p.name, cb.name),
                });
            } else if !callback_param_type_supported(&p.ty) {
                errors.push(ValidationError::UnsupportedCallbackParamType {
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
        check_identifier(&l.name, errors);
        if !listener_names.insert(l.name.clone()) {
            errors.push(ValidationError::DuplicateListenerName {
                module: module.name.clone(),
                name: l.name.clone(),
            });
        }
        if !callback_names.contains(&l.event_callback) {
            errors.push(ValidationError::ListenerCallbackNotFound {
                module: module.name.clone(),
                listener: l.name.clone(),
                callback: l.event_callback.clone(),
            });
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
        validate_module(sub, all_modules, has_domain, errors);
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

/// Type-level checks for one callable's parameters and return: iterator and
/// borrowed positions, async-iterator exclusion, reference existence, element
/// shapes, and interface positions.
fn validate_callable_types(
    module_name: &str,
    display_name: &str,
    f: &Function,
    ctx: &TypeCtx<'_>,
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
        validate_type_ref(&p.ty, ctx, errors);
        check_element_shapes(&p.ty, location, errors);
        check_interface_positions(&p.ty, ctx, true, location, errors);
    }
    if let Some(ret) = &f.returns {
        let location = || format!("return type of {module_name}::{display_name}");
        if let Some(ty) = contains_borrowed(ret) {
            errors.push(ValidationError::BorrowedTypeInInvalidPosition {
                ty: ty.to_string(),
                location: location(),
            });
        }
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
        validate_type_ref(ret, ctx, errors);
        check_element_shapes(ret, location, errors);
        check_interface_positions(ret, ctx, true, location, errors);
    }
}

fn validate_param(p: &Param, errors: &mut Vec<ValidationError>) {
    check_identifier(&p.name, errors);
}

/// Shared context for type-reference checks: the current module's own type
/// names, the full module forest for cross-module lookups, and the global set
/// of interface names.
struct TypeCtx<'a> {
    known: &'a BTreeSet<&'a str>,
    all_modules: &'a [Module],
    interfaces: &'a BTreeSet<String>,
}

impl TypeCtx<'_> {
    /// Is `name` (bare or dot-qualified) an interface?
    fn is_interface(&self, name: &str) -> bool {
        let bare = name.rsplit('.').next().unwrap_or(name);
        self.interfaces.contains(bare)
    }
}

/// The element shapes the C ABI can faithfully represent. Lists, maps, and
/// iterators lower to flat arrays (`T* + len`, parallel key/value arrays, or
/// a one-slot `next` out-param), so their element types must themselves be
/// single C slots. Composite elements (lists of lists, optional scalars in
/// lists, bytes elements needing a second length slot, ...) silently flatten
/// in `element_ctype` and would generate wrong code in every backend; reject
/// them up front. Returns the first offending element type, if any.
fn unsupported_element_shape(ty: &TypeRef) -> Option<&TypeRef> {
    match ty {
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            t if slot_element(t) => None,
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
            if !scalar_element(k) {
                Some(k)
            } else if !scalar_element(v) {
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
    errors: &mut Vec<ValidationError>,
) {
    if let Some(bad) = unsupported_element_shape(ty) {
        errors.push(ValidationError::UnsupportedElementType {
            location: location(),
            ty: format!("{bad:?}"),
        });
    }
}

/// Does `ty` reference an interface anywhere in its structure? Used for
/// positions where interfaces are wholly disallowed (callback parameters).
fn references_interface(ty: &TypeRef, ctx: &TypeCtx<'_>) -> bool {
    match ty {
        TypeRef::Struct(name) | TypeRef::Interface(name) => ctx.is_interface(name),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            references_interface(inner, ctx)
        }
        TypeRef::Map(k, v) => references_interface(k, ctx) || references_interface(v, ctx),
        _ => false,
    }
}

/// Render the interface name inside `ty` for an error message, falling back
/// to the debug spelling for composite shapes.
fn type_ref_display(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Struct(name) | TypeRef::Interface(name) => name.clone(),
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            type_ref_display(inner)
        }
        other => format!("{other:?}"),
    }
}

/// Enforce where an interface reference may appear. With `top` true (a
/// function parameter or return), a bare interface or an optional interface
/// is allowed; anywhere deeper (collection elements, map keys/values,
/// iterator elements, struct fields via `top` false) is rejected: an
/// interface is a live object reference, and element positions imply deep
/// copies the object cannot provide.
fn check_interface_positions(
    ty: &TypeRef,
    ctx: &TypeCtx<'_>,
    top: bool,
    location: impl Fn() -> String + Copy,
    errors: &mut Vec<ValidationError>,
) {
    match ty {
        TypeRef::Struct(name) | TypeRef::Interface(name) => {
            if ctx.is_interface(name) && !top {
                errors.push(ValidationError::InterfaceInInvalidPosition {
                    name: name.clone(),
                    location: location(),
                });
            }
        }
        // A typed handle names a token *type tag*, not an object; pointing
        // one at an interface would conflate u64 tokens with object pointers.
        TypeRef::TypedHandle(name) => {
            if ctx.is_interface(name) {
                errors.push(ValidationError::InterfaceInInvalidPosition {
                    name: name.clone(),
                    location: format!("typed handle in {}", location()),
                });
            }
        }
        // Optionality does not change the position: `Store?` is still a
        // top-level object reference.
        TypeRef::Optional(inner) => check_interface_positions(inner, ctx, top, location, errors),
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            check_interface_positions(inner, ctx, false, location, errors);
        }
        TypeRef::Map(k, v) => {
            check_interface_positions(k, ctx, false, location, errors);
            check_interface_positions(v, ctx, false, location, errors);
        }
        _ => {}
    }
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

/// Does a struct, enum, or interface named `name` exist anywhere in the
/// module tree, including nested submodules? Validation runs before reference
/// qualification, so an unqualified reference is valid if its bare name is
/// defined anywhere; the resolver later rewrites it to the owning module's
/// full path.
fn type_exists(modules: &[Module], name: &str) -> bool {
    modules.iter().any(|m| {
        m.structs.iter().any(|s| s.name == name)
            || m.enums.iter().any(|e| e.name == name)
            || m.interfaces.iter().any(|i| i.name == name)
            || type_exists(&m.modules, name)
    })
}

fn validate_type_ref(ty: &TypeRef, ctx: &TypeCtx<'_>, errors: &mut Vec<ValidationError>) {
    match ty {
        TypeRef::Struct(name)
        | TypeRef::Interface(name)
        | TypeRef::Enum(name)
        | TypeRef::TypedHandle(name) => {
            if !ctx.known.contains(name.as_str()) && !type_exists(ctx.all_modules, name) {
                errors.push(ValidationError::UnknownTypeRef { name: name.clone() });
            }
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            validate_type_ref(inner, ctx, errors);
        }
        TypeRef::Map(k, v) => {
            let bad_key = match k.as_ref() {
                TypeRef::Struct(name) => Some(format!("struct {name}")),
                TypeRef::List(_) => Some("list".to_string()),
                TypeRef::Map(_, _) => Some("map".to_string()),
                _ => None,
            };
            if let Some(key_type) = bad_key {
                errors.push(ValidationError::InvalidMapKey { key_type });
            }
            validate_type_ref(k, ctx, errors);
            validate_type_ref(v, ctx, errors);
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
    if function_names.contains(&domain.name) {
        errors.push(ValidationError::NameCollisionWithErrorDomain {
            module: module.name.clone(),
            name: domain.name.clone(),
        });
    }

    let mut by_name: BTreeSet<String> = BTreeSet::new();
    let mut by_code: BTreeMap<i32, String> = BTreeMap::new();
    for c in &domain.codes {
        // 0 means success and -2 is the runtime's reserved panic code.
        if c.code == 0 || c.code == -2 {
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
