//! Shared codegen primitives that every language generator can reuse.
//!
//! Until 0.4.0, every generator hand-rolled its own copy of the module
//! tree walker, the doc-comment emitter, and the "is this type a C
//! pointer at the ABI boundary?" predicate. Pulling them in here gives
//! the generators one source of truth and shrinks each crate by a few
//! dozen lines of near-identical helper code.
//!
//! Specialised flavours that exist in only one generator (Go's
//! godoc-style first-line symbol prefix, .NET's `<summary>` XML tags,
//! Python's triple-quoted docstring) stay generator-local because
//! their behaviour is non-uniform; this module deliberately covers
//! only the common 80%.

use weaveffi_ir::ir::{Module, TypeRef};

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

/// Iterate over every module in `roots` and its descendants in
/// depth-first pre-order: each module is yielded before its children,
/// and children are yielded left-to-right.
///
/// Equivalent to the recursive `collect_all_modules` helper that
/// every generator used to define locally.
pub fn walk_modules<'a>(roots: &'a [Module]) -> impl Iterator<Item = &'a Module> {
    let mut stack: Vec<&'a Module> = roots.iter().rev().collect();
    std::iter::from_fn(move || {
        let m = stack.pop()?;
        for child in m.modules.iter().rev() {
            stack.push(child);
        }
        Some(m)
    })
}

/// Like [`walk_modules`], but each module is paired with its
/// underscore-joined path (e.g. `parent_child_grandchild`). The path
/// matches the canonical C symbol prefix segment that the C generator
/// builds when emitting `{c_prefix}_{module_path}_{name}`.
pub fn walk_modules_with_path<'a>(
    roots: &'a [Module],
) -> impl Iterator<Item = (&'a Module, String)> {
    let mut stack: Vec<(&'a Module, String)> =
        roots.iter().rev().map(|m| (m, m.name.clone())).collect();
    std::iter::from_fn(move || {
        let (m, path) = stack.pop()?;
        for child in m.modules.iter().rev() {
            stack.push((child, format!("{path}_{}", child.name)));
        }
        Some((m, path))
    })
}

/// Predicate: returns `true` when the IR type is represented as a
/// pointer at the C ABI boundary.
///
/// String types, byte buffers, struct values (including rich/algebraic enums,
/// which are spelled `Struct` after resolution), typed handles, lists, maps,
/// and iterators all cross the ABI as pointers. Scalars (`i32`/`bool`/etc.),
/// `Handle`, and a C-style `Enum(_)` cross by value.
///
/// `Optional(T)` is *not* automatically a pointer here: callers that
/// care about Optional pointer-ness (the C/C++ generators) recurse
/// into the inner type before consulting this predicate.
pub fn is_c_pointer_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes
            | TypeRef::Struct(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
            | TypeRef::Iterator(_)
    )
}

/// Convert a `snake_case` identifier to `PascalCase` by uppercasing the
/// first character of each `_`-separated segment and preserving the rest.
///
/// This deliberately splits on `_` only — it does **not** re-case interior
/// letters the way `heck::ToUpperCamelCase` does — so an acronym-bearing
/// name like `get_HTTP` becomes `GetHTTP`, not `GetHttp`. It is the single
/// source of truth for the `snake_to_pascal` / `to_pascal_case` helpers
/// that the Python, Android, and WASM generators each defined locally.
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
    use weaveffi_ir::ir::Module;

    fn leaf(name: &str) -> Module {
        Module {
            name: name.to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn with_children(name: &str, children: Vec<Module>) -> Module {
        Module {
            modules: children,
            ..leaf(name)
        }
    }

    // --- walk_modules ---

    #[test]
    fn walk_modules_visits_pre_order() {
        let roots = vec![
            with_children("a", vec![leaf("a1"), leaf("a2")]),
            with_children("b", vec![leaf("b1")]),
        ];
        let names: Vec<&str> = walk_modules(&roots).map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["a", "a1", "a2", "b", "b1"]);
    }

    #[test]
    fn walk_modules_descends_to_arbitrary_depth() {
        let roots = vec![with_children(
            "a",
            vec![with_children(
                "b",
                vec![with_children("c", vec![leaf("d")])],
            )],
        )];
        let names: Vec<&str> = walk_modules(&roots).map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn walk_modules_empty_input_yields_nothing() {
        let roots: Vec<Module> = vec![];
        assert_eq!(walk_modules(&roots).count(), 0);
    }

    // --- walk_modules_with_path ---

    #[test]
    fn walk_modules_with_path_joins_with_underscore() {
        let roots = vec![with_children(
            "outer",
            vec![with_children("inner", vec![leaf("leaf")])],
        )];
        let pairs: Vec<(String, String)> = walk_modules_with_path(&roots)
            .map(|(m, p)| (m.name.clone(), p))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("outer".into(), "outer".into()),
                ("inner".into(), "outer_inner".into()),
                ("leaf".into(), "outer_inner_leaf".into()),
            ]
        );
    }

    #[test]
    fn walk_modules_with_path_independent_roots() {
        let roots = vec![
            with_children("a", vec![leaf("a1")]),
            with_children("b", vec![leaf("b1")]),
        ];
        let paths: Vec<String> = walk_modules_with_path(&roots).map(|(_, p)| p).collect();
        assert_eq!(paths, vec!["a", "a_a1", "b", "b_b1"]);
    }

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
        // lines in the IR — the existing per-generator behaviour we
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

    // --- is_c_pointer_type ---

    #[test]
    fn is_c_pointer_for_pointer_carrying_types() {
        for ty in [
            TypeRef::StringUtf8,
            TypeRef::BorrowedStr,
            TypeRef::Bytes,
            TypeRef::BorrowedBytes,
            TypeRef::Struct("X".into()),
            TypeRef::TypedHandle("X".into()),
            TypeRef::List(Box::new(TypeRef::I32)),
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            TypeRef::Iterator(Box::new(TypeRef::StringUtf8)),
        ] {
            assert!(is_c_pointer_type(&ty), "expected pointer: {ty:?}");
        }
    }

    #[test]
    fn is_c_pointer_for_value_types_is_false() {
        for ty in [
            TypeRef::I32,
            TypeRef::U32,
            TypeRef::I64,
            TypeRef::F64,
            TypeRef::Bool,
            TypeRef::Handle,
            TypeRef::Enum("E".into()),
        ] {
            assert!(!is_c_pointer_type(&ty), "expected non-pointer: {ty:?}");
        }
    }

    #[test]
    fn is_c_pointer_does_not_recurse_into_optional() {
        // Callers that care about Optional pointer-ness recurse first.
        // We document and enforce that contract: bare Optional is not
        // a pointer.
        assert!(!is_c_pointer_type(&TypeRef::Optional(Box::new(
            TypeRef::I32
        ))));
        assert!(!is_c_pointer_type(&TypeRef::Optional(Box::new(
            TypeRef::StringUtf8
        ))));
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
