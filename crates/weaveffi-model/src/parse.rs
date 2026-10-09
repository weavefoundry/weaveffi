//! Multi-format IDL parsing: turn YAML or JSON source text into an [`Api`].
//!
//! [`parse_api_str`] is the entry point. On failure it returns a [`ParseError`]
//! that carries the source text and, where available, a `miette` span, so the
//! CLI can render a caret-annotated diagnostic at the offending line and column.

use crate::ir::Api;
use miette::{Diagnostic, SourceSpan};

/// Everything that can go wrong while turning IDL source text into an [`Api`].
///
/// Every variant implements [`miette::Diagnostic`], pairing a message with a
/// `help` hint and, when the underlying parser reports a location, a labeled
/// span into the original source so the CLI can render a caret-annotated error.
#[derive(Debug, thiserror::Error, Diagnostic)]
pub enum ParseError {
    /// The requested format wasn't one of `yaml`, `yml`, or `json`. Carries
    /// the unrecognized format string.
    #[error("unsupported format: {0}")]
    #[diagnostic(help("supported formats are 'yaml', 'yml', and 'json'"))]
    UnsupportedFormat(String),
    /// The YAML deserializer rejected the document.
    #[error("YAML parse error at line {line}, column {column}: {message}")]
    #[diagnostic(help(
        "check YAML indentation, quoting, and that all required fields have valid values"
    ))]
    Yaml {
        /// 1-indexed line of the error, or `0` when the location is unknown.
        line: usize,
        /// 1-indexed column of the error, or `0` when the location is unknown.
        column: usize,
        /// Message reported by the underlying deserializer.
        message: String,
        /// Full source text, retained so the diagnostic can render a snippet.
        #[source_code]
        src: String,
        /// Byte span of the offending location within `src`, when known.
        #[label("here")]
        span: Option<SourceSpan>,
    },
    /// The JSON deserializer rejected the document.
    #[error("JSON parse error at line {line}, column {column}: {message}")]
    #[diagnostic(help(
        "check JSON syntax: matching braces/brackets, quoted keys, and trailing commas"
    ))]
    Json {
        /// 1-indexed line of the error, or `0` when the location is unknown.
        line: usize,
        /// 1-indexed column of the error, or `0` when the location is unknown.
        column: usize,
        /// Message reported by the underlying deserializer.
        message: String,
        /// Full source text, retained so the diagnostic can render a snippet.
        #[source_code]
        src: String,
        /// Byte span of the offending location within `src`, when known.
        #[label("here")]
        span: Option<SourceSpan>,
    },
}

/// Convert a 1-indexed `(line, col)` pair into a 0-indexed byte offset within
/// `src`. Returns `0` when either coordinate is `0` (i.e., unknown), and
/// clamps to `src.len()` when the requested location is past the end.
pub fn line_col_to_offset(src: &str, line: usize, col: usize) -> usize {
    if line == 0 {
        return 0;
    }
    let line_idx = line - 1;
    let col_idx = col.saturating_sub(1);

    let mut start = 0usize;
    for (i, l) in src.split('\n').enumerate() {
        if i == line_idx {
            let line_offset = l
                .char_indices()
                .nth(col_idx)
                .map(|(b, _)| b)
                .unwrap_or(l.len());
            return (start + line_offset).min(src.len());
        }
        start += l.len() + 1;
    }
    src.len()
}

/// Parse IDL source text in the given format into an [`Api`].
///
/// `format` selects the deserializer: `yaml` or `yml` for YAML and `json`
/// for JSON. On failure the returned [`ParseError`] captures
/// the source text and, when the deserializer reports one, a span for a rich
/// diagnostic.
///
/// # Errors
///
/// Returns [`ParseError::UnsupportedFormat`] when `format` isn't a recognized
/// format string, or the matching [`Yaml`](ParseError::Yaml) or
/// [`Json`](ParseError::Json) variant when the source text is malformed or
/// doesn't match the schema.
pub fn parse_api_str(s: &str, format: &str) -> Result<Api, ParseError> {
    match format {
        "yaml" | "yml" => serde_yaml_ng::from_str(s).map_err(|e| {
            let (line, column) = e
                .location()
                .map(|m| (m.line(), m.column()))
                .unwrap_or((0, 0));
            let span = if line > 0 && column > 0 {
                Some(SourceSpan::new(
                    line_col_to_offset(s, line, column).into(),
                    1,
                ))
            } else {
                None
            };
            ParseError::Yaml {
                line,
                column,
                message: e.to_string(),
                src: s.to_string(),
                span,
            }
        }),
        "json" => serde_json::from_str(s).map_err(|e| {
            let line = e.line();
            let column = e.column();
            let span = if line > 0 && column > 0 {
                Some(SourceSpan::new(
                    line_col_to_offset(s, line, column).into(),
                    1,
                ))
            } else {
                None
            };
            ParseError::Json {
                line,
                column,
                message: e.to_string(),
                src: s.to_string(),
                span,
            }
        }),
        other => Err(ParseError::UnsupportedFormat(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Function, Module, Param, TypeRef, CURRENT_SCHEMA_VERSION};
    use crate::ty::Prim;

    fn expected_api() -> Api {
        Api {
            version: CURRENT_SCHEMA_VERSION.to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                doc: None,
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::Prim(Prim::I32),
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Prim(Prim::I32)),
                    doc: Some("Adds two numbers".to_string()),
                    throws: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                }],
                interfaces: vec![],
                callback_interfaces: vec![],
                structs: vec![],
                enums: vec![],
                errors: vec![],
                modules: vec![],
            }],
        }
    }

    #[test]
    fn every_format_parses_the_same_document() {
        let yaml = r#"
version: "0.12.0"
modules:
  - name: math
    functions:
      - name: add
        params:
          - name: a
            type: i32
          - name: b
            type: i32
        return: i32
        doc: "Adds two numbers"
"#;
        let json = r#"{
            "version": "0.12.0",
            "modules": [{
                "name": "math",
                "functions": [{
                    "name": "add",
                    "params": [
                        {"name": "a", "type": "i32"},
                        {"name": "b", "type": "i32"}
                    ],
                    "return": "i32",
                    "doc": "Adds two numbers"
                }]
            }]
        }"#;
        assert_eq!(parse_api_str(yaml, "yaml").unwrap(), expected_api());
        assert_eq!(parse_api_str(yaml, "yml").unwrap(), expected_api());
        assert_eq!(parse_api_str(json, "json").unwrap(), expected_api());
    }

    #[test]
    fn unsupported_format_returns_error() {
        assert!(matches!(
            parse_api_str("", "xml"),
            Err(ParseError::UnsupportedFormat(f)) if f == "xml"
        ));
        assert!(matches!(
            parse_api_str("version = \"0.12.0\"", "toml"),
            Err(ParseError::UnsupportedFormat(f)) if f == "toml"
        ));
    }

    #[test]
    fn line_col_to_offset_maps_and_clamps() {
        let src = "ab\ncd\nef";
        assert_eq!(line_col_to_offset(src, 1, 1), 0);
        assert_eq!(line_col_to_offset(src, 2, 2), 4);
        assert_eq!(line_col_to_offset(src, 0, 0), 0);
        assert_eq!(line_col_to_offset(src, 99, 1), src.len());
        assert_eq!(line_col_to_offset(src, 1, 99), 2);
    }

    #[test]
    fn parse_errors_carry_spans() {
        let yaml = "version: \"0.12.0\"\nmodules:\n  - name: [oops\n";
        match parse_api_str(yaml, "yaml").unwrap_err() {
            ParseError::Yaml { line, span, .. } => {
                assert!(line > 0);
                assert!(span.is_some());
            }
            other => panic!("expected YAML error, got {other:?}"),
        }
        match parse_api_str("{\"version\": }", "json").unwrap_err() {
            ParseError::Json { line, column, .. } => assert!(line > 0 && column > 0),
            other => panic!("expected JSON error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_type_syntax_is_a_parse_error() {
        let yaml = "version: \"0.12.0\"\nmodules:\n  - name: m\n    functions:\n      - name: f\n        params: [{ name: x, type: \"{string}\" }]\n";
        let err = parse_api_str(yaml, "yaml").unwrap_err();
        assert!(err.to_string().contains("map type missing"), "{err}");
    }
}
