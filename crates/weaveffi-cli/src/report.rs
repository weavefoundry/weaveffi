//! Diagnostic-rendering helpers shared by every subcommand that parses an
//! IDL file.

use miette::{NamedSource, Report};

/// Wrap a [`miette::Diagnostic`] (e.g. a parse error) in a [`Report`] while
/// forcing its source code to be a [`NamedSource`] so the fancy renderer prints
/// the filename in the snippet header. miette's built-in `with_source_code` is
/// a no-op when the inner diagnostic already provides `#[source_code]`, so we
/// use a small wrapper that overrides `source_code()` instead.
pub(crate) fn with_named_source<E>(err: E, filename: &str, contents: &str) -> Report
where
    E: miette::Diagnostic + Send + Sync + 'static,
{
    Report::new(NamedDiagnostic {
        inner: err,
        src: NamedSource::new(filename, contents.to_string()),
    })
}

#[derive(Debug)]
struct NamedDiagnostic<E> {
    inner: E,
    src: NamedSource<String>,
}

impl<E: std::fmt::Display> std::fmt::Display for NamedDiagnostic<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.inner, f)
    }
}

impl<E: std::error::Error + 'static> std::error::Error for NamedDiagnostic<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.inner.source()
    }
}

impl<E: miette::Diagnostic + 'static> miette::Diagnostic for NamedDiagnostic<E> {
    fn code<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.code()
    }
    fn severity(&self) -> Option<miette::Severity> {
        self.inner.severity()
    }
    fn help<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.help()
    }
    fn url<'a>(&'a self) -> Option<Box<dyn std::fmt::Display + 'a>> {
        self.inner.url()
    }
    fn labels(&self) -> Option<Box<dyn Iterator<Item = miette::LabeledSpan> + '_>> {
        self.inner.labels()
    }
    fn related<'a>(&'a self) -> Option<Box<dyn Iterator<Item = &'a dyn miette::Diagnostic> + 'a>> {
        self.inner.related()
    }
    fn diagnostic_source(&self) -> Option<&dyn miette::Diagnostic> {
        self.inner.diagnostic_source()
    }
    fn source_code(&self) -> Option<&dyn miette::SourceCode> {
        Some(&self.src)
    }
}
