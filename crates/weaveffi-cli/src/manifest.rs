//! Package-manifest emission helpers shared by every backend.
//!
//! JSON manifests (`package.json` and friends) are built as
//! [`serde_json::Map`]s, whose keys keep their insertion order (the
//! `preserve_order` feature) so a backend controls the conventional field
//! order, and rendered with [`render_json`]. XML manifests (`.nuspec`,
//! `.csproj`) are text, with values escaped by [`xml_escape`].

use serde_json::{Map, Value};

/// Render `object` as a pretty-printed JSON document with two-space
/// indentation and a trailing newline, the conventional shape of
/// `package.json`-style manifests.
#[must_use]
pub fn render_json(object: Map<String, Value>) -> String {
    let mut out =
        serde_json::to_string_pretty(&Value::Object(object)).unwrap_or_else(|_| "{}".to_string());
    out.push('\n');
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn xml_escape_covers_the_five_entities() {
        assert_eq!(
            xml_escape(r#"a & b < c > "d" 'e'"#),
            "a &amp; b &lt; c &gt; &quot;d&quot; &apos;e&apos;"
        );
    }

    #[test]
    fn json_renders_in_insertion_order_with_escapes() {
        let Value::Object(obj) = json!({
            "name": "kvstore",
            "version": "1.2.0",
            "description": "the \"best\" lib",
            "files": [],
            "engines": { "node": ">=18" },
        }) else {
            unreachable!()
        };
        assert_eq!(
            render_json(obj),
            "{\n  \"name\": \"kvstore\",\n  \"version\": \"1.2.0\",\n  \"description\": \"the \\\"best\\\" lib\",\n  \"files\": [],\n  \"engines\": {\n    \"node\": \">=18\"\n  }\n}\n"
        );
    }
}
