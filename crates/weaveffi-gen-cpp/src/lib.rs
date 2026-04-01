use anyhow::Result;
use camino::Utf8Path;
use weaveffi_core::codegen::Generator;
use weaveffi_ir::ir::{Api, ErrorCode, Function, Module, StructField, TypeRef};

pub struct CppGenerator;

impl Generator for CppGenerator {
    fn name(&self) -> &'static str {
        "cpp"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        let dir = out_dir.join("cpp");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("weaveffi.hpp"), render_cpp_header(api))?;
        Ok(())
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![out_dir.join("cpp/weaveffi.hpp").to_string()]
    }
}

fn render_cpp_header(api: &Api) -> String {
    let mut out = String::new();

    out.push_str("#pragma once\n\n");
    out.push_str("#include <cstdint>\n");
    out.push_str("#include <string>\n");
    out.push_str("#include <vector>\n");
    out.push_str("#include <optional>\n");
    out.push_str("#include <unordered_map>\n");
    out.push_str("#include <memory>\n");
    out.push_str("#include <stdexcept>\n\n");

    out.push_str("extern \"C\" {\n\n");
    render_extern_c(&mut out, api);
    out.push_str("} // extern \"C\"\n\n");

    out.push_str("namespace weaveffi {\n\n");

    let error_codes: Vec<_> = api
        .modules
        .iter()
        .filter_map(|m| m.errors.as_ref())
        .flat_map(|e| &e.codes)
        .collect();
    render_cpp_error_classes(&mut out, &error_codes);

    for module in &api.modules {
        render_cpp_enums(&mut out, module);
        render_cpp_classes(&mut out, module);
        render_cpp_functions(&mut out, module, &error_codes);
    }
    out.push_str("} // namespace weaveffi\n");

    out
}

// ── C ABI type helpers (mirrors the C generator logic) ──

fn is_c_pointer_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8
            | TypeRef::BorrowedStr
            | TypeRef::Bytes
            | TypeRef::BorrowedBytes
            | TypeRef::Struct(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::List(_)
            | TypeRef::Map(_, _)
    )
}

fn c_element_type(ty: &TypeRef, module: &str) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::Handle => "weaveffi_handle_t".into(),
        TypeRef::TypedHandle(n) => format!("weaveffi_{module}_{n}*"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "const char*".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "const uint8_t*".into(),
        TypeRef::Struct(s) => format!("weaveffi_{module}_{s}*"),
        TypeRef::Enum(e) => format!("weaveffi_{module}_{e}"),
        TypeRef::Optional(inner) | TypeRef::List(inner) => c_element_type(inner, module),
        TypeRef::Map(_, _) => "void*".into(),
    }
}

fn c_param_type(ty: &TypeRef, name: &str, module: &str) -> String {
    match ty {
        TypeRef::I32 => format!("int32_t {name}"),
        TypeRef::U32 => format!("uint32_t {name}"),
        TypeRef::I64 => format!("int64_t {name}"),
        TypeRef::F64 => format!("double {name}"),
        TypeRef::Bool => format!("bool {name}"),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("const char* {name}"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("const uint8_t* {name}_ptr, size_t {name}_len")
        }
        TypeRef::Handle => format!("weaveffi_handle_t {name}"),
        TypeRef::TypedHandle(n) => format!("weaveffi_{module}_{n}* {name}"),
        TypeRef::Struct(s) => format!("const weaveffi_{module}_{s}* {name}"),
        TypeRef::Enum(e) => format!("weaveffi_{module}_{e} {name}"),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_param_type(inner, name, module)
            } else {
                let base = c_element_type(inner, module);
                format!("const {base}* {name}")
            }
        }
        TypeRef::List(inner) => {
            let elem = c_element_type(inner, module);
            if is_c_pointer_type(inner) {
                format!("{elem} const* {name}, size_t {name}_len")
            } else {
                format!("const {elem}* {name}, size_t {name}_len")
            }
        }
        TypeRef::Map(k, v) => {
            let ke = c_element_type(k, module);
            let ve = c_element_type(v, module);
            let kp = if is_c_pointer_type(k) {
                format!("{ke} const* {name}_keys")
            } else {
                format!("const {ke}* {name}_keys")
            };
            let vp = if is_c_pointer_type(v) {
                format!("{ve} const* {name}_values")
            } else {
                format!("const {ve}* {name}_values")
            };
            format!("{kp}, {vp}, size_t {name}_len")
        }
    }
}

fn c_ret_type(ty: &TypeRef, module: &str) -> (String, Vec<String>) {
    match ty {
        TypeRef::I32 => ("int32_t".into(), vec![]),
        TypeRef::U32 => ("uint32_t".into(), vec![]),
        TypeRef::I64 => ("int64_t".into(), vec![]),
        TypeRef::F64 => ("double".into(), vec![]),
        TypeRef::Bool => ("bool".into(), vec![]),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ("const char*".into(), vec![]),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            ("const uint8_t*".into(), vec!["size_t* out_len".into()])
        }
        TypeRef::Handle => ("weaveffi_handle_t".into(), vec![]),
        TypeRef::TypedHandle(n) => (format!("weaveffi_{module}_{n}*"), vec![]),
        TypeRef::Struct(s) => (format!("weaveffi_{module}_{s}*"), vec![]),
        TypeRef::Enum(e) => (format!("weaveffi_{module}_{e}"), vec![]),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                c_ret_type(inner, module)
            } else {
                (format!("{}*", c_element_type(inner, module)), vec![])
            }
        }
        TypeRef::List(inner) => (
            format!("{}*", c_element_type(inner, module)),
            vec!["size_t* out_len".into()],
        ),
        TypeRef::Map(k, v) => (
            "void".into(),
            vec![
                format!("{}* out_keys", c_element_type(k, module)),
                format!("{}* out_values", c_element_type(v, module)),
                "size_t* out_len".into(),
            ],
        ),
    }
}

// ── extern "C" block ──

fn render_extern_c(out: &mut String, api: &Api) {
    out.push_str("typedef uint64_t weaveffi_handle_t;\n\n");
    out.push_str("typedef struct weaveffi_error {\n");
    out.push_str("    int32_t code;\n");
    out.push_str("    const char* message;\n");
    out.push_str("} weaveffi_error;\n\n");
    out.push_str("void weaveffi_error_clear(weaveffi_error* err);\n");
    out.push_str("void weaveffi_free_string(const char* ptr);\n");
    out.push_str("void weaveffi_free_bytes(uint8_t* ptr, size_t len);\n\n");

    for module in &api.modules {
        for e in &module.enums {
            let tag = format!("weaveffi_{}_{}", module.name, e.name);
            let vars: Vec<String> = e
                .variants
                .iter()
                .map(|v| format!("{tag}_{} = {}", v.name, v.value))
                .collect();
            out.push_str(&format!("typedef enum {{ {} }} {tag};\n", vars.join(", ")));
        }

        for s in &module.structs {
            let tag = format!("weaveffi_{}_{}", module.name, s.name);
            out.push_str(&format!("typedef struct {tag} {tag};\n"));

            let mut params: Vec<String> = s
                .fields
                .iter()
                .map(|f| c_param_type(&f.ty, &f.name, &module.name))
                .collect();
            params.push("weaveffi_error* out_err".into());
            out.push_str(&format!("{tag}* {tag}_create({});\n", params.join(", ")));
            out.push_str(&format!("void {tag}_destroy({tag}* ptr);\n"));

            for field in &s.fields {
                let (ret_ty, extra) = c_ret_type(&field.ty, &module.name);
                let getter = format!("{tag}_get_{}", field.name);
                if extra.is_empty() {
                    out.push_str(&format!("{ret_ty} {getter}(const {tag}* ptr);\n"));
                } else {
                    out.push_str(&format!(
                        "{ret_ty} {getter}(const {tag}* ptr, {});\n",
                        extra.join(", ")
                    ));
                }
            }
        }

        for f in &module.functions {
            let mut p: Vec<String> = f
                .params
                .iter()
                .map(|p| c_param_type(&p.ty, &p.name, &module.name))
                .collect();
            let ret = if let Some(r) = &f.returns {
                let (rt, extra) = c_ret_type(r, &module.name);
                p.extend(extra);
                rt
            } else {
                "void".into()
            };
            p.push("weaveffi_error* out_err".into());
            out.push_str(&format!(
                "{ret} weaveffi_{}_{}({});\n",
                module.name,
                f.name,
                p.join(", ")
            ));
        }

        out.push('\n');
    }
}

// ── C++ type mapping ──

fn cpp_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I32 => "int32_t".into(),
        TypeRef::U32 => "uint32_t".into(),
        TypeRef::I64 => "int64_t".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string".into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "std::vector<uint8_t>".into(),
        TypeRef::Handle => "void*".into(),
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) => n.clone(),
        TypeRef::Enum(n) => n.clone(),
        TypeRef::Optional(inner) => format!("std::optional<{}>", cpp_type(inner)),
        TypeRef::List(inner) => format!("std::vector<{}>", cpp_type(inner)),
        TypeRef::Map(k, v) => {
            format!("std::unordered_map<{}, {}>", cpp_type(k), cpp_type(v))
        }
    }
}

fn cpp_param_decl(ty: &TypeRef, name: &str) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => format!("const std::string& {name}"),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("const std::vector<uint8_t>& {name}")
        }
        TypeRef::TypedHandle(n) => format!("{n}& {name}"),
        TypeRef::Struct(n) => format!("const {n}& {name}"),
        TypeRef::Optional(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            format!("const {}& {name}", cpp_type(ty))
        }
        _ => format!("{} {name}", cpp_type(ty)),
    }
}

// ── Namespace: error classes ──

fn render_cpp_error_classes(out: &mut String, error_codes: &[&ErrorCode]) {
    out.push_str("class WeaveFFIError : public std::runtime_error {\n");
    out.push_str("    int32_t code_;\n\n");
    out.push_str("public:\n");
    out.push_str("    WeaveFFIError(int32_t code, const std::string& msg) : std::runtime_error(msg), code_(code) {}\n");
    out.push_str("    int32_t code() const { return code_; }\n");
    out.push_str("};\n\n");

    for ec in error_codes {
        out.push_str(&format!(
            "class {}Error : public WeaveFFIError {{\n",
            ec.name
        ));
        out.push_str("public:\n");
        out.push_str(&format!(
            "    {}Error(const std::string& msg) : WeaveFFIError({}, msg) {{}}\n",
            ec.name, ec.code
        ));
        out.push_str("};\n\n");
    }
}

// ── Namespace: enums ──

fn render_cpp_enums(out: &mut String, module: &Module) {
    for e in &module.enums {
        out.push_str(&format!("enum class {} : int32_t {{\n", e.name));
        for (i, v) in e.variants.iter().enumerate() {
            let comma = if i + 1 < e.variants.len() { "," } else { "" };
            out.push_str(&format!("    {} = {}{}\n", v.name, v.value, comma));
        }
        out.push_str("};\n\n");
    }
}

// ── Namespace: RAII classes ──

fn render_cpp_classes(out: &mut String, module: &Module) {
    for s in &module.structs {
        let tag = format!("weaveffi_{}_{}", module.name, s.name);
        let name = &s.name;

        out.push_str(&format!("class {name} {{\n"));
        out.push_str("    void* handle_;\n\n");
        out.push_str("public:\n");
        out.push_str(&format!(
            "    explicit {name}(void* h) : handle_(h) {{}}\n\n"
        ));

        // Destructor
        out.push_str(&format!("    ~{name}() {{\n"));
        out.push_str(&format!(
            "        if (handle_) {tag}_destroy(static_cast<{tag}*>(handle_));\n"
        ));
        out.push_str("    }\n\n");

        // Deleted copy
        out.push_str(&format!("    {name}(const {name}&) = delete;\n"));
        out.push_str(&format!(
            "    {name}& operator=(const {name}&) = delete;\n\n"
        ));

        // Move constructor
        out.push_str(&format!(
            "    {name}({name}&& other) noexcept : handle_(other.handle_) {{\n"
        ));
        out.push_str("        other.handle_ = nullptr;\n");
        out.push_str("    }\n\n");

        // Move assignment
        out.push_str(&format!(
            "    {name}& operator=({name}&& other) noexcept {{\n"
        ));
        out.push_str("        if (this != &other) {\n");
        out.push_str(&format!(
            "            if (handle_) {tag}_destroy(static_cast<{tag}*>(handle_));\n"
        ));
        out.push_str("            handle_ = other.handle_;\n");
        out.push_str("            other.handle_ = nullptr;\n");
        out.push_str("        }\n");
        out.push_str("        return *this;\n");
        out.push_str("    }\n\n");

        out.push_str("    void* handle() const { return handle_; }\n\n");

        for field in &s.fields {
            render_cpp_getter(out, name, &module.name, field);
        }

        out.push_str("};\n\n");
    }
}

fn render_cpp_getter(out: &mut String, struct_name: &str, module: &str, field: &StructField) {
    let tag = format!("weaveffi_{module}_{struct_name}");
    let getter = format!("{tag}_get_{}", field.name);
    let cast = format!("static_cast<const {tag}*>(handle_)");
    let ret_type = cpp_type(&field.ty);

    out.push_str(&format!("    {ret_type} {}() const {{\n", field.name));

    match &field.ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            out.push_str(&format!("        return {getter}({cast});\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("        const char* raw = {getter}({cast});\n"));
            out.push_str("        std::string ret(raw);\n");
            out.push_str("        weaveffi_free_string(raw);\n");
            out.push_str("        return ret;\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("        size_t len = 0;\n");
            out.push_str(&format!("        auto* raw = {getter}({cast}, &len);\n"));
            out.push_str("        return std::vector<uint8_t>(raw, raw + len);\n");
        }
        TypeRef::Handle => {
            out.push_str(&format!(
                "        return reinterpret_cast<void*>(static_cast<uintptr_t>({getter}({cast})));\n"
            ));
        }
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) => {
            out.push_str(&format!("        return {n}({getter}({cast}));\n"));
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!(
                "        return static_cast<{n}>({getter}({cast}));\n"
            ));
        }
        TypeRef::Optional(inner) => {
            render_getter_optional(out, inner, &getter, &cast);
        }
        TypeRef::List(inner) => {
            render_getter_list(out, inner, &getter, &cast);
        }
        TypeRef::Map(k, v) => {
            render_getter_map(out, k, v, &getter, &cast, module);
        }
    }

    out.push_str("    }\n\n");
}

fn render_getter_optional(out: &mut String, inner: &TypeRef, getter: &str, cast: &str) {
    out.push_str(&format!("        auto* raw = {getter}({cast});\n"));
    out.push_str("        if (!raw) return std::nullopt;\n");
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("        std::string ret(raw);\n");
            out.push_str("        weaveffi_free_string(raw);\n");
            out.push_str("        return ret;\n");
        }
        TypeRef::Struct(n) | TypeRef::TypedHandle(n) => {
            out.push_str(&format!("        return {n}(raw);\n"));
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!("        return static_cast<{n}>(*raw);\n"));
        }
        _ if !is_c_pointer_type(inner) => {
            out.push_str("        return *raw;\n");
        }
        _ => {
            out.push_str(&format!("        return {}(raw);\n", cpp_type(inner)));
        }
    }
}

fn render_getter_list(out: &mut String, inner: &TypeRef, getter: &str, cast: &str) {
    out.push_str("        size_t len = 0;\n");
    out.push_str(&format!("        auto* raw = {getter}({cast}, &len);\n"));
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("        std::vector<std::string> ret;\n");
            out.push_str("        ret.reserve(len);\n");
            out.push_str("        for (size_t i = 0; i < len; ++i) ret.emplace_back(raw[i]);\n");
            out.push_str("        return ret;\n");
        }
        TypeRef::Struct(n) => {
            out.push_str(&format!("        std::vector<{n}> ret;\n"));
            out.push_str("        ret.reserve(len);\n");
            out.push_str(&format!(
                "        for (size_t i = 0; i < len; ++i) ret.emplace_back({n}(raw[i]));\n"
            ));
            out.push_str("        return ret;\n");
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!("        std::vector<{n}> ret;\n"));
            out.push_str("        ret.reserve(len);\n");
            out.push_str(&format!(
                "        for (size_t i = 0; i < len; ++i) ret.emplace_back(static_cast<{n}>(raw[i]));\n"
            ));
            out.push_str("        return ret;\n");
        }
        _ => {
            out.push_str(&format!(
                "        return std::vector<{}>(raw, raw + len);\n",
                cpp_type(inner)
            ));
        }
    }
}

fn render_getter_map(
    out: &mut String,
    k: &TypeRef,
    v: &TypeRef,
    getter: &str,
    cast: &str,
    module: &str,
) {
    let kc = c_element_type(k, module);
    let vc = c_element_type(v, module);
    out.push_str(&format!("        {kc}* out_keys = nullptr;\n"));
    out.push_str(&format!("        {vc}* out_values = nullptr;\n"));
    out.push_str("        size_t len = 0;\n");
    out.push_str(&format!(
        "        {getter}({cast}, out_keys, out_values, &len);\n"
    ));

    let cpp_k = cpp_type(k);
    let cpp_v = cpp_type(v);
    out.push_str(&format!(
        "        std::unordered_map<{cpp_k}, {cpp_v}> ret;\n"
    ));
    out.push_str("        for (size_t i = 0; i < len; ++i) {\n");
    let ke = match k {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_keys[i])".into(),
        TypeRef::Enum(n) => format!("static_cast<{n}>(out_keys[i])"),
        _ => "out_keys[i]".into(),
    };
    let ve = match v {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_values[i])".into(),
        TypeRef::Enum(n) => format!("static_cast<{n}>(out_values[i])"),
        TypeRef::Struct(n) => format!("{n}(out_values[i])"),
        _ => "out_values[i]".into(),
    };
    out.push_str(&format!("            ret[{ke}] = {ve};\n"));
    out.push_str("        }\n");
    out.push_str("        return ret;\n");
}

// ── Namespace: free function wrappers ──

fn render_cpp_functions(out: &mut String, module: &Module, error_codes: &[&ErrorCode]) {
    for func in &module.functions {
        render_cpp_function(out, func, module, error_codes);
    }
}

/// Converts a C++ param into setup lines and C argument expressions.
fn param_to_c_args(ty: &TypeRef, name: &str, module: &str) -> (Vec<String>, Vec<String>) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            (vec![], vec![name.into()])
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => (vec![], vec![format!("{name}.c_str()")]),
        TypeRef::Bytes | TypeRef::BorrowedBytes => (
            vec![],
            vec![format!("{name}.data()"), format!("{name}.size()")],
        ),
        TypeRef::Handle => (
            vec![],
            vec![format!(
                "static_cast<weaveffi_handle_t>(reinterpret_cast<uintptr_t>({name}))"
            )],
        ),
        TypeRef::TypedHandle(n) => (
            vec![],
            vec![format!(
                "static_cast<weaveffi_{module}_{n}*>({name}.handle())"
            )],
        ),
        TypeRef::Struct(s) => (
            vec![],
            vec![format!(
                "static_cast<const weaveffi_{module}_{s}*>({name}.handle())"
            )],
        ),
        TypeRef::Enum(e) => (
            vec![],
            vec![format!(
                "static_cast<weaveffi_{module}_{e}>(static_cast<int32_t>({name}))"
            )],
        ),
        TypeRef::Optional(inner) => {
            if is_c_pointer_type(inner) {
                match inner.as_ref() {
                    TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
                        vec![],
                        vec![format!(
                            "{name}.has_value() ? {name}.value().c_str() : nullptr"
                        )],
                    ),
                    TypeRef::Struct(s) => (
                        vec![],
                        vec![format!(
                            "{name}.has_value() ? static_cast<const weaveffi_{module}_{s}*>({name}.value().handle()) : nullptr"
                        )],
                    ),
                    _ => param_to_c_args(inner, name, module),
                }
            } else {
                let c_ty = c_element_type(inner, module);
                let conv = match inner.as_ref() {
                    TypeRef::Enum(_) => {
                        format!("static_cast<{c_ty}>(static_cast<int32_t>(*{name}))")
                    }
                    _ => format!("*{name}"),
                };
                (
                    vec![
                        format!("const {c_ty}* {name}_ptr = nullptr;"),
                        format!("{c_ty} {name}_tmp{{}};"),
                        format!(
                            "if ({name}.has_value()) {{ {name}_tmp = {conv}; {name}_ptr = &{name}_tmp; }}"
                        ),
                    ],
                    vec![format!("{name}_ptr")],
                )
            }
        }
        TypeRef::List(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
                vec![
                    format!("std::vector<const char*> {name}_cstrs;"),
                    format!("{name}_cstrs.reserve({name}.size());"),
                    format!("for (const auto& s : {name}) {name}_cstrs.push_back(s.c_str());"),
                ],
                vec![
                    format!("{name}_cstrs.data()"),
                    format!("{name}_cstrs.size()"),
                ],
            ),
            TypeRef::Struct(s) => {
                let c_ptr = format!("const weaveffi_{module}_{s}*");
                (
                    vec![
                        format!("std::vector<{c_ptr}> {name}_ptrs;"),
                        format!("{name}_ptrs.reserve({name}.size());"),
                        format!(
                            "for (const auto& item : {name}) {name}_ptrs.push_back(static_cast<{c_ptr}>(item.handle()));"
                        ),
                    ],
                    vec![
                        format!("{name}_ptrs.data()"),
                        format!("{name}_ptrs.size()"),
                    ],
                )
            }
            _ => (
                vec![],
                vec![format!("{name}.data()"), format!("{name}.size()")],
            ),
        },
        TypeRef::Map(k, v) => {
            let kc = c_element_type(k, module);
            let vc = c_element_type(v, module);
            let ke = match k.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "kv.first.c_str()".into(),
                TypeRef::Enum(e) => {
                    format!("static_cast<weaveffi_{module}_{e}>(static_cast<int32_t>(kv.first))")
                }
                _ => "kv.first".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "kv.second.c_str()".into(),
                TypeRef::Enum(e) => {
                    format!("static_cast<weaveffi_{module}_{e}>(static_cast<int32_t>(kv.second))")
                }
                TypeRef::Struct(s) => {
                    format!("static_cast<const weaveffi_{module}_{s}*>(kv.second.handle())")
                }
                _ => "kv.second".into(),
            };
            (
                vec![
                    format!("std::vector<{kc}> {name}_keys_v;"),
                    format!("std::vector<{vc}> {name}_vals_v;"),
                    format!(
                        "for (const auto& kv : {name}) {{ {name}_keys_v.push_back({ke}); {name}_vals_v.push_back({ve}); }}"
                    ),
                ],
                vec![
                    format!("{name}_keys_v.data()"),
                    format!("{name}_vals_v.data()"),
                    format!("{name}_keys_v.size()"),
                ],
            )
        }
    }
}

fn render_cpp_function(
    out: &mut String,
    func: &Function,
    module: &Module,
    error_codes: &[&ErrorCode],
) {
    let cpp_ret = func
        .returns
        .as_ref()
        .map_or("void".to_string(), |ty| cpp_type(ty));
    let cpp_params: Vec<String> = func
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &p.name))
        .collect();
    let fn_name = format!("{}_{}", module.name, func.name);

    out.push_str(&format!(
        "inline {cpp_ret} {fn_name}({}) {{\n",
        cpp_params.join(", ")
    ));

    let mut setup = Vec::new();
    let mut c_args = Vec::new();
    for p in &func.params {
        let (s, a) = param_to_c_args(&p.ty, &p.name, &module.name);
        setup.extend(s);
        c_args.extend(a);
    }

    let is_void_c = func
        .returns
        .as_ref()
        .map_or(true, |r| matches!(r, TypeRef::Map(_, _)));

    if let Some(ret) = &func.returns {
        match ret {
            TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_) => {
                setup.push("size_t out_len = 0;".into());
                c_args.push("&out_len".into());
            }
            TypeRef::Map(k, v) => {
                let kc = c_element_type(k, &module.name);
                let vc = c_element_type(v, &module.name);
                setup.push(format!("{kc}* out_keys = nullptr;"));
                setup.push(format!("{vc}* out_values = nullptr;"));
                setup.push("size_t out_len = 0;".into());
                c_args.push("out_keys".into());
                c_args.push("out_values".into());
                c_args.push("&out_len".into());
            }
            _ => {}
        }
    }

    c_args.push("&err".into());

    for line in &setup {
        out.push_str(&format!("    {line}\n"));
    }
    out.push_str("    weaveffi_error err{};\n");

    let c_fn = format!("weaveffi_{}_{}", module.name, func.name);
    let args_str = c_args.join(", ");
    if is_void_c {
        out.push_str(&format!("    {c_fn}({args_str});\n"));
    } else {
        out.push_str(&format!("    auto result = {c_fn}({args_str});\n"));
    }

    out.push_str("    if (err.code != 0) {\n");
    out.push_str("        std::string msg(err.message ? err.message : \"unknown error\");\n");
    out.push_str("        int32_t code = err.code;\n");
    out.push_str("        weaveffi_error_clear(&err);\n");
    if error_codes.is_empty() {
        out.push_str("        throw WeaveFFIError(code, msg);\n");
    } else {
        out.push_str("        switch (code) {\n");
        for ec in error_codes {
            out.push_str(&format!(
                "        case {}: throw {}Error(msg);\n",
                ec.code, ec.name
            ));
        }
        out.push_str("        default: throw WeaveFFIError(code, msg);\n");
        out.push_str("        }\n");
    }
    out.push_str("    }\n");

    if let Some(ret) = &func.returns {
        render_cpp_return(out, ret);
    }

    out.push_str("}\n\n");
}

fn render_cpp_return(out: &mut String, ty: &TypeRef) {
    match ty {
        TypeRef::I32 | TypeRef::U32 | TypeRef::I64 | TypeRef::F64 | TypeRef::Bool => {
            out.push_str("    return result;\n");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str("    std::string ret(result);\n");
            out.push_str("    weaveffi_free_string(result);\n");
            out.push_str("    return ret;\n");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str("    std::vector<uint8_t> ret(result, result + out_len);\n");
            out.push_str("    weaveffi_free_bytes(const_cast<uint8_t*>(result), out_len);\n");
            out.push_str("    return ret;\n");
        }
        TypeRef::Handle => {
            out.push_str("    return reinterpret_cast<void*>(static_cast<uintptr_t>(result));\n");
        }
        TypeRef::TypedHandle(n) | TypeRef::Struct(n) => {
            out.push_str(&format!("    return {n}(result);\n"));
        }
        TypeRef::Enum(n) => {
            out.push_str(&format!("    return static_cast<{n}>(result);\n"));
        }
        TypeRef::Optional(inner) => {
            out.push_str("    if (!result) return std::nullopt;\n");
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str("    std::string ret(result);\n");
                    out.push_str("    weaveffi_free_string(result);\n");
                    out.push_str("    return ret;\n");
                }
                TypeRef::Struct(n) | TypeRef::TypedHandle(n) => {
                    out.push_str(&format!("    return {n}(result);\n"));
                }
                TypeRef::Enum(n) => {
                    out.push_str(&format!("    return static_cast<{n}>(*result);\n"));
                }
                _ if !is_c_pointer_type(inner) => {
                    out.push_str("    return *result;\n");
                }
                _ => {
                    out.push_str(&format!("    return {}(result);\n", cpp_type(inner)));
                }
            }
        }
        TypeRef::List(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                out.push_str("    std::vector<std::string> ret;\n");
                out.push_str("    ret.reserve(out_len);\n");
                out.push_str(
                    "    for (size_t i = 0; i < out_len; ++i) ret.emplace_back(result[i]);\n",
                );
                out.push_str("    return ret;\n");
            }
            TypeRef::Struct(n) => {
                out.push_str(&format!("    std::vector<{n}> ret;\n"));
                out.push_str("    ret.reserve(out_len);\n");
                out.push_str(&format!(
                    "    for (size_t i = 0; i < out_len; ++i) ret.emplace_back({n}(result[i]));\n"
                ));
                out.push_str("    return ret;\n");
            }
            TypeRef::Enum(n) => {
                out.push_str(&format!("    std::vector<{n}> ret;\n"));
                out.push_str("    ret.reserve(out_len);\n");
                out.push_str(&format!(
                    "    for (size_t i = 0; i < out_len; ++i) ret.emplace_back(static_cast<{n}>(result[i]));\n"
                ));
                out.push_str("    return ret;\n");
            }
            _ => {
                out.push_str(&format!(
                    "    return std::vector<{}>(result, result + out_len);\n",
                    cpp_type(inner)
                ));
            }
        },
        TypeRef::Map(k, v) => {
            let ck = cpp_type(k);
            let cv = cpp_type(v);
            out.push_str(&format!("    std::unordered_map<{ck}, {cv}> ret;\n"));
            out.push_str("    for (size_t i = 0; i < out_len; ++i) {\n");
            let ke = match k.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_keys[i])".into(),
                TypeRef::Enum(n) => format!("static_cast<{n}>(out_keys[i])"),
                _ => "out_keys[i]".into(),
            };
            let ve = match v.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => "std::string(out_values[i])".into(),
                TypeRef::Enum(n) => format!("static_cast<{n}>(out_values[i])"),
                TypeRef::Struct(n) => format!("{n}(out_values[i])"),
                _ => "out_values[i]".into(),
            };
            out.push_str(&format!("        ret[{ke}] = {ve};\n"));
            out.push_str("    }\n");
            out.push_str("    return ret;\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, Module, Param, StructDef,
        StructField, TypeRef,
    };

    fn minimal_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "calculator".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        }
    }

    fn contacts_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "contacts".to_string(),
                enums: vec![EnumDef {
                    name: "ContactType".to_string(),
                    doc: None,
                    variants: vec![
                        EnumVariant {
                            name: "Personal".to_string(),
                            value: 0,
                            doc: None,
                        },
                        EnumVariant {
                            name: "Work".to_string(),
                            value: 1,
                            doc: None,
                        },
                    ],
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
                            name: "age".to_string(),
                            ty: TypeRef::I32,
                            doc: None,
                        },
                        StructField {
                            name: "email".to_string(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            doc: None,
                        },
                        StructField {
                            name: "contact_type".to_string(),
                            ty: TypeRef::Enum("ContactType".to_string()),
                            doc: None,
                        },
                    ],
                }],
                functions: vec![
                    Function {
                        name: "get_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                        }],
                        returns: Some(TypeRef::Struct("Contact".to_string())),
                        doc: None,
                        r#async: false,
                    },
                    Function {
                        name: "delete_contact".to_string(),
                        params: vec![Param {
                            name: "id".to_string(),
                            ty: TypeRef::Handle,
                        }],
                        returns: None,
                        doc: None,
                        r#async: false,
                    },
                ],
                errors: None,
            }],
        }
    }

    #[test]
    fn name_returns_cpp() {
        assert_eq!(CppGenerator.name(), "cpp");
    }

    #[test]
    fn output_files_lists_hpp() {
        let api = minimal_api();
        let out_dir = Utf8Path::new("/tmp/out");
        let files = CppGenerator.output_files(&api, out_dir);
        assert_eq!(files, vec!["/tmp/out/cpp/weaveffi.hpp"]);
    }

    #[test]
    fn generate_creates_hpp_file() {
        let api = minimal_api();
        let tmp = std::env::temp_dir().join("weaveffi_test_cpp_gen");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("temp dir is valid UTF-8");

        CppGenerator.generate(&api, out_dir).unwrap();

        let hpp = tmp.join("cpp").join("weaveffi.hpp");
        assert!(hpp.exists(), "weaveffi.hpp should be created");

        let content = std::fs::read_to_string(&hpp).unwrap();
        assert!(content.contains("#pragma once"), "missing pragma once");
        assert!(
            content.contains("#include <cstdint>"),
            "missing cstdint include"
        );
        assert!(content.contains("extern \"C\""), "missing extern C block");
        assert!(content.contains("namespace weaveffi"), "missing namespace");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn header_includes() {
        let h = render_cpp_header(&minimal_api());
        for inc in [
            "<cstdint>",
            "<string>",
            "<vector>",
            "<optional>",
            "<unordered_map>",
            "<memory>",
            "<stdexcept>",
        ] {
            assert!(
                h.contains(&format!("#include {inc}")),
                "missing include {inc}"
            );
        }
    }

    #[test]
    fn extern_c_common_declarations() {
        let h = render_cpp_header(&minimal_api());
        assert!(
            h.contains("typedef uint64_t weaveffi_handle_t;"),
            "missing handle_t typedef"
        );
        assert!(
            h.contains("typedef struct weaveffi_error"),
            "missing error struct"
        );
        assert!(
            h.contains("void weaveffi_error_clear(weaveffi_error* err);"),
            "missing error_clear"
        );
        assert!(
            h.contains("void weaveffi_free_string(const char* ptr);"),
            "missing free_string"
        );
        assert!(
            h.contains("void weaveffi_free_bytes(uint8_t* ptr, size_t len);"),
            "missing free_bytes"
        );
    }

    #[test]
    fn extern_c_function_declarations() {
        let h = render_cpp_header(&minimal_api());
        assert!(
            h.contains(
                "int32_t weaveffi_calculator_add(int32_t a, int32_t b, weaveffi_error* out_err);"
            ),
            "missing add declaration: {h}"
        );
    }

    #[test]
    fn extern_c_enum_declarations() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("weaveffi_contacts_ContactType_Personal = 0"),
            "missing enum variant: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_ContactType_Work = 1"),
            "missing enum variant: {h}"
        );
        assert!(
            h.contains("} weaveffi_contacts_ContactType;"),
            "missing enum typedef: {h}"
        );
    }

    #[test]
    fn extern_c_struct_declarations() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"),
            "missing opaque struct: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact* weaveffi_contacts_Contact_create("),
            "missing create: {h}"
        );
        assert!(
            h.contains("void weaveffi_contacts_Contact_destroy(weaveffi_contacts_Contact* ptr);"),
            "missing destroy: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact_get_name("),
            "missing name getter: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact_get_age("),
            "missing age getter: {h}"
        );
    }

    #[test]
    fn cpp_enum_class() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("enum class ContactType : int32_t {"),
            "missing enum class: {h}"
        );
        assert!(h.contains("Personal = 0,"), "missing Personal variant: {h}");
        assert!(h.contains("Work = 1"), "missing Work variant: {h}");
    }

    #[test]
    fn cpp_raii_class_structure() {
        let h = render_cpp_header(&contacts_api());
        assert!(h.contains("class Contact {"), "missing class: {h}");
        assert!(h.contains("void* handle_;"), "missing handle member: {h}");
        assert!(
            h.contains("explicit Contact(void* h) : handle_(h) {}"),
            "missing constructor: {h}"
        );
        assert!(h.contains("~Contact()"), "missing destructor: {h}");
        assert!(
            h.contains("weaveffi_contacts_Contact_destroy(static_cast<weaveffi_contacts_Contact*>(handle_))"),
            "destructor should call destroy: {h}"
        );
        assert!(
            h.contains("Contact(const Contact&) = delete;"),
            "missing deleted copy ctor: {h}"
        );
        assert!(
            h.contains("Contact& operator=(const Contact&) = delete;"),
            "missing deleted copy assign: {h}"
        );
        assert!(
            h.contains("Contact(Contact&& other) noexcept"),
            "missing move ctor: {h}"
        );
        assert!(
            h.contains("Contact& operator=(Contact&& other) noexcept"),
            "missing move assign: {h}"
        );
        assert!(
            h.contains("other.handle_ = nullptr;"),
            "move should null source: {h}"
        );
    }

    #[test]
    fn cpp_string_getter() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("std::string name() const {"),
            "missing string getter: {h}"
        );
        assert!(
            h.contains("weaveffi_contacts_Contact_get_name(static_cast<const weaveffi_contacts_Contact*>(handle_))"),
            "getter should call C function with cast: {h}"
        );
        assert!(
            h.contains("weaveffi_free_string(raw)"),
            "string getter should free: {h}"
        );
    }

    #[test]
    fn cpp_i32_getter() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("int32_t age() const {"),
            "missing i32 getter: {h}"
        );
    }

    #[test]
    fn cpp_optional_string_getter() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("std::optional<std::string> email() const {"),
            "missing optional string getter: {h}"
        );
        assert!(
            h.contains("if (!raw) return std::nullopt;"),
            "should check null for optional: {h}"
        );
    }

    #[test]
    fn cpp_enum_getter() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("ContactType contact_type() const {"),
            "missing enum getter: {h}"
        );
        assert!(
            h.contains("static_cast<ContactType>("),
            "enum getter should cast: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_function_scalar() {
        let h = render_cpp_header(&minimal_api());
        assert!(
            h.contains("inline int32_t calculator_add(int32_t a, int32_t b) {"),
            "missing wrapper function: {h}"
        );
        assert!(
            h.contains("weaveffi_calculator_add(a, b, &err)"),
            "should call C function: {h}"
        );
        assert!(
            h.contains("throw WeaveFFIError(code, msg)"),
            "should throw on error: {h}"
        );
        assert!(h.contains("return result;"), "should return result: {h}");
    }

    #[test]
    fn cpp_wrapper_function_struct_return() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("inline Contact contacts_get_contact(void* id) {"),
            "missing struct-returning function: {h}"
        );
        assert!(
            h.contains("return Contact(result);"),
            "should construct and return class: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_function_void_return() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("inline void contacts_delete_contact(void* id) {"),
            "missing void function: {h}"
        );
        let void_fn_start = h.find("contacts_delete_contact").unwrap();
        let void_fn = &h[void_fn_start..void_fn_start + 300];
        assert!(
            !void_fn.contains("return"),
            "void function should not return: {void_fn}"
        );
    }

    #[test]
    fn cpp_wrapper_handle_param_conversion() {
        let h = render_cpp_header(&contacts_api());
        assert!(
            h.contains("static_cast<weaveffi_handle_t>(reinterpret_cast<uintptr_t>(id))"),
            "should convert void* to handle_t: {h}"
        );
    }

    #[test]
    fn cpp_wrapper_error_handling() {
        let h = render_cpp_header(&minimal_api());
        assert!(
            h.contains("weaveffi_error err{};"),
            "should declare error: {h}"
        );
        assert!(h.contains("if (err.code != 0)"), "should check error: {h}");
        assert!(
            h.contains("weaveffi_error_clear(&err)"),
            "should clear error: {h}"
        );
    }

    #[test]
    fn cpp_string_param_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "echo".into(),
                    params: vec![Param {
                        name: "msg".into(),
                        ty: TypeRef::StringUtf8,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline std::string io_echo(const std::string& msg)"),
            "string param should be const ref: {h}"
        );
        assert!(h.contains("msg.c_str()"), "should pass c_str: {h}");
        assert!(
            h.contains("weaveffi_free_string(result)"),
            "should free returned string: {h}"
        );
    }

    #[test]
    fn cpp_list_return_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "list_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline std::vector<int32_t> store_list_ids()"),
            "missing list return function: {h}"
        );
        assert!(
            h.contains("size_t out_len = 0;"),
            "should declare out_len: {h}"
        );
        assert!(
            h.contains("result, result + out_len"),
            "should build vector from range: {h}"
        );
    }

    #[test]
    fn cpp_optional_i32_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I32,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline std::optional<int32_t> store_find(int32_t id)"),
            "missing optional return function: {h}"
        );
        assert!(
            h.contains("if (!result) return std::nullopt;"),
            "should null check: {h}"
        );
        assert!(h.contains("return *result;"), "should dereference: {h}");
    }

    #[test]
    fn cpp_enum_param_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "paint".into(),
                functions: vec![Function {
                    name: "mix".into(),
                    params: vec![Param {
                        name: "color".into(),
                        ty: TypeRef::Enum("Color".into()),
                    }],
                    returns: Some(TypeRef::Enum("Color".into())),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
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
                    ],
                }],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline Color paint_mix(Color color)"),
            "missing enum function: {h}"
        );
        assert!(
            h.contains("static_cast<weaveffi_paint_Color>(static_cast<int32_t>(color))"),
            "should double-cast enum param: {h}"
        );
        assert!(
            h.contains("return static_cast<Color>(result);"),
            "should cast return to enum class: {h}"
        );
    }

    #[test]
    fn cpp_list_struct_return() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "list_all".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    }],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline std::vector<Contact> contacts_list_all()"),
            "missing list struct return: {h}"
        );
        assert!(
            h.contains("ret.emplace_back(Contact(result[i]))"),
            "should construct each element: {h}"
        );
    }

    #[test]
    fn cpp_map_return_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "store".into(),
                functions: vec![Function {
                    name: "get_scores".into(),
                    params: vec![],
                    returns: Some(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::I32),
                    )),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline std::unordered_map<std::string, int32_t> store_get_scores()"),
            "missing map return function: {h}"
        );
        assert!(
            h.contains("std::string(out_keys[i])"),
            "should convert string keys: {h}"
        );
    }

    #[test]
    fn cpp_struct_getter_list() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "m".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Data".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "scores".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        doc: None,
                    }],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("std::vector<int32_t> scores() const {"),
            "missing list getter: {h}"
        );
    }

    #[test]
    fn cpp_struct_getter_map() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "m".into(),
                functions: vec![],
                structs: vec![StructDef {
                    name: "Data".into(),
                    doc: None,
                    fields: vec![StructField {
                        name: "tags".into(),
                        ty: TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
                        doc: None,
                    }],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("std::unordered_map<std::string, int32_t> tags() const {"),
            "missing map getter: {h}"
        );
    }

    #[test]
    fn cpp_type_mapping() {
        assert_eq!(cpp_type(&TypeRef::I32), "int32_t");
        assert_eq!(cpp_type(&TypeRef::U32), "uint32_t");
        assert_eq!(cpp_type(&TypeRef::I64), "int64_t");
        assert_eq!(cpp_type(&TypeRef::F64), "double");
        assert_eq!(cpp_type(&TypeRef::Bool), "bool");
        assert_eq!(cpp_type(&TypeRef::StringUtf8), "std::string");
        assert_eq!(cpp_type(&TypeRef::Bytes), "std::vector<uint8_t>");
        assert_eq!(cpp_type(&TypeRef::Handle), "void*");
        assert_eq!(cpp_type(&TypeRef::TypedHandle("Session".into())), "Session");
        assert_eq!(cpp_type(&TypeRef::Struct("Contact".into())), "Contact");
        assert_eq!(cpp_type(&TypeRef::Enum("Color".into())), "Color");
        assert_eq!(
            cpp_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "std::optional<int32_t>"
        );
        assert_eq!(
            cpp_type(&TypeRef::List(Box::new(TypeRef::I32))),
            "std::vector<int32_t>"
        );
        assert_eq!(
            cpp_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "std::unordered_map<std::string, int32_t>"
        );
    }

    #[test]
    fn cpp_namespace_wrapping() {
        let h = render_cpp_header(&minimal_api());
        let ns_open = h.find("namespace weaveffi {").unwrap();
        let ns_close = h.find("} // namespace weaveffi").unwrap();
        let fn_pos = h.find("inline int32_t calculator_add").unwrap();
        assert!(
            fn_pos > ns_open && fn_pos < ns_close,
            "functions should be inside namespace"
        );
    }

    #[test]
    fn cpp_extern_c_wrapping() {
        let h = render_cpp_header(&minimal_api());
        let ext_open = h.find("extern \"C\" {").unwrap();
        let ext_close = h.find("} // extern \"C\"").unwrap();
        let c_fn = h.find("weaveffi_calculator_add(").unwrap();
        assert!(
            c_fn > ext_open && c_fn < ext_close,
            "C declarations should be inside extern C"
        );
    }

    #[test]
    fn cpp_bytes_return_function() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "io".into(),
                functions: vec![Function {
                    name: "read".into(),
                    params: vec![],
                    returns: Some(TypeRef::Bytes),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline std::vector<uint8_t> io_read()"),
            "missing bytes return function: {h}"
        );
        assert!(h.contains("weaveffi_free_bytes("), "should free bytes: {h}");
    }

    #[test]
    fn cpp_typed_handle_param() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "db".into(),
                functions: vec![Function {
                    name: "query".into(),
                    params: vec![Param {
                        name: "conn".into(),
                        ty: TypeRef::TypedHandle("Connection".into()),
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![StructDef {
                    name: "Connection".into(),
                    doc: None,
                    fields: vec![],
                }],
                enums: vec![],
                errors: None,
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("inline int32_t db_query(Connection& conn)"),
            "TypedHandle param should be ref: {h}"
        );
        assert!(h.contains("conn.handle()"), "should extract handle: {h}");
    }

    #[test]
    fn cpp_has_error_class() {
        let h = render_cpp_header(&minimal_api());
        assert!(
            h.contains("class WeaveFFIError : public std::runtime_error"),
            "missing WeaveFFIError class: {h}"
        );
        assert!(h.contains("int32_t code_"), "missing code_ member: {h}");
        assert!(
            h.contains("WeaveFFIError(int32_t code, const std::string& msg) : std::runtime_error(msg), code_(code) {}"),
            "missing constructor: {h}"
        );
        assert!(
            h.contains("int32_t code() const { return code_; }"),
            "missing code() getter: {h}"
        );
    }

    #[test]
    fn cpp_error_domains_generate_subclasses() {
        let api = Api {
            version: "0.1.0".into(),
            modules: vec![Module {
                name: "auth".into(),
                functions: vec![Function {
                    name: "login".into(),
                    params: vec![Param {
                        name: "user".into(),
                        ty: TypeRef::StringUtf8,
                    }],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: Some(ErrorDomain {
                    name: "AuthError".into(),
                    codes: vec![
                        ErrorCode {
                            name: "NotFound".into(),
                            code: 1,
                            message: "not found".into(),
                        },
                        ErrorCode {
                            name: "InvalidCredentials".into(),
                            code: 2,
                            message: "invalid credentials".into(),
                        },
                    ],
                }),
            }],
        };
        let h = render_cpp_header(&api);
        assert!(
            h.contains("class NotFoundError : public WeaveFFIError"),
            "missing NotFoundError subclass: {h}"
        );
        assert!(
            h.contains("class InvalidCredentialsError : public WeaveFFIError"),
            "missing InvalidCredentialsError subclass: {h}"
        );
        assert!(
            h.contains("case 1: throw NotFoundError(msg);"),
            "missing NotFound throw case: {h}"
        );
        assert!(
            h.contains("case 2: throw InvalidCredentialsError(msg);"),
            "missing InvalidCredentials throw case: {h}"
        );
        assert!(
            h.contains("default: throw WeaveFFIError(code, msg);"),
            "missing default throw case: {h}"
        );
    }
}
