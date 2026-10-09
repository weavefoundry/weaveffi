//! The validator's test suite.
//!
//! Every case is a short YAML document paired with the error variant it must
//! (or must not) produce, so adding a rule means adding one table row and a
//! few lines of IDL rather than forty lines of struct literals. Spans and
//! snippets are exercised by the CLI tests against on-disk files.

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
/// underlined text of every diagnostic.
fn spans(doc: &str) -> Vec<(String, String)> {
    let api = parse_api_str(doc, "yaml").expect("fixture must parse");
    let err = validate(&api, &Identity::named("api"), Some(("api.yml", doc))).unwrap_err();
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

/// Variant discriminator names of every error the document produced.
fn errors(body: &str) -> Vec<String> {
    match check(body) {
        Ok(_) => vec![],
        Err(d) => d.diagnostics.iter().map(|d| variant(&d.error)).collect(),
    }
}

/// The variant name of a [`ValidationError`] (the debug spelling up to the
/// first `(` or ` {`).
fn variant(e: &ValidationError) -> String {
    let dbg = format!("{e:?}");
    dbg.split(['(', ' ']).next().unwrap_or(&dbg).to_string()
}

const MATH: &str = r#"
modules:
  - name: math
    functions:
      - name: add
        params: [{ name: a, type: i32 }, { name: b, type: i32 }]
        return: i32
"#;

#[test]
fn valid_documents_pass() {
    let ok: &[&str] = &[
        MATH,
        r#"
modules:
  - name: shared
    enums:
      - name: Status
        variants: [{ name: Ok, value: 0 }, { name: Bad, value: 1 }]
      - name: Shape
        variants:
          - { name: Dot, value: 0 }
          - { name: Circle, value: 1, fields: [{ name: r, type: f64 }] }
    structs:
      - name: Point
        fields:
          - { name: x, type: f64 }
          - { name: status, type: Status }
          - { name: shape, type: "Shape?" }
          - { name: tags, type: "[string]" }
          - { name: by_status, type: "{Status:i32}" }
  - name: geo
    errors:
      name: GeoError
      codes:
        - { name: NotFound, code: 1, message: "missing" }
        - { name: Bad, code: 2, message: "bad", fields: [{ name: why, type: string }] }
    interfaces:
      - name: Store
        constructors: [{ name: open, params: [{ name: path, type: string }] }]
        methods:
          - { name: get, params: [{ name: k, type: string }], return: "Point?", throws: true }
          - { name: scan, params: [], return: "iter<Point>" }
          - { name: stores, params: [], return: "iter<Store>" }
          - { name: sibling, params: [{ name: other, type: "Store?" }], return: Store }
          - { name: children, params: [], return: "[Store]" }
          - { name: by_name, params: [], return: "{string:Store}" }
          - { name: watch, params: [{ name: listener, type: PointListener }] }
        statics: [{ name: version, params: [], return: string }]
    structs:
      - name: Owner
        fields: [{ name: store, type: "Store?" }, { name: all, type: "[Store]" }]
    callback_interfaces:
      - name: PointListener
        methods:
          - name: on_point
            params: [{ name: p, type: Point }, { name: raw, type: bytes }, { name: from, type: Store }]
          - { name: wants_more, params: [{ name: seen, type: u32 }], return: bool }
          - { name: status, params: [], return: Status }
          - { name: label, params: [], return: string, throws: true }
          - { name: nearest, params: [], return: "Point?" }
          - { name: store, params: [], return: "Store?" }
          - { name: payload, params: [], return: bytes }
    functions:
      - { name: fetch, params: [{ name: id, type: i64 }], return: "[Point]", async: true, cancellable: true }
      - { name: subscribe, params: [{ name: l, type: PointListener }, { name: s, type: Store }] }
      - { name: maybe_subscribe, params: [{ name: l, type: "PointListener?" }] }
    modules:
      - name: inner
        functions:
          - { name: fails, params: [], throws: true }
"#,
    ];
    for doc in ok {
        assert_eq!(errors(doc), Vec::<String>::new(), "{doc}");
    }
}

#[test]
fn every_rule_fires() {
    let cases: &[(&str, &str)] = &[
        (
            "DuplicateModuleName",
            "modules: [{ name: a, functions: [] }, { name: a, functions: [] }]",
        ),
        (
            "NoModuleName",
            "modules: [{ name: '  ', functions: [] }]",
        ),
        (
            "InvalidModuleName",
            "modules: [{ name: 9lives, functions: [] }]",
        ),
        (
            "InvalidModuleName",
            "modules: [{ name: async, functions: [] }]",
        ),
        (
            "DuplicateFunctionName",
            r#"
modules:
  - name: m
    functions:
      - { name: f, params: [] }
      - { name: f, params: [] }
"#,
        ),
        (
            "DuplicateFunctionName",
            r#"
modules:
  - name: a
    functions: [{ name: open, params: [] }]
  - name: b
    modules:
      - name: c
        functions: [{ name: open, params: [] }]
"#,
        ),
        (
            "DuplicateParamName",
            r#"
modules:
  - name: m
    functions:
      - { name: f, params: [{ name: x, type: i32 }, { name: x, type: i32 }] }
"#,
        ),
        (
            "ReservedKeyword",
            "modules: [{ name: m, functions: [{ name: match, params: [] }] }]",
        ),
        (
            "InvalidIdentifier",
            "modules: [{ name: m, functions: [{ name: 'has space', params: [] }] }]",
        ),
        (
            "ErrorDomainMissingName",
            r#"
modules:
  - name: m
    errors: { name: ' ', codes: [] }
"#,
        ),
        (
            "DuplicateErrorCodeName",
            r#"
modules:
  - name: m
    errors:
      name: E
      codes:
        - { name: A, code: 1, message: a }
        - { name: A, code: 2, message: a }
"#,
        ),
        (
            "DuplicateErrorCode",
            r#"
modules:
  - name: m
    errors:
      name: E
      codes:
        - { name: A, code: 1, message: a }
        - { name: B, code: 1, message: b }
"#,
        ),
        (
            "InvalidErrorCode",
            r#"
modules:
  - name: m
    errors:
      name: E
      codes: [{ name: A, code: 0, message: a }]
"#,
        ),
        (
            "InvalidErrorCode",
            r#"
modules:
  - name: m
    errors:
      name: E
      codes: [{ name: A, code: -2, message: a }]
"#,
        ),
        (
            "NameCollisionWithErrorDomain",
            r#"
modules:
  - name: m
    functions: [{ name: E, params: [] }]
    errors: { name: E, codes: [{ name: A, code: 1, message: a }] }
"#,
        ),
        (
            "NameCollisionWithErrorDomain",
            r#"
modules:
  - name: m
    errors: { name: E, codes: [{ name: A, code: 1, message: a }] }
  - name: n
    functions: [{ name: E, params: [] }]
"#,
        ),
        (
            "SymbolCollision",
            r#"
modules:
  - name: m
    functions:
      - { name: foo, async: true, params: [{ name: a, type: i32 }] }
      - { name: foo_callback, params: [] }
"#,
        ),
        (
            "SymbolCollision",
            r#"
modules:
  - name: m
    functions: [{ name: x_y, params: [] }]
    modules:
      - name: x
        functions: [{ name: y, params: [] }]
"#,
        ),
        (
            "SymbolCollision",
            r#"
modules:
  - name: m
    functions: [{ name: x_y, params: [] }]
  - name: m_x
    functions: [{ name: y, params: [] }]
"#,
        ),
        (
            "SymbolCollision",
            r#"
modules:
  - name: list
    functions: [{ name: names, params: [] }]
"#,
        ),
        (
            "CancellableNotAsync",
            r#"
modules:
  - name: m
    functions: [{ name: f, cancellable: true, params: [] }]
"#,
        ),
        (
            "InvalidMapKey",
            r#"
modules:
  - name: m
    functions: [{ name: f, params: [{ name: x, type: "{f64:i32}" }] }]
"#,
        ),
        (
            "QualifiedTypeRef",
            r#"
modules:
  - name: m
    structs: [{ name: P, fields: [{ name: v, type: i32 }] }]
    functions: [{ name: f, params: [{ name: p, type: m.P }] }]
"#,
        ),
        (
            "UnsupportedPrimitive",
            "modules: [{ name: m, functions: [{ name: f, params: [{ name: n, type: usize }] }] }]",
        ),
        (
            "UnsupportedPrimitive",
            "modules: [{ name: m, structs: [{ name: S, fields: [{ name: c, type: '[char]' }] }] }]",
        ),
        (
            "SymbolCollision",
            r#"
modules:
  - name: m
    functions: [{ name: Store_get, params: [] }]
    interfaces:
      - name: Store
        methods: [{ name: get, params: [] }]
"#,
        ),
        (
            "SymbolCollision",
            r#"
modules:
  - name: m
    functions: [{ name: Store_destroy, params: [] }]
    interfaces:
      - name: Store
        methods: [{ name: get, params: [] }]
"#,
        ),
        (
            "SymbolCollision",
            r#"
modules:
  - name: m
    functions: [{ name: Store_clone, params: [] }]
    interfaces:
      - name: Store
        methods: [{ name: get, params: [] }]
"#,
        ),
        (
            "ThrowsWithoutErrorDomain",
            "modules: [{ name: m, functions: [{ name: f, params: [], throws: true }] }]",
        ),
        (
            "DuplicateTypeName",
            r#"
modules:
  - name: a
    structs: [{ name: Config, fields: [{ name: x, type: i32 }] }]
  - name: b
    enums: [{ name: Config, variants: [{ name: A, value: 0 }] }]
"#,
        ),
        (
            "DuplicateTypeName",
            r#"
modules:
  - name: a
    structs: [{ name: E, fields: [{ name: x, type: i32 }] }]
    errors: { name: E, codes: [{ name: A, code: 1, message: a }] }
"#,
        ),
        (
            "DuplicateErrorCodeName",
            r#"
modules:
  - name: a
    errors: { name: AErr, codes: [{ name: NotFound, code: 1, message: a }] }
  - name: b
    errors: { name: BErr, codes: [{ name: NotFound, code: 1, message: b }] }
"#,
        ),
        (
            "DuplicateTypeName",
            r#"
modules:
  - name: m
    structs:
      - { name: S, fields: [{ name: x, type: i32 }] }
      - { name: S, fields: [{ name: x, type: i32 }] }
"#,
        ),
        (
            "DuplicateStructField",
            r#"
modules:
  - name: m
    structs:
      - { name: S, fields: [{ name: x, type: i32 }, { name: x, type: i32 }] }
"#,
        ),
        (
            "EmptyStruct",
            "modules: [{ name: m, structs: [{ name: S, fields: [] }] }]",
        ),
        (
            "DuplicateTypeName",
            r#"
modules:
  - name: m
    enums:
      - { name: E, variants: [{ name: A, value: 0 }] }
      - { name: E, variants: [{ name: A, value: 0 }] }
"#,
        ),
        (
            "EmptyEnum",
            "modules: [{ name: m, enums: [{ name: E, variants: [] }] }]",
        ),
        (
            "DuplicateEnumVariant",
            r#"
modules:
  - name: m
    enums:
      - { name: E, variants: [{ name: A, value: 0 }, { name: A, value: 1 }] }
"#,
        ),
        (
            "DuplicateEnumVariantField",
            r#"
modules:
  - name: m
    enums:
      - name: E
        variants:
          - { name: A, value: 0, fields: [{ name: x, type: i32 }, { name: x, type: i32 }] }
"#,
        ),
        (
            "DuplicateEnumValue",
            r#"
modules:
  - name: m
    enums:
      - { name: E, variants: [{ name: A, value: 0 }, { name: B, value: 0 }] }
"#,
        ),
        (
            "DuplicateTypeName",
            r#"
modules:
  - name: m
    interfaces:
      - { name: I, methods: [{ name: a, params: [] }] }
      - { name: I, methods: [{ name: b, params: [] }] }
"#,
        ),
        (
            "DuplicateInterfaceMember",
            r#"
modules:
  - name: m
    interfaces:
      - name: I
        constructors: [{ name: a, params: [] }]
        methods: [{ name: a, params: [] }]
"#,
        ),
        (
            "EmptyInterface",
            "modules: [{ name: m, interfaces: [{ name: I }] }]",
        ),
        (
            "ConstructorHasReturn",
            r#"
modules:
  - name: m
    interfaces:
      - name: I
        constructors: [{ name: new, params: [], return: i32 }]
"#,
        ),
        (
            "AsyncConstructor",
            r#"
modules:
  - name: m
    interfaces:
      - name: I
        constructors: [{ name: new, params: [], async: true }]
"#,
        ),
        (
            "InterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    interfaces: [{ name: I, methods: [{ name: a, params: [] }] }]
    functions: [{ name: f, params: [{ name: xs, type: "{I:i32}" }] }]
"#,
        ),
        (
            "InterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    interfaces: [{ name: I, methods: [{ name: a, params: [] }] }]
    structs: [{ name: S, fields: [{ name: m, type: "{I:i32}" }] }]
"#,
        ),
        (
            "UnknownTypeRef",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: Nope }] }]",
        ),
        (
            "UnknownTypeRef",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: '[Nope?]' }] }]",
        ),
        (
            "InvalidMapKey",
            r#"
modules:
  - name: m
    structs: [{ name: S, fields: [{ name: x, type: i32 }] }]
    functions: [{ name: f, params: [], return: "{S:i32}" }]
"#,
        ),
        (
            "InvalidMapKey",
            r#"
modules:
  - name: m
    enums:
      - name: Rich
        variants: [{ name: A, value: 0, fields: [{ name: x, type: i32 }] }]
    functions: [{ name: f, params: [], return: "{Rich:i32}" }]
"#,
        ),
        (
            "InvalidMapKey",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: '{bytes:i32}' }] }]",
        ),
        (
            "DuplicateTypeName",
            r#"
modules:
  - name: m
    callback_interfaces:
      - { name: L, methods: [{ name: a, params: [] }] }
      - { name: L, methods: [{ name: b, params: [] }] }
"#,
        ),
        (
            "EmptyCallbackInterface",
            "modules: [{ name: m, callback_interfaces: [{ name: L, methods: [] }] }]",
        ),
        (
            "DuplicateCallbackMethod",
            r#"
modules:
  - name: m
    callback_interfaces:
      - { name: L, methods: [{ name: a, params: [] }, { name: a, params: [] }] }
"#,
        ),
        (
            "InvalidCallbackMethod",
            "modules: [{ name: m, callback_interfaces: [{ name: L, methods: [{ name: a, params: [], async: true }] }] }]",
        ),
        (
            "ThrowsWithoutErrorDomain",
            "modules: [{ name: m, callback_interfaces: [{ name: L, methods: [{ name: a, params: [], throws: true }] }] }]",
        ),
        (
            "InvalidCallbackMethod",
            "modules: [{ name: m, callback_interfaces: [{ name: L, methods: [{ name: a, params: [], cancellable: true }] }] }]",
        ),
        (
            "InvalidCallbackMethod",
            "modules: [{ name: m, callback_interfaces: [{ name: L, methods: [{ name: a, params: [], return: 'iter<i32>' }] }] }]",
        ),
        (
            "CallbackInterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    callback_interfaces: [{ name: L, methods: [{ name: a, params: [], return: "L?" }] }]
"#,
        ),
        (
            "CallbackInterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    callback_interfaces: [{ name: L, methods: [{ name: a, params: [] }] }]
    functions: [{ name: f, params: [], return: L }]
"#,
        ),
        (
            "CallbackInterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    callback_interfaces: [{ name: L, methods: [{ name: a, params: [] }] }]
    functions: [{ name: f, params: [{ name: l, type: "[L]" }] }]
"#,
        ),
        (
            "CallbackInterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    callback_interfaces: [{ name: L, methods: [{ name: a, params: [] }] }]
    structs: [{ name: S, fields: [{ name: l, type: L }] }]
"#,
        ),
        (
            "CallbackInterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    callback_interfaces:
      - { name: L, methods: [{ name: a, params: [{ name: other, type: L }] }] }
"#,
        ),
        (
            "IteratorInInvalidPosition",
            "modules: [{ name: m, callback_interfaces: [{ name: L, methods: [{ name: a, params: [{ name: x, type: 'iter<i32>' }] }] }] }]",
        ),
        (
            "IteratorInInvalidPosition",
            "modules: [{ name: m, functions: [{ name: f, params: [{ name: x, type: 'iter<i32>' }] }] }]",
        ),
        (
            "IteratorInInvalidPosition",
            "modules: [{ name: m, structs: [{ name: S, fields: [{ name: x, type: 'iter<i32>' }] }] }]",
        ),
        (
            "IteratorInInvalidPosition",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: '[iter<i32>]' }] }]",
        ),
        (
            "IteratorInInvalidPosition",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: 'iter<i32>?' }] }]",
        ),
        (
            "IteratorInInvalidPosition",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: 'iter<iter<i32>>' }] }]",
        ),
        (
            "AsyncIteratorReturn",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: 'iter<i32>', async: true }] }]",
        ),
        (
            "ReservedKeyword",
            "modules: [{ name: m, errors: { name: 'if', codes: [{ name: A, code: 1, message: a }] } }]",
        ),
        (
            "InvalidIdentifier",
            "modules: [{ name: m, errors: { name: E, codes: [{ name: '1bad', code: 1, message: a }] } }]",
        ),
    ];
    for (expected, doc) in cases {
        let got = errors(doc);
        assert!(
            got.iter().any(|v| v == expected),
            "expected {expected}, got {got:?} for:\n{doc}"
        );
    }
}

#[test]
fn unsupported_schema_version_short_circuits() {
    let api = parse_api_str(
        "version: '0.1.0'\nmodules: [{ name: a, functions: [] }, { name: a, functions: [] }]",
        "yaml",
    )
    .unwrap();
    let err = validate(&api, &Identity::named("api"), None).unwrap_err();
    assert_eq!(err.diagnostics.len(), 1);
    assert!(matches!(
        &err.first().error,
        ValidationError::UnsupportedSchemaVersion { version, .. } if version == "0.1.0"
    ));
}

#[test]
fn all_violations_are_reported_together() {
    let got = errors(
        r#"
modules:
  - name: m
    functions:
      - { name: f, params: [], return: Nope }
      - { name: f, params: [], throws: true }
    structs: [{ name: S, fields: [] }]
"#,
    );
    for expected in [
        "DuplicateFunctionName",
        "UnknownTypeRef",
        "ThrowsWithoutErrorDomain",
        "EmptyStruct",
    ] {
        assert!(
            got.iter().any(|v| v == expected),
            "{expected} missing from {got:?}"
        );
    }
}

#[test]
fn diagnostics_render_every_message_and_related() {
    let err = check("modules: [{ name: a, functions: [] }, { name: a, functions: [] }, { name: a, functions: [] }]")
        .unwrap_err();
    assert_eq!(err.diagnostics.len(), 2);
    let text = err.to_string();
    assert_eq!(text.lines().count(), 2);
    assert!(text.contains("duplicate module name: a"));
    assert_eq!(err.related().map(Iterator::count), Some(1));
    assert!(err.help().is_some());
}

#[test]
fn global_duplicates_are_reported_once() {
    let got = errors(
        r#"
modules:
  - name: m
    structs:
      - { name: S, fields: [{ name: x, type: i32 }] }
      - { name: S, fields: [{ name: x, type: i32 }] }
    functions:
      - { name: f, params: [] }
      - { name: f, params: [] }
    errors:
      name: E
      codes:
        - { name: A, code: 1, message: a }
        - { name: A, code: 2, message: a }
"#,
    );
    assert_eq!(
        got,
        [
            "DuplicateTypeName",
            "DuplicateFunctionName",
            "DuplicateErrorCodeName"
        ]
    );
}

#[test]
fn symbol_collisions_use_the_real_prefix() {
    // `acme_list_*` is reserved by the C buffer helpers only under the
    // `acme` prefix the library actually uses.
    let doc = format!(
        "version: \"{CURRENT_SCHEMA_VERSION}\"\nmodules: [{{ name: list, functions: [{{ name: names }}] }}]"
    );
    let api = parse_api_str(&doc, "yaml").unwrap();
    let err = validate(&api, &Identity::named("acme"), None).unwrap_err();
    assert!(err.to_string().contains("'acme_list_names'"), "{err}");
}

#[test]
fn buffer_header_structs_and_codecs_are_reserved_symbols() {
    // The C buffer header declares a struct for every record, rich enum,
    // and error code with fields, each with `_write`, `_read`, `_decode`,
    // and `_free`; a function spelled like any of them would redefine it.
    let cases = [
        (
            "errors: { name: E, codes: [{ name: Busy, code: 1, message: b, fields: [{ name: n, type: i32 }] }] }\n    functions: [{ name: E_Busy_payload }]",
            "'kv_m_E_Busy_payload'",
            "the payload struct of error code 'm.E.Busy'",
        ),
        (
            "errors: { name: E, codes: [{ name: Busy, code: 1, message: b, fields: [{ name: n, type: i32 }] }] }\n    functions: [{ name: E_Busy_payload_read }]",
            "'kv_m_E_Busy_payload_read'",
            "codec 'kv_m_E_Busy_payload_read'",
        ),
        (
            "structs: [{ name: S, fields: [{ name: x, type: i32 }] }]\n    functions: [{ name: S_free }]",
            "'kv_m_S_free'",
            "of record 'm.S'",
        ),
        (
            "enums: [{ name: Shape, variants: [{ name: Dot, value: 0, fields: [{ name: r, type: f64 }] }] }]\n    functions: [{ name: Shape_decode }]",
            "'kv_m_Shape_decode'",
            "of enum 'm.Shape'",
        ),
    ];
    for (decls, symbol, origin) in cases {
        let doc =
            format!("version: \"{CURRENT_SCHEMA_VERSION}\"\nmodules:\n  - name: m\n    {decls}\n");
        let api = parse_api_str(&doc, "yaml").unwrap();
        let err = validate(&api, &Identity::named("kv"), None).unwrap_err();
        let text = err.to_string();
        assert!(text.contains(symbol) && text.contains(origin), "{text}");
    }
    // An error code without fields has no payload struct.
    let doc = format!(
        "version: \"{CURRENT_SCHEMA_VERSION}\"\nmodules:\n  - name: m\n    errors: {{ name: E, codes: [{{ name: Busy, code: 1, message: b }}] }}\n    functions: [{{ name: E_Busy_payload }}]\n"
    );
    let api = parse_api_str(&doc, "yaml").unwrap();
    assert!(validate(&api, &Identity::named("kv"), None).is_ok());
}

#[test]
fn source_spans_underline_the_offending_identifier() {
    let src = format!(
        "version: \"{CURRENT_SCHEMA_VERSION}\"\nmodules:\n  - name: \"dup\"\n  - name: \"dup\"\n"
    );
    let api = parse_api_str(&src, "yaml").unwrap();
    let err = validate(&api, &Identity::named("api"), Some(("api.yml", &src))).unwrap_err();
    let d = err.first();
    let span = d.span.expect("span located");
    assert_eq!(span.offset(), src.rfind("\"dup\"").unwrap());
    assert_eq!(&src[span.offset()..span.offset() + span.len()], "\"dup\"");
    assert!(d.labels().is_some());
    assert!(d.source_code().is_some());
}

#[test]
fn spans_are_scoped_to_the_enclosing_declaration() {
    let doc = format!(
        r#"version: "{CURRENT_SCHEMA_VERSION}"
modules:
  - name: a
    structs:
      - name: x
        fields: [{{ name: Nope, type: i32 }}]
    functions:
      - name: f
        params: [{{ name: x, type: i32 }}]
      - name: g
        params: [{{ name: x, type: i32 }}, {{ name: x, type: Nope }}]
  - name: b
    functions:
      - name: h
        params: [{{ name: s, type: a.x }}]
        return: usize
  - name: c
    interfaces:
      - name: Store
        methods:
          - {{ name: h }}
          - {{ name: put, cancellable: true }}
  - name: c
"#
    );
    assert_eq!(
        spans(&doc),
        [
            ("DuplicateParamName".to_string(), "11:x".to_string()),
            ("UnknownTypeRef".to_string(), "11:Nope".to_string()),
            ("QualifiedTypeRef".to_string(), "15:a.x".to_string()),
            ("UnsupportedPrimitive".to_string(), "16:usize".to_string()),
            ("CancellableNotAsync".to_string(), "22:put".to_string()),
            ("DuplicateModuleName".to_string(), "23:c".to_string()),
        ]
    );
}

#[test]
fn model_resolves_bare_names() {
    let model = check(
        r#"
modules:
  - name: shared
    enums: [{ name: Status, variants: [{ name: Ok, value: 0 }] }]
    structs: [{ name: Point, fields: [{ name: x, type: f64 }] }]
    interfaces: [{ name: Store, methods: [{ name: get, params: [] }] }]
    callback_interfaces: [{ name: Watcher, methods: [{ name: on, params: [] }] }]
  - name: orders
    functions:
      - name: f
        params: [{ name: s, type: Store }, { name: st, type: Status }, { name: w, type: Watcher }]
        return: "[Point]"
"#,
    )
    .unwrap();
    use crate::ty::Ty;
    let f = &model.modules[1].functions[0];
    assert_eq!(f.params[0].ty, Ty::Interface("Store".into()));
    assert_eq!(f.params[1].ty, Ty::Enum("Status".into()));
    assert_eq!(f.params[2].ty, Ty::CallbackInterface("Watcher".into()));
    assert_eq!(f.ret, Some(Ty::List(Box::new(Ty::Record("Point".into())))));
    assert_eq!(model.owner("Point").dot_path, "shared");
    assert_eq!(model.prefix(), "api");
    assert_eq!(model.version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn warnings_are_advisory() {
    let body = r#"
modules:
  - name: m
    enums:
      - name: Big
        variants:
          - { name: A, value: 0 }
    functions:
      - { name: deep, params: [{ name: x, type: "[[[[i32]]]]" }] }
      - { name: fire, params: [], async: true }
      - { name: old, params: [], deprecated: "use new" }
    modules:
      - name: documented
        doc: Has a module doc.
        functions: [{ name: f, params: [] }]
"#;
    check(body).unwrap();
    let doc = format!("version: \"{CURRENT_SCHEMA_VERSION}\"\n{body}");
    let warnings = collect_warnings(&parse_api_str(&doc, "yaml").unwrap());
    let kinds: Vec<String> = warnings
        .iter()
        .map(|w| format!("{w:?}").split(' ').next().unwrap().to_string())
        .collect();
    assert_eq!(
        kinds,
        [
            "DeepNesting",
            "AsyncVoidFunction",
            "DeprecatedFunction",
            "EmptyModuleDoc"
        ]
    );
    assert!(warnings[0].to_string().contains("depth 4"));
    assert!(warnings[2].to_string().contains("use new"));
}
