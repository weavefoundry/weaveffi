//! The validator's test suite.
//!
//! Every case is a short YAML document paired with the error variant it must
//! (or must not) produce, so adding a rule means adding one table row.

use super::*;
use crate::ir::CURRENT_SCHEMA_VERSION;
use crate::parse::parse_api_str;

/// Parse a YAML body (without the `version:` line) and validate it.
fn check(body: &str) -> Result<Model, ValidationDiagnostics> {
    let doc = format!("version: \"{CURRENT_SCHEMA_VERSION}\"\n{body}");
    let api = parse_api_str(&doc, "yaml").expect("fixture must parse");
    validate(&api, &Identity::named("api"), None)
}

/// Validate a full YAML document with its source attached, returning the
/// line and underlined text of every diagnostic.
fn spans(doc: &str) -> Vec<(String, String)> {
    let api = parse_api_str(doc, "yaml").expect("fixture must parse");
    let sources = Sources::single("api.yml", doc);
    let err = validate(&api, &Identity::named("api"), Some(&sources)).unwrap_err();
    err.diagnostics
        .iter()
        .map(|d| {
            let span = d.span.expect("span located");
            let line = doc[..span.offset()].lines().count();
            (
                variant(&d.error),
                format!("{line}:{}", &doc[span.offset()..span.offset() + span.len()]),
            )
        })
        .collect()
}

/// The variant names of every error the document produced.
fn errors(body: &str) -> Vec<String> {
    match check(body) {
        Ok(_) => vec![],
        Err(d) => d.diagnostics.iter().map(|d| variant(&d.error)).collect(),
    }
}

/// The variant name of a [`ValidationError`].
fn variant(e: &ValidationError) -> String {
    let dbg = format!("{e:?}");
    dbg.split(['(', ' ']).next().unwrap_or(&dbg).to_string()
}

#[test]
fn valid_documents_pass() {
    for body in [
        "functions: [{ name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }]",
        r#"
records:
  - { name: Node, fields: [{ name: children, type: "[Node]" }, { name: index, type: "{string:Node}" }] }
interfaces:
  - name: Store
    constructors: [{ name: open }]
    methods: [{ name: get, params: [{ name: key, type: string }], return: "Node?" }]
callbacks:
  - name: Listener
    methods: [{ name: on_node, params: [{ name: node, type: Node }], return: "Store?" }]
functions:
  - { name: watch, params: [{ name: l, type: "Listener?" }], return: "iter<Node>" }
  - { name: fetch, return: "[u64]", async: true, cancellable: true }
"#,
        // A recursive record with no object may sit in an error payload.
        r#"
records:
  - { name: Tree, fields: [{ name: kids, type: "[Tree]" }, { name: label, type: string }] }
errors:
  - { name: E, codes: [{ name: Bad, code: 1, message: bad, fields: [{ name: at, type: Tree }] }] }
"#,
    ] {
        check(body).unwrap_or_else(|e| panic!("{body}: {:?}", e.diagnostics));
    }
}

#[test]
fn every_rule_fires() {
    let cases: &[(&str, &str)] = &[
        ("ReservedKeyword", "functions: [{ name: match }]"),
        ("ReservedKeyword", "errors: [{ name: any, codes: [{ name: A, code: 1, message: a }] }]"),
        ("InvalidIdentifier", "functions: [{ name: 9lives }]"),
        ("InvalidIdentifier", "records: [{ name: R, fields: [{ name: a-b, type: i32 }] }]"),
        (
            "DuplicateName",
            "records: [{ name: Thing, fields: [{ name: x, type: i32 }] }]\nfunctions: [{ name: Thing }]",
        ),
        ("DuplicateName", "functions: [{ name: f }, { name: f }]"),
        (
            "DuplicateErrorCodeName",
            "errors:\n  - { name: A, codes: [{ name: Gone, code: 1, message: a }] }\n  - { name: B, codes: [{ name: Gone, code: 1, message: b }] }",
        ),
        (
            "SymbolCollision",
            "functions: [{ name: f, async: true }, { name: f_callback }]",
        ),
        (
            "SymbolCollision",
            "interfaces: [{ name: Store, statics: [{ name: get }] }]\nfunctions: [{ name: Store_get }]",
        ),
        ("SymbolCollision", "functions: [{ name: opt_i32 }]"),
        ("SlotCollision", "functions: [{ name: f, params: [{ name: out_err, type: i32 }] }]"),
        (
            "SlotCollision",
            "callbacks: [{ name: L, methods: [{ name: m, params: [{ name: ctx, type: i32 }] }] }]",
        ),
        (
            "DuplicateParamName",
            "functions: [{ name: f, params: [{ name: x, type: i32 }, { name: x, type: i32 }] }]",
        ),
        ("CancellableNotAsync", "functions: [{ name: f, cancellable: true }]"),
        ("UnknownErrorDomain", "functions: [{ name: f, throws: Nope }]"),
        ("AsyncIteratorReturn", "functions: [{ name: f, return: \"iter<i32>\", async: true }]"),
        (
            "DuplicateErrorCode",
            "errors: [{ name: E, codes: [{ name: A, code: 1, message: a }, { name: B, code: 1, message: b }] }]",
        ),
        ("InvalidErrorCode", "errors: [{ name: E, codes: [{ name: A, code: 0, message: a }] }]"),
        ("EmptyRecord", "records: [{ name: R, fields: [] }]"),
        (
            "DuplicateRecordField",
            "records: [{ name: R, fields: [{ name: x, type: i32 }, { name: x, type: i32 }] }]",
        ),
        ("RecursiveType", "records: [{ name: Node, fields: [{ name: next, type: Node }] }]"),
        (
            "RecursiveType",
            "records:\n  - { name: A, fields: [{ name: b, type: \"B?\" }] }\n  - { name: B, fields: [{ name: a, type: A }] }",
        ),
        (
            "RecursiveType",
            "enums: [{ name: Expr, variants: [{ name: Neg, fields: [{ name: e, type: Expr }] }] }]",
        ),
        ("EmptyEnum", "enums: [{ name: E, variants: [] }]"),
        ("DuplicateEnumVariant", "enums: [{ name: E, variants: [{ name: A }, { name: A }] }]"),
        (
            "DuplicateEnumVariantField",
            "enums: [{ name: E, variants: [{ name: A, fields: [{ name: x, type: i32 }, { name: x, type: i32 }] }] }]",
        ),
        ("DuplicateEnumValue", "enums: [{ name: E, variants: [{ name: A, value: 1 }, { name: B, value: 0 }, { name: C }] }]"),
        ("EmptyInterface", "interfaces: [{ name: I }]"),
        ("DuplicateInterfaceMember", "interfaces: [{ name: I, methods: [{ name: m }], statics: [{ name: m }] }]"),
        ("ConstructorHasReturn", "interfaces: [{ name: I, constructors: [{ name: new, return: i32 }] }]"),
        ("AsyncConstructor", "interfaces: [{ name: I, constructors: [{ name: new, async: true }] }]"),
        ("EmptyCallbackInterface", "callbacks: [{ name: L, methods: [] }]"),
        ("DuplicateCallbackMethod", "callbacks: [{ name: L, methods: [{ name: m }, { name: m }] }]"),
        ("InvalidCallbackMethod", "callbacks: [{ name: L, methods: [{ name: m, async: true }] }]"),
        ("InvalidCallbackMethod", "callbacks: [{ name: L, methods: [{ name: m, return: \"iter<i32>\" }] }]"),
        ("UnknownTypeRef", "functions: [{ name: f, return: Nope }]"),
        (
            "ErrorDomainAsType",
            "errors: [{ name: E, codes: [{ name: A, code: 1, message: a }] }]\nfunctions: [{ name: f, return: E }]",
        ),
        ("UnsupportedPrimitive", "functions: [{ name: f, return: usize }]"),
        ("InvalidMapKey", "functions: [{ name: f, return: \"{f64:i32}\" }]"),
        (
            "InterfaceInInvalidPosition",
            "interfaces: [{ name: I, statics: [{ name: s }] }]\nfunctions: [{ name: f, return: \"{I:i32}\" }]",
        ),
        (
            "CallbackInterfaceInInvalidPosition",
            "callbacks: [{ name: L, methods: [{ name: m }] }]\nfunctions: [{ name: f, return: L }]",
        ),
        (
            "CallbackInterfaceInInvalidPosition",
            "callbacks: [{ name: L, methods: [{ name: m }] }]\nrecords: [{ name: R, fields: [{ name: l, type: \"[L]\" }] }]",
        ),
        (
            "ObjectInErrorPayload",
            "interfaces: [{ name: I, statics: [{ name: s }] }]\nerrors: [{ name: E, codes: [{ name: A, code: 1, message: a, fields: [{ name: i, type: I }] }] }]",
        ),
        // Through a list of records holding an optional object.
        (
            "ObjectInErrorPayload",
            "interfaces: [{ name: I, statics: [{ name: s }] }]\nrecords: [{ name: H, fields: [{ name: i, type: \"I?\" }] }]\nerrors: [{ name: E, codes: [{ name: A, code: 1, message: a, fields: [{ name: h, type: \"[H]\" }] }] }]",
        ),
        // Through records that contain each other, and a rich enum.
        (
            "ObjectInErrorPayload",
            "interfaces: [{ name: I, statics: [{ name: s }] }]\nrecords: [{ name: D, fields: [{ name: f, type: \"[F]\" }] }, { name: F, fields: [{ name: d, type: \"[D]\" }, { name: i, type: \"I?\" }] }]\nerrors: [{ name: E, codes: [{ name: A, code: 1, message: a, fields: [{ name: d, type: D }] }] }]",
        ),
        (
            "ObjectInErrorPayload",
            "interfaces: [{ name: I, statics: [{ name: s }] }]\nenums: [{ name: V, variants: [{ name: N, fields: [{ name: v, type: \"[V]\" }] }, { name: O, fields: [{ name: i, type: I }] }] }]\nerrors: [{ name: E, codes: [{ name: A, code: 1, message: a, fields: [{ name: v, type: V }] }] }]",
        ),
        ("IteratorInInvalidPosition", "functions: [{ name: f, params: [{ name: x, type: \"iter<i32>\" }] }]"),
        ("IteratorInInvalidPosition", "functions: [{ name: f, return: \"[iter<i32>]\" }]"),
    ];
    for (expected, body) in cases {
        let got = errors(body);
        assert!(
            got.iter().any(|v| v == expected),
            "{expected} expected from:\n{body}\ngot {got:?}"
        );
    }
}

#[test]
fn unsupported_schema_version_short_circuits() {
    let api = parse_api_str("version: \"0.12.0\"\nfunctions: [{ name: f }]", "yaml").unwrap();
    let err = validate(&api, &Identity::named("api"), None).unwrap_err();
    assert_eq!(err.diagnostics.len(), 1);
    assert!(matches!(
        &err.first().error,
        ValidationError::UnsupportedSchemaVersion { version, .. } if version == "0.12.0"
    ));
}

#[test]
fn all_violations_are_reported_together_and_display_shows_the_first() {
    let err = check(
        "functions:\n  - { name: f, return: Nope }\n  - { name: f, throws: Missing }\nrecords: [{ name: S, fields: [] }]",
    )
    .unwrap_err();
    let got: Vec<String> = err.diagnostics.iter().map(|d| variant(&d.error)).collect();
    for expected in [
        "DuplicateName",
        "UnknownTypeRef",
        "UnknownErrorDomain",
        "EmptyRecord",
    ] {
        assert!(
            got.iter().any(|v| v == expected),
            "{expected} missing: {got:?}"
        );
    }
    let text = err.to_string();
    assert_eq!(text, err.first().to_string());
    assert_eq!(
        miette::Diagnostic::related(&err).map(Iterator::count),
        Some(err.diagnostics.len() - 1)
    );
}

#[test]
fn reserved_families_use_the_real_prefix() {
    let api = parse_api_str(
        &format!("version: \"{CURRENT_SCHEMA_VERSION}\"\nfunctions: [{{ name: contract_x }}]"),
        "yaml",
    )
    .unwrap();
    let err = validate(&api, &Identity::named("acme"), None).unwrap_err();
    assert!(err.to_string().contains("'acme_contract_x'"), "{err}");
}

#[test]
fn spans_underline_the_offending_text_in_its_declaration() {
    let doc = format!(
        r#"version: "{CURRENT_SCHEMA_VERSION}"
records:
  - name: x
    fields: [{{ name: Nope, type: i32 }}]
functions:
  - name: f
    params: [{{ name: x, type: i32 }}]
  - name: g
    params: [{{ name: x, type: i32 }}, {{ name: x, type: Nope }}]
  - name: h
    return: usize
interfaces:
  - name: Store
    methods:
      - {{ name: h }}
      - {{ name: put, cancellable: true }}
"#
    );
    assert_eq!(
        spans(&doc),
        [
            ("DuplicateParamName".to_string(), "9:x".to_string()),
            ("UnknownTypeRef".to_string(), "9:Nope".to_string()),
            ("UnsupportedPrimitive".to_string(), "11:usize".to_string()),
            ("CancellableNotAsync".to_string(), "16:put".to_string()),
        ]
    );
}

#[test]
fn diagnostics_are_located_in_the_importing_file() {
    let main = format!(
        "version: \"{CURRENT_SCHEMA_VERSION}\"\nfunctions: [{{ name: f, return: Item }}]\n"
    );
    let types = format!(
        "version: \"{CURRENT_SCHEMA_VERSION}\"\nrecords:\n  - name: Item\n    fields: [{{ name: a, type: Nope }}]\n"
    );
    let mut api = parse_api_str(&main, "yaml").unwrap();
    api.merge(parse_api_str(&types, "yaml").unwrap());
    let mut sources = Sources::single("main.yml", &main);
    sources.files.push(("types.yml".into(), types.clone()));
    sources.origin.insert("Item".into(), 1);
    let err = validate(&api, &Identity::named("api"), Some(&sources)).unwrap_err();
    let d = err.first();
    let span = d.span.expect("span located");
    assert_eq!(&types[span.offset()..span.offset() + span.len()], "Nope");
}

#[test]
fn warnings_are_advisory() {
    let api = parse_api_str(
        &format!("version: \"{CURRENT_SCHEMA_VERSION}\"\nfunctions: [{{ name: f, return: \"[[[[i32]]]]\" }}]"),
        "yaml",
    )
    .unwrap();
    assert!(validate(&api, &Identity::named("api"), None).is_ok());
    let warnings = collect_warnings(&api);
    assert!(warnings
        .iter()
        .any(|w| matches!(w, ValidationWarning::DeepNesting { depth: 4, .. })));
}
