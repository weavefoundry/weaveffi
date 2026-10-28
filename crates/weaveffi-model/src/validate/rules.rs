//! The validation rules: library-wide name uniqueness, identifier and shape
//! checks, type-reference resolution, ABI-representability of element
//! shapes, callback-interface rules, interface positions, recursive value
//! types, and error-domain consistency.
//!
//! Every rule pushes into a shared sink instead of returning early, so one
//! validation pass reports every violation in the document. Each violation
//! carries the declaration path that encloses it (the declaration, then
//! member and parameter or field names), which the diagnostic uses to locate
//! the offending text.

use super::{Found, ValidationError};
use crate::ir::{Api, CallbackDef, ErrorDomain, Field, Function, InterfaceDef, Throws, TypeRef};
use crate::ty::{Prim, TypeIndex, TypeKind};
use std::collections::{BTreeMap, BTreeSet};

const RESERVED: &[&str] = &[
    "if", "else", "for", "while", "loop", "match", "type", "return", "async", "await", "break",
    "continue", "fn", "struct", "enum", "mod", "use",
];

/// The slots the C ABI adds to signatures, which no parameter may be named.
const ABI_SLOTS: &[&str] = &[
    "self",
    "ctx",
    "out_err",
    "callback",
    "context",
    "cancel_token",
];

/// Primitive spellings other languages have that WeaveFFI deliberately
/// doesn't support; a reference to one gets a dedicated diagnostic instead
/// of "unknown type".
const UNSUPPORTED_PRIMITIVES: &[&str] = &["usize", "isize", "u128", "i128", "char"];

const IDENTIFIER_RULE: &str =
    "must start with a letter or underscore and contain only alphanumeric characters or underscores";

fn is_valid_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        None => false,
        Some(c) if !(c.is_ascii_alphabetic() || c == '_') => false,
        _ => chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
    }
}

/// `scope` extended with `names`.
fn within(scope: &[String], names: &[&str]) -> Vec<String> {
    let mut out = scope.to_vec();
    out.extend(names.iter().map(|n| (*n).to_string()));
    out
}

/// Run every rule over `api`, whose declarations `types` indexes.
pub(super) fn check(api: &Api, types: &TypeIndex, found: &mut Vec<Found>) {
    let carriers = object_carriers(api, types);
    let mut cx = Cx {
        types,
        carriers: &carriers,
        found,
    };
    cx.declarations(api);
    check_names(api, cx.found);
    check_recursion(api, types, cx.found);
}

/// Enforce library-wide name uniqueness: every record, enum, error domain,
/// interface, callback interface, and free function shares one namespace,
/// and every error-code name is unique too.
fn check_names(api: &Api, found: &mut Vec<Found>) {
    let mut seen: BTreeMap<&str, &'static str> = BTreeMap::new();
    let decls = api
        .records
        .iter()
        .map(|d| (d.name.as_str(), "record"))
        .chain(api.enums.iter().map(|d| (d.name.as_str(), "enum")))
        .chain(api.errors.iter().map(|d| (d.name.as_str(), "error domain")))
        .chain(
            api.interfaces
                .iter()
                .map(|d| (d.name.as_str(), "interface")),
        )
        .chain(
            api.callbacks
                .iter()
                .map(|d| (d.name.as_str(), "callback interface")),
        )
        .chain(api.functions.iter().map(|d| (d.name.as_str(), "function")));
    for (name, kind) in decls {
        match seen.get(name) {
            Some(first) => found.push((
                ValidationError::DuplicateName {
                    name: name.to_string(),
                    first,
                    second: kind,
                },
                vec![],
            )),
            None => {
                seen.insert(name, kind);
            }
        }
    }
    let mut codes: BTreeMap<&str, &str> = BTreeMap::new();
    for domain in &api.errors {
        for code in &domain.codes {
            match codes.get(code.name.as_str()) {
                Some(first) => found.push((
                    ValidationError::DuplicateErrorCodeName {
                        name: code.name.clone(),
                        first: (*first).to_string(),
                        second: domain.name.clone(),
                    },
                    vec![domain.name.clone()],
                )),
                None => {
                    codes.insert(&code.name, &domain.name);
                }
            }
        }
    }
}

/// Reject a record or rich enum that contains itself by value: directly or
/// through optionals and other records and rich enums, but not through a
/// list or a map, which hold their elements indirectly.
fn check_recursion<'a>(api: &'a Api, types: &TypeIndex, found: &mut Vec<Found>) {
    // The by-value edges: (type, field path) -> the user types the field
    // holds by value.
    let mut edges: BTreeMap<&str, Vec<(String, String)>> = BTreeMap::new();
    let mut add = |name: &'a str, label: String, fields: &'a [Field]| {
        for f in fields {
            let mut ty = &f.ty;
            while let TypeRef::Optional(inner) = ty {
                ty = inner;
            }
            if let TypeRef::Named(target) = ty {
                if matches!(
                    types.kind(target),
                    Some(TypeKind::Record | TypeKind::RichEnum)
                ) {
                    edges
                        .entry(name)
                        .or_default()
                        .push((format!("{label}.{}", f.name), target.clone()));
                }
            }
        }
    };
    for r in &api.records {
        add(&r.name, r.name.clone(), &r.fields);
    }
    for e in &api.enums {
        for v in &e.variants {
            add(&e.name, format!("{}::{}", e.name, v.name), &v.fields);
        }
    }
    let mut reported = BTreeSet::new();
    for start in edges.keys() {
        // Depth-first search for a path back to `start`.
        let mut stack: Vec<(&str, Vec<String>)> = vec![(start, Vec::new())];
        let mut visited = BTreeSet::new();
        while let Some((at, path)) = stack.pop() {
            for (field, target) in edges.get(at).into_iter().flatten() {
                let mut next = path.clone();
                next.push(format!("{field} -> {target}"));
                if target == start {
                    if reported.insert(start.to_string()) {
                        found.push((
                            ValidationError::RecursiveType {
                                name: start.to_string(),
                                path: next.join(", "),
                            },
                            vec![start.to_string()],
                        ));
                    }
                } else if visited.insert(target.as_str()) {
                    stack.push((target, next));
                }
            }
        }
    }
}

/// The rule context: the type index and the violation sink.
struct Cx<'a> {
    types: &'a TypeIndex,
    /// Records and rich enums that carry an interface object somewhere in
    /// their fields (see [`object_carriers`]).
    carriers: &'a BTreeSet<String>,
    found: &'a mut Vec<Found>,
}

impl Cx<'_> {
    fn push(&mut self, scope: &[String], error: ValidationError) {
        self.found.push((error, scope.to_vec()));
    }

    /// Check that `name` is a valid, non-reserved identifier.
    fn identifier(&mut self, scope: &[String], name: &str) {
        if !is_valid_identifier(name) {
            self.push(
                scope,
                ValidationError::InvalidIdentifier {
                    name: name.to_string(),
                    reason: IDENTIFIER_RULE,
                },
            );
        } else if RESERVED.contains(&name) {
            self.push(
                scope,
                ValidationError::ReservedKeyword {
                    name: name.to_string(),
                },
            );
        }
    }

    fn kind(&self, name: &str) -> Option<TypeKind> {
        self.types.kind(name)
    }

    fn declarations(&mut self, api: &Api) {
        let top: &[String] = &[];
        for f in &api.functions {
            self.function(top, &f.name, f);
            self.callable_types(std::slice::from_ref(&f.name), &f.name, f);
        }

        for r in &api.records {
            self.identifier(top, &r.name);
            let decl = within(top, &[&r.name]);
            if r.fields.is_empty() {
                self.push(
                    &decl,
                    ValidationError::EmptyRecord {
                        name: r.name.clone(),
                    },
                );
            }
            let mut names = BTreeSet::new();
            for f in &r.fields {
                self.identifier(&decl, &f.name);
                if !names.insert(&f.name) {
                    self.push(
                        &decl,
                        ValidationError::DuplicateRecordField {
                            record: r.name.clone(),
                            field: f.name.clone(),
                        },
                    );
                }
                let location = || format!("field '{}' of record '{}'", f.name, r.name);
                self.buffered_field(&decl, f, &location);
            }
        }

        for e in &api.enums {
            self.identifier(top, &e.name);
            let decl = within(top, &[&e.name]);
            if e.variants.is_empty() {
                self.push(
                    &decl,
                    ValidationError::EmptyEnum {
                        name: e.name.clone(),
                    },
                );
            }
            let mut names = BTreeSet::new();
            let mut values = BTreeSet::new();
            for (v, value) in e.variants.iter().zip(e.values()) {
                self.identifier(&decl, &v.name);
                if !names.insert(&v.name) {
                    self.push(
                        &decl,
                        ValidationError::DuplicateEnumVariant {
                            enum_name: e.name.clone(),
                            variant: v.name.clone(),
                        },
                    );
                }
                if !values.insert(value) {
                    self.push(
                        &decl,
                        ValidationError::DuplicateEnumValue {
                            enum_name: e.name.clone(),
                            value,
                        },
                    );
                }
                let variant = within(&decl, &[&v.name]);
                let mut fields = BTreeSet::new();
                for f in &v.fields {
                    self.identifier(&variant, &f.name);
                    if !fields.insert(&f.name) {
                        self.push(
                            &variant,
                            ValidationError::DuplicateEnumVariantField {
                                enum_name: e.name.clone(),
                                variant: v.name.clone(),
                                field: f.name.clone(),
                            },
                        );
                    }
                    let location =
                        || format!("field '{}' of variant '{}::{}'", f.name, e.name, v.name);
                    self.buffered_field(&variant, f, &location);
                }
            }
        }

        for i in &api.interfaces {
            self.identifier(top, &i.name);
            self.interface(i);
        }

        for cb in &api.callbacks {
            self.identifier(top, &cb.name);
            self.callback_interface(cb);
        }

        for domain in &api.errors {
            self.error_domain(domain);
        }
    }

    /// A field of a record, a rich-enum variant, or an error payload is
    /// serialized inside a value buffer, so it obeys the buffered positional
    /// rules: no iterators, no callback interfaces, no interface map keys,
    /// and every reference must resolve.
    fn buffered_field(&mut self, scope: &[String], f: &Field, location: &dyn Fn() -> String) {
        let scope = within(scope, &[&f.name]);
        if contains_iterator(&f.ty) {
            self.push(
                &scope,
                ValidationError::IteratorInInvalidPosition {
                    location: location(),
                },
            );
        }
        self.type_ref(&scope, &f.ty);
        self.interface_positions(&scope, &f.ty, location);
        self.no_callback_interface(&scope, &f.ty, location);
    }

    /// Validate an interface's shape: at least one member, unique member
    /// names, constructor restrictions, and per-member signature rules.
    fn interface(&mut self, iface: &InterfaceDef) {
        let decl = vec![iface.name.clone()];
        if iface.constructors.is_empty() && iface.methods.is_empty() && iface.statics.is_empty() {
            self.push(
                &decl,
                ValidationError::EmptyInterface {
                    name: iface.name.clone(),
                },
            );
        }
        let mut names = BTreeSet::new();
        for (f, constructor) in iface.constructors.iter().map(|c| (c, true)).chain(
            iface
                .methods
                .iter()
                .chain(&iface.statics)
                .map(|m| (m, false)),
        ) {
            if !names.insert(&f.name) {
                self.push(
                    &decl,
                    ValidationError::DuplicateInterfaceMember {
                        interface: iface.name.clone(),
                        name: f.name.clone(),
                    },
                );
            }
            let display = format!("{}.{}", iface.name, f.name);
            self.function(&decl, &display, f);
            self.callable_types(&within(&decl, &[&f.name]), &display, f);
            if constructor && f.returns.is_some() {
                self.push(
                    &decl,
                    ValidationError::ConstructorHasReturn {
                        interface: iface.name.clone(),
                        constructor: f.name.clone(),
                    },
                );
            }
            if constructor && f.r#async {
                self.push(
                    &decl,
                    ValidationError::AsyncConstructor {
                        interface: iface.name.clone(),
                        constructor: f.name.clone(),
                    },
                );
            }
        }
    }

    /// Validate a callback interface: at least one method, unique method
    /// names, and per-method restrictions. A callback method is implemented
    /// by the consumer, so it is synchronous, never takes a cancel token,
    /// returns nothing or any value, takes no callback interface or
    /// iterator as a parameter, and may throw only an existing domain (or
    /// `any`).
    fn callback_interface(&mut self, cb: &CallbackDef) {
        let decl = vec![cb.name.clone()];
        if cb.methods.is_empty() {
            self.push(
                &decl,
                ValidationError::EmptyCallbackInterface {
                    name: cb.name.clone(),
                },
            );
        }
        let mut names = BTreeSet::new();
        for m in &cb.methods {
            self.identifier(&decl, &m.name);
            if !names.insert(&m.name) {
                self.push(
                    &decl,
                    ValidationError::DuplicateCallbackMethod {
                        interface: cb.name.clone(),
                        name: m.name.clone(),
                    },
                );
            }
            let reject = |cx: &mut Self, reason: &'static str| {
                cx.push(
                    &decl,
                    ValidationError::InvalidCallbackMethod {
                        interface: cb.name.clone(),
                        method: m.name.clone(),
                        reason,
                    },
                );
            };
            if m.r#async {
                reject(self, "cannot be async");
            }
            if m.cancellable {
                reject(self, "cannot be cancellable");
            }
            let display = format!("{}.{}", cb.name, m.name);
            let method = within(&decl, &[&m.name]);
            self.throws(&method, &display, m);
            if let Some(ret) = &m.returns {
                let location = || format!("return type of {display}");
                if contains_iterator(ret) {
                    reject(self, "cannot return an iterator");
                }
                self.type_ref(&method, ret);
                self.interface_positions(&method, ret, &location);
                self.no_callback_interface(&method, ret, &location);
            }
            self.params(&method, &display, m);
            for p in &m.params {
                let param = within(&method, &[&p.name]);
                let location = || {
                    format!(
                        "param '{}' of callback interface method '{display}'",
                        p.name
                    )
                };
                if contains_iterator(&p.ty) {
                    self.push(
                        &param,
                        ValidationError::IteratorInInvalidPosition {
                            location: location(),
                        },
                    );
                }
                self.type_ref(&param, &p.ty);
                self.interface_positions(&param, &p.ty, &location);
                self.no_callback_interface(&param, &p.ty, &location);
            }
        }
    }

    /// Valid, unique, slot-free parameter names.
    fn params(&mut self, decl: &[String], display: &str, f: &Function) {
        let mut names = BTreeSet::new();
        for p in &f.params {
            self.identifier(decl, &p.name);
            if !names.insert(&p.name) {
                self.push(
                    decl,
                    ValidationError::DuplicateParamName {
                        function: display.to_string(),
                        param: p.name.clone(),
                    },
                );
            }
            if ABI_SLOTS.contains(&p.name.as_str()) {
                self.push(
                    decl,
                    ValidationError::SlotCollision {
                        function: display.to_string(),
                        slot: p.name.clone(),
                    },
                );
            }
        }
    }

    /// Name-level checks for one callable declared in `scope`: a valid
    /// identifier, valid parameter names, and an existing error domain when
    /// the callable throws one.
    fn function(&mut self, scope: &[String], display: &str, f: &Function) {
        self.identifier(scope, &f.name);
        let decl = within(scope, &[&f.name]);
        if f.cancellable && !f.r#async {
            self.push(
                &decl,
                ValidationError::CancellableNotAsync {
                    function: display.to_string(),
                },
            );
        }
        self.throws(&decl, display, f);
        self.params(&decl, display, f);
    }

    /// Check that a callable's `throws` names an error domain or is `any`.
    fn throws(&mut self, scope: &[String], display: &str, f: &Function) {
        if let Some(Throws::Domain(domain)) = &f.throws {
            if self.kind(domain) != Some(TypeKind::ErrorDomain) {
                self.push(
                    scope,
                    ValidationError::UnknownErrorDomain {
                        function: display.to_string(),
                        domain: domain.clone(),
                    },
                );
            }
        }
    }

    /// Type-level checks for one callable's parameters and return: iterator
    /// positions, async-iterator exclusion, reference resolution, element
    /// shapes, interface map keys, and callback-interface positions. `scope`
    /// ends with the callable's own name.
    fn callable_types(&mut self, scope: &[String], display: &str, f: &Function) {
        for p in &f.params {
            let param = within(scope, &[&p.name]);
            let location = || format!("param '{}' of function '{display}'", p.name);
            if contains_iterator(&p.ty) {
                self.push(
                    &param,
                    ValidationError::IteratorInInvalidPosition {
                        location: location(),
                    },
                );
            }
            self.type_ref(&param, &p.ty);
            self.interface_positions(&param, &p.ty, &location);
            // A bare or optional callback interface is the one legal
            // callback position; anything nested (`[Listener]`, a record
            // field) is not.
            let top = match &p.ty {
                TypeRef::Optional(inner) => inner.as_ref(),
                other => other,
            };
            match top {
                TypeRef::Named(name) if self.kind(name) == Some(TypeKind::CallbackInterface) => {}
                _ => self.no_callback_interface(&param, &p.ty, &location),
            }
        }
        if let Some(ret) = &f.returns {
            let location = || format!("return type of {display}");
            if f.r#async && contains_iterator(ret) {
                self.push(
                    scope,
                    ValidationError::AsyncIteratorReturn {
                        function: display.to_string(),
                    },
                );
            }
            // An iterator is valid only as the outermost return shape.
            let nested_iterator = match ret {
                TypeRef::Iterator(elem) => contains_iterator(elem),
                other => contains_iterator(other),
            };
            if nested_iterator {
                self.push(
                    scope,
                    ValidationError::IteratorInInvalidPosition {
                        location: format!("nested inside the {}", location()),
                    },
                );
            }
            self.type_ref(scope, ret);
            self.interface_positions(scope, ret, &location);
            self.no_callback_interface(scope, ret, &location);
        }
    }

    /// Enforce the one place an interface reference may not appear: as a
    /// map key. No target can hash an object by identity in a way that
    /// survives the ABI.
    fn interface_positions(
        &mut self,
        scope: &[String],
        ty: &TypeRef,
        location: &dyn Fn() -> String,
    ) {
        let mut keys = Vec::new();
        ty.walk(&mut |t| {
            if let TypeRef::Map(k, _) = t {
                if let TypeRef::Named(name) = &**k {
                    keys.push(name.clone());
                }
            }
        });
        for name in keys {
            if self.kind(&name) == Some(TypeKind::Interface) {
                self.push(
                    scope,
                    ValidationError::InterfaceInInvalidPosition {
                        name,
                        location: format!("map key of {}", location()),
                    },
                );
            }
        }
    }

    /// Reject an error-code field that carries an interface object, directly
    /// or through a record or rich enum. Error domains are open: a consumer
    /// generated before the code existed discards its payload undecoded, so
    /// an object token inside it would never be released.
    fn no_object_in_payload(&mut self, scope: &[String], f: &Field, location: &dyn Fn() -> String) {
        let mut names = Vec::new();
        f.ty.walk(&mut |t| {
            if let TypeRef::Named(name) = t {
                names.push(name.clone());
            }
        });
        for name in names {
            if self.kind(&name) == Some(TypeKind::Interface) || self.carriers.contains(&name) {
                self.push(
                    &within(scope, &[&f.name]),
                    ValidationError::ObjectInErrorPayload {
                        name,
                        location: location(),
                    },
                );
            }
        }
    }

    /// Reject any callback-interface reference reachable from `ty`. Callers
    /// that permit a bare top-level callback interface unwrap it first.
    fn no_callback_interface(
        &mut self,
        scope: &[String],
        ty: &TypeRef,
        location: &dyn Fn() -> String,
    ) {
        let mut names = Vec::new();
        ty.walk(&mut |t| {
            if let TypeRef::Named(name) = t {
                names.push(name.clone());
            }
        });
        for name in names {
            if self.kind(&name) == Some(TypeKind::CallbackInterface) {
                self.push(
                    scope,
                    ValidationError::CallbackInterfaceInInvalidPosition {
                        name,
                        location: location(),
                    },
                );
            }
        }
    }

    /// Check that every name in `ty` resolves to a declaration, and that
    /// every map key is a legal key type.
    fn type_ref(&mut self, scope: &[String], ty: &TypeRef) {
        match ty {
            TypeRef::Named(name) => {
                let error = if self.kind(name) == Some(TypeKind::ErrorDomain) {
                    ValidationError::ErrorDomainAsType { name: name.clone() }
                } else if self.kind(name).is_some() {
                    return;
                } else if UNSUPPORTED_PRIMITIVES.contains(&name.as_str()) {
                    ValidationError::UnsupportedPrimitive { name: name.clone() }
                } else {
                    ValidationError::UnknownTypeRef { name: name.clone() }
                };
                self.push(scope, error);
            }
            TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
                self.type_ref(scope, inner);
            }
            TypeRef::Map(k, v) => {
                if !self.is_map_key(k) {
                    self.push(
                        scope,
                        ValidationError::InvalidMapKey {
                            key_type: k.to_string(),
                        },
                    );
                }
                self.type_ref(scope, k);
                self.type_ref(scope, v);
            }
            TypeRef::Prim(_) => {}
        }
    }

    /// May `ty` be a map key? Only integers, bools, strings, and C-style
    /// enums: every target must use the key in its native dictionary with
    /// exact equality.
    fn is_map_key(&self, ty: &TypeRef) -> bool {
        match ty {
            TypeRef::Prim(p) => p.is_integer() || matches!(p, Prim::Bool | Prim::String),
            TypeRef::Named(name) => self.kind(name) == Some(TypeKind::Enum),
            _ => false,
        }
    }

    fn error_domain(&mut self, domain: &ErrorDomain) {
        let top: &[String] = &[];
        self.identifier(top, &domain.name);
        if domain.name == crate::ir::THROWS_ANY {
            // `throws: any` means an untyped error, so no domain can be
            // named `any`.
            self.push(
                top,
                ValidationError::ReservedKeyword {
                    name: domain.name.clone(),
                },
            );
        }
        let decl = within(top, &[&domain.name]);
        let mut values = BTreeSet::new();
        for c in &domain.codes {
            self.identifier(&decl, &c.name);
            // 0 means success and the whole negative range is reserved for
            // the runtime, so domain codes must be positive.
            if c.code <= 0 {
                self.push(
                    &decl,
                    ValidationError::InvalidErrorCode {
                        domain: domain.name.clone(),
                        name: c.name.clone(),
                    },
                );
            }
            if !values.insert(c.code) {
                self.push(
                    &decl,
                    ValidationError::DuplicateErrorCode {
                        domain: domain.name.clone(),
                        value: c.code,
                    },
                );
            }
            let code = within(&decl, &[&c.name]);
            for f in &c.fields {
                let location = || {
                    format!(
                        "payload field '{}' of error code '{}.{}'",
                        f.name, domain.name, c.name
                    )
                };
                self.buffered_field(&code, f, &location);
                self.no_object_in_payload(&code, f, &location);
            }
        }
    }
}

/// Every record and rich enum whose fields reach an interface object, directly
/// or through other records and rich enums (a fixed point over the whole API,
/// since types may reference each other in any order).
fn object_carriers(api: &Api, types: &TypeIndex) -> BTreeSet<String> {
    let mut decls: Vec<(&str, Vec<&TypeRef>)> = Vec::new();
    for r in &api.records {
        decls.push((&r.name, r.fields.iter().map(|f| &f.ty).collect()));
    }
    for e in &api.enums {
        let fields = e.variants.iter().flat_map(|v| &v.fields);
        decls.push((&e.name, fields.map(|f| &f.ty).collect()));
    }
    let mut carriers = BTreeSet::new();
    loop {
        let before = carriers.len();
        for (name, fields) in &decls {
            if carriers.contains(*name) {
                continue;
            }
            let mut carries = false;
            for ty in fields {
                ty.walk(&mut |t| {
                    if let TypeRef::Named(n) = t {
                        carries |=
                            types.kind(n) == Some(TypeKind::Interface) || carriers.contains(n);
                    }
                });
            }
            if carries {
                carriers.insert((*name).to_string());
            }
        }
        if carriers.len() == before {
            return carriers;
        }
    }
}

fn contains_iterator(ty: &TypeRef) -> bool {
    let mut found = false;
    ty.walk(&mut |t| found |= matches!(t, TypeRef::Iterator(_)));
    found
}
