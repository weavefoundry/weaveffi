//! [`ValidationDiagnostic`]: a [`ValidationError`] plus an optional source
//! snippet and best-effort span for fancy miette rendering.
//!
//! The span is located within the error's enclosing declaration: the
//! validator records the declaration path (module segments, then declaration
//! and member names), and the search walks that path through the source,
//! finding each name as the value of a `name` key, before looking for the
//! offending text. So a duplicate parameter `x` underlines the second `x`
//! of the right function, not the first `x` anywhere in the file.
//!
//! The source snippet, the span, and the miette `Diagnostic` impl are only
//! available with the `idl` feature; without it the wrapper carries the
//! error alone.

use super::ValidationError;
#[cfg(feature = "idl")]
use miette::{Diagnostic, NamedSource, SourceSpan};

/// Diagnostic wrapper that attaches an optional source code snippet and a
/// best-effort byte range to a [`ValidationError`] for fancy rendering via
/// miette. The wrapper delegates `help()` and `code()` to the inner error
/// while exposing its own `source_code` and `labels` so the renderer can
/// underline the offending identifier in the input.
#[derive(Debug)]
pub struct ValidationDiagnostic {
    /// The underlying validation error being rendered.
    pub error: ValidationError,
    /// Named source snippet (filename plus contents), when an on-disk IDL was
    /// supplied. `None` for in-memory APIs.
    #[cfg(feature = "idl")]
    pub src: Option<NamedSource<String>>,
    /// Best-effort byte range of the offending identifier within `src`, used
    /// to underline it. `None` when no span could be located.
    #[cfg(feature = "idl")]
    pub span: Option<SourceSpan>,
}

impl std::fmt::Display for ValidationDiagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for ValidationDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

#[cfg(feature = "idl")]
impl Diagnostic for ValidationDiagnostic {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.error.code()
    }

    fn severity(&self) -> Option<miette::Severity> {
        self.error.severity()
    }

    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.error.help()
    }

    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.error.url()
    }

    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        self.src
            .as_ref()
            .map(|s| s as &dyn miette::SourceCode)
            .or_else(|| self.error.source_code())
    }

    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        if let Some(span) = self.span {
            Some(Box::new(std::iter::once(
                miette::LabeledSpan::new_with_span(Some("here".to_string()), span),
            )))
        } else {
            self.error.labels()
        }
    }
}

impl ValidationDiagnostic {
    /// Wrap `error`, located within the declaration path `scope`, with an
    /// optional `(filename, contents)` source. When a source is provided the
    /// constructor searches for the offending text within the enclosing
    /// declaration and attaches a [`SourceSpan`] for fancy rendering. If no
    /// span can be computed the label is omitted and miette still produces
    /// a nicer message + help section than plain `Display`.
    #[cfg(feature = "idl")]
    pub(crate) fn new(
        error: ValidationError,
        scope: &[String],
        source: Option<(&str, &str)>,
    ) -> Self {
        let (src, span) = match source {
            Some((filename, contents)) => {
                let span = error
                    .needle()
                    .and_then(|needle| locate(contents, scope, needle));
                (Some(NamedSource::new(filename, contents.to_string())), span)
            }
            None => (None, None),
        };
        Self { error, src, span }
    }

    /// Wrap `error`. Without the `idl` feature there's no miette rendering,
    /// so the scope and source are accepted for signature parity and
    /// ignored.
    #[cfg(not(feature = "idl"))]
    pub(crate) fn new(
        error: ValidationError,
        _scope: &[String],
        _source: Option<(&str, &str)>,
    ) -> Self {
        Self { error }
    }
}

/// What to look for in the source, after walking the enclosing declaration
/// path.
#[cfg(feature = "idl")]
#[derive(Clone, Copy)]
enum Needle<'a> {
    /// A declared name: the value of a `name` key.
    Decl(&'a str),
    /// The second declaration of a name (the duplicate).
    Second(&'a str),
    /// Any other text, such as a type reference, as a whole token.
    Text(&'a str),
    /// The innermost declaration of the scope itself.
    Scope,
    /// The second declaration of the scope's innermost name (a duplicate
    /// sibling).
    SecondScope,
}

#[cfg(feature = "idl")]
impl ValidationError {
    /// The text a diagnostic underlines, or `None` when no one spot in the
    /// source is to blame.
    fn needle(&self) -> Option<Needle<'_>> {
        use Needle::{Decl, Scope, Second, SecondScope, Text};
        /// A same-module duplicate is the second declaration in scope; a
        /// cross-module one is the first declaration in the second module.
        fn duplicate<'a>(name: &'a str, first: &str, second: &str) -> Needle<'a> {
            if first == second {
                Second(name)
            } else {
                Decl(name)
            }
        }
        Some(match self {
            Self::UnsupportedSchemaVersion { version, .. } => Text(version),
            Self::DuplicateModuleName { .. } => SecondScope,
            Self::InvalidModuleName { name, .. }
            | Self::ReservedKeyword { name }
            | Self::InvalidIdentifier { name, .. }
            | Self::NameCollisionWithErrorDomain { name, .. }
            | Self::InvalidErrorCode { name, .. }
            | Self::EmptyStruct { name, .. }
            | Self::EmptyEnum { name, .. }
            | Self::EmptyInterface { name, .. }
            | Self::EmptyCallbackInterface { name, .. } => Decl(name),
            Self::DuplicateTypeName {
                name,
                first,
                second,
            }
            | Self::DuplicateFunctionName {
                name,
                first,
                second,
            }
            | Self::DuplicateErrorCodeName {
                name,
                first,
                second,
            } => duplicate(name, first, second),
            Self::DuplicateParamName { param: name, .. }
            | Self::DuplicateStructField { field: name, .. }
            | Self::DuplicateEnumVariant { variant: name, .. }
            | Self::DuplicateEnumVariantField { field: name, .. }
            | Self::DuplicateInterfaceMember { name, .. }
            | Self::DuplicateCallbackMethod { name, .. } => Second(name),
            Self::CancellableNotAsync { .. }
            | Self::AsyncIteratorReturn { .. }
            | Self::SlotCollision { .. } => Scope,
            Self::UnknownErrorDomain { domain, .. } => Text(domain),
            Self::DuplicateEnumValue { enum_name, .. } => Decl(enum_name),
            Self::ConstructorHasReturn { constructor, .. }
            | Self::AsyncConstructor { constructor, .. } => Decl(constructor),
            Self::InvalidCallbackMethod { method, .. } => Decl(method),
            Self::UnknownTypeRef { name }
            | Self::QualifiedTypeRef { name }
            | Self::UnsupportedPrimitive { name }
            | Self::ErrorDomainAsType { name }
            | Self::InterfaceInInvalidPosition { name, .. }
            | Self::CallbackInterfaceInInvalidPosition { name, .. } => Text(name),
            Self::InvalidMapKey { key_type } => Text(key_type),
            Self::IteratorInInvalidPosition { .. } => Text("iter"),
            Self::NoModuleName
            | Self::SymbolCollision { .. }
            | Self::ErrorDomainMissingName { .. }
            | Self::DuplicateErrorCode { .. } => return None,
        })
    }
}

/// Locate `needle` in `src` within the declaration path `scope`, falling
/// back to its first occurrence anywhere when the scoped search fails.
#[cfg(feature = "idl")]
fn locate(src: &str, scope: &[String], needle: Needle<'_>) -> Option<SourceSpan> {
    // A needle naming the scope itself searches for the innermost name
    // within the scope around it.
    let (scope, needle) = match (needle, scope.split_last()) {
        (Needle::Scope, Some((last, outer))) => (outer, Needle::Decl(last)),
        (Needle::SecondScope, Some((last, outer))) => (outer, Needle::Second(last)),
        (Needle::Scope | Needle::SecondScope, None) => return None,
        _ => (scope, needle),
    };
    let mut from = 0;
    for anchor in scope {
        if let Some(at) = find_decl(src, from, anchor) {
            from = at + anchor.len();
        }
    }
    let (text, scoped) = match needle {
        Needle::Decl(name) => (name, find_decl(src, from, name)),
        Needle::Second(name) => (
            name,
            find_decl(src, from, name)
                .and_then(|first| find_decl(src, first + name.len(), name))
                .or_else(|| find_decl(src, from, name)),
        ),
        Needle::Text(text) => (text, find_token(src, from, text)),
        Needle::Scope | Needle::SecondScope => unreachable!("resolved above"),
    };
    let at = scoped.or_else(|| find_token(src, 0, text))?;
    Some(span(src, at, text.len()))
}

#[cfg(feature = "idl")]
fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// The byte offset of the first whole-token occurrence of `needle` at or
/// after `from`: one not glued to identifier characters on either side.
#[cfg(feature = "idl")]
fn find_token(src: &str, from: usize, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    src.get(from..)?
        .match_indices(needle)
        .map(|(i, _)| from + i)
        .find(|&at| {
            let before = src[..at].chars().next_back();
            let after = src[at + needle.len()..].chars().next();
            !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
        })
}

/// The byte offset of the first occurrence of `name` at or after `from` that
/// is the value of a `name` key, in YAML (`name: x`) or JSON
/// (`"name": "x"`) syntax.
#[cfg(feature = "idl")]
fn find_decl(src: &str, from: usize, name: &str) -> Option<usize> {
    let mut at = from;
    loop {
        let hit = find_token(src, at, name)?;
        let before = src[..hit].trim_end_matches(['"', '\'']).trim_end();
        if let Some(key) = before.strip_suffix(':') {
            let key = key.trim_end().trim_end_matches(['"', '\'']);
            if key
                .strip_suffix("name")
                .is_some_and(|rest| !rest.chars().next_back().is_some_and(is_ident_char))
            {
                return Some(hit);
            }
        }
        at = hit + name.len();
    }
}

/// The span of `len` bytes at `at`, widened to its surrounding quotes when
/// the text is a quoted scalar.
#[cfg(feature = "idl")]
fn span(src: &str, at: usize, len: usize) -> SourceSpan {
    let quote = src[..at]
        .chars()
        .next_back()
        .filter(|c| matches!(c, '"' | '\''));
    match quote {
        Some(q) if src[at + len..].starts_with(q) => SourceSpan::new((at - 1).into(), len + 2),
        _ => SourceSpan::new(at.into(), len),
    }
}
