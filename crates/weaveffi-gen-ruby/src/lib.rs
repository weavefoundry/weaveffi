use anyhow::Result;
use camino::Utf8Path;
use heck::{ToShoutySnakeCase, ToSnakeCase};
use weaveffi_core::codegen::Generator;
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::{c_symbol_name, local_type_name};
use weaveffi_ir::ir::{Api, EnumDef, Function, Module, StructDef, StructField, TypeRef};

pub struct RubyGenerator;

impl RubyGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        module_name: &str,
        gem_name: &str,
    ) -> Result<()> {
        let dir = out_dir.join("ruby");
        let lib_dir = dir.join("lib");
        std::fs::create_dir_all(&lib_dir)?;
        std::fs::write(
            lib_dir.join("weaveffi.rb"),
            render_ruby_module(api, module_name),
        )?;
        std::fs::write(dir.join("weaveffi.gemspec"), render_gemspec(gem_name))?;
        std::fs::write(dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for RubyGenerator {
    fn name(&self) -> &'static str {
        "ruby"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "WeaveFFI", "weaveffi")
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
            config.ruby_module_name(),
            config.ruby_gem_name(),
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("ruby/lib/weaveffi.rb").to_string(),
            out_dir.join("ruby/weaveffi.gemspec").to_string(),
            out_dir.join("ruby/README.md").to_string(),
        ]
    }
}

// ── Type helpers ──

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
            | TypeRef::Iterator(_)
    )
}

fn rb_ffi_scalar(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => ":int32",
        TypeRef::U32 => ":uint32",
        TypeRef::I64 => ":int64",
        TypeRef::F64 => ":double",
        TypeRef::Handle => ":uint64",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ":string",
        _ => ":pointer",
    }
}

fn rb_param_ffi_types(ty: &TypeRef) -> Vec<&'static str> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => vec![":pointer", ":size_t"],
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => vec![":pointer"],
        TypeRef::Optional(inner) => rb_param_ffi_types(inner),
        TypeRef::List(_) => vec![":pointer", ":size_t"],
        TypeRef::Map(_, _) => vec![":pointer", ":pointer", ":size_t"],
        _ => vec![rb_ffi_scalar(ty)],
    }
}

fn rb_return_ffi_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => ":pointer",
        TypeRef::Bytes | TypeRef::BorrowedBytes => ":pointer",
        TypeRef::Optional(inner) if is_c_pointer_type(inner) => rb_return_ffi_type(inner),
        TypeRef::Optional(_) => ":pointer",
        TypeRef::List(_) => ":pointer",
        TypeRef::Map(_, _) => ":void",
        _ => rb_ffi_scalar(ty),
    }
}

fn rb_return_out_params(ty: &TypeRef) -> Vec<&'static str> {
    match ty {
        TypeRef::Bytes | TypeRef::BorrowedBytes => vec![":pointer"],
        TypeRef::Optional(inner) if is_c_pointer_type(inner) => rb_return_out_params(inner),
        TypeRef::List(_) | TypeRef::Iterator(_) => vec![":pointer"],
        TypeRef::Map(_, _) => vec![":pointer", ":pointer", ":pointer"],
        _ => vec![],
    }
}

fn rb_read_method(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => "read_int32",
        TypeRef::U32 => "read_uint32",
        TypeRef::I64 => "read_int64",
        TypeRef::F64 => "read_double",
        TypeRef::Handle => "read_uint64",
        _ => "read_pointer",
    }
}

fn rb_mem_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => ":int32",
        TypeRef::U32 => ":uint32",
        TypeRef::I64 => ":int64",
        TypeRef::F64 => ":double",
        TypeRef::Handle => ":uint64",
        _ => ":pointer",
    }
}

fn rb_write_method(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => "write_int32",
        TypeRef::U32 => "write_uint32",
        TypeRef::I64 => "write_int64",
        TypeRef::F64 => "write_double",
        TypeRef::Handle => "write_uint64",
        _ => "write_pointer",
    }
}

fn rb_array_reader(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => "read_array_of_int32",
        TypeRef::U32 => "read_array_of_uint32",
        TypeRef::I64 => "read_array_of_int64",
        TypeRef::F64 => "read_array_of_double",
        TypeRef::Handle => "read_array_of_uint64",
        _ => "read_array_of_pointer",
    }
}

fn rb_array_writer(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I32 | TypeRef::Enum(_) => "write_array_of_int32",
        TypeRef::U32 => "write_array_of_uint32",
        TypeRef::I64 => "write_array_of_int64",
        TypeRef::F64 => "write_array_of_double",
        TypeRef::Handle => "write_array_of_uint64",
        _ => "write_array_of_pointer",
    }
}

fn get_map_kv(ty: &TypeRef) -> Option<(&TypeRef, &TypeRef)> {
    match ty {
        TypeRef::Map(k, v) => Some((k, v)),
        TypeRef::Optional(inner) => get_map_kv(inner),
        _ => None,
    }
}

fn rb_call_args(name: &str, ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_)
        | TypeRef::StringUtf8
        | TypeRef::BorrowedStr => {
            vec![name.to_string()]
        }
        TypeRef::Bool => vec![format!("{name}_c")],
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![format!("{name}_buf"), format!("{name}.bytesize")]
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}.handle")],
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => vec![format!("{name}_c")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![name.to_string()],
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}&.handle")],
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                vec![format!("{name}_buf"), format!("{name}_len")]
            }
            TypeRef::List(_) => vec![format!("{name}_buf"), format!("{name}_len")],
            TypeRef::Map(_, _) => vec![
                format!("{name}_keys_buf"),
                format!("{name}_vals_buf"),
                format!("{name}_len"),
            ],
            _ => rb_call_args(name, inner),
        },
        TypeRef::List(_) => vec![format!("{name}_buf"), format!("{name}.length")],
        TypeRef::Map(_, _) => vec![
            format!("{name}_keys_buf"),
            format!("{name}_vals_buf"),
            format!("{name}.length"),
        ],
        TypeRef::Callback(_) => vec![name.to_string()],
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
    }
}

fn rb_element_expr(var: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("{var}.null? ? '' : {var}.read_string")
        }
        TypeRef::TypedHandle(name) => format!("{name}.new({var})"),
        TypeRef::Struct(name) => format!("{}.new({var})", local_type_name(name)),
        TypeRef::Bool => format!("{var} != 0"),
        _ => var.to_string(),
    }
}

#[allow(dead_code)]
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

// ── Rendering ──

fn render_ruby_module(api: &Api, module_name: &str) -> String {
    let mut out = String::new();
    render_preamble(&mut out, module_name);
    for (m, path) in collect_modules_with_path(&api.modules) {
        out.push_str(&format!("\n  # === Module: {} ===\n", path));
        for e in &m.enums {
            render_enum(&mut out, e);
        }
        for s in &m.structs {
            render_struct_ffi(&mut out, &path, s);
        }
        for f in &m.functions {
            if !f.r#async {
                render_attach_function(&mut out, &path, f);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, &path, s, module_name);
            if s.builder {
                render_ruby_builder_class(&mut out, s);
            }
        }
        for f in &m.functions {
            if !f.r#async {
                render_function_wrapper(&mut out, &path, f);
            }
        }
    }
    out.push_str("end\n");
    out
}

fn render_preamble(out: &mut String, module_name: &str) {
    out.push_str(&format!(
        "# frozen_string_literal: true
# {module_name} Ruby FFI bindings (auto-generated)

require 'ffi'

module {module_name}
  extend FFI::Library

  case FFI::Platform::OS
  when /darwin/
    ffi_lib 'libweaveffi.dylib'
  when /mswin|mingw/
    ffi_lib 'weaveffi.dll'
  else
    ffi_lib 'libweaveffi.so'
  end

  class ErrorStruct < FFI::Struct
    layout :code, :int32,
           :message, :pointer
  end

  class Error < StandardError
    attr_reader :code

    def initialize(code, message)
      @code = code
      super(message)
    end
  end

  attach_function :weaveffi_error_clear, [:pointer], :void
  attach_function :weaveffi_free_string, [:pointer], :void
  attach_function :weaveffi_free_bytes, [:pointer, :size_t], :void

  def self.check_error!(err)
    return if err[:code].zero?
    code = err[:code]
    msg_ptr = err[:message]
    msg = msg_ptr.null? ? '' : msg_ptr.read_string
    weaveffi_error_clear(err.to_ptr)
    raise Error.new(code, msg)
  end
"
    ));
}

fn render_enum(out: &mut String, e: &EnumDef) {
    out.push_str(&format!("\n  module {}\n", e.name));
    for v in &e.variants {
        out.push_str(&format!(
            "    {} = {}\n",
            v.name.to_shouty_snake_case(),
            v.value
        ));
    }
    out.push_str("  end\n");
}

fn render_struct_ffi(out: &mut String, module_name: &str, s: &StructDef) {
    let prefix = format!("weaveffi_{}_{}", module_name, s.name);
    out.push_str(&format!(
        "\n  attach_function :{prefix}_destroy, [:pointer], :void\n"
    ));
    for field in &s.fields {
        let getter = format!("{prefix}_get_{}", field.name);
        let mut argtypes = vec![":pointer".to_string()];
        argtypes.extend(
            rb_return_out_params(&field.ty)
                .iter()
                .map(|s| s.to_string()),
        );
        let restype = rb_return_ffi_type(&field.ty);
        out.push_str(&format!(
            "  attach_function :{getter}, [{}], {restype}\n",
            argtypes.join(", ")
        ));
    }
}

fn render_attach_function(out: &mut String, module_name: &str, f: &Function) {
    let c_sym = c_symbol_name(module_name, &f.name);
    let mut argtypes: Vec<String> = Vec::new();
    for p in &f.params {
        argtypes.extend(rb_param_ffi_types(&p.ty).iter().map(|s| s.to_string()));
    }
    if let Some(ret_ty) = &f.returns {
        argtypes.extend(rb_return_out_params(ret_ty).iter().map(|s| s.to_string()));
    }
    argtypes.push(":pointer".into());
    let restype = f
        .returns
        .as_ref()
        .map(|ty| rb_return_ffi_type(ty))
        .unwrap_or(":void");
    out.push_str(&format!(
        "  attach_function :{c_sym}, [{}], {restype}\n",
        argtypes.join(", ")
    ));
}

fn render_struct_class(
    out: &mut String,
    api_module_name: &str,
    s: &StructDef,
    rb_module_name: &str,
) {
    let prefix = format!("weaveffi_{}_{}", api_module_name, s.name);

    out.push_str(&format!("\n  class {}Ptr < FFI::AutoPointer\n", s.name));
    out.push_str(&format!(
        "    def self.release(ptr)\n      {rb_module_name}.{prefix}_destroy(ptr)\n    end\n"
    ));
    out.push_str("  end\n\n");

    out.push_str(&format!("  class {}\n", s.name));
    out.push_str("    attr_reader :handle\n\n");
    out.push_str(&format!(
        "    def initialize(handle)\n      @handle = {}Ptr.new(handle)\n    end\n\n",
        s.name
    ));
    out.push_str("    def self.create(handle)\n      new(handle)\n    end\n\n");
    out.push_str(
        "    def destroy\n      return if @handle.nil?\n      @handle.free\n      @handle = nil\n    end\n",
    );

    for field in &s.fields {
        render_getter(out, &prefix, field, rb_module_name);
    }

    out.push_str("  end\n");
}

fn render_ruby_builder_class(out: &mut String, s: &StructDef) {
    let builder = format!("{}Builder", s.name);
    out.push_str(&format!("\n  class {builder}\n"));
    out.push_str("    def initialize\n");
    for field in &s.fields {
        out.push_str(&format!("      @{} = nil\n", field.name));
    }
    out.push_str("    end\n\n");

    for field in &s.fields {
        out.push_str(&format!(
            "    def with_{}(value)\n      @{} = value\n      self\n    end\n\n",
            field.name, field.name
        ));
    }

    out.push_str("    def build\n");
    for field in &s.fields {
        out.push_str(&format!(
            "      raise \"missing field: {}\" if @{}.nil?\n",
            field.name, field.name
        ));
    }
    out.push_str(&format!(
        "      raise NotImplementedError, \"{builder}.build requires FFI backing\"\n"
    ));
    out.push_str("    end\n");
    out.push_str("  end\n");
}

fn render_getter(out: &mut String, prefix: &str, field: &StructField, rb_module_name: &str) {
    let getter = format!("{prefix}_get_{}", field.name);
    let ind = "      ";

    out.push_str(&format!("\n    def {}\n", field.name));

    let out_params = rb_return_out_params(&field.ty);
    let is_map = get_map_kv(&field.ty).is_some();

    if is_map {
        out.push_str(&format!(
            "{ind}out_keys = FFI::MemoryPointer.new(:pointer)\n"
        ));
        out.push_str(&format!(
            "{ind}out_values = FFI::MemoryPointer.new(:pointer)\n"
        ));
        out.push_str(&format!("{ind}out_len = FFI::MemoryPointer.new(:size_t)\n"));
        out.push_str(&format!(
            "{ind}{rb_module_name}.{getter}(@handle, out_keys, out_values, out_len)\n"
        ));
        let (k, v) = get_map_kv(&field.ty).unwrap();
        let is_optional = matches!(&field.ty, TypeRef::Optional(_));
        render_map_return_code(out, k, v, ind, is_optional);
    } else if !out_params.is_empty() {
        out.push_str(&format!("{ind}out_len = FFI::MemoryPointer.new(:size_t)\n"));
        out.push_str(&format!(
            "{ind}result = {rb_module_name}.{getter}(@handle, out_len)\n"
        ));
        render_return_code(out, &field.ty, ind, Some(rb_module_name));
    } else {
        out.push_str(&format!(
            "{ind}result = {rb_module_name}.{getter}(@handle)\n"
        ));
        render_return_code(out, &field.ty, ind, Some(rb_module_name));
    }

    out.push_str("    end\n");
}

fn render_function_wrapper(out: &mut String, module_name: &str, f: &Function) {
    let c_sym = c_symbol_name(module_name, &f.name);
    let func_name = f.name.to_snake_case();
    let ind = "    ";

    let params: Vec<String> = f.params.iter().map(|p| p.name.to_snake_case()).collect();
    out.push_str(&format!(
        "\n  def self.{}({})\n",
        func_name,
        params.join(", ")
    ));

    out.push_str(&format!("{ind}err = ErrorStruct.new\n"));

    for p in &f.params {
        render_param_conversion(out, &p.name.to_snake_case(), &p.ty, ind);
    }

    let is_map_ret = f.returns.as_ref().and_then(get_map_kv).is_some();
    let has_out_len = f
        .returns
        .as_ref()
        .is_some_and(|ty| !rb_return_out_params(ty).is_empty())
        && !is_map_ret;

    if is_map_ret {
        out.push_str(&format!(
            "{ind}out_keys = FFI::MemoryPointer.new(:pointer)\n"
        ));
        out.push_str(&format!(
            "{ind}out_values = FFI::MemoryPointer.new(:pointer)\n"
        ));
        out.push_str(&format!("{ind}out_len = FFI::MemoryPointer.new(:size_t)\n"));
    } else if has_out_len {
        out.push_str(&format!("{ind}out_len = FFI::MemoryPointer.new(:size_t)\n"));
    }

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        call_args.extend(rb_call_args(&p.name.to_snake_case(), &p.ty));
    }
    if is_map_ret {
        call_args.extend(["out_keys".into(), "out_values".into(), "out_len".into()]);
    } else if has_out_len {
        call_args.push("out_len".into());
    }
    call_args.push("err".into());

    let call = format!("{c_sym}({})", call_args.join(", "));
    if f.returns.is_some() && !is_map_ret {
        out.push_str(&format!("{ind}result = {call}\n"));
    } else {
        out.push_str(&format!("{ind}{call}\n"));
    }

    out.push_str(&format!("{ind}check_error!(err)\n"));

    if let Some(ret_ty) = &f.returns {
        if is_map_ret {
            let (k, v) = get_map_kv(ret_ty).unwrap();
            let is_optional = matches!(ret_ty, TypeRef::Optional(_));
            render_map_return_code(out, k, v, ind, is_optional);
        } else {
            render_return_code(out, ret_ty, ind, None);
        }
    }

    out.push_str("  end\n");
}

// ── Parameter conversion ──

fn render_param_conversion(out: &mut String, name: &str, ty: &TypeRef, ind: &str) {
    match ty {
        TypeRef::Bool => {
            out.push_str(&format!("{ind}{name}_c = {name} ? 1 : 0\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!(
                "{ind}{name}_buf = FFI::MemoryPointer.new(:uint8, {name}.bytesize)\n"
            ));
            out.push_str(&format!("{ind}{name}_buf.put_bytes(0, {name})\n"));
        }
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => {
            let mem = rb_mem_type(inner);
            let write = rb_write_method(inner);
            let val = match inner.as_ref() {
                TypeRef::Bool => format!("{name} ? 1 : 0"),
                _ => name.to_string(),
            };
            out.push_str(&format!(
                "{ind}{name}_c = {name}.nil? ? FFI::Pointer::NULL : \
                 begin; p = FFI::MemoryPointer.new({mem}); p.{write}({val}); p; end\n"
            ));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                out.push_str(&format!("{ind}if {name}.nil?\n"));
                out.push_str(&format!("{ind}  {name}_buf = FFI::Pointer::NULL\n"));
                out.push_str(&format!("{ind}  {name}_len = 0\n"));
                out.push_str(&format!("{ind}else\n"));
                out.push_str(&format!(
                    "{ind}  {name}_buf = FFI::MemoryPointer.new(:uint8, {name}.bytesize)\n"
                ));
                out.push_str(&format!("{ind}  {name}_buf.put_bytes(0, {name})\n"));
                out.push_str(&format!("{ind}  {name}_len = {name}.bytesize\n"));
                out.push_str(&format!("{ind}end\n"));
            }
            TypeRef::List(elem) => {
                out.push_str(&format!("{ind}if {name}.nil?\n"));
                out.push_str(&format!("{ind}  {name}_buf = FFI::Pointer::NULL\n"));
                out.push_str(&format!("{ind}  {name}_len = 0\n"));
                out.push_str(&format!("{ind}else\n"));
                render_list_buf(out, name, elem, &format!("{ind}  "));
                out.push_str(&format!("{ind}  {name}_len = {name}.length\n"));
                out.push_str(&format!("{ind}end\n"));
            }
            TypeRef::Map(k, v) => {
                out.push_str(&format!("{ind}if {name}.nil?\n"));
                out.push_str(&format!("{ind}  {name}_keys_buf = FFI::Pointer::NULL\n"));
                out.push_str(&format!("{ind}  {name}_vals_buf = FFI::Pointer::NULL\n"));
                out.push_str(&format!("{ind}  {name}_len = 0\n"));
                out.push_str(&format!("{ind}else\n"));
                render_map_buf(out, name, k, v, &format!("{ind}  "));
                out.push_str(&format!("{ind}end\n"));
            }
            _ => {}
        },
        TypeRef::List(elem) => {
            render_list_buf(out, name, elem, ind);
        }
        TypeRef::Map(k, v) => {
            render_map_buf(out, name, k, v, ind);
        }
        _ => {}
    }
}

fn render_list_buf(out: &mut String, name: &str, elem: &TypeRef, ind: &str) {
    let mem = rb_mem_type(elem);
    out.push_str(&format!(
        "{ind}{name}_buf = FFI::MemoryPointer.new({mem}, {name}.length)\n"
    ));
    match elem {
        TypeRef::Bool => {
            out.push_str(&format!(
                "{ind}{name}_buf.write_array_of_int32({name}.map {{ |v| v ? 1 : 0 }})\n"
            ));
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => {
            out.push_str(&format!(
                "{ind}{name}_buf.write_array_of_pointer({name}.map(&:handle))\n"
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{ind}{name}_buf.write_array_of_pointer(\
                 {name}.map {{ |s| FFI::MemoryPointer.from_string(s) }})\n"
            ));
        }
        _ => {
            let write = rb_array_writer(elem);
            out.push_str(&format!("{ind}{name}_buf.{write}({name})\n"));
        }
    }
}

fn render_map_buf(out: &mut String, name: &str, k: &TypeRef, v: &TypeRef, ind: &str) {
    let k_mem = rb_mem_type(k);
    let v_mem = rb_mem_type(v);
    out.push_str(&format!("{ind}{name}_k = {name}.keys\n"));
    out.push_str(&format!("{ind}{name}_v = {name}.values\n"));
    out.push_str(&format!(
        "{ind}{name}_keys_buf = FFI::MemoryPointer.new({k_mem}, {name}_k.length)\n"
    ));
    out.push_str(&format!(
        "{ind}{name}_vals_buf = FFI::MemoryPointer.new({v_mem}, {name}_v.length)\n"
    ));
    let k_write = rb_array_writer(k);
    let v_write = rb_array_writer(v);
    out.push_str(&format!("{ind}{name}_keys_buf.{k_write}({name}_k)\n"));
    out.push_str(&format!("{ind}{name}_vals_buf.{v_write}({name}_v)\n"));
}

// ── Return value rendering ──

fn render_return_code(out: &mut String, ty: &TypeRef, ind: &str, qualifier: Option<&str>) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    match ty {
        TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            out.push_str(&format!("{ind}result\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}result != 0\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}return '' if result.null?\n"));
            out.push_str(&format!("{ind}str = result.read_string\n"));
            out.push_str(&format!("{ind}{m}weaveffi_free_string(result)\n"));
            out.push_str(&format!("{ind}str\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}return ''.b if result.null?\n"));
            out.push_str(&format!("{ind}len = out_len.read(:size_t)\n"));
            out.push_str(&format!("{ind}data = result.read_string(len)\n"));
            out.push_str(&format!("{ind}{m}weaveffi_free_bytes(result, len)\n"));
            out.push_str(&format!("{ind}data\n"));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!(
                "{ind}raise Error.new(-1, 'null pointer') if result.null?\n"
            ));
            out.push_str(&format!("{ind}{name}.new(result)\n"));
        }
        TypeRef::Struct(name) => {
            out.push_str(&format!(
                "{ind}raise Error.new(-1, 'null pointer') if result.null?\n"
            ));
            out.push_str(&format!("{ind}{}.new(result)\n", local_type_name(name)));
        }
        TypeRef::Optional(inner) => render_optional_return_code(out, inner, ind, qualifier),
        TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            out.push_str(&format!("{ind}return [] if result.null?\n"));
            render_list_return_body(out, inner, ind);
        }
        TypeRef::Map(_, _) | TypeRef::Callback(_) => {
            out.push_str(&format!("{ind}result\n"));
        }
    }
}

fn render_optional_return_code(
    out: &mut String,
    inner: &TypeRef,
    ind: &str,
    qualifier: Option<&str>,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            out.push_str(&format!("{ind}str = result.read_string\n"));
            out.push_str(&format!("{ind}{m}weaveffi_free_string(result)\n"));
            out.push_str(&format!("{ind}str\n"));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            out.push_str(&format!("{ind}{name}.new(result)\n"));
        }
        TypeRef::Struct(name) => {
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            out.push_str(&format!("{ind}{}.new(result)\n", local_type_name(name)));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            out.push_str(&format!("{ind}len = out_len.read(:size_t)\n"));
            out.push_str(&format!("{ind}data = result.read_string(len)\n"));
            out.push_str(&format!("{ind}{m}weaveffi_free_bytes(result, len)\n"));
            out.push_str(&format!("{ind}data\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            out.push_str(&format!("{ind}result.read_int32 != 0\n"));
        }
        TypeRef::List(elem) => {
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            render_list_return_body(out, elem, ind);
        }
        TypeRef::Map(k, v) => {
            render_map_return_code(out, k, v, ind, true);
        }
        _ if !is_c_pointer_type(inner) => {
            let read = rb_read_method(inner);
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            out.push_str(&format!("{ind}result.{read}\n"));
        }
        _ => {
            out.push_str(&format!("{ind}result\n"));
        }
    }
}

fn render_list_return_body(out: &mut String, inner: &TypeRef, ind: &str) {
    out.push_str(&format!("{ind}len = out_len.read(:size_t)\n"));
    let reader = rb_array_reader(inner);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!(
                "{ind}result.{reader}(len).map {{ |p| p.null? ? '' : p.read_string }}\n"
            ));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!(
                "{ind}result.{reader}(len).map {{ |p| {name}.new(p) }}\n"
            ));
        }
        TypeRef::Struct(name) => {
            let local = local_type_name(name);
            out.push_str(&format!(
                "{ind}result.{reader}(len).map {{ |p| {local}.new(p) }}\n"
            ));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}result.{reader}(len).map {{ |v| v != 0 }}\n"));
        }
        _ => {
            out.push_str(&format!("{ind}result.{reader}(len)\n"));
        }
    }
}

fn render_map_return_code(out: &mut String, k: &TypeRef, v: &TypeRef, ind: &str, optional: bool) {
    let null_val = if optional { "nil" } else { "{}" };
    out.push_str(&format!("{ind}len = out_len.read(:size_t)\n"));
    out.push_str(&format!("{ind}keys_ptr = out_keys.read_pointer\n"));
    out.push_str(&format!("{ind}vals_ptr = out_values.read_pointer\n"));
    out.push_str(&format!(
        "{ind}return {null_val} if keys_ptr.null? || vals_ptr.null?\n"
    ));
    let k_reader = rb_array_reader(k);
    let v_reader = rb_array_reader(v);
    let k_expr = rb_element_expr("k", k);
    let v_expr = rb_element_expr("v", v);
    out.push_str(&format!(
        "{ind}keys_ptr.{k_reader}(len).zip(vals_ptr.{v_reader}(len))\
         .each_with_object({{}}) {{ |(k, v), h| h[{k_expr}] = {v_expr} }}\n"
    ));
}

fn render_gemspec(gem_name: &str) -> String {
    format!(
        "Gem::Specification.new do |s|
  s.name        = '{gem_name}'
  s.version     = '0.1.0'
  s.summary     = 'Ruby FFI bindings for {gem_name} (auto-generated)'
  s.files       = Dir['lib/**/*.rb']
  s.require_paths = ['lib']

  s.add_dependency 'ffi', '~> 1.15'
end
"
    )
}

fn render_readme() -> &'static str {
    r#"# WeaveFFI Ruby Bindings

Auto-generated Ruby bindings using the [ffi](https://github.com/ffi/ffi) gem.

## Prerequisites

- Ruby >= 2.7
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`, or `weaveffi.dll`) available on your library search path.

## Install

```bash
gem build weaveffi.gemspec
gem install weaveffi-0.1.0.gem
```

## Usage

```ruby
require 'weaveffi'
```
"#
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
            version: "0.1.0".to_string(),
            modules,
            generators: None,
        }
    }

    fn simple_module(name: &str, functions: Vec<Function>) -> Module {
        Module {
            name: name.into(),
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
    fn name_returns_ruby() {
        assert_eq!(RubyGenerator.name(), "ruby");
    }

    #[test]
    fn generates_output_file() {
        let api = make_api(vec![simple_module(
            "math",
            vec![Function {
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
            }],
        )]);

        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        RubyGenerator.generate(&api, out_dir).unwrap();

        let file = out_dir.join("ruby/lib/weaveffi.rb");
        assert!(file.exists(), "weaveffi.rb should exist");
        let contents = std::fs::read_to_string(&file).unwrap();
        assert!(contents.contains("require 'ffi'"));
        assert!(contents.contains("module WeaveFFI"));
        assert!(contents.contains("attach_function :weaveffi_math_add"));
        assert!(contents.contains("def self.add(a, b)"));
    }

    #[test]
    fn output_files_returns_correct_path() {
        let api = make_api(vec![]);
        let out_dir = Utf8Path::new("/tmp/out");
        let files = RubyGenerator.output_files(&api, out_dir);
        assert_eq!(
            files,
            vec![
                "/tmp/out/ruby/lib/weaveffi.rb",
                "/tmp/out/ruby/weaveffi.gemspec",
                "/tmp/out/ruby/README.md",
            ]
        );
    }

    #[test]
    fn ruby_generates_gemspec() {
        let api = make_api(vec![simple_module("math", vec![])]);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        RubyGenerator.generate(&api, out_dir).unwrap();

        let gemspec = out_dir.join("ruby/weaveffi.gemspec");
        assert!(gemspec.exists(), "gemspec should exist");
        let contents = std::fs::read_to_string(&gemspec).unwrap();
        assert!(
            contents.contains("Gem::Specification.new do |s|"),
            "gemspec header: {contents}"
        );
        assert!(contents.contains("s.name"), "name field: {contents}");
        assert!(contents.contains("s.version"), "version field: {contents}");
        assert!(contents.contains("s.summary"), "summary field: {contents}");
        assert!(contents.contains("s.files"), "files field: {contents}");
        assert!(
            contents.contains("s.require_paths"),
            "require_paths: {contents}"
        );
        assert!(
            contents.contains("s.add_dependency 'ffi', '~> 1.15'"),
            "ffi dependency: {contents}"
        );

        let readme = out_dir.join("ruby/README.md");
        assert!(readme.exists(), "README should exist");
        let readme_contents = std::fs::read_to_string(&readme).unwrap();
        assert!(
            readme_contents.contains("gem build"),
            "usage instructions: {readme_contents}"
        );
    }

    #[test]
    fn renders_enum_with_shouty_snake_case() {
        let api = make_api(vec![Module {
            name: "gfx".into(),
            functions: vec![],
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
                        name: "DarkBlue".into(),
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

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains("module Color"), "enum module: {code}");
        assert!(code.contains("RED = 0"), "RED: {code}");
        assert!(code.contains("DARK_BLUE = 1"), "DARK_BLUE: {code}");
    }

    #[test]
    fn renders_struct_with_auto_pointer() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        doc: None,
                    },
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(
            code.contains("class ContactPtr < FFI::AutoPointer"),
            "AutoPointer: {code}"
        );
        assert!(
            code.contains("WeaveFFI.weaveffi_contacts_Contact_destroy(ptr)"),
            "release: {code}"
        );
        assert!(code.contains("class Contact"), "class: {code}");
        assert!(code.contains("attr_reader :handle"), "handle: {code}");
        assert!(
            code.contains("@handle = ContactPtr.new(handle)"),
            "init: {code}"
        );
        assert!(code.contains("def self.create(handle)"), "create: {code}");
        assert!(code.contains("def destroy"), "destroy: {code}");
        assert!(code.contains("def id"), "id getter: {code}");
        assert!(code.contains("def name"), "name getter: {code}");
    }

    #[test]
    fn renders_struct_builder_class() {
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
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains("class PointBuilder"), "builder class: {code}");
        assert!(code.contains("def with_x(value)"), "with_x: {code}");
        assert!(
            code.contains("NotImplementedError, \"PointBuilder.build requires FFI backing\""),
            "build stub: {code}"
        );
    }

    #[test]
    fn struct_getter_frees_string() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "label".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(
            code.contains("WeaveFFI.weaveffi_free_string(result)"),
            "free_string in getter: {code}"
        );
    }

    #[test]
    fn function_wrapper_checks_error() {
        let api = make_api(vec![simple_module(
            "math",
            vec![Function {
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
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains("err = ErrorStruct.new"), "err alloc: {code}");
        assert!(code.contains("check_error!(err)"), "check_error: {code}");
    }

    #[test]
    fn string_return_reads_and_frees() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "get_name".into(),
                params: vec![],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains("result.read_string"), "read_string: {code}");
        assert!(
            code.contains("weaveffi_free_string(result)"),
            "free_string: {code}"
        );
        assert!(
            code.contains("return '' if result.null?"),
            "null check: {code}"
        );
    }

    #[test]
    fn bool_param_and_return_conversion() {
        let api = make_api(vec![simple_module(
            "check",
            vec![Function {
                name: "is_valid".into(),
                params: vec![Param {
                    name: "value".into(),
                    ty: TypeRef::Bool,
                    mutable: false,
                }],
                returns: Some(TypeRef::Bool),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(
            code.contains("value_c = value ? 1 : 0"),
            "bool param: {code}"
        );
        assert!(code.contains("result != 0"), "bool return: {code}");
    }

    #[test]
    fn optional_string_returns_nil() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "find".into(),
                params: vec![],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(
            code.contains("return nil if result.null?"),
            "optional nil: {code}"
        );
    }

    #[test]
    fn list_return_uses_array() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "list_ids".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(
            code.contains("return [] if result.null?"),
            "empty array: {code}"
        );
        assert!(code.contains("read_array_of_int32"), "array reader: {code}");
    }

    #[test]
    fn map_return_builds_hash() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "get_metadata".into(),
                params: vec![],
                returns: Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains("out_keys"), "out_keys: {code}");
        assert!(code.contains("out_values"), "out_values: {code}");
        assert!(code.contains("each_with_object"), "hash build: {code}");
    }

    #[test]
    fn struct_return_wraps_in_class() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![Function {
                name: "get_item".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    mutable: false,
                }],
                returns: Some(TypeRef::Struct("Item".into())),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains("Item.new(result)"), "struct wrap: {code}");
        assert!(
            code.contains("raise Error.new(-1, 'null pointer') if result.null?"),
            "null ptr: {code}"
        );
    }

    #[test]
    fn skips_async_functions() {
        let api = make_api(vec![simple_module(
            "io",
            vec![Function {
                name: "read".into(),
                params: vec![],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: true,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(
            !code.contains("def self.read"),
            "async should be skipped: {code}"
        );
        assert!(
            !code.contains("weaveffi_io_read"),
            "async attach should be skipped: {code}"
        );
    }

    #[test]
    fn preamble_has_platform_detection() {
        let code = render_ruby_module(&make_api(vec![]), "WeaveFFI");
        assert!(code.contains("FFI::Platform::OS"), "platform: {code}");
        assert!(code.contains("libweaveffi.dylib"), "darwin: {code}");
        assert!(code.contains("weaveffi.dll"), "windows: {code}");
        assert!(code.contains("libweaveffi.so"), "linux: {code}");
    }

    #[test]
    fn error_class_structure() {
        let code = render_ruby_module(&make_api(vec![]), "WeaveFFI");
        assert!(
            code.contains("class Error < StandardError"),
            "Error class: {code}"
        );
        assert!(code.contains("attr_reader :code"), "code attr: {code}");
    }

    #[test]
    fn handle_type_uses_uint64() {
        let api = make_api(vec![simple_module(
            "store",
            vec![Function {
                name: "create".into(),
                params: vec![],
                returns: Some(TypeRef::Handle),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains(":uint64"), "handle type: {code}");
    }

    #[test]
    fn ffi_type_mapping() {
        assert_eq!(rb_ffi_scalar(&TypeRef::I32), ":int32");
        assert_eq!(rb_ffi_scalar(&TypeRef::U32), ":uint32");
        assert_eq!(rb_ffi_scalar(&TypeRef::I64), ":int64");
        assert_eq!(rb_ffi_scalar(&TypeRef::F64), ":double");
        assert_eq!(rb_ffi_scalar(&TypeRef::Bool), ":int32");
        assert_eq!(rb_ffi_scalar(&TypeRef::Handle), ":uint64");
        assert_eq!(rb_ffi_scalar(&TypeRef::StringUtf8), ":string");
        assert_eq!(rb_ffi_scalar(&TypeRef::Enum("Color".into())), ":int32");
        assert_eq!(rb_ffi_scalar(&TypeRef::Struct("Foo".into())), ":pointer");
    }

    #[test]
    fn return_type_string_is_pointer() {
        assert_eq!(rb_return_ffi_type(&TypeRef::StringUtf8), ":pointer");
    }

    #[test]
    fn return_type_map_is_void() {
        assert_eq!(
            rb_return_ffi_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            ":void"
        );
    }

    #[test]
    fn enum_param_passes_int32() {
        let api = make_api(vec![simple_module(
            "gfx",
            vec![Function {
                name: "set_color".into(),
                params: vec![Param {
                    name: "color".into(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains(":int32"), "enum type: {code}");
    }

    #[test]
    fn void_function_no_result() {
        let api = make_api(vec![simple_module(
            "store",
            vec![Function {
                name: "clear".into(),
                params: vec![],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains(":void"), "void return: {code}");
        assert!(
            !code.contains("result = weaveffi_store_clear"),
            "no result capture: {code}"
        );
    }

    #[test]
    fn list_of_structs_return() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![Function {
                name: "list_items".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Item".into())))),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    doc: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(code.contains("Item.new(p)"), "struct list element: {code}");
    }

    #[test]
    fn optional_struct_returns_nil_on_null() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "find_item".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    mutable: false,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct("Item".into())))),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI");
        assert!(
            code.contains("return nil if result.null?"),
            "optional struct nil: {code}"
        );
        assert!(
            code.contains("Item.new(result)"),
            "optional struct wrap: {code}"
        );
    }

    // ── Comprehensive tests ──

    fn contacts_api() -> Api {
        Api {
            version: "0.1.0".into(),
            modules: vec![Module {
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
                    },
                    Function {
                        name: "list_contacts".into(),
                        params: vec![],
                        returns: Some(TypeRef::List(Box::new(TypeRef::Struct("Contact".into())))),
                        doc: None,
                        r#async: false,
                        cancellable: false,
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
                    },
                    Function {
                        name: "count_contacts".into(),
                        params: vec![],
                        returns: Some(TypeRef::I32),
                        doc: None,
                        r#async: false,
                        cancellable: false,
                    },
                ],
                structs: vec![StructDef {
                    name: "Contact".into(),
                    doc: None,
                    builder: false,
                    fields: vec![
                        StructField {
                            name: "id".into(),
                            ty: TypeRef::I64,
                            doc: None,
                        },
                        StructField {
                            name: "first_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                        },
                        StructField {
                            name: "last_name".into(),
                            ty: TypeRef::StringUtf8,
                            doc: None,
                        },
                        StructField {
                            name: "email".into(),
                            ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                            doc: None,
                        },
                        StructField {
                            name: "contact_type".into(),
                            ty: TypeRef::Enum("ContactType".into()),
                            doc: None,
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
            }],
            generators: None,
        }
    }

    #[test]
    fn generate_ruby_basic() {
        let api = make_api(vec![simple_module(
            "math",
            vec![Function {
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
            }],
        )]);

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
        assert!(rb.contains("module WeaveFFI"), "module name: {rb}");
        assert!(
            rb.contains("attach_function :weaveffi_math_add"),
            "attach_function: {rb}"
        );
        assert!(rb.contains("def self.add(a, b)"), "wrapper fn: {rb}");
        assert!(rb.contains("check_error!(err)"), "error check: {rb}");
    }

    #[test]
    fn generate_ruby_with_structs() {
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
            }],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![
                    StructField {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
        assert!(
            rb.contains("class ContactPtr < FFI::AutoPointer"),
            "auto pointer: {rb}"
        );
        assert!(rb.contains("class Contact"), "struct class: {rb}");
        assert!(rb.contains("attr_reader :handle"), "handle attr: {rb}");
        assert!(rb.contains("def first_name"), "getter: {rb}");
        assert!(rb.contains("def last_name"), "getter: {rb}");
        assert!(
            rb.contains("Contact.new(result)"),
            "struct return wrap: {rb}"
        );
    }

    #[test]
    fn generate_ruby_with_enums() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "classify".into(),
                params: vec![Param {
                    name: "ct".into(),
                    ty: TypeRef::Enum("ContactType".into()),
                    mutable: false,
                }],
                returns: Some(TypeRef::Enum("ContactType".into())),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![],
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

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
        assert!(rb.contains("module ContactType"), "enum module: {rb}");
        assert!(rb.contains("PERSONAL = 0"), "variant 0: {rb}");
        assert!(rb.contains("WORK = 1"), "variant 1: {rb}");
        assert!(rb.contains("OTHER = 2"), "variant 2: {rb}");
        assert!(rb.contains(":int32"), "enum ffi type: {rb}");
    }

    #[test]
    fn generate_ruby_with_optionals() {
        let api = make_api(vec![simple_module(
            "data",
            vec![
                Function {
                    name: "find_name".into(),
                    params: vec![Param {
                        name: "id".into(),
                        ty: TypeRef::I64,
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                },
                Function {
                    name: "find_count".into(),
                    params: vec![Param {
                        name: "key".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        mutable: false,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                },
            ],
        )]);

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
        assert!(
            rb.contains("return nil if result.null?"),
            "nil return for optional string: {rb}"
        );
        assert!(
            rb.contains("FFI::Pointer::NULL"),
            "optional scalar encoding: {rb}"
        );
    }

    #[test]
    fn generate_ruby_with_lists() {
        let api = make_api(vec![simple_module(
            "data",
            vec![
                Function {
                    name: "list_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                },
                Function {
                    name: "set_names".into(),
                    params: vec![Param {
                        name: "names".into(),
                        ty: TypeRef::List(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                },
            ],
        )]);

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
        assert!(
            rb.contains("return [] if result.null?"),
            "empty list fallback: {rb}"
        );
        assert!(
            rb.contains("read_array_of_int32"),
            "list return reader: {rb}"
        );
        assert!(
            rb.contains("FFI::MemoryPointer.new(:pointer, names.length)"),
            "list param buffer: {rb}"
        );
    }

    #[test]
    fn generate_ruby_full_contacts() {
        let api = contacts_api();

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate_with_config(&api, out_dir, &GeneratorConfig::default())
            .unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();

        assert!(rb.contains("module WeaveFFI"), "module: {rb}");
        assert!(rb.contains("module ContactType"), "enum: {rb}");
        assert!(rb.contains("PERSONAL = 0"), "enum variant: {rb}");
        assert!(rb.contains("class Contact"), "struct class: {rb}");
        assert!(
            rb.contains("def self.create_contact(first_name, last_name, email, contact_type)"),
            "create fn: {rb}"
        );
        assert!(rb.contains("def self.get_contact(id)"), "get fn: {rb}");
        assert!(rb.contains("def self.list_contacts"), "list fn: {rb}");
        assert!(
            rb.contains("def self.delete_contact(id)"),
            "delete fn: {rb}"
        );
        assert!(rb.contains("def self.count_contacts"), "count fn: {rb}");
        assert!(rb.contains("def id"), "id getter: {rb}");
        assert!(rb.contains("def first_name"), "first_name getter: {rb}");
        assert!(rb.contains("def email"), "email getter: {rb}");
        assert!(rb.contains("def contact_type"), "contact_type getter: {rb}");

        let gemspec = std::fs::read_to_string(tmp.path().join("ruby/weaveffi.gemspec")).unwrap();
        assert!(
            gemspec.contains("s.name        = 'weaveffi'"),
            "gem name: {gemspec}"
        );

        let readme = std::fs::read_to_string(tmp.path().join("ruby/README.md")).unwrap();
        assert!(readme.contains("Ruby"), "readme: {readme}");
    }

    #[test]
    fn ruby_custom_module_name() {
        let api = make_api(vec![simple_module(
            "math",
            vec![Function {
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
            }],
        )]);

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        let config = GeneratorConfig {
            ruby_module_name: Some("MyBindings".into()),
            ruby_gem_name: Some("my_bindings".into()),
            ..Default::default()
        };
        RubyGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/weaveffi.rb")).unwrap();
        assert!(rb.contains("module MyBindings"), "custom module name: {rb}");
        assert!(
            !rb.contains("module WeaveFFI"),
            "should not contain default module name: {rb}"
        );

        let gemspec = std::fs::read_to_string(tmp.path().join("ruby/weaveffi.gemspec")).unwrap();
        assert!(
            gemspec.contains("s.name        = 'my_bindings'"),
            "custom gem name: {gemspec}"
        );
        assert!(
            !gemspec.contains("s.name        = 'weaveffi'"),
            "should not contain default gem name: {gemspec}"
        );
    }

    #[test]
    fn ruby_no_double_free_on_error() {
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
            }],
            errors: None,
            modules: vec![],
        }]);

        let rb = render_ruby_module(&api, "WeaveFFI");

        let fn_start = rb
            .find("def self.find_contact(name)")
            .expect("find_contact wrapper");
        let fn_body = &rb[fn_start..];
        let fn_end = fn_body.find("\n  end\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        assert!(
            !fn_text.contains("weaveffi_free_string(name"),
            "borrowed string param must not be freed by wrapper: {fn_text}"
        );

        let err_check = fn_text
            .find("check_error!(err)")
            .expect("check_error in find_contact");
        let contact_wrap = fn_text
            .find("Contact.new(result)")
            .expect("Contact.new in find_contact");
        assert!(
            err_check < contact_wrap,
            "error must be checked before wrapping struct return: {fn_text}"
        );

        assert!(
            rb.contains("class ContactPtr < FFI::AutoPointer")
                && rb.contains("weaveffi_contacts_Contact_destroy"),
            "struct return type should use AutoPointer with destroy: {rb}"
        );
    }

    #[test]
    fn ruby_null_check_on_optional_return() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "find_contact".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I64,
                    mutable: false,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Struct(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let rb = render_ruby_module(&api, "WeaveFFI");

        let fn_start = rb
            .find("def self.find_contact(id)")
            .expect("find_contact wrapper");
        let fn_body = &rb[fn_start..];
        let fn_end = fn_body.find("\n  end\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        let null_check = fn_text
            .find("return nil if result.null?")
            .expect("nil check in find_contact");
        let contact_wrap = fn_text
            .find("Contact.new(result)")
            .expect("Contact.new in find_contact");
        assert!(
            null_check < contact_wrap,
            "optional struct return should check nil before wrapping: {fn_text}"
        );
    }
}
