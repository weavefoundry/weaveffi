//! Shared codegen primitives that every language generator can reuse: the
//! doc-comment emitter and `PascalCase` conversion.
//!
//! Specialised flavours that exist in only one generator (Go's
//! godoc-style first-line symbol prefix, .NET's `<summary>` XML tags,
//! Python's triple-quoted docstring) stay generator-local because
//! their behaviour is non-uniform; this module deliberately covers
//! only the common 80%.

/// Doc-comment flavour used by [`emit_doc`].
///
/// Specialised flavours like Go's godoc-symbol prefix or .NET's
/// `<summary>` element are intentionally absent and remain in their
/// own generators because their first-line behaviour is non-uniform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocCommentStyle {
    /// `/// ...` per line (Swift, Dart, Rust).
    TripleSlash,
    /// `# ...` per line (Python `#` comments, Ruby).
    Hash,
    /// `// ...` per line (Go base case; Go's symbol-prefixed
    /// godoc convention stays generator-local).
    DoubleSlash,
    /// `/** ... */` block; single-line collapses to `/** text */`
    /// (C, C++, Kotlin/KDoc, JSDoc, TypeScript .d.ts).
    Javadoc,
}

/// Emit a doc comment for `doc` at the given `indent`, using the given
/// `style`. No-op when `doc` is `None` or trims to empty.
///
/// The output always ends with a trailing newline so a generator can
/// follow it directly with the symbol declaration on the next line.
pub fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str, style: DocCommentStyle) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    match style {
        DocCommentStyle::TripleSlash => emit_line_doc(out, doc, indent, "///"),
        DocCommentStyle::Hash => emit_line_doc(out, doc, indent, "#"),
        DocCommentStyle::DoubleSlash => emit_line_doc(out, doc, indent, "//"),
        DocCommentStyle::Javadoc => emit_javadoc(out, doc, indent),
    }
}

fn emit_line_doc(out: &mut String, doc: &str, indent: &str, marker: &str) {
    for line in doc.lines() {
        out.push_str(indent);
        if line.is_empty() {
            out.push_str(marker);
            out.push('\n');
        } else {
            out.push_str(marker);
            out.push(' ');
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn emit_javadoc(out: &mut String, doc: &str, indent: &str) {
    if doc.contains('\n') {
        out.push_str(indent);
        out.push_str("/**\n");
        for line in doc.lines() {
            out.push_str(indent);
            if line.is_empty() {
                out.push_str(" *\n");
            } else {
                out.push_str(" * ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out.push_str(indent);
        out.push_str(" */\n");
    } else {
        out.push_str(indent);
        out.push_str("/** ");
        out.push_str(doc);
        out.push_str(" */\n");
    }
}

/// Convert a `snake_case` identifier to `PascalCase` by uppercasing the
/// first character of each `_`-separated segment and preserving the rest.
///
/// This deliberately splits on `_` only; it does **not** re-case interior
/// letters the way `heck::ToUpperCamelCase` does, so an acronym-bearing
/// name like `get_HTTP` becomes `GetHTTP`, not `GetHttp`. It is the single
/// source of truth for the `snake_to_pascal` / `to_pascal_case` helpers
/// that the Python, Android, and Wasm generators each defined locally.
pub fn pascal_case(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- emit_doc ---

    #[test]
    fn emit_doc_none_writes_nothing() {
        let mut out = String::new();
        emit_doc(&mut out, &None, "", DocCommentStyle::TripleSlash);
        assert!(out.is_empty());
    }

    #[test]
    fn emit_doc_empty_string_writes_nothing() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("   \n  ".into()),
            "",
            DocCommentStyle::TripleSlash,
        );
        assert!(out.is_empty());
    }

    #[test]
    fn emit_doc_triple_slash_single_line() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("Hello, world.".into()),
            "  ",
            DocCommentStyle::TripleSlash,
        );
        assert_eq!(out, "  /// Hello, world.\n");
    }

    #[test]
    fn emit_doc_triple_slash_multi_line_with_blank() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("First line.\n\nThird line.".into()),
            "",
            DocCommentStyle::TripleSlash,
        );
        assert_eq!(out, "/// First line.\n///\n/// Third line.\n");
    }

    #[test]
    fn emit_doc_hash_single_line() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("ruby/python style".into()),
            "",
            DocCommentStyle::Hash,
        );
        assert_eq!(out, "# ruby/python style\n");
    }

    #[test]
    fn emit_doc_double_slash_single_line() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("Go-style line comment.".into()),
            "",
            DocCommentStyle::DoubleSlash,
        );
        assert_eq!(out, "// Go-style line comment.\n");
    }

    #[test]
    fn emit_doc_double_slash_multi_line() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("first\n\nsecond".into()),
            "\t",
            DocCommentStyle::DoubleSlash,
        );
        assert_eq!(out, "\t// first\n\t//\n\t// second\n");
    }

    #[test]
    fn emit_doc_hash_multi_line() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("one\n\ntwo".into()),
            "    ",
            DocCommentStyle::Hash,
        );
        assert_eq!(out, "    # one\n    #\n    # two\n");
    }

    #[test]
    fn emit_doc_javadoc_single_line_collapses() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("short".into()),
            "",
            DocCommentStyle::Javadoc,
        );
        assert_eq!(out, "/** short */\n");
    }

    #[test]
    fn emit_doc_javadoc_multi_line_expands() {
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("line one\n\nline three".into()),
            "  ",
            DocCommentStyle::Javadoc,
        );
        assert_eq!(out, "  /**\n   * line one\n   *\n   * line three\n   */\n");
    }

    #[test]
    fn emit_doc_trims_outer_whitespace_before_decisions() {
        // A doc that's "single line" after trimming should still
        // collapse to `/** text */` even if it had surrounding blank
        // lines in the IR; the existing per-generator behaviour we
        // are replacing did the same.
        let mut out = String::new();
        emit_doc(
            &mut out,
            &Some("\n\nhello\n\n".into()),
            "",
            DocCommentStyle::Javadoc,
        );
        assert_eq!(out, "/** hello */\n");
    }

    // --- pascal_case ---

    #[test]
    fn pascal_case_snake_segments() {
        assert_eq!(pascal_case("first_name"), "FirstName");
        assert_eq!(pascal_case("name"), "Name");
        assert_eq!(pascal_case("is_active"), "IsActive");
    }

    #[test]
    fn pascal_case_preserves_interior_casing() {
        // Unlike heck, interior letters keep their case (acronym-safe).
        assert_eq!(pascal_case("get_HTTP"), "GetHTTP");
        assert_eq!(pascal_case("toJSON"), "ToJSON");
    }

    #[test]
    fn pascal_case_empty_and_trailing_underscore() {
        assert_eq!(pascal_case(""), "");
        assert_eq!(pascal_case("a_"), "A");
    }
}
