//! Dart (`dart:ffi`) binding generator for WeaveFFI.
//!
//! Emits a Dart package (`pubspec.yaml` + library) with `dart:ffi`
//! bindings over the C ABI for use in Flutter and Dart projects.
//! Implements [`LanguageBackend`]; the shared driver bridges it into the
//! generator pipeline.

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToSnakeCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, FieldBinding, FnBinding,
    IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding, RichVariantBinding,
    StructBinding,
};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::utils::{local_type_name, render_prelude, render_trailer, CommentStyle};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`DartGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DartConfig {
    /// Dart package name (recorded in `pubspec.yaml`). Defaults to
    /// `"weaveffi"`.
    pub package_name: Option<String>,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the `dart:ffi` bindings call the
    /// same exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl DartConfig {
    pub fn package_name(&self) -> &str {
        self.package_name.as_deref().unwrap_or("weaveffi")
    }

    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

pub struct DartGenerator;

impl LanguageBackend for DartGenerator {
    type Config = DartConfig;

    fn name(&self) -> &'static str {
        "dart"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        api: &Api,
        _model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dart_dir = out_dir.join("dart");
        let lib_dir = dart_dir.join("lib");
        let input_basename = config.input_basename();
        vec![
            OutputFile::new(
                lib_dir.join("weaveffi.dart"),
                render_dart_module(api, config.prefix(), input_basename),
            ),
            OutputFile::new(
                dart_dir.join("pubspec.yaml"),
                render_pubspec(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
            OutputFile::new(
                dart_dir.join("README.md"),
                render_readme(
                    &pkg::resolve(
                        api,
                        config.package_name.as_deref(),
                        config.input_basename.as_deref(),
                    ),
                    input_basename,
                ),
            ),
        ]
    }
}

weaveffi_core::impl_generator_via_backend!(DartGenerator);

fn dart_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle => "int".into(),
        TypeRef::F32 | TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "List<int>".into(),
        // Structs, enums, and typed handles all surface as bare local Dart
        // classes. A cross-module typed handle (resolved to e.g. `kv.Store`) must
        // still name the local `Store` class, not the qualified IR name.
        TypeRef::TypedHandle(n) | TypeRef::Enum(n) | TypeRef::Struct(n) => {
            local_type_name(n).to_upper_camel_case()
        }
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
        TypeRef::I8 => "Int8".into(),
        TypeRef::I16 => "Int16".into(),
        TypeRef::I32 => "Int32".into(),
        TypeRef::U8 => "Uint8".into(),
        TypeRef::U16 => "Uint16".into(),
        TypeRef::U32 => "Uint32".into(),
        TypeRef::U64 => "Uint64".into(),
        TypeRef::I64 | TypeRef::Handle => "Int64".into(),
        TypeRef::F32 => "Float".into(),
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
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::Bool
        | TypeRef::Enum(_) => "int".into(),
        TypeRef::F32 | TypeRef::F64 => "double".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Pointer<Uint8>".into(),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => "Pointer<Void>".into(),
        TypeRef::Optional(inner) => dart_ffi_type(inner),
        TypeRef::List(_) | TypeRef::Iterator(_) | TypeRef::Map(_, _) => "Pointer<Void>".into(),
    }
}

// ── Complex-type marshaling (inputs, getters, returns) ──

/// dart:ffi (native, dart) types of a leaf scalar passed by value.
fn scalar_ffi(ty: &TypeRef) -> (&'static str, &'static str) {
    match ty {
        TypeRef::I8 => ("Int8", "int"),
        TypeRef::I16 => ("Int16", "int"),
        TypeRef::U8 => ("Uint8", "int"),
        TypeRef::U16 => ("Uint16", "int"),
        TypeRef::U32 => ("Uint32", "int"),
        TypeRef::U64 => ("Uint64", "int"),
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => ("Int32", "int"),
        TypeRef::I64 | TypeRef::Handle => ("Int64", "int"),
        TypeRef::F32 => ("Float", "double"),
        TypeRef::F64 => ("Double", "double"),
        _ => ("Int64", "int"),
    }
}

/// dart:ffi pointer type of the array staged for a `[T]`/map-side *input* (the C
/// element is `const char*` for strings, `T*` for handles, or a value scalar).
fn input_array_ffi(elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Pointer<Utf8>>".into(),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => "Pointer<Pointer<Void>>".into(),
        _ => format!("Pointer<{}>", scalar_ffi(elem).0),
    }
}

/// The (native, dart) FFI typedef slot pairs a single input parameter expands
/// into. Simple types stay one slot (matching [`native_ffi_type`]); bytes/list/
/// map fan out to the ABI's `(ptr, len)` / `(keys, vals, len)` shape; nullable
/// scalars pass through a pointer.
fn input_slots(ty: &TypeRef) -> Vec<(String, String)> {
    let ptr = |s: &str| (s.to_string(), s.to_string());
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![ptr("Pointer<Uint8>"), ("Size".into(), "int".into())]
        }
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            vec![ptr(&input_array_ffi(inner)), ("Size".into(), "int".into())]
        }
        TypeRef::Map(k, v) => vec![
            ptr(&input_array_ffi(k)),
            ptr(&input_array_ffi(v)),
            ("Size".into(), "int".into()),
        ],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![ptr("Pointer<Utf8>")],
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![ptr("Pointer<Void>")],
            other => vec![ptr(&format!("Pointer<{}>", scalar_ffi(other).0))],
        },
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![ptr("Pointer<Utf8>")],
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![ptr("Pointer<Void>")],
        _ => {
            let (n, d) = scalar_ffi(ty);
            vec![(n.into(), d.into())]
        }
    }
}

/// Emit pre-call staging for one input (`name`), returning the call-argument
/// expressions it contributes (in ABI order) and appending any cleanup
/// statements to `frees`. Mirrors the `(ptr, len)` / `(keys, vals, len)` ABI.
fn emit_input(out: &mut String, name: &str, ty: &TypeRef, frees: &mut Vec<String>) -> Vec<String> {
    match ty {
        TypeRef::Bool => vec![format!("{name} ? 1 : 0")],
        TypeRef::Enum(_) => vec![format!("{name}.value")],
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => vec![format!("{name}._handle")],
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => {
            vec![name.to_string()]
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let p = format!("{name}Ptr");
            out.push_str(&format!("  final {p} = {name}.toNativeUtf8();\n"));
            frees.push(format!("calloc.free({p});"));
            vec![p]
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let p = format!("{name}Ptr");
            out.push_str(&format!(
                "  final {p} = {name}.isEmpty ? nullptr : calloc<Uint8>({name}.length);\n"
            ));
            out.push_str(&format!(
                "  for (var i = 0; i < {name}.length; i++) {{ {p}[i] = {name}[i]; }}\n"
            ));
            frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
            vec![p, format!("{name}.length")]
        }
        TypeRef::Optional(inner) => emit_optional_input(out, name, inner, frees),
        TypeRef::List(inner) | TypeRef::Iterator(inner) => emit_list_input(out, name, inner, frees),
        TypeRef::Map(k, v) => emit_map_input(out, name, k, v, frees),
    }
}

fn emit_optional_input(
    out: &mut String,
    name: &str,
    inner: &TypeRef,
    frees: &mut Vec<String>,
) -> Vec<String> {
    let p = format!("{name}Ptr");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "  final {p} = {name} == null ? nullptr : {name}.toNativeUtf8();\n"
            ));
            frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
            vec![p]
        }
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => {
            vec![format!("{name}?._handle ?? nullptr")]
        }
        other => {
            let (native, _) = scalar_ffi(other);
            let val = match other {
                TypeRef::Bool => format!("{name} ? 1 : 0"),
                TypeRef::Enum(_) => format!("{name}.value"),
                _ => name.to_string(),
            };
            out.push_str(&format!("  Pointer<{native}> {p} = nullptr;\n"));
            out.push_str(&format!("  if ({name} != null) {{\n"));
            out.push_str(&format!("    {p} = calloc<{native}>();\n"));
            out.push_str(&format!("    {p}.value = {val};\n"));
            out.push_str("  }\n");
            frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
            vec![p]
        }
    }
}

fn emit_list_input(
    out: &mut String,
    name: &str,
    inner: &TypeRef,
    frees: &mut Vec<String>,
) -> Vec<String> {
    let p = format!("{name}Ptr");
    let arr_ty = input_array_ffi(inner);
    let inner_ffi = arr_ty
        .strip_prefix("Pointer<")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or("Pointer<Void>")
        .to_string();
    out.push_str(&format!(
        "  final {p} = {name}.isEmpty ? nullptr : calloc<{inner_ffi}>({name}.length);\n"
    ));
    out.push_str(&format!("  for (var i = 0; i < {name}.length; i++) {{\n"));
    out.push_str(&format!(
        "    {p}[i] = {};\n",
        elem_to_native(&format!("{name}[i]"), inner)
    ));
    out.push_str("  }\n");
    if matches!(inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
        frees.push(format!(
            "if ({p} != nullptr) {{ for (var i = 0; i < {name}.length; i++) {{ calloc.free({p}[i]); }} calloc.free({p}); }}"
        ));
    } else {
        frees.push(format!("if ({p} != nullptr) calloc.free({p});"));
    }
    vec![p, format!("{name}.length")]
}

fn emit_map_input(
    out: &mut String,
    name: &str,
    k: &TypeRef,
    v: &TypeRef,
    frees: &mut Vec<String>,
) -> Vec<String> {
    let kp = format!("{name}Keys");
    let vp = format!("{name}Vals");
    let kff = input_array_ffi(k);
    let vff = input_array_ffi(v);
    let ki = kff
        .strip_prefix("Pointer<")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or("Pointer<Void>")
        .to_string();
    let vi = vff
        .strip_prefix("Pointer<")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or("Pointer<Void>")
        .to_string();
    out.push_str(&format!(
        "  final {name}Entries = {name}.entries.toList();\n"
    ));
    out.push_str(&format!(
        "  final {kp} = {name}.isEmpty ? nullptr : calloc<{ki}>({name}.length);\n"
    ));
    out.push_str(&format!(
        "  final {vp} = {name}.isEmpty ? nullptr : calloc<{vi}>({name}.length);\n"
    ));
    out.push_str(&format!(
        "  for (var i = 0; i < {name}Entries.length; i++) {{\n"
    ));
    out.push_str(&format!(
        "    {kp}[i] = {};\n",
        elem_to_native(&format!("{name}Entries[i].key"), k)
    ));
    out.push_str(&format!(
        "    {vp}[i] = {};\n",
        elem_to_native(&format!("{name}Entries[i].value"), v)
    ));
    out.push_str("  }\n");
    let free_arr = |which: &str, ty: &TypeRef| -> String {
        if matches!(ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
            format!("if ({which} != nullptr) {{ for (var i = 0; i < {name}.length; i++) {{ calloc.free({which}[i]); }} calloc.free({which}); }}")
        } else {
            format!("if ({which} != nullptr) calloc.free({which});")
        }
    };
    frees.push(free_arr(&kp, k));
    frees.push(free_arr(&vp, v));
    vec![kp, vp, format!("{name}.length")]
}

/// Native expression converting a Dart element (`expr`) of a list/map into the
/// value stored in a native array slot.
fn elem_to_native(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{expr}.toNativeUtf8()"),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => format!("{expr}._handle"),
        TypeRef::Bool => format!("{expr} ? 1 : 0"),
        TypeRef::Enum(_) => format!("{expr}.value"),
        _ => expr.to_string(),
    }
}

/// Whether a return type lowers to callee-allocated out-parameters (so the call
/// wrapper must allocate `outLen`/`outKeys`/`outVals` and decode afterwards).
fn return_has_out_params(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) | TypeRef::Map(_, _)
    )
}

/// Whether an `optional<inner>` already lowers to a nullable pointer (so the
/// option is encoded by the pointer's nullness, not an extra indirection).
fn optional_inner_is_pointer(inner: &TypeRef) -> bool {
    matches!(
        inner,
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Struct(_) | TypeRef::TypedHandle(_)
    )
}

/// The FFI return type (native, dart) of a call symbol. Maps return-by-value via
/// out-params to a `void` symbol; an optional scalar lowers to a nullable
/// pointer-to-scalar; everything else follows [`native_ffi_type`].
fn return_ffi(ty: &TypeRef) -> (String, String) {
    match ty {
        TypeRef::Map(_, _) => ("Void".into(), "void".into()),
        TypeRef::Optional(inner) if !optional_inner_is_pointer(inner) => {
            let p = format!("Pointer<{}>", scalar_ffi(inner).0);
            (p.clone(), p)
        }
        _ => (native_ffi_type(ty), dart_ffi_type(ty)),
    }
}

/// dart:ffi type of the `outKeys`/`outValues` pointer passed *by address* for a
/// map return (the C slot is `K***`/`V***`).
fn map_out_ffi(elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Pointer<Pointer<Utf8>>>".into(),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => "Pointer<Pointer<Pointer<Void>>>".into(),
        _ => format!("Pointer<Pointer<{}>>", scalar_ffi(elem).0),
    }
}

/// Read one decoded map key/value (`arr` is the `outX.value` array pointer).
fn map_elem_read(arr: &str, idx: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{arr}[{idx}].toDartString()"),
        TypeRef::Enum(n) => format!(
            "{}.fromValue({arr}[{idx}])",
            local_type_name(n).to_upper_camel_case()
        ),
        TypeRef::Bool => format!("{arr}[{idx}] != 0"),
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => {
            format!(
                "{}._({arr}[{idx}])",
                local_type_name(n).to_upper_camel_case()
            )
        }
        _ => format!("{arr}[{idx}]"),
    }
}

/// The trailing FFI typedef slots (native, dart) a return type contributes for
/// its callee-allocated out-parameters.
fn return_out_slots(ty: &TypeRef) -> Vec<(String, String)> {
    let ptr = |s: String| (s.clone(), s);
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
            vec![ptr("Pointer<Size>".into())]
        }
        TypeRef::Map(k, v) => vec![
            ptr(map_out_ffi(k)),
            ptr(map_out_ffi(v)),
            ptr("Pointer<Size>".into()),
        ],
        _ => vec![],
    }
}

/// Allocate the out-parameter locals a complex return needs before the call,
/// returning the extra call-argument expressions and recording cleanup.
fn emit_return_alloc(
    out: &mut String,
    ty: &TypeRef,
    frees: &mut Vec<String>,
    indent: &str,
) -> Vec<String> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
            out.push_str(&format!("{indent}final outLen = calloc<Size>();\n"));
            frees.push("calloc.free(outLen);".into());
            vec!["outLen".into()]
        }
        TypeRef::Map(k, v) => {
            let kf = map_out_ffi(k);
            let vf = map_out_ffi(v);
            // `outKeys`/`outValues` hold the array pointer the callee writes.
            let ki = kf
                .strip_prefix("Pointer<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap();
            let vi = vf
                .strip_prefix("Pointer<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap();
            out.push_str(&format!("{indent}final outKeys = calloc<{ki}>();\n"));
            out.push_str(&format!("{indent}final outValues = calloc<{vi}>();\n"));
            out.push_str(&format!("{indent}final outLen = calloc<Size>();\n"));
            frees.push("calloc.free(outKeys);".into());
            frees.push("calloc.free(outValues);".into());
            frees.push("calloc.free(outLen);".into());
            vec!["outKeys".into(), "outValues".into(), "outLen".into()]
        }
        _ => vec![],
    }
}

/// Emit the post-call decode of a (possibly complex) return into the wrapper's
/// Dart return value. `result` is the call result (absent for `void` map returns).
fn emit_return_decode(out: &mut String, ty: &TypeRef, indent: &str) {
    match ty {
        TypeRef::List(inner) => emit_list_conversion(out, inner, indent),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{indent}final n = outLen.value;\n"));
            out.push_str(&format!(
                "{indent}if (result == nullptr || n == 0) return <int>[];\n"
            ));
            out.push_str(&format!(
                "{indent}return List<int>.generate(n, (i) => result[i]);\n"
            ));
        }
        TypeRef::Map(k, v) => {
            let kt = dart_type(k);
            let vt = dart_type(v);
            out.push_str(&format!("{indent}final n = outLen.value;\n"));
            out.push_str(&format!("{indent}final m = <{kt}, {vt}>{{}};\n"));
            out.push_str(&format!("{indent}final keys = outKeys.value;\n"));
            out.push_str(&format!("{indent}final vals = outValues.value;\n"));
            out.push_str(&format!("{indent}for (var i = 0; i < n; i++) {{\n"));
            out.push_str(&format!(
                "{indent}  m[{}] = {};\n",
                map_elem_read("keys", "i", k),
                map_elem_read("vals", "i", v)
            ));
            out.push_str(&format!("{indent}}}\n"));
            out.push_str(&format!("{indent}return m;\n"));
        }
        _ => emit_result_conversion(out, ty, indent),
    }
}

/// Convert a single native leaf value (`expr`) into its Dart representation.
fn read_value(expr: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("{expr}.toDartString()"),
        TypeRef::Enum(n) => format!(
            "{}.fromValue({expr})",
            local_type_name(n).to_upper_camel_case()
        ),
        TypeRef::Bool => format!("{expr} != 0"),
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => {
            format!("{}._({expr})", local_type_name(n).to_upper_camel_case())
        }
        _ => expr.to_string(),
    }
}

/// dart:ffi pointee type allocated for an iterator's `out_item` slot (the C slot
/// is `T*`, so we allocate one `T`).
fn iter_item_pointee(elem: &TypeRef) -> String {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>".into(),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => "Pointer<Void>".into(),
        _ => scalar_ffi(elem).0.to_string(),
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

fn render_pubspec(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "pubspec.yaml");
    let name = package.ident_name();
    let version = &package.version;
    let description = package.description_or_default();
    let mut meta = format!("description: {description}\n");
    if let Some(homepage) = package.homepage.as_ref().or(package.repository.as_ref()) {
        meta.push_str(&format!("homepage: {homepage}\n"));
    }
    format!(
        "{prelude}name: {name}\n\
         version: {version}\n\
         {meta}\
         environment:\n\
         \x20 sdk: '>=3.0.0 <4.0.0'\n\
         dependencies:\n\
         \x20 ffi: ^2.0.0\n\n\
         {trailer}"
    )
}

fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let import_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Dart)

Auto-generated Dart bindings using `dart:ffi`.

## Usage

1. Place the compiled shared library (`libweaveffi.dylib`, `libweaveffi.so`,
   or `weaveffi.dll`) where the Dart process can find it.

2. Add this package as a dependency and import the bindings:

```dart
import 'package:{import_name}/weaveffi.dart';
```

3. Call the generated functions directly. The bindings use `dart:ffi` to load
   the native library at runtime via `DynamicLibrary.open` and resolve symbols
   with `lookupFunction`.

## Requirements

- Dart SDK >= 3.0.0
- The `ffi` package (`^2.0.0`) for `Utf8` and `calloc` helpers.

{trailer}"#
    )
}

fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::TripleSlash);
}

fn render_dart_module(api: &Api, prefix: &str, input_basename: &str) -> String {
    let model = BindingModel::build(api, prefix);
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let has_async = model.functions().any(|(_, f)| f.is_async);
    // The default shared-library basename follows the package identity
    // (`lib<name>`), matching the producer cdylib. WEAVEFFI_LIBRARY still wins.
    let resolved = pkg::resolve(api, None, Some(input_basename));
    let lib_base = resolved.ident_name();

    out.push_str("// ignore_for_file: non_constant_identifier_names, camel_case_types\n\n");
    out.push_str("import 'dart:ffi';\n");
    out.push_str("import 'dart:io' show Platform;\n");
    if has_async {
        out.push_str("import 'dart:async';\n");
    }
    out.push_str("import 'package:ffi/ffi.dart';\n\n");

    out.push_str("DynamicLibrary _openLibrary() {\n");
    out.push_str("  // An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a\n");
    out.push_str("  // specific build artifact regardless of its file name or location.\n");
    out.push_str("  final override = Platform.environment['WEAVEFFI_LIBRARY'];\n");
    out.push_str(
        "  if (override != null && override.isNotEmpty) return DynamicLibrary.open(override);\n",
    );
    out.push_str(&format!(
        "  if (Platform.isMacOS) return DynamicLibrary.open('lib{lib_base}.dylib');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isLinux) return DynamicLibrary.open('lib{lib_base}.so');\n"
    ));
    out.push_str(&format!(
        "  if (Platform.isWindows) return DynamicLibrary.open('{lib_base}.dll');\n"
    ));
    out.push_str(
        "  throw UnsupportedError('Unsupported platform: ${Platform.operatingSystem}');\n",
    );
    out.push_str("}\n\n");
    out.push_str("final DynamicLibrary _lib = _openLibrary();\n\n");

    out.push_str("final class _WeaveFFIError extends Struct {\n");
    out.push_str("  @Int32()\n");
    out.push_str("  external int code;\n");
    out.push_str("  external Pointer<Utf8> message;\n");
    out.push_str("}\n");

    emit_typedef_and_lookup(
        &mut out,
        "weaveffi_error_clear",
        "Pointer<_WeaveFFIError>",
        "Pointer<_WeaveFFIError>",
        "Void",
        "void",
    );

    out.push_str("\nclass WeaveFFIException implements Exception {\n");
    out.push_str("  final int code;\n");
    out.push_str("  final String message;\n");
    out.push_str("  WeaveFFIException(this.code, this.message);\n");
    out.push_str("  @override\n");
    out.push_str("  String toString() => 'WeaveFFIException($code): $message';\n");
    out.push_str("}\n\n");

    out.push_str("void _checkError(Pointer<_WeaveFFIError> err) {\n");
    out.push_str("  if (err.ref.code != 0) {\n");
    // Capture code and message *before* clearing, which zeroes the struct.
    out.push_str("    final code = err.ref.code;\n");
    out.push_str("    final msg = err.ref.message.toDartString();\n");
    out.push_str("    _weaveffiErrorClear(err);\n");
    out.push_str("    throw WeaveFFIException(code, msg);\n");
    out.push_str("  }\n");
    out.push_str("}\n");

    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    if has_listeners {
        out.push_str("\n// Live listener trampolines by subscription id. Holding the\n");
        out.push_str("// NativeCallable here keeps its native thunk alive until unregistered.\n");
        out.push_str("final Map<int, NativeCallable> _listenerCallables = {};\n");
    }

    for module in &model.modules {
        for e in &module.enums {
            render_enum(&mut out, e);
        }
        for s in &module.structs {
            render_struct(&mut out, s);
            if s.builder.is_some() {
                render_dart_builder(&mut out, s);
            }
        }
        for cb in &module.callbacks {
            render_callback_typedef(&mut out, cb);
        }
        for l in &module.listeners {
            render_listener(&mut out, module, l);
        }
        for f in &module.functions {
            render_function(&mut out, f);
        }
    }

    out.push('\n');
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, "weaveffi.dart"));
    out
}

fn render_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum crosses the ABI as an opaque object, so it is
    // emitted as a wrapper class (like a struct), not a plain Dart `enum`.
    if e.is_rich() {
        render_rich_enum(out, e);
        return;
    }
    let name = e.name.to_upper_camel_case();
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("enum {name} {{\n"));
    for v in &e.variants {
        let vname = v.name.to_lower_camel_case();
        emit_doc(out, &v.doc, "  ");
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

fn render_struct(out: &mut String, s: &StructBinding) {
    let class_name = s.name.to_upper_camel_case();
    // Symbols come precomputed from the shared BindingModel, so Dart never
    // re-derives the `{prefix}_{module}_{Name}_*` scheme itself.
    let destroy_sym = &s.destroy_symbol;
    emit_typedef_and_lookup(
        out,
        destroy_sym,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );

    for field in &s.fields {
        emit_field_getter_typedef(out, field);
    }

    out.push('\n');
    emit_doc(out, &s.doc, "");
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
        emit_field_getter_method(out, field);
    }

    out.push_str("}\n");
}

/// Emit the dart:ffi typedef + `lookupFunction` for one opaque-object field
/// getter (a struct field or a rich-enum variant field). The getter takes only
/// the opaque handle and reports no error; a bytes/list field adds its
/// callee-allocated out-param and a map field adds its triple and lowers to a
/// `void` symbol. The lookup is keyed on the field's precomputed
/// `getter_symbol`, so a rich enum may rename the Dart member freely.
fn emit_field_getter_typedef(out: &mut String, field: &FieldBinding) {
    let getter_sym = &field.getter_symbol;
    let mut nparams = vec!["Pointer<Void>".to_string()];
    let mut dparams = vec!["Pointer<Void>".to_string()];
    for (n, d) in return_out_slots(&field.ty) {
        nparams.push(n);
        dparams.push(d);
    }
    let (nr, dr) = return_ffi(&field.ty);
    emit_typedef_and_lookup(
        out,
        getter_sym,
        &nparams.join(", "),
        &dparams.join(", "),
        &nr,
        &dr,
    );
}

/// Emit the idiomatic Dart getter for one opaque-object field. The member name
/// comes from `field.name` — so a rich enum can namespace it per variant (e.g.
/// `circleRadius`) by passing a renamed [`FieldBinding`] — while the FFI lookup
/// stays keyed on the precomputed `getter_symbol`. Receiver is the wrapper's
/// `_handle`, common to both the struct and rich-enum classes.
fn emit_field_getter_method(out: &mut String, field: &FieldBinding) {
    let getter_sym = &field.getter_symbol;
    let dart_ret = dart_type(&field.ty);
    let fname = field.name.to_lower_camel_case();

    out.push('\n');
    emit_doc(out, &field.doc, "  ");
    out.push_str(&format!("  {dart_ret} get {fname} {{\n"));
    if return_has_out_params(&field.ty) {
        let mut frees: Vec<String> = Vec::new();
        let mut args = vec!["_handle".to_string()];
        args.extend(emit_return_alloc(out, &field.ty, &mut frees, "    "));
        out.push_str("    try {\n");
        if matches!(&field.ty, TypeRef::Map(_, _)) {
            out.push_str(&format!(
                "      _{}({});\n",
                getter_sym.to_lower_camel_case(),
                args.join(", ")
            ));
        } else {
            out.push_str(&format!(
                "      final result = _{}({});\n",
                getter_sym.to_lower_camel_case(),
                args.join(", ")
            ));
        }
        emit_return_decode(out, &field.ty, "      ");
        out.push_str("    } finally {\n");
        for fr in &frees {
            out.push_str(&format!("      {fr}\n"));
        }
        out.push_str("    }\n");
    } else {
        out.push_str(&format!(
            "    final result = _{}(_handle);\n",
            getter_sym.to_lower_camel_case()
        ));
        emit_result_conversion(out, &field.ty, "    ");
    }
    out.push_str("  }\n");
}

fn render_dart_builder(out: &mut String, s: &StructBinding) {
    let class_name = s.name.to_upper_camel_case();
    let builder_name = format!("{class_name}Builder");
    let create_sym = &s.create.symbol;

    // `{Struct}_create(<field slots>, error* out_err) -> {Struct}*`: each field
    // expands to its ABI slots, then the trailing error pointer.
    let mut nparams: Vec<String> = Vec::new();
    let mut dparams: Vec<String> = Vec::new();
    for field in &s.fields {
        for (n, d) in input_slots(&field.ty) {
            nparams.push(n);
            dparams.push(d);
        }
    }
    nparams.push("Pointer<_WeaveFFIError>".into());
    dparams.push("Pointer<_WeaveFFIError>".into());
    emit_typedef_and_lookup(
        out,
        create_sym,
        &nparams.join(", "),
        &dparams.join(", "),
        "Pointer<Void>",
        "Pointer<Void>",
    );

    out.push('\n');
    emit_doc(out, &s.doc, "");
    out.push_str(&format!("class {builder_name} {{\n"));
    for field in &s.fields {
        let dt = dart_nullable_type_for_builder_field(&field.ty);
        let priv_name = field.name.to_lower_camel_case();
        out.push_str(&format!("  {dt} _{priv_name};\n"));
    }

    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let dt = dart_type(&field.ty);
        let priv_name = field.name.to_lower_camel_case();
        out.push('\n');
        emit_doc(out, &field.doc, "  ");
        out.push_str(&format!(
            "  {builder_name} with{pascal}({dt} value) {{\n    _{priv_name} = value;\n    return this;\n  }}\n"
        ));
    }

    out.push_str(&format!("\n  {class_name} build() {{\n"));
    // Required fields must be set; optional fields default to null.
    for field in &s.fields {
        if !matches!(&field.ty, TypeRef::Optional(_)) {
            let priv_name = field.name.to_lower_camel_case();
            out.push_str(&format!(
                "    if (_{priv_name} == null) {{\n      throw StateError('missing field: {}');\n    }}\n",
                field.name
            ));
        }
    }
    for field in &s.fields {
        let priv_name = field.name.to_lower_camel_case();
        if matches!(&field.ty, TypeRef::Optional(_)) {
            out.push_str(&format!("    final {priv_name} = _{priv_name};\n"));
        } else {
            out.push_str(&format!("    final {priv_name} = _{priv_name}!;\n"));
        }
    }
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    for field in &s.fields {
        let args = emit_input(
            out,
            &field.name.to_lower_camel_case(),
            &field.ty,
            &mut frees,
        );
        call_args.extend(args);
    }
    out.push_str("    final err = calloc<_WeaveFFIError>();\n");
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());
    out.push_str("    try {\n");
    out.push_str(&format!(
        "      final result = _{}({});\n",
        create_sym.to_lower_camel_case(),
        call_args.join(", ")
    ));
    out.push_str("      _checkError(err);\n");
    out.push_str(&format!("      return {class_name}._(result);\n"));
    out.push_str("    } finally {\n");
    for fr in &frees {
        out.push_str(&format!("      {fr}\n"));
    }
    out.push_str("    }\n");
    out.push_str("  }\n");
    out.push_str("}\n");
}

/// Render a rich (algebraic) enum as an opaque-object wrapper, mirroring the
/// Dart struct wrapper: it owns the C handle behind a private `_handle` (so the
/// existing function marshalling — `x._handle` in, `Name._(result)` out — keeps
/// working unchanged, since a rich enum lowers to `TypeRef::Struct`), frees it
/// once in `dispose()`, and exposes a `tag` discriminant reader, one `factory`
/// per variant (`Shape.circle(2.5)`), and per-variant field getters namespaced
/// by variant (`circleRadius`) to avoid collisions. The opaque-object surface
/// (tag/destroy symbols, per-variant constructors and field getters) is
/// precomputed in the binding model exactly like a struct's.
fn render_rich_enum(out: &mut String, e: &EnumBinding) {
    let rich = e
        .rich
        .as_ref()
        .expect("render_rich_enum requires a rich (algebraic) enum");
    let class_name = e.name.to_upper_camel_case();
    let tag_name = format!("{class_name}Tag");

    // A top-level discriminant enum (`ShapeTag.circle`), rendered exactly like a
    // plain enum so the active variant reads back as a typed value.
    render_rich_enum_tag(out, e, &tag_name);

    // FFI typedefs + lookups, keyed on the model's precomputed symbols: the
    // destructor, the tag getter, one constructor per variant, and every
    // per-variant field getter.
    emit_typedef_and_lookup(
        out,
        &rich.destroy_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );
    emit_typedef_and_lookup(
        out,
        &rich.tag_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Int32",
        "int",
    );
    for v in &rich.variants {
        emit_rich_variant_create_typedef(out, v);
    }
    for v in &rich.variants {
        for field in namespaced_variant_fields(v) {
            emit_field_getter_typedef(out, &field);
        }
    }

    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("class {class_name} {{\n"));
    out.push_str("  final Pointer<Void> _handle;\n");
    out.push_str(&format!("  {class_name}._(this._handle);\n\n"));

    out.push_str("  void dispose() {\n");
    out.push_str(&format!(
        "    _{}(_handle);\n",
        rich.destroy_symbol.to_lower_camel_case()
    ));
    out.push_str("  }\n");

    // The active variant's discriminant, read back as the typed tag enum.
    out.push('\n');
    out.push_str(&format!(
        "  {tag_name} get tag =>\n      {tag_name}.fromValue(_{}(_handle));\n",
        rich.tag_symbol.to_lower_camel_case()
    ));

    // One factory constructor per variant (`Shape.circle(2.5)`).
    for v in &rich.variants {
        emit_rich_variant_factory(out, &class_name, v);
    }

    // Per-variant field getters, namespaced by variant (`circleRadius`).
    for v in &rich.variants {
        for field in namespaced_variant_fields(v) {
            emit_field_getter_method(out, &field);
        }
    }

    out.push_str("}\n");
}

/// The typed discriminant of a rich enum, emitted as a top-level Dart `enum`
/// (Dart cannot nest an `enum` in a class). Mirrors [`render_enum`]'s enhanced
/// enum so `tag` reads back as e.g. `ShapeTag.circle`.
fn render_rich_enum_tag(out: &mut String, e: &EnumBinding, tag_name: &str) {
    out.push('\n');
    emit_doc(out, &e.doc, "");
    out.push_str(&format!("enum {tag_name} {{\n"));
    for v in &e.variants {
        let vname = v.name.to_lower_camel_case();
        emit_doc(out, &v.doc, "  ");
        out.push_str(&format!("  {vname}({}),\n", v.value));
    }
    out.push_str("  ;\n");
    out.push_str(&format!("  const {tag_name}(this.value);\n"));
    out.push_str("  final int value;\n\n");
    out.push_str(&format!(
        "  static {tag_name} fromValue(int value) =>\n      {tag_name}.values.firstWhere((e) => e.value == value);\n"
    ));
    out.push_str("}\n");
}

/// Project a variant's fields into [`FieldBinding`]s whose Dart member name is
/// namespaced by the variant (`circle` + `radius` -> `circle_radius`, rendered
/// `circleRadius`). The precomputed `getter_symbol` is left untouched, so the
/// FFI lookup still targets the correct per-variant C symbol — this is what lets
/// the rich enum reuse the struct field-getter renderers verbatim.
fn namespaced_variant_fields(v: &RichVariantBinding) -> Vec<FieldBinding> {
    let variant = v.name.to_snake_case();
    v.fields
        .iter()
        .map(|f| {
            let mut namespaced = f.clone();
            namespaced.name = format!("{variant}_{}", f.name);
            namespaced
        })
        .collect()
}

/// Emit the dart:ffi typedef + lookup for one variant constructor
/// (`{c_tag}_{Variant}_new`): each variant field lowers to its ABI input slots,
/// then a trailing `out_err`; the call returns the opaque handle. Mirrors the
/// struct builder's `create` typedef.
fn emit_rich_variant_create_typedef(out: &mut String, v: &RichVariantBinding) {
    let create_sym = &v.create.symbol;
    let mut nparams: Vec<String> = Vec::new();
    let mut dparams: Vec<String> = Vec::new();
    for f in &v.fields {
        for (n, d) in input_slots(&f.ty) {
            nparams.push(n);
            dparams.push(d);
        }
    }
    nparams.push("Pointer<_WeaveFFIError>".into());
    dparams.push("Pointer<_WeaveFFIError>".into());
    emit_typedef_and_lookup(
        out,
        create_sym,
        &nparams.join(", "),
        &dparams.join(", "),
        "Pointer<Void>",
        "Pointer<Void>",
    );
}

/// Emit one variant's factory constructor (`Shape.circle(double radius)`).
/// Mirrors the struct builder's `build()`: each field marshals to its ABI
/// argument slots via [`emit_input`], the call threads an `out_err` checked with
/// `_checkError`, and the returned handle is wrapped (`return Shape._(result)`).
/// A unit variant takes no parameters and passes only the error slot.
fn emit_rich_variant_factory(out: &mut String, class_name: &str, v: &RichVariantBinding) {
    let create_sym = &v.create.symbol;
    let factory = v.name.to_lower_camel_case();
    let params: Vec<String> = v
        .fields
        .iter()
        .map(|f| format!("{} {}", dart_type(&f.ty), f.name.to_lower_camel_case()))
        .collect();

    out.push('\n');
    emit_doc(out, &v.doc, "  ");
    out.push_str(&format!(
        "  factory {class_name}.{factory}({}) {{\n",
        params.join(", ")
    ));

    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    for f in &v.fields {
        let args = emit_input(out, &f.name.to_lower_camel_case(), &f.ty, &mut frees);
        call_args.extend(args);
    }
    out.push_str("    final err = calloc<_WeaveFFIError>();\n");
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());
    out.push_str("    try {\n");
    out.push_str(&format!(
        "      final result = _{}({});\n",
        create_sym.to_lower_camel_case(),
        call_args.join(", ")
    ));
    out.push_str("      _checkError(err);\n");
    out.push_str(&format!("      return {class_name}._(result);\n"));
    out.push_str("    } finally {\n");
    for fr in &frees {
        out.push_str(&format!("      {fr}\n"));
    }
    out.push_str("    }\n");
    out.push_str("  }\n");
}

fn render_function(out: &mut String, f: &FnBinding) {
    // `c_base` is the prefixed `{prefix}_{module}_{name}` symbol the shared
    // BindingModel already computed; the async/iterator suffixing matches the C
    // ABI by construction.
    let c_sym = f.c_base.as_str();
    let wrapper_name = f.name.to_lower_camel_case();
    let pub_ret = f.ret.as_ref().map_or("void".into(), dart_type);
    let wrapper_params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), p.name.to_lower_camel_case()))
        .collect();

    if f.is_async {
        render_async_function(out, c_sym, f, &wrapper_name, &pub_ret, &wrapper_params);
        return;
    }

    // Each input parameter expands to its ABI slots (bytes/list/map fan out to
    // `(ptr, len)` / `(keys, vals, len)`); a complex return adds its callee-
    // allocated out-params; the trailing error slot closes the signature.
    let mut native_params: Vec<String> = Vec::new();
    let mut dart_params: Vec<String> = Vec::new();
    for p in &f.params {
        for (n, d) in input_slots(&p.ty) {
            native_params.push(n);
            dart_params.push(d);
        }
    }
    if let Some(ret) = &f.ret {
        for (n, d) in return_out_slots(ret) {
            native_params.push(n);
            dart_params.push(d);
        }
    }
    native_params.push("Pointer<_WeaveFFIError>".into());
    dart_params.push("Pointer<_WeaveFFIError>".into());

    let (native_ret, dart_ret) = match &f.ret {
        Some(ret) => return_ffi(ret),
        None => ("Void".into(), "void".into()),
    };

    emit_typedef_and_lookup(
        out,
        c_sym,
        &native_params.join(", "),
        &dart_params.join(", "),
        &native_ret,
        &dart_ret,
    );

    // Iterator-returning functions also bind the element `next`/`destroy` symbols.
    if let CallShape::Iterator(ib) = &f.shape {
        emit_iter_lookups(out, ib);
    }

    out.push('\n');
    emit_doc(out, &f.doc, "");
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('\'', "\\'");
        out.push_str(&format!("@Deprecated('{escaped}')\n"));
    }
    out.push_str(&format!(
        "{pub_ret} {wrapper_name}({}) {{\n",
        wrapper_params.join(", ")
    ));
    emit_function_body(out, f, c_sym);
    out.push_str("}\n");
}

/// The native FFI typedef for a module-level callback declaration, shared by
/// every listener that fires it.
fn render_callback_typedef(out: &mut String, cb: &CallbackBinding) {
    let mut slots: Vec<String> = Vec::new();
    for p in &cb.params {
        for (n, _) in input_slots(&p.ty) {
            slots.push(n);
        }
    }
    slots.push("Pointer<Void>".into());
    out.push_str(&format!(
        "\ntypedef _NativeCb_{} = Void Function({});\n",
        cb.c_fn_type,
        slots.join(", ")
    ));
}

/// The Dart expression converting one callback parameter's trampoline slots
/// into the value handed to the user callback. Slot names follow the lowered
/// ABI (`{n}`, `{n}_ptr`/`{n}_len`, `{n}_keys`/`{n}_values`/`{n}_len`).
fn cb_arg_expr(p: &ParamBinding) -> String {
    let n0 = p.abi[0].name.to_lower_camel_case();
    match &p.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::I64
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::U64
        | TypeRef::Handle
        | TypeRef::F32
        | TypeRef::F64 => n0,
        TypeRef::Bool => format!("{n0} != 0"),
        TypeRef::Enum(name) => format!(
            "{}.fromValue({n0})",
            local_type_name(name).to_upper_camel_case()
        ),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("{n0} == nullptr ? '' : {n0}.toDartString()")
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let len = p.abi[1].name.to_lower_camel_case();
            format!("{n0} == nullptr ? <int>[] : {n0}.asTypedList({len}).toList()")
        }
        // Borrowed for the duration of the callback: do not dispose().
        TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
            format!("{}._({n0})", local_type_name(name).to_upper_camel_case())
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                format!("{n0} == nullptr ? null : {n0}.toDartString()")
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                let len = p.abi[1].name.to_lower_camel_case();
                format!("{n0} == nullptr ? null : {n0}.asTypedList({len}).toList()")
            }
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => format!(
                "{n0} == nullptr ? null : {}._({n0})",
                local_type_name(name).to_upper_camel_case()
            ),
            TypeRef::Bool => format!("{n0} == nullptr ? null : {n0}.value != 0"),
            TypeRef::Enum(name) => format!(
                "{n0} == nullptr ? null : {}.fromValue({n0}.value)",
                local_type_name(name).to_upper_camel_case()
            ),
            _ => format!("{n0} == nullptr ? null : {n0}.value"),
        },
        TypeRef::List(inner) => {
            let len = p.abi[1].name.to_lower_camel_case();
            let elem = map_elem_read(&n0, "i", inner);
            let dt = dart_type(inner);
            format!("{n0} == nullptr ? <{dt}>[] : List.generate({len}, (i) => {elem})")
        }
        TypeRef::Map(k, v) => {
            let keys = p.abi[0].name.to_lower_camel_case();
            let vals = p.abi[1].name.to_lower_camel_case();
            let len = p.abi[2].name.to_lower_camel_case();
            let kexpr = map_elem_read(&keys, "i", k);
            let vexpr = map_elem_read(&vals, "i", v);
            let (kt, vt) = (dart_type(k), dart_type(v));
            format!(
                "{keys} == nullptr ? <{kt}, {vt}>{{}} : {{ for (var i = 0; i < {len}; i++) {kexpr}: {vexpr} }}"
            )
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
    }
}

/// The register/unregister wrapper pair for one listener. The trampoline is an
/// `isolateLocal` NativeCallable: WeaveFFI listeners fire synchronously on the
/// thread calling the producer API, so arguments are converted inside the
/// borrow window (a `.listener` callable would read freed pointers later).
fn render_listener(out: &mut String, m: &ModuleBinding, l: &ListenerBinding) {
    let Some(cb) = m.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let cb_typedef = format!("_NativeCb_{}", cb.c_fn_type);
    let register_name = format!("register_{}", l.name).to_lower_camel_case();
    let unregister_name = format!("unregister_{}", l.name).to_lower_camel_case();

    emit_typedef_and_lookup(
        out,
        &l.register_symbol,
        &format!("Pointer<NativeFunction<{cb_typedef}>>, Pointer<Void>"),
        &format!("Pointer<NativeFunction<{cb_typedef}>>, Pointer<Void>"),
        "Uint64",
        "int",
    );
    emit_typedef_and_lookup(out, &l.unregister_symbol, "Uint64", "int", "Void", "void");

    let user_fn_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{} {}", dart_type(&p.ty), p.name.to_lower_camel_case()))
        .collect();
    let mut tramp_decls: Vec<String> = Vec::new();
    for p in &cb.params {
        for ((_, d), slot) in input_slots(&p.ty).iter().zip(p.abi.iter()) {
            tramp_decls.push(format!("{d} {}", slot.name.to_lower_camel_case()));
        }
    }
    tramp_decls.push("Pointer<Void> context".into());
    let call_args: Vec<String> = cb.params.iter().map(cb_arg_expr).collect();

    out.push('\n');
    emit_doc(out, &l.doc, "");
    out.push_str(&format!(
        "/// Registers a {} listener. Returns a subscription id for {unregister_name}().\n",
        cb.name
    ));
    out.push_str(&format!(
        "int {register_name}(void Function({}) callback) {{\n",
        user_fn_params.join(", ")
    ));
    out.push_str(&format!(
        "  final callable = NativeCallable<{cb_typedef}>.isolateLocal(({}) {{\n",
        tramp_decls.join(", ")
    ));
    out.push_str(&format!("    callback({});\n", call_args.join(", ")));
    out.push_str("  });\n");
    out.push_str(&format!(
        "  final id = _{}(callable.nativeFunction, nullptr);\n",
        l.register_symbol.to_lower_camel_case()
    ));
    out.push_str("  _listenerCallables[id] = callable;\n");
    out.push_str("  return id;\n");
    out.push_str("}\n");

    out.push('\n');
    out.push_str(&format!(
        "/// Unregisters a listener previously registered with {register_name}().\n"
    ));
    out.push_str(&format!("void {unregister_name}(int id) {{\n"));
    out.push_str(&format!(
        "  _{}(id);\n",
        l.unregister_symbol.to_lower_camel_case()
    ));
    out.push_str("  _listenerCallables.remove(id)?.close();\n");
    out.push_str("}\n");
}

/// Returns the (native, dart) FFI types of the trailing callback parameters
/// (those after `(context, err)`) for an async function with the given return
/// type. The empty vec means the callback signature is `(context, err)` with
/// no extra payload.
fn async_cb_extra_params(ret: Option<&TypeRef>) -> Vec<(&'static str, &'static str)> {
    match ret {
        None => vec![],
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            vec![("Pointer<Uint8>", "Pointer<Uint8>"), ("Size", "int")]
        }
        Some(TypeRef::List(_) | TypeRef::Iterator(_)) => {
            vec![("Pointer<Void>", "Pointer<Void>"), ("Size", "int")]
        }
        Some(TypeRef::Map(_, _)) => vec![
            ("Pointer<Void>", "Pointer<Void>"),
            ("Pointer<Void>", "Pointer<Void>"),
            ("Size", "int"),
        ],
        Some(t) => vec![{
            let n: &'static str = match t {
                TypeRef::I32 | TypeRef::U32 | TypeRef::Bool | TypeRef::Enum(_) => "Int32",
                TypeRef::I64 | TypeRef::Handle | TypeRef::TypedHandle(_) => "Int64",
                TypeRef::F64 => "Double",
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>",
                TypeRef::Struct(_) => "Pointer<Void>",
                _ => "Pointer<Void>",
            };
            let d: &'static str = match t {
                TypeRef::I32
                | TypeRef::U32
                | TypeRef::I64
                | TypeRef::Bool
                | TypeRef::Enum(_)
                | TypeRef::Handle
                | TypeRef::TypedHandle(_) => "int",
                TypeRef::F64 => "double",
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "Pointer<Utf8>",
                TypeRef::Struct(_) => "Pointer<Void>",
                _ => "Pointer<Void>",
            };
            (n, d)
        }],
    }
}

fn render_async_function(
    out: &mut String,
    c_sym: &str,
    f: &FnBinding,
    wrapper_name: &str,
    pub_ret: &str,
    wrapper_params: &[String],
) {
    let cb_extras = async_cb_extra_params(f.ret.as_ref());
    let cb_native_params: Vec<String> = std::iter::once("Pointer<Void>".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError>".to_string()))
        .chain(cb_extras.iter().map(|(n, _)| (*n).to_string()))
        .collect();

    let cb_typedef = format!("_NativeAsyncCb_{c_sym}");
    out.push_str(&format!(
        "\ntypedef {cb_typedef} = Void Function({});\n",
        cb_native_params.join(", ")
    ));

    let async_sym = format!("{c_sym}_async");
    let mut native_params: Vec<String> = f.params.iter().map(|p| native_ffi_type(&p.ty)).collect();
    if f.cancellable {
        native_params.push("Pointer<Void>".into());
    }
    native_params.push(format!("Pointer<NativeFunction<{cb_typedef}>>"));
    native_params.push("Pointer<Void>".into());
    let mut dart_params: Vec<String> = f.params.iter().map(|p| dart_ffi_type(&p.ty)).collect();
    if f.cancellable {
        dart_params.push("Pointer<Void>".into());
    }
    dart_params.push(format!("Pointer<NativeFunction<{cb_typedef}>>"));
    dart_params.push("Pointer<Void>".into());

    emit_typedef_and_lookup(
        out,
        &async_sym,
        &native_params.join(", "),
        &dart_params.join(", "),
        "Void",
        "void",
    );

    out.push('\n');
    emit_doc(out, &f.doc, "");
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('\'', "\\'");
        out.push_str(&format!("@Deprecated('{escaped}')\n"));
    }
    out.push_str(&format!(
        "Future<{pub_ret}> {wrapper_name}({}) {{\n",
        wrapper_params.join(", ")
    ));

    let completer_type = if f.ret.is_some() {
        pub_ret.to_string()
    } else {
        "void".to_string()
    };
    out.push_str(&format!(
        "  final completer = Completer<{completer_type}>();\n"
    ));

    let mut native_strings = Vec::new();
    for p in &f.params {
        if matches!(p.ty, TypeRef::StringUtf8 | TypeRef::BorrowedStr) {
            let pname = p.name.to_lower_camel_case();
            let ptr = format!("{pname}Ptr");
            out.push_str(&format!("  final {ptr} = {pname}.toNativeUtf8();\n"));
            native_strings.push(ptr);
        }
    }

    let cb_dart_params: Vec<String> = std::iter::once("Pointer<Void>".to_string())
        .chain(std::iter::once("Pointer<_WeaveFFIError>".to_string()))
        .chain(cb_extras.iter().map(|(_, d)| (*d).to_string()))
        .collect();
    let cb_arg_names: Vec<String> = (0..cb_dart_params.len())
        .map(|i| match i {
            0 => "context".to_string(),
            1 => "err".to_string(),
            2 => "result".to_string(),
            3 => "resultLen".to_string(),
            4 => "resultLenExtra".to_string(),
            _ => format!("arg{i}"),
        })
        .collect();
    let cb_param_decls: Vec<String> = cb_dart_params
        .iter()
        .zip(cb_arg_names.iter())
        .map(|(t, n)| format!("{t} {n}"))
        .collect();

    out.push_str(&format!("  late NativeCallable<{cb_typedef}> callable;\n"));
    out.push_str(&format!(
        "  callable = NativeCallable<{cb_typedef}>.listener(({}) {{\n",
        cb_param_decls.join(", ")
    ));
    out.push_str("    try {\n");
    out.push_str("      if (err.address != 0 && err.ref.code != 0) {\n");
    out.push_str("        final code = err.ref.code;\n");
    out.push_str("        final msg = err.ref.message.toDartString();\n");
    out.push_str("        _weaveffiErrorClear(err);\n");
    out.push_str("        completer.completeError(WeaveFFIException(code, msg));\n");
    out.push_str("        return;\n");
    out.push_str("      }\n");
    emit_async_complete(out, f.ret.as_ref(), "      ");
    out.push_str("    } catch (e) {\n");
    out.push_str("      completer.completeError(e);\n");
    out.push_str("    } finally {\n");
    out.push_str("      callable.close();\n");
    out.push_str("    }\n");
    out.push_str("  });\n");

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
    if f.cancellable {
        call_args.push("nullptr".into());
    }
    call_args.push("callable.nativeFunction".into());
    call_args.push("nullptr".into());

    let var = async_sym.to_lower_camel_case();
    out.push_str("  try {\n");
    out.push_str(&format!("    _{var}({});\n", call_args.join(", ")));
    out.push_str("  } catch (e) {\n");
    out.push_str("    callable.close();\n");
    for ns in &native_strings {
        out.push_str(&format!("    calloc.free({ns});\n"));
    }
    out.push_str("    rethrow;\n");
    out.push_str("  }\n");
    if native_strings.is_empty() {
        out.push_str("  return completer.future;\n");
    } else {
        out.push_str("  return completer.future.whenComplete(() {\n");
        for ns in &native_strings {
            out.push_str(&format!("    calloc.free({ns});\n"));
        }
        out.push_str("  });\n");
    }
    out.push_str("}\n");
}

fn emit_async_complete(out: &mut String, ty: Option<&TypeRef>, indent: &str) {
    match ty {
        None => {
            out.push_str(&format!("{indent}completer.complete();\n"));
        }
        Some(TypeRef::Bool) => {
            out.push_str(&format!("{indent}completer.complete(result != 0);\n"));
        }
        Some(TypeRef::Enum(name)) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!(
                "{indent}completer.complete({n}.fromValue(result));\n"
            ));
        }
        Some(TypeRef::Struct(name)) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!("{indent}completer.complete({n}._(result));\n"));
        }
        Some(TypeRef::TypedHandle(name)) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!("{indent}completer.complete({n}._(result));\n"));
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            out.push_str(&format!(
                "{indent}completer.complete(result.toDartString());\n"
            ));
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!("{indent}if (result == nullptr) {{\n"));
                out.push_str(&format!("{indent}  completer.complete(null);\n"));
                out.push_str(&format!("{indent}}} else {{\n"));
                out.push_str(&format!(
                    "{indent}  completer.complete(result.toDartString());\n"
                ));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::Struct(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                out.push_str(&format!("{indent}if (result == nullptr) {{\n"));
                out.push_str(&format!("{indent}  completer.complete(null);\n"));
                out.push_str(&format!("{indent}}} else {{\n"));
                out.push_str(&format!("{indent}  completer.complete({n}._(result));\n"));
                out.push_str(&format!("{indent}}}\n"));
            }
            TypeRef::TypedHandle(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                out.push_str(&format!("{indent}if (result == nullptr) {{\n"));
                out.push_str(&format!("{indent}  completer.complete(null);\n"));
                out.push_str(&format!("{indent}}} else {{\n"));
                out.push_str(&format!("{indent}  completer.complete({n}._(result));\n"));
                out.push_str(&format!("{indent}}}\n"));
            }
            _ => {
                out.push_str(&format!("{indent}completer.complete(result);\n"));
            }
        },
        Some(_) => {
            out.push_str(&format!("{indent}completer.complete(result);\n"));
        }
    }
}

fn emit_function_body(out: &mut String, f: &FnBinding, c_sym: &str) {
    if let CallShape::Iterator(ib) = &f.shape {
        emit_iterator_body(out, f, c_sym, ib);
        return;
    }

    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        let args = emit_input(out, &p.name.to_lower_camel_case(), &p.ty, &mut frees);
        call_args.extend(args);
    }
    if let Some(ret) = &f.ret {
        call_args.extend(emit_return_alloc(out, ret, &mut frees, "  "));
    }
    out.push_str("  final err = calloc<_WeaveFFIError>();\n");
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    out.push_str("  try {\n");
    let var = c_sym.to_lower_camel_case();
    let args = call_args.join(", ");
    // A map return is a `void` symbol whose results land in the out-params.
    let void_call = f.ret.is_none() || matches!(&f.ret, Some(TypeRef::Map(_, _)));
    if void_call {
        out.push_str(&format!("    _{var}({args});\n"));
    } else {
        out.push_str(&format!("    final result = _{var}({args});\n"));
    }
    out.push_str("    _checkError(err);\n");
    if let Some(ret) = &f.ret {
        emit_return_decode(out, ret, "    ");
    }
    out.push_str("  } finally {\n");
    for fr in &frees {
        out.push_str(&format!("    {fr}\n"));
    }
    out.push_str("  }\n");
}

/// Bind the element `next`/`destroy` symbols of an iterator-returning function.
fn emit_iter_lookups(out: &mut String, ib: &IteratorBinding) {
    let item = input_array_ffi(&ib.elem);
    emit_typedef_and_lookup(
        out,
        &ib.next.symbol,
        &format!("Pointer<Void>, {item}, Pointer<_WeaveFFIError>"),
        &format!("Pointer<Void>, {item}, Pointer<_WeaveFFIError>"),
        "Int32",
        "int",
    );
    emit_typedef_and_lookup(
        out,
        &ib.destroy_symbol,
        "Pointer<Void>",
        "Pointer<Void>",
        "Void",
        "void",
    );
}

/// Launch the iterator, then drain it via `next`/`destroy` into a `List<T>`.
fn emit_iterator_body(out: &mut String, f: &FnBinding, c_sym: &str, ib: &IteratorBinding) {
    let mut frees: Vec<String> = Vec::new();
    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        let args = emit_input(out, &p.name.to_lower_camel_case(), &p.ty, &mut frees);
        call_args.extend(args);
    }
    out.push_str("  final err = calloc<_WeaveFFIError>();\n");
    frees.push("calloc.free(err);".into());
    call_args.push("err".into());

    out.push_str("  try {\n");
    let var = c_sym.to_lower_camel_case();
    out.push_str(&format!(
        "    final iter = _{var}({});\n",
        call_args.join(", ")
    ));
    out.push_str("    _checkError(err);\n");
    let elem = &ib.elem;
    let dt = dart_type(elem);
    out.push_str(&format!("    final items = <{dt}>[];\n"));
    out.push_str(&format!(
        "    final outItem = calloc<{}>();\n",
        iter_item_pointee(elem)
    ));
    out.push_str(&format!(
        "    while (_{}(iter, outItem, err) != 0) {{\n",
        ib.next.symbol.to_lower_camel_case()
    ));
    out.push_str("      _checkError(err);\n");
    out.push_str(&format!(
        "      items.add({});\n",
        read_value("outItem.value", elem)
    ));
    out.push_str("    }\n");
    out.push_str("    _checkError(err);\n");
    out.push_str("    calloc.free(outItem);\n");
    out.push_str(&format!(
        "    _{}(iter);\n",
        ib.destroy_symbol.to_lower_camel_case()
    ));
    out.push_str("    return items;\n");
    out.push_str("  } finally {\n");
    for fr in &frees {
        out.push_str(&format!("    {fr}\n"));
    }
    out.push_str("  }\n");
}

/// Materialises a `T**` + `out_len` C return into a Dart `List<T>`. The array
/// buffer is owned by the callee per the WeaveFFI ABI; element ownership (e.g.
/// struct handles) transfers to the caller, who disposes each element.
fn emit_list_conversion(out: &mut String, inner: &TypeRef, indent: &str) {
    out.push_str(&format!("{indent}final n = outLen.value;\n"));
    let dt = dart_type(inner);
    out.push_str(&format!(
        "{indent}if (result == nullptr || n == 0) return <{dt}>[];\n"
    ));
    match inner {
        TypeRef::Struct(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!(
                "{indent}final arr = result.cast<Pointer<Void>>();\n"
            ));
            out.push_str(&format!(
                "{indent}return List<{n}>.generate(n, (i) => {n}._(arr[i]));\n"
            ));
        }
        TypeRef::TypedHandle(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!(
                "{indent}final arr = result.cast<Pointer<Void>>();\n"
            ));
            out.push_str(&format!(
                "{indent}return List<{n}>.generate(n, (i) => {n}._(arr[i]));\n"
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{indent}final arr = result.cast<Pointer<Utf8>>();\n"
            ));
            out.push_str(&format!(
                "{indent}return List<String>.generate(n, (i) => arr[i].toDartString());\n"
            ));
        }
        TypeRef::I64 | TypeRef::Handle => {
            out.push_str(&format!("{indent}final arr = result.cast<Int64>();\n"));
            out.push_str(&format!(
                "{indent}return List<int>.generate(n, (i) => arr[i]);\n"
            ));
        }
        TypeRef::F64 => {
            out.push_str(&format!("{indent}final arr = result.cast<Double>();\n"));
            out.push_str(&format!(
                "{indent}return List<double>.generate(n, (i) => arr[i]);\n"
            ));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{indent}final arr = result.cast<Int32>();\n"));
            out.push_str(&format!(
                "{indent}return List<bool>.generate(n, (i) => arr[i] != 0);\n"
            ));
        }
        TypeRef::Enum(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!("{indent}final arr = result.cast<Int32>();\n"));
            out.push_str(&format!(
                "{indent}return List<{n}>.generate(n, (i) => {n}.fromValue(arr[i]));\n"
            ));
        }
        _ => {
            // I32/U32 and any other word-sized element.
            out.push_str(&format!("{indent}final arr = result.cast<Int32>();\n"));
            out.push_str(&format!(
                "{indent}return List<int>.generate(n, (i) => arr[i]);\n"
            ));
        }
    }
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
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!("{indent}return {n}.fromValue(result);\n"));
        }
        TypeRef::Struct(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!("{indent}return {n}._(result);\n"));
        }
        TypeRef::TypedHandle(name) => {
            let n = local_type_name(name).to_upper_camel_case();
            out.push_str(&format!("{indent}return {n}._(result);\n"));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}return result.toDartString();\n"));
            }
            TypeRef::Struct(name) | TypeRef::TypedHandle(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}return {n}._(result);\n"));
            }
            // Optional scalars/bools/enums lower to a nullable pointer-to-scalar.
            TypeRef::Bool => {
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}return result.value != 0;\n"));
            }
            TypeRef::Enum(name) => {
                let n = local_type_name(name).to_upper_camel_case();
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}return {n}.fromValue(result.value);\n"));
            }
            _ => {
                out.push_str(&format!("{indent}if (result == nullptr) return null;\n"));
                out.push_str(&format!("{indent}return result.value;\n"));
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
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
            modules,
            generators: None,
            package: None,
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
        assert_eq!(Generator::name(&DartGenerator), "dart");
    }

    #[test]
    fn output_files_lists_dart_file() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = DartGenerator.output_files(&api, out, &DartConfig::default());
        assert_eq!(
            files,
            vec![
                format!("{out}/dart/README.md"),
                format!("{out}/dart/lib/weaveffi.dart"),
                format!("{out}/dart/pubspec.yaml"),
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
                    doc: None,
                },
                Param {
                    name: "b".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
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

        DartGenerator
            .generate(&api, out_dir, &DartConfig::default())
            .unwrap();

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
            dart.contains("_WeaveFFIError extends Struct"),
            "missing error struct: {dart}"
        );
        assert!(
            dart.contains("class WeaveFFIException"),
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
            dart.contains("calloc<_WeaveFFIError>()"),
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("class PointBuilder {"),
            "builder class: {dart}"
        );
        assert!(
            dart.contains("PointBuilder withX(double value)"),
            "fluent setter: {dart}"
        );
        assert!(dart.contains("Point build() {"), "build method: {dart}");
        assert!(
            dart.contains("_checkError(err);") && dart.contains("return Point._(result);"),
            "build calls FFI create and wraps the result: {dart}"
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
                    doc: None,
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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Green".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Blue".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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
                    doc: None,
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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
                doc: None,
            }],
            returns: Some(TypeRef::Bool),
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("import 'dart:async'"),
            "missing dart:async import: {dart}"
        );
        assert!(
            dart.contains("Future<String> fetchData(int id)"),
            "missing async wrapper: {dart}"
        );
        assert!(
            dart.contains("NativeCallable<_NativeAsyncCb_weaveffi_math_fetch_data>.listener"),
            "missing NativeCallable.listener: {dart}"
        );
        assert!(
            dart.contains("weaveffi_math_fetch_data_async"),
            "must call the _async C symbol: {dart}"
        );
    }

    /// `NativeCallable.listener` allocates a native trampoline that pins the
    /// Dart closure across the C boundary. It must be matched by exactly one
    /// `callable.close()` on every exit path so the trampoline is freed when
    /// the future resolves.
    #[test]
    fn dart_async_pins_callback_for_lifetime() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "fetch_data".into(),
            params: vec![Param {
                name: "id".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns: Some(TypeRef::StringUtf8),
            doc: None,
            r#async: true,
            cancellable: false,
            deprecated: None,
            since: None,
        }])]);
        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
        let pin_count = dart.matches(".listener(").count();
        let unpin_count = dart.matches("callable.close()").count();
        assert_eq!(
            pin_count, 1,
            "expected one NativeCallable.listener per async fn, got {pin_count}: {dart}"
        );
        // Two close sites per fn: callback finally, and try/catch around _ffiCall.
        assert_eq!(
            unpin_count, 2,
            "expected callable.close() in callback finally and synchronous catch (2 total), got {unpin_count}: {dart}"
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
                    doc: None,
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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

        DartGenerator
            .generate(&api, out_dir, &DartConfig::default())
            .unwrap();

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
                    doc: None,
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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
                    doc: None,
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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
                        doc: None,
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
                        doc: None,
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");
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
                            doc: None,
                        },
                        Param {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            mutable: false,
                            doc: None,
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
                        doc: None,
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
                        doc: None,
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
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Work".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Other".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

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

        let config = DartConfig {
            package_name: Some("my_custom_dart".into()),
            ..DartConfig::default()
        };
        DartGenerator.generate(&api, out_dir, &config).unwrap();

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
                    doc: None,
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

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
                    doc: None,
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

        let dart = render_dart_module(&api, "weaveffi", "weaveffi.yml");

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

    fn doc_api() -> Api {
        make_api(vec![Module {
            name: "docs".into(),
            functions: vec![Function {
                name: "do_thing".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: Some("Performs a thing.".into()),
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: Some("An item we track.".into()),
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: Some("Stable id".into()),
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Kind".into(),
                doc: Some("Kind of item.".into()),
                variants: vec![EnumVariant {
                    name: "Small".into(),
                    value: 0,
                    doc: Some("A small one".into()),
                    fields: vec![],
                }],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn dart_emits_doc_on_function() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// Performs a thing."), "{dart}");
    }

    #[test]
    fn dart_emits_doc_on_struct() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// An item we track."), "{dart}");
    }

    #[test]
    fn dart_emits_doc_on_enum_variant() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// Kind of item."), "{dart}");
        assert!(dart.contains("/// A small one"), "{dart}");
    }

    #[test]
    fn dart_emits_doc_on_field() {
        let dart = render_dart_module(&doc_api(), "weaveffi", "weaveffi.yml");
        assert!(dart.contains("/// Stable id"), "{dart}");
    }

    /// A rich (algebraic) enum mirroring `samples/shapes`: a unit variant, an
    /// f64 payload, two f32 payloads, and a (string, u8) payload, plus a plain
    /// sibling enum and functions that take/return the rich enum by handle.
    fn rich_enum_api() -> Api {
        make_api(vec![Module {
            name: "shapes".into(),
            functions: vec![
                Function {
                    name: "describe".into(),
                    params: vec![Param {
                        name: "shape".into(),
                        ty: TypeRef::Struct("Shape".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "scale".into(),
                    params: vec![
                        Param {
                            name: "shape".into(),
                            ty: TypeRef::Struct("Shape".into()),
                            mutable: false,
                            doc: None,
                        },
                        Param {
                            name: "factor".into(),
                            ty: TypeRef::F64,
                            mutable: false,
                            doc: None,
                        },
                    ],
                    returns: Some(TypeRef::Struct("Shape".into())),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![
                EnumDef {
                    name: "Shape".into(),
                    doc: Some("An algebraic shape".into()),
                    variants: vec![
                        EnumVariant {
                            name: "Empty".into(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Circle".into(),
                            value: 1,
                            doc: None,
                            fields: vec![StructField {
                                name: "radius".into(),
                                ty: TypeRef::F64,
                                doc: Some("Radius in points".into()),
                                default: None,
                            }],
                        },
                        EnumVariant {
                            name: "Rectangle".into(),
                            value: 2,
                            doc: None,
                            fields: vec![
                                StructField {
                                    name: "width".into(),
                                    ty: TypeRef::F32,
                                    doc: None,
                                    default: None,
                                },
                                StructField {
                                    name: "height".into(),
                                    ty: TypeRef::F32,
                                    doc: None,
                                    default: None,
                                },
                            ],
                        },
                        EnumVariant {
                            name: "Labeled".into(),
                            value: 3,
                            doc: None,
                            fields: vec![
                                StructField {
                                    name: "label".into(),
                                    ty: TypeRef::StringUtf8,
                                    doc: None,
                                    default: None,
                                },
                                StructField {
                                    name: "count".into(),
                                    ty: TypeRef::U8,
                                    doc: None,
                                    default: None,
                                },
                            ],
                        },
                    ],
                },
                EnumDef {
                    name: "Channel".into(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Red".into(),
                            value: 0,
                            doc: None,
                            fields: vec![],
                        },
                        EnumVariant {
                            name: "Green".into(),
                            value: 1,
                            doc: None,
                            fields: vec![],
                        },
                    ],
                },
            ],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }])
    }

    #[test]
    fn rich_enum_is_opaque_class_not_plain_enum() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        // The rich enum must NOT be a plain Dart `enum`...
        assert!(
            !dart.contains("enum Shape {"),
            "rich enum must not render as a plain enum: {dart}"
        );
        // ...but an opaque-object wrapper class, exactly like a struct.
        assert!(
            dart.contains("class Shape {"),
            "missing Shape class: {dart}"
        );
        assert!(
            dart.contains("final Pointer<Void> _handle;"),
            "missing opaque handle field: {dart}"
        );
        assert!(
            dart.contains("Shape._(this._handle);"),
            "missing private wrapping constructor: {dart}"
        );
        assert!(
            dart.contains("void dispose()") && dart.contains("weaveffi_shapes_Shape_destroy"),
            "missing dispose()/destroy symbol: {dart}"
        );
        // A plain sibling enum still renders as a plain Dart enum.
        assert!(
            dart.contains("enum Channel {"),
            "plain sibling enum should still render as an enum: {dart}"
        );
    }

    #[test]
    fn rich_enum_tag_reader() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("enum ShapeTag {"),
            "missing typed discriminant enum: {dart}"
        );
        assert!(
            dart.contains("empty(0)")
                && dart.contains("circle(1)")
                && dart.contains("rectangle(2)")
                && dart.contains("labeled(3)"),
            "missing tag discriminants: {dart}"
        );
        assert!(
            dart.contains("ShapeTag get tag"),
            "missing tag discriminant reader: {dart}"
        );
        assert!(
            dart.contains("ShapeTag.fromValue(_weaveffiShapesShapeTag(_handle))"),
            "tag reader must read the C tag symbol: {dart}"
        );
    }

    #[test]
    fn rich_enum_per_variant_factories() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        assert!(
            dart.contains("factory Shape.empty()"),
            "missing unit-variant factory: {dart}"
        );
        assert!(
            dart.contains("factory Shape.circle(double radius)"),
            "missing f64 factory: {dart}"
        );
        assert!(
            dart.contains("factory Shape.rectangle(double width, double height)"),
            "missing two-f32 factory: {dart}"
        );
        assert!(
            dart.contains("factory Shape.labeled(String label, int count)"),
            "missing (string,u8) factory: {dart}"
        );
        // Each factory binds its own per-variant `_new` symbol...
        assert!(
            dart.contains("weaveffi_shapes_Shape_Empty_new")
                && dart.contains("weaveffi_shapes_Shape_Circle_new")
                && dart.contains("weaveffi_shapes_Shape_Rectangle_new")
                && dart.contains("weaveffi_shapes_Shape_Labeled_new"),
            "missing per-variant constructor symbols: {dart}"
        );
        // ...marshals string fields, checks the error, and wraps the handle.
        assert!(
            dart.contains("label.toNativeUtf8()"),
            "labeled factory must marshal its string field: {dart}"
        );
        assert!(
            dart.contains("_checkError(err);") && dart.contains("return Shape._(result);"),
            "factory must check error and wrap the returned handle: {dart}"
        );
    }

    #[test]
    fn rich_enum_per_variant_getters_namespaced() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        // Getters are namespaced by variant to avoid collisions; numerics map to
        // int/double (incl. f32 -> double, u8 -> int) and strings decode.
        assert!(
            dart.contains("double get circleRadius"),
            "missing circleRadius getter: {dart}"
        );
        assert!(
            dart.contains("double get rectangleWidth"),
            "missing rectangleWidth getter: {dart}"
        );
        assert!(
            dart.contains("double get rectangleHeight"),
            "missing rectangleHeight getter: {dart}"
        );
        assert!(
            dart.contains("String get labeledLabel"),
            "missing labeledLabel getter: {dart}"
        );
        assert!(
            dart.contains("int get labeledCount"),
            "missing labeledCount getter: {dart}"
        );
        // Getters bind their per-variant C symbols and the string getter decodes.
        assert!(
            dart.contains("weaveffi_shapes_Shape_Circle_get_radius")
                && dart.contains("weaveffi_shapes_Shape_Labeled_get_label"),
            "missing per-variant getter symbols: {dart}"
        );
        assert!(
            dart.contains("return result.toDartString();"),
            "string getter must decode the C string: {dart}"
        );
        // Carries the per-variant field doc through the namespaced getter.
        assert!(
            dart.contains("/// Radius in points"),
            "variant field doc should be emitted: {dart}"
        );
    }

    #[test]
    fn rich_enum_functions_marshal_opaque_handle() {
        let dart = render_dart_module(&rich_enum_api(), "weaveffi", "weaveffi.yml");
        // Functions taking/returning the rich enum lower it to TypeRef::Struct,
        // so they pass the opaque handle in and wrap the handle out unchanged.
        assert!(
            dart.contains("String describe(Shape shape)"),
            "missing describe signature: {dart}"
        );
        assert!(
            dart.contains("Shape scale(Shape shape, double factor)"),
            "missing scale signature: {dart}"
        );
        assert!(
            dart.contains("shape._handle"),
            "a rich-enum argument must marshal as its opaque handle: {dart}"
        );
        assert!(
            dart.contains("return Shape._(result);"),
            "a rich-enum return must wrap the opaque handle: {dart}"
        );
    }
}
