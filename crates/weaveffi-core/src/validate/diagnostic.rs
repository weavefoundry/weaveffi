//! [`ValidationDiagnostic`]: a [`ValidationError`] plus an optional source
//! snippet and best-effort span for fancy [`miette`] rendering.

use super::ValidationError;
use miette::{Diagnostic, NamedSource, SourceSpan};

/// Diagnostic wrapper that attaches an optional source code snippet and a
/// best-effort byte range to a [`ValidationError`] for fancy rendering via
/// [`miette`]. The wrapper delegates `help()` and `code()` to the inner error
/// while exposing its own `source_code` and `labels` so the renderer can
/// underline the offending identifier in the input.
#[derive(Debug)]
pub struct ValidationDiagnostic {
    /// The underlying validation error being rendered.
    pub error: ValidationError,
    /// Named source snippet (filename plus contents), when an on-disk IDL was
    /// supplied. `None` for in-memory APIs.
    pub src: Option<NamedSource<String>>,
    /// Best-effort byte range of the offending identifier within `src`, used
    /// to underline it. `None` when no span could be located.
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
    /// Build a [`ValidationDiagnostic`] from a [`ValidationError`] and an
    /// optional `(filename, contents)` source. When a source is provided the
    /// constructor performs a best-effort search for the offending identifier
    /// (e.g. a duplicate module name or unknown type reference) and attaches
    /// a [`SourceSpan`] for fancy rendering. If no span can be computed the
    /// label is omitted and miette still produces a nicer message + help
    /// section than plain `Display`.
    pub fn new(error: ValidationError, source: Option<(&str, &str)>) -> Self {
        let (src, span) = match source {
            Some((filename, contents)) => {
                let span = find_offending_span(&error, contents);
                (Some(NamedSource::new(filename, contents.to_string())), span)
            }
            None => (None, None),
        };
        Self { error, src, span }
    }
}

fn find_offending_span(err: &ValidationError, src: &str) -> Option<SourceSpan> {
    let needle: &str = match err {
        ValidationError::DuplicateModuleName(n) => Some(n.as_str()),
        ValidationError::InvalidModuleName(n, _) => Some(n.as_str()),
        ValidationError::DuplicateFunctionName { function, .. } => Some(function.as_str()),
        ValidationError::DuplicateParamName { param, .. } => Some(param.as_str()),
        ValidationError::ReservedKeyword(n) => Some(n.as_str()),
        ValidationError::InvalidIdentifier(n, _) => Some(n.as_str()),
        ValidationError::DuplicateErrorName { name, .. } => Some(name.as_str()),
        ValidationError::InvalidErrorCode { name, .. } => Some(name.as_str()),
        ValidationError::NameCollisionWithErrorDomain { name, .. } => Some(name.as_str()),
        ValidationError::DuplicateStructName { name, .. } => Some(name.as_str()),
        ValidationError::DuplicateStructField { field, .. } => Some(field.as_str()),
        ValidationError::EmptyStruct { name, .. } => Some(name.as_str()),
        ValidationError::DuplicateEnumName { name, .. } => Some(name.as_str()),
        ValidationError::EmptyEnum { name, .. } => Some(name.as_str()),
        ValidationError::DuplicateEnumVariant { variant, .. } => Some(variant.as_str()),
        ValidationError::UnknownTypeRef { name } => Some(name.as_str()),
        ValidationError::DuplicateCallbackName { name, .. } => Some(name.as_str()),
        ValidationError::UnsupportedCallbackParamType { param, .. } => Some(param.as_str()),
        ValidationError::ListenerCallbackNotFound { callback, .. } => Some(callback.as_str()),
        ValidationError::DuplicateListenerName { name, .. } => Some(name.as_str()),
        ValidationError::BuilderStructEmpty { name, .. } => Some(name.as_str()),
        ValidationError::UnsupportedSchemaVersion { version, .. } => Some(version.as_str()),
        ValidationError::AsyncIteratorReturn { function, .. } => Some(function.as_str()),
        ValidationError::DuplicateInterfaceName { name, .. } => Some(name.as_str()),
        ValidationError::DuplicateInterfaceMember { name, .. } => Some(name.as_str()),
        ValidationError::EmptyInterface { name, .. } => Some(name.as_str()),
        ValidationError::ConstructorHasReturn { constructor, .. } => Some(constructor.as_str()),
        ValidationError::AsyncConstructor { constructor, .. } => Some(constructor.as_str()),
        ValidationError::InterfaceInInvalidPosition { name, .. } => Some(name.as_str()),
        ValidationError::DuplicateTypeName { name, .. } => Some(name.as_str()),
        ValidationError::DuplicateErrorCodeName { name, .. } => Some(name.as_str()),
        ValidationError::ThrowsWithoutErrorDomain { function, .. } => Some(function.as_str()),
        ValidationError::AbiSymbolCollision { symbol, .. } => Some(symbol.as_str()),
        _ => None,
    }?;
    let quoted = format!("\"{needle}\"");
    if let Some(pos) = src.find(&quoted) {
        return Some(SourceSpan::new(pos.into(), quoted.len()));
    }
    src.find(needle)
        .map(|pos| SourceSpan::new(pos.into(), needle.len()))
}
