//! The validator's test suite.
//!
//! Every case is a short YAML document paired with the error variant it must
//! (or must not) produce, so adding a rule means adding one table row and a
//! few lines of IDL rather than forty lines of struct literals. Spans and
//! snippets are exercised by the CLI tests against on-disk files.

use super::*;
use weaveffi_ir::ir::CURRENT_SCHEMA_VERSION;
use weaveffi_ir::parse::parse_api_str;

/// Parse a YAML body (without the `version:` line) and validate it.
fn check(body: &str) -> Result<ResolvedApi, ValidationDiagnostics> {
    let doc = format!("version: \"{CURRENT_SCHEMA_VERSION}\"\n{body}");
    let api = parse_api_str(&doc, "yaml").expect("fixture must parse");
    validate_api(api, None)
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
        constructors: [{ name: open, params: [{ name: path, type: "&str" }] }]
        methods:
          - { name: get, params: [{ name: k, type: string }], return: "Point?", throws: true }
          - { name: scan, params: [], return: "iter<Point>" }
          - { name: sibling, params: [{ name: other, type: "Store?" }], return: Store }
        statics: [{ name: version, params: [], return: string }]
    callbacks:
      - name: on_point
        params: [{ name: p, type: Point }, { name: raw, type: "&[u8]" }]
    listeners:
      - { name: points, event_callback: on_point }
    functions:
      - { name: fetch, params: [{ name: id, type: i64 }], return: "[Point]", async: true, cancellable: true }
      - { name: token, params: [], return: "handle<Point>" }
      - { name: rename, params: [{ name: s, type: string, mutable: true }] }
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
            "DuplicateErrorName",
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
            "AbiSymbolCollision",
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
            "AbiSymbolCollision",
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
            "DuplicateStructName",
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
            "DuplicateEnumName",
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
            "DuplicateInterfaceName",
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
    structs: [{ name: S, fields: [{ name: i, type: I }] }]
"#,
        ),
        (
            "InterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    interfaces: [{ name: I, methods: [{ name: a, params: [] }] }]
    functions: [{ name: f, params: [{ name: xs, type: "[I]" }] }]
"#,
        ),
        (
            "InterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    interfaces: [{ name: I, methods: [{ name: a, params: [] }] }]
    functions: [{ name: f, params: [], return: "handle<I>" }]
"#,
        ),
        (
            "InterfaceInInvalidPosition",
            r#"
modules:
  - name: m
    interfaces: [{ name: I, methods: [{ name: a, params: [] }] }]
    callbacks: [{ name: cb, params: [{ name: i, type: I }] }]
"#,
        ),
        (
            "UnknownTypeRef",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: Nope }] }]",
        ),
        (
            "UnknownTypeRef",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: 'handle<Nope>' }] }]",
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
            "BorrowedTypeInInvalidPosition",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: '&str' }] }]",
        ),
        (
            "BorrowedTypeInInvalidPosition",
            "modules: [{ name: m, functions: [{ name: f, params: [{ name: x, type: '[&str]' }] }] }]",
        ),
        (
            "BorrowedTypeInInvalidPosition",
            "modules: [{ name: m, structs: [{ name: S, fields: [{ name: x, type: '&[u8]' }] }] }]",
        ),
        (
            "DuplicateCallbackName",
            r#"
modules:
  - name: m
    callbacks: [{ name: cb, params: [] }, { name: cb, params: [] }]
"#,
        ),
        (
            "ListenerCallbackNotFound",
            "modules: [{ name: m, listeners: [{ name: l, event_callback: nope }] }]",
        ),
        (
            "DuplicateListenerName",
            r#"
modules:
  - name: m
    callbacks: [{ name: cb, params: [] }]
    listeners: [{ name: l, event_callback: cb }, { name: l, event_callback: cb }]
"#,
        ),
        (
            "UnsupportedCallbackParamType",
            "modules: [{ name: m, callbacks: [{ name: cb, params: [{ name: x, type: 'iter<i32>' }] }] }]",
        ),
        (
            "UnsupportedCallbackParamType",
            "modules: [{ name: m, callbacks: [{ name: cb, params: [{ name: x, type: '[&str]' }] }] }]",
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
            "MutableParamUnsupported",
            "modules: [{ name: m, functions: [{ name: f, params: [{ name: x, type: i32, mutable: true }] }] }]",
        ),
        (
            "MutableParamUnsupported",
            "modules: [{ name: m, functions: [{ name: f, params: [{ name: x, type: '[i32]', mutable: true }] }] }]",
        ),
        (
            "AsyncIteratorReturn",
            "modules: [{ name: m, functions: [{ name: f, params: [], return: 'iter<i32>', async: true }] }]",
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
    let err = validate_api(api, None).unwrap_err();
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
fn source_spans_underline_the_offending_identifier() {
    let src = "version: \"0.8.0\"\nmodules:\n  - name: \"dup\"\n  - name: \"dup\"\n";
    let api = parse_api_str(src, "yaml").unwrap();
    let err = validate_api(api, Some(("api.yml", src))).unwrap_err();
    let d = err.first();
    let span = d.span.expect("span located");
    assert_eq!(&src[span.offset()..span.offset() + span.len()], "\"dup\"");
    assert!(d.labels().is_some());
    assert!(d.source_code().is_some());
}

#[test]
fn resolved_api_qualifies_cross_module_types() {
    let api = check(
        r#"
modules:
  - name: shared
    enums: [{ name: Status, variants: [{ name: Ok, value: 0 }] }]
    structs: [{ name: Point, fields: [{ name: x, type: f64 }] }]
    interfaces: [{ name: Store, methods: [{ name: get, params: [] }] }]
  - name: orders
    functions:
      - name: f
        params: [{ name: s, type: Store }, { name: st, type: Status }]
        return: "[Point]"
"#,
    )
    .unwrap();
    use crate::model::Ty;
    use weaveffi_ir::ir::TypeRef;
    let f = &api.modules[1].functions[0];
    assert_eq!(
        api.resolve(&f.params[0].ty, "orders"),
        Ty::Interface("shared.Store".into())
    );
    assert_eq!(
        api.resolve(&f.params[1].ty, "orders"),
        Ty::Enum("shared.Status".into())
    );
    assert_eq!(
        api.resolve(f.returns.as_ref().unwrap(), "orders"),
        Ty::List(Box::new(Ty::Record("shared.Point".into())))
    );
    assert_eq!(
        api.resolve(&TypeRef::Named("Point".into()), "shared"),
        Ty::Record("Point".into())
    );
}

#[test]
fn warnings_are_advisory() {
    let api = check(
        r#"
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
"#,
    )
    .unwrap();
    let warnings = collect_warnings(&api);
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
