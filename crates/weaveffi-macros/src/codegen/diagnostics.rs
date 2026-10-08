//! Turn validation errors into compile errors on the offending Rust item.
//!
//! The macro validates its module tree with the same rules the CLI applies
//! to an IDL. Each error comes with its declaration path (module segments,
//! then declaration, member, and parameter or field names), and the
//! extraction's [`SourceMap`] records the span of every such path, so the
//! error lands on the item, member, or written type at fault.

use proc_macro2::Span;
use weaveffi_model::rust::SourceMap;
use weaveffi_model::validate::{Found, ValidationError};

/// What to look for under an error's declaration path.
struct Target<'a> {
    /// A member named by the error, appended to the path.
    member: Option<&'a str>,
    /// The error is about a written type: prefer the type's span.
    typed: bool,
    /// The error is about a return type.
    returned: bool,
    /// The error is about a duplicate: point at the last declaration.
    last: bool,
}

fn target(error: &ValidationError) -> Target<'_> {
    use ValidationError as E;
    let mut t = Target {
        member: None,
        typed: false,
        returned: false,
        last: false,
    };
    match error {
        E::DuplicateParamName { param: name, .. }
        | E::DuplicateStructField { field: name, .. }
        | E::DuplicateEnumVariant { variant: name, .. }
        | E::DuplicateEnumVariantField { field: name, .. }
        | E::DuplicateInterfaceMember { name, .. }
        | E::DuplicateCallbackMethod { name, .. }
        | E::DuplicateTypeName { name, .. }
        | E::DuplicateFunctionName { name, .. }
        | E::DuplicateErrorCodeName { name, .. } => {
            t.member = Some(name);
            t.last = true;
        }
        E::InvalidIdentifier { name, .. }
        | E::ReservedKeyword { name }
        | E::EmptyStruct { name, .. }
        | E::EmptyEnum { name, .. }
        | E::EmptyInterface { name, .. }
        | E::EmptyCallbackInterface { name, .. }
        | E::NameCollisionWithErrorDomain { name, .. }
        | E::InvalidErrorCode { name, .. } => t.member = Some(name),
        E::DuplicateEnumValue { enum_name, .. } => t.member = Some(enum_name),
        E::ConstructorHasReturn { constructor, .. } | E::AsyncConstructor { constructor, .. } => {
            t.member = Some(constructor);
        }
        E::InvalidCallbackMethod { method, reason, .. } => {
            t.member = Some(method);
            t.returned = reason.contains("return");
        }
        E::AsyncIteratorReturn { .. } => t.returned = true,
        E::UnknownTypeRef { .. }
        | E::QualifiedTypeRef { .. }
        | E::UnsupportedPrimitive { .. }
        | E::InvalidMapKey { .. }
        | E::InterfaceInInvalidPosition { .. }
        | E::CallbackInterfaceInInvalidPosition { .. }
        | E::IteratorInInvalidPosition { .. } => t.typed = true,
        _ => {}
    }
    t
}

/// The span `error`, reported under the declaration path `scope`, belongs
/// to: the most specific recorded path, falling back to enclosing ones.
fn locate(error: &ValidationError, scope: &[String], source: &SourceMap) -> Option<Span> {
    let t = target(error);
    let mut base = scope.to_vec();
    if let Some(member) = t.member {
        let with = [scope, &[member.to_string()]].concat();
        if !source.spans(&with).is_empty() {
            base = with;
        }
    }
    let mut candidates = Vec::new();
    if t.typed {
        candidates.push([&base[..], &[SourceMap::TYPE.to_string()]].concat());
    }
    if t.typed || t.returned {
        candidates.push([&base[..], &[SourceMap::RETURN.to_string()]].concat());
    }
    for n in (1..=base.len()).rev() {
        candidates.push(base[..n].to_vec());
    }
    candidates.iter().find_map(|path| {
        let spans = source.spans(path);
        if t.last {
            spans.last().copied()
        } else {
            spans.first().copied()
        }
    })
}

/// The fix for the errors a Rust producer most often meets, phrased in Rust
/// terms (the IDL help text speaks of `errors:` blocks and IDL spellings).
fn hint(error: &ValidationError) -> Option<&'static str> {
    use ValidationError as E;
    Some(match error {
        E::UnsupportedPrimitive { .. } => {
            "use `u64` or `i64` for sizes and counts (their width is the same on every \
             platform), or `String` for a `char`"
        }
        E::ThrowsWithoutErrorDomain { .. } => {
            "declare a #[weaveffi::error] enum in this module or a parent module"
        }
        _ => return None,
    })
}

/// One compile error per validation error, each at its item's span (or at
/// `fallback`, the module's name, when nothing more specific is known).
pub(crate) fn validation_errors(
    found: Vec<Found>,
    source: &SourceMap,
    fallback: Span,
) -> syn::Error {
    let mut errors = found.into_iter().map(|(error, scope)| {
        let span = locate(&error, &scope, source).unwrap_or(fallback);
        let message = match hint(&error) {
            Some(hint) => format!("weaveffi: {error}; {hint}"),
            None => format!("weaveffi: {error}"),
        };
        syn::Error::new(span, message)
    });
    let mut first = errors
        .next()
        .unwrap_or_else(|| syn::Error::new(fallback, "weaveffi: invalid module"));
    for e in errors {
        first.combine(e);
    }
    first
}
