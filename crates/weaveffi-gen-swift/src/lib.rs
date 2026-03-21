use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, Function, Param, StructDef, StructField, TypeRef};

pub struct SwiftGenerator;

impl Generator for SwiftGenerator {
    fn name(&self) -> &'static str {
        "swift"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("swift");
        let module_dir = dir.join("CWeaveFFI");
        std::fs::create_dir_all(&module_dir)?;

        let package = r#"// swift-tools-version:5.7
import PackageDescription

let package = Package(
    name: "WeaveFFI",
    products: [
        .library(name: "WeaveFFI", targets: ["WeaveFFI"]),
    ],
    targets: [
        .systemLibrary(name: "CWeaveFFI"),
        .target(name: "WeaveFFI", dependencies: ["CWeaveFFI"]),
    ]
)
"#;
        std::fs::write(dir.join("Package.swift"), package)?;

        let modulemap = "module CWeaveFFI [system] {\n  header \"../../c/weaveffi.h\"\n  link \"weaveffi\"\n  export *\n}\n";
        std::fs::write(module_dir.join("module.modulemap"), modulemap)?;

        let src_dir = dir.join("Sources").join("WeaveFFI");
        std::fs::create_dir_all(&src_dir)?;
        std::fs::write(src_dir.join("WeaveFFI.swift"), render_swift_wrapper(api))?;
        Ok(())
    }
}

fn swift_type_for(t: &TypeRef) -> String {
    match t {
        TypeRef::I32 => "Int32".to_string(),
        TypeRef::U32 => "UInt32".to_string(),
        TypeRef::I64 => "Int64".to_string(),
        TypeRef::F64 => "Double".to_string(),
        TypeRef::Bool => "Bool".to_string(),
        TypeRef::StringUtf8 => "String".to_string(),
        TypeRef::Bytes => "Data".to_string(),
        TypeRef::Handle => "UInt64".to_string(),
        TypeRef::Struct(name) => name.clone(),
        TypeRef::Enum(_) => todo!("enum codegen"),
        TypeRef::Optional(_) => todo!("optional codegen"),
        TypeRef::List(_) => todo!("list codegen"),
    }
}

fn has_buffer_params(params: &[Param]) -> bool {
    params
        .iter()
        .any(|p| matches!(p.ty, TypeRef::StringUtf8 | TypeRef::Bytes))
}

fn render_swift_wrapper(api: &Api) -> String {
    let mut out = String::new();
    out.push_str("import CWeaveFFI\nimport Foundation\n\n");

    // Error type
    out.push_str("public enum WeaveFFIError: Error, CustomStringConvertible {\n");
    out.push_str("    case error(code: Int32, message: String)\n");
    out.push_str("    public var description: String {\n");
    out.push_str("        switch self { case let .error(code, message): return \"(\\(code)) \\(message)\" }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    // Error check helper
    out.push_str("@inline(__always)\nfunc check(_ err: inout weaveffi_error) throws {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    out.push_str("        throw WeaveFFIError.error(code: code, message: message)\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    for m in &api.modules {
        for s in &m.structs {
            render_swift_struct(&mut out, &m.name, s);
        }
        let type_name = m.name.to_upper_camel_case();
        out.push_str(&format!("public enum {} {{\n", type_name));
        for f in &m.functions {
            render_swift_function(&mut out, &m.name, f);
        }
        out.push_str("}\n\n");
    }
    out
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
            out.push_str(&format!("        return {}(ptr: {}(ptr)!)\n", name, getter));
        }
        _ => {
            out.push_str(&format!("        return {}(ptr)\n", getter));
        }
    }

    out.push_str("    }\n");
}

fn render_swift_function(out: &mut String, module_name: &str, f: &Function) {
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
        f.name,
        params_sig.join(", "),
        ret_swift
    ));
    out.push_str("        var err = weaveffi_error(code: 0, message: nil)\n");

    let c_sym = c_symbol_name(module_name, &f.name);
    let call_args = build_c_call_args(&f.params);
    let call_with_err = if call_args.is_empty() {
        format!("{}(&err)", c_sym)
    } else {
        format!("{}({}, &err)", c_sym, call_args)
    };

    if !has_buffer_params(&f.params) {
        render_direct_call(out, f, &call_with_err);
    } else {
        render_buffered_call(out, f, &f.params, module_name);
    }

    out.push_str("    }\n");
}

fn build_c_call_args(params: &[Param]) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in params {
        match p.ty {
            TypeRef::StringUtf8 | TypeRef::Bytes => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            TypeRef::Struct(_) => args.push(format!("{}.ptr", p.name)),
            _ => args.push(p.name.clone()),
        }
    }
    args.join(", ")
}

fn render_direct_call(out: &mut String, f: &Function, call_with_err: &str) {
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
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        guard let rv = rv else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
            out.push_str(&format!("        return {}(ptr: rv)\n", name));
        }
        Some(_) => {
            out.push_str(&format!("        let rv = {}\n", call_with_err));
            out.push_str("        try check(&err)\n");
            out.push_str("        return rv\n");
        }
    }
}

fn render_buffered_call(out: &mut String, f: &Function, params: &[Param], module_name: &str) {
    // For params that need buffer access, we use Array + withUnsafeBufferPointer.
    // We generate nested closures to keep pointers valid during the FFI call.
    for p in params {
        match p.ty {
            TypeRef::StringUtf8 => {
                out.push_str(&format!(
                    "        let {n}_bytes = Array({n}.utf8)\n",
                    n = p.name
                ));
            }
            TypeRef::Bytes => {
                out.push_str(&format!("        let {n}_bytes = Array({n})\n", n = p.name));
            }
            _ => {}
        }
    }

    // Build the nested withUnsafeBufferPointer calls
    let buffer_params: Vec<&Param> = params
        .iter()
        .filter(|p| matches!(p.ty, TypeRef::StringUtf8 | TypeRef::Bytes))
        .collect();

    let ret_type = match &f.returns {
        Some(TypeRef::Struct(_)) => "OpaquePointer?".to_string(),
        Some(ty) => swift_type_for(ty),
        None => "Void".to_string(),
    };
    let needs_return = f.returns.is_some();

    // Start nested closures
    for (i, p) in buffer_params.iter().enumerate() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        if needs_return && i == 0 {
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
    }

    // The FFI call at the innermost level
    let inner_indent = "        ".to_string() + &"    ".repeat(buffer_params.len());
    let c_sym = c_symbol_name(module_name, &f.name);
    let call_args = build_c_call_args(params);
    let call_with_err = if call_args.is_empty() {
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
        Some(_) => {
            out.push_str(&format!("{}    return {}\n", inner_indent, call_with_err));
        }
    }

    // Close nested closures
    for i in (0..buffer_params.len()).rev() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        out.push_str(&format!("{}}}\n", indent));
    }

    // After closures: check error and return
    if f.returns.is_none() {
        out.push_str("        try check(&err)\n");
    } else if let Some(TypeRef::Struct(name)) = &f.returns {
        out.push_str("        try check(&err)\n");
        out.push_str("        guard let result = result else { throw WeaveFFIError.error(code: -1, message: \"null pointer\") }\n");
        out.push_str(&format!("        return {}(ptr: result)\n", name));
    } else if !matches!(f.returns, Some(TypeRef::StringUtf8)) {
        out.push_str("        try check(&err)\n");
        out.push_str("        return result\n");
    } else {
        out.push_str("        return result\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Function, Module, Param, StructDef, StructField};

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules,
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
                    },
                    StructField {
                        name: "age".to_string(),
                        ty: TypeRef::I32,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
    fn render_function_returning_struct() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "create".to_string(),
                params: vec![Param {
                    name: "age".to_string(),
                    ty: TypeRef::I32,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
                }],
                returns: None,
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
                }],
            }],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
                }],
            }],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
        assert!(
            out.contains("-> Contact {"),
            "missing struct return type with buffer params: {out}"
        );
        assert!(
            out.contains("Contact(ptr: result)"),
            "missing struct wrapping after buffered call: {out}"
        );
    }
}
