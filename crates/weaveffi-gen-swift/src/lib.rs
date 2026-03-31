use anyhow::Result;
use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::Generator;
use weaveffi_core::utils::c_symbol_name;
use weaveffi_ir::ir::{Api, EnumDef, Function, Param, StructDef, StructField, TypeRef};

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
        TypeRef::Struct(name) | TypeRef::Enum(name) => name.clone(),
        TypeRef::Optional(inner) => format!("{}?", swift_type_for(inner)),
        TypeRef::List(inner) => format!("[{}]", swift_type_for(inner)),
        TypeRef::Map(k, v) => format!("[{}: {}]", swift_type_for(k), swift_type_for(v)),
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
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::List(_) => true,
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

fn render_swift_wrapper(api: &Api) -> String {
    let mut out = String::new();
    out.push_str("import CWeaveFFI\nimport Foundation\n\n");

    out.push_str("public enum WeaveFFIError: Error, CustomStringConvertible {\n");
    out.push_str("    case error(code: Int32, message: String)\n");
    out.push_str("    public var description: String {\n");
    out.push_str("        switch self { case let .error(code, message): return \"(\\(code)) \\(message)\" }\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc check(_ err: inout weaveffi_error) throws {\n");
    out.push_str("    if err.code != 0 {\n");
    out.push_str("        let code = err.code\n");
    out.push_str("        let message = err.message.flatMap { String(cString: $0) } ?? \"\"\n");
    out.push_str("        weaveffi_error_clear(&err)\n");
    out.push_str("        throw WeaveFFIError.error(code: code, message: message)\n");
    out.push_str("    }\n");
    out.push_str("}\n\n");

    out.push_str("@inline(__always)\nfunc withOptionalPointer<T, R>(to value: T?, _ body: (UnsafePointer<T>?) throws -> R) rethrows -> R {\n");
    out.push_str("    guard let value = value else { return try body(nil) }\n");
    out.push_str("    return try withUnsafePointer(to: value) { try body($0) }\n");
    out.push_str("}\n\n");

    for m in &api.modules {
        for e in &m.enums {
            render_swift_enum(&mut out, e);
        }
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
    let call_args = build_c_call_args(&f.params, module_name);
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

fn build_c_call_args(params: &[Param], module_name: &str) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::Bytes => {
                args.push(format!("{}_ptr", p.name));
                args.push(format!("{}_len", p.name));
            }
            TypeRef::Struct(_) => args.push(format!("{}.ptr", p.name)),
            TypeRef::Enum(enum_name) => args.push(format!(
                "weaveffi_{}_{}({}.rawValue)",
                module_name, enum_name, p.name
            )),
            TypeRef::Optional(inner) => match inner.as_ref() {
                TypeRef::Struct(_) => args.push(format!("{}?.ptr", p.name)),
                TypeRef::StringUtf8 | TypeRef::Bytes => {
                    args.push(format!("{}_ptr", p.name));
                    args.push(format!("{}_len", p.name));
                }
                _ => args.push(format!("{}_ptr", p.name)),
            },
            TypeRef::List(_) => {
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
        Some(TypeRef::Struct(name)) => {
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

fn render_buffered_call(out: &mut String, f: &Function, params: &[Param], module_name: &str) {
    for p in params {
        match &p.ty {
            TypeRef::StringUtf8 => {
                out.push_str(&format!(
                    "        let {n}_bytes = Array({n}.utf8)\n",
                    n = p.name
                ));
            }
            TypeRef::Bytes => {
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
                TypeRef::Struct(_) => {
                    out.push_str(&format!(
                        "        let {n}_ptrs = {n}.map {{ $0.ptr }}\n",
                        n = p.name
                    ));
                }
                _ => {}
            },
            _ => {}
        }
    }

    let closure_params: Vec<&Param> = params.iter().filter(|p| needs_closure(&p.ty)).collect();

    let is_list_return = matches!(f.returns.as_ref(), Some(TypeRef::List(_)));
    if is_list_return {
        out.push_str("        var outLen: Int = 0\n");
    }

    let handles_return_inside = matches!(
        f.returns.as_ref(),
        Some(TypeRef::StringUtf8)
            | Some(TypeRef::Enum(_))
            | Some(TypeRef::Optional(_))
            | Some(TypeRef::List(_))
    );

    let ret_type = match &f.returns {
        Some(TypeRef::Struct(_)) => "OpaquePointer?".to_string(),
        Some(ty) => swift_type_for(ty),
        None => "Void".to_string(),
    };
    let needs_return = f.returns.is_some();

    for (i, p) in closure_params.iter().enumerate() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        match &p.ty {
            TypeRef::StringUtf8 | TypeRef::Bytes => {
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
            TypeRef::Optional(inner) if is_c_value_type(inner) => {
                let source = if matches!(inner.as_ref(), TypeRef::Enum(_)) {
                    format!("{}_c", p.name)
                } else {
                    p.name.clone()
                };
                if needs_return && i == 0 {
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
            }
            TypeRef::List(inner) => {
                let source = match inner.as_ref() {
                    TypeRef::Enum(_) => format!("{}_raw", p.name),
                    TypeRef::Struct(_) => format!("{}_ptrs", p.name),
                    _ => p.name.clone(),
                };
                if needs_return && i == 0 {
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
            }
            _ => unreachable!(),
        }
    }

    let inner_indent = "        ".to_string() + &"    ".repeat(closure_params.len());
    let c_sym = c_symbol_name(module_name, &f.name);
    let call_args = build_c_call_args(params, module_name);
    let call_with_err = if is_list_return {
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
        Some(_) => {
            out.push_str(&format!("{}    return {}\n", inner_indent, call_with_err));
        }
    }

    for i in (0..closure_params.len()).rev() {
        let indent = "        ".to_string() + &"    ".repeat(i);
        out.push_str(&format!("{}}}\n", indent));
    }

    if f.returns.is_none() {
        out.push_str("        try check(&err)\n");
    } else if let Some(TypeRef::Struct(name)) = &f.returns {
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

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

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
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
            }],
            structs: vec![],
            enums: vec![],
            errors: None,
        }]);

        let out = render_swift_wrapper(&api);
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
        let out = render_swift_wrapper(&api);
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

    #[test]
    fn generate_swift_with_structs_and_enums() {
        let api = make_api(vec![Module {
            name: "contacts".to_string(),
            functions: vec![Function {
                name: "get_contact".to_string(),
                params: vec![Param {
                    name: "id".to_string(),
                    ty: TypeRef::I32,
                }],
                returns: Some(TypeRef::Struct("Contact".into())),
                doc: None,
                r#async: false,
            }],
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
                        name: "email".to_string(),
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
            errors: None,
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
}
