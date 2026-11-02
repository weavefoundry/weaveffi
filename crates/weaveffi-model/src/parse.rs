//! Multi-format IDL parsing: turn YAML or JSON source text into an [`Api`].
//!
//! [`parse_api_str`] parses one document. [`load_api`] reads a document from
//! disk with every file it imports, merged into one [`Api`]. On failure each
//! returns a [`ParseError`] that carries the source text and, where
//! available, a `miette` span, so the CLI can render a caret-annotated
//! diagnostic at the offending line and column.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::ir::{Api, CURRENT_SCHEMA_VERSION};
use crate::validate::Sources;
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
    /// An IDL file couldn't be read.
    #[error("failed to read {path}: {message}")]
    #[diagnostic(help("check that the file exists and is readable"))]
    Io {
        /// The file.
        path: String,
        /// The I/O error.
        message: String,
    },
    /// The file's extension names no IDL format.
    #[error("{path} isn't an IDL file")]
    #[diagnostic(help("an IDL file ends in .yml, .yaml, or .json"))]
    UnknownExtension {
        /// The file.
        path: String,
    },
    /// An import chain leads back to a file it started from.
    #[error("import cycle: {chain}")]
    #[diagnostic(help(
        "an imported file can't import, directly or indirectly, a file importing it"
    ))]
    ImportCycle {
        /// The files of the cycle, joined with ` -> `.
        chain: String,
    },
    /// An imported file declares a schema version other than its importer's.
    #[error("{path} declares schema version {found}, but this tool reads {expected}")]
    #[diagnostic(help("every file of an API declares the current schema version"))]
    ImportVersion {
        /// The imported file.
        path: String,
        /// The version it declares.
        found: String,
        /// The version this tool reads.
        expected: String,
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
        "yaml" | "yml" => serde_saphyr::from_str(s).map_err(|e| {
            let (line, column) = e
                .location()
                .map(|m| {
                    (
                        usize::try_from(m.line()).unwrap_or(0),
                        usize::try_from(m.column()).unwrap_or(0),
                    )
                })
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

/// The IDL format of a file, from its extension (`yaml` for `.yml` and
/// `.yaml`, `json` for `.json`).
#[must_use]
pub fn format_of(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("yml" | "yaml") => Some("yaml"),
        Some("json") => Some("json"),
        _ => None,
    }
}

/// Read the IDL document at `path` and every file it imports, recursively,
/// merged into one [`Api`] (the root's `version`, `doc`, and `imports`,
/// every file's declarations), with the [`Sources`] that locate each
/// declaration's diagnostics. A file imported twice is read once.
///
/// # Errors
///
/// Returns an error when a file can't be read, has no IDL extension, fails
/// to parse, declares another schema version than the root, or imports a
/// file importing it.
pub fn load_api(path: &Path) -> Result<(Api, Sources), ParseError> {
    let mut sources = Sources::default();
    let mut done = BTreeSet::new();
    let mut chain = Vec::new();
    let root = load(path, &mut sources, &mut done, &mut chain)?;
    let mut api = root.0;
    for imported in root.1 {
        api.merge(imported);
    }
    Ok((api, sources))
}

/// Load `path` (recording it in `sources`) and every file it imports,
/// returning its own document and the flattened imported documents.
fn load(
    path: &Path,
    sources: &mut Sources,
    done: &mut BTreeSet<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<(Api, Vec<Api>), ParseError> {
    let display = path.display().to_string();
    let key = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if chain.contains(&key) {
        let mut names: Vec<String> = chain.iter().map(|p| p.display().to_string()).collect();
        names.push(key.display().to_string());
        return Err(ParseError::ImportCycle {
            chain: names.join(" -> "),
        });
    }
    let format = format_of(path).ok_or_else(|| ParseError::UnknownExtension {
        path: display.clone(),
    })?;
    let text = std::fs::read_to_string(path).map_err(|e| ParseError::Io {
        path: display.clone(),
        message: e.to_string(),
    })?;
    let api = parse_api_str(&text, format)?;
    let index = sources.files.len();
    for name in api.declaration_names() {
        sources.origin.entry(name.to_string()).or_insert(index);
    }
    sources.files.push((display, text));
    done.insert(key.clone());
    chain.push(key);
    let base = path.parent().unwrap_or_else(|| Path::new(""));
    let mut imported = Vec::new();
    for import in &api.imports {
        let child = base.join(import);
        let child_key = child.canonicalize().unwrap_or_else(|_| child.clone());
        if done.contains(&child_key) && !chain.contains(&child_key) {
            continue;
        }
        let (doc, nested) = load(&child, sources, done, chain)?;
        if doc.version != CURRENT_SCHEMA_VERSION {
            return Err(ParseError::ImportVersion {
                path: child.display().to_string(),
                found: doc.version,
                expected: CURRENT_SCHEMA_VERSION.to_string(),
            });
        }
        imported.push(doc);
        imported.extend(nested);
    }
    chain.pop();
    Ok((api, imported))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Function, Param, TypeRef};
    use crate::ty::Prim;

    fn expected_api() -> Api {
        let mut api = Api::new();
        api.functions.push(Function {
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
            ..Function::named("add")
        });
        api
    }

    #[test]
    fn every_format_parses_the_same_document() {
        let yaml = r#"
version: "0.13.0"
functions:
  - name: add
    params:
      - { name: a, type: i32 }
      - { name: b, type: i32 }
    return: i32
    doc: "Adds two numbers"
"#;
        let json = r#"{
            "version": "0.13.0",
            "functions": [{
                "name": "add",
                "params": [{"name": "a", "type": "i32"}, {"name": "b", "type": "i32"}],
                "return": "i32",
                "doc": "Adds two numbers"
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
        let yaml = "version: \"0.13.0\"\nfunctions:\n  - name: [oops\n";
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
        let yaml = "version: \"0.13.0\"\nfunctions:\n  - name: f\n    params: [{ name: x, type: \"{string}\" }]\n";
        let err = parse_api_str(yaml, "yaml").unwrap_err();
        assert!(err.to_string().contains("map type missing"), "{err}");
    }

    #[test]
    fn imports_merge_into_one_namespace_and_cycles_are_errors() {
        let dir = std::env::temp_dir().join(format!("weaveffi-imports-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write(
            "api.yml",
            "version: \"0.13.0\"\nimports: [sub/types.yml, other.yml]\nfunctions: [{ name: get, return: Item }]\n",
        );
        write(
            "sub/types.yml",
            "version: \"0.13.0\"\nimports: [../other.yml]\nrecords: [{ name: Item, fields: [{ name: id, type: u64 }] }]\n",
        );
        write(
            "other.yml",
            "version: \"0.13.0\"\nfunctions: [{ name: ping }]\n",
        );
        let (api, sources) = load_api(&dir.join("api.yml")).unwrap();
        let names: Vec<&str> = api.declaration_names().collect();
        assert_eq!(names, ["Item", "get", "ping"]);
        assert_eq!(sources.files.len(), 3);
        assert!(sources
            .file_of(Some("Item"))
            .unwrap()
            .0
            .ends_with("types.yml"));
        assert!(sources
            .file_of(Some("ping"))
            .unwrap()
            .0
            .ends_with("other.yml"));

        write(
            "other.yml",
            "version: \"0.13.0\"\nimports: [api.yml]\nfunctions: [{ name: ping }]\n",
        );
        let err = load_api(&dir.join("api.yml")).unwrap_err();
        assert!(matches!(err, ParseError::ImportCycle { .. }), "{err}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
