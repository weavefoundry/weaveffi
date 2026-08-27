//! Package-manifest emission helpers shared by every backend.
//!
//! Backends emit ecosystem manifests (`package.json`, `.nuspec`, and friends)
//! as text. Before this module existed each backend built JSON and XML by
//! `format!`, with its own partial escaping (or none); a package description
//! containing a quote or a backslash could corrupt the manifest. These
//! helpers centralize the escaping rules and give JSON-shaped manifests a
//! small insertion-ordered builder so structure and content can't drift out
//! of sync with the quoting.

use std::fmt::Write as _;

/// Escape `s` for placement inside a double-quoted JSON string literal.
///
/// Handles the two mandatory escapes (`"` and `\`), the common control
/// characters with short forms, and the remaining C0 control characters as
/// `\u00XX`, per RFC 8259.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Escape `s` for placement inside XML text content or a double-quoted XML
/// attribute value (`.nuspec`, `.csproj`, Android manifests).
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// A JSON value a manifest can hold. Manifests are small and fully known at
/// emission time, so this is a rendering model, not a parsing one.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// A string literal (escaped on render).
    Str(String),
    /// A bare boolean.
    Bool(bool),
    /// A raw pre-rendered fragment spliced verbatim (for the rare numeric or
    /// pre-built value). The caller is responsible for its validity.
    Raw(String),
    /// An array of values.
    Array(Vec<JsonValue>),
    /// A nested object with insertion-ordered keys.
    Object(JsonObject),
}

impl JsonValue {
    /// Build a string value.
    pub fn str(s: impl Into<String>) -> Self {
        JsonValue::Str(s.into())
    }

    /// Build an array of string values.
    pub fn str_array<I, S>(items: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        JsonValue::Array(items.into_iter().map(JsonValue::str).collect())
    }

    fn render_into(&self, out: &mut String, indent: usize) {
        match self {
            JsonValue::Str(s) => {
                out.push('"');
                out.push_str(&json_escape(s));
                out.push('"');
            }
            JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JsonValue::Raw(r) => out.push_str(r),
            JsonValue::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push_str("[\n");
                for (i, item) in items.iter().enumerate() {
                    push_indent(out, indent + 1);
                    item.render_into(out, indent + 1);
                    if i + 1 < items.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                push_indent(out, indent);
                out.push(']');
            }
            JsonValue::Object(obj) => obj.render_into(out, indent),
        }
    }
}

/// An insertion-ordered JSON object: the shape of every JSON manifest a
/// backend emits.
///
/// Keys render in the order they were inserted, so a backend controls the
/// conventional field order (`name` before `version` before `description`)
/// while the builder guarantees quoting and commas.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JsonObject {
    entries: Vec<(String, JsonValue)>,
}

impl JsonObject {
    /// Create an empty object.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a key/value entry, returning `self` for chaining.
    #[must_use]
    pub fn entry(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.entries.push((key.into(), value));
        self
    }

    /// Append a string entry, returning `self` for chaining.
    #[must_use]
    pub fn str_entry(self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.entry(key, JsonValue::str(value))
    }

    /// Append a string entry only when `value` is `Some`, for optional
    /// manifest fields like `description` and `license`.
    #[must_use]
    pub fn opt_str_entry(self, key: impl Into<String>, value: Option<&str>) -> Self {
        match value {
            Some(v) => self.str_entry(key, v),
            None => self,
        }
    }

    /// `true` when no entries have been added.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render the object as a pretty-printed JSON document with two-space
    /// indentation and a trailing newline, the conventional shape of
    /// `package.json`-style manifests.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.render_into(&mut out, 0);
        out.push('\n');
        out
    }

    fn render_into(&self, out: &mut String, indent: usize) {
        if self.entries.is_empty() {
            out.push_str("{}");
            return;
        }
        out.push_str("{\n");
        for (i, (key, value)) in self.entries.iter().enumerate() {
            push_indent(out, indent + 1);
            out.push('"');
            out.push_str(&json_escape(key));
            out.push_str("\": ");
            value.render_into(out, indent + 1);
            if i + 1 < self.entries.len() {
                out.push(',');
            }
            out.push('\n');
        }
        push_indent(out, indent);
        out.push('}');
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escape_covers_quotes_backslashes_and_controls() {
        assert_eq!(json_escape(r#"a "b" \c"#), r#"a \"b\" \\c"#);
        assert_eq!(json_escape("line1\nline2\t."), "line1\\nline2\\t.");
        assert_eq!(json_escape("\u{01}"), "\\u0001");
        assert_eq!(json_escape("plain"), "plain");
    }

    #[test]
    fn xml_escape_covers_the_five_entities() {
        assert_eq!(
            xml_escape(r#"a & b < c > "d" 'e'"#),
            "a &amp; b &lt; c &gt; &quot;d&quot; &apos;e&apos;"
        );
    }

    #[test]
    fn object_renders_in_insertion_order() {
        let obj = JsonObject::new()
            .str_entry("name", "kvstore")
            .str_entry("version", "1.2.0")
            .entry("private", JsonValue::Bool(false))
            .entry("keywords", JsonValue::str_array(["ffi", "native"]))
            .entry(
                "scripts",
                JsonValue::Object(JsonObject::new().str_entry("install", "node-gyp rebuild")),
            );
        assert_eq!(
            obj.render(),
            "{\n  \"name\": \"kvstore\",\n  \"version\": \"1.2.0\",\n  \"private\": false,\n  \"keywords\": [\n    \"ffi\",\n    \"native\"\n  ],\n  \"scripts\": {\n    \"install\": \"node-gyp rebuild\"\n  }\n}\n"
        );
    }

    #[test]
    fn optional_entries_and_empty_shapes() {
        let obj = JsonObject::new()
            .str_entry("name", "x")
            .opt_str_entry("description", None)
            .opt_str_entry("license", Some("MIT"))
            .entry("files", JsonValue::Array(vec![]));
        assert_eq!(
            obj.render(),
            "{\n  \"name\": \"x\",\n  \"license\": \"MIT\",\n  \"files\": []\n}\n"
        );
        assert_eq!(JsonObject::new().render(), "{}\n");
    }

    #[test]
    fn strings_with_quotes_render_escaped() {
        let obj = JsonObject::new().str_entry("description", r#"the "best" lib"#);
        assert!(obj
            .render()
            .contains(r#""description": "the \"best\" lib""#));
    }
}
