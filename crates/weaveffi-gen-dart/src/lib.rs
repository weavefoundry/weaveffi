use anyhow::Result;
use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::{stamp_header, Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, Function, ListenerDef, Module, StructDef, TypeRef,
};

pub struct DartGenerator;

fn stamp_slash(body: String) -> String {
    format!("// {}\n{body}", stamp_header("dart"))
}

fn stamp_hash(body: String) -> String {
    format!("# {}\n{body}", stamp_header("dart"))
}

impl DartGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        package_name: &str,
        c_prefix: &str,
    ) -> Result<()> {
        let dart_dir = out_dir.join("dart");
        let lib_dir = dart_dir.join("lib");
        let src_dir = lib_dir.join("src");
        std::fs::create_dir_all(&src_dir)?;
        std::fs::write(
            src_dir.join("bindings.dart"),
            stamp_slash(render_dart_module(api, c_prefix)),
        )?;
        std::fs::write(lib_dir.join("weaveffi.dart"), stamp_slash(render_barrel()))?;
        std::fs::write(
            dart_dir.join("pubspec.yaml"),
            stamp_hash(render_pubspec(package_name)),
        )?;
        std::fs::write(
            dart_dir.join("analysis_options.yaml"),
            stamp_hash(render_analysis_options()),
        )?;
        // README.md is documentation, not a source file; leave it unstamped.
        std::fs::write(dart_dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for DartGenerator {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi", "weaveffi")
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(api, out_dir, config.dart_package_name(), config.c_prefix())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("dart/lib/weaveffi.dart").to_string(),
            out_dir.join("dart/lib/src/bindings.dart").to_string(),
            out_dir.join("dart/pubspec.yaml").to_string(),
            out_dir.join("dart/analysis_options.yaml").to_string(),
            out_dir.join("dart/README.md").to_string(),
        ]
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[
            Capability::Callbacks,
            Capability::Iterators,
            Capability::AsyncFunctions,
            Capability::CancellableAsync,
            Capability::TypedHandles,
            Capability::BorrowedTypes,
            Capability::MapTypes,
            Capability::NestedModules,
            Capability::CrossModuleTypes,
            Capability::ErrorDomains,
            Capability::DeprecatedAnnotations,
        ]
    }
}

fn dart_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::Handle => "int".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "List<int>".into(),
        TypeRef::TypedHandle(n) | TypeRef::Enum(n) => n.to_upper_camel_case(),
        TypeRef::Struct(n) => local_type_name(n).to_upper_camel_case(),
        TypeRef::Optional(inner) => format!("{}?", dart_type(inner)),
        TypeRef::List(inner) => format!("List<{}>", dart_type(inner)),
        TypeRef::Iterator(inner) => format!("Iterable<{}>", dart_type(inner)),
        TypeRef::Map(k, v) => format!("Map<{}, {}>", dart_type(k), dart_type(v)),
        TypeRef::Callback(n) => n.to_upper_camel_case(),
    }
}

fn native_ffi_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "Int32".into(),
        TypeRef::U32 => "Uint32".into(),
        TypeRef::I64 | TypeRef::Handle => "Int64".into(),
        TypeRef::F64 => "Double".into(),
        TypeRef::Bool | TypeRef::Enum(_) => "Int32".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Pointer<Uint8>".into(),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => "Pointer<Void>".into(),
        TypeRef::Optional(inner) => native_ffi_type(inner),
        TypeRef::Callback(n) => {
            format!(
                "Pointer<NativeFunction<_Native{}>>",
                n.to_upper_camel_case()
            )
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => "Pointer<Pointer<Void>>".into(),
            _ => "Pointer<Void>".into(),
        },
        TypeRef::Map(_, _) => "Pointer<Void>".into(),
    }
}

fn dart_ffi_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::Handle
        | TypeRef::Bool
        | TypeRef::Enum(_) => "int".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Pointer<Uint8>".into(),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => "Pointer<Void>".into(),
        TypeRef::Optional(inner) => dart_ffi_type(inner),
        TypeRef::Callback(n) => {
            format!(
                "Pointer<NativeFunction<_Native{}>>",
                n.to_upper_camel_case()
            )
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => "Pointer<Pointer<Void>>".into(),
            _ => "Pointer<Void>".into(),
        },
        TypeRef::Map(_, _) => "Pointer<Void>".into(),
    }
}

/// Native FFI type(s) for a parameter. `StringUtf8` and `Bytes`/`BorrowedBytes` expand to a
/// (ptr, len) pair to match the C ABI `(const uint8_t* X_ptr, size_t X_len)`, and
/// `Optional<StringUtf8>` / `Optional<Bytes>` / `Optional<BorrowedBytes>` do the same with
/// (nullptr, 0) encoding `None`. `Callback` expands to a `(function_ptr, context_ptr)` pair.
fn native_ffi_param_types(ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec!["Pointer<Uint8>".into(), "IntPtr".into()]
        }
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes
            ) =>
        {
            vec!["Pointer<Uint8>".into(), "IntPtr".into()]
        }
        TypeRef::Callback(_) => vec![native_ffi_type(ty), "Pointer<Void>".into()],
        _ => vec![native_ffi_type(ty)],
    }
}

/// Dart-side FFI type(s) for a parameter. `StringUtf8` and `Bytes`/`BorrowedBytes` expand to a
/// (ptr, len) pair, and `Optional<StringUtf8>` / `Optional<Bytes>` / `Optional<BorrowedBytes>`
/// do the same with (nullptr, 0) encoding `None`. `Callback` expands to a
/// `(function_ptr, context_ptr)` pair.
fn dart_ffi_param_types(ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec!["Pointer<Uint8>".into(), "int".into()]
        }
        TypeRef::Optional(inner)
            if matches!(
                inner.as_ref(),
                TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes
            ) =>
        {
            vec!["Pointer<Uint8>".into(), "int".into()]
        }
        TypeRef::Callback(_) => vec![dart_ffi_type(ty), "Pointer<Void>".into()],
        _ => vec![dart_ffi_type(ty)],
    }
}

fn emit_typedef_and_lookup(
    out: &mut String,
    c_sym: &str,
    native_params: &str,
    dart_params: &str,
    native_ret: &str,
    dart_ret: &str,
) {
    let td = c_sym.to_upper_camel_case();
    let var = c_sym.to_lower_camel_case();
    out.push_str(&format!(
        "\ntypedef _Native{td} = {native_ret} Function({native_params});\n"
    ));
    out.push_str(&format!(
        "typedef _Dart{td} = {dart_ret} Function({dart_params});\n"
    ));
    out.push_str(&format!(
        "final _{var} = _lib.lookupFunction<\n    _Native{td}, _Dart{td}>('{c_sym}');\n"
    ));
}

fn render_pubspec(package_name: &str) -> String {
    format!(
        "name: {package_name}\n\
         version: 0.1.0\n\
         environment:\n\
         \x20 sdk: '>=3.0.0 <4.0.0'\n\
         dependencies:\n\
         \x20 ffi: ^2.1.0\n\
         dev_dependencies:\n\
         \x20 test: ^1.24.0\n"
    )
}

fn render_analysis_options() -> String {
    "include: package:flutter_lints/flutter.yaml\n".into()
}

fn render_barrel() -> String {
    "export 'src/bindings.dart';\n".into()
}

fn render_readme() -> String {
    r#"# WeaveFFI Dart Bindings

Auto-generated Dart bindings using `dart:ffi`.

## Usage

1. Place the compiled shared library (`libweaveffi.dylib`, `libweaveffi.so`,
   or `weaveffi.dll`) where the Dart process can find it.

2. Add this package as a dependency and import the bindings:

```dart
import 'package:weaveffi/weaveffi.dart';
```

3. Call the generated functions directly. The bindings use `dart:ffi` to load
   the native library at runtime via `DynamicLibrary.open` and resolve symbols
   with `lookupFunction`.

## Requirements

- Dart SDK >= 3.0.0
- The `ffi` package (`^2.0.0`) for `Utf8` and `calloc` helpers.
"#
    .into()
}

fn collect_all_modules(modules: &[Module]) -> Vec<&Module> {
    let mut all = Vec::new();
    for m in modules {
        all.push(m);
        all.extend(collect_all_modules(&m.modules));
    }
    all
}

fn collect_modules_with_path(modules: &[Module]) -> Vec<(&Module, String)> {
    let mut result = Vec::new();
    for m in modules {
        collect_module_with_path(m, &m.name, &mut result);
    }
    result
}

fn collect_module_with_path<'a>(m: &'a Module, path: &str, out: &mut Vec<(&'a Module, String)>) {
    out.push((m, path.to_string()));
    for sub in &m.modules {
        collect_module_with_path(sub, &format!("{path}_{}", sub.name), out);
    }
}

fn render_dart_module(api: &Api, c_prefix: &str) -> String {
    let mut out = String::new();
    let has_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    let has_cancellable_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable));

    out.push_str("// Auto-generated by WeaveFFI. Do not edit.\n");
    out.push_str("// ignore_for_file: non_constant_identifier_names, camel_case_types\n\n");
    out.push_str("import 'dart:convert';\n");
    out.push_str("import 'dart:ffi';\n");
    out.push_str("import 'dart:io' show Platform;\n");
    if has_async {
        out.push_str("import 'dart:isolate';\n");
    }
    out.push_str("import 'package:ffi/ffi.dart';\n\n");

    out.push_str("DynamicLibrary _openLibrary() {\n");
    out.push_str(&format!(
        "  if (Platform.isMacOS) return DynamicLibrary.open('lib{c_prefix}.dylib');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isLinux) return DynamicLibrary.open('lib{c_prefix}.so');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isWindows) return DynamicLibrary.open('{c_prefix}.dll');\n"
    ));
    out.push_str(
        "  throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');\n",
    );
    out.push_str("}\n\n");
    out.push_str("final DynamicLibrary _lib = _openLibrary();\n\n");

    out.push_str("final class _WeaveffiError extends Struct {\n");
    out.push_str("  @Int32()\n");
    out.push_str("  external int code;\n");
    out.push_str("  external Pointer<Utf8> message;\n");
    out.push_str("}\n");

    let error_clear_sym = format!("{c_prefix}_error_clear");
    emit_typedef_and_lookup(
        &mut out,
        &error_clear_sym,
        "Pointer<_WeaveffiError>",
        "Pointer<_WeaveffiError>",
        "Void",
        "void",
    );

    emit_typedef_and_lookup(
        &mut out,
        &format!("{c_prefix}_free_bytes"),
        "Pointer<Uint8>, IntPtr",
        "Pointer<Uint8>, int",
        "Void",
        "void",
    );

    emit_typedef_and_lookup(
        &mut out,
        &format!("{c_prefix}_free_string"),
        "Pointer<Char>",
        "Pointer<Char>",
        "Void",
        "void",
    );

    out.push_str("\nclass WeaveffiException implements Exception {\n");
    out.push_str("  final int code;\n");
    out.push_str("  final String message;\n");
    out.push_str("  WeaveffiException(this.code, this.message);\n");
    out.push_str("  @override\n");
    out.push_str("  String toString() => 'WeaveffiException($code): $message';\n");
    out.push_str("}\n\n");

    let error_clear_var = error_clear_sym.to_lower_camel_case();
    out.push_str("void _checkError(Pointer<_WeaveffiError> err) {\n");
    out.push_str("  if (err.ref.code != 0) {\n");
    out.push_str("    final msg = err.ref.message.toDartString();\n");
    out.push_str(&format!("    _{error_clear_var}(err);\n"));
    out.push_str("    throw WeaveffiException(err.ref.code, msg);\n");
    out.push_str("  }\n");
    out.push_str("}\n");

    if has_cancellable_async {
        render_cancel_token(&mut out, c_prefix);
    }

    for (module, path) in collect_modules_with_path(&api.modules) {
        for e in &module.enums {
            render_enum(&mut out, e);
        }
        for cb in &module.callbacks {
            render_callback(&mut out, cb);
        }
        for s in &module.structs {
            render_struct(&mut out, &path, s, c_prefix);
            if s.builder {
                render_dart_builder(&mut out, &path, s, c_prefix);
            }
        }
        for l in &module.listeners {
            render_listener(&mut out, &path, l, c_prefix);
        }
        for f in &module.functions {
            render_function(&mut out, &path, f, c_prefix);
        }
    }

    out
}

/// Emit FFI bindings for the `{c_prefix}_cancel_token_*` C ABI and a lightweight
/// `CancelToken` Dart class whose `cancel()` forwards to
/// `{c_prefix}_cancel_token_cancel`, giving Dart callers a cancellation primitive
/// they can pass to cancellable async wrappers.
fn render_cancel_token(out: &mut String, c_prefix: &str) {
    let create_sym = format!("{c_prefix}_cancel_token_create");
    let cancel_sym = format!("{c_prefix}_cancel_token_cancel");
    let destroy_sym = format!("{c_prefix}_cancel_token_destroy");

    emit_typedef_and_lookup(out, &create_sym, "", "", "Pointer<Void>", "Pointer<Void>");
    emit_typedef_and_lookup(
        out,
        &cancel_sym,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );
    emit_typedef_and_lookup(
        out,
        &destroy_sym,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );

    let create_var = create_sym.to_lower_camel_case();
    let cancel_var = cancel_sym.to_lower_camel_case();
    let destroy_var = destroy_sym.to_lower_camel_case();

    out.push_str("\nclass CancelToken {\n");
    out.push_str("  Pointer<Void> _handle;\n");
    out.push_str("  bool _disposed = false;\n");
    out.push_str(&format!("  CancelToken() : _handle = _{create_var}();\n"));
    out.push_str("  Pointer<Void> get handle => _handle;\n");
    out.push_str("  void cancel() {\n");
    out.push_str("    if (_disposed) return;\n");
    out.push_str(&format!("    _{cancel_var}(_handle);\n"));
    out.push_str("  }\n");
    out.push_str("  void dispose() {\n");
    out.push_str("    if (_disposed) return;\n");
    out.push_str("    _disposed = true;\n");
    out.push_str(&format!("    _{destroy_var}(_handle);\n"));
    out.push_str("    _handle = nullptr;\n");
    out.push_str("  }\n");
    out.push_str("}\n");
}

fn render_enum(out: &mut String, e: &EnumDef) {
    let name = e.name.to_upper_camel_case();
    if let Some(doc) = &e.doc {
        out.push_str(&format!("\n/// {doc}\n"));
    } else {
        out.push('\n');
    }
    out.push_str(&format!("enum {name} {{\n"));
    for v in &e.variants {
        let vname = v.name.to_lower_camel_case();
        if let Some(doc) = &v.doc {
            out.push_str(&format!("  /// {doc}\n"));
        }
        out.push_str(&format!("  {vname}({}),\n", v.value));
    }
    out.push_str("  ;\n");
    out.push_str(&format!("  const {name}(this.value);\n"));
    out.push_str("  final int value;\n\n");
    out.push_str(&format!(
        "  static {name} fromValue(int value) =>\n      {name}.values.firstWhere((e) => e.value == value);\n"
    ));
    out.push_str("}\n");
}

fn render_callback(out: &mut String, cb: &CallbackDef) {
    let name = cb.name.to_upper_camel_case();

    let dart_ret = cb.returns.as_ref().map_or("void".into(), dart_type);
    let user_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), p.name.to_lower_camel_case()))
        .collect();

    if let Some(doc) = &cb.doc {
        out.push_str(&format!("\n/// {doc}\n"));
    } else {
        out.push('\n');
    }
    out.push_str(&format!(
        "typedef {name} = {dart_ret} Function({});\n",
        user_params.join(", ")
    ));

    let native_ret = cb.returns.as_ref().map_or("Void".into(), native_ffi_type);
    let dart_ffi_ret = cb.returns.as_ref().map_or("void".into(), dart_ffi_type);

    let mut native_params: Vec<String> = vec!["Pointer<Void>".into()];
    for p in &cb.params {
        native_params.extend(native_ffi_param_types(&p.ty));
    }
    let mut dart_ffi_params: Vec<String> = vec!["Pointer<Void>".into()];
    for p in &cb.params {
        dart_ffi_params.extend(dart_ffi_param_types(&p.ty));
    }

    out.push_str(&format!(
        "typedef _Native{name} = {native_ret} Function({});\n",
        native_params.join(", ")
    ));
    out.push_str(&format!(
        "typedef _Dart{name} = {dart_ffi_ret} Function({});\n",
        dart_ffi_params.join(", ")
    ));
}

/// Emit a Dart wrapper class for a listener. The class exposes:
///   - `register(callback)`: wraps the callback via `Pointer.fromFunction`,
///     calls the C `register_{name}` symbol, stores the function pointer in
///     a class-level `Map<int, ...>` keyed by the returned id to pin it
///     against GC, and returns the id.
///   - `unregister(id)`: calls the C `unregister_{name}` symbol and removes
///     the pinned pointer from the map.
fn render_listener(out: &mut String, module_path: &str, l: &ListenerDef, c_prefix: &str) {
    let class_name = l.name.to_upper_camel_case();
    let cb_td = l.event_callback.to_upper_camel_case();
    let cb_ptr_ty = format!("Pointer<NativeFunction<_Native{cb_td}>>");
    let reg_sym = format!("{c_prefix}_{module_path}_register_{}", l.name);
    let unreg_sym = format!("{c_prefix}_{module_path}_unregister_{}", l.name);

    emit_typedef_and_lookup(
        out,
        &reg_sym,
        &format!("{cb_ptr_ty}, Pointer<Void>"),
        &format!("{cb_ptr_ty}, Pointer<Void>"),
        "Uint64",
        "int",
    );
    emit_typedef_and_lookup(out, &unreg_sym, "Uint64", "int", "Void", "void");

    let reg_var = reg_sym.to_lower_camel_case();
    let unreg_var = unreg_sym.to_lower_camel_case();

    if let Some(doc) = &l.doc {
        out.push_str(&format!("\n/// {doc}\n"));
    } else {
        out.push('\n');
    }
    out.push_str(&format!("class {class_name} {{\n"));
    out.push_str(&format!(
        "  static final Map<int, {cb_ptr_ty}> _callbacks = {{}};\n\n"
    ));
    out.push_str(&format!("  static int register({cb_td} callback) {{\n"));
    out.push_str(&format!(
        "    final ptr = Pointer.fromFunction<_Native{cb_td}>(callback);\n"
    ));
    out.push_str(&format!("    final id = _{reg_var}(ptr, nullptr);\n"));
    out.push_str("    _callbacks[id] = ptr;\n");
    out.push_str("    return id;\n");
    out.push_str("  }\n\n");

    out.push_str("  static void unregister(int id) {\n");
    out.push_str(&format!("    _{unreg_var}(id);\n"));
    out.push_str("    _callbacks.remove(id);\n");
    out.push_str("  }\n");
    out.push_str("}\n");
}

fn render_struct(out: &mut String, module_path: &str, s: &StructDef, c_prefix: &str) {
    let class_name = s.name.to_upper_camel_case();
    let struct_sym_prefix = format!("{c_prefix}_{}_{}", module_path, s.name);

    let destroy_sym = format!("{struct_sym_prefix}_destroy");
    emit_typedef_and_lookup(
        out,
        &destroy_sym,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );

    for field in &s.fields {
        let getter_sym = format!("{struct_sym_prefix}_get_{}", field.name);
        let nr = native_ffi_type(&field.ty);
        let dr = dart_ffi_type(&field.ty);
        emit_typedef_and_lookup(
            out,
            &getter_sym,
            "Pointer<Void>, Pointer<_WeaveffiError>",
            "Pointer<Void>, Pointer<_WeaveffiError>",
            &nr,
            &dr,
        );
    }

    if let Some(doc) = &s.doc {
        out.push_str(&format!("\n/// {doc}\n"));
    } else {
        out.push('\n');
    }
    let destroy_camel = destroy_sym.to_lower_camel_case();
    out.push_str(&format!("class {class_name} {{\n"));
    out.push_str("  Pointer<Void> _handle;\n");
    out.push_str("  bool _disposed = false;\n");
    out.push_str(&format!(
        "  static final Finalizer<Pointer<Void>> _finalizer =\n      Finalizer<Pointer<Void>>((ptr) {{\n        if (ptr != nullptr) _{destroy_camel}(ptr);\n      }});\n\n"
    ));
    out.push_str(&format!(
        "  {class_name}._(this._handle) {{\n    _finalizer.attach(this, _handle, detach: this);\n  }}\n\n"
    ));

    out.push_str("  void dispose() {\n");
    out.push_str("    if (_disposed) return;\n");
    out.push_str("    _disposed = true;\n");
    out.push_str("    _finalizer.detach(this);\n");
    out.push_str("    if (_handle != nullptr) {\n");
    out.push_str(&format!("      _{destroy_camel}(_handle);\n"));
    out.push_str("      _handle = nullptr;\n");
    out.push_str("    }\n");
    out.push_str("  }\n");

    for field in &s.fields {
        let getter_sym = format!("{struct_sym_prefix}_get_{}", field.name);
        let dart_ret = dart_type(&field.ty);
        let fname = field.name.to_lower_camel_case();

        out.push_str(&format!("\n  {dart_ret} get {fname} {{\n"));
        out.push_str("    final err = calloc<_WeaveffiError>();\n");
        out.push_str("    try {\n");
        out.push_str(&format!(
            "      final result = _{}(_handle, err);\n",
            getter_sym.to_lower_camel_case()
        ));
        out.push_str("      _checkError(err);\n");
        emit_result_conversion(out, &field.ty, "      ", c_prefix);
        out.push_str("    } finally {\n");
        out.push_str("      calloc.free(err);\n");
        out.push_str("    }\n");
        out.push_str("  }\n");
    }

    out.push_str("}\n");
}

fn render_dart_builder(out: &mut String, module_path: &str, s: &StructDef, c_prefix: &str) {
    let class_name = s.name.to_upper_camel_case();
    let builder_name = format!("{class_name}Builder");
    let struct_sym_prefix = format!("{c_prefix}_{}_{}", module_path, s.name);

    let new_sym = format!("{struct_sym_prefix}_Builder_new");
    emit_typedef_and_lookup(out, &new_sym, "", "", "Pointer<Void>", "Pointer<Void>");

    for field in &s.fields {
        let set_sym = format!("{struct_sym_prefix}_Builder_set_{}", field.name);
        let mut native_params: Vec<String> = vec!["Pointer<Void>".into()];
        native_params.extend(native_ffi_param_types(&field.ty));
        let mut dart_params: Vec<String> = vec!["Pointer<Void>".into()];
        dart_params.extend(dart_ffi_param_types(&field.ty));
        emit_typedef_and_lookup(
            out,
            &set_sym,
            &native_params.join(", "),
            &dart_params.join(", "),
            "Void",
            "void",
        );
    }

    let build_sym = format!("{struct_sym_prefix}_Builder_build");
    emit_typedef_and_lookup(
        out,
        &build_sym,
        "Pointer<Void>, Pointer<_WeaveffiError>",
        "Pointer<Void>, Pointer<_WeaveffiError>",
        "Pointer<Void>",
        "Pointer<Void>",
    );

    let destroy_sym = format!("{struct_sym_prefix}_Builder_destroy");
    emit_typedef_and_lookup(
        out,
        &destroy_sym,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );

    let new_var = new_sym.to_lower_camel_case();
    let build_var = build_sym.to_lower_camel_case();
    let destroy_var = destroy_sym.to_lower_camel_case();

    out.push_str(&format!("\nclass {builder_name} {{\n"));
    out.push_str(&format!("  Pointer<Void> _handle = _{new_var}();\n"));

    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let dt = dart_type(&field.ty);
        let set_sym = format!("{struct_sym_prefix}_Builder_set_{}", field.name);
        let set_var = set_sym.to_lower_camel_case();

        out.push_str(&format!("\n  {builder_name} with{pascal}({dt} value) {{\n"));
        emit_builder_setter_body(out, &field.ty, &set_var);
        out.push_str("    return this;\n");
        out.push_str("  }\n");
    }

    out.push_str(&format!("\n  {class_name} build() {{\n"));
    out.push_str("    final err = calloc<_WeaveffiError>();\n");
    out.push_str("    try {\n");
    out.push_str(&format!(
        "      final result = _{build_var}(_handle, err);\n"
    ));
    out.push_str("      _checkError(err);\n");
    out.push_str(&format!("      _{destroy_var}(_handle);\n"));
    out.push_str("      _handle = nullptr;\n");
    out.push_str(&format!("      return {class_name}._(result);\n"));
    out.push_str("    } finally {\n");
    out.push_str("      calloc.free(err);\n");
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str("}\n");
}

fn emit_builder_setter_body(out: &mut String, ty: &TypeRef, set_var: &str) {
    match ty {
        TypeRef::StringUtf8 => {
            out.push_str("    final valueBytes = utf8.encode(value);\n");
            out.push_str("    final valueBuf = calloc<Uint8>(valueBytes.length);\n");
            out.push_str("    valueBuf.asTypedList(valueBytes.length).setAll(0, valueBytes);\n");
            out.push_str("    try {\n");
            out.push_str(&format!(
                "      _{set_var}(_handle, valueBuf, valueBytes.length);\n"
            ));
            out.push_str("    } finally {\n");
            out.push_str("      calloc.free(valueBuf);\n");
            out.push_str("    }\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("    final valueBuf = calloc<Uint8>(value.length);\n");
            out.push_str("    valueBuf.asTypedList(value.length).setAll(0, value);\n");
            out.push_str("    try {\n");
            out.push_str(&format!(
                "      _{set_var}(_handle, valueBuf, value.length);\n"
            ));
            out.push_str("    } finally {\n");
            out.push_str("      calloc.free(valueBuf);\n");
            out.push_str("    }\n");
        }
        TypeRef::BorrowedStr => {
            out.push_str("    final valuePtr = value.toNativeUtf8();\n");
            out.push_str("    try {\n");
            out.push_str(&format!("      _{set_var}(_handle, valuePtr);\n"));
            out.push_str("    } finally {\n");
            out.push_str("      calloc.free(valuePtr);\n");
            out.push_str("    }\n");
        }
        TypeRef::Bool => {
            out.push_str(&format!("    _{set_var}(_handle, value ? 1 : 0);\n"));
        }
        TypeRef::Enum(_) => {
            out.push_str(&format!("    _{set_var}(_handle, value.value);\n"));
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            out.push_str(&format!("    _{set_var}(_handle, value._handle);\n"));
        }
        _ => {
            out.push_str(&format!("    _{set_var}(_handle, value);\n"));
        }
    }
}

fn render_function(out: &mut String, module_path: &str, f: &Function, c_prefix: &str) {
    let c_sym = format!("{c_prefix}_{module_path}_{}", f.name);

    let mut native_params: Vec<String> = f
        .params
        .iter()
        .flat_map(|p| native_ffi_param_types(&p.ty))
        .collect();
    let mut dart_params: Vec<String> = f
        .params
        .iter()
        .flat_map(|p| dart_ffi_param_types(&p.ty))
        .collect();
    if returns_out_len(f) {
        native_params.push("Pointer<IntPtr>".into());
        dart_params.push("Pointer<IntPtr>".into());
    }
    native_params.push("Pointer<_WeaveffiError>".into());
    dart_params.push("Pointer<_WeaveffiError>".into());

    let native_ret = f.returns.as_ref().map_or("Void".into(), native_ffi_type);
    let dart_ret = f.returns.as_ref().map_or("void".into(), dart_ffi_type);

    emit_typedef_and_lookup(
        out,
        &c_sym,
        &native_params.join(", "),
        &dart_params.join(", "),
        &native_ret,
        &dart_ret,
    );

    let wrapper_name = f.name.to_lower_camel_case();
    let pub_ret = f.returns.as_ref().map_or("void".into(), dart_type);
    let wrapper_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), p.name.to_lower_camel_case()))
        .collect();

    if f.r#async {
        let sync_name = format!("_{wrapper_name}");
        out.push('\n');
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('\'', "\\'");
            out.push_str(&format!("@Deprecated('{escaped}')\n"));
        }
        out.push_str(&format!(
            "{pub_ret} {sync_name}({}) {{\n",
            wrapper_params.join(", ")
        ));
        emit_function_body(out, f, &c_sym, c_prefix);
        out.push_str("}\n");

        let call_args: Vec<String> = f
            .params
            .iter()
            .map(|p| p.name.to_lower_camel_case())
            .collect();
        let mut wrapper_params_async = wrapper_params.clone();
        if f.cancellable {
            wrapper_params_async.push("{CancelToken? cancelToken}".to_string());
        }
        out.push('\n');
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('\'', "\\'");
            out.push_str(&format!("@Deprecated('{escaped}')\n"));
        }
        out.push_str(&format!(
            "Future<{pub_ret}> {wrapper_name}({}) async {{\n",
            wrapper_params_async.join(", ")
        ));
        if f.cancellable {
            // When the caller does not pre-allocate a `CancelToken`, create a
            // one-shot token internally. The token is wired so that
            // `CancelToken.cancel()` forwards to `weaveffi_cancel_token_cancel`.
            out.push_str("  final _ownsToken = cancelToken == null;\n");
            out.push_str("  final _token = cancelToken ?? CancelToken();\n");
            out.push_str("  try {\n");
            out.push_str(&format!(
                "    return await Isolate.run(() => {sync_name}({}));\n",
                call_args.join(", ")
            ));
            out.push_str("  } finally {\n");
            out.push_str("    if (_ownsToken) _token.dispose();\n");
            out.push_str("  }\n");
        } else {
            out.push_str(&format!(
                "  return await Isolate.run(() => {sync_name}({}));\n",
                call_args.join(", ")
            ));
        }
        out.push_str("}\n");
    } else {
        out.push('\n');
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('\'', "\\'");
            out.push_str(&format!("@Deprecated('{escaped}')\n"));
        }
        out.push_str(&format!(
            "{pub_ret} {wrapper_name}({}) {{\n",
            wrapper_params.join(", ")
        ));
        emit_function_body(out, f, &c_sym, c_prefix);
        out.push_str("}\n");
    }
}

/// Return `true` when the C ABI signature ends with an extra `size_t* out_len`
/// parameter right before `weaveffi_error* out_err`. That happens for byte-buffer
/// returns (`Bytes`/`BorrowedBytes`) and for list-of-handle returns
/// (`[Struct]`/`[TypedHandle]`), both of which materialise into Dart as
/// `(Pointer<IntPtr>, Pointer<_WeaveffiError>)` trailing params.
fn returns_out_len(f: &Function) -> bool {
    match f.returns.as_ref() {
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => true,
        Some(TypeRef::List(inner) | TypeRef::Iterator(inner)) => {
            matches!(inner.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_))
        }
        _ => false,
    }
}

/// Emit allocation for an `Optional<StringUtf8|Bytes|BorrowedBytes>` parameter as a
/// `(Pointer<Uint8>, int)` pair, using `(nullptr, 0)` to encode `None` so the callee sees
/// the same `const uint8_t* X_ptr, size_t X_len` shape as the non-optional variant.
fn emit_optional_bytes_alloc(out: &mut String, pname: &str, inner: &TypeRef) {
    let buf = format!("{pname}Buf");
    let len = format!("{pname}Len");
    out.push_str(&format!("  final Pointer<Uint8> {buf};\n"));
    out.push_str(&format!("  final int {len};\n"));
    out.push_str(&format!("  if ({pname} == null) {{\n"));
    out.push_str(&format!("    {buf} = nullptr;\n"));
    out.push_str(&format!("    {len} = 0;\n"));
    out.push_str("  } else {\n");
    match inner {
        TypeRef::StringUtf8 => {
            let bytes = format!("{pname}Bytes");
            out.push_str(&format!("    final {bytes} = utf8.encode({pname});\n"));
            out.push_str(&format!("    {buf} = calloc<Uint8>({bytes}.length);\n"));
            out.push_str(&format!(
                "    {buf}.asTypedList({bytes}.length).setAll(0, {bytes});\n"
            ));
            out.push_str(&format!("    {len} = {bytes}.length;\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("    {buf} = calloc<Uint8>({pname}.length);\n"));
            out.push_str(&format!(
                "    {buf}.asTypedList({pname}.length).setAll(0, {pname});\n"
            ));
            out.push_str(&format!("    {len} = {pname}.length;\n"));
        }
        _ => unreachable!("emit_optional_bytes_alloc called with unsupported inner"),
    }
    out.push_str("  }\n");
}

fn emit_function_body(out: &mut String, f: &Function, c_sym: &str, c_prefix: &str) {
    out.push_str("  final err = calloc<_WeaveffiError>();\n");

    let mut allocations: Vec<String> = Vec::new();
    let mut optional_allocations: Vec<String> = Vec::new();
    for p in &f.params {
        let pname = p.name.to_lower_camel_case();
        match &p.ty {
            TypeRef::StringUtf8 => {
                let bytes = format!("{pname}Bytes");
                let buf = format!("{pname}Buf");
                out.push_str(&format!("  final {bytes} = utf8.encode({pname});\n"));
                out.push_str(&format!("  final {buf} = calloc<Uint8>({bytes}.length);\n"));
                out.push_str(&format!(
                    "  {buf}.asTypedList({bytes}.length).setAll(0, {bytes});\n"
                ));
                allocations.push(buf);
            }
            TypeRef::BorrowedStr => {
                let ptr = format!("{pname}Ptr");
                out.push_str(&format!("  final {ptr} = {pname}.toNativeUtf8();\n"));
                allocations.push(ptr);
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let buf = format!("{pname}Buf");
                out.push_str(&format!("  final {buf} = calloc<Uint8>({pname}.length);\n"));
                out.push_str(&format!(
                    "  {buf}.asTypedList({pname}.length).setAll(0, {pname});\n"
                ));
                allocations.push(buf);
            }
            TypeRef::Optional(inner)
                if matches!(
                    inner.as_ref(),
                    TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes
                ) =>
            {
                emit_optional_bytes_alloc(out, &pname, inner);
                optional_allocations.push(format!("{pname}Buf"));
            }
            TypeRef::Callback(name) => {
                let td = name.to_upper_camel_case();
                out.push_str(&format!(
                    "  final {pname}Ptr = Pointer.fromFunction<_Native{td}>({pname});\n"
                ));
            }
            _ => {}
        }
    }

    let needs_out_len = returns_out_len(f);
    if needs_out_len {
        out.push_str("  final outLen = calloc<IntPtr>();\n");
    }

    out.push_str("  try {\n");

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        let pname = p.name.to_lower_camel_case();
        match &p.ty {
            TypeRef::StringUtf8 => {
                call_args.push(format!("{pname}Buf"));
                call_args.push(format!("{pname}Bytes.length"));
            }
            TypeRef::BorrowedStr => call_args.push(format!("{pname}Ptr")),
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                call_args.push(format!("{pname}Buf"));
                call_args.push(format!("{pname}.length"));
            }
            TypeRef::Optional(inner)
                if matches!(
                    inner.as_ref(),
                    TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes
                ) =>
            {
                call_args.push(format!("{pname}Buf"));
                call_args.push(format!("{pname}Len"));
            }
            TypeRef::Bool => call_args.push(format!("{pname} ? 1 : 0")),
            TypeRef::Enum(_) => call_args.push(format!("{pname}.value")),
            TypeRef::TypedHandle(_) | TypeRef::Struct(_) => {
                call_args.push(format!("{pname}._handle"))
            }
            TypeRef::Callback(_) => {
                call_args.push(format!("{pname}Ptr"));
                call_args.push("nullptr".into());
            }
            _ => call_args.push(pname),
        }
    }
    if needs_out_len {
        call_args.push("outLen".into());
    }
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    if let Some(ret) = &f.returns {
        out.push_str(&format!(
            "    final result = _{var}({});\n",
            call_args.join(", ")
        ));
        out.push_str("    _checkError(err);\n");
        emit_result_conversion(out, ret, "    ", c_prefix);
    } else {
        out.push_str(&format!("    _{var}({});\n", call_args.join(", ")));
        out.push_str("    _checkError(err);\n");
    }

    out.push_str("  } finally {\n");
    for alloc in &allocations {
        out.push_str(&format!("    calloc.free({alloc});\n"));
    }
    for alloc in &optional_allocations {
        out.push_str(&format!(
            "    if ({alloc} != nullptr) calloc.free({alloc});\n"
        ));
    }
    if needs_out_len {
        out.push_str("    calloc.free(outLen);\n");
    }
    out.push_str("    calloc.free(err);\n");
    out.push_str("  }\n");
}

fn emit_result_conversion(out: &mut String, ty: &TypeRef, indent: &str, c_prefix: &str) {
    let free_string_var = format!("{c_prefix}_free_string").to_lower_camel_case();
    let free_bytes_var = format!("{c_prefix}_free_bytes").to_lower_camel_case();
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}final str = result.cast<Char>() == nullptr ? '' : result.toDartString();\n"
            ));
            out.push_str(&format!(
                "{indent}_{free_string_var}(result.cast<Char>());\n"
            ));
            out.push_str(&format!("{indent}return str;\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{indent}return result != 0;\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{indent}final len = outLen.value;\n"));
            out.push_str(&format!(
                "{indent}final bytes = List<int>.from(result.asTypedList(len));\n"
            ));
            out.push_str(&format!("{indent}_{free_bytes_var}(result, len);\n"));
            out.push_str(&format!("{indent}return bytes;\n"));
        }
        TypeRef::Enum(name) => {
            let n = name.to_upper_camel_case();
            out.push_str(&format!("{indent}return {n}.fromValue(result);\n"));
        }
        TypeRef::Struct(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!("{indent}return {n}._(result);\n"));
        }
        TypeRef::TypedHandle(name) => {
            let n = name.to_upper_camel_case();
            out.push_str(&format!("{indent}return {n}._(result);\n"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}final str = result.toDartString();\n"));
                out.push_str(&format!(
                    "{indent}_{free_string_var}(result.cast<Char>());\n"
                ));
                out.push_str(&format!("{indent}return str;\n"));
            }
            TypeRef::Struct(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}return {n}._(result);\n"));
            }
            TypeRef::TypedHandle(name) => {
                let n = name.to_upper_camel_case();
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}return {n}._(result);\n"));
            }
            TypeRef::Bool => {
                out.push_str(&format!("{indent}return result != 0;\n"));
            }
            _ => {
                out.push_str(&format!("{indent}return result;\n"));
            }
        },
        TypeRef::List(inner) | TypeRef::Iterator(inner) => match inner.as_ref() {
            TypeRef::Struct(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                emit_list_of_handles_conversion(out, indent, &n);
            }
            TypeRef::TypedHandle(name) => {
                let n = name.to_upper_camel_case();
                emit_list_of_handles_conversion(out, indent, &n);
            }
            _ => {
                out.push_str(&format!("{indent}return result;\n"));
            }
        },
        _ => {
            out.push_str(&format!("{indent}return result;\n"));
        }
    }
}

/// Emit the Dart loop that materialises a C `T**`/`size_t* out_len` pair returned from
/// `weaveffi_{module}_{fn}` into a `List<{dart_class}>`, wrapping each handle in
/// `{dart_class}._(ptr)` so the returned Dart objects participate in the same
/// `dispose()`/`Finalizer` lifecycle as direct `{dart_class}` returns.
fn emit_list_of_handles_conversion(out: &mut String, indent: &str, dart_class: &str) {
    out.push_str(&format!("{indent}final len = outLen.value;\n"));
    out.push_str(&format!("{indent}final list = <{dart_class}>[];\n"));
    out.push_str(&format!("{indent}for (var i = 0; i < len; i++) {{\n"));
    out.push_str(&format!("{indent}  list.add({dart_class}._(result[i]));\n"));
    out.push_str(&format!("{indent}}}\n"));
    out.push_str(&format!("{indent}return list;\n"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::config::GeneratorConfig;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".into(),
            modules,
            generators: None,
        }
    }

    fn simple_module(functions: Vec<Function>) -> Module {
        Module {
            name: "math".into(),
            functions,
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    #[test]
    fn generator_name_is_dart() {
        assert_eq!(DartGenerator.name(), "dart");
    }

    #[test]
    fn output_files_lists_dart_file() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = DartGenerator.output_files(&api, out);
        assert_eq!(
            files,
            vec![
                out.join("dart/lib/weaveffi.dart").to_string(),
                out.join("dart/lib/src/bindings.dart").to_string(),
                out.join("dart/pubspec.yaml").to_string(),
                out.join("dart/analysis_options.yaml").to_string(),
                out.join("dart/README.md").to_string(),
            ]
        );
    }

    #[test]
    fn dart_output_files_with_config_respects_naming() {
        // `dart_package_name` is only written into pubspec.yaml, so it must not
        // change the emitted file paths.
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");

        let expected = vec![
            out.join("dart/lib/weaveffi.dart").to_string(),
            out.join("dart/lib/src/bindings.dart").to_string(),
            out.join("dart/pubspec.yaml").to_string(),
            out.join("dart/analysis_options.yaml").to_string(),
            out.join("dart/README.md").to_string(),
        ];

        let default_files =
            DartGenerator.output_files_with_config(&api, out, &GeneratorConfig::default());
        assert_eq!(default_files, expected);

        let config = GeneratorConfig {
            dart_package_name: Some("my_dart_pkg".into()),
            ..GeneratorConfig::default()
        };
        let custom_files = DartGenerator.output_files_with_config(&api, out, &config);
        assert_eq!(
            custom_files, expected,
            "dart_package_name must not affect output paths"
        );
    }

    #[test]
    fn dart_type_mapping() {
        assert_eq!(dart_type(&TypeRef::I32), "int");
        assert_eq!(dart_type(&TypeRef::U32), "int");
        assert_eq!(dart_type(&TypeRef::I64), "int");
        assert_eq!(dart_type(&TypeRef::F64), "double");
        assert_eq!(dart_type(&TypeRef::Bool), "bool");
        assert_eq!(dart_type(&TypeRef::StringUtf8), "String");
        assert_eq!(dart_type(&TypeRef::Handle), "int");
        assert_eq!(dart_type(&TypeRef::Struct("Foo".into())), "Foo");
        assert_eq!(dart_type(&TypeRef::Enum("Bar".into())), "Bar");
        assert_eq!(
            dart_type(&TypeRef::TypedHandle("Session".into())),
            "Session"
        );
        assert_eq!(
            dart_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "int?"
        );
        assert_eq!(
            dart_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "List<int>"
        );
        assert_eq!(
            dart_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "Map<String, int>"
        );
    }

    #[test]
    fn native_ffi_type_mapping() {
        assert_eq!(native_ffi_type(&TypeRef::I32), "Int32");
        assert_eq!(native_ffi_type(&TypeRef::U32), "Uint32");
        assert_eq!(native_ffi_type(&TypeRef::I64), "Int64");
        assert_eq!(native_ffi_type(&TypeRef::F64), "Double");
        assert_eq!(native_ffi_type(&TypeRef::Bool), "Int32");
        assert_eq!(native_ffi_type(&TypeRef::StringUtf8), "Pointer<Utf8>");
        assert_eq!(native_ffi_type(&TypeRef::Handle), "Int64");
        assert_eq!(
            native_ffi_type(&TypeRef::Struct("X".into())),
            "Pointer<Void>"
        );
        assert_eq!(native_ffi_type(&TypeRef::Enum("X".into())), "Int32");
        assert_eq!(
            native_ffi_type(&TypeRef::TypedHandle("S".into())),
            "Pointer<Void>"
        );
        assert_eq!(
            native_ffi_type(&TypeRef::List(Box::new(TypeRef::Struct("X".into())))),
            "Pointer<Pointer<Void>>",
            "List<Struct> returns are a C `T**` array"
        );
        assert_eq!(
            native_ffi_type(&TypeRef::List(Box::new(TypeRef::TypedHandle("S".into())))),
            "Pointer<Pointer<Void>>",
            "List<TypedHandle> returns are a C `T**` array"
        );
    }

    #[test]
    fn optional_string_param_expands_to_ptr_len_pair() {
        assert_eq!(
            native_ffi_param_types(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            vec!["Pointer<Uint8>".to_string(), "IntPtr".to_string()],
            "Optional<StringUtf8> must marshal to `(const uint8_t*, size_t)` with (nullptr, 0) = None"
        );
        assert_eq!(
            dart_ffi_param_types(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            vec!["Pointer<Uint8>".to_string(), "int".to_string()],
        );
    }

    #[test]
    fn optional_bytes_param_expands_to_ptr_len_pair() {
        assert_eq!(
            native_ffi_param_types(&TypeRef::Optional(Box::new(TypeRef::Bytes))),
            vec!["Pointer<Uint8>".to_string(), "IntPtr".to_string()],
        );
        assert_eq!(
            native_ffi_param_types(&TypeRef::Optional(Box::new(TypeRef::BorrowedBytes))),
            vec!["Pointer<Uint8>".to_string(), "IntPtr".to_string()],
        );
    }

    #[test]
    fn generate_dart_basic() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dart_basic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator.generate(&api, out_dir).unwrap();

        let dart = std::fs::read_to_string(tmp.join("dart/lib/src/bindings.dart")).unwrap();

        assert!(
            dart.contains("import 'dart:ffi'"),
            "missing dart:ffi import: {dart}"
        );
        assert!(
            dart.contains("import 'package:ffi/ffi.dart'"),
            "missing ffi package import: {dart}"
        );
        assert!(
            dart.contains("import 'dart:io' show Platform"),
            "missing Platform import: {dart}"
        );
        assert!(
            dart.contains("DynamicLibrary _openLibrary()"),
            "missing _openLibrary: {dart}"
        );
        assert!(
            dart.contains("libweaveffi.dylib"),
            "missing macOS lib: {dart}"
        );
        assert!(dart.contains("libweaveffi.so"), "missing Linux lib: {dart}");
        assert!(dart.contains("weaveffi.dll"), "missing Windows lib: {dart}");
        assert!(
            dart.contains("final DynamicLibrary _lib"),
            "missing _lib: {dart}"
        );
        assert!(
            dart.contains("_WeaveffiError extends Struct"),
            "missing error struct: {dart}"
        );
        assert!(
            dart.contains("class WeaveffiException"),
            "missing exception class: {dart}"
        );
        assert!(dart.contains("_checkError"), "missing error check: {dart}");
        assert!(
            dart.contains("weaveffi_error_clear"),
            "missing error_clear: {dart}"
        );
        assert!(
            dart.contains("typedef _NativeWeaveffiMathAdd"),
            "missing native typedef: {dart}"
        );
        assert!(
            dart.contains("typedef _DartWeaveffiMathAdd"),
            "missing dart typedef: {dart}"
        );
        assert!(
            dart.contains("lookupFunction"),
            "missing lookupFunction: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_math_add'"),
            "missing C symbol: {dart}"
        );
        assert!(
            dart.contains("Int32 Function(Int32, Int32"),
            "missing native sig: {dart}"
        );
        assert!(
            dart.contains("int Function(int, int"),
            "missing dart sig: {dart}"
        );
        assert!(
            dart.contains("int add(int a, int b)"),
            "missing wrapper: {dart}"
        );
        assert!(
            dart.contains("calloc<_WeaveffiError>()"),
            "missing calloc: {dart}"
        );
        assert!(
            dart.contains("_checkError(err)"),
            "missing error check in wrapper: {dart}"
        );
        assert!(dart.contains("return result"), "missing return: {dart}");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dart_with_structs() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
                builder: false,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(dart.contains("class Contact {"), "missing class: {dart}");
        assert!(
            dart.contains("Pointer<Void> _handle"),
            "missing _handle: {dart}"
        );
        assert!(
            dart.contains("Contact._(this._handle)"),
            "missing constructor: {dart}"
        );
        assert!(dart.contains("void dispose()"), "missing dispose: {dart}");
        assert!(
            dart.contains("weaveffi_contacts_Contact_destroy"),
            "missing destroy sym: {dart}"
        );
        assert!(dart.contains("int get id"), "missing id getter: {dart}");
        assert!(
            dart.contains("weaveffi_contacts_Contact_get_id"),
            "missing id getter sym: {dart}"
        );
        assert!(
            dart.contains("String get firstName"),
            "missing firstName getter: {dart}"
        );
        assert!(
            dart.contains("weaveffi_contacts_Contact_get_first_name"),
            "missing firstName getter sym: {dart}"
        );
        assert!(
            dart.contains("result.toDartString()"),
            "missing toDartString: {dart}"
        );
        assert!(
            dart.contains("String? get email"),
            "missing email getter: {dart}"
        );
        assert!(
            dart.contains("weaveffi_contacts_Contact_get_email"),
            "missing email getter sym: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_builder_struct() {
        let api = make_api(vec![Module {
            name: "geo".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: None,
                builder: true,
                fields: vec![StructField {
                    name: "x".into(),
                    ty: TypeRef::F64,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("class PointBuilder {"),
            "builder class: {dart}"
        );
        assert!(
            dart.contains("PointBuilder withX(double value)"),
            "fluent setter: {dart}"
        );
        assert!(
            !dart.contains("UnimplementedError"),
            "builder must not throw UnimplementedError: {dart}"
        );
    }

    #[test]
    fn dart_builder_build_calls_native() {
        let api = make_api(vec![Module {
            name: "geo".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: None,
                builder: true,
                fields: vec![
                    StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "y".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains("'weaveffi_geo_Point_Builder_new'"),
            "must look up Builder_new C symbol: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_geo_Point_Builder_set_x'"),
            "must look up Builder_set_x C symbol: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_geo_Point_Builder_set_y'"),
            "must look up Builder_set_y C symbol: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_geo_Point_Builder_build'"),
            "must look up Builder_build C symbol: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_geo_Point_Builder_destroy'"),
            "must look up Builder_destroy C symbol: {dart}"
        );

        assert!(
            dart.contains("typedef _NativeWeaveffiGeoPointBuilderNew = Pointer<Void> Function();"),
            "Builder_new native typedef must return Pointer<Void> with no params: {dart}"
        );
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiGeoPointBuilderBuild = Pointer<Void> Function(Pointer<Void>, Pointer<_WeaveffiError>);"
            ),
            "Builder_build native typedef must take (Pointer<Void>, Pointer<_WeaveffiError>) and return Pointer<Void>: {dart}"
        );
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiGeoPointBuilderDestroy = Void Function(Pointer<Void>);"
            ),
            "Builder_destroy native typedef must take Pointer<Void> and return Void: {dart}"
        );
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiGeoPointBuilderSetX = Void Function(Pointer<Void>, Double);"
            ),
            "Builder_set_x native typedef must take (Pointer<Void>, Double): {dart}"
        );

        assert!(
            dart.contains("Pointer<Void> _handle = _weaveffiGeoPointBuilderNew();"),
            "builder class must init _handle by calling native Builder_new: {dart}"
        );
        assert!(
            dart.contains("_weaveffiGeoPointBuilderSetX(_handle, value);"),
            "withX must call native Builder_set_x with (_handle, value): {dart}"
        );
        assert!(
            dart.contains("_weaveffiGeoPointBuilderSetY(_handle, value);"),
            "withY must call native Builder_set_y with (_handle, value): {dart}"
        );

        let build_start = dart
            .find("Point build() {")
            .expect("build() method must be present");
        let build_body = &dart[build_start..];
        let native_build_pos = build_body
            .find("_weaveffiGeoPointBuilderBuild(_handle, err)")
            .expect("build() must call native Builder_build with (_handle, err)");
        let check_pos = build_body
            .find("_checkError(err);")
            .expect("build() must check err after native Builder_build");
        let native_destroy_pos = build_body
            .find("_weaveffiGeoPointBuilderDestroy(_handle);")
            .expect("build() must call native Builder_destroy with _handle");
        let null_pos = build_body
            .find("_handle = nullptr;")
            .expect("build() must reset _handle to nullptr");
        let return_pos = build_body
            .find("return Point._(result);")
            .expect("build() must return a wrapped Point._(result)");
        assert!(
            native_build_pos < check_pos
                && check_pos < native_destroy_pos
                && native_destroy_pos < null_pos
                && null_pos < return_pos,
            "build() order must be: native build, check err, native destroy, null handle, return wrapped struct: {build_body}"
        );
    }

    #[test]
    fn generate_dart_with_enums() {
        let api = make_api(vec![Module {
            name: "paint".into(),
            functions: vec![Function {
                name: "mix".into(),
                params: vec![Param {
                    name: "color".into(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: Some("Primary colors".into()),
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(dart.contains("enum Color {"), "missing enum: {dart}");
        assert!(dart.contains("red(0)"), "missing red: {dart}");
        assert!(dart.contains("green(1)"), "missing green: {dart}");
        assert!(dart.contains("blue(2)"), "missing blue: {dart}");
        assert!(
            dart.contains("const Color(this.value)"),
            "missing const constructor: {dart}"
        );
        assert!(
            dart.contains("final int value"),
            "missing value field: {dart}"
        );
        assert!(
            dart.contains("static Color fromValue(int value)"),
            "missing fromValue: {dart}"
        );
        assert!(
            dart.contains("Color mix(Color color)"),
            "missing mix signature: {dart}"
        );
        assert!(
            dart.contains("color.value"),
            "missing .value conversion: {dart}"
        );
        assert!(
            dart.contains("Color.fromValue(result)"),
            "missing fromValue conversion: {dart}"
        );
    }

    #[test]
    fn void_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "reset".into(),
            params: vec![],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("void reset()"),
            "missing void function: {dart}"
        );
        assert!(
            dart.contains("Void Function("),
            "missing Void native return: {dart}"
        );
    }

    #[test]
    fn string_function() {
        let api = make_api(vec![Module {
            name: "text".into(),
            functions: vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("String echo(String msg)"),
            "missing signature: {dart}"
        );
        assert!(
            dart.contains("utf8.encode(msg)"),
            "missing utf8.encode for StringUtf8 param: {dart}"
        );
        assert!(
            dart.contains("calloc<Uint8>(msgBytes.length)"),
            "missing Uint8 buffer alloc: {dart}"
        );
        assert!(
            dart.contains("result.toDartString()"),
            "missing toDartString: {dart}"
        );
        assert!(
            dart.contains("calloc.free(msgBuf)"),
            "missing free for string buffer: {dart}"
        );
    }

    #[test]
    fn bool_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "is_valid".into(),
            params: vec![Param {
                name: "flag".into(),
                ty: TypeRef::Bool,
                mutable: false,
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("bool isValid(bool flag)"),
            "missing signature: {dart}"
        );
        assert!(dart.contains("flag ? 1 : 0"), "missing bool-to-int: {dart}");
        assert!(dart.contains("result != 0"), "missing int-to-bool: {dart}");
    }

    #[test]
    fn async_function() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "fetch_data".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("import 'dart:isolate'"),
            "missing isolate import: {dart}"
        );
        assert!(
            dart.contains("String _fetchData(int id)"),
            "missing sync helper: {dart}"
        );
        assert!(
            dart.contains("Future<String> fetchData(int id) async"),
            "missing async wrapper: {dart}"
        );
        assert!(dart.contains("Isolate.run"), "missing Isolate.run: {dart}");
    }

    #[test]
    fn dart_cancellable_async_wires_cancel_token_to_native() {
        let api = make_api(vec![simple_module(vec![
            Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: true,
                deprecated: None,
                since: None,
            },
            Function {
                name: "fire".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            },
        ])]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains("'weaveffi_cancel_token_create'"),
            "must emit cancel_token_create FFI lookup: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_cancel_token_cancel'"),
            "must emit cancel_token_cancel FFI lookup: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_cancel_token_destroy'"),
            "must emit cancel_token_destroy FFI lookup: {dart}"
        );
        assert!(
            dart.contains("class CancelToken {"),
            "must emit CancelToken class: {dart}"
        );
        assert!(
            dart.contains("_weaveffiCancelTokenCancel(_handle)"),
            "CancelToken.cancel() must forward to weaveffi_cancel_token_cancel: {dart}"
        );
        assert!(
            dart.contains("_weaveffiCancelTokenDestroy(_handle)"),
            "CancelToken.dispose() must forward to weaveffi_cancel_token_destroy: {dart}"
        );
        assert!(
            dart.contains("Future<int> run(int id, {CancelToken? cancelToken}) async"),
            "cancellable async must accept optional CancelToken param: {dart}"
        );
        assert!(
            dart.contains("final _token = cancelToken ?? CancelToken();"),
            "cancellable async must create a token when none is supplied: {dart}"
        );

        let fire_line = dart
            .lines()
            .find(|l| l.contains("Future<void> fire("))
            .expect("non-cancellable fire wrapper should still be emitted");
        assert!(
            !fire_line.contains("CancelToken"),
            "non-cancellable async must not accept a CancelToken: {fire_line}"
        );
    }

    #[test]
    fn struct_return_wraps_handle() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("Contact getContact(int id)"),
            "missing signature: {dart}"
        );
        assert!(
            dart.contains("Contact._(result)"),
            "missing struct wrapping: {dart}"
        );
    }

    #[test]
    fn handle_uses_int64() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "create".into(),
            params: vec![],
            returns: Some(TypeRef::Handle),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("Int64 Function("),
            "missing Int64 for Handle: {dart}"
        );
    }

    #[test]
    fn dart_generates_pubspec() {
        let api = make_api(vec![simple_module(vec![])]);
        let tmp = std::env::temp_dir().join("weaveffi_test_dart_pubspec");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator.generate(&api, out_dir).unwrap();

        let pubspec_path = tmp.join("dart/pubspec.yaml");
        assert!(pubspec_path.exists(), "pubspec.yaml should exist");
        let pubspec = std::fs::read_to_string(&pubspec_path).unwrap();
        assert!(
            pubspec.contains("name: weaveffi"),
            "missing name: {pubspec}"
        );
        assert!(
            pubspec.contains("version: 0.1.0"),
            "missing version: {pubspec}"
        );
        assert!(
            pubspec.contains("sdk: '>=3.0.0 <4.0.0'"),
            "missing sdk constraint: {pubspec}"
        );
        assert!(
            pubspec.contains("ffi: ^2.1.0"),
            "missing ffi dependency: {pubspec}"
        );

        let readme_path = tmp.join("dart/README.md");
        assert!(readme_path.exists(), "README.md should exist");
        let readme = std::fs::read_to_string(&readme_path).unwrap();
        assert!(
            readme.contains("dart:ffi"),
            "README should mention dart:ffi: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dart_with_optionals() {
        let api = make_api(vec![Module {
            name: "users".into(),
            functions: vec![Function {
                name: "find_user".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    mutable: false,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("String? findUser(int id)"),
            "missing optional return type: {dart}"
        );
        assert!(
            dart.contains("if (result == nullptr) return null;"),
            "missing null check: {dart}"
        );
        assert!(
            dart.contains("result.toDartString()"),
            "missing toDartString for optional: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_lists() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![Function {
                name: "get_scores".into(),
                params: vec![Param {
                    name: "items".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    mutable: false,
                }],
                returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("List<String> getScores(List<int> items)"),
            "missing list signature: {dart}"
        );
        assert!(
            dart.contains("Pointer<Void>"),
            "missing Pointer<Void> for list FFI type: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_maps() {
        let api = make_api(vec![Module {
            name: "cache".into(),
            functions: vec![Function {
                name: "get_entries".into(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("Map<String, int> getEntries()"),
            "missing map return type: {dart}"
        );
    }

    #[test]
    fn generate_dart_with_typed_handle() {
        let api = make_api(vec![Module {
            name: "sessions".into(),
            functions: vec![
                Function {
                    name: "create_session".into(),
                    params: vec![Param {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::TypedHandle("Session".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "close_session".into(),
                    params: vec![Param {
                        name: "session".into(),
                        ty: TypeRef::TypedHandle("Session".into()),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains("Session createSession(String name)"),
            "missing typed handle return: {dart}"
        );
        assert!(
            dart.contains("Session._(result)"),
            "missing typed handle wrapping: {dart}"
        );
        assert!(
            dart.contains("void closeSession(Session session)"),
            "missing typed handle param: {dart}"
        );
        assert!(
            dart.contains("session._handle"),
            "missing _handle access for typed handle param: {dart}"
        );
    }

    #[test]
    fn generate_dart_full_contacts() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![
                Function {
                    name: "create_contact".into(),
                    params: vec![
                        Param {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                        Param {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::Handle),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Struct("Contact".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "delete_contact".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::Handle,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Bool),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "count_contacts".into(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
                builder: false,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![EnumDef {
                name: "ContactType".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Personal".into(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Work".into(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Other".into(),
                        value: 2,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains("enum ContactType {"),
            "missing ContactType enum: {dart}"
        );
        assert!(
            dart.contains("personal(0)"),
            "missing personal variant: {dart}"
        );
        assert!(dart.contains("work(1)"), "missing work variant: {dart}");
        assert!(dart.contains("other(2)"), "missing other variant: {dart}");

        assert!(
            dart.contains("class Contact {"),
            "missing Contact class: {dart}"
        );
        assert!(
            dart.contains("/// A contact record"),
            "missing doc comment: {dart}"
        );
        assert!(dart.contains("int get id"), "missing id getter: {dart}");
        assert!(
            dart.contains("String get firstName"),
            "missing firstName getter: {dart}"
        );
        assert!(
            dart.contains("String get lastName"),
            "missing lastName getter: {dart}"
        );
        assert!(
            dart.contains("String? get email"),
            "missing optional email getter: {dart}"
        );
        assert!(
            dart.contains("ContactType get contactType"),
            "missing contactType getter: {dart}"
        );

        assert!(
            dart.contains("int createContact("),
            "missing createContact: {dart}"
        );
        assert!(
            dart.contains("Contact getContact(int id)"),
            "missing getContact: {dart}"
        );
        assert!(
            dart.contains("List<Contact> listContacts()"),
            "missing listContacts: {dart}"
        );
        assert!(
            dart.contains("bool deleteContact(int id)"),
            "missing deleteContact: {dart}"
        );
        assert!(
            dart.contains("int countContacts()"),
            "missing countContacts: {dart}"
        );

        // createContact accepts a nullable Dart `String?` but must marshal it to the
        // C ABI's (const uint8_t*, size_t) pair with (nullptr, 0) representing `None`.
        assert!(
            dart.contains("String? email"),
            "createContact must accept String? email: {dart}"
        );
        assert!(
            dart.contains("if (email == null) {\n    emailBuf = nullptr;"),
            "Optional<String> param must marshal null as nullptr: {dart}"
        );
        assert!(
            dart.contains("emailBuf, emailLen, contactType.value, err"),
            "createContact must pass (emailBuf, emailLen) pair: {dart}"
        );
        assert!(
            dart.contains("if (emailBuf != nullptr) calloc.free(emailBuf);"),
            "Optional<String> buf must only be freed when allocated: {dart}"
        );

        // listContacts must return a List<Contact> by iterating the T** / out_len
        // pair returned by weaveffi_contacts_list_contacts.
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiContactsListContacts = Pointer<Pointer<Void>> Function(Pointer<IntPtr>, Pointer<_WeaveffiError>);"
            ),
            "listContacts native typedef must take (out_len, err) and return T**: {dart}"
        );
        assert!(
            dart.contains("final outLen = calloc<IntPtr>();"),
            "listContacts must allocate out_len: {dart}"
        );
        assert!(
            dart.contains("list.add(Contact._(result[i]));"),
            "listContacts must wrap each handle in Contact._: {dart}"
        );
    }

    #[test]
    fn dart_custom_package_name() {
        let api = make_api(vec![simple_module(vec![])]);
        let tmp = std::env::temp_dir().join("weaveffi_test_dart_custom_pkg");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        let config = GeneratorConfig {
            dart_package_name: Some("my_custom_dart".into()),
            ..Default::default()
        };
        DartGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let pubspec = std::fs::read_to_string(tmp.join("dart/pubspec.yaml")).unwrap();
        assert!(
            pubspec.contains("name: my_custom_dart"),
            "pubspec should use custom package name: {pubspec}"
        );
        assert!(
            !pubspec.contains("name: weaveffi"),
            "pubspec should not use default name: {pubspec}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dart_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            !dart.contains("weaveffi_free_string(namePtr"),
            "borrowed string param must not be freed via weaveffi_free_string: {dart}"
        );

        let fn_start = dart
            .find("Contact findContact(")
            .expect("findContact wrapper");
        let fn_body = &dart[fn_start..];

        let err_check = fn_body
            .find("_checkError(err)")
            .expect("_checkError in findContact");
        let contact_wrap = fn_body
            .find("Contact._(result)")
            .expect("Contact._ in findContact");
        assert!(
            err_check < contact_wrap,
            "error must be checked before wrapping struct return: {dart}"
        );

        assert!(
            dart.contains("void dispose()") && dart.contains("_destroy"),
            "struct return type should have dispose calling destroy: {dart}"
        );
    }

    #[test]
    fn dart_string_param_uses_uint8_pointer_and_length() {
        let api = make_api(vec![Module {
            name: "text".into(),
            functions: vec![Function {
                name: "log".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains("import 'dart:convert';"),
            "should import dart:convert for utf8 encoding: {dart}"
        );

        assert!(
            dart.contains(
                "typedef _NativeWeaveffiTextLog = Void Function(Pointer<Uint8>, IntPtr, Pointer<_WeaveffiError>);"
            ),
            "native typedef should use Pointer<Uint8>, IntPtr for StringUtf8 param: {dart}"
        );
        assert!(
            dart.contains(
                "typedef _DartWeaveffiTextLog = void Function(Pointer<Uint8>, int, Pointer<_WeaveffiError>);"
            ),
            "dart typedef should use Pointer<Uint8>, int for StringUtf8 param: {dart}"
        );

        assert!(
            !dart.contains("Pointer<Utf8>, Pointer<_WeaveffiError>"),
            "typedef must not use Pointer<Utf8> for StringUtf8 param: {dart}"
        );

        assert!(
            dart.contains("final msgBytes = utf8.encode(msg);"),
            "wrapper should encode String to UTF-8 bytes: {dart}"
        );
        assert!(
            dart.contains("final msgBuf = calloc<Uint8>(msgBytes.length);"),
            "wrapper should allocate Uint8 buffer via calloc: {dart}"
        );
        assert!(
            dart.contains("msgBuf.asTypedList(msgBytes.length).setAll(0, msgBytes);"),
            "wrapper should copy bytes into native buffer: {dart}"
        );

        assert!(
            dart.contains("_weaveffiTextLog(msgBuf, msgBytes.length, err)"),
            "wrapper should call native fn with (buf, length, err): {dart}"
        );

        let log_start = dart.find("void log(String msg)").expect("log wrapper");
        let log_body = &dart[log_start..];
        let finally_pos = log_body.find("} finally {").expect("finally block");
        let finally_body = &log_body[finally_pos..];
        assert!(
            finally_body.contains("calloc.free(msgBuf);"),
            "buffer must be freed in finally block: {finally_body}"
        );
    }

    #[test]
    fn dart_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        let fn_start = dart
            .find("Contact? findContact(")
            .expect("findContact wrapper");
        let fn_body = &dart[fn_start..];

        let null_check = fn_body
            .find("if (result == nullptr) return null")
            .expect("null check in findContact");
        let contact_wrap = fn_body
            .find("Contact._(result)")
            .expect("Contact._ in findContact");
        assert!(
            null_check < contact_wrap,
            "optional struct return should check null before wrapping: {dart}"
        );
    }

    #[test]
    fn dart_bytes_param_uses_canonical_shape() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![Function {
                name: "send".into(),
                params: vec![Param {
                    name: "payload".into(),
                    ty: TypeRef::Bytes,
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiIoSend = Void Function(Pointer<Uint8>, IntPtr, Pointer<_WeaveffiError>);"
            ),
            "native typedef must expand Bytes param to (Pointer<Uint8>, IntPtr) matching (const uint8_t* X_ptr, size_t X_len): {dart}"
        );
        assert!(
            dart.contains(
                "typedef _DartWeaveffiIoSend = void Function(Pointer<Uint8>, int, Pointer<_WeaveffiError>);"
            ),
            "Dart typedef must expand Bytes param to (Pointer<Uint8>, int): {dart}"
        );
        assert!(
            dart.contains("final payloadBuf = calloc<Uint8>(payload.length);"),
            "wrapper must allocate a Uint8 buffer sized to the payload: {dart}"
        );
        assert!(
            dart.contains("payloadBuf.asTypedList(payload.length).setAll(0, payload);"),
            "wrapper must copy payload bytes into the native buffer: {dart}"
        );
        assert!(
            dart.contains("_weaveffiIoSend(payloadBuf, payload.length, err);"),
            "wrapper must call native with (ptr, len, err) for Bytes param: {dart}"
        );
        assert!(
            dart.contains("calloc.free(payloadBuf);"),
            "wrapper must free the allocated buffer in the finally block: {dart}"
        );
    }

    #[test]
    fn dart_bytes_return_uses_canonical_shape() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![Function {
                name: "read".into(),
                params: vec![],
                returns: Some(TypeRef::Bytes),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi");
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiIoRead = Pointer<Uint8> Function(Pointer<IntPtr>, Pointer<_WeaveffiError>);"
            ),
            "native typedef for Bytes return must be Pointer<Uint8> with (Pointer<IntPtr> out_len, Pointer<_WeaveffiError> err): {dart}"
        );
        assert!(
            dart.contains(
                "typedef _DartWeaveffiIoRead = Pointer<Uint8> Function(Pointer<IntPtr>, Pointer<_WeaveffiError>);"
            ),
            "Dart typedef for Bytes return must keep Pointer<Uint8> + (Pointer<IntPtr>, Pointer<_WeaveffiError>): {dart}"
        );
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiFreeBytes = Void Function(Pointer<Uint8>, IntPtr);"
            ),
            "weaveffi_free_bytes typedef must take (Pointer<Uint8>, IntPtr) (no const): {dart}"
        );
        assert!(
            dart.contains("final outLen = calloc<IntPtr>();"),
            "wrapper must allocate the out_len IntPtr cell: {dart}"
        );
        assert!(
            dart.contains("final result = _weaveffiIoRead(outLen, err);"),
            "wrapper must call native with (outLen, err) for Bytes return: {dart}"
        );
        assert!(
            dart.contains("_weaveffiFreeBytes(result, len);"),
            "wrapper must free the returned bytes via weaveffi_free_bytes(ptr, len): {dart}"
        );
        assert!(
            dart.contains("List<int>.from(result.asTypedList(len))"),
            "wrapper must copy returned bytes into a List<int> before returning: {dart}"
        );
        assert!(
            dart.contains("calloc.free(outLen);"),
            "wrapper must free outLen in the finally block: {dart}"
        );
    }

    #[test]
    fn dart_check_error_calls_weaveffi_error_clear() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi");
        let def_pos = dart
            .find("void _checkError(Pointer<_WeaveffiError> err) {")
            .expect("_checkError must be defined");
        let msg_pos = dart[def_pos..]
            .find("final msg = err.ref.message.toDartString();")
            .map(|p| p + def_pos)
            .expect("_checkError must capture err.ref.message into a Dart String");
        let clear_pos = dart[def_pos..]
            .find("_weaveffiErrorClear(err);")
            .map(|p| p + def_pos)
            .expect("_checkError must call _weaveffiErrorClear after capturing the message");
        let throw_pos = dart[def_pos..]
            .find("throw WeaveffiException(")
            .map(|p| p + def_pos)
            .expect("_checkError must throw after clearing");
        assert!(
            msg_pos < clear_pos,
            "_weaveffiErrorClear must run AFTER capturing err.ref.message: {dart}"
        );
        assert!(
            clear_pos < throw_pos,
            "_weaveffiErrorClear must run BEFORE throwing: {dart}"
        );
    }

    #[test]
    fn dart_string_return_calls_free_string() {
        let api = make_api(vec![Module {
            name: "text".into(),
            functions: vec![
                Function {
                    name: "echo".into(),
                    params: vec![Param {
                        name: "msg".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "fetch".into(),
                    params: vec![],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: true,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "lookup".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![StructDef {
                name: "Note".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "title".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains("typedef _NativeWeaveffiFreeString = Void Function(Pointer<Char>);"),
            "missing _NativeWeaveffiFreeString typedef using Pointer<Char>: {dart}"
        );
        assert!(
            dart.contains("typedef _DartWeaveffiFreeString = void Function(Pointer<Char>);"),
            "missing _DartWeaveffiFreeString typedef using Pointer<Char>: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_free_string'"),
            "missing 'weaveffi_free_string' lookup: {dart}"
        );

        let echo_start = dart.find("String echo(String msg)").expect("echo wrapper");
        let echo_body = &dart[echo_start..];
        let echo_end = echo_body.find("\n}\n").expect("end of echo body") + echo_start;
        let echo_text = &dart[echo_start..echo_end];
        let convert_pos = echo_text
            .find("result.cast<Char>() == nullptr ? '' : result.toDartString()")
            .expect("echo must capture string with null guard before freeing");
        let free_pos = echo_text
            .find("_weaveffiFreeString(result.cast<Char>())")
            .expect("echo must call _weaveffiFreeString on the returned pointer");
        let return_pos = echo_text
            .find("return str;")
            .expect("echo must return the captured str");
        assert!(
            convert_pos < free_pos && free_pos < return_pos,
            "free must occur after capture and before return: {echo_text}"
        );
        assert!(
            !echo_text.contains("return result.toDartString();"),
            "echo must not return result.toDartString() directly (leak): {echo_text}"
        );

        let lookup_start = dart.find("String? lookup(int id)").expect("lookup wrapper");
        let lookup_body = &dart[lookup_start..];
        let lookup_end = lookup_body.find("\n}\n").expect("end of lookup body") + lookup_start;
        let lookup_text = &dart[lookup_start..lookup_end];
        let null_pos = lookup_text
            .find("if (result == nullptr) return null;")
            .expect("optional lookup must short-circuit on null");
        let opt_convert_pos = lookup_text
            .find("final str = result.toDartString();")
            .expect("optional lookup must capture the string");
        let opt_free_pos = lookup_text
            .find("_weaveffiFreeString(result.cast<Char>())")
            .expect("optional lookup must free the returned pointer");
        let opt_return_pos = lookup_text
            .find("return str;")
            .expect("optional lookup must return the captured str");
        assert!(
            null_pos < opt_convert_pos
                && opt_convert_pos < opt_free_pos
                && opt_free_pos < opt_return_pos,
            "optional string return must null-check, capture, free, then return: {lookup_text}"
        );

        let getter_start = dart.find("String get title").expect("title getter");
        let getter_body = &dart[getter_start..];
        let getter_end = getter_body.find("\n  }\n").expect("end of getter") + getter_start;
        let getter_text = &dart[getter_start..getter_end];
        assert!(
            getter_text.contains("_weaveffiFreeString(result.cast<Char>())"),
            "struct string getter must free the returned pointer: {getter_text}"
        );
        assert!(
            getter_text.contains("return str;"),
            "struct string getter must return captured str: {getter_text}"
        );

        let sync_async_start = dart
            .find("String _fetch()")
            .expect("sync helper for async fetch");
        let sync_async_body = &dart[sync_async_start..];
        let sync_async_end =
            sync_async_body.find("\n}\n").expect("end of _fetch body") + sync_async_start;
        let sync_async_text = &dart[sync_async_start..sync_async_end];
        assert!(
            sync_async_text.contains("_weaveffiFreeString(result.cast<Char>())"),
            "async result delivery must free the returned pointer in sync helper: {sync_async_text}"
        );
        assert!(
            sync_async_text.contains("return str;"),
            "async sync helper must return captured str: {sync_async_text}"
        );
    }

    #[test]
    fn dart_bytes_return_calls_free_bytes() {
        let api = make_api(vec![Module {
            name: "parity".into(),
            functions: vec![Function {
                name: "echo".into(),
                params: vec![Param {
                    name: "b".into(),
                    ty: TypeRef::Bytes,
                    mutable: false,
                }],
                returns: Some(TypeRef::Bytes),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi");

        let copy_pos = dart
            .find("List<int>.from(result.asTypedList(len))")
            .expect("Dart wrapper must copy returned bytes into a List<int> via asTypedList");
        let free_pos = dart
            .find("_weaveffiFreeBytes(result, len)")
            .expect("Dart wrapper must free the returned pointer via _weaveffiFreeBytes");
        assert!(
            copy_pos < free_pos,
            "_weaveffiFreeBytes must run AFTER the payload is copied into a List<int>: {dart}"
        );
    }

    #[test]
    fn dart_struct_wrapper_calls_destroy() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains("class Contact {"),
            "Dart struct must be a class: {dart}"
        );
        assert!(
            dart.contains("Finalizer<Pointer<Void>>"),
            "Dart struct must register a Finalizer<Pointer>: {dart}"
        );
        assert!(
            dart.contains("_finalizer.attach(this, _handle, detach: this);"),
            "constructor must attach the finalizer: {dart}"
        );
        let dispose_pos = dart
            .find("void dispose() {")
            .expect("Dart struct must declare dispose()");
        let detach_pos = dart[dispose_pos..]
            .find("_finalizer.detach(this);")
            .map(|p| dispose_pos + p)
            .expect("dispose must detach the finalizer before destroying the handle");
        let destroy_pos = dart[dispose_pos..]
            .find("_weaveffiContactsContactDestroy(_handle);")
            .map(|p| dispose_pos + p)
            .expect("dispose must call the native destroy");
        assert!(detach_pos < destroy_pos);
    }

    #[test]
    fn dart_struct_setter_string_uses_uint8_pointer_and_length() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "set_contact_name".into(),
                params: vec![
                    Param {
                        name: "contact".into(),
                        ty: TypeRef::TypedHandle("Contact".into()),
                        mutable: false,
                    },
                    Param {
                        name: "new_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    },
                ],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains(
                "typedef _NativeWeaveffiContactsSetContactName = Void Function(Pointer<Void>, Pointer<Uint8>, IntPtr, Pointer<_WeaveffiError>);"
            ),
            "struct setter native typedef should expand StringUtf8 into (Pointer<Uint8>, IntPtr): {dart}"
        );
        assert!(
            dart.contains(
                "typedef _DartWeaveffiContactsSetContactName = void Function(Pointer<Void>, Pointer<Uint8>, int, Pointer<_WeaveffiError>);"
            ),
            "struct setter dart typedef should expand StringUtf8 into (Pointer<Uint8>, int): {dart}"
        );

        assert!(
            dart.contains("void setContactName(Contact contact, String newName)"),
            "struct setter wrapper should still take a Dart String: {dart}"
        );
        assert!(
            dart.contains("final newNameBytes = utf8.encode(newName);"),
            "struct setter wrapper must encode string to UTF-8 bytes: {dart}"
        );
        assert!(
            dart.contains("final newNameBuf = calloc<Uint8>(newNameBytes.length);"),
            "struct setter wrapper must allocate a Uint8 buffer: {dart}"
        );
        assert!(
            dart.contains("newNameBuf.asTypedList(newNameBytes.length).setAll(0, newNameBytes);"),
            "struct setter wrapper must copy bytes into the native buffer: {dart}"
        );
        assert!(
            dart.contains(
                "_weaveffiContactsSetContactName(contact._handle, newNameBuf, newNameBytes.length, err);"
            ),
            "struct setter wrapper must call native with (handle, buf, length, err): {dart}"
        );

        let fn_start = dart
            .find("void setContactName(Contact contact, String newName)")
            .expect("setContactName wrapper");
        let fn_body = &dart[fn_start..];
        let finally_pos = fn_body.find("} finally {").expect("finally block");
        let finally_body = &fn_body[finally_pos..];
        assert!(
            finally_body.contains("calloc.free(newNameBuf);"),
            "struct setter wrapper must free the buffer in the finally block: {finally_body}"
        );
    }

    #[test]
    fn dart_builder_setter_string_uses_uint8_pointer_and_length() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "Contact_Builder_set_name".into(),
                params: vec![
                    Param {
                        name: "builder".into(),
                        ty: TypeRef::Handle,
                        mutable: true,
                    },
                    Param {
                        name: "value".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                    },
                ],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains(
                "typedef _NativeWeaveffiContactsContactBuilderSetName = Void Function(Int64, Pointer<Uint8>, IntPtr, Pointer<_WeaveffiError>);"
            ),
            "builder setter native typedef should expand StringUtf8 into (Pointer<Uint8>, IntPtr): {dart}"
        );
        assert!(
            dart.contains(
                "typedef _DartWeaveffiContactsContactBuilderSetName = void Function(int, Pointer<Uint8>, int, Pointer<_WeaveffiError>);"
            ),
            "builder setter dart typedef should expand StringUtf8 into (Pointer<Uint8>, int): {dart}"
        );

        assert!(
            dart.contains("void contactBuilderSetName(int builder, String value)"),
            "builder setter wrapper should still take a Dart String: {dart}"
        );
        assert!(
            dart.contains("final valueBytes = utf8.encode(value);"),
            "builder setter wrapper must encode string to UTF-8 bytes: {dart}"
        );
        assert!(
            dart.contains("final valueBuf = calloc<Uint8>(valueBytes.length);"),
            "builder setter wrapper must allocate a Uint8 buffer: {dart}"
        );
        assert!(
            dart.contains("valueBuf.asTypedList(valueBytes.length).setAll(0, valueBytes);"),
            "builder setter wrapper must copy bytes into the native buffer: {dart}"
        );
        assert!(
            dart.contains(
                "_weaveffiContactsContactBuilderSetName(builder, valueBuf, valueBytes.length, err);"
            ),
            "builder setter wrapper must call native with (handle, buf, length, err): {dart}"
        );

        let fn_start = dart
            .find("void contactBuilderSetName(int builder, String value)")
            .expect("contactBuilderSetName wrapper");
        let fn_body = &dart[fn_start..];
        let finally_pos = fn_body.find("} finally {").expect("finally block");
        let finally_body = &fn_body[finally_pos..];
        assert!(
            finally_body.contains("calloc.free(valueBuf);"),
            "builder setter wrapper must free the buffer in the finally block: {finally_body}"
        );
    }

    #[test]
    fn capabilities_includes_callbacks_excludes_listeners_and_builders() {
        let caps = DartGenerator.capabilities();
        assert!(
            caps.contains(&Capability::Callbacks),
            "Dart generator must advertise Callbacks now that callback codegen is implemented"
        );
        assert!(
            !caps.contains(&Capability::Listeners),
            "Dart generator must not advertise Listeners until listener codegen is implemented"
        );
        assert!(
            !caps.contains(&Capability::Builders),
            "Dart generator must not advertise Builders while build() throws at runtime"
        );
        for cap in Capability::ALL {
            if matches!(cap, Capability::Listeners | Capability::Builders) {
                continue;
            }
            assert!(caps.contains(cap), "Dart generator must support {cap:?}");
        }
    }

    #[test]
    fn dart_emits_callback_typedef_using_pointer_from_function() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![Function {
                name: "subscribe".into(),
                params: vec![Param {
                    name: "handler".into(),
                    ty: TypeRef::Callback("OnMessage".into()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnMessage".into(),
                params: vec![Param {
                    name: "msg".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains("typedef OnMessage = void Function(String msg);"),
            "missing user-facing OnMessage typedef: {dart}"
        );
        assert!(
            dart.contains(
                "typedef _NativeOnMessage = Void Function(Pointer<Void>, Pointer<Uint8>, IntPtr);"
            ),
            "missing native OnMessage typedef with context-first + expanded StringUtf8: {dart}"
        );
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiEventsSubscribe = Void Function(Pointer<NativeFunction<_NativeOnMessage>>, Pointer<Void>, Pointer<_WeaveffiError>);"
            ),
            "subscribe typedef must pass callback pointer + context pointer: {dart}"
        );
        assert!(
            dart.contains("void subscribe(OnMessage handler)"),
            "wrapper should accept user-facing OnMessage type: {dart}"
        );
        assert!(
            dart.contains("final handlerPtr = Pointer.fromFunction<_NativeOnMessage>(handler);"),
            "wrapper must wrap callback via Pointer.fromFunction: {dart}"
        );
        assert!(
            dart.contains("_weaveffiEventsSubscribe(handlerPtr, nullptr, err);"),
            "wrapper must pass (pointer, nullptr context) to native fn: {dart}"
        );
    }

    #[test]
    fn dart_emits_listener_class() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnData".into(),
                params: vec![Param {
                    name: "value".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "data_stream".into(),
                event_callback: "OnData".into(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi");

        assert!(
            dart.contains(
                "typedef _NativeWeaveffiEventsRegisterDataStream = Uint64 Function(Pointer<NativeFunction<_NativeOnData>>, Pointer<Void>);"
            ),
            "register native typedef must return Uint64 and take (cb_ptr, context): {dart}"
        );
        assert!(
            dart.contains(
                "typedef _DartWeaveffiEventsRegisterDataStream = int Function(Pointer<NativeFunction<_NativeOnData>>, Pointer<Void>);"
            ),
            "register dart typedef must return int and take (cb_ptr, context): {dart}"
        );
        assert!(
            dart.contains("'weaveffi_events_register_data_stream'"),
            "must look up the register C symbol: {dart}"
        );
        assert!(
            dart.contains(
                "typedef _NativeWeaveffiEventsUnregisterDataStream = Void Function(Uint64);"
            ),
            "unregister native typedef must take (Uint64) and return Void: {dart}"
        );
        assert!(
            dart.contains("typedef _DartWeaveffiEventsUnregisterDataStream = void Function(int);"),
            "unregister dart typedef must take (int) and return void: {dart}"
        );
        assert!(
            dart.contains("'weaveffi_events_unregister_data_stream'"),
            "must look up the unregister C symbol: {dart}"
        );

        assert!(
            dart.contains("class DataStream {"),
            "must emit a Dart class named after the listener in PascalCase: {dart}"
        );
        assert!(
            dart.contains(
                "static final Map<int, Pointer<NativeFunction<_NativeOnData>>> _callbacks = {};"
            ),
            "listener class must pin callbacks in a Map<int, Pointer<NativeFunction<...>>>: {dart}"
        );
        assert!(
            dart.contains("static int register(OnData callback) {"),
            "listener class must expose static int register(CallbackType): {dart}"
        );
        assert!(
            dart.contains("final ptr = Pointer.fromFunction<_NativeOnData>(callback);"),
            "register must wrap the callback via Pointer.fromFunction: {dart}"
        );
        assert!(
            dart.contains("final id = _weaveffiEventsRegisterDataStream(ptr, nullptr);"),
            "register must call the native register symbol with (ptr, nullptr): {dart}"
        );
        assert!(
            dart.contains("_callbacks[id] = ptr;"),
            "register must store the pinned pointer in the map keyed by id: {dart}"
        );
        assert!(
            dart.contains("static void unregister(int id) {"),
            "listener class must expose static void unregister(int): {dart}"
        );
        assert!(
            dart.contains("_weaveffiEventsUnregisterDataStream(id);"),
            "unregister must call the native unregister symbol with the id: {dart}"
        );
        assert!(
            dart.contains("_callbacks.remove(id);"),
            "unregister must remove the pinned pointer from the map: {dart}"
        );

        let reg_start = dart
            .find("static int register(OnData callback) {")
            .expect("register method");
        let reg_body = &dart[reg_start..];
        let ptr_pos = reg_body
            .find("Pointer.fromFunction<_NativeOnData>(callback)")
            .expect("Pointer.fromFunction in register");
        let call_pos = reg_body
            .find("_weaveffiEventsRegisterDataStream(ptr, nullptr)")
            .expect("native call in register");
        let store_pos = reg_body
            .find("_callbacks[id] = ptr;")
            .expect("pin in register");
        let return_pos = reg_body.find("return id;").expect("return in register");
        assert!(
            ptr_pos < call_pos && call_pos < store_pos && store_pos < return_pos,
            "register ordering must be: fromFunction, native call, pin, return: {reg_body}"
        );
    }

    #[test]
    fn dart_open_library_respects_c_prefix() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![
                Param {
                    name: "a".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let config = GeneratorConfig {
            c_prefix: Some("myffi".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_dart_c_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let dart = std::fs::read_to_string(tmp.join("dart/lib/src/bindings.dart")).unwrap();

        assert!(
            dart.contains("DynamicLibrary.open('libmyffi.dylib')"),
            "_openLibrary must use libmyffi.dylib for macOS: {dart}"
        );
        assert!(
            dart.contains("DynamicLibrary.open('libmyffi.so')"),
            "_openLibrary must use libmyffi.so for Linux: {dart}"
        );
        assert!(
            dart.contains("DynamicLibrary.open('myffi.dll')"),
            "_openLibrary must use myffi.dll for Windows: {dart}"
        );
        assert!(
            !dart.contains("libweaveffi.dylib")
                && !dart.contains("libweaveffi.so")
                && !dart.contains("'weaveffi.dll'"),
            "must not retain default weaveffi library names when c_prefix is set: {dart}"
        );

        assert!(
            dart.contains("'myffi_math_add'"),
            "lookupFunction must reference myffi_math_add: {dart}"
        );
        assert!(
            !dart.contains("'weaveffi_math_add'"),
            "lookupFunction must not retain default weaveffi prefix: {dart}"
        );

        assert!(
            dart.contains("'myffi_error_clear'"),
            "preamble bindings must use c_prefix for error_clear: {dart}"
        );
        assert!(
            dart.contains("'myffi_free_string'"),
            "preamble bindings must use c_prefix for free_string: {dart}"
        );
        assert!(
            dart.contains("'myffi_free_bytes'"),
            "preamble bindings must use c_prefix for free_bytes: {dart}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dart_outputs_have_version_stamp() {
        let api = Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_dart_stamp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).unwrap();

        DartGenerator.generate(&api, out_dir).unwrap();

        let dart = std::fs::read_to_string(tmp.join("dart/lib/weaveffi.dart")).unwrap();
        assert!(
            dart.starts_with("// WeaveFFI "),
            "weaveffi.dart missing stamp: {dart}"
        );
        assert!(dart.contains(" dart "));
        assert!(dart.contains("DO NOT EDIT"));

        let pubspec = std::fs::read_to_string(tmp.join("dart/pubspec.yaml")).unwrap();
        assert!(
            pubspec.starts_with("# WeaveFFI "),
            "pubspec.yaml missing stamp: {pubspec}"
        );
        assert!(pubspec.contains(" dart "));
        assert!(pubspec.contains("DO NOT EDIT"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dart_pubspec_has_modern_sdk_constraints() {
        let api = make_api(vec![simple_module(vec![])]);
        let tmp = std::env::temp_dir().join("weaveffi_test_dart_modern_sdk");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator.generate(&api, out_dir).unwrap();

        let pubspec = std::fs::read_to_string(tmp.join("dart/pubspec.yaml")).unwrap();
        assert!(
            pubspec.contains("sdk: '>=3.0.0 <4.0.0'"),
            "pubspec should pin a modern Dart SDK range: {pubspec}"
        );
        assert!(
            !pubspec.contains("flutter:"),
            "pubspec should not require Flutter so pure-Dart consumers can \
             `dart pub get`: {pubspec}"
        );
        assert!(
            pubspec.contains("ffi: ^2.1.0"),
            "pubspec should use ffi ^2.1.0: {pubspec}"
        );
        assert!(
            pubspec.contains("dev_dependencies:") && pubspec.contains("test: ^1.24.0"),
            "pubspec should declare test ^1.24.0 as a dev dependency: {pubspec}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dart_has_analysis_options() {
        let api = make_api(vec![simple_module(vec![])]);
        let tmp = std::env::temp_dir().join("weaveffi_test_dart_analysis_options");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator.generate(&api, out_dir).unwrap();

        let options_path = tmp.join("dart/analysis_options.yaml");
        assert!(
            options_path.exists(),
            "analysis_options.yaml should be emitted"
        );
        let options = std::fs::read_to_string(&options_path).unwrap();
        assert!(
            options.contains("include: package:flutter_lints/flutter.yaml"),
            "analysis_options.yaml should enable flutter_lints: {options}"
        );
        assert!(
            options.starts_with("# WeaveFFI "),
            "analysis_options.yaml missing stamp: {options}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dart_has_barrel_export() {
        let api = make_api(vec![simple_module(vec![])]);
        let tmp = std::env::temp_dir().join("weaveffi_test_dart_barrel");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DartGenerator.generate(&api, out_dir).unwrap();

        let barrel_path = tmp.join("dart/lib/weaveffi.dart");
        let bindings_path = tmp.join("dart/lib/src/bindings.dart");
        assert!(barrel_path.exists(), "barrel lib/weaveffi.dart must exist");
        assert!(
            bindings_path.exists(),
            "internal lib/src/bindings.dart must exist"
        );

        let barrel = std::fs::read_to_string(&barrel_path).unwrap();
        assert!(
            barrel.contains("export 'src/bindings.dart';"),
            "barrel should re-export src/bindings.dart: {barrel}"
        );
        assert!(
            !barrel.contains("DynamicLibrary"),
            "barrel must not contain FFI implementation details: {barrel}"
        );

        let bindings = std::fs::read_to_string(&bindings_path).unwrap();
        assert!(
            bindings.contains("DynamicLibrary"),
            "internal bindings file should hold FFI declarations: {bindings}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
