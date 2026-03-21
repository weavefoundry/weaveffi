use anyhow::Result;
use camino::Utf8Path;
use heck::ToUpperCamelCase;
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, Function, Param, TypeRef};

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

fn swift_type_for(t: &TypeRef) -> &'static str {
    match t {
        TypeRef::I32 => "Int32",
        TypeRef::U32 => "UInt32",
        TypeRef::I64 => "Int64",
        TypeRef::F64 => "Double",
        TypeRef::Bool => "Bool",
        TypeRef::StringUtf8 => "String",
        TypeRef::Bytes => "Data",
        TypeRef::Handle => "UInt64",
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
        let type_name = m.name.to_upper_camel_case();
        out.push_str(&format!("public enum {} {{\n", type_name));
        for f in &m.functions {
            render_swift_function(&mut out, &m.name, f);
        }
        out.push_str("}\n\n");
    }
    out
}

fn render_swift_function(out: &mut String, module_name: &str, f: &Function) {
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("_ {}: {}", p.name, swift_type_for(&p.ty)))
        .collect();
    let ret_swift = f.returns.as_ref().map(swift_type_for).unwrap_or("Void");
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

    let ret_type = f.returns.as_ref().map(swift_type_for).unwrap_or("Void");
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
    } else if !matches!(f.returns, Some(TypeRef::StringUtf8)) {
        out.push_str("        try check(&err)\n");
        out.push_str("        return result\n");
    } else {
        out.push_str("        return result\n");
    }
}
