use anyhow::Result;
use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_symbol_name, local_type_name, wrapper_name};
use weaveffi_ir::ir::{Api, EnumDef, Function, Module, Param, StructDef, StructField, TypeRef};

pub struct SwiftGenerator;

impl SwiftGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        module_name: &str,
        strip_module_prefix: bool,
    ) -> Result<()> {
        let dir = out_dir.join("swift");
        let c_module = format!("C{}", module_name);
        let module_dir = dir.join(&c_module);
        std::fs::create_dir_all(&module_dir)?;

        let package = format!(
            r#"// swift-tools-version:5.7
import PackageDescription

let package = Package(
    name: "{name}",
    products: [
        .library(name: "{name}", targets: ["{name}"]),
    ],
    targets: [
        .systemLibrary(name: "{c_name}"),
        .target(name: "{name}", dependencies: ["{c_name}"]),
    ]
)
"#,
            name = module_name,
            c_name = c_module,
        );
        std::fs::write(dir.join("Package.swift"), package)?;

        let modulemap = format!(
            "module {} [system] {{\n  header \"../../c/weaveffi.h\"\n  link \"weaveffi\"\n  export *\n}}\n",
            c_module
        );
        std::fs::write(module_dir.join("module.modulemap"), modulemap)?;

        let src_dir = dir.join("Sources").join(module_name);
        std::fs::create_dir_all(&src_dir)?;
        std::fs::write(
            src_dir.join(format!("{}.swift", module_name)),
            render_swift_wrapper(api, strip_module_prefix),
        )?;
        Ok(())
    }
}

impl Generator for SwiftGenerator {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "WeaveFFI", true)
    }

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate_impl(
            api,
            out_dir,
            config.swift_module_name(),
            config.strip_module_prefix,
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        let module_name = "WeaveFFI";
        let c_module = format!("C{module_name}");
        vec![
            out_dir.join("swift/Package.swift").to_string(),
            out_dir
                .join(format!("swift/{c_module}/module.modulemap"))
                .to_string(),
            out_dir
                .join(format!("swift/Sources/{module_name}/{module_name}.swift"))
                .to_string(),
        ]
    }
}

fn swift_type_for(t: &TypeRef) -> String {
    match t {
        TypeRef::I32 => "Int32".to_string(),
        TypeRef::U32 => "UInt32".to_string(),
        TypeRef::I64 => "Int64".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Bool".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "String".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "Data".to_string(),
        TypeRef::Handle => "UInt64".to_string(),
        TypeRef::TypedHandle(name) | TypeRef::Enum(name) => name.clone(),
        TypeRef::Struct(name) => local_type_name(name).to_string(),
        TypeRef::Optional(inner) => format!("{}?", swift_type_for(inner)),
        TypeRef::List(inner) => format!("[{}]", swift_type_for(inner)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_for(k), swift_type_for(v)),
        TypeRef::Iterator(inner) => format!("[{}]", swift_type_for(inner)),
    }
}

fn is_c_value_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I32
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_)
    )
}

fn needs_closure(ty: &TypeRef) -> bool {
    match ty {
        TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Bytes
        | TypeRef::BorrowedBytes
        | TypeRef::List(_)
        | TypeRef::Map(_, _) => true,
        TypeRef::Optional(inner) => is_c_value_type(inner),
        _ => false,
    }
}

fn has_buffer_params(params: &[Param]) -> bool {
    params.iter().any(|p| needs_closure(&p.ty))
}

fn render_swift_enum(out: &mut String, e: &EnumDef) {
    out.push_str(&format!("public enum {}: Int32 {{\n", e.name));
    for v in &e.variants {
        out.push_str(&format!(
            "    case {} = {}\n",
            v.name.to_lower_camel_case(),
            v.value
        ));
    }
    out.push_str("}\n\n");
}

fn render_swift_wrapper(api: &Api, strip_module_prefix: bool) -> String {
    let mut out = String::new();
    out.push_str("import CWeaveFFI\nimport Foundation\n\n");

    let all_mods = collect_all_modules(&api.modules);
    let error_codes: Vec<_> = all_mods
        .iter()
        .filter_map(|m| m.errors.as_ref())
        .flat_map(|e| &e.codes)
        .collect();

    out.push_str("public enum WeaveFFIError: Error, LocalizedError {\n");
    out.push_str("    case error(code: Int32, message: String)\n");
    for ec in &error_codes {
        out.push_str(&format!("    case {}\n", ec.name.to_lower_camel_case()));
    }
    out.push_str("    public var errorDescription: String? {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(_, message): return message\n");
    for ec in &error_codes {
        out.push_str(&format!(
            "        case .{}: return \"{}\"\n",
            ec.name.to_lower_camel_case(),
            ec.message
        ));
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("    public var errorCode: Int32 {\n");
    out.push_str("        switch self {\n");
    out.push_str("        case let .error(code, _): return code\n");
    for ec in &error_codes {
        out.push_str(&format!(
            "        case .{}: return {}\n",
            ec.name.to_lower_camel_case(),
            ec.code
        ));
    }
    out.push_str("        }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc check(_ err: inout weaveffi_error) throws {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    if error_codes.is_empty() {
        out.push_str("        throw WeaveFFIError.error(code: code, message: message)\n");
    } else {
        out.push_str("        switch code {\n");
        for ec in &error_codes {
            out.push_str(&format!(
                "        case {}: throw WeaveFFIError.{}\n",
                ec.code,
                ec.name.to_lower_camel_case()
            ));
        }
        out.push_str("        default: throw WeaveFFIError.error(code: code, message: message)\n");
        out.push_str("        }\n");
    }
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc withOptionalPointer<T, R>(to value: T?, _ body: (UnsafePointer<T>?) throws -> R) rethrows -> R {\n");
    out.push_str("    guard let value = value else { return try body(nil) }\n");
    out.push_str("    return try withUnsafePointer(to: value) { try body($0) }\n");
    out.push_str("}\n\n");

    let has_async = all_mods
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async));
    if has_async {
        out.push_str("private final class ContinuationRef<T> {\n");
        out.push_str("    let value: CheckedContinuation<T, Error>\n");
        out.push_str("    init(_ value: CheckedContinuation<T, Error>) { self.value = value }\n");
        out.push_str("}\n\n");
    }

    for m in &api.modules {
        render_swift_module_types(&mut out, m, &m.name);
        let type_name = m.name.to_upper_camel_case();
        out.push_str(&format!("public enum {} {{\n", type_name));
        render_swift_module_body(&mut out, m, &m.name, 1, strip_module_prefix);
        out.push_str("}\n\n");
    }
    out
}

fn collect_all_modules(modules: &[Module]) -> Vec<&Module> {
    let mut all = Vec::new();
    for m in modules {
        all.push(m);
        all.extend(collect_all_modules(&m.modules));
    }
    all
}

fn render_swift_module_types(out: &mut String, m: &Module, module_path: &str) {
    for e in &m.enums {
        render_swift_enum(out, e);
    }
    for s in &m.structs {
        render_swift_struct(out, module_path, s);
        if s.builder {
            render_swift_builder(out, module_path, s);
        }
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        render_swift_module_types(out, sub, &sub_path);
    }
}

fn render_swift_module_body(
    out: &mut String,
    m: &Module,
    module_path: &str,
    depth: usize,
    strip_module_prefix: bool,
) {
    let indent = "    ".repeat(depth);
    for f in &m.functions {
        let mut buf = String::new();
        if f.r#async {
            render_swift_async_function(&mut buf, module_path, f, strip_module_prefix);
        } else {
            render_swift_function(&mut buf, module_path, f, strip_module_prefix);
        }
        if depth > 1 {
            let extra = "    ".repeat(depth - 1);
            for line in buf.lines() {
                if line.is_empty() {
                    out.push('\n');
                } else {
                    out.push_str(&extra);
                    out.push_str(line);
                    out.push('\n');
                }
            }
        } else {
            out.push_str(&buf);
        }
    }
    for sub in &m.modules {
        let sub_path = format!("{module_path}_{}", sub.name);
        let sub_name = sub.name.to_upper_camel_case();
        out.push_str(&format!("{indent}public enum {sub_name} {{\n"));
        render_swift_module_body(out, sub, &sub_path, depth + 1, strip_module_prefix);
        out.push_str(&format!("{indent}}}\n"));
    }
}

fn render_swift_struct(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);

    out.push_str(&format!("public class {} {{\n", s.name));
    out.push_str("    let ptr: OpaquePointer\n\n");
    out.push_str("    init(ptr: OpaquePointer) {\n");
    out.push_str("        self.ptr = ptr\n");
    out.push_str("    }\n\n");
    out.push_str(&format!(
        "    deinit {{\n        {}_destroy(ptr)\n    }}\n",
        prefix
    ));

    for field in &s.fields {
        render_swift_getter(out, &prefix, field);
    }

    out.push_str("}\n\n");
}

fn struct_fields_as_params(fields: &[StructField]) -> Vec<Param> {
    fields
        .iter()
        .map(|f| Param {
            name: f.name.clone(),
            ty: f.ty.clone(),
            mutable: false,
        })
        .collect()
}

fn render_swift_builder(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);
    let class_name = local_type_name(&s.name);
    let builder_name = format!("{class_name}Builder");

    out.push_str(&format!("public class {} {{\n", builder_name));
    for field in &s.fields {
        let swift_ty = swift_type_for(&field.ty);
        out.push_str(&format!("    private var _{}: {}?\n", field.name, swift_ty));
    }
    out.push_str("\n    public init() {}\n\n");

    for field in &s.fields {
        let pascal = field.name.to_upper_camel_case();
        let swift_ty = swift_type_for(&field.ty);
        out.push_str("    @discardableResult\n");
        out.push_str(&format!(
            "    public func with{}(_ value: {}) -> Self {{\n        self._{} = value\n        return self\n    }}\n\n",
            pascal, swift_ty, field.name
        ));
    }

    let params = struct_fields_as_params(&s.fields);
    out.push_str(&format!(
        "    public func build() throws -> {} {{\n",
        class_name
    ));
    for field in &s.fields {
        out.push_str(&format!(
            "        guard let {} = _{} else {{ fatalError(\"missing field: {}\") }}\n",
            field.name, field.name, field.name
        ));
    }
    out.push_str("        var err = weaveffi_error(code: 0, message: nil)\n");

    if !has_buffer_params(&params) {
        let create_sym = format!("{}_create", prefix);
        let call_args = build_c_call_args(&params, module_name);
        if call_args.is_empty() {
            out.push_str(&format!("        let ptr = {}(&err)\n", create_sym));
        } else {
            out.push_str(&format!(
                "        let ptr = {}({}, &err)\n",
                create_sym, call_args
            ));
        }
        out.push_str("        try check(&err)\n");
        out.push_str(
            "        guard let ptr = ptr else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n",
        );
        out.push_str(&format!("        return {}(ptr: ptr)\n", class_name));
    } else {
        render_buffered_struct_create(out, module_name, &prefix, &params, class_name);
    }

    out.push_str("    }\n}\n\n");
}

fn render_swift_getter(out: &mut String, prefix: &str, field: &StructField) {
    let getter = format!("{}_get_{}", prefix, field.name);
    let swift_ty = swift_type_for(&field.ty);

    out.push_str(&format!(
        "\n    public var {}: {} {{\n",
        field.name, swift_ty
    ));

    match &field.ty {
        TypeRef::StringUtf8 => {
            out.push_str(&format!("        let raw = {}(ptr)\n", getter));
            out.push_str("        guard let raw = raw else { return \"\" }\n");
            out.push_str("        defer { weaveffi_free_string(raw) }\n");
            out.push_str("        return String(cString: raw)\n");
        }
        TypeRef::Bytes => {
            out.push_str("        var outLen: Int = 0\n");
            out.push_str(&format!("        let raw = {}(ptr, &outLen)\n", getter));
            out.push_str("        guard let raw = raw else { return Data() }\n");
            out.push_str("        defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: raw), outLen) }\n");
            out.push_str("        return Data(bytes: raw, count: outLen)\n");
        }
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("        return {}(ptr: {}(ptr)!)\n", name, getter));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("        return {}(ptr: {}(ptr)!)\n", name, getter));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 => {
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str("        guard let p = p else { return nil }\n");
                out.push_str("        defer { weaveffi_free_string(p) }\n");
                out.push_str("        return String(cString: p)\n");
            }
            TypeRef::Bytes => {
                out.push_str("        var outLen: Int = 0\n");
                out.push_str(&format!("        let p = {}(ptr, &outLen)\n", getter));
                out.push_str("        guard let p = p else { return nil }\n");
                out.push_str("        defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: p), outLen) }\n");
                out.push_str("        return Data(bytes: p, count: outLen)\n");
            }
            TypeRef::Struct(name) => {
                let name = local_type_name(name);
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str(&format!("        return p.map {{ {}(ptr: $0) }}\n", name));
            }
            TypeRef::TypedHandle(name) => {
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str(&format!("        return p.map {{ {}(ptr: $0) }}\n", name));
            }
            TypeRef::Enum(name) => {
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str(&format!(
                    "        return p.map {{ {}(rawValue: $0.pointee.rawValue)! }}\n",
                    name
                ));
            }
            _ if is_c_value_type(inner) => {
                out.push_str(&format!("        let p = {}(ptr)\n", getter));
                out.push_str("        return p?.pointee\n");
            }
            _ => {
                out.push_str(&format!("        return {}(ptr)\n", getter));
            }
        },
        TypeRef::List(inner) => {
            out.push_str("        var outLen: Int = 0\n");
            out.push_str(&format!("        let rv = {}(ptr, &outLen)\n", getter));
            out.push_str("        guard let rv = rv else { return [] }\n");
            match inner.as_ref() {
                TypeRef::Enum(name) => {
                    out.push_str(&format!(
                        "        return (0..<outLen).map {{ {}(rawValue: rv[$0].rawValue)! }}\n",
                        name
                    ));
                }
                TypeRef::Struct(name) => {
                    let name = local_type_name(name);
                    out.push_str(&format!(
                        "        return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                        name
                    ));
                }
                TypeRef::TypedHandle(name) => {
                    out.push_str(&format!(
                        "        return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                        name
                    ));
                }
                _ => {
                    out.push_str(
                        "        return Array(UnsafeBufferPointer(start: rv, count: outLen))\n",
                    );
                }
            }
        }
        _ => {
            out.push_str(&format!("        return {}(ptr)\n", getter));
        }
    }

    out.push_str("    }\n");
}

fn render_swift_function(
    out: &mut String,
    module_name: &str,
    f: &Function,
    strip_module_prefix: bool,
) {
    if let Some(msg) = &f.deprecated {
        out.push_str(&format!(
            "    @available(*, deprecated, message: \"{}\")\n",
            msg.replace('"', "\\\"")
        ));
    }
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("_ {}: {}", p.name, swift_type_for(&p.ty)))
        .collect();
    let ret_swift = f
        .returns
        .as_ref()
        .map(swift_type_for)
        .unwrap_or_else(|| "Void".to_string());
    out.push_str(&format!(
        "    public static func {}({}) throws -> {} {{\n",
        func_name,
        params_sig.join(", "),
        ret_swift
    ));
    out.push_str("        var err = weaveffi_error(code: 0, message: nil)\n");

    let c_sym = c_symbol_name(module_name, &f.name);
    let call_args = build_c_call_args(&f.params, module_name);
    let call_with_err = if call_args.is_empty() {
        format!("{}(&err)", c_sym)
    } else {
        format!("{}({}, &err)", c_sym, call_args)
    };

    if !has_buffer_params(&f.params) {
        render_direct_call(out, f, &call_with_err, module_name);
    } else {
        render_buffered_call(out, f, &f.params, module_name);
    }

    out.push_str("    }\n");
}

fn render_swift_async_function(
    out: &mut String,
    module_name: &str,
    f: &Function,
    strip_module_prefix: bool,
) {
    if let Some(msg) = &f.deprecated {
        out.push_str(&format!(
            "    @available(*, deprecated, message: \"{}\")\n",
            msg.replace('"', "\\\"")
        ));
    }
    let func_name = wrapper_name(module_name, &f.name, strip_module_prefix);
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("_ {}: {}", p.name, swift_type_for(&p.ty)))
        .collect();
    let ret_swift = f
        .returns
        .as_ref()
        .map(swift_type_for)
        .unwrap_or_else(|| "Void".to_string());

    out.push_str(&format!(
        "    public static func {}({}) async throws -> {} {{\n",
        func_name,
        params_sig.join(", "),
        ret_swift
    ));
    out.push_str(&format!(
        "        try await withCheckedThrowingContinuation {{ (continuation: CheckedContinuation<{}, Error>) in\n",
        ret_swift
    ));
    out.push_str(
        "            let ctx = Unmanaged.passRetained(ContinuationRef(continuation)).toOpaque()\n",
    );

    let base = "            ";

    for p in &f.params {
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!(
                    "{}let {n}_bytes = Array({n}.utf8)\n",
                    base,
                    n = p.name
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("{}let {n}_bytes = Array({n})\n", base, n = p.name));
            }
            TypeRef::Optional(inner) => {
                if let TypeRef::Enum(enum_name) = inner.as_ref() {
                    out.push_str(&format!(
                        "{}let {n}_c: weaveffi_{m}_{e}? = {n}.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
            }
            TypeRef::List(inner) => match inner.as_ref() {
                TypeRef::Enum(enum_name) => {
                    out.push_str(&format!(
                        "{}let {n}_raw = {n}.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!(
                        "{}let {n}_ptrs = {n}.map {{ $0.ptr }}\n",
                        base,
                        n = p.name
                    ));
                }
                _ => {}
            },
            TypeRef::Map(k, v) => {
                out.push_str(&format!(
                    "{}let {n}_keys = Array({n}.keys)\n",
                    base,
                    n = p.name
                ));
                out.push_str(&format!(
                    "{}let {n}_values = {n}_keys.map {{ {n}[$0]! }}\n",
                    base,
                    n = p.name
                ));
                if let TypeRef::Enum(e) = k.as_ref() {
                    out.push_str(&format!(
                        "{}let {n}_keysRaw = {n}_keys.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = e
                    ));
                } else if matches!(k.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) {
                    out.push_str(&format!(
                        "{}let {n}_keysPtrs = {n}_keys.map {{ $0.ptr }}\n",
                        base,
                        n = p.name
                    ));
                }
                if let TypeRef::Enum(e) = v.as_ref() {
                    out.push_str(&format!(
                        "{}let {n}_valuesRaw = {n}_values.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        base,
                        n = p.name,
                        m = module_name,
                        e = e
                    ));
                } else if matches!(v.as_ref(), TypeRef::Struct(_) | TypeRef::TypedHandle(_)) {
                    out.push_str(&format!(
                        "{}let {n}_valuesPtrs = {n}_values.map {{ $0.ptr }}\n",
                        base,
                        n = p.name
                    ));
                }
            }
            _ => {}
        }
    }

    let closure_params: Vec<&Param> = f.params.iter().filter(|p| needs_closure(&p.ty)).collect();
    let mut closure_depth: usize = 0;

    for p in &closure_params {
        let indent = format!("{}{}", base, "    ".repeat(closure_depth));
        match &p.ty {
            TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes => {
                out.push_str(&format!(
                    "{}{}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress!\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner) if is_c_value_type(inner) => {
                let source = if matches!(inner.as_ref(), TypeRef::Enum(_)) {
                    format!("{}_c", p.name)
                } else {
                    p.name.clone()
                };
                out.push_str(&format!(
                    "{}withOptionalPointer(to: {}) {{ {}_ptr in\n",
                    indent, source, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::List(inner) => {
                let source = match inner.as_ref() {
                    TypeRef::Enum(_) => format!("{}_raw", p.name),
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("{}_ptrs", p.name),
                    _ => p.name.clone(),
                };
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_buf in\n",
                    indent, source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Map(k, v) => {
                let keys_source = map_array_source(k, &p.name, "keys");
                let values_source = map_array_source(v, &p.name, "values");
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                    indent, keys_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_keys_ptr = {}_keys_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
                let vind = format!("{}{}", base, "    ".repeat(closure_depth));
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_values_buf in\n",
                    vind, values_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_values_ptr = {}_values_buf.baseAddress\n",
                    vind, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_values_buf.count\n",
                    vind, p.name, p.name
                ));
                closure_depth += 1;
            }
            _ => unreachable!(),
        }
    }

    let inner_indent = format!("{}{}", base, "    ".repeat(closure_depth));
    let c_sym = format!("{}_async", c_symbol_name(module_name, &f.name));
    let call_args = build_c_call_args(&f.params, module_name);
    let cb_param_names = async_callback_param_names(&f.returns);

    if f.cancellable {
        if call_args.is_empty() {
            out.push_str(&format!(
                "{}{}(nil, {{ {} in\n",
                inner_indent, c_sym, cb_param_names
            ));
        } else {
            out.push_str(&format!(
                "{}{}({}, nil, {{ {} in\n",
                inner_indent, c_sym, call_args, cb_param_names
            ));
        }
    } else if call_args.is_empty() {
        out.push_str(&format!(
            "{}{}({{ {} in\n",
            inner_indent, c_sym, cb_param_names
        ));
    } else {
        out.push_str(&format!(
            "{}{}({}, {{ {} in\n",
            inner_indent, c_sym, call_args, cb_param_names
        ));
    }

    let cb_indent = format!("{}    ", inner_indent);
    out.push_str(&format!(
        "{}let contRef = Unmanaged<ContinuationRef<{}>>.fromOpaque(context!).takeRetainedValue()\n",
        cb_indent, ret_swift
    ));
    out.push_str(&format!(
        "{}if let err = err, err.pointee.code != 0 {{\n",
        cb_indent
    ));
    out.push_str(&format!("{}    let code = err.pointee.code\n", cb_indent));
    out.push_str(&format!(
        "{}    let msg = err.pointee.message.flatMap {{ String(cString: $0) }} ?? \"\"\n",
        cb_indent
    ));
    out.push_str(&format!(
        "{}    contRef.value.resume(throwing: WeaveFFIError.error(code: code, message: msg))\n",
        cb_indent
    ));
    out.push_str(&format!("{}}} else {{\n", cb_indent));

    let success_indent = format!("{}    ", cb_indent);
    render_async_resume_result(out, &f.returns, &success_indent, module_name, &f.name);

    out.push_str(&format!("{}}}\n", cb_indent));
    out.push_str(&format!("{}}}, ctx)\n", inner_indent));

    for i in (0..closure_depth).rev() {
        let indent = format!("{}{}", base, "    ".repeat(i));
        out.push_str(&format!("{}}}\n", indent));
    }

    out.push_str("        }\n");
    out.push_str("    }\n");
}

fn async_callback_param_names(returns: &Option<TypeRef>) -> &'static str {
    match returns {
        None => "context, err",
        Some(TypeRef::Bytes) | Some(TypeRef::BorrowedBytes) | Some(TypeRef::List(_)) => {
            "context, err, result, resultLen"
        }
        Some(TypeRef::Map(_, _)) => "context, err, resultKeys, resultValues, resultLen",
        Some(_) => "context, err, result",
    }
}

fn render_async_resume_result(
    out: &mut String,
    returns: &Option<TypeRef>,
    indent: &str,
    module_name: &str,
    func_name: &str,
) {
    match returns {
        None => {
            out.push_str(&format!("{}contRef.value.resume(returning: ())\n", indent));
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(throwing: WeaveFFIError.error(code: -1, message: \"null string\"))\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}let str = String(cString: result)\n", indent));
            out.push_str(&format!(
                "{}weaveffi_free_string(UnsafeMutablePointer(mutating: result))\n",
                indent
            ));
            out.push_str(&format!("{}contRef.value.resume(returning: str)\n", indent));
        }
        Some(TypeRef::Struct(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(throwing: WeaveFFIError.error(code: -1, message: \"null pointer\"))\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: {}(ptr: result))\n",
                indent, name
            ));
        }
        Some(TypeRef::TypedHandle(name)) => {
            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(throwing: WeaveFFIError.error(code: -1, message: \"null pointer\"))\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: {}(ptr: result))\n",
                indent, name
            ));
        }
        Some(TypeRef::Enum(name)) => {
            out.push_str(&format!(
                "{}contRef.value.resume(returning: {}(rawValue: result.rawValue)!)\n",
                indent, name
            ));
        }
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::StringUtf8 => {
                out.push_str(&format!("{}if let result = result {{\n", indent));
                out.push_str(&format!(
                    "{}    let str = String(cString: result)\n",
                    indent
                ));
                out.push_str(&format!(
                    "{}    weaveffi_free_string(UnsafeMutablePointer(mutating: result))\n",
                    indent
                ));
                out.push_str(&format!(
                    "{}    contRef.value.resume(returning: str)\n",
                    indent
                ));
                out.push_str(&format!("{}}} else {{\n", indent));
                out.push_str(&format!(
                    "{}    contRef.value.resume(returning: nil)\n",
                    indent
                ));
                out.push_str(&format!("{}}}\n", indent));
            }
            TypeRef::Struct(name) => {
                let name = local_type_name(name);
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result.map {{ {}(ptr: $0) }})\n",
                    indent, name
                ));
            }
            TypeRef::TypedHandle(name) => {
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result.map {{ {}(ptr: $0) }})\n",
                    indent, name
                ));
            }
            TypeRef::Enum(name) => {
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result.map {{ {}(rawValue: $0.pointee.rawValue)! }})\n",
                    indent, name
                ));
            }
            _ if is_c_value_type(inner) => {
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result?.pointee)\n",
                    indent
                ));
            }
            _ => {
                out.push_str(&format!(
                    "{}contRef.value.resume(returning: result)\n",
                    indent
                ));
            }
        },
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            out.push_str(&format!("{}if let result = result {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: Data(bytes: result, count: Int(resultLen)))\n",
                indent
            ));
            out.push_str(&format!("{}}} else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: Data())\n",
                indent
            ));
            out.push_str(&format!("{}}}\n", indent));
        }
        Some(TypeRef::List(inner)) => {
            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: [])\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}let len = Int(resultLen)\n", indent));
            match inner.as_ref() {
                TypeRef::Enum(name) => {
                    out.push_str(&format!(
                        "{}contRef.value.resume(returning: (0..<len).map {{ {}(rawValue: result[$0].rawValue)! }})\n",
                        indent, name
                    ));
                }
                TypeRef::Struct(name) => {
                    let name = local_type_name(name);
                    out.push_str(&format!(
                        "{}contRef.value.resume(returning: (0..<len).map {{ {}(ptr: result[$0]!) }})\n",
                        indent, name
                    ));
                }
                TypeRef::TypedHandle(name) => {
                    out.push_str(&format!(
                        "{}contRef.value.resume(returning: (0..<len).map {{ {}(ptr: result[$0]!) }})\n",
                        indent, name
                    ));
                }
                _ => {
                    out.push_str(&format!(
                        "{}contRef.value.resume(returning: Array(UnsafeBufferPointer(start: result, count: len)))\n",
                        indent
                    ));
                }
            }
        }
        Some(TypeRef::Map(k, v)) => {
            let key_swift = swift_type_for(k);
            let val_swift = swift_type_for(v);
            out.push_str(&format!(
                "{}guard let resultKeys = resultKeys, let resultValues = resultValues else {{\n",
                indent
            ));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: [:])\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}let len = Int(resultLen)\n", indent));
            out.push_str(&format!(
                "{}var dict: [{}: {}] = [:]\n",
                indent, key_swift, val_swift
            ));
            out.push_str(&format!("{}for i in 0..<len {{\n", indent));
            let key_expr = map_element_read(k, "resultKeys[i]");
            let val_expr = map_element_read(v, "resultValues[i]");
            out.push_str(&format!(
                "{}    dict[{}] = {}\n",
                indent, key_expr, val_expr
            ));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: dict)\n",
                indent
            ));
        }
        Some(TypeRef::Iterator(inner)) => {
            let pascal_func = func_name.to_upper_camel_case();
            let iter_prefix = format!("weaveffi_{module_name}_{pascal_func}Iterator");
            let next_fn = format!("{iter_prefix}_next");
            let destroy_fn = format!("{iter_prefix}_destroy");
            let inner_swift = swift_type_for(inner);

            out.push_str(&format!("{}guard let result = result else {{\n", indent));
            out.push_str(&format!(
                "{}    contRef.value.resume(returning: [])\n",
                indent
            ));
            out.push_str(&format!("{}    return\n", indent));
            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}var items: [{}] = []\n", indent, inner_swift));

            match inner.as_ref() {
                TypeRef::Struct(name) => {
                    let name = local_type_name(name);
                    out.push_str(&format!(
                        "{}while let ptr = {}(result) {{\n",
                        indent, next_fn
                    ));
                    out.push_str(&format!("{}    items.append({}(ptr: ptr))\n", indent, name));
                }
                TypeRef::TypedHandle(name) => {
                    out.push_str(&format!(
                        "{}while let ptr = {}(result) {{\n",
                        indent, next_fn
                    ));
                    out.push_str(&format!("{}    items.append({}(ptr: ptr))\n", indent, name));
                }
                TypeRef::StringUtf8 => {
                    out.push_str(&format!(
                        "{}while let ptr = {}(result) {{\n",
                        indent, next_fn
                    ));
                    out.push_str(&format!(
                        "{}    items.append(String(cString: ptr))\n",
                        indent
                    ));
                }
                TypeRef::Enum(name) => {
                    out.push_str(&format!(
                        "{}while let raw = {}(result) {{\n",
                        indent, next_fn
                    ));
                    out.push_str(&format!(
                        "{}    items.append({}(rawValue: raw.pointee.rawValue)!)\n",
                        indent, name
                    ));
                }
                _ => {
                    out.push_str(&format!(
                        "{}while let val = {}(result) {{\n",
                        indent, next_fn
                    ));
                    out.push_str(&format!("{}    items.append(val.pointee)\n", indent));
                }
            }

            out.push_str(&format!("{}}}\n", indent));
            out.push_str(&format!("{}{}(result)\n", indent, destroy_fn));
            out.push_str(&format!(
                "{}contRef.value.resume(returning: items)\n",
                indent
            ));
        }
        Some(_) => {
            out.push_str(&format!(
                "{}contRef.value.resume(returning: result)\n",
                indent
            ));
        }
    }
}

fn build_c_call_args(params: &[Param], module_name: &str) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => args.push(format!("{}.ptr", p.name)),
            TypeRef::Enum(enum_name) => args.push(format!(
                "weaveffi_{}_{}({}.rawValue)",
                module_name, enum_name, p.name
            )),
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    args.push(format!("{}?.ptr", p.name))
                }
                TypeRef::StringUtf8
                | TypeRef::BorrowedStr
                | TypeRef::Bytes
                | TypeRef::BorrowedBytes => {
                    args.push(format!("{}_ptr", p.name));
                    args.push(format!("{}_len", p.name));
                }
                _ => args.push(format!("{}_ptr", p.name)),
            },
            TypeRef::List(_) => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            TypeRef::Map(_, _) => {
                args.push(format!("{}_keys_ptr", p.name));
                args.push(format!("{}_values_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            _ => args.push(p.name.clone()),
        }
    }
    args.join(", ")
}

fn render_direct_call(out: &mut String, f: &Function, call_with_err: &str, module_name: &str) {
    match &f.returns {
        None => {
            out.push_str(&format!("        {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: \"null string\") }\n");
            out.push_str("        defer { weaveffi_free_string(rv) }\n");
            out.push_str("        return String(cString: rv)\n");
        }
        Some(TypeRef::Bytes) => {
            out.push_str("        var outLen: Int = 0\n");
            out.push_str(&format!(
                "        let rv = {}\n",
                call_with_err.replace("&err)", "&outLen, &err)")
            ));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { return Data() }\n");
            out.push_str("        defer { weaveffi_free_bytes(UnsafeMutablePointer(mutating: rv), outLen) }\n");
            out.push_str("        return Data(bytes: rv, count: outLen)\n");
        }
        Some(TypeRef::Struct(name)) => {
            let name = local_type_name(name);
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
            out.push_str(&format!("        return {}(ptr: rv)\n", name));
        }
        Some(TypeRef::TypedHandle(name)) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
            out.push_str(&format!("        return {}(ptr: rv)\n", name));
        }
        Some(TypeRef::Enum(name)) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str(&format!(
                "        return {}(rawValue: rv.rawValue)!\n",
                name
            ));
        }
        Some(TypeRef::Optional(inner)) => {
            render_optional_return(out, call_with_err, inner);
        }
        Some(TypeRef::List(inner)) => {
            render_list_return(out, call_with_err, inner);
        }
        Some(TypeRef::Map(k, v)) => {
            render_map_return(out, call_with_err, k, v);
        }
        Some(TypeRef::Iterator(inner)) => {
            render_iterator_return(out, module_name, &f.name, inner, call_with_err, "        ");
        }
        Some(_) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        return rv\n");
        }
    }
}

fn render_optional_return(out: &mut String, call_with_err: &str, inner: &TypeRef) {
    match inner {
        TypeRef::StringUtf8 => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { return nil }\n");
            out.push_str("        defer { weaveffi_free_string(rv) }\n");
            out.push_str("        return String(cString: rv)\n");
        }
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str(&format!("        return rv.map {{ {}(ptr: $0) }}\n", name));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str(&format!("        return rv.map {{ {}(ptr: $0) }}\n", name));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str(&format!(
                "        return rv.map {{ {}(rawValue: $0.pointee.rawValue)! }}\n",
                name
            ));
        }
        _ if is_c_value_type(inner) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        return rv?.pointee\n");
        }
        _ => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        return rv\n");
        }
    }
}

fn render_list_return(out: &mut String, call_with_err: &str, inner: &TypeRef) {
    out.push_str("        var outLen: Int = 0\n");
    let modified_call = call_with_err.replace("&err)", "&outLen, &err)");
    out.push_str(&format!("        let rv = {}\n", modified_call));
    out.push_str("        try check(&err)\n");
    out.push_str("        guard let rv = rv else { return [] }\n");
    match inner {
        TypeRef::Enum(name) => {
            out.push_str(&format!(
                "        return (0..<outLen).map {{ {}(rawValue: rv[$0].rawValue)! }}\n",
                name
            ));
        }
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!(
                "        return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                name
            ));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!(
                "        return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                name
            ));
        }
        _ => {
            out.push_str("        return Array(UnsafeBufferPointer(start: rv, count: outLen))\n");
        }
    }
}

fn render_optional_return_inner(out: &mut String, call: &str, inner: &TypeRef, indent: &str) {
    match inner {
        TypeRef::StringUtf8 => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!(
                "{}    guard let rv = rv else {{ return nil }}\n",
                indent
            ));
            out.push_str(&format!(
                "{}    defer {{ weaveffi_free_string(rv) }}\n",
                indent
            ));
            out.push_str(&format!("{}    return String(cString: rv)\n", indent));
        }
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!(
                "{}    return rv.map {{ {}(ptr: $0) }}\n",
                indent, name
            ));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!(
                "{}    return rv.map {{ {}(ptr: $0) }}\n",
                indent, name
            ));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!(
                "{}    return rv.map {{ {}(rawValue: $0.pointee.rawValue)! }}\n",
                indent, name
            ));
        }
        _ if is_c_value_type(inner) => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!("{}    return rv?.pointee\n", indent));
        }
        _ => {
            out.push_str(&format!("{}    let rv = {}\n", indent, call));
            out.push_str(&format!("{}    try check(&err)\n", indent));
            out.push_str(&format!("{}    return rv\n", indent));
        }
    }
}

fn render_list_return_inner(out: &mut String, call: &str, inner: &TypeRef, indent: &str) {
    out.push_str(&format!("{}    let rv = {}\n", indent, call));
    out.push_str(&format!("{}    try check(&err)\n", indent));
    out.push_str(&format!(
        "{}    guard let rv = rv else {{ return [] }}\n",
        indent
    ));
    match inner {
        TypeRef::Enum(name) => {
            out.push_str(&format!(
                "{}    return (0..<outLen).map {{ {}(rawValue: rv[$0].rawValue)! }}\n",
                indent, name
            ));
        }
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!(
                "{}    return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                indent, name
            ));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!(
                "{}    return (0..<outLen).map {{ {}(ptr: rv[$0]!) }}\n",
                indent, name
            ));
        }
        _ => {
            out.push_str(&format!(
                "{}    return Array(UnsafeBufferPointer(start: rv, count: outLen))\n",
                indent
            ));
        }
    }
}

fn swift_c_ptr_element(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "Int32".to_string(),
        TypeRef::U32 => "UInt32".to_string(),
        TypeRef::I64 => "Int64".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Bool".to_string(),
        TypeRef::Handle => "UInt64".to_string(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "UnsafePointer<CChar>?".to_string(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "UInt8".to_string(),
        TypeRef::Enum(_) => "Int32".to_string(),
        TypeRef::TypedHandle(_) | TypeRef::Struct(_) => "OpaquePointer?".to_string(),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) | TypeRef::Iterator(_) => {
            "OpaquePointer?".to_string()
        }
    }
}

fn map_element_read(ty: &TypeRef, expr: &str) -> String {
    match ty {
        TypeRef::StringUtf8 => format!("String(cString: {}!)", expr),
        TypeRef::Enum(name) => format!("{}(rawValue: {}.rawValue)!", name, expr),
        TypeRef::Struct(name) => format!("{}(ptr: {}!)", local_type_name(name), expr),
        TypeRef::TypedHandle(name) => format!("{}(ptr: {}!)", name, expr),
        _ => expr.to_string(),
    }
}

fn render_map_return(out: &mut String, call_with_err: &str, k: &TypeRef, v: &TypeRef) {
    let key_elem = swift_c_ptr_element(k);
    let val_elem = swift_c_ptr_element(v);
    let key_swift = swift_type_for(k);
    let val_swift = swift_type_for(v);

    out.push_str(&format!(
        "        var outKeysPtr: UnsafeMutablePointer<{}>? = nil\n",
        key_elem
    ));
    out.push_str(&format!(
        "        var outValuesPtr: UnsafeMutablePointer<{}>? = nil\n",
        val_elem
    ));
    out.push_str("        var outLen: Int = 0\n");
    let modified_call =
        call_with_err.replace("&err)", "&outKeysPtr, &outValuesPtr, &outLen, &err)");
    out.push_str(&format!("        {}\n", modified_call));
    out.push_str("        try check(&err)\n");
    out.push_str(
        "        guard let outKeys = outKeysPtr, let outValues = outValuesPtr else { return [:] }\n",
    );
    out.push_str(&format!(
        "        var result: [{}: {}] = [:]\n",
        key_swift, val_swift
    ));
    out.push_str("        for i in 0..<outLen {\n");
    let key_expr = map_element_read(k, "outKeys[i]");
    let val_expr = map_element_read(v, "outValues[i]");
    out.push_str(&format!(
        "            result[{}] = {}\n",
        key_expr, val_expr
    ));
    out.push_str("        }\n");
    out.push_str("        return result\n");
}

fn render_map_return_inner(out: &mut String, call: &str, k: &TypeRef, v: &TypeRef, indent: &str) {
    let key_swift = swift_type_for(k);
    let val_swift = swift_type_for(v);

    out.push_str(&format!("{}    {}\n", indent, call));
    out.push_str(&format!("{}    try check(&err)\n", indent));
    out.push_str(&format!(
        "{}    guard let outKeys = outKeysPtr, let outValues = outValuesPtr else {{ return [:] }}\n",
        indent
    ));
    out.push_str(&format!(
        "{}    var result: [{}: {}] = [:]\n",
        indent, key_swift, val_swift
    ));
    out.push_str(&format!("{}    for i in 0..<outLen {{\n", indent));
    let key_expr = map_element_read(k, "outKeys[i]");
    let val_expr = map_element_read(v, "outValues[i]");
    out.push_str(&format!(
        "{}        result[{}] = {}\n",
        indent, key_expr, val_expr
    ));
    out.push_str(&format!("{}    }}\n", indent));
    out.push_str(&format!("{}    return result\n", indent));
}

fn render_iterator_return(
    out: &mut String,
    module_name: &str,
    func_name: &str,
    inner: &TypeRef,
    call_with_err: &str,
    indent: &str,
) {
    let pascal_func = func_name.to_upper_camel_case();
    let iter_prefix = format!("weaveffi_{module_name}_{pascal_func}Iterator");
    let next_fn = format!("{iter_prefix}_next");
    let destroy_fn = format!("{iter_prefix}_destroy");
    let inner_swift = swift_type_for(inner);

    out.push_str(&format!("{indent}let iter = {call_with_err}\n"));
    out.push_str(&format!("{indent}try check(&err)\n"));
    out.push_str(&format!(
        "{indent}guard let iter = iter else {{ return [] }}\n"
    ));
    out.push_str(&format!("{indent}var items: [{inner_swift}] = []\n"));

    match inner {
        TypeRef::Struct(name) => {
            let name = local_type_name(name);
            out.push_str(&format!("{indent}while let ptr = {next_fn}(iter) {{\n"));
            out.push_str(&format!("{indent}    items.append({name}(ptr: ptr))\n"));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("{indent}while let ptr = {next_fn}(iter) {{\n"));
            out.push_str(&format!("{indent}    items.append({name}(ptr: ptr))\n"));
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!("{indent}while let ptr = {next_fn}(iter) {{\n"));
            out.push_str(&format!("{indent}    items.append(String(cString: ptr))\n"));
        }
        TypeRef::Enum(name) => {
            out.push_str(&format!("{indent}while let raw = {next_fn}(iter) {{\n"));
            out.push_str(&format!(
                "{indent}    items.append({name}(rawValue: raw.pointee.rawValue)!)\n"
            ));
        }
        _ => {
            out.push_str(&format!("{indent}while let val = {next_fn}(iter) {{\n"));
            out.push_str(&format!("{indent}    items.append(val.pointee)\n"));
        }
    }

    out.push_str(&format!("{indent}}}\n"));
    out.push_str(&format!("{indent}{destroy_fn}(iter)\n"));
    out.push_str(&format!("{indent}return items\n"));
}

fn map_array_source(ty: &TypeRef, name: &str, suffix: &str) -> String {
    match ty {
        TypeRef::Enum(_) => format!("{name}_{suffix}Raw"),
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("{name}_{suffix}Ptrs"),
        _ => format!("{name}_{suffix}"),
    }
}

fn render_buffered_call(out: &mut String, f: &Function, params: &[Param], module_name: &str) {
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!(
                    "        let {n}_bytes = Array({n}.utf8)\n",
                    n = p.name
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("        let {n}_bytes = Array({n})\n", n = p.name));
            }
            TypeRef::Optional(inner) => {
                if let TypeRef::Enum(enum_name) = inner.as_ref() {
                    out.push_str(&format!(
                        "        let {n}_c: weaveffi_{m}_{e}? = {n}.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        n = p.name, m = module_name, e = enum_name
                    ));
                }
            }
            TypeRef::List(inner) => match inner.as_ref() {
                TypeRef::Enum(enum_name) => {
                    out.push_str(&format!(
                        "        let {n}_raw = {n}.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!(
                        "        let {n}_ptrs = {n}.map {{ $0.ptr }}\n",
                        n = p.name
                    ));
                }
                _ => {}
            },
            TypeRef::Map(k, v) => {
                out.push_str(&format!(
                    "        let {n}_keys = Array({n}.keys)\n",
                    n = p.name
                ));
                out.push_str(&format!(
                    "        let {n}_values = {n}_keys.map {{ {n}[$0]! }}\n",
                    n = p.name
                ));
                match k.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_keysRaw = {n}_keys.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                            n = p.name, m = module_name, e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_keysPtrs = {n}_keys.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
                match v.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_valuesRaw = {n}_values.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                            n = p.name, m = module_name, e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_valuesPtrs = {n}_values.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let closure_params: Vec<&Param> = params.iter().filter(|p| needs_closure(&p.ty)).collect();

    let is_list_return = matches!(f.returns.as_ref(), Some(TypeRef::List(_)));
    let is_map_return = matches!(f.returns.as_ref(), Some(TypeRef::Map(_, _)));
    if is_list_return || is_map_return {
        out.push_str("        var outLen: Int = 0\n");
    }
    if let Some(TypeRef::Map(k, v)) = &f.returns {
        let key_elem = swift_c_ptr_element(k);
        let val_elem = swift_c_ptr_element(v);
        out.push_str(&format!(
            "        var outKeysPtr: UnsafeMutablePointer<{}>? = nil\n",
            key_elem
        ));
        out.push_str(&format!(
            "        var outValuesPtr: UnsafeMutablePointer<{}>? = nil\n",
            val_elem
        ));
    }

    let handles_return_inside = matches!(
        f.returns.as_ref(),
        Some(TypeRef::StringUtf8)
            | Some(TypeRef::Enum(_))
            | Some(TypeRef::Optional(_))
            | Some(TypeRef::List(_))
            | Some(TypeRef::Map(_, _))
            | Some(TypeRef::Iterator(_))
    );

    let ret_type = match &f.returns {
        Some(TypeRef::Struct(_) | TypeRef::TypedHandle(_)) => "OpaquePointer?".to_string(),
        Some(ty) => swift_type_for(ty),
        None => "Void".to_string(),
    };
    let needs_return = f.returns.is_some();

    let mut closure_depth: usize = 0;
    for p in &closure_params {
        let indent = "        ".to_string() + &"    ".repeat(closure_depth);
        let is_first = closure_depth == 0;
        match &p.ty {
            TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes => {
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = {}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, ret_type, p.name, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, p.name, p.name
                    ));
                }
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress!\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner) if is_c_value_type(inner) => {
                let source = if matches!(inner.as_ref(), TypeRef::Enum(_)) {
                    format!("{}_c", p.name)
                } else {
                    p.name.clone()
                };
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = withOptionalPointer(to: {}) {{ {}_ptr in\n",
                        indent, ret_type, source, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}withOptionalPointer(to: {}) {{ {}_ptr in\n",
                        indent, source, p.name
                    ));
                }
                closure_depth += 1;
            }
            TypeRef::List(inner) => {
                let source = match inner.as_ref() {
                    TypeRef::Enum(_) => format!("{}_raw", p.name),
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("{}_ptrs", p.name),
                    _ => p.name.clone(),
                };
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = {}.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, ret_type, source, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{}.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, source, p.name
                    ));
                }
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Map(k, v) => {
                let keys_source = map_array_source(k, &p.name, "keys");
                let values_source = map_array_source(v, &p.name, "values");
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = {}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                        indent, ret_type, keys_source, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                        indent, keys_source, p.name
                    ));
                }
                out.push_str(&format!(
                    "{}    let {}_keys_ptr = {}_keys_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
                let vind = "        ".to_string() + &"    ".repeat(closure_depth);
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_values_buf in\n",
                    vind, values_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_values_ptr = {}_values_buf.baseAddress\n",
                    vind, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_values_buf.count\n",
                    vind, p.name, p.name
                ));
                closure_depth += 1;
            }
            _ => unreachable!(),
        }
    }

    let inner_indent = "        ".to_string() + &"    ".repeat(closure_depth);
    let c_sym = c_symbol_name(module_name, &f.name);
    let call_args = build_c_call_args(params, module_name);
    let call_with_err = if is_map_return {
        if call_args.is_empty() {
            format!("{}(&outKeysPtr, &outValuesPtr, &outLen, &err)", c_sym)
        } else {
            format!(
                "{}({}, &outKeysPtr, &outValuesPtr, &outLen, &err)",
                c_sym, call_args
            )
        }
    } else if is_list_return {
        if call_args.is_empty() {
            format!("{}(&outLen, &err)", c_sym)
        } else {
            format!("{}({}, &outLen, &err)", c_sym, call_args)
        }
    } else if call_args.is_empty() {
        format!("{}(&err)", c_sym)
    } else {
        format!("{}({}, &err)", c_sym, call_args)
    };

    match &f.returns {
        None => {
            out.push_str(&format!("{}    {}\n", inner_indent, call_with_err));
        }
        Some(TypeRef::StringUtf8) => {
            out.push_str(&format!("{}    let rv = {}\n", inner_indent, call_with_err));
            out.push_str(&format!("{}    try check(&err)\n", inner_indent));
            out.push_str(&format!("{}    guard let rv = rv else {{ throw WeaveFFIError.error(code: -1, message: \"null string\") }}\n", inner_indent));
            out.push_str(&format!(
                "{}    defer {{ weaveffi_free_string(rv) }}\n",
                inner_indent
            ));
            out.push_str(&format!("{}    return String(cString: rv)\n", inner_indent));
        }
        Some(TypeRef::Enum(name)) => {
            out.push_str(&format!("{}    let rv = {}\n", inner_indent, call_with_err));
            out.push_str(&format!("{}    try check(&err)\n", inner_indent));
            out.push_str(&format!(
                "{}    return {}(rawValue: rv.rawValue)!\n",
                inner_indent, name
            ));
        }
        Some(TypeRef::Optional(inner)) => {
            render_optional_return_inner(out, &call_with_err, inner, &inner_indent);
        }
        Some(TypeRef::List(inner)) => {
            render_list_return_inner(out, &call_with_err, inner, &inner_indent);
        }
        Some(TypeRef::Map(k, v)) => {
            render_map_return_inner(out, &call_with_err, k, v, &inner_indent);
        }
        Some(TypeRef::Iterator(inner)) => {
            let ind = format!("{}    ", inner_indent);
            render_iterator_return(out, module_name, &f.name, inner, &call_with_err, &ind);
        }
        Some(_) => {
            out.push_str(&format!("{}    return {}\n", inner_indent, call_with_err));
        }
    }

    for i in (0..closure_depth).rev() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        out.push_str(&format!("{}}}\n", indent));
    }

    if f.returns.is_none() {
        out.push_str("        try check(&err)\n");
    } else if let Some(TypeRef::Struct(name)) = &f.returns {
        let name = local_type_name(name);
        out.push_str("        try check(&err)\n");
        out.push_str("        guard let result = result else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
        out.push_str(&format!("        return {}(ptr: result)\n", name));
    } else if let Some(TypeRef::TypedHandle(name)) = &f.returns {
        out.push_str("        try check(&err)\n");
        out.push_str("        guard let result = result else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
        out.push_str(&format!("        return {}(ptr: result)\n", name));
    } else if handles_return_inside {
        out.push_str("        return result\n");
    } else {
        out.push_str("        try check(&err)\n");
        out.push_str("        return result\n");
    }
}

/// Like `render_buffered_call`, but calls `{struct_prefix}_create` and always returns a struct pointer.
fn render_buffered_struct_create(
    out: &mut String,
    module_name: &str,
    struct_prefix: &str,
    params: &[Param],
    struct_class_name: &str,
) {
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str(&format!(
                    "        let {n}_bytes = Array({n}.utf8)\n",
                    n = p.name
                ));
            }
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("        let {n}_bytes = Array({n})\n", n = p.name));
            }
            TypeRef::Optional(inner) => {
                if let TypeRef::Enum(enum_name) = inner.as_ref() {
                    out.push_str(&format!(
                        "        let {n}_c: weaveffi_{m}_{e}? = {n}.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
            }
            TypeRef::List(inner) => match inner.as_ref() {
                TypeRef::Enum(enum_name) => {
                    out.push_str(&format!(
                        "        let {n}_raw = {n}.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                        n = p.name,
                        m = module_name,
                        e = enum_name
                    ));
                }
                TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                    out.push_str(&format!(
                        "        let {n}_ptrs = {n}.map {{ $0.ptr }}\n",
                        n = p.name
                    ));
                }
                _ => {}
            },
            TypeRef::Map(k, v) => {
                out.push_str(&format!(
                    "        let {n}_keys = Array({n}.keys)\n",
                    n = p.name
                ));
                out.push_str(&format!(
                    "        let {n}_values = {n}_keys.map {{ {n}[$0]! }}\n",
                    n = p.name
                ));
                match k.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_keysRaw = {n}_keys.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                            n = p.name,
                            m = module_name,
                            e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_keysPtrs = {n}_keys.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
                match v.as_ref() {
                    TypeRef::Enum(e) => {
                        out.push_str(&format!(
                            "        let {n}_valuesRaw = {n}_values.map {{ weaveffi_{m}_{e}($0.rawValue) }}\n",
                            n = p.name,
                            m = module_name,
                            e = e
                        ));
                    }
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
                        out.push_str(&format!(
                            "        let {n}_valuesPtrs = {n}_values.map {{ $0.ptr }}\n",
                            n = p.name
                        ));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let closure_params: Vec<&Param> = params.iter().filter(|p| needs_closure(&p.ty)).collect();

    let ret_type = "OpaquePointer?";
    let needs_return = true;

    let mut closure_depth: usize = 0;
    for p in &closure_params {
        let indent = "        ".to_string() + &"    ".repeat(closure_depth);
        let is_first = closure_depth == 0;
        match &p.ty {
            TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes => {
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = {}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, ret_type, p.name, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{}_bytes.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, p.name, p.name
                    ));
                }
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress!\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Optional(inner) if is_c_value_type(inner) => {
                let source = if matches!(inner.as_ref(), TypeRef::Enum(_)) {
                    format!("{}_c", p.name)
                } else {
                    p.name.clone()
                };
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = withOptionalPointer(to: {}) {{ {}_ptr in\n",
                        indent, ret_type, source, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}withOptionalPointer(to: {}) {{ {}_ptr in\n",
                        indent, source, p.name
                    ));
                }
                closure_depth += 1;
            }
            TypeRef::List(inner) => {
                let source = match inner.as_ref() {
                    TypeRef::Enum(_) => format!("{}_raw", p.name),
                    TypeRef::Struct(_) | TypeRef::TypedHandle(_) => format!("{}_ptrs", p.name),
                    _ => p.name.clone(),
                };
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = {}.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, ret_type, source, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{}.withUnsafeBufferPointer {{ {}_buf in\n",
                        indent, source, p.name
                    ));
                }
                out.push_str(&format!(
                    "{}    let {}_ptr = {}_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_buf.count\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
            }
            TypeRef::Map(k, v) => {
                let keys_source = map_array_source(k, &p.name, "keys");
                let values_source = map_array_source(v, &p.name, "values");
                if needs_return && is_first {
                    out.push_str(&format!(
                        "{}let result: {} = {}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                        indent, ret_type, keys_source, p.name
                    ));
                } else {
                    out.push_str(&format!(
                        "{}{}.withUnsafeBufferPointer {{ {}_keys_buf in\n",
                        indent, keys_source, p.name
                    ));
                }
                out.push_str(&format!(
                    "{}    let {}_keys_ptr = {}_keys_buf.baseAddress\n",
                    indent, p.name, p.name
                ));
                closure_depth += 1;
                let vind = "        ".to_string() + &"    ".repeat(closure_depth);
                out.push_str(&format!(
                    "{}{}.withUnsafeBufferPointer {{ {}_values_buf in\n",
                    vind, values_source, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_values_ptr = {}_values_buf.baseAddress\n",
                    vind, p.name, p.name
                ));
                out.push_str(&format!(
                    "{}    let {}_len = {}_values_buf.count\n",
                    vind, p.name, p.name
                ));
                closure_depth += 1;
            }
            _ => unreachable!(),
        }
    }

    let inner_indent = "        ".to_string() + &"    ".repeat(closure_depth);
    let create_sym = format!("{struct_prefix}_create");
    let call_args = build_c_call_args(params, module_name);
    let call_with_err = if call_args.is_empty() {
        format!("{}(&err)", create_sym)
    } else {
        format!("{}({}, &err)", create_sym, call_args)
    };

    out.push_str(&format!("{}    return {}\n", inner_indent, call_with_err));

    for i in (0..closure_depth).rev() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        out.push_str(&format!("{}}}\n", indent));
    }

    out.push_str("        try check(&err)\n");
    out.push_str(
        "        guard let result = result else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n",
    );
    out.push_str(&format!(
        "        return {}(ptr: result)\n",
        struct_class_name
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
        StructField,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules,
            generators: None,
        }
    }

    #[test]
    fn swift_type_for_struct_returns_name() {
        assert_eq!(
            swift_type_for(&TypeRef::Struct("Contact".into())),
            "Contact"
        );
    }

    #[test]
    fn swift_type_for_enum_returns_name() {
        assert_eq!(swift_type_for(&TypeRef::Enum("Color".into())), "Color");
    }

    #[test]
    fn swift_type_for_optional_wraps_inner() {
        assert_eq!(
            swift_type_for(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "Int32?"
        );
        assert_eq!(
            swift_type_for(&TypeRef::Optional(Box::new(TypeRef::Struct(
                "Contact".into()
            )))),
            "Contact?"
        );
    }

    #[test]
    fn swift_type_for_list_wraps_inner() {
        assert_eq!(
            swift_type_for(&TypeRef::List(Box::new(TypeRef::I32))),
            "[Int32]"
        );
        assert_eq!(
            swift_type_for(&TypeRef::List(Box::new(TypeRef::Enum("Color".into())))),
            "[Color]"
        );
    }

    #[test]
    fn render_enum_declaration() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".to_string(),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("public enum Color: Int32 {"),
            "missing enum declaration: {out}"
        );
        assert!(out.contains("case red = 0"), "missing red variant: {out}");
        assert!(
            out.contains("case green = 1"),
            "missing green variant: {out}"
        );
        assert!(out.contains("case blue = 2"), "missing blue variant: {out}");
    }

    #[test]
    fn render_enum_variant_camel_case() {
        let api = make_api(vec![Module {
            name: "status".to_string(),
            functions: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Status".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "InProgress".to_string(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "AllDone".to_string(),
                        value: 1,
                        doc: None,
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("case inProgress = 0"),
            "missing camelCase variant: {out}"
        );
        assert!(
            out.contains("case allDone = 1"),
            "missing camelCase variant: {out}"
        );
    }

    #[test]
    fn render_function_with_enum_param_and_return() {
        let api = make_api(vec![Module {
            name: "paint".to_string(),
            functions: vec![Function {
                name: "mix".to_string(),
                params: vec![Param {
                    name: "a".to_string(),
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
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);
        assert!(out.contains("_ a: Color"), "missing enum param type: {out}");
        assert!(
            out.contains("-> Color {"),
            "missing enum return type: {out}"
        );
        assert!(
            out.contains("weaveffi_paint_Color(a.rawValue)"),
            "missing enum-to-C conversion: {out}"
        );
        assert!(
            out.contains("Color(rawValue: rv.rawValue)!"),
            "missing C-to-enum conversion: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_value_param() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::I32)),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("_ id: Int32?"),
            "missing optional param type: {out}"
        );
        assert!(
            out.contains("withOptionalPointer(to: id)"),
            "missing withOptionalPointer call: {out}"
        );
        assert!(out.contains("id_ptr"), "missing pointer binding: {out}");
    }

    #[test]
    fn render_function_with_optional_struct_param() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "update".to_string(),
                params: vec![Param {
                    name: "person".to_string(),
                    ty: TypeRef::Optional(Box::new(TypeRef::Struct("Contact".into()))),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("_ person: Contact?"),
            "missing optional struct param: {out}"
        );
        assert!(
            out.contains("person?.ptr"),
            "missing optional struct ptr access: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_value_return() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "lookup".to_string(),
                params: vec![Param {
                    name: "key".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("-> Int32? {"),
            "missing optional return type: {out}"
        );
        assert!(
            out.contains("rv?.pointee"),
            "missing pointer dereference: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_string_return() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_name".to_string(),
                params: vec![],
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("-> String? {"),
            "missing optional string return type: {out}"
        );
        assert!(
            out.contains("guard let rv = rv else { return nil }"),
            "missing nil guard: {out}"
        );
        assert!(
            out.contains("weaveffi_free_string(rv)"),
            "missing free_string: {out}"
        );
    }

    #[test]
    fn render_function_with_list_param() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "process".to_string(),
                params: vec![Param {
                    name: "ids".to_string(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("_ ids: [Int32]"),
            "missing list param type: {out}"
        );
        assert!(
            out.contains(".withUnsafeBufferPointer"),
            "missing withUnsafeBufferPointer: {out}"
        );
        assert!(out.contains("ids_ptr"), "missing pointer binding: {out}");
        assert!(out.contains("ids_len"), "missing length binding: {out}");
    }

    #[test]
    fn render_function_with_list_return() {
        let api = make_api(vec![Module {
            name: "batch".to_string(),
            functions: vec![Function {
                name: "get_ids".to_string(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("-> [Int32] {"),
            "missing list return type: {out}"
        );
        assert!(
            out.contains("var outLen: Int = 0"),
            "missing outLen declaration: {out}"
        );
        assert!(out.contains("&outLen"), "missing outLen in call: {out}");
        assert!(
            out.contains("UnsafeBufferPointer(start: rv, count: outLen)"),
            "missing buffer-to-array conversion: {out}"
        );
    }

    #[test]
    fn render_function_with_optional_struct_return() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "find".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("-> Contact? {"),
            "missing optional struct return: {out}"
        );
        assert!(
            out.contains("rv.map { Contact(ptr: $0) }"),
            "missing optional struct wrapping: {out}"
        );
    }

    #[test]
    fn render_with_optional_pointer_helper() {
        let api = make_api(vec![]);
        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("func withOptionalPointer<T, R>"),
            "missing withOptionalPointer helper: {out}"
        );
        assert!(
            out.contains("guard let value = value else { return try body(nil) }"),
            "missing nil guard in helper: {out}"
        );
    }

    #[test]
    fn render_struct_class_with_fields() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("public class Contact {"),
            "missing class declaration: {out}"
        );
        assert!(
            out.contains("let ptr: OpaquePointer"),
            "missing ptr property: {out}"
        );
        assert!(
            out.contains("init(ptr: OpaquePointer)"),
            "missing init: {out}"
        );
        assert!(
            out.contains("weaveffi_contacts_Contact_destroy(ptr)"),
            "missing destroy in deinit: {out}"
        );
        assert!(
            out.contains("public var name: String {"),
            "missing name getter: {out}"
        );
        assert!(
            out.contains("weaveffi_contacts_Contact_get_name(ptr)"),
            "missing name getter call: {out}"
        );
        assert!(
            out.contains("String(cString: raw)"),
            "missing string conversion: {out}"
        );
        assert!(
            out.contains("weaveffi_free_string(raw)"),
            "missing free_string: {out}"
        );
        assert!(
            out.contains("public var age: Int32 {"),
            "missing age getter: {out}"
        );
        assert!(
            out.contains("weaveffi_contacts_Contact_get_age(ptr)"),
            "missing age getter call: {out}"
        );
    }

    #[test]
    fn swift_builder_generated() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![
                        StructField {
                            name: "name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                            default: None,
                        },
                        StructField {
                            name: "age".into(),
                            ty: TypeRef::I32,
                            doc: None,
                            default: None,
                        },
                    ],
                    builder: true,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        };
        let tmp = std::env::temp_dir().join("weaveffi_test_swift_builder");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");
        SwiftGenerator.generate(&api, out_dir).unwrap();
        let swift = std::fs::read_to_string(
            tmp.join("swift")
                .join("Sources")
                .join("WeaveFFI")
                .join("WeaveFFI.swift"),
        )
        .unwrap();
        assert!(
            swift.contains("public class ContactBuilder"),
            "missing builder class: {swift}"
        );
        assert!(
            swift.contains("func withName("),
            "missing withName: {swift}"
        );
        assert!(swift.contains("func withAge("), "missing withAge: {swift}");
        assert!(swift.contains("func build()"), "missing build: {swift}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn render_function_returning_struct() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create".to_string(),
                params: vec![Param {
                    name: "age".to_string(),
                    ty: TypeRef::I32,
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("-> Contact {"),
            "missing struct return type: {out}"
        );
        assert!(
            out.contains("Contact(ptr: rv)"),
            "missing struct wrapping: {out}"
        );
    }

    #[test]
    fn render_function_with_struct_param() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "save".to_string(),
                params: vec![Param {
                    name: "contact".to_string(),
                    ty: TypeRef::Struct("Contact".into()),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("_ contact: Contact"),
            "missing struct param type: {out}"
        );
        assert!(
            out.contains("contact.ptr"),
            "missing .ptr access for struct param: {out}"
        );
    }

    #[test]
    fn render_struct_with_bytes_field() {
        let api = make_api(vec![Module {
            name: "storage".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Blob".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "data".to_string(),
                    ty: TypeRef::Bytes,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("public var data: Data {"),
            "missing bytes getter: {out}"
        );
        assert!(
            out.contains("weaveffi_storage_Blob_get_data(ptr, &outLen)"),
            "missing bytes getter with outLen: {out}"
        );
    }

    #[test]
    fn render_struct_with_nested_struct_field() {
        let api = make_api(vec![Module {
            name: "geo".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Line".to_string(),
                doc: None,
                fields: vec![StructField {
                    name: "start".to_string(),
                    ty: TypeRef::Struct("Point".into()),
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("public var start: Point {"),
            "missing nested struct getter: {out}"
        );
        assert!(
            out.contains("Point(ptr: weaveffi_geo_Line_get_start(ptr)!)"),
            "missing nested struct wrapping: {out}"
        );
    }

    #[test]
    fn render_function_returning_struct_with_buffer_params() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "find_by_name".to_string(),
                params: vec![Param {
                    name: "query".to_string(),
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
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("-> Contact {"),
            "missing struct return type with buffer params: {out}"
        );
        assert!(
            out.contains("Contact(ptr: result)"),
            "missing struct wrapping after buffered call: {out}"
        );
    }

    #[test]
    fn generate_swift_with_structs_and_enums() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get_contact".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "name".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".to_string(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".to_string(),
                        value: 0,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Green".to_string(),
                        value: 1,
                        doc: None,
                    },
                    EnumVariant {
                        name: "Blue".to_string(),
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

        let tmp = std::env::temp_dir().join("weaveffi_test_swift_structs_and_enums");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        SwiftGenerator.generate(&api, out_dir).unwrap();

        let swift = std::fs::read_to_string(
            tmp.join("swift")
                .join("Sources")
                .join("WeaveFFI")
                .join("WeaveFFI.swift"),
        )
        .unwrap();

        assert!(
            swift.contains("public enum Color: Int32 {"),
            "missing enum declaration: {swift}"
        );
        assert!(swift.contains("case red = 0"), "missing red case: {swift}");
        assert!(
            swift.contains("case green = 1"),
            "missing green case: {swift}"
        );
        assert!(
            swift.contains("case blue = 2"),
            "missing blue case: {swift}"
        );

        assert!(
            swift.contains("public class Contact {"),
            "missing class declaration: {swift}"
        );
        assert!(
            swift.contains("let ptr: OpaquePointer"),
            "missing ptr property: {swift}"
        );
        assert!(
            swift.contains("public var name: String {"),
            "missing name getter: {swift}"
        );
        assert!(
            swift.contains("public var email: String {"),
            "missing email getter: {swift}"
        );
        assert!(
            swift.contains("public var age: Int32 {"),
            "missing age getter: {swift}"
        );

        assert!(
            swift.contains("public static func get_contact(_ id: Int32) throws -> Contact {"),
            "missing function signature: {swift}"
        );
        assert!(
            swift.contains("Contact(ptr: rv)"),
            "missing struct wrapping: {swift}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn swift_type_for_map() {
        assert_eq!(
            swift_type_for(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "[String: Int32]"
        );
        assert_eq!(
            swift_type_for(&TypeRef::Map(
                Box::new(TypeRef::I32),
                Box::new(TypeRef::F64)
            )),
            "[Int32: Double]"
        );
    }

    #[test]
    fn render_function_with_map_param() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "update_scores".to_string(),
                params: vec![Param {
                    name: "scores".to_string(),
                    ty: TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64)),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("_ scores: [Int32: Double]"),
            "missing map param type: {out}"
        );
        assert!(
            out.contains("scores_keys = Array(scores.keys)"),
            "missing keys extraction: {out}"
        );
        assert!(
            out.contains("scores_values = scores_keys.map { scores[$0]! }"),
            "missing values extraction: {out}"
        );
        assert!(
            out.contains(".withUnsafeBufferPointer"),
            "missing withUnsafeBufferPointer: {out}"
        );
        assert!(
            out.contains("scores_keys_ptr"),
            "missing keys pointer: {out}"
        );
        assert!(
            out.contains("scores_values_ptr"),
            "missing values pointer: {out}"
        );
        assert!(out.contains("scores_len"), "missing length: {out}");
    }

    #[test]
    fn render_function_with_map_return() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![Function {
                name: "get_scores".to_string(),
                params: vec![],
                returns: Some(TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64))),
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("-> [Int32: Double] {"),
            "missing map return type: {out}"
        );
        assert!(out.contains("var outLen: Int = 0"), "missing outLen: {out}");
        assert!(out.contains("outKeysPtr"), "missing keys out-param: {out}");
        assert!(
            out.contains("outValuesPtr"),
            "missing values out-param: {out}"
        );
        assert!(
            out.contains("var result: [Int32: Double] = [:]"),
            "missing dict construction: {out}"
        );
        assert!(
            out.contains("for i in 0..<outLen"),
            "missing iteration: {out}"
        );
    }

    #[test]
    fn swift_struct_optional_field_getter() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "email".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "role".to_string(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Enum("Role".into()))),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);

        assert!(
            out.contains("public var email: String? {"),
            "missing optional string getter: {out}"
        );
        assert!(
            out.contains("guard let p = p else { return nil }"),
            "missing nil guard for optional string: {out}"
        );
        assert!(
            out.contains("defer { weaveffi_free_string(p) }"),
            "missing free_string for optional string: {out}"
        );
        assert!(
            out.contains("return String(cString: p)"),
            "missing cString conversion: {out}"
        );

        assert!(
            out.contains("public var age: Int32? {"),
            "missing optional i32 getter: {out}"
        );
        assert!(
            out.contains("return p?.pointee"),
            "missing pointee for optional value: {out}"
        );

        assert!(
            out.contains("public var role: Role? {"),
            "missing optional enum getter: {out}"
        );
        assert!(
            out.contains("Role(rawValue: $0.pointee.rawValue)!"),
            "missing optional enum conversion: {out}"
        );
    }

    #[test]
    fn swift_custom_module_name() {
        let api = make_api(vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add".to_string(),
                params: vec![
                    Param {
                        name: "a".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                    Param {
                        name: "b".to_string(),
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
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let config = GeneratorConfig {
            swift_module_name: Some("MyCoolLib".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_swift_custom_module");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        SwiftGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let pkg = std::fs::read_to_string(tmp.join("swift").join("Package.swift")).unwrap();
        assert!(
            pkg.contains("name: \"MyCoolLib\""),
            "Package.swift should use custom module name: {pkg}"
        );
        assert!(
            pkg.contains("\"CMyCoolLib\""),
            "Package.swift should reference CMyCoolLib: {pkg}"
        );
        assert!(
            !pkg.contains("WeaveFFI"),
            "Package.swift should not contain WeaveFFI: {pkg}"
        );

        let modulemap = std::fs::read_to_string(
            tmp.join("swift")
                .join("CMyCoolLib")
                .join("module.modulemap"),
        )
        .unwrap();
        assert!(
            modulemap.contains("module CMyCoolLib"),
            "modulemap should use custom name: {modulemap}"
        );

        let swift_src = tmp
            .join("swift")
            .join("Sources")
            .join("MyCoolLib")
            .join("MyCoolLib.swift");
        assert!(
            swift_src.exists(),
            "Swift source should be at MyCoolLib/MyCoolLib.swift"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn swift_inline_error_types() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
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
            errors: Some(ErrorDomain {
                name: "ContactError".to_string(),
                codes: vec![
                    ErrorCode {
                        name: "ContactNotFound".to_string(),
                        code: 1001,
                        message: "Contact not found".to_string(),
                    },
                    ErrorCode {
                        name: "InvalidInput".to_string(),
                        code: 1002,
                        message: "Invalid input provided".to_string(),
                    },
                ],
            }),
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);

        assert!(
            out.contains("public enum WeaveFFIError: Error, LocalizedError {"),
            "missing LocalizedError conformance: {out}"
        );
        assert!(
            out.contains("case contactNotFound"),
            "missing contactNotFound case: {out}"
        );
        assert!(
            out.contains("case invalidInput"),
            "missing invalidInput case: {out}"
        );
        assert!(
            out.contains("public var errorDescription: String?"),
            "missing errorDescription property: {out}"
        );
        assert!(
            out.contains("case .contactNotFound: return \"Contact not found\""),
            "missing contactNotFound description: {out}"
        );
        assert!(
            out.contains("case .invalidInput: return \"Invalid input provided\""),
            "missing invalidInput description: {out}"
        );
        assert!(
            out.contains("public var errorCode: Int32"),
            "missing errorCode property: {out}"
        );
        assert!(
            out.contains("case .contactNotFound: return 1001"),
            "missing contactNotFound code: {out}"
        );
        assert!(
            out.contains("case .invalidInput: return 1002"),
            "missing invalidInput code: {out}"
        );
        assert!(
            out.contains("case 1001: throw WeaveFFIError.contactNotFound"),
            "missing domain-specific throw in check(): {out}"
        );
        assert!(
            out.contains("case 1002: throw WeaveFFIError.invalidInput"),
            "missing domain-specific throw in check(): {out}"
        );
        assert!(
            out.contains("default: throw WeaveFFIError.error(code: code, message: message)"),
            "missing fallback throw in check(): {out}"
        );
    }

    #[test]
    fn swift_struct_list_field_getter() {
        let api = make_api(vec![Module {
            name: "store".to_string(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Order".to_string(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "item_ids".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "tags".to_string(),
                        ty: TypeRef::List(Box::new(TypeRef::Enum("Tag".into()))),
                        doc: None,
                        default: None,
                    },
                ],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let out = render_swift_wrapper(&api, true);

        assert!(
            out.contains("public var item_ids: [Int32] {"),
            "missing list i32 getter: {out}"
        );
        assert!(
            out.contains("weaveffi_store_Order_get_item_ids(ptr, &outLen)"),
            "missing list getter call with outLen: {out}"
        );
        assert!(
            out.contains("guard let rv = rv else { return [] }"),
            "missing empty-array guard: {out}"
        );
        assert!(
            out.contains("UnsafeBufferPointer(start: rv, count: outLen)"),
            "missing buffer-to-array conversion: {out}"
        );

        assert!(
            out.contains("public var tags: [Tag] {"),
            "missing list enum getter: {out}"
        );
        assert!(
            out.contains("Tag(rawValue: rv[$0].rawValue)!"),
            "missing list enum conversion: {out}"
        );
    }

    #[test]
    fn swift_strip_module_prefix() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create_contact".to_string(),
                params: vec![Param {
                    name: "name".to_string(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
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
        }]);

        let config = GeneratorConfig {
            strip_module_prefix: true,
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_swift_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        SwiftGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let swift =
            std::fs::read_to_string(tmp.join("swift/Sources/WeaveFFI/WeaveFFI.swift")).unwrap();

        assert!(
            swift.contains("func create_contact("),
            "stripped name should be create_contact: {swift}"
        );
        assert!(
            !swift.contains("func contacts_create_contact("),
            "should not contain module-prefixed name: {swift}"
        );
        assert!(
            swift.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {swift}"
        );

        let no_strip_config = GeneratorConfig {
            strip_module_prefix: false,
            ..Default::default()
        };
        let tmp2 = std::env::temp_dir().join("weaveffi_test_swift_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        SwiftGenerator
            .generate_with_config(&api, out_dir2, &no_strip_config)
            .unwrap();

        let swift2 =
            std::fs::read_to_string(tmp2.join("swift/Sources/WeaveFFI/WeaveFFI.swift")).unwrap();

        assert!(
            swift2.contains("func contacts_create_contact("),
            "default should use module-prefixed name: {swift2}"
        );
        assert!(
            swift2.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {swift2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn swift_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Struct("Contact".into())),
                    ))))),
                    mutable: false,
                }],
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
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let swift = render_swift_wrapper(&api, true);
        assert!(
            swift.contains("[Contact?]?"),
            "should contain deeply nested optional type: {swift}"
        );
    }

    #[test]
    fn swift_map_of_lists() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "scores".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::List(Box::new(TypeRef::I32))),
                    ),
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
        let swift = render_swift_wrapper(&api, true);
        assert!(
            swift.contains("[String: [Int32]]"),
            "should contain map of lists type: {swift}"
        );
    }

    #[test]
    fn swift_enum_keyed_map() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "contacts".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".into())),
                        Box::new(TypeRef::Struct("Contact".into())),
                    ),
                    mutable: false,
                }],
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
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
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
        let swift = render_swift_wrapper(&api, true);
        assert!(
            swift.contains("[Color: Contact]"),
            "should contain enum-keyed map type: {swift}"
        );
    }

    #[test]
    fn swift_type_for_borrowed_str() {
        assert_eq!(swift_type_for(&TypeRef::BorrowedStr), "String");
    }

    #[test]
    fn swift_type_for_borrowed_bytes() {
        assert_eq!(swift_type_for(&TypeRef::BorrowedBytes), "Data");
    }

    #[test]
    fn swift_function_with_borrowed_str_param() {
        let api = make_api(vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "write".to_string(),
                params: vec![Param {
                    name: "msg".to_string(),
                    ty: TypeRef::BorrowedStr,
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("_ msg: String"),
            "BorrowedStr param should use String type: {out}"
        );
        assert!(
            out.contains("weaveffi_io_write"),
            "should call the C function: {out}"
        );
    }

    #[test]
    fn swift_function_with_borrowed_bytes_param() {
        let api = make_api(vec![Module {
            name: "io".to_string(),
            functions: vec![Function {
                name: "upload".to_string(),
                params: vec![Param {
                    name: "data".to_string(),
                    ty: TypeRef::BorrowedBytes,
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("_ data: Data"),
            "BorrowedBytes param should use Data type: {out}"
        );
        assert!(
            out.contains("weaveffi_io_upload"),
            "should call the C function: {out}"
        );
    }

    #[test]
    fn swift_typed_handle_type() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "get_info".into(),
                params: vec![Param {
                    name: "contact".into(),
                    ty: TypeRef::TypedHandle("Contact".into()),
                    mutable: false,
                }],
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
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let swift = render_swift_wrapper(&api, true);
        assert!(
            swift.contains("_ contact: Contact"),
            "TypedHandle should use class type not UInt64: {swift}"
        );
        assert!(
            swift.contains("contact.ptr"),
            "TypedHandle should extract .ptr: {swift}"
        );
    }

    #[test]
    fn swift_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: false,
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

        let out = render_swift_wrapper(&api, true);

        assert!(
            !out.contains("weaveffi_free_string(name"),
            "borrowed string param must not be freed by the wrapper: {out}"
        );

        let fn_start = out
            .find("public static func find_contact")
            .expect("find_contact wrapper");
        let fn_body = &out[fn_start..];
        let check_pos = fn_body
            .find("try check(&err)")
            .expect("try check in find_contact");
        let contact_ptr_pos = fn_body
            .find("Contact(ptr:")
            .expect("Contact(ptr: in find_contact");
        assert!(
            check_pos < contact_ptr_pos,
            "error must be checked before wrapping the struct return: {out}"
        );

        assert!(
            out.contains("deinit") && out.contains("weaveffi_contacts_Contact_destroy(ptr)"),
            "struct return type should use a class with destroy in deinit: {out}"
        );
    }

    #[test]
    fn swift_null_check_on_optional_return() {
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("rv.map { Contact(ptr: $0) }"),
            "optional struct return should map null before wrapping: {out}"
        );
    }

    #[test]
    fn swift_async_function_signature() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("async throws"),
            "missing async throws in signature: {out}"
        );
        assert!(
            out.contains("public static func run(_ id: Int32) async throws -> Int32"),
            "missing complete async function signature: {out}"
        );
    }

    #[test]
    fn swift_async_uses_continuation() {
        let api = make_api(vec![Module {
            name: "tasks".to_string(),
            functions: vec![Function {
                name: "run".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
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

        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("withCheckedThrowingContinuation"),
            "missing withCheckedThrowingContinuation: {out}"
        );
        assert!(
            out.contains("ContinuationRef"),
            "missing ContinuationRef usage: {out}"
        );
        assert!(
            out.contains("Unmanaged"),
            "missing Unmanaged for context bridging: {out}"
        );
        assert!(
            out.contains("weaveffi_tasks_run_async"),
            "missing async C function call: {out}"
        );
    }

    #[test]
    fn swift_cross_module_struct() {
        let api = make_api(vec![
            Module {
                name: "types".to_string(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Name".to_string(),
                    doc: None,
                    fields: vec![StructField {
                        name: "value".to_string(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    }],
                    builder: false,
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            },
            Module {
                name: "ops".to_string(),
                functions: vec![Function {
                    name: "get_name".to_string(),
                    params: vec![Param {
                        name: "id".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Struct("types.Name".to_string())),
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
            },
        ]);

        let out = render_swift_wrapper(&api, true);

        assert!(
            out.contains("-> Name"),
            "cross-module return type should use local name 'Name': {out}"
        );
        assert!(
            out.contains("Name(ptr:"),
            "cross-module struct constructor should use local name 'Name': {out}"
        );
        assert!(
            !out.contains("types.Name"),
            "dot-qualified name should not appear in generated Swift code: {out}"
        );
    }

    #[test]
    fn swift_nested_module_output() {
        let api = make_api(vec![Module {
            name: "parent".to_string(),
            functions: vec![Function {
                name: "outer_fn".to_string(),
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
            modules: vec![Module {
                name: "child".to_string(),
                functions: vec![Function {
                    name: "inner_fn".to_string(),
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
        }]);
        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("public enum Parent {"),
            "top-level module enum missing: {out}"
        );
        assert!(
            out.contains("public enum Child {"),
            "nested module enum missing: {out}"
        );
        assert!(
            out.contains("weaveffi_parent_outer_fn"),
            "parent C ABI call missing: {out}"
        );
        assert!(
            out.contains("weaveffi_parent_child_inner_fn"),
            "nested child C ABI call missing: {out}"
        );
    }

    #[test]
    fn swift_type_for_iterator() {
        assert_eq!(
            swift_type_for(&TypeRef::Iterator(Box::new(TypeRef::I32))),
            "[Int32]"
        );
        assert_eq!(
            swift_type_for(&TypeRef::Iterator(Box::new(TypeRef::Struct(
                "Contact".into()
            )))),
            "[Contact]"
        );
    }

    #[test]
    fn swift_iterator_return_generates_consumption_code() {
        let api = make_api(vec![Module {
            name: "data".to_string(),
            functions: vec![Function {
                name: "list_items".to_string(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
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
        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("ListItemsIterator"),
            "should reference iterator type: {out}"
        );
        assert!(
            out.contains("_next"),
            "should call _next to consume iterator: {out}"
        );
        assert!(
            out.contains("_destroy"),
            "should call _destroy to clean up iterator: {out}"
        );
    }

    #[test]
    fn deprecated_function_generates_annotation() {
        let api = make_api(vec![Module {
            name: "math".to_string(),
            functions: vec![Function {
                name: "add_old".to_string(),
                params: vec![
                    Param {
                        name: "a".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                    Param {
                        name: "b".to_string(),
                        ty: TypeRef::I32,
                        mutable: false,
                    },
                ],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: Some("Use addV2 instead".to_string()),
                since: Some("0.1.0".to_string()),
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let out = render_swift_wrapper(&api, true);
        assert!(
            out.contains("@available(*, deprecated, message: \"Use addV2 instead\")"),
            "missing deprecation annotation: {out}"
        );
        assert!(
            out.contains("func add_old("),
            "missing function declaration: {out}"
        );
    }
}
