use anyhow::Result;
use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_symbol_name, local_type_name};
use weaveffi_ir::ir::{Api, EnumDef, Function, Module, StructDef, TypeRef};

pub struct DartGenerator;

impl DartGenerator {
    fn generate_impl(&self, api: &Api, out_dir: &Utf8Path, package_name: &str) -> Result<()> {
        let dart_dir = out_dir.join("dart");
        let lib_dir = dart_dir.join("lib");
        std::fs::create_dir_all(&lib_dir)?;
        std::fs::write(lib_dir.join("weaveffi.dart"), render_dart_module(api))?;
        std::fs::write(dart_dir.join("pubspec.yaml"), render_pubspec(package_name))?;
        std::fs::write(dart_dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for DartGenerator {
    fn name(&self) -> &'static str {
        "dart"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "weaveffi")
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(api, out_dir, config.dart_package_name())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("dart/lib/weaveffi.dart").to_string(),
            out_dir.join("dart/pubspec.yaml").to_string(),
            out_dir.join("dart/README.md").to_string(),
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
    }
}

fn dart_nullable_type_for_builder_field(ty: &TypeRef) -> String {
    let t = dart_type(ty);
    if t.ends_with('?') {
        t
    } else {
        format!("{t}?")
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
        TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => "Pointer<Void>".into(),
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
        TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => "Pointer<Void>".into(),
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
         \x20 ffi: ^2.0.0\n"
    )
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

fn render_dart_module(api: &Api) -> String {
    let mut out = String::new();
    let has_async = collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));

    out.push_str("// Auto-generated by WeaveFFI. Do not edit.\n");
    out.push_str("// ignore_for_file: non_constant_identifier_names, camel_case_types\n\n");
    out.push_str("import 'dart:ffi';\n");
    out.push_str("import 'dart:io' show Platform;\n");
    if has_async {
        out.push_str("import 'dart:isolate';\n");
    }
    out.push_str("import 'package:ffi/ffi.dart';\n\n");

    out.push_str("DynamicLibrary _openLibrary() {\n");
    out.push_str("  if (Platform.isMacOS) return DynamicLibrary.open('libweaveffi.dylib');\n");
    out.push_str("  if (Platform.isLinux) return DynamicLibrary.open('libweaveffi.so');\n");
    out.push_str("  if (Platform.isWindows) return DynamicLibrary.open('weaveffi.dll');\n");
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

    emit_typedef_and_lookup(
        &mut out,
        "weaveffi_error_clear",
        "Pointer<_WeaveffiError>",
        "Pointer<_WeaveffiError>",
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

    out.push_str("void _checkError(Pointer<_WeaveffiError> err) {\n");
    out.push_str("  if (err.ref.code != 0) {\n");
    out.push_str("    final msg = err.ref.message.toDartString();\n");
    out.push_str("    _weaveffiErrorClear(err);\n");
    out.push_str("    throw WeaveffiException(err.ref.code, msg);\n");
    out.push_str("  }\n");
    out.push_str("}\n");

    for (module, path) in collect_modules_with_path(&api.modules) {
        for e in &module.enums {
            render_enum(&mut out, e);
        }
        for s in &module.structs {
            render_struct(&mut out, &path, s);
            if s.builder {
                render_dart_builder(&mut out, s);
            }
        }
        for f in &module.functions {
            render_function(&mut out, &path, f);
        }
    }

    out
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

fn render_struct(out: &mut String, module_path: &str, s: &StructDef) {
    let class_name = s.name.to_upper_camel_case();
    let c_prefix = format!("weaveffi_{}_{}", module_path, s.name);

    let destroy_sym = format!("{c_prefix}_destroy");
    emit_typedef_and_lookup(
        out,
        &destroy_sym,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );

    for field in &s.fields {
        let getter_sym = format!("{c_prefix}_get_{}", field.name);
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
    out.push_str(&format!("class {class_name} {{\n"));
    out.push_str("  final Pointer<Void> _handle;\n");
    out.push_str(&format!("  {class_name}._(this._handle);\n\n"));

    out.push_str("  void dispose() {\n");
    out.push_str(&format!(
        "    _{}(_handle);\n",
        destroy_sym.to_lower_camel_case()
    ));
    out.push_str("  }\n");

    for field in &s.fields {
        let getter_sym = format!("{c_prefix}_get_{}", field.name);
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
        emit_result_conversion(out, &field.ty, "      ");
        out.push_str("    } finally {\n");
        out.push_str("      calloc.free(err);\n");
        out.push_str("    }\n");
        out.push_str("  }\n");
    }

    out.push_str("}\n");
}

fn render_dart_builder(out: &mut String, s: &StructDef) {
    let class_name = s.name.to_upper_camel_case();
    let builder_name = format!("{class_name}Builder");

    out.push_str(&format!("\nclass {builder_name} {{\n"));
    for field in &s.fields {
        let dt = dart_nullable_type_for_builder_field(&field.ty);
        let priv_name = field.name.to_lower_camel_case();
        out.push_str(&format!("  {dt} _{priv_name};\n"));
    }

    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let dt = dart_type(&field.ty);
        let priv_name = field.name.to_lower_camel_case();
        out.push_str(&format!(
            "\n  {builder_name} with{pascal}({dt} value) {{\n    _{priv_name} = value;\n    return this;\n  }}\n"
        ));
    }

    out.push_str(&format!("\n  {class_name} build() {{\n"));
    for field in &s.fields {
        if !matches!(&field.ty, TypeRef::Optional(_)) {
            let priv_name = field.name.to_lower_camel_case();
            out.push_str(&format!(
                "    if (_{priv_name} == null) {{\n      throw StateError('missing field: {}');\n    }}\n",
                field.name
            ));
        }
    }
    out.push_str(&format!(
        "    throw UnimplementedError('{builder_name}.build requires FFI backing');\n"
    ));
    out.push_str("  }\n");
    out.push_str("}\n");
}

fn render_function(out: &mut String, module_path: &str, f: &Function) {
    let c_sym = c_symbol_name(module_path, &f.name);

    let mut native_params: Vec<String> = f.params.iter().map(|p| native_ffi_type(&p.ty)).collect();
    native_params.push("Pointer<_WeaveffiError>".into());
    let mut dart_params: Vec<String> = f.params.iter().map(|p| dart_ffi_type(&p.ty)).collect();
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
        emit_function_body(out, f, &c_sym);
        out.push_str("}\n");

        let call_args: Vec<String> = f
            .params
            .iter()
            .map(|p| p.name.to_lower_camel_case())
            .collect();
        out.push('\n');
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('\'', "\\'");
            out.push_str(&format!("@Deprecated('{escaped}')\n"));
        }
        out.push_str(&format!(
            "Future<{pub_ret}> {wrapper_name}({}) async {{\n",
            wrapper_params.join(", ")
        ));
        out.push_str(&format!(
            "  return await Isolate.run(() => {sync_name}({}));\n",
            call_args.join(", ")
        ));
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
        emit_function_body(out, f, &c_sym);
        out.push_str("}\n");
    }
}

fn emit_function_body(out: &mut String, f: &Function, c_sym: &str) {
    out.push_str("  final err = calloc<_WeaveffiError>();\n");

    let mut native_strings = Vec::new();
    for p in &f.params {
        if matches!(p.ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
            let pname = p.name.to_lower_camel_case();
            let ptr = format!("{pname}Ptr");
            out.push_str(&format!("  final {ptr} = {pname}.toNativeUtf8();\n"));
            native_strings.push(ptr);
        }
    }

    out.push_str("  try {\n");

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        let pname = p.name.to_lower_camel_case();
        call_args.push(match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{pname}Ptr"),
            TypeRef::Bool => format!("{pname} ? 1 : 0"),
            TypeRef::Enum(_) => format!("{pname}.value"),
            TypeRef::TypedHandle(_) | TypeRef::Struct(_) => format!("{pname}._handle"),
            _ => pname,
        });
    }
    call_args.push("err".into());

    let var = c_sym.to_lower_camel_case();
    if let Some(ret) = &f.returns {
        out.push_str(&format!(
            "    final result = _{var}({});\n",
            call_args.join(", ")
        ));
        out.push_str("    _checkError(err);\n");
        emit_result_conversion(out, ret, "    ");
    } else {
        out.push_str(&format!("    _{var}({});\n", call_args.join(", ")));
        out.push_str("    _checkError(err);\n");
    }

    out.push_str("  } finally {\n");
    for ns in &native_strings {
        out.push_str(&format!("    calloc.free({ns});\n"));
    }
    out.push_str("    calloc.free(err);\n");
    out.push_str("  }\n");
}

fn emit_result_conversion(out: &mut String, ty: &TypeRef, indent: &str) {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{indent}return result.toDartString();\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{indent}return result != 0;\n"));
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
                out.push_str(&format!("{indent}return result.toDartString();\n"));
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
        _ => {
            out.push_str(&format!("{indent}return result;\n"));
        }
    }
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
                out.join("dart/pubspec.yaml").to_string(),
                out.join("dart/README.md").to_string(),
            ]
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

        let dart = std::fs::read_to_string(tmp.join("dart/lib/weaveffi.dart")).unwrap();

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

        let dart = render_dart_module(&api);

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

        let dart = render_dart_module(&api);
        assert!(
            dart.contains("class PointBuilder {"),
            "builder class: {dart}"
        );
        assert!(
            dart.contains("PointBuilder withX(double value)"),
            "fluent setter: {dart}"
        );
        assert!(
            dart.contains("UnimplementedError('PointBuilder.build requires FFI backing')"),
            "build stub: {dart}"
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

        let dart = render_dart_module(&api);

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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);
        assert!(
            dart.contains("String echo(String msg)"),
            "missing signature: {dart}"
        );
        assert!(
            dart.contains("toNativeUtf8()"),
            "missing toNativeUtf8: {dart}"
        );
        assert!(
            dart.contains("result.toDartString()"),
            "missing toDartString: {dart}"
        );
        assert!(
            dart.contains("calloc.free(msgPtr)"),
            "missing free for string: {dart}"
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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);
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
            pubspec.contains("ffi: ^2.0.0"),
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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);
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

        let dart = render_dart_module(&api);

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

        let dart = render_dart_module(&api);

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

        let dart = render_dart_module(&api);

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
}
