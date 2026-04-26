use anyhow::Result;
use camino::Utf8Path;
use heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase};
use weaveffi_core::codegen::{Capability, Generator};
use weaveffi_core::config::GeneratorConfig;
use weaveffi_core::utils::local_type_name;
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, Function, ListenerDef, Module, StructDef, StructField, TypeRef,
};

pub struct RubyGenerator;

impl RubyGenerator {
    fn generate_impl(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        module_name: &str,
        gem_name: &str,
        c_prefix: &str,
    ) -> Result<()> {
        let dir = out_dir.join("ruby");
        let lib_dir = dir.join("lib");
        std::fs::create_dir_all(&lib_dir)?;
        std::fs::write(
            lib_dir.join("weaveffi.rb"),
            render_ruby_module(api, module_name, c_prefix),
        )?;
        std::fs::write(
            dir.join("weaveffi.gemspec"),
            render_gemspec(gem_name, has_any_async(api)),
        )?;
        std::fs::write(dir.join("README.md"), render_readme())?;
        Ok(())
    }
}

impl Generator for RubyGenerator {
    fn name(&self) -> &'static str {
        "ruby"
    }

    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()> {
        self.generate_impl(api, out_dir, "WeaveFFI", "weaveffi", "weaveffi")
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
            config.c_prefix(),
        )
    }

    fn output_files(&self, _api: &Api, out_dir: &Utf8Path) -> Vec<String> {
        vec![
            out_dir.join("ruby/lib/weaveffi.rb").to_string(),
            out_dir.join("ruby/weaveffi.gemspec").to_string(),
            out_dir.join("ruby/README.md").to_string(),
        ]
    }

    fn capabilities(&self) -> &'static [Capability] {
        &[
            Capability::Builders,
            Capability::Callbacks,
            Capability::Listeners,
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

fn rb_param_ffi_types(ty: &TypeRef) -> Vec<String> {
    match ty {
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![":pointer".into(), ":size_t".into()]
        }
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => vec![":pointer".into()],
        TypeRef::Optional(inner) => rb_param_ffi_types(inner),
        TypeRef::List(_) => vec![":pointer".into(), ":size_t".into()],
        TypeRef::Map(_, _) => vec![":pointer".into(), ":pointer".into(), ":size_t".into()],
        TypeRef::Callback(name) => vec![format!(":{name}"), ":pointer".into()],
        _ => vec![rb_ffi_scalar(ty).into()],
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
        TypeRef::List(_) => vec![":pointer"],
        TypeRef::Iterator(_) => vec![],
        TypeRef::Map(_, _) => vec![":pointer", ":pointer", ":pointer"],
        _ => vec![],
    }
}

fn iter_type_name(c_prefix: &str, module: &str, func_name: &str) -> String {
    let pascal = func_name.to_upper_camel_case();
    format!("{c_prefix}_{module}_{pascal}Iterator")
}

fn rb_iter_item_expr(inner: &TypeRef) -> String {
    match inner {
        TypeRef::Bool => "out_item.read_int32 != 0".into(),
        TypeRef::TypedHandle(name) => format!("{name}.new(out_item.read_pointer)"),
        TypeRef::Struct(name) => {
            format!("{}.new(out_item.read_pointer)", local_type_name(name))
        }
        _ => {
            let read = rb_read_method(inner);
            format!("out_item.{read}")
        }
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
        | TypeRef::BorrowedStr => {
            vec![name.to_string()]
        }
        TypeRef::Bool => vec![format!("{name}_c")],
        TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => {
            vec![format!("{name}_buf"), format!("{name}.bytesize")]
        }
        TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}.handle")],
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => vec![format!("{name}_c")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::BorrowedStr => vec![name.to_string()],
            TypeRef::Struct(_) | TypeRef::TypedHandle(_) => vec![format!("{name}&.handle")],
            TypeRef::StringUtf8 | TypeRef::Bytes | TypeRef::BorrowedBytes => {
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
        TypeRef::Callback(_) => vec![name.to_string(), "FFI::Pointer::NULL".to_string()],
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

fn render_ruby_module(api: &Api, module_name: &str, c_prefix: &str) -> String {
    let mut out = String::new();
    let has_async = has_any_async(api);
    let has_cancellable_async = has_any_cancellable_async(api);
    render_preamble(
        &mut out,
        module_name,
        c_prefix,
        has_async,
        has_cancellable_async,
    );
    for (m, path) in collect_modules_with_path(&api.modules) {
        out.push_str(&format!("\n  # === Module: {} ===\n", path));
        for e in &m.enums {
            render_enum(&mut out, e);
        }
        for cb in &m.callbacks {
            render_callback_def(&mut out, cb);
        }
        for l in &m.listeners {
            render_listener_attach_functions(&mut out, &path, l, c_prefix);
        }
        for s in &m.structs {
            render_struct_ffi(&mut out, &path, s, c_prefix);
        }
        for f in &m.functions {
            if f.r#async {
                render_async_attach_function(&mut out, &path, f, c_prefix);
            } else {
                render_attach_function(&mut out, &path, f, c_prefix);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, &path, s, module_name, c_prefix);
            if s.builder {
                render_ruby_builder_class(&mut out, s);
            }
        }
        for f in &m.functions {
            if f.r#async {
                render_async_function_wrapper(&mut out, &path, f, c_prefix);
            } else {
                render_function_wrapper(&mut out, &path, f, c_prefix);
            }
        }
        for l in &m.listeners {
            render_listener_module(&mut out, &path, l, module_name, c_prefix);
        }
    }
    out.push_str("end\n");
    out
}

fn has_any_async(api: &Api) -> bool {
    collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async))
}

fn has_any_cancellable_async(api: &Api) -> bool {
    collect_all_modules(&api.modules)
        .iter()
        .any(|m| m.functions.iter().any(|f| f.r#async && f.cancellable))
}

fn render_preamble(
    out: &mut String,
    module_name: &str,
    c_prefix: &str,
    has_async: bool,
    has_cancellable_async: bool,
) {
    let extra_require = if has_async {
        "require 'concurrent'\n"
    } else {
        ""
    };
    out.push_str(&format!(
        "# frozen_string_literal: true
# {module_name} Ruby FFI bindings (auto-generated)

require 'ffi'
{extra_require}
module {module_name}
  extend FFI::Library

  case FFI::Platform::OS
  when /darwin/
    ffi_lib 'lib{c_prefix}.dylib'
  when /mswin|mingw/
    ffi_lib '{c_prefix}.dll'
  else
    ffi_lib 'lib{c_prefix}.so'
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

  attach_function :{c_prefix}_error_clear, [:pointer], :void
  attach_function :{c_prefix}_free_string, [:pointer], :void
  attach_function :{c_prefix}_free_bytes, [:pointer, :size_t], :void

  def self.check_error!(err)
    return if err[:code].zero?
    code = err[:code]
    msg_ptr = err[:message]
    msg = msg_ptr.null? ? '' : msg_ptr.read_string
    {c_prefix}_error_clear(err.to_ptr)
    raise Error.new(code, msg)
  end
"
    ));

    if has_cancellable_async {
        out.push_str(&format!(
            "
  # Cancellation token bindings. Cancellable `_async` wrappers create a
  # token, forward it to the C ABI, and call `{c_prefix}_cancel_token_cancel`
  # when the caller's `Concurrent::Cancellation` origin fires.
  attach_function :{c_prefix}_cancel_token_create, [], :pointer
  attach_function :{c_prefix}_cancel_token_cancel, [:pointer], :void
  attach_function :{c_prefix}_cancel_token_destroy, [:pointer], :void
"
        ));
    }

    if has_async {
        out.push_str(
            "
  # Async callback registry. Pins Proc objects keyed by id so Ruby's GC
  # cannot reclaim them while the native side still holds a function
  # pointer. The id is also passed to C as the callback context.
  @@async_callbacks = {}
  @@async_next_id = 0
  @@async_mutex = Mutex.new

  def self.register_async_callback(cb)
    @@async_mutex.synchronize do
      id = @@async_next_id
      @@async_next_id += 1
      @@async_callbacks[id] = cb
      id
    end
  end

  def self.pop_async_callback(id)
    @@async_mutex.synchronize { @@async_callbacks.delete(id) }
  end
",
        );
    }
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

fn render_callback_def(out: &mut String, cb: &CallbackDef) {
    let mut cb_param_types: Vec<String> = vec![":pointer".into()];
    for p in &cb.params {
        cb_param_types.extend(rb_param_ffi_types(&p.ty));
    }
    let ret = cb
        .returns
        .as_ref()
        .map(rb_return_ffi_type)
        .unwrap_or(":void");
    out.push_str(&format!(
        "\n  callback :{}, [{}], {ret}\n",
        cb.name,
        cb_param_types.join(", ")
    ));
}

fn render_listener_attach_functions(
    out: &mut String,
    module_path: &str,
    l: &ListenerDef,
    c_prefix: &str,
) {
    let reg_fn = format!("{c_prefix}_{module_path}_register_{}", l.name);
    let unreg_fn = format!("{c_prefix}_{module_path}_unregister_{}", l.name);
    out.push_str(&format!(
        "\n  attach_function :{reg_fn}, [:{}, :pointer], :uint64\n",
        l.event_callback
    ));
    out.push_str(&format!(
        "  attach_function :{unreg_fn}, [:uint64], :void\n"
    ));
}

/// Emit a Ruby `module` wrapper for a listener. The module exposes:
///   - `register(&block)`: wraps the block in a Proc that strips the C
///     context pointer, calls `{c_prefix}_{module}_register_{listener}`,
///     pins the Proc in `@@callbacks` keyed by the returned id so Ruby's
///     GC cannot reclaim it while the native side still holds a pointer,
///     and returns the id.
///   - `unregister(id)`: calls `{c_prefix}_{module}_unregister_{listener}`
///     and drops the Proc from `@@callbacks`.
fn render_listener_module(
    out: &mut String,
    module_path: &str,
    l: &ListenerDef,
    rb_module_name: &str,
    c_prefix: &str,
) {
    let class_name = l.name.to_upper_camel_case();
    let reg_fn = format!("{c_prefix}_{module_path}_register_{}", l.name);
    let unreg_fn = format!("{c_prefix}_{module_path}_unregister_{}", l.name);

    out.push_str(&format!("\n  module {class_name}\n"));
    out.push_str("    @@callbacks = {}\n\n");
    out.push_str("    def self.register(&block)\n");
    out.push_str("      cb = proc { |_ctx, *args| block.call(*args) }\n");
    out.push_str(&format!(
        "      id = {rb_module_name}.{reg_fn}(cb, FFI::Pointer::NULL)\n"
    ));
    out.push_str("      @@callbacks[id] = cb\n");
    out.push_str("      id\n");
    out.push_str("    end\n\n");
    out.push_str("    def self.unregister(id)\n");
    out.push_str(&format!("      {rb_module_name}.{unreg_fn}(id)\n"));
    out.push_str("      @@callbacks.delete(id)\n");
    out.push_str("    end\n");
    out.push_str("  end\n");
}

fn render_struct_ffi(out: &mut String, module_name: &str, s: &StructDef, c_prefix: &str) {
    let prefix = format!("{c_prefix}_{}_{}", module_name, s.name);
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

fn render_attach_function(out: &mut String, module_name: &str, f: &Function, c_prefix: &str) {
    let c_sym = format!("{c_prefix}_{module_name}_{}", f.name);
    let mut argtypes: Vec<String> = Vec::new();
    for p in &f.params {
        argtypes.extend(rb_param_ffi_types(&p.ty));
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
    if let Some(TypeRef::Iterator(_)) = &f.returns {
        let iter_tag = iter_type_name(c_prefix, module_name, &f.name);
        out.push_str(&format!(
            "  attach_function :{iter_tag}_next, [:pointer, :pointer, :pointer], :int32\n"
        ));
        out.push_str(&format!(
            "  attach_function :{iter_tag}_destroy, [:pointer], :void\n"
        ));
    }
}

fn render_struct_class(
    out: &mut String,
    api_module_name: &str,
    s: &StructDef,
    rb_module_name: &str,
    c_prefix: &str,
) {
    let prefix = format!("{c_prefix}_{}_{}", api_module_name, s.name);

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
        render_getter(out, &prefix, field, rb_module_name, c_prefix);
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

fn render_getter(
    out: &mut String,
    prefix: &str,
    field: &StructField,
    rb_module_name: &str,
    c_prefix: &str,
) {
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
        render_return_code(out, &field.ty, ind, Some(rb_module_name), c_prefix);
    } else {
        out.push_str(&format!(
            "{ind}result = {rb_module_name}.{getter}(@handle)\n"
        ));
        render_return_code(out, &field.ty, ind, Some(rb_module_name), c_prefix);
    }

    out.push_str("    end\n");
}

fn render_function_wrapper(out: &mut String, module_name: &str, f: &Function, c_prefix: &str) {
    let c_sym = format!("{c_prefix}_{module_name}_{}", f.name);
    let func_name = f.name.to_snake_case();
    let ind = "    ";

    let params: Vec<String> = f.params.iter().map(|p| p.name.to_snake_case()).collect();
    out.push_str(&format!(
        "\n  def self.{}({})\n",
        func_name,
        params.join(", ")
    ));

    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('"', "\\\"");
        out.push_str(&format!("{ind}warn \"[DEPRECATED] {escaped}\"\n"));
    }

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
        } else if let TypeRef::Iterator(inner) = ret_ty {
            render_iterator_return(out, module_name, &f.name, inner, ind, c_prefix);
        } else {
            render_return_code(out, ret_ty, ind, None, c_prefix);
        }
    }

    out.push_str("  end\n");
}

// ── Async rendering ──

/// FFI types for the C async callback parameters: `(context, err, result...)`.
/// Strings are represented as `:pointer` (not `:string`) so the wrapper can
/// call `weaveffi_free_string` after copying the payload into Ruby memory.
fn rb_async_cb_param_ffi_types(ret: &Option<TypeRef>) -> Vec<String> {
    let mut v = vec![":pointer".into(), ":pointer".into()];
    match ret {
        None => {}
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_)) => {
            v.push(":pointer".into());
            v.push(":size_t".into());
        }
        Some(TypeRef::Map(_, _)) => {
            v.push(":pointer".into());
            v.push(":pointer".into());
            v.push(":size_t".into());
        }
        Some(TypeRef::Optional(inner)) if !is_c_pointer_type(inner) => {
            v.push(":pointer".into());
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            v.push(":pointer".into());
        }
        Some(ty) => {
            v.push(rb_ffi_scalar(ty).into());
        }
    }
    v
}

/// Ruby block parameter names mirroring `rb_async_cb_param_ffi_types`.
fn rb_async_cb_param_names(ret: &Option<TypeRef>) -> Vec<&'static str> {
    let mut v = vec!["ctx", "err_ptr"];
    match ret {
        None => {}
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes | TypeRef::List(_)) => {
            v.push("result");
            v.push("result_len");
        }
        Some(TypeRef::Map(_, _)) => {
            v.push("result_keys");
            v.push("result_values");
            v.push("result_len");
        }
        _ => {
            v.push("result");
        }
    }
    v
}

fn render_async_attach_function(out: &mut String, module_name: &str, f: &Function, c_prefix: &str) {
    let c_sym = format!("{c_prefix}_{module_name}_{}", f.name);
    let cb_name = format!("{c_sym}_callback");
    let async_fn = format!("{c_sym}_async");

    let cb_types = rb_async_cb_param_ffi_types(&f.returns);
    out.push_str(&format!(
        "  callback :{cb_name}, [{}], :void\n",
        cb_types.join(", ")
    ));

    let mut argtypes: Vec<String> = Vec::new();
    for p in &f.params {
        argtypes.extend(rb_param_ffi_types(&p.ty));
    }
    if f.cancellable {
        argtypes.push(":pointer".into());
    }
    argtypes.push(format!(":{cb_name}"));
    argtypes.push(":pointer".into());
    out.push_str(&format!(
        "  attach_function :{async_fn}, [{}], :void\n",
        argtypes.join(", ")
    ));
}

fn render_async_result_conversion(
    out: &mut String,
    ret: &Option<TypeRef>,
    ind: &str,
    c_prefix: &str,
) {
    match ret {
        None => {
            out.push_str(&format!("{ind}ruby_result = nil\n"));
        }
        Some(ty) => render_async_result_from_type(out, ty, ind, c_prefix),
    }
}

fn render_async_result_from_type(out: &mut String, ty: &TypeRef, ind: &str, c_prefix: &str) {
    match ty {
        TypeRef::I32
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::F64
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            out.push_str(&format!("{ind}ruby_result = result\n"));
        }
        TypeRef::Bool => {
            out.push_str(&format!("{ind}ruby_result = result != 0\n"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = ''\n"));
            out.push_str(&format!("{ind}else\n"));
            out.push_str(&format!("{ind}  ruby_result = result.read_string\n"));
            out.push_str(&format!("{ind}  {c_prefix}_free_string(result)\n"));
            out.push_str(&format!("{ind}end\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = ''.b\n"));
            out.push_str(&format!("{ind}else\n"));
            out.push_str(&format!(
                "{ind}  ruby_result = result.read_string(result_len)\n"
            ));
            out.push_str(&format!(
                "{ind}  {c_prefix}_free_bytes(result, result_len)\n"
            ));
            out.push_str(&format!("{ind}end\n"));
        }
        TypeRef::Struct(name) => {
            out.push_str(&format!(
                "{ind}ruby_result = {}.new(result)\n",
                local_type_name(name)
            ));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("{ind}ruby_result = {name}.new(result)\n"));
        }
        TypeRef::List(inner) => {
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = []\n"));
            out.push_str(&format!("{ind}else\n"));
            let reader = rb_array_reader(inner);
            match inner.as_ref() {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    out.push_str(&format!(
                        "{ind}  ruby_result = result.{reader}(result_len).map {{ |p| p.null? ? '' : p.read_string }}\n"
                    ));
                }
                TypeRef::TypedHandle(n) => {
                    out.push_str(&format!(
                        "{ind}  ruby_result = result.{reader}(result_len).map {{ |p| {n}.new(p) }}\n"
                    ));
                }
                TypeRef::Struct(n) => {
                    let local = local_type_name(n);
                    out.push_str(&format!(
                        "{ind}  ruby_result = result.{reader}(result_len).map {{ |p| {local}.new(p) }}\n"
                    ));
                }
                TypeRef::Bool => {
                    out.push_str(&format!(
                        "{ind}  ruby_result = result.{reader}(result_len).map {{ |v| v != 0 }}\n"
                    ));
                }
                _ => {
                    out.push_str(&format!(
                        "{ind}  ruby_result = result.{reader}(result_len)\n"
                    ));
                }
            }
            out.push_str(&format!("{ind}end\n"));
        }
        TypeRef::Map(k, v) => {
            out.push_str(&format!(
                "{ind}if result_keys.null? || result_values.null?\n"
            ));
            out.push_str(&format!("{ind}  ruby_result = {{}}\n"));
            out.push_str(&format!("{ind}else\n"));
            let k_reader = rb_array_reader(k);
            let v_reader = rb_array_reader(v);
            let k_expr = rb_element_expr("k", k);
            let v_expr = rb_element_expr("v", v);
            out.push_str(&format!(
                "{ind}  ruby_result = result_keys.{k_reader}(result_len).zip(result_values.{v_reader}(result_len))\
                 .each_with_object({{}}) {{ |(k, v), h| h[{k_expr}] = {v_expr} }}\n"
            ));
            out.push_str(&format!("{ind}end\n"));
        }
        TypeRef::Optional(inner) => render_async_optional_from_type(out, inner, ind, c_prefix),
        TypeRef::Iterator(_) => unreachable!("iterator return is not valid for async functions"),
        TypeRef::Callback(_) => unreachable!("callback return is not valid for async functions"),
    }
}

fn render_async_optional_from_type(out: &mut String, inner: &TypeRef, ind: &str, c_prefix: &str) {
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = nil\n"));
            out.push_str(&format!("{ind}else\n"));
            out.push_str(&format!("{ind}  ruby_result = result.read_string\n"));
            out.push_str(&format!("{ind}  {c_prefix}_free_string(result)\n"));
            out.push_str(&format!("{ind}end\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = nil\n"));
            out.push_str(&format!("{ind}else\n"));
            out.push_str(&format!(
                "{ind}  ruby_result = result.read_string(result_len)\n"
            ));
            out.push_str(&format!(
                "{ind}  {c_prefix}_free_bytes(result, result_len)\n"
            ));
            out.push_str(&format!("{ind}end\n"));
        }
        TypeRef::Struct(name) => {
            let local = local_type_name(name);
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = nil\n"));
            out.push_str(&format!("{ind}else\n"));
            out.push_str(&format!("{ind}  ruby_result = {local}.new(result)\n"));
            out.push_str(&format!("{ind}end\n"));
        }
        TypeRef::TypedHandle(name) => {
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = nil\n"));
            out.push_str(&format!("{ind}else\n"));
            out.push_str(&format!("{ind}  ruby_result = {name}.new(result)\n"));
            out.push_str(&format!("{ind}end\n"));
        }
        _ if !is_c_pointer_type(inner) => {
            let read = rb_read_method(inner);
            out.push_str(&format!("{ind}if result.null?\n"));
            out.push_str(&format!("{ind}  ruby_result = nil\n"));
            out.push_str(&format!("{ind}else\n"));
            match inner {
                TypeRef::Bool => {
                    out.push_str(&format!("{ind}  ruby_result = result.read_int32 != 0\n"));
                }
                _ => {
                    out.push_str(&format!("{ind}  ruby_result = result.{read}\n"));
                }
            }
            out.push_str(&format!("{ind}end\n"));
        }
        _ => {
            out.push_str(&format!("{ind}ruby_result = result\n"));
        }
    }
}

fn render_async_function_wrapper(
    out: &mut String,
    module_name: &str,
    f: &Function,
    c_prefix: &str,
) {
    let c_sym = format!("{c_prefix}_{module_name}_{}", f.name);
    let async_fn = format!("{c_sym}_async");
    let func_name = f.name.to_snake_case();
    let ind = "    ";

    let params: Vec<String> = f.params.iter().map(|p| p.name.to_snake_case()).collect();
    let params_comma = if params.is_empty() { "" } else { ", " };
    let kw_sep = if f.cancellable {
        if params.is_empty() {
            ""
        } else {
            ", "
        }
    } else {
        ""
    };
    let cancellable_kw = if f.cancellable {
        "cancellation: nil"
    } else {
        ""
    };

    out.push_str(&format!(
        "\n  def self.{func_name}_async({}{kw_sep}{cancellable_kw}{params_comma}&block)\n",
        params.join(", ")
    ));

    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('"', "\\\"");
        out.push_str(&format!("{ind}warn \"[DEPRECATED] {escaped}\"\n"));
    }

    for p in &f.params {
        render_param_conversion(out, &p.name.to_snake_case(), &p.ty, ind);
    }

    if f.cancellable {
        // Create the native cancel token and register a handler on the
        // `Concurrent::Cancellation` origin (when provided) so its `cancel`
        // forwards to `{c_prefix}_cancel_token_cancel`.
        out.push_str(&format!(
            "{ind}cancel_tok = {c_prefix}_cancel_token_create\n"
        ));
        out.push_str(&format!("{ind}if cancellation\n"));
        out.push_str(&format!(
            "{ind}  cancellation.origin.on_completion {{ {c_prefix}_cancel_token_cancel(cancel_tok) }}\n"
        ));
        out.push_str(&format!("{ind}end\n"));
    }

    let pipe_names = rb_async_cb_param_names(&f.returns).join(", ");
    out.push_str(&format!("{ind}cb = proc do |{pipe_names}|\n"));
    out.push_str(&format!("{ind}  begin\n"));
    out.push_str(&format!("{ind}    err_struct = ErrorStruct.new(err_ptr)\n"));
    out.push_str(&format!("{ind}    if !err_struct[:code].zero?\n"));
    out.push_str(&format!("{ind}      code = err_struct[:code]\n"));
    out.push_str(&format!("{ind}      msg_ptr = err_struct[:message]\n"));
    out.push_str(&format!(
        "{ind}      msg = msg_ptr.null? ? '' : msg_ptr.read_string\n"
    ));
    out.push_str(&format!("{ind}      {c_prefix}_error_clear(err_ptr)\n"));
    out.push_str(&format!(
        "{ind}      block.call(nil, Error.new(code, msg))\n"
    ));
    out.push_str(&format!("{ind}    else\n"));
    render_async_result_conversion(out, &f.returns, &format!("{ind}      "), c_prefix);
    out.push_str(&format!("{ind}      block.call(ruby_result, nil)\n"));
    out.push_str(&format!("{ind}    end\n"));
    out.push_str(&format!("{ind}  ensure\n"));
    out.push_str(&format!("{ind}    pop_async_callback(ctx.address)\n"));
    if f.cancellable {
        out.push_str(&format!(
            "{ind}    {c_prefix}_cancel_token_destroy(cancel_tok)\n"
        ));
    }
    out.push_str(&format!("{ind}  end\n"));
    out.push_str(&format!("{ind}end\n"));

    out.push_str(&format!("{ind}id = register_async_callback(cb)\n"));

    let mut call_args: Vec<String> = Vec::new();
    for p in &f.params {
        call_args.extend(rb_call_args(&p.name.to_snake_case(), &p.ty));
    }
    if f.cancellable {
        call_args.push("cancel_tok".into());
    }
    call_args.push("cb".into());
    call_args.push("FFI::Pointer.new(id)".into());

    out.push_str(&format!("{ind}{async_fn}({})\n", call_args.join(", ")));
    out.push_str("  end\n");

    let sync_kw_sep = kw_sep;
    out.push_str(&format!(
        "\n  def self.{func_name}({}{sync_kw_sep}{cancellable_kw})\n",
        params.join(", ")
    ));
    out.push_str(&format!("{ind}Concurrent::Promise.execute do\n"));
    out.push_str(&format!("{ind}  queue = Queue.new\n"));
    let forward_kw = if f.cancellable {
        if params.is_empty() {
            "cancellation: cancellation"
        } else {
            ", cancellation: cancellation"
        }
    } else {
        ""
    };
    out.push_str(&format!(
        "{ind}  {func_name}_async({}{forward_kw}) do |result, err|\n",
        params.join(", ")
    ));
    out.push_str(&format!("{ind}    queue.push([result, err])\n"));
    out.push_str(&format!("{ind}  end\n"));
    out.push_str(&format!("{ind}  result, err = queue.pop\n"));
    out.push_str(&format!("{ind}  raise err if err\n"));
    out.push_str(&format!("{ind}  result\n"));
    out.push_str(&format!("{ind}end\n"));
    out.push_str("  end\n");
}

fn render_iterator_return(
    out: &mut String,
    module_name: &str,
    func_name: &str,
    inner: &TypeRef,
    ind: &str,
    c_prefix: &str,
) {
    let iter_tag = iter_type_name(c_prefix, module_name, func_name);
    let item_mem = rb_mem_type(inner);
    let item_expr = rb_iter_item_expr(inner);

    out.push_str(&format!("{ind}iter_ptr = result\n"));
    out.push_str(&format!("{ind}Enumerator.new do |y|\n"));
    out.push_str(&format!("{ind}  begin\n"));
    out.push_str(&format!("{ind}    loop do\n"));
    out.push_str(&format!(
        "{ind}      out_item = FFI::MemoryPointer.new({item_mem})\n"
    ));
    out.push_str(&format!("{ind}      item_err = ErrorStruct.new\n"));
    out.push_str(&format!(
        "{ind}      has_item = {iter_tag}_next(iter_ptr, out_item, item_err)\n"
    ));
    out.push_str(&format!("{ind}      check_error!(item_err)\n"));
    out.push_str(&format!("{ind}      break if has_item.zero?\n"));
    out.push_str(&format!("{ind}      y << {item_expr}\n"));
    out.push_str(&format!("{ind}    end\n"));
    out.push_str(&format!("{ind}  ensure\n"));
    out.push_str(&format!("{ind}    {iter_tag}_destroy(iter_ptr)\n"));
    out.push_str(&format!("{ind}  end\n"));
    out.push_str(&format!("{ind}end\n"));
}

// ── Parameter conversion ──

fn render_param_conversion(out: &mut String, name: &str, ty: &TypeRef, ind: &str) {
    match ty {
        TypeRef::Bool => {
            out.push_str(&format!("{ind}{name}_c = {name} ? 1 : 0\n"));
        }
        TypeRef::StringUtf8 => {
            out.push_str(&format!(
                "{ind}{name}_buf = FFI::MemoryPointer.from_string({name}.b)\n"
            ));
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
            TypeRef::StringUtf8 => {
                out.push_str(&format!("{ind}if {name}.nil?\n"));
                out.push_str(&format!("{ind}  {name}_buf = FFI::Pointer::NULL\n"));
                out.push_str(&format!("{ind}  {name}_len = 0\n"));
                out.push_str(&format!("{ind}else\n"));
                out.push_str(&format!(
                    "{ind}  {name}_buf = FFI::MemoryPointer.from_string({name}.b)\n"
                ));
                out.push_str(&format!("{ind}  {name}_len = {name}.bytesize\n"));
                out.push_str(&format!("{ind}end\n"));
            }
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

fn render_return_code(
    out: &mut String,
    ty: &TypeRef,
    ind: &str,
    qualifier: Option<&str>,
    c_prefix: &str,
) {
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
            out.push_str(&format!("{ind}{m}{c_prefix}_free_string(result)\n"));
            out.push_str(&format!("{ind}str\n"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            out.push_str(&format!("{ind}return ''.b if result.null?\n"));
            out.push_str(&format!("{ind}len = out_len.read(:size_t)\n"));
            out.push_str(&format!("{ind}data = result.read_string(len)\n"));
            out.push_str(&format!("{ind}{m}{c_prefix}_free_bytes(result, len)\n"));
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
        TypeRef::Optional(inner) => {
            render_optional_return_code(out, inner, ind, qualifier, c_prefix)
        }
        TypeRef::List(inner) => {
            out.push_str(&format!("{ind}return [] if result.null?\n"));
            render_list_return_body(out, inner, ind);
        }
        TypeRef::Iterator(_) => {
            unreachable!("iterator returns are handled in render_function_wrapper")
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
    c_prefix: &str,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            out.push_str(&format!("{ind}return nil if result.null?\n"));
            out.push_str(&format!("{ind}str = result.read_string\n"));
            out.push_str(&format!("{ind}{m}{c_prefix}_free_string(result)\n"));
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
            out.push_str(&format!("{ind}{m}{c_prefix}_free_bytes(result, len)\n"));
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

fn render_gemspec(gem_name: &str, has_async: bool) -> String {
    let extra_dep = if has_async {
        "  s.add_dependency 'concurrent-ruby', '~> 1.1'\n"
    } else {
        ""
    };
    format!(
        "Gem::Specification.new do |s|
  s.name        = '{gem_name}'
  s.version     = '0.1.0'
  s.summary     = 'Ruby FFI bindings for {gem_name} (auto-generated)'
  s.files       = Dir['lib/**/*.rb']
  s.require_paths = ['lib']

  s.add_dependency 'ffi', '~> 1.15'
{extra_dep}end
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
                deprecated: None,
                since: None,
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
                out_dir.join("ruby/lib/weaveffi.rb").to_string(),
                out_dir.join("ruby/weaveffi.gemspec").to_string(),
                out_dir.join("ruby/README.md").to_string(),
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

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                        default: None,
                    },
                    StructField {
                        name: "name".into(),
                        ty: TypeRef::StringUtf8,
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

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
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

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
        assert!(code.contains("Item.new(result)"), "struct wrap: {code}");
        assert!(
            code.contains("raise Error.new(-1, 'null pointer') if result.null?"),
            "null ptr: {code}"
        );
    }

    #[test]
    fn ruby_async_emits_block_and_promise_versions() {
        let api = make_api(vec![simple_module(
            "io",
            vec![Function {
                name: "read".into(),
                params: vec![Param {
                    name: "path".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            code.contains("require 'concurrent'"),
            "concurrent-ruby must be required when async functions are present: {code}"
        );
        assert!(
            code.contains("@@async_callbacks = {}"),
            "preamble must declare the async callback registry: {code}"
        );
        assert!(
            code.contains("def self.register_async_callback(cb)"),
            "preamble must expose register_async_callback helper: {code}"
        );
        assert!(
            code.contains("def self.pop_async_callback(id)"),
            "preamble must expose pop_async_callback helper: {code}"
        );
        assert!(
            code.contains(
                "callback :weaveffi_io_read_callback, [:pointer, :pointer, :pointer], :void"
            ),
            "must declare the async C callback typedef with (ctx, err, result): {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_io_read_async, \
                 [:pointer, :size_t, :weaveffi_io_read_callback, :pointer], :void"
            ),
            "must attach the async C function with callback and context: {code}"
        );
        assert!(
            code.contains("def self.read_async(path, &block)"),
            "must emit block-based async wrapper: {code}"
        );
        assert!(
            code.contains("id = register_async_callback(cb)"),
            "block wrapper must pin the proc in the registry: {code}"
        );
        assert!(
            code.contains(
                "weaveffi_io_read_async(path_buf, path.bytesize, cb, FFI::Pointer.new(id))"
            ),
            "block wrapper must call the async C function with the id as context: {code}"
        );
        assert!(
            code.contains("pop_async_callback(ctx.address)"),
            "callback proc must unpin itself from the registry using ctx.address: {code}"
        );
        assert!(
            code.contains("block.call(nil, Error.new(code, msg))"),
            "callback proc must deliver errors to the block: {code}"
        );
        assert!(
            code.contains("block.call(ruby_result, nil)"),
            "callback proc must deliver the result to the block on success: {code}"
        );

        assert!(
            code.contains("def self.read(path)"),
            "must emit Promise-returning wrapper: {code}"
        );
        assert!(
            code.contains("Concurrent::Promise.execute do"),
            "Promise wrapper must use Concurrent::Promise.execute: {code}"
        );
        assert!(
            code.contains("read_async(path) do |result, err|"),
            "Promise wrapper must delegate to the block-based async variant: {code}"
        );
        assert!(
            code.contains("raise err if err"),
            "Promise wrapper must re-raise errors: {code}"
        );
    }

    #[test]
    fn ruby_cancellable_async_wires_concurrent_cancellation_to_token() {
        let api = make_api(vec![simple_module(
            "tasks",
            vec![
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
            ],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            code.contains("attach_function :weaveffi_cancel_token_create, [], :pointer"),
            "must expose weaveffi_cancel_token_create via FFI: {code}"
        );
        assert!(
            code.contains("attach_function :weaveffi_cancel_token_cancel, [:pointer], :void"),
            "must expose weaveffi_cancel_token_cancel via FFI: {code}"
        );
        assert!(
            code.contains("attach_function :weaveffi_cancel_token_destroy, [:pointer], :void"),
            "must expose weaveffi_cancel_token_destroy via FFI: {code}"
        );
        assert!(
            code.contains("def self.run_async(id, cancellation: nil, &block)"),
            "cancellable async must accept cancellation keyword: {code}"
        );
        assert!(
            code.contains("cancel_tok = weaveffi_cancel_token_create"),
            "cancellable async must create a native cancel token: {code}"
        );
        assert!(
            code.contains(
                "cancellation.origin.on_completion { weaveffi_cancel_token_cancel(cancel_tok) }"
            ),
            "cancellation.origin completion must call weaveffi_cancel_token_cancel: {code}"
        );
        assert!(
            code.contains("weaveffi_tasks_run_async(id, cancel_tok, cb, FFI::Pointer.new(id))"),
            "cancellable async must forward the native token to _async: {code}"
        );
        assert!(
            code.contains("weaveffi_cancel_token_destroy(cancel_tok)"),
            "cancellable async must destroy the native token on completion: {code}"
        );
        assert!(
            code.contains("def self.run(id, cancellation: nil)"),
            "synchronous-style wrapper must forward the cancellation keyword: {code}"
        );

        let fire_sig = code
            .lines()
            .find(|l| l.contains("def self.fire_async("))
            .expect("non-cancellable fire_async wrapper must still be emitted");
        assert!(
            !fire_sig.contains("cancellation:"),
            "non-cancellable async must not accept cancellation: {fire_sig}"
        );
    }

    #[test]
    fn ruby_async_gemspec_includes_concurrent_ruby() {
        let api = make_api(vec![simple_module(
            "io",
            vec![Function {
                name: "read".into(),
                params: vec![],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        RubyGenerator.generate(&api, out_dir).unwrap();

        let gemspec = std::fs::read_to_string(out_dir.join("ruby/weaveffi.gemspec")).unwrap();
        assert!(
            gemspec.contains("s.add_dependency 'concurrent-ruby', '~> 1.1'"),
            "gemspec must add concurrent-ruby dependency when async is present: {gemspec}"
        );
    }

    #[test]
    fn ruby_sync_only_gemspec_omits_concurrent_ruby() {
        let api = make_api(vec![simple_module(
            "math",
            vec![Function {
                name: "add".into(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        RubyGenerator.generate(&api, out_dir).unwrap();

        let gemspec = std::fs::read_to_string(out_dir.join("ruby/weaveffi.gemspec")).unwrap();
        assert!(
            !gemspec.contains("concurrent-ruby"),
            "sync-only gemspec must not pull in concurrent-ruby: {gemspec}"
        );
        let rb = std::fs::read_to_string(out_dir.join("ruby/lib/weaveffi.rb")).unwrap();
        assert!(
            !rb.contains("require 'concurrent'"),
            "sync-only module must not require concurrent: {rb}"
        );
        assert!(
            !rb.contains("@@async_callbacks"),
            "sync-only module must not emit async registry: {rb}"
        );
    }

    #[test]
    fn preamble_has_platform_detection() {
        let code = render_ruby_module(&make_api(vec![]), "WeaveFFI", "weaveffi");
        assert!(code.contains("FFI::Platform::OS"), "platform: {code}");
        assert!(code.contains("libweaveffi.dylib"), "darwin: {code}");
        assert!(code.contains("weaveffi.dll"), "windows: {code}");
        assert!(code.contains("libweaveffi.so"), "linux: {code}");
    }

    #[test]
    fn error_class_structure() {
        let code = render_ruby_module(&make_api(vec![]), "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Item".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I64,
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

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");
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
                    doc: None,
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
                deprecated: None,
                since: None,
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
                deprecated: None,
                since: None,
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
                        default: None,
                    },
                    StructField {
                        name: "last_name".into(),
                        ty: TypeRef::StringUtf8,
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
                deprecated: None,
                since: None,
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
                    deprecated: None,
                    since: None,
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
                    deprecated: None,
                    since: None,
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
                    deprecated: None,
                    since: None,
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
                    deprecated: None,
                    since: None,
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
                deprecated: None,
                since: None,
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

        let rb = render_ruby_module(&api, "WeaveFFI", "weaveffi");

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
    fn ruby_string_param_uses_pointer_and_length() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "set_name".into(),
                params: vec![Param {
                    name: "name".into(),
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
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            code.contains(
                "attach_function :weaveffi_data_set_name, [:pointer, :size_t, :pointer], :void"
            ),
            "StringUtf8 param attach_function uses pointer + size_t: {code}"
        );
        assert!(
            !code.contains("attach_function :weaveffi_data_set_name, [:string"),
            "StringUtf8 param must not use :string: {code}"
        );
        assert!(
            code.contains("name_buf = FFI::MemoryPointer.from_string(name.b)"),
            "wrapper allocates buffer via MemoryPointer.from_string with .b: {code}"
        );
        assert!(
            code.contains("weaveffi_data_set_name(name_buf, name.bytesize, err)"),
            "wrapper calls C with (buf, bytesize, err): {code}"
        );
    }

    #[test]
    fn ruby_optional_string_param_uses_pointer_and_length() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "maybe_set".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    mutable: false,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            code.contains(
                "attach_function :weaveffi_data_maybe_set, [:pointer, :size_t, :pointer], :void"
            ),
            "Optional<StringUtf8> param attach_function uses pointer + size_t: {code}"
        );
        assert!(
            code.contains("name_buf = FFI::Pointer::NULL"),
            "nil branch passes NULL pointer: {code}"
        );
        assert!(
            code.contains("name_buf = FFI::MemoryPointer.from_string(name.b)"),
            "non-nil branch allocates via MemoryPointer.from_string with .b: {code}"
        );
        assert!(
            code.contains("weaveffi_data_maybe_set(name_buf, name_len, err)"),
            "wrapper calls C with (buf, len, err): {code}"
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

        let rb = render_ruby_module(&api, "WeaveFFI", "weaveffi");

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

    #[test]
    fn ruby_bytes_param_uses_canonical_shape() {
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
        let rb = render_ruby_module(&api, "WeaveFFI", "weaveffi");
        assert!(
            rb.contains("attach_function :weaveffi_io_send, [:pointer, :size_t, :pointer], :void"),
            "Ruby attach_function for Bytes param must lower to (:pointer, :size_t) + (:pointer err): {rb}"
        );
        assert!(
            rb.contains("payload_buf = FFI::MemoryPointer.new(:uint8, payload.bytesize)"),
            "Ruby wrapper must allocate a uint8 MemoryPointer sized to payload: {rb}"
        );
        assert!(
            rb.contains("payload_buf.put_bytes(0, payload)"),
            "Ruby wrapper must copy payload bytes into the native buffer: {rb}"
        );
        assert!(
            rb.contains("weaveffi_io_send(payload_buf, payload.bytesize, err)"),
            "Ruby wrapper must call C with (ptr, len, err) for Bytes param: {rb}"
        );
    }

    #[test]
    fn ruby_bytes_return_uses_canonical_shape() {
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
        let rb = render_ruby_module(&api, "WeaveFFI", "weaveffi");
        assert!(
            rb.contains("attach_function :weaveffi_io_read, [:pointer, :pointer], :pointer"),
            "Ruby attach_function for Bytes return must add :pointer out_len + :pointer err and return :pointer: {rb}"
        );
        assert!(
            rb.contains("attach_function :weaveffi_free_bytes, [:pointer, :size_t], :void"),
            "Ruby must declare weaveffi_free_bytes with (:pointer, :size_t) (no const): {rb}"
        );
        assert!(
            rb.contains("out_len = FFI::MemoryPointer.new(:size_t)"),
            "Ruby wrapper must allocate out_len MemoryPointer: {rb}"
        );
        assert!(
            rb.contains("result = weaveffi_io_read(out_len, err)"),
            "Ruby wrapper must call C with (out_len, err) for Bytes return: {rb}"
        );
        assert!(
            rb.contains("len = out_len.read(:size_t)"),
            "Ruby wrapper must read out_len as :size_t: {rb}"
        );
        assert!(
            rb.contains("data = result.read_string(len)"),
            "Ruby wrapper must copy returned bytes via result.read_string(len): {rb}"
        );
        assert!(
            rb.contains("weaveffi_free_bytes(result, len)"),
            "Ruby wrapper must free returned bytes via weaveffi_free_bytes(result, len): {rb}"
        );
    }

    #[test]
    fn ruby_check_error_calls_weaveffi_error_clear() {
        let api = make_api(vec![Module {
            name: "math".into(),
            functions: vec![Function {
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
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let rb = render_ruby_module(&api, "WeaveFFI", "weaveffi");
        let def_pos = rb
            .find("def self.check_error!(err)")
            .expect("check_error! must be defined");
        let msg_pos = rb[def_pos..]
            .find("msg = msg_ptr.null? ? '' : msg_ptr.read_string")
            .map(|p| p + def_pos)
            .expect("check_error! must capture msg_ptr.read_string into msg");
        let clear_pos = rb[def_pos..]
            .find("weaveffi_error_clear(err.to_ptr)")
            .map(|p| p + def_pos)
            .expect("check_error! must call weaveffi_error_clear after capturing the message");
        let raise_pos = rb[def_pos..]
            .find("raise Error.new(code, msg)")
            .map(|p| p + def_pos)
            .expect("check_error! must raise after clearing");
        assert!(
            msg_pos < clear_pos,
            "weaveffi_error_clear must run AFTER capturing msg_ptr.read_string: {rb}"
        );
        assert!(
            clear_pos < raise_pos,
            "weaveffi_error_clear must run BEFORE raising: {rb}"
        );
    }

    #[test]
    fn ruby_bytes_return_calls_free_bytes() {
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
        let rb = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        let copy_pos = rb
            .find("data = result.read_string(len)")
            .expect("Ruby wrapper must copy the returned bytes via result.read_string(len)");
        let free_pos = rb
            .find("weaveffi_free_bytes(result, len)")
            .expect("Ruby wrapper must free the returned pointer via weaveffi_free_bytes");
        assert!(
            copy_pos < free_pos,
            "weaveffi_free_bytes must run AFTER read_string has copied the payload: {rb}"
        );
    }

    #[test]
    fn ruby_struct_wrapper_calls_destroy() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::I32,
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
        let rb = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            rb.contains("attach_function :weaveffi_contacts_Contact_destroy, [:pointer], :void"),
            "Ruby output must attach the native _destroy function: {rb}"
        );
        assert!(
            rb.contains("class ContactPtr < FFI::AutoPointer"),
            "Ruby output must define a ContactPtr subclass of FFI::AutoPointer: {rb}"
        );
        assert!(
            rb.contains("def self.release(ptr)"),
            "ContactPtr must define the release callback: {rb}"
        );
        assert!(
            rb.contains("WeaveFFI.weaveffi_contacts_Contact_destroy(ptr)"),
            "release must invoke the C ABI destroy function: {rb}"
        );
        assert!(
            rb.contains("@handle = ContactPtr.new(handle)"),
            "Contact#initialize must wrap handle in ContactPtr so AutoPointer owns the resource: {rb}"
        );
        assert!(
            rb.contains("def destroy"),
            "Contact must expose an explicit destroy method: {rb}"
        );
        assert!(
            rb.contains("@handle.free"),
            "Contact#destroy must call handle.free to trigger AutoPointer.release: {rb}"
        );
        assert!(
            rb.contains("@handle = nil"),
            "Contact#destroy must null out @handle for idempotency: {rb}"
        );
    }

    #[test]
    fn capabilities_is_feature_complete() {
        let caps = RubyGenerator.capabilities();
        for cap in Capability::ALL {
            assert!(caps.contains(cap), "Ruby generator must support {cap:?}");
        }
    }

    #[test]
    fn ruby_emits_callback_via_ffi_callback() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![Function {
                name: "subscribe".into(),
                params: vec![Param {
                    name: "handler".into(),
                    ty: TypeRef::Callback("OnData".into()),
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
                name: "OnData".into(),
                params: vec![Param {
                    name: "value".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            code.contains("callback :OnData, [:pointer, :int32], :void"),
            "Ruby must declare the callback via FFI's callback macro \
             with a leading :pointer context and user-defined params: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_events_subscribe, \
                 [:OnData, :pointer, :pointer], :void"
            ),
            "attach_function must reference the callback type, include a :pointer \
             context arg, and the :pointer err out-param: {code}"
        );
        assert!(
            code.contains("def self.subscribe(handler)"),
            "wrapper must accept the callback Proc as its only user-facing param: {code}"
        );
        assert!(
            code.contains("weaveffi_events_subscribe(handler, FFI::Pointer::NULL, err)"),
            "wrapper must pass the Proc directly plus a NULL context \
             so FFI manages the trampoline and lifetime: {code}"
        );
    }

    #[test]
    fn ruby_emits_listener_module() {
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnMessage".into(),
                params: vec![Param {
                    name: "message".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                }],
                returns: None,
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            code.contains(
                "attach_function :weaveffi_events_register_message_listener, \
                 [:OnMessage, :pointer], :uint64"
            ),
            "register attach_function must reference the callback type \
             and return the registration id as :uint64: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_events_unregister_message_listener, \
                 [:uint64], :void"
            ),
            "unregister attach_function must take the id as :uint64 and \
             return :void: {code}"
        );
        assert!(
            code.contains("module MessageListener"),
            "listener wrapper must be a Ruby `module` named in PascalCase: {code}"
        );
        assert!(
            code.contains("@@callbacks = {}"),
            "listener module must use a class variable hash to pin Procs \
             against GC: {code}"
        );
        assert!(
            code.contains("def self.register(&block)"),
            "register must be a module method taking a block: {code}"
        );
        assert!(
            code.contains(
                "WeaveFFI.weaveffi_events_register_message_listener(cb, FFI::Pointer::NULL)"
            ),
            "register must call the C ABI register function with the Proc \
             and a NULL context pointer: {code}"
        );
        assert!(
            code.contains("@@callbacks[id] = cb"),
            "register must pin the Proc in @@callbacks keyed by the id: {code}"
        );
        assert!(
            code.contains("def self.unregister(id)"),
            "unregister must be a module method taking an id: {code}"
        );
        assert!(
            code.contains("WeaveFFI.weaveffi_events_unregister_message_listener(id)"),
            "unregister must call the C ABI unregister function with the id: {code}"
        );
        assert!(
            code.contains("@@callbacks.delete(id)"),
            "unregister must unpin the Proc from @@callbacks: {code}"
        );
    }

    #[test]
    fn ruby_iterator_return_uses_lazy_enumerator() {
        let api = make_api(vec![simple_module(
            "data",
            vec![Function {
                name: "list_items".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render_ruby_module(&api, "WeaveFFI", "weaveffi");

        assert!(
            code.contains("attach_function :weaveffi_data_list_items, [:pointer], :pointer"),
            "main function returns the iterator handle and only takes the err out-param: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_data_ListItemsIterator_next, \
                 [:pointer, :pointer, :pointer], :int32"
            ),
            "per-iterator _next must be attached with (iter, out_item, err) -> int32: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_data_ListItemsIterator_destroy, [:pointer], :void"
            ),
            "per-iterator _destroy must be attached taking the iterator handle: {code}"
        );

        let fn_start = code
            .find("def self.list_items")
            .expect("list_items wrapper");
        let fn_body = &code[fn_start..];
        let fn_end = fn_body.find("\n  end\n").unwrap();
        let fn_text = &fn_body[..fn_end];

        assert!(
            fn_text.contains("Enumerator.new do |y|"),
            "wrapper must return a lazy Enumerator, not an Array: {fn_text}"
        );
        assert!(
            fn_text.contains("loop do"),
            "Enumerator body must drive _next in a loop: {fn_text}"
        );
        assert!(
            fn_text.contains("weaveffi_data_ListItemsIterator_next(iter_ptr, out_item, item_err)"),
            "loop must invoke _next with (iter_ptr, out_item, item_err): {fn_text}"
        );
        assert!(
            fn_text.contains("break if has_item.zero?"),
            "loop must terminate when _next reports no more items: {fn_text}"
        );
        assert!(
            fn_text.contains("y << out_item.read_int32"),
            "items must be yielded (not collected) to the Enumerator: {fn_text}"
        );
        assert!(
            fn_text.contains("weaveffi_data_ListItemsIterator_destroy(iter_ptr)"),
            "wrapper must call _destroy once iteration ends: {fn_text}"
        );
        assert!(
            fn_text.contains("ensure"),
            "destroy must run in an ensure block so it fires even on early exit: {fn_text}"
        );

        assert!(
            !fn_text.contains("result.read_array_of_int32"),
            "iterator must not be materialised via read_array_of_*: {fn_text}"
        );
        assert!(
            !fn_text.contains("return [] if result.null?"),
            "iterator must not fall through the list return path: {fn_text}"
        );
    }

    #[test]
    fn ruby_ffi_lib_respects_c_prefix() {
        let api = make_api(vec![
            simple_module(
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
                    deprecated: None,
                    since: None,
                }],
            ),
            Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "find".into(),
                    params: vec![],
                    returns: Some(TypeRef::StringUtf8),
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
            },
        ]);

        let config = GeneratorConfig {
            c_prefix: Some("myffi".into()),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_ruby_c_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        RubyGenerator
            .generate_with_config(&api, out_dir, &config)
            .unwrap();

        let rb = std::fs::read_to_string(tmp.join("ruby/lib/weaveffi.rb")).unwrap();

        assert!(
            rb.contains("ffi_lib 'libmyffi.dylib'"),
            "ffi_lib must use libmyffi.dylib on macOS: {rb}"
        );
        assert!(
            rb.contains("ffi_lib 'myffi.dll'"),
            "ffi_lib must use myffi.dll on Windows: {rb}"
        );
        assert!(
            rb.contains("ffi_lib 'libmyffi.so'"),
            "ffi_lib must use libmyffi.so on Linux: {rb}"
        );
        assert!(
            !rb.contains("libweaveffi.dylib")
                && !rb.contains("'weaveffi.dll'")
                && !rb.contains("libweaveffi.so"),
            "ffi_lib must not retain default weaveffi library names: {rb}"
        );

        assert!(
            rb.contains("attach_function :myffi_error_clear"),
            "preamble attach_function must use c_prefix for error_clear: {rb}"
        );
        assert!(
            rb.contains("attach_function :myffi_free_string"),
            "preamble attach_function must use c_prefix for free_string: {rb}"
        );
        assert!(
            rb.contains("attach_function :myffi_free_bytes"),
            "preamble attach_function must use c_prefix for free_bytes: {rb}"
        );

        assert!(
            rb.contains("attach_function :myffi_math_add"),
            "function attach_function must use c_prefix: {rb}"
        );
        assert!(
            rb.contains("attach_function :myffi_contacts_Contact_destroy"),
            "struct destroy attach_function must use c_prefix: {rb}"
        );
        assert!(
            rb.contains("attach_function :myffi_contacts_Contact_get_name"),
            "struct getter attach_function must use c_prefix: {rb}"
        );

        assert!(
            rb.contains("myffi_free_string(result)"),
            "string return wrapper must call c_prefix-qualified free_string: {rb}"
        );
        assert!(
            rb.contains("myffi_error_clear(err.to_ptr)"),
            "check_error! must call c_prefix-qualified error_clear: {rb}"
        );

        assert!(
            !rb.contains("weaveffi_"),
            "no generated symbol may retain the default weaveffi_ prefix: {rb}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
