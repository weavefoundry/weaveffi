//! Non-fatal, lint-style checks over a validated [`Api`].
//!
//! These are distinct from the hard validation errors in the parent
//! [`crate::validate`] module: errors *reject* an IDL, whereas warnings
//! merely flag stylistic or ergonomic concerns (deep nesting, undocumented
//! modules, no-op `mutable` flags, …) that the caller can surface and the
//! user can choose to ignore.

use weaveffi_ir::ir::{Api, TypeRef};

/// A non-fatal advisory emitted by [`collect_warnings`].
#[derive(Debug, Clone)]
pub enum ValidationWarning {
    /// An enum has an unusually large number of variants (more than 100).
    LargeEnumVariantCount {
        /// Enum that tripped the threshold.
        enum_name: String,
        /// Number of variants the enum declares.
        count: usize,
    },
    /// A type is nested more deeply than recommended (more than 3 levels).
    DeepNesting {
        /// Where the deeply nested type appears (a `module::fn::param` path).
        location: String,
        /// Measured nesting depth.
        depth: usize,
    },
    /// A module has functions but none of them carry a doc comment.
    EmptyModuleDoc {
        /// Module with no documented functions.
        module: String,
    },
    /// An async function declares no return type, which is unusual.
    AsyncVoidFunction {
        /// Module that contains the function.
        module: String,
        /// Async function with no return type.
        function: String,
    },
    /// A `mutable` flag sits on a value-type parameter, where it has no effect.
    MutableOnValueType {
        /// Module that contains the function.
        module: String,
        /// Function that declares the parameter.
        function: String,
        /// Parameter that carries the no-op `mutable` flag.
        param: String,
    },
    /// A function is marked deprecated; the message is surfaced to consumers.
    DeprecatedFunction {
        /// Module that contains the function.
        module: String,
        /// Deprecated function.
        function: String,
        /// Deprecation message declared in the IDL.
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

/// Walk every module and collect all advisory warnings for `api`.
///
/// Assumes `api` has already passed hard validation; it does not re-check
/// structural invariants.
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
