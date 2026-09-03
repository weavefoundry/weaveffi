//! Non-fatal, lint-style checks over a validated [`Api`].
//!
//! These are distinct from the hard validation errors in the parent
//! [`crate::validate`] module: errors *reject* an IDL, whereas warnings
//! merely flag stylistic or ergonomic concerns (deep nesting, undocumented
//! modules, async functions with no result) that the caller can surface and
//! the user can choose to ignore.

use weaveffi_ir::ir::{Api, Module, TypeRef};

/// Nesting deeper than this (`[[[[i32]]]]`) is flagged.
const MAX_NESTING: usize = 3;

/// Enums with more variants than this are flagged.
const MAX_ENUM_VARIANTS: usize = 100;

/// A non-fatal advisory emitted by [`collect_warnings`].
#[derive(Debug, Clone, PartialEq, Eq)]
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
    /// A module declares functions but carries no doc comment on itself or
    /// on any of them.
    EmptyModuleDoc {
        /// Module with no documentation.
        module: String,
    },
    /// An async function declares no return type, which is unusual.
    AsyncVoidFunction {
        /// Module that contains the function.
        module: String,
        /// Async function with no return type.
        function: String,
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
                write!(
                    f,
                    "enum '{enum_name}' has {count} variants (>{MAX_ENUM_VARIANTS})"
                )
            }
            Self::DeepNesting { location, depth } => write!(
                f,
                "deep type nesting at {location} (depth {depth}, max recommended {MAX_NESTING})"
            ),
            Self::EmptyModuleDoc { module } => {
                write!(
                    f,
                    "module '{module}' has no doc comment on itself or any function"
                )
            }
            Self::AsyncVoidFunction { module, function } => write!(
                f,
                "async function {module}::{function} has no return type; async void is unusual"
            ),
            Self::DeprecatedFunction {
                module,
                function,
                message,
            } => write!(f, "function {module}::{function} is deprecated: {message}"),
        }
    }
}

/// Walk every module (including nested submodules) and collect all advisory
/// warnings for `api`.
///
/// Assumes `api` has already passed hard validation; it does not re-check
/// structural invariants.
pub fn collect_warnings(api: &Api) -> Vec<ValidationWarning> {
    let mut warnings = Vec::new();
    for module in &api.modules {
        collect_module(module, &mut warnings);
    }
    warnings
}

fn collect_module(module: &Module, warnings: &mut Vec<ValidationWarning>) {
    let name = &module.name;
    for e in &module.enums {
        if e.variants.len() > MAX_ENUM_VARIANTS {
            warnings.push(ValidationWarning::LargeEnumVariantCount {
                enum_name: e.name.clone(),
                count: e.variants.len(),
            });
        }
    }
    let mut deep = |ty: &TypeRef, location: String| {
        let depth = nesting_depth(ty);
        if depth > MAX_NESTING {
            warnings.push(ValidationWarning::DeepNesting { location, depth });
        }
    };
    for s in &module.structs {
        for field in &s.fields {
            deep(&field.ty, format!("{name}::{}::{}", s.name, field.name));
        }
    }
    for f in &module.functions {
        for p in &f.params {
            deep(&p.ty, format!("{name}::{}::{}", f.name, p.name));
        }
        if let Some(ret) = &f.returns {
            deep(ret, format!("{name}::{}::return", f.name));
        }
    }
    for f in &module.functions {
        if f.r#async && f.returns.is_none() {
            warnings.push(ValidationWarning::AsyncVoidFunction {
                module: name.clone(),
                function: f.name.clone(),
            });
        }
        if let Some(msg) = &f.deprecated {
            warnings.push(ValidationWarning::DeprecatedFunction {
                module: name.clone(),
                function: f.name.clone(),
                message: msg.clone(),
            });
        }
    }
    if module.doc.is_none()
        && !module.functions.is_empty()
        && module.functions.iter().all(|f| f.doc.is_none())
    {
        warnings.push(ValidationWarning::EmptyModuleDoc {
            module: name.clone(),
        });
    }
    for child in &module.modules {
        collect_module(child, warnings);
    }
}

fn nesting_depth(ty: &TypeRef) -> usize {
    match ty {
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            1 + nesting_depth(inner)
        }
        TypeRef::Map(k, v) => 1 + nesting_depth(k).max(nesting_depth(v)),
        _ => 0,
    }
}
