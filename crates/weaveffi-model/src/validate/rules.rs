//! The validation rules: global name uniqueness, per-module identifier and
//! shape checks, type-reference resolution, ABI-representability of element
//! shapes, callback-interface rules, interface positions, and error-domain
//! consistency.
//!
//! Every rule pushes into a shared sink instead of returning early, so one
//! validation pass reports every violation in the document. Each violation
//! carries the declaration path that encloses it (module segments, then
//! declaration and member names), which the diagnostic uses to locate the
//! offending text.

use super::{Found, Options, ValidationError};
use crate::ir::{
    Api, CallbackInterfaceDef, ErrorDomain, Function, InterfaceDef, Module, StructField, TypeRef,
};
use crate::ty::{Prim, TypeIndex, TypeKind};
use std::collections::{BTreeMap, BTreeSet};

const RESERVED: &[&str] = &[
    "if", "else", "for", "while", "loop", "match", "type", "return", "async", "await", "break",
    "continue", "fn", "struct", "enum", "mod", "use",
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
pub(super) fn check(api: &Api, types: &TypeIndex, options: Options, found: &mut Vec<Found>) {
    let mut cx = Cx {
        types,
        options,
        found,
    };
    let mut names = BTreeSet::new();
    for m in &api.modules {
        if !names.insert(m.name.as_str()) {
            cx.push(
                &within(&[], &[&m.name]),
                ValidationError::DuplicateModuleName {
                    module: m.name.clone(),
                },
            );
        }
        cx.module(m, &[], false);
    }
    check_global_names(&api.modules, found);
}

/// Enforce API-wide name uniqueness: every type name (records, enums,
/// interfaces, callback interfaces, and error domains), every free-function
/// name, and every error-code name is global. A free function also may not
/// share its name with an error domain.
fn check_global_names(modules: &[Module], found: &mut Vec<Found>) {
    #[derive(Default)]
    struct Seen<'a> {
        types: BTreeMap<&'a str, String>,
        functions: BTreeMap<&'a str, String>,
        codes: BTreeMap<&'a str, String>,
        domains: BTreeMap<&'a str, String>,
        /// Every free function, as `(name, module path, module segments)`.
        all_functions: Vec<(&'a str, String, Vec<String>)>,
    }
    fn walk<'a>(
        modules: &'a [Module],
        parent: &[String],
        seen: &mut Seen<'a>,
        found: &mut Vec<Found>,
    ) {
        for m in modules {
            let segments = within(parent, &[&m.name]);
            let path = segments.join(".");
            let types = m
                .structs
                .iter()
                .map(|s| s.name.as_str())
                .chain(m.enums.iter().map(|e| e.name.as_str()))
                .chain(m.interfaces.iter().map(|i| i.name.as_str()))
                .chain(m.callback_interfaces.iter().map(|c| c.name.as_str()))
                .chain(m.errors.iter().map(|d| d.name.as_str()));
            for name in types {
                match seen.types.get(name) {
                    Some(first) => found.push((
                        ValidationError::DuplicateTypeName {
                            name: name.to_string(),
                            first: first.clone(),
                            second: path.clone(),
                        },
                        segments.clone(),
                    )),
                    None => {
                        seen.types.insert(name, path.clone());
                    }
                }
            }
            for f in &m.functions {
                match seen.functions.get(f.name.as_str()) {
                    Some(first) => found.push((
                        ValidationError::DuplicateFunctionName {
                            name: f.name.clone(),
                            first: first.clone(),
                            second: path.clone(),
                        },
                        segments.clone(),
                    )),
                    None => {
                        seen.functions.insert(&f.name, path.clone());
                    }
                }
                seen.all_functions
                    .push((&f.name, path.clone(), segments.clone()));
            }
            if let Some(domain) = &m.errors {
                seen.domains.entry(&domain.name).or_insert(path.clone());
                let owner = format!("{path}.{}", domain.name);
                for code in &domain.codes {
                    match seen.codes.get(code.name.as_str()) {
                        Some(first) => found.push((
                            ValidationError::DuplicateErrorCodeName {
                                name: code.name.clone(),
                                first: first.clone(),
                                second: owner.clone(),
                            },
                            within(&segments, &[&domain.name]),
                        )),
                        None => {
                            seen.codes.insert(&code.name, owner.clone());
                        }
                    }
                }
            }
            walk(&m.modules, &segments, seen, found);
        }
    }
    let mut seen = Seen::default();
    walk(modules, &[], &mut seen, found);
    for (name, function, segments) in &seen.all_functions {
        if let Some(domain) = seen.domains.get(name) {
            found.push((
                ValidationError::NameCollisionWithErrorDomain {
                    name: (*name).to_string(),
                    function: function.clone(),
                    domain: domain.clone(),
                },
                segments.clone(),
            ));
        }
    }
}

/// The rule context: the type index, the options, and the violation sink.
struct Cx<'a> {
    types: &'a TypeIndex,
    options: Options,
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

    fn module(&mut self, module: &Module, parent: &[String], ancestor_has_domain: bool) {
        if module.name.trim().is_empty() {
            self.push(parent, ValidationError::NoModuleName);
            return;
        }
        if !is_valid_identifier(&module.name) {
            self.push(
                parent,
                ValidationError::InvalidModuleName {
                    name: module.name.clone(),
                    reason: IDENTIFIER_RULE,
                },
            );
        } else if RESERVED.contains(&module.name.as_str()) {
            self.push(
                parent,
                ValidationError::InvalidModuleName {
                    name: module.name.clone(),
                    reason: "reserved word",
                },
            );
        }
        let scope = within(parent, &[&module.name]);
        let path = scope.join(".");
        let has_domain = ancestor_has_domain || module.errors.is_some();

        for f in &module.functions {
            self.function(&scope, &path, &f.name, f, has_domain);
            self.callable_types(&within(&scope, &[&f.name]), &path, &f.name, f);
        }

        for s in &module.structs {
            self.identifier(&scope, &s.name);
            if s.fields.is_empty() {
                self.push(
                    &scope,
                    ValidationError::EmptyStruct {
                        module: path.clone(),
                        name: s.name.clone(),
                    },
                );
            }
            let decl = within(&scope, &[&s.name]);
            let mut names = BTreeSet::new();
            for f in &s.fields {
                self.identifier(&decl, &f.name);
                if !names.insert(&f.name) {
                    self.push(
                        &decl,
                        ValidationError::DuplicateStructField {
                            struct_name: s.name.clone(),
                            field: f.name.clone(),
                        },
                    );
                }
                let location = || format!("field '{}' of struct '{}'", f.name, s.name);
                self.buffered_field(&decl, f, &location);
            }
        }

        for e in &module.enums {
            self.identifier(&scope, &e.name);
            if e.variants.is_empty() {
                self.push(
                    &scope,
                    ValidationError::EmptyEnum {
                        module: path.clone(),
                        name: e.name.clone(),
                    },
                );
            }
            let decl = within(&scope, &[&e.name]);
            let mut names = BTreeSet::new();
            let mut values = BTreeSet::new();
            for v in &e.variants {
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
                if !values.insert(v.value) {
                    self.push(
                        &scope,
                        ValidationError::DuplicateEnumValue {
                            enum_name: e.name.clone(),
                            value: v.value,
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

        for i in &module.interfaces {
            self.identifier(&scope, &i.name);
            self.interface(&scope, &path, i, has_domain);
        }

        for cb in &module.callback_interfaces {
            self.identifier(&scope, &cb.name);
            self.callback_interface(&scope, &path, cb, has_domain);
        }

        if let Some(domain) = &module.errors {
            self.error_domain(&scope, &path, domain);
        }

        let mut names = BTreeSet::new();
        for sub in &module.modules {
            if !names.insert(sub.name.as_str()) {
                self.push(
                    &within(&scope, &[&sub.name]),
                    ValidationError::DuplicateModuleName {
                        module: format!("{path}.{}", sub.name),
                    },
                );
            }
            self.module(sub, &scope, has_domain);
        }
    }

    /// A field of a record, a rich-enum variant, or an error payload is
    /// serialized inside a value buffer, so it obeys the buffered positional
    /// rules: no iterators, no callback interfaces, no interface map keys,
    /// and every reference must resolve.
    fn buffered_field(&mut self, scope: &[String], f: &StructField, location: &dyn Fn() -> String) {
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

    /// Validate an interface's shape: unique member names across
    /// constructors, methods, and statics; constructor restrictions;
    /// per-member signature rules. C symbol collisions are checked API-wide
    /// once the model is built.
    fn interface(&mut self, scope: &[String], path: &str, iface: &InterfaceDef, has_domain: bool) {
        if iface.constructors.is_empty() && iface.methods.is_empty() && iface.statics.is_empty() {
            self.push(
                scope,
                ValidationError::EmptyInterface {
                    module: path.to_string(),
                    name: iface.name.clone(),
                },
            );
        }
        let decl = within(scope, &[&iface.name]);
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
            self.function(&decl, path, &display, f, has_domain);
            self.callable_types(&within(&decl, &[&f.name]), path, &display, f);
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
    /// returns nothing or any value but an iterator or a callback interface,
    /// takes no callback interface or iterator as a parameter, and may
    /// declare `throws` only when an error domain is in scope.
    fn callback_interface(
        &mut self,
        scope: &[String],
        path: &str,
        cb: &CallbackInterfaceDef,
        has_domain: bool,
    ) {
        if cb.methods.is_empty() {
            self.push(
                scope,
                ValidationError::EmptyCallbackInterface {
                    module: path.to_string(),
                    name: cb.name.clone(),
                },
            );
        }
        let decl = within(scope, &[&cb.name]);
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
            let method = within(&decl, &[&m.name]);
            if m.throws && !has_domain {
                self.push(
                    &method,
                    ValidationError::ThrowsWithoutErrorDomain {
                        module: path.to_string(),
                        function: format!("{}.{}", cb.name, m.name),
                    },
                );
            }
            if let Some(ret) = &m.returns {
                let location = || format!("return type of {path}::{}.{}", cb.name, m.name);
                if contains_iterator(ret) {
                    reject(self, "cannot return an iterator");
                }
                self.type_ref(&method, ret);
                self.interface_positions(&method, ret, &location);
                self.no_callback_interface(&method, ret, &location);
            }
            let mut params = BTreeSet::new();
            for p in &m.params {
                self.identifier(&method, &p.name);
                if !params.insert(&p.name) {
                    self.push(
                        &method,
                        ValidationError::DuplicateParamName {
                            module: path.to_string(),
                            function: format!("{}.{}", cb.name, m.name),
                            param: p.name.clone(),
                        },
                    );
                }
                let param = within(&method, &[&p.name]);
                let location = || {
                    format!(
                        "param '{}' of callback interface method '{}.{}'",
                        p.name, cb.name, m.name
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

    /// Name-level checks for one callable declared in `scope`: a valid
    /// identifier, unique parameter names, and an error domain in scope when
    /// the callable declares `throws`.
    fn function(
        &mut self,
        scope: &[String],
        path: &str,
        display: &str,
        f: &Function,
        has_domain: bool,
    ) {
        self.identifier(scope, &f.name);
        let decl = within(scope, &[&f.name]);
        if f.cancellable && !f.r#async {
            self.push(
                &decl,
                ValidationError::CancellableNotAsync {
                    module: path.to_string(),
                    function: display.to_string(),
                },
            );
        }
        if f.throws && !has_domain {
            self.push(
                &decl,
                ValidationError::ThrowsWithoutErrorDomain {
                    module: path.to_string(),
                    function: display.to_string(),
                },
            );
        }
        let mut names = BTreeSet::new();
        for p in &f.params {
            self.identifier(&decl, &p.name);
            if !names.insert(&p.name) {
                self.push(
                    &decl,
                    ValidationError::DuplicateParamName {
                        module: path.to_string(),
                        function: display.to_string(),
                        param: p.name.clone(),
                    },
                );
            }
        }
    }

    /// Type-level checks for one callable's parameters and return: iterator
    /// positions, async-iterator exclusion, reference resolution, element
    /// shapes, interface map keys, and callback-interface positions. `scope`
    /// ends with the callable's own name.
    fn callable_types(&mut self, scope: &[String], path: &str, display: &str, f: &Function) {
        for p in &f.params {
            let param = within(scope, &[&p.name]);
            let location = || format!("param '{}' of function '{path}::{display}'", p.name);
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
            let location = || format!("return type of {path}::{display}");
            // An async function completes through a one-shot callback; an
            // iterator needs a pull-based handle. The two shapes cannot
            // compose on the C ABI, so reject the combination up front
            // instead of letting backends lower it inconsistently.
            if f.r#async && contains_iterator(ret) {
                self.push(
                    scope,
                    ValidationError::AsyncIteratorReturn {
                        module: path.to_string(),
                        function: display.to_string(),
                    },
                );
            }
            // An iterator is a pull handle, valid only as the outermost
            // return shape: `[iter<T>]`, `iter<T>?`, `iter<iter<T>>`, and
            // iterators inside a map have no lowering.
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
    /// map key. Objects are reference-counted tokens, so they compose with
    /// every other buffered shape (fields, elements, map values, optionals),
    /// but no target can hash an object by identity in a way that survives
    /// the ABI.
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
                let error = if name.contains('.') {
                    ValidationError::QualifiedTypeRef { name: name.clone() }
                } else if self.kind(name).is_some() {
                    return;
                } else if UNSUPPORTED_PRIMITIVES.contains(&name.as_str()) {
                    ValidationError::UnsupportedPrimitive { name: name.clone() }
                } else if self.options.foreign_names {
                    // A record or rich enum from another module tree.
                    return;
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

    /// May `ty` be a map key? Every target must be able to use the key in
    /// its native dictionary idiom with exact equality, so only integers,
    /// bools, strings, and C-style enums qualify; floats (NaN and signed zero
    /// break key equality in several languages), composites, optionals,
    /// bytes, and objects are rejected.
    fn is_map_key(&self, ty: &TypeRef) -> bool {
        match ty {
            TypeRef::Prim(p) => p.is_integer() || matches!(p, Prim::Bool | Prim::String),
            TypeRef::Named(name) => self.kind(name) == Some(TypeKind::Enum),
            _ => false,
        }
    }

    fn error_domain(&mut self, scope: &[String], path: &str, domain: &ErrorDomain) {
        if domain.name.trim().is_empty() {
            self.push(
                scope,
                ValidationError::ErrorDomainMissingName {
                    module: path.to_string(),
                },
            );
            return;
        }
        self.identifier(scope, &domain.name);
        let decl = within(scope, &[&domain.name]);
        let mut values = BTreeSet::new();
        for c in &domain.codes {
            self.identifier(&decl, &c.name);
            // 0 means success and the whole negative range is reserved for
            // the runtime (-1 generic error, -2 panic, -3 marshalling
            // failure, -4 foreign callback error, -5 cancelled, and room to
            // grow), so domain codes must be positive.
            if c.code <= 0 {
                self.push(
                    &decl,
                    ValidationError::InvalidErrorCode {
                        module: path.to_string(),
                        name: c.name.clone(),
                    },
                );
            }
            if !values.insert(c.code) {
                self.push(
                    &decl,
                    ValidationError::DuplicateErrorCode {
                        module: path.to_string(),
                        value: c.code,
                    },
                );
            }
            let code = within(&decl, &[&c.name]);
            for f in &c.fields {
                let location = || {
                    format!(
                        "payload field '{}' of error code '{}::{}'",
                        f.name, domain.name, c.name
                    )
                };
                self.buffered_field(&code, f, &location);
            }
        }
    }
}

fn contains_iterator(ty: &TypeRef) -> bool {
    let mut found = false;
    ty.walk(&mut |t| found |= matches!(t, TypeRef::Iterator(_)));
    found
}
