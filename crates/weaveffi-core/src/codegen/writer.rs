//! A small, deterministic code-emission toolkit shared by every generator.
//!
//! Before this module existed, all eleven generators built their output by hand
//! with thousands of `out.push_str(&format!(...))` calls, threading the current
//! indentation through every call site as literal spaces. That made the *shape*
//! of the emitted code invisible in the Rust source and turned indentation and
//! block nesting into a manual, bug-prone bookkeeping chore.
//!
//! [`CodeWriter`] owns the indentation and block scoping so a backend writes
//! intent (`line`, `block`, `scope`) instead of whitespace. It is intentionally
//! tiny and unopinionated: it does not reflow or pretty-print, so a backend
//! stays in full control of the exact text it emits while losing the manual
//! `\n`/indent bookkeeping. Output is byte-deterministic: blank lines never
//! carry trailing whitespace, and the indent unit is fixed per writer.
//!
//! ```
//! use weaveffi_core::codegen::writer::CodeWriter;
//!
//! let mut w = CodeWriter::new("    ");
//! w.line("class Greeter:");
//! w.scope(|w| {
//!     w.line("def hello(self):");
//!     w.scope(|w| {
//!         w.line("return \"hi\"");
//!     });
//! });
//! assert_eq!(
//!     w.finish(),
//!     "class Greeter:\n    def hello(self):\n        return \"hi\"\n",
//! );
//! ```

use crate::codegen::common::{emit_doc, DocCommentStyle};

/// An indentation-aware string builder for generated source code.
///
/// Construct one with [`CodeWriter::new`], passing the per-target indent unit
/// (`"    "`, `"  "`, or `"\t"`). Emit lines with [`line`](Self::line), nest
/// with [`scope`](Self::scope) / [`block`](Self::block), splice pre-rendered
/// multi-line text with [`block_raw`](Self::block_raw), and finish with
/// [`finish`](Self::finish).
#[derive(Debug, Clone)]
pub struct CodeWriter {
    buf: String,
    depth: usize,
    unit: String,
}

impl CodeWriter {
    /// Create an empty writer whose one indent level is `unit` (commonly
    /// `"    "`, `"  "`, or `"\t"`).
    pub fn new(unit: impl Into<String>) -> Self {
        Self {
            buf: String::new(),
            depth: 0,
            unit: unit.into(),
        }
    }

    /// Create an empty writer with a four-space indent unit, the most common
    /// default across the generators.
    pub fn four_space() -> Self {
        Self::new("    ")
    }

    /// Create an empty writer with a two-space indent unit.
    pub fn two_space() -> Self {
        Self::new("  ")
    }

    /// Create an empty writer with a tab indent unit.
    pub fn tabs() -> Self {
        Self::new("\t")
    }

    /// The current indentation depth (number of `unit`s prepended to a line).
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Start the writer already nested `depth` levels deep. Useful when
    /// rendering a fragment that belongs inside an enclosing block whose
    /// indentation the caller tracks separately (for example a backend that
    /// still threads an explicit indent string through its render functions).
    #[must_use]
    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// The literal indentation prefix at the current depth. Useful when calling
    /// a helper that takes an explicit indent string.
    pub fn indent_str(&self) -> String {
        self.unit.repeat(self.depth)
    }

    /// Increase the indentation depth by one level.
    pub fn indent(&mut self) -> &mut Self {
        self.depth += 1;
        self
    }

    /// Decrease the indentation depth by one level. Saturates at zero so an
    /// unbalanced `dedent` can never panic mid-render.
    pub fn dedent(&mut self) -> &mut Self {
        self.depth = self.depth.saturating_sub(1);
        self
    }

    /// Write one line at the current indentation, followed by a newline.
    ///
    /// An empty (or whitespace-only-after-trim is *not* applied here; only a
    /// truly empty string) argument emits a bare newline with no trailing
    /// whitespace, so callers can use `line("")` interchangeably with
    /// [`blank`](Self::blank).
    pub fn line(&mut self, s: impl AsRef<str>) -> &mut Self {
        let s = s.ref_str();
        if s.is_empty() {
            self.buf.push('\n');
        } else {
            self.buf.push_str(&self.unit.repeat(self.depth));
            self.buf.push_str(s);
            self.buf.push('\n');
        }
        self
    }

    /// Write a blank line (a single newline, never trailing whitespace).
    pub fn blank(&mut self) -> &mut Self {
        self.buf.push('\n');
        self
    }

    /// Append text verbatim with no indentation and no trailing newline.
    ///
    /// Use this for already-fully-formatted fragments (a generated-file
    /// prelude, a precomputed block) that must be spliced in unchanged.
    pub fn raw(&mut self, s: impl AsRef<str>) -> &mut Self {
        self.buf.push_str(s.ref_str());
        self
    }

    /// Splice a multi-line fragment, re-indenting every non-empty line to the
    /// current depth while preserving the fragment's own *relative*
    /// indentation. A trailing newline on the fragment is honored; blank lines
    /// stay blank (no trailing whitespace).
    ///
    /// This is the migration workhorse: a backend can keep a large literal
    /// snippet as a raw string and let the writer place it at the right depth.
    pub fn block_raw(&mut self, s: impl AsRef<str>) -> &mut Self {
        let s = s.ref_str();
        if s.is_empty() {
            return self;
        }
        let prefix = self.unit.repeat(self.depth);
        // Split on '\n'; a trailing newline yields a final empty segment we
        // must not emit as its own indented line.
        let ends_with_newline = s.ends_with('\n');
        let mut lines = s.split('\n').peekable();
        while let Some(line) = lines.next() {
            let is_last = lines.peek().is_none();
            if is_last && line.is_empty() && ends_with_newline {
                // The empty tail produced by a trailing '\n': stop, the
                // previous iteration already wrote that newline.
                break;
            }
            if line.is_empty() {
                self.buf.push('\n');
            } else {
                self.buf.push_str(&prefix);
                self.buf.push_str(line);
                self.buf.push('\n');
            }
        }
        self
    }

    /// Run `f` with the indentation increased by one level, then restore it.
    pub fn scope(&mut self, f: impl FnOnce(&mut Self)) -> &mut Self {
        self.indent();
        f(self);
        self.dedent();
        self
    }

    /// Emit `open`, run `f` at one deeper indent, then emit `close` at the
    /// original indent. The canonical way to write a braced or `:`-introduced
    /// block.
    ///
    /// ```
    /// use weaveffi_core::codegen::writer::CodeWriter;
    /// let mut w = CodeWriter::four_space();
    /// w.block("fn main() {", "}", |w| {
    ///     w.line("println!(\"hi\");");
    /// });
    /// assert_eq!(w.finish(), "fn main() {\n    println!(\"hi\");\n}\n");
    /// ```
    pub fn block(
        &mut self,
        open: impl AsRef<str>,
        close: impl AsRef<str>,
        f: impl FnOnce(&mut Self),
    ) -> &mut Self {
        self.line(open);
        self.scope(f);
        self.line(close);
        self
    }

    /// Emit a doc comment for `doc` in `style` at the current indentation.
    /// No-op when `doc` is `None` or trims to empty. Mirrors [`emit_doc`],
    /// but indents from the writer's current depth instead of an explicit
    /// prefix argument.
    pub fn doc(&mut self, doc: &Option<String>, style: DocCommentStyle) -> &mut Self {
        let prefix = self.unit.repeat(self.depth);
        emit_doc(&mut self.buf, doc, &prefix, style);
        self
    }

    /// Borrow the accumulated text without consuming the writer.
    pub fn as_str(&self) -> &str {
        &self.buf
    }

    /// True when nothing has been written yet.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Consume the writer and return the accumulated source text.
    pub fn finish(self) -> String {
        self.buf
    }
}

/// Tiny internal helper so `line`/`raw`/`block_raw` accept both `&str` and
/// `String` (and `&String`) without each method taking a turbofished generic
/// that the docs would have to explain. Not part of the public API.
trait RefStr {
    fn ref_str(&self) -> &str;
}

impl<T: AsRef<str>> RefStr for T {
    fn ref_str(&self) -> &str {
        self.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_indents_and_newline_terminates() {
        let mut w = CodeWriter::four_space();
        w.line("a");
        w.indent();
        w.line("b");
        w.dedent();
        w.line("c");
        assert_eq!(w.finish(), "a\n    b\nc\n");
    }

    #[test]
    fn empty_line_is_bare_newline() {
        let mut w = CodeWriter::four_space();
        w.indent();
        w.line("");
        w.blank();
        w.line("x");
        assert_eq!(w.finish(), "\n\n    x\n");
    }

    #[test]
    fn scope_restores_depth() {
        let mut w = CodeWriter::two_space();
        w.line("outer");
        w.scope(|w| {
            w.line("inner");
            w.scope(|w| {
                w.line("deepest");
            });
            w.line("inner again");
        });
        w.line("outer again");
        assert_eq!(
            w.finish(),
            "outer\n  inner\n    deepest\n  inner again\nouter again\n"
        );
    }

    #[test]
    fn block_brackets_body() {
        let mut w = CodeWriter::four_space();
        w.block("if (x) {", "}", |w| {
            w.line("do_a();");
            w.line("do_b();");
        });
        assert_eq!(w.finish(), "if (x) {\n    do_a();\n    do_b();\n}\n");
    }

    #[test]
    fn nested_blocks() {
        let mut w = CodeWriter::four_space();
        w.block("class A:", "", |w| {
            w.block("def f(self):", "", |w| {
                w.line("pass");
            });
        });
        // Note: an empty close just emits a bare newline.
        assert_eq!(w.finish(), "class A:\n    def f(self):\n        pass\n\n\n");
    }

    #[test]
    fn raw_appends_verbatim() {
        let mut w = CodeWriter::four_space();
        w.indent();
        w.raw("no-indent");
        w.raw(" continues");
        assert_eq!(w.finish(), "no-indent continues");
    }

    #[test]
    fn block_raw_reindents_relative_structure() {
        let mut w = CodeWriter::four_space();
        w.indent();
        w.block_raw("def foo():\n    return 1\n");
        assert_eq!(w.finish(), "    def foo():\n        return 1\n");
    }

    #[test]
    fn block_raw_preserves_blank_lines_without_trailing_ws() {
        let mut w = CodeWriter::two_space();
        w.indent();
        w.block_raw("a\n\nb\n");
        assert_eq!(w.finish(), "  a\n\n  b\n");
    }

    #[test]
    fn block_raw_without_trailing_newline() {
        let mut w = CodeWriter::four_space();
        w.block_raw("one\ntwo");
        assert_eq!(w.finish(), "one\ntwo\n");
    }

    #[test]
    fn block_raw_empty_is_noop() {
        let mut w = CodeWriter::four_space();
        w.block_raw("");
        assert!(w.is_empty());
    }

    #[test]
    fn doc_uses_current_indent() {
        let mut w = CodeWriter::four_space();
        w.indent();
        w.doc(&Some("Hello.".to_string()), DocCommentStyle::TripleSlash);
        w.line("fn f() {}");
        assert_eq!(w.finish(), "    /// Hello.\n    fn f() {}\n");
    }

    #[test]
    fn doc_none_is_noop() {
        let mut w = CodeWriter::four_space();
        w.doc(&None, DocCommentStyle::Hash);
        assert!(w.is_empty());
    }

    #[test]
    fn accepts_string_and_str() {
        let mut w = CodeWriter::four_space();
        let owned = String::from("owned");
        w.line(&owned);
        w.line("borrowed");
        w.line(format!("fmt {}", 1));
        assert_eq!(w.finish(), "owned\nborrowed\nfmt 1\n");
    }

    #[test]
    fn indent_str_reflects_depth() {
        let mut w = CodeWriter::new("  ");
        assert_eq!(w.indent_str(), "");
        w.indent().indent();
        assert_eq!(w.indent_str(), "    ");
        assert_eq!(w.depth(), 2);
    }

    #[test]
    fn with_depth_seeds_initial_indentation() {
        let mut w = CodeWriter::four_space().with_depth(2);
        w.line("if x:");
        w.scope(|w| {
            w.line("pass");
        });
        assert_eq!(w.finish(), "        if x:\n            pass\n");
    }

    #[test]
    fn dedent_saturates_at_zero() {
        let mut w = CodeWriter::four_space();
        w.dedent().dedent();
        w.line("x");
        assert_eq!(w.finish(), "x\n");
    }
}
