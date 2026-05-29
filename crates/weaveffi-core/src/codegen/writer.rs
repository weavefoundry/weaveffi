//! A small indentation-aware string builder shared by every generator.
//!
//! Before this existed, each generator tracked indentation by hand: literal
//! `"    "` / `"\t"` prefixes threaded through dozens of `push_str` calls,
//! `format!("{indent}...")` interpolation, and ad-hoc capacity estimators.
//! [`CodeWriter`] centralises that bookkeeping so a generator says *what*
//! to emit and *at what nesting level*, never *how many spaces* that is.
//!
//! The writer is deliberately language-agnostic: it knows about lines,
//! blank lines, and an indent stack, and nothing about braces, comments, or
//! syntax. Generators layer their own helpers on top.

/// An indentation-aware buffer of generated source text.
///
/// ```
/// use weaveffi_core::codegen::writer::CodeWriter;
/// let mut w = CodeWriter::new("    ");
/// w.line("fn main() {");
/// w.scope(|w| {
///     w.line("println!(\"hi\");");
/// });
/// w.line("}");
/// assert_eq!(w.finish(), "fn main() {\n    println!(\"hi\");\n}\n");
/// ```
#[derive(Debug, Clone)]
pub struct CodeWriter {
    buf: String,
    indent_unit: String,
    level: usize,
}

impl CodeWriter {
    /// Create a writer that indents nested scopes with `indent_unit`
    /// (e.g. `"    "` for four spaces, `"\t"` for a tab, `"  "` for two).
    pub fn new(indent_unit: impl Into<String>) -> Self {
        Self {
            buf: String::new(),
            indent_unit: indent_unit.into(),
            level: 0,
        }
    }

    /// Create a writer pre-allocated to `capacity` bytes.
    pub fn with_capacity(indent_unit: impl Into<String>, capacity: usize) -> Self {
        Self {
            buf: String::with_capacity(capacity),
            indent_unit: indent_unit.into(),
            level: 0,
        }
    }

    /// Seed the writer with already-rendered text (e.g. a file prelude) at
    /// indent level zero. The text is appended verbatim.
    pub fn push_raw(&mut self, text: impl AsRef<str>) {
        self.buf.push_str(text.as_ref());
    }

    /// The current indentation depth, in scope levels (not characters).
    pub fn level(&self) -> usize {
        self.level
    }

    /// Increase the indentation level by one for subsequent lines.
    pub fn indent(&mut self) {
        self.level += 1;
    }

    /// Decrease the indentation level by one. Saturates at zero so an
    /// unbalanced `dedent` cannot panic mid-generation.
    pub fn dedent(&mut self) {
        self.level = self.level.saturating_sub(1);
    }

    /// Run `f` with the indentation level increased by one, restoring it
    /// afterwards even if `f` adjusts it internally.
    pub fn scope(&mut self, f: impl FnOnce(&mut Self)) {
        let saved = self.level;
        self.level = saved + 1;
        f(self);
        self.level = saved;
    }

    /// Emit one logical line: the current indent, then `line`, then `\n`.
    ///
    /// If `line` itself contains `\n`, every segment is re-indented so the
    /// caller can pass a multi-line literal and still get consistent
    /// indentation. Empty segments stay empty (no trailing indent on blank
    /// lines), matching what the hand-rolled emitters produced.
    pub fn line(&mut self, line: impl AsRef<str>) {
        let line = line.as_ref();
        if line.is_empty() {
            self.buf.push('\n');
            return;
        }
        for segment in line.split('\n') {
            if !segment.is_empty() {
                self.write_indent();
                self.buf.push_str(segment);
            }
            self.buf.push('\n');
        }
    }

    /// Emit `line` only when the level is zero — convenience for the common
    /// "top-level declaration" case that reads better than `line`.
    pub fn top(&mut self, line: impl AsRef<str>) {
        debug_assert_eq!(self.level, 0, "top() called inside an indented scope");
        self.line(line);
    }

    /// Emit a single empty line with no indentation.
    pub fn blank(&mut self) {
        self.buf.push('\n');
    }

    /// Append verbatim text with no indentation handling. Escape hatch for
    /// pre-formatted blocks (e.g. embedded templates) where the caller has
    /// already taken responsibility for layout.
    pub fn raw(&mut self, text: impl AsRef<str>) {
        self.buf.push_str(text.as_ref());
    }

    /// Borrow the buffer accumulated so far without consuming the writer.
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

    fn write_indent(&mut self) {
        for _ in 0..self.level {
            self.buf.push_str(&self.indent_unit);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_writer_is_empty() {
        let w = CodeWriter::new("    ");
        assert!(w.is_empty());
        assert_eq!(w.finish(), "");
    }

    #[test]
    fn line_appends_newline() {
        let mut w = CodeWriter::new("    ");
        w.line("hello");
        assert_eq!(w.finish(), "hello\n");
    }

    #[test]
    fn scope_indents_with_unit() {
        let mut w = CodeWriter::new("    ");
        w.line("a");
        w.scope(|w| w.line("b"));
        w.line("c");
        assert_eq!(w.finish(), "a\n    b\nc\n");
    }

    #[test]
    fn nested_scopes_stack() {
        let mut w = CodeWriter::new("  ");
        w.line("a");
        w.scope(|w| {
            w.line("b");
            w.scope(|w| w.line("c"));
        });
        assert_eq!(w.finish(), "a\n  b\n    c\n");
    }

    #[test]
    fn tab_indent_unit() {
        let mut w = CodeWriter::new("\t");
        w.scope(|w| w.line("x"));
        assert_eq!(w.finish(), "\tx\n");
    }

    #[test]
    fn blank_has_no_indent() {
        let mut w = CodeWriter::new("    ");
        w.scope(|w| {
            w.line("x");
            w.blank();
            w.line("y");
        });
        assert_eq!(w.finish(), "    x\n\n    y\n");
    }

    #[test]
    fn multiline_line_reindents_each_segment() {
        let mut w = CodeWriter::new("  ");
        w.scope(|w| w.line("a\nb\nc"));
        assert_eq!(w.finish(), "  a\n  b\n  c\n");
    }

    #[test]
    fn multiline_keeps_interior_blanks_unindented() {
        let mut w = CodeWriter::new("  ");
        w.scope(|w| w.line("a\n\nb"));
        assert_eq!(w.finish(), "  a\n\n  b\n");
    }

    #[test]
    fn manual_indent_dedent_saturates() {
        let mut w = CodeWriter::new("  ");
        w.dedent(); // would underflow; must saturate at 0
        w.line("a");
        w.indent();
        w.line("b");
        assert_eq!(w.finish(), "a\n  b\n");
    }

    #[test]
    fn push_raw_and_raw_bypass_indentation() {
        let mut w = CodeWriter::new("    ");
        w.push_raw("// prelude\n");
        w.scope(|w| w.raw("verbatim"));
        assert_eq!(w.finish(), "// prelude\nverbatim");
    }
}
