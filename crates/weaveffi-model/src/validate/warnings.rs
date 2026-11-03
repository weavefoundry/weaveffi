//! Non-fatal, lint-style checks over a validated [`Api`].
//!
//! These are distinct from the hard validation errors in the parent
//! [`crate::validate`] module: errors *reject* an API, whereas warnings flag
//! ergonomic concerns (deep nesting, huge enums, undocumented declarations)
//! that `weaveffi validate --warn` surfaces and the user may ignore.

use crate::ir::{Api, Function, TypeRef};

/// Nesting deeper than this (`[[[[i32]]]]`) is flagged.
const MAX_NESTING: usize = 3;

/// Enums with more variants than this are flagged.
const MAX_ENUM_VARIANTS: usize = 100;

/// A non-fatal advisory emitted by [`collect_warnings`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "code")]
pub enum ValidationWarning {
    /// An enum has more than 100 variants.
    LargeEnumVariantCount {
        /// The enum.
        enum_name: String,
        /// How many variants it declares.
        count: usize,
    },
    /// A type is nested more than 3 levels deep.
    DeepNesting {
        /// Where the type appears (`Store.get.key`, `Entry.tags`).
        location: String,
        /// The nesting depth.
        depth: usize,
    },
    /// A top-level declaration has no documentation, so its bindings have
    /// no doc comment.
    Undocumented {
        /// The declaration.
        name: String,
    },
}

impl std::fmt::Display for ValidationWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LargeEnumVariantCount { enum_name, count } => write!(
                f,
                "enum '{enum_name}' has {count} variants (more than {MAX_ENUM_VARIANTS})"
            ),
            Self::DeepNesting { location, depth } => write!(
                f,
                "deep type nesting at {location} (depth {depth}, at most {MAX_NESTING} \
                 recommended)"
            ),
            Self::Undocumented { name } => write!(f, "'{name}' has no documentation"),
        }
    }
}

impl ValidationWarning {
    /// The warning's code, the variant name (`--format json` reports it).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::LargeEnumVariantCount { .. } => "LargeEnumVariantCount",
            Self::DeepNesting { .. } => "DeepNesting",
            Self::Undocumented { .. } => "Undocumented",
        }
    }
}

/// Collect every advisory warning for `api`, which has passed validation.
#[must_use]
pub fn collect_warnings(api: &Api) -> Vec<ValidationWarning> {
    let mut warnings = Vec::new();
    for e in &api.enums {
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
    for r in &api.records {
        for field in &r.fields {
            deep(&field.ty, format!("{}.{}", r.name, field.name));
        }
    }
    let mut callable = |owner: Option<&str>, f: &Function| {
        let path = owner.map_or_else(|| f.name.clone(), |o| format!("{o}.{}", f.name));
        for p in &f.params {
            deep(&p.ty, format!("{path}.{}", p.name));
        }
        if let Some(ret) = &f.returns {
            deep(ret, format!("{path} (return)"));
        }
    };
    for f in &api.functions {
        callable(None, f);
    }
    for i in &api.interfaces {
        for f in i.constructors.iter().chain(&i.methods).chain(&i.statics) {
            callable(Some(&i.name), f);
        }
    }
    let docs = api
        .records
        .iter()
        .map(|d| (&d.name, &d.doc))
        .chain(api.enums.iter().map(|d| (&d.name, &d.doc)))
        .chain(api.errors.iter().map(|d| (&d.name, &d.doc)))
        .chain(api.interfaces.iter().map(|d| (&d.name, &d.doc)))
        .chain(api.callbacks.iter().map(|d| (&d.name, &d.doc)))
        .chain(api.functions.iter().map(|d| (&d.name, &d.doc)));
    for (name, doc) in docs {
        if doc.is_none() {
            warnings.push(ValidationWarning::Undocumented { name: name.clone() });
        }
    }
    warnings
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_nesting_large_enums_and_missing_docs_warn() {
        let api: Api = serde_json::from_str(
            r#"{"version":"0.13.0",
                "records":[{"name":"R","doc":"r","fields":[{"name":"x","type":"[[[[i32]]]]"}]}],
                "functions":[{"name":"f"}]}"#,
        )
        .unwrap();
        let warnings = collect_warnings(&api);
        assert_eq!(
            warnings,
            [
                ValidationWarning::DeepNesting {
                    location: "R.x".into(),
                    depth: 4
                },
                ValidationWarning::Undocumented { name: "f".into() },
            ]
        );
        assert_eq!(warnings[1].code(), "Undocumented");
    }
}
