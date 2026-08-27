//! Per-language identifier policy: the reserved-word tables and the escape
//! rule every backend applies before emitting a user-chosen name.
//!
//! IDL names (parameters, fields, functions) are chosen by the producer and
//! land verbatim (or case-converted) in eleven languages, any of which may
//! reserve them (`class`, `type`, `import`, `end`, ...). Before this module
//! existed each backend kept its own partial keyword list, or none; a field
//! named `type` broke some targets and silently worked in others. The tables
//! here are the single source of truth, and [`escape_ident`] is the single
//! escape rule: a reserved name gains a trailing `_`, which is legal in every
//! supported target and stable under repeated application.

/// Python reserved words (keywords plus the keyword-like constants), per the
/// `keyword` module of `CPython` 3.12.
pub const PYTHON_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];

/// Go reserved words, per the Go language specification.
pub const GO_KEYWORDS: &[&str] = &[
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];

/// Swift reserved words that require backtick-escaping or renaming in
/// declaration and expression positions.
pub const SWIFT_KEYWORDS: &[&str] = &[
    "Any",
    "Protocol",
    "Self",
    "Type",
    "as",
    "associatedtype",
    "break",
    "case",
    "catch",
    "class",
    "continue",
    "default",
    "defer",
    "deinit",
    "do",
    "else",
    "enum",
    "extension",
    "fallthrough",
    "false",
    "fileprivate",
    "for",
    "func",
    "guard",
    "if",
    "import",
    "in",
    "init",
    "inout",
    "internal",
    "is",
    "let",
    "nil",
    "operator",
    "precedencegroup",
    "private",
    "protocol",
    "public",
    "repeat",
    "rethrows",
    "return",
    "self",
    "static",
    "struct",
    "subscript",
    "super",
    "switch",
    "throw",
    "throws",
    "true",
    "try",
    "typealias",
    "var",
    "where",
    "while",
];

/// Kotlin hard keywords, per the Kotlin language grammar. Soft keywords
/// (`by`, `get`, `set`, ...) stay legal as identifiers and are not listed.
pub const KOTLIN_KEYWORDS: &[&str] = &[
    "as",
    "break",
    "class",
    "continue",
    "do",
    "else",
    "false",
    "for",
    "fun",
    "if",
    "in",
    "interface",
    "is",
    "null",
    "object",
    "package",
    "return",
    "super",
    "this",
    "throw",
    "true",
    "try",
    "typealias",
    "typeof",
    "val",
    "var",
    "when",
    "while",
];

/// C# reserved keywords, per the C# language specification.
pub const CSHARP_KEYWORDS: &[&str] = &[
    "abstract",
    "as",
    "base",
    "bool",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "checked",
    "class",
    "const",
    "continue",
    "decimal",
    "default",
    "delegate",
    "do",
    "double",
    "else",
    "enum",
    "event",
    "explicit",
    "extern",
    "false",
    "finally",
    "fixed",
    "float",
    "for",
    "foreach",
    "goto",
    "if",
    "implicit",
    "in",
    "int",
    "interface",
    "internal",
    "is",
    "lock",
    "long",
    "namespace",
    "new",
    "null",
    "object",
    "operator",
    "out",
    "override",
    "params",
    "private",
    "protected",
    "public",
    "readonly",
    "ref",
    "return",
    "sbyte",
    "sealed",
    "short",
    "sizeof",
    "stackalloc",
    "static",
    "string",
    "struct",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "uint",
    "ulong",
    "unchecked",
    "unsafe",
    "ushort",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
];

/// Dart reserved words, per the Dart language specification (the always
/// reserved set plus the builtin identifiers that cannot name locals).
pub const DART_KEYWORDS: &[&str] = &[
    "assert", "break", "case", "catch", "class", "const", "continue", "default", "do", "else",
    "enum", "extends", "false", "final", "finally", "for", "if", "in", "is", "new", "null",
    "rethrow", "return", "super", "switch", "this", "throw", "true", "try", "var", "void", "while",
    "with",
];

/// Ruby reserved words, per the Ruby language documentation.
pub const RUBY_KEYWORDS: &[&str] = &[
    "BEGIN", "END", "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do",
    "else", "elsif", "end", "ensure", "false", "for", "if", "in", "module", "next", "nil", "not",
    "or", "redo", "rescue", "retry", "return", "self", "super", "then", "true", "undef", "unless",
    "until", "when", "while", "yield",
];

/// JavaScript and TypeScript reserved words (ECMAScript reserved words plus
/// the strict-mode and TypeScript-only reservations that cannot name
/// parameters or properties-in-shorthand).
pub const JS_KEYWORDS: &[&str] = &[
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "static",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
];

/// C keywords, per C17. The runtime's own symbols need no entries here
/// because every emitted C name carries the symbol prefix.
pub const C_KEYWORDS: &[&str] = &[
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
    "union", "unsigned", "void", "volatile", "while",
];

/// C++ keywords, per C++20 (a superset of the C set relevant to emitted
/// declarations).
pub const CPP_KEYWORDS: &[&str] = &[
    "alignas",
    "alignof",
    "and",
    "asm",
    "auto",
    "bitand",
    "bitor",
    "bool",
    "break",
    "case",
    "catch",
    "char",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "compl",
    "concept",
    "const",
    "consteval",
    "constexpr",
    "constinit",
    "continue",
    "decltype",
    "default",
    "delete",
    "do",
    "double",
    "dynamic_cast",
    "else",
    "enum",
    "explicit",
    "export",
    "extern",
    "false",
    "float",
    "for",
    "friend",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "mutable",
    "namespace",
    "new",
    "noexcept",
    "not",
    "nullptr",
    "operator",
    "or",
    "private",
    "protected",
    "public",
    "register",
    "requires",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "static_assert",
    "static_cast",
    "struct",
    "switch",
    "template",
    "this",
    "throw",
    "true",
    "try",
    "typedef",
    "typeid",
    "typename",
    "union",
    "unsigned",
    "using",
    "virtual",
    "void",
    "volatile",
    "while",
    "xor",
];

/// `true` when `name` is reserved in the language whose keyword table is
/// `keywords`. Tables are sorted case-sensitively, so lookup is a binary
/// search.
pub fn is_reserved(name: &str, keywords: &[&str]) -> bool {
    keywords.binary_search(&name).is_ok()
}

/// Escape `name` for the language whose keyword table is `keywords`: a
/// reserved name gains a trailing `_`, anything else passes through.
///
/// The trailing underscore is legal in every supported target, never
/// collides with another IDL name (validation would have flagged the
/// duplicate spelling), and is idempotent because `name_` is never itself a
/// keyword.
pub fn escape_ident(name: &str, keywords: &[&str]) -> String {
    if is_reserved(name, keywords) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_for_binary_search() {
        for (label, table) in [
            ("python", PYTHON_KEYWORDS),
            ("go", GO_KEYWORDS),
            ("swift", SWIFT_KEYWORDS),
            ("kotlin", KOTLIN_KEYWORDS),
            ("csharp", CSHARP_KEYWORDS),
            ("dart", DART_KEYWORDS),
            ("ruby", RUBY_KEYWORDS),
            ("js", JS_KEYWORDS),
            ("c", C_KEYWORDS),
            ("cpp", CPP_KEYWORDS),
        ] {
            assert!(
                table.windows(2).all(|w| w[0] < w[1]),
                "{label} keyword table must be sorted and duplicate-free"
            );
        }
    }

    #[test]
    fn reserved_names_gain_a_trailing_underscore() {
        assert_eq!(escape_ident("type", GO_KEYWORDS), "type_");
        assert_eq!(escape_ident("class", PYTHON_KEYWORDS), "class_");
        assert_eq!(escape_ident("end", RUBY_KEYWORDS), "end_");
        assert_eq!(escape_ident("value", GO_KEYWORDS), "value");
    }

    #[test]
    fn escape_is_idempotent() {
        let once = escape_ident("import", JS_KEYWORDS);
        assert_eq!(escape_ident(&once, JS_KEYWORDS), once);
    }
}
