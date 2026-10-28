//! Unit tests for what's specific to the Dart target: package naming and
//! layout, identifier escaping, how each value crosses, leaf calls, the
//! contract rows, and the packaged loader. The `kitchen_sink` snapshot pins
//! the rendered bindings themselves.

use weaveffi_model::ir::Api;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use crate::codegen::contract::{hex, rows};
use crate::codegen::OutputFile;
use crate::package::{FileContent, PackageContext};
use crate::platform::{BinarySet, NativeBinary, Platform};
use crate::targets::Target;

use super::{DartConfig, DartGenerator, MIN_DART_SDK};

/// An error domain and a callback interface, and names that collide with
/// Dart keywords, the runtime's types, and the wrapper's members.
const FIXTURE: &str = r#"
version: "0.13.0"
errors:
  - name: Native
    codes: [{ name: Closed, code: 1, message: "closed" }]
records:
  - name: Int32List
    fields: [{ name: class, type: i32 }]
interfaces:
  - name: Cart
    methods:
      - { name: dispose, return: bool }
callbacks:
  - name: Watcher
    methods:
      - { name: seen, params: [{ name: count, type: i32 }], return: bool }
functions:
  - { name: watch, params: [{ name: watcher, type: "Watcher?" }] }
  - name: wait
    params: [{ name: cancelToken, type: i64 }]
    return: i64
    async: true
    cancellable: true
    throws: Native
  - { name: ping, return: i32 }
"#;

/// Every family of value, in a library without callbacks.
const VALUES: &str = r#"
version: "0.13.0"
enums:
  - name: Kind
    variants: [{ name: Hot }, { name: Cold }]
interfaces:
  - name: Store
    methods:
      - { name: label, params: [{ name: key, type: string }], return: "string?" }
functions:
  - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
  - { name: echo, params: [{ name: s, type: string }], return: string }
  - { name: maybe, params: [{ name: k, type: "Kind?" }, { name: n, type: "u32?" }], return: "f64?" }
  - { name: sum, params: [{ name: xs, type: "[f64]" }], return: "[i64]" }
  - { name: find, params: [{ name: s, type: "Store?" }], return: Store }
  - { name: counts, return: "iter<u16?>" }
"#;

fn model_of(yaml: &str, name: &str) -> Model {
    let api: Api = serde_saphyr::from_str(yaml).unwrap();
    validate(&api, &Identity::named(name), None).unwrap()
}

fn model() -> Model {
    model_of(FIXTURE, "shop_kit")
}

fn render_with(config: DartConfig) -> Vec<OutputFile> {
    DartGenerator::from(config).render(&model())
}

fn file<'a>(files: &'a [OutputFile], path: &str) -> &'a str {
    &files
        .iter()
        .find(|f| f.path.as_str() == path)
        .unwrap_or_else(|| panic!("no {path}"))
        .contents
}

fn assert_has(out: &str, needle: &str) {
    assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
}

/// The packaged library of the fixture, bundling every platform, as
/// `(path, text)` pairs.
fn packaged() -> Vec<(String, String)> {
    let mut binaries = BinarySet::new("shop_kit");
    for p in Platform::ALL {
        binaries.insert(NativeBinary::new(p, format!("/prebuilt/{}/lib", p.id())));
    }
    let ctx = PackageContext::new(&binaries);
    let artifacts = DartGenerator::from(DartConfig::default())
        .package(&model(), &ctx)
        .expect("dart packages");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, "dart/shop_kit");
    let files = &artifacts[0].files;
    let bundled: Vec<&str> = files
        .iter()
        .filter(|f| f.is_binary())
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(bundled.len(), Platform::DESKTOP.len());
    for p in [
        Platform::AndroidArm64,
        Platform::AndroidX64,
        Platform::Wasm32,
    ] {
        assert!(
            !bundled.iter().any(|b| b.contains(p.id())),
            "{} bundled",
            p.id()
        );
    }
    files
        .iter()
        .filter_map(|f| match &f.content {
            FileContent::Text(s) => Some((f.path.to_string(), s.clone())),
            _ => None,
        })
        .collect()
}

#[test]
fn names_come_from_the_identity() {
    let files = render_with(DartConfig::default());
    assert_has(file(&files, "pubspec.yaml"), "name: shop_kit\n");
    assert_has(
        file(&files, "README.md"),
        "import 'package:shop_kit/shop_kit.dart';",
    );
    let loader = file(&files, "lib/src/runtime/loader.dart");
    assert_has(loader, "Platform.environment['SHOP_KIT_LIBRARY']");
    assert_has(loader, "? 'libshop_kit.dylib'");
    assert_has(loader, "? 'shop_kit.dll'");
    assert_has(loader, ": 'libshop_kit.so';");
    assert_has(loader, "'shop_kit_abi_version'");
    assert!(
        !loader.contains("_openBundled"),
        "unpackaged bindings look for bundles"
    );
    for f in &files {
        assert!(
            !f.contents.to_lowercase().contains("weaveffi_"),
            "branded symbol in {}",
            f.path
        );
    }

    let config = DartConfig {
        name: Some("custom".into()),
        ..DartConfig::default()
    };
    let files = render_with(config);
    assert_has(file(&files, "pubspec.yaml"), "name: custom\n");
    assert_has(file(&files, "lib/custom.dart"), "part 'src/api.dart';");
    assert_has(
        file(&files, "lib/src/api.dart"),
        "part of '../custom.dart';",
    );
}

#[test]
fn pubspec_states_the_minimum_sdk() {
    let files = render_with(DartConfig::default());
    assert_has(
        file(&files, "pubspec.yaml"),
        &format!("sdk: '>={MIN_DART_SDK} <4.0.0'"),
    );
    assert_has(
        file(&files, "README.md"),
        &format!("Dart SDK `>={MIN_DART_SDK} <4.0.0`"),
    );
}

#[test]
fn one_library_with_one_api_part() {
    let files = render_with(DartConfig::default());
    let main = file(&files, "lib/shop_kit.dart");
    let parts: Vec<&str> = main
        .lines()
        .filter_map(|l| l.strip_prefix("part '")?.strip_suffix("';"))
        .collect();
    assert_eq!(
        parts,
        [
            "src/runtime/loader.dart",
            "src/runtime/contracts.dart",
            "src/runtime/core.dart",
            "src/runtime/object.dart",
            "src/runtime/codec.dart",
            "src/runtime/async.dart",
            "src/runtime/cancel.dart",
            "src/runtime/callbacks.dart",
            "src/api.dart",
        ]
    );
    let mut rendered: Vec<&str> = files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| p.starts_with("lib/src/"))
        .collect();
    rendered.sort_unstable();
    let mut declared: Vec<String> = parts.iter().map(|p| format!("lib/{p}")).collect();
    declared.sort();
    assert_eq!(rendered, declared);
    for part in &parts {
        let up = "../".repeat(part.matches('/').count());
        assert_has(
            file(&files, &format!("lib/{part}")),
            &format!("\npart of '{up}shop_kit.dart';\n"),
        );
    }
    let api = file(&files, "lib/src/api.dart");
    assert_has(api, "int ping() {");
    assert_has(api, "('shop_kit_ping');");
}

#[test]
fn colliding_names_gain_an_underscore() {
    let files = render_with(DartConfig::default());
    let api = file(&files, "lib/src/api.dart");
    // A record named like a typed list, a field named like a keyword, a
    // domain whose exception is the runtime's, and a member named like the
    // wrapper's own.
    assert_has(api, "final class Int32List_ {");
    assert_has(api, "final int class_;");
    assert_has(api, "class NativeException_ extends NativeException {");
    assert_has(api, "bool dispose_() {");
    // A parameter named like the cancellation token keeps its name.
    assert_has(
        api,
        "Future<int> wait(int cancelToken, {CancelToken? cancelToken_}) {",
    );
    assert_has(api, "_NativeCancel.bind(cancelToken_)");
}

#[test]
fn every_value_is_one_c_value() {
    let files = DartGenerator::from(DartConfig::default()).render(&model_of(VALUES, "kv"));
    let api = file(&files, "lib/src/api.dart");
    // Runs and optional scalars cross by value, objects as pointers.
    assert_has(api, "_Run Function(_Run, Pointer<_Error>),");
    assert_has(api, "_OptF64 Function(_OptI32, _OptU32, Pointer<_Error>),");
    assert_has(api, "_optI32(k?.value), _optU32(n), _f.err");
    assert_has(api, "return _result.has ? _result.value : null;");
    assert_has(api, "final _sArg = _stageText(_f.arena, s);");
    assert_has(api, "return _takeText(_result);");
    assert_has(api, "final _xsArg = _stageF64s(_f.arena, xs);");
    assert_has(api, "return _takeI64s(_result);");
    assert_has(
        api,
        "final _sArg = s == null ? nullptr : _borrow(_f.arena, s);",
    );
    assert_has(api, "return Store._(_result);");
    // An iterator's element arrives in the frame's out slot.
    assert_has(
        api,
        "Int32 Function(Pointer<Void>, Pointer<_OptU16>, Pointer<_Error>),",
    );
    assert_has(api, "final _out = _step.out.cast<_OptU16>().ref;");
    let main = file(&files, "lib/kv.dart");
    for part in ["options", "arrays", "object", "codec", "iterator"] {
        assert_has(main, &format!("part 'src/runtime/{part}.dart';"));
    }
    // An optional string is a value buffer.
    assert_has(api, "return _takeDecoded(_result, _unpack_opt_string);");
}

#[test]
fn callback_arguments_are_registered_last() {
    let files = render_with(DartConfig::default());
    let api = file(&files, "lib/src/api.dart");
    assert_has(
        api,
        "final _watcherArg = watcher == null ? _noCallback() : _registerCallback(watcher, _watcherDispatch, _watcherVtable.cast());",
    );
    assert_has(api, "Void Function(_CallbackArg, Pointer<_Error>),");
    assert_has(api, "Bool Function(Pointer<Void>, Int32, Pointer<_Error>);");
    assert_has(api, "..flags = _vtableThreadAffine");
}

#[test]
fn every_declaration_has_a_contract_row() {
    let model = model();
    let files = DartGenerator::from(DartConfig::default()).render(&model);
    let contracts = file(&files, "lib/src/runtime/contracts.dart");
    let rows = rows(&model);
    assert!(rows.iter().any(|r| r.path == "Watcher.seen"));
    for row in &rows {
        assert_has(
            contracts,
            &format!("('{}', {}, '{}'),", row.symbol, hex(row.hash), row.path),
        );
        assert_has(contracts, &format!("// {}", row.signature));
    }
}

#[test]
fn calls_are_leaf_calls_without_callbacks() {
    let model = model_of(
        r#"
version: "0.13.0"
functions:
  - { name: add, params: [{ name: a, type: i32 }, { name: b, type: i32 }], return: i32 }
"#,
        "calc",
    );
    let files = DartGenerator::from(DartConfig::default()).render(&model);
    let api = file(&files, "lib/src/api.dart");
    assert_has(api, "Int32 Function(Int32, Int32, Pointer<_Error>),");
    assert_has(api, "('calc_add', isLeaf: true);");
    assert!(
        !file(&files, "lib/calc.dart").contains("dart:isolate"),
        "isolate glue without callbacks"
    );
    // With a callback interface, no call may be a leaf call.
    let api = file(&render_with(DartConfig::default()), "lib/src/api.dart").to_string();
    assert!(!api.contains("'shop_kit_watch', isLeaf"));
}

#[test]
fn packaged_loader_resolves_the_bundle_from_the_package() {
    let files = packaged();
    let text = |path: &str| {
        files
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, t)| t.as_str())
            .unwrap_or_else(|| panic!("no {path}"))
    };
    assert_has(
        text("lib/shop_kit.dart"),
        "import 'dart:io' show Directory, File, Platform;",
    );
    assert_has(
        text("lib/shop_kit.dart"),
        "part 'src/runtime/bundled.dart';",
    );
    let bundled = text("lib/src/runtime/bundled.dart");
    assert_has(
        bundled,
        "      Abi.macosArm64 => 'native/darwin-arm64/libshop_kit.dylib',",
    );
    assert_has(
        bundled,
        "      Abi.windowsX64 => 'native/windows-x64/shop_kit.dll',",
    );
    assert_has(
        bundled,
        "      Abi.linuxArm64 => 'native/linux-arm64/libshop_kit.so',",
    );
    assert_has(bundled, "Uri.parse('package:shop_kit/shop_kit.dart')");
    assert_has(
        bundled,
        "for (final root in [_packageRoot(), Directory.current.path]) {",
    );
    let loader = text("lib/src/runtime/loader.dart");
    assert_has(
        loader,
        "  final bundled = _openBundled();\n  if (bundled != null) return bundled;\n",
    );
    assert_has(loader, "Platform.environment['SHOP_KIT_LIBRARY']");
}

#[test]
fn every_line_is_free_of_trailing_whitespace() {
    let rendered = render_with(DartConfig::default())
        .into_iter()
        .map(|f| (f.path.to_string(), f.contents));
    for (path, out) in rendered.chain(packaged()) {
        for (i, line) in out.lines().enumerate() {
            assert_eq!(
                line.trim_end(),
                line,
                "trailing whitespace on {path}:{}",
                i + 1
            );
        }
    }
}
