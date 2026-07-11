//! Ruby (FFI gem) binding generator for WeaveFFI.
//!
//! Emits a Ruby gem (`.gemspec` + library) using the `ffi` gem to call
//! into the C ABI exposed by the underlying cdylib. Implements
//! [`LanguageBackend`]; the shared driver bridges it into the generator
//! pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::{ToShoutySnakeCase, ToSnakeCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, AbiParam, CType};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::common::{
    emit_doc as common_emit_doc, is_c_pointer_type, DocCommentStyle,
};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    AsyncBinding, BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding,
    FieldBinding, FnBinding, InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding,
    RichVariantBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_core::platform::Platform;
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`RubyGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RubyConfig {
    /// Top-level Ruby module name (default `"WeaveFFI"`).
    pub module_name: Option<String>,
    /// Ruby gem name written into `weaveffi.gemspec` (default `"weaveffi"`).
    pub gem_name: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from
    /// emitted Ruby method names, so a `contacts` module exports
    /// `create_contact` rather than `contacts_create_contact`. Set to
    /// `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the FFI bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for RubyConfig {
    fn default() -> Self {
        Self {
            module_name: None,
            gem_name: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl RubyConfig {
    /// Returns the configured top-level Ruby module name, falling back to
    /// `"WeaveFFI"`.
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("WeaveFFI")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the configured gem name, falling back to `"weaveffi"`.
    pub fn gem_name(&self) -> &str {
        self.gem_name.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// Ruby backend: emits an `ffi`-gem package (a library module, a `.gemspec`,
/// and a README) binding the C ABI exposed by the underlying cdylib.
pub struct RubyGenerator;

impl LanguageBackend for RubyGenerator {
    type Config = RubyConfig;

    fn name(&self) -> &'static str {
        "ruby"
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
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let dir = out_dir.join("ruby");
        let lib_dir = dir.join("lib");
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.gem_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib_file = format!("{}.rb", package.ident_name());
        let gem_file = format!("{}.gemspec", package.name);
        vec![
            OutputFile::new(
                lib_dir.join(&lib_file),
                render_ruby_module(
                    model,
                    config.module_name(),
                    config.strip_module_prefix,
                    &lib_file,
                    input_basename,
                ),
            ),
            OutputFile::new(
                dir.join(&gem_file),
                render_gemspec(&package, &gem_file, input_basename),
            ),
            OutputFile::new(
                dir.join("README.md"),
                render_readme(&package, input_basename),
            ),
        ]
    }

    fn package(
        &self,
        api: &Api,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let input_basename = config.input_basename();
        let package = pkg::resolve(
            api,
            config.gem_name.as_deref(),
            config.input_basename.as_deref(),
        );
        let lib_file = format!("{}.rb", package.ident_name());
        let gem_file = format!("{}.gemspec", package.name);

        // Render the FFI module once with the bundled-first loader.
        let module_src = render_ruby_module(
            model,
            config.module_name(),
            config.strip_module_prefix,
            &lib_file,
            input_basename,
        )
        .replace(
            RUBY_LOADER_ORIGINAL,
            &ruby_loader_packaged(&ctx.binaries.lib_name),
        );
        let readme = render_packaged_readme(&package, input_basename);

        let ruby_dir = out_dir.join("ruby");
        let mut files = Vec::new();
        for nb in &ctx.binaries.binaries {
            let platform = nb.platform;
            let gem_dir = ruby_dir.join(platform.id());
            let lib_dir = gem_dir.join("lib");
            files.push(PackagedFile::text(
                lib_dir.join(&lib_file),
                module_src.clone(),
            ));
            files.push(PackagedFile::copy(
                lib_dir
                    .join("native")
                    .join(ctx.binaries.bundled_filename(platform)),
                nb.source.clone(),
            ));
            files.push(PackagedFile::text(
                gem_dir.join(&gem_file),
                render_packaged_gemspec(&package, &gem_file, platform, input_basename),
            ));
            files.push(PackagedFile::text(
                gem_dir.join("README.md"),
                readme.clone(),
            ));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(RubyGenerator);

/// The exact `ffi_lib` loader block `render_ruby_module` emits in `generate`
/// mode, so the packager can swap it for a bundled-first variant.
const RUBY_LOADER_ORIGINAL: &str = r#"  # An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a
  # specific build artifact regardless of its file name or location.
  _wv_override = ENV['WEAVEFFI_LIBRARY']
  if _wv_override && !_wv_override.empty?
    ffi_lib _wv_override
  else
    case FFI::Platform::OS
    when /darwin/
      ffi_lib 'libweaveffi.dylib'
    when /mswin|mingw/
      ffi_lib 'weaveffi.dll'
    else
      ffi_lib 'libweaveffi.so'
    end
  end"#;

/// The packaged `ffi_lib` loader for `lib`: prefer the per-platform library
/// bundled under `lib/native/`, then `WEAVEFFI_LIBRARY`, then the system path.
fn ruby_loader_packaged(lib: &str) -> String {
    format!(
        r#"  # A bundled per-platform library ships inside this gem; prefer it so the gem
  # works with no external setup. WEAVEFFI_LIBRARY still overrides.
  _wv_override = ENV['WEAVEFFI_LIBRARY']
  if _wv_override && !_wv_override.empty?
    ffi_lib _wv_override
  else
    case FFI::Platform::OS
    when /darwin/
      _wv_name = 'lib{lib}.dylib'
    when /mswin|mingw/
      _wv_name = '{lib}.dll'
    else
      _wv_name = 'lib{lib}.so'
    end
    _wv_bundled = File.join(__dir__, 'native', _wv_name)
    ffi_lib(File.exist?(_wv_bundled) ? _wv_bundled : _wv_name)
  end"#
    )
}

/// Render a platform gemspec: it stamps `s.platform` and ships the bundled
/// native library alongside the Ruby sources.
fn render_packaged_gemspec(
    package: &ResolvedPackage,
    gem_file: &str,
    platform: Platform,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, gem_file);
    let name = &package.name;
    let version = &package.version;
    let summary = package.description_or_default().replace('\'', "\\'");
    let ruby_platform = platform.ruby_platform();
    let mut extra = String::new();
    if !package.authors.is_empty() {
        let authors = package
            .authors
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");
        extra.push_str(&format!("  s.authors     = [{authors}]\n"));
    }
    if let Some(license) = &package.license {
        extra.push_str(&format!("  s.license     = '{license}'\n"));
    }
    if let Some(homepage) = package.homepage.as_ref().or(package.repository.as_ref()) {
        extra.push_str(&format!("  s.homepage    = '{homepage}'\n"));
    }
    format!(
        "{prelude}Gem::Specification.new do |s|
  s.name        = '{name}'
  s.version     = '{version}'
  s.platform    = '{ruby_platform}'
  s.summary     = '{summary}'
{extra}  s.files       = Dir['lib/**/*.rb'] + Dir['lib/**/*.{{so,dylib,dll}}']
  s.require_paths = ['lib']

  s.add_dependency 'ffi', '~> 1.15'
end

{trailer}"
    )
}

/// README for a packaged Ruby platform gem.
fn render_packaged_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let version = &package.version;
    let require_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Ruby)

Auto-generated Ruby bindings using the [ffi](https://github.com/ffi/ffi) gem,
with the native library bundled for this platform. The library loads
automatically; no external setup is required.

## Install

```bash
gem build {name}.gemspec
gem install {name}-{version}-*.gem
```

## Usage

```ruby
require '{require_name}'
```

{trailer}"#
    )
}

// ── Type helpers ──

/// Maps a shared ABI [`CType`] onto its Ruby FFI symbol. The structural
/// lowering comes from [`weaveffi_core::abi`]; this is the Ruby vocabulary.
/// `string_as_pointer` distinguishes the two char-pointer conventions: `ffi`
/// auto-marshals `:string` for *input* parameters but owned-return pointers
/// must stay `:pointer` so the caller can free them.
fn rb_ffi_type(ty: &CType, string_as_pointer: bool) -> &'static str {
    match ty {
        CType::Int8 => ":int8",
        CType::Int16 => ":int16",
        CType::Int32 | CType::Bool | CType::Enum { .. } => ":int32",
        CType::Uint8 => ":uint8",
        CType::Uint16 => ":uint16",
        CType::Uint32 => ":uint32",
        CType::Int64 => ":int64",
        CType::Uint64 => ":uint64",
        CType::Float => ":float",
        CType::Double => ":double",
        CType::Handle => ":uint64",
        CType::Size => ":size_t",
        CType::Void => ":void",
        CType::Ptr { pointee, .. } if matches!(**pointee, CType::Char) && !string_as_pointer => {
            ":string"
        }
        _ => ":pointer",
    }
}

fn rb_return_ffi_type(ty: &TypeRef) -> &'static str {
    rb_ffi_type(&abi::lower_return(ty, "").ret, true)
}

fn rb_return_out_params(ty: &TypeRef) -> Vec<&'static str> {
    abi::lower_return(ty, "")
        .out_params
        .iter()
        .map(|p| rb_ffi_type(&p.ty, true))
        .collect()
}

fn rb_read_method(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "read_int8",
        TypeRef::I16 => "read_int16",
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => "read_int32",
        TypeRef::U8 => "read_uint8",
        TypeRef::U16 => "read_uint16",
        TypeRef::U32 => "read_uint32",
        TypeRef::I64 => "read_int64",
        TypeRef::U64 => "read_uint64",
        TypeRef::F32 => "read_float",
        TypeRef::F64 => "read_double",
        TypeRef::Handle => "read_uint64",
        _ => "read_pointer",
    }
}

fn rb_mem_type(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => ":int8",
        TypeRef::I16 => ":int16",
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => ":int32",
        TypeRef::U8 => ":uint8",
        TypeRef::U16 => ":uint16",
        TypeRef::U32 => ":uint32",
        TypeRef::I64 => ":int64",
        TypeRef::U64 => ":uint64",
        TypeRef::F32 => ":float",
        TypeRef::F64 => ":double",
        TypeRef::Handle => ":uint64",
        _ => ":pointer",
    }
}

fn rb_write_method(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "write_int8",
        TypeRef::I16 => "write_int16",
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => "write_int32",
        TypeRef::U8 => "write_uint8",
        TypeRef::U16 => "write_uint16",
        TypeRef::U32 => "write_uint32",
        TypeRef::I64 => "write_int64",
        TypeRef::U64 => "write_uint64",
        TypeRef::F32 => "write_float",
        TypeRef::F64 => "write_double",
        TypeRef::Handle => "write_uint64",
        _ => "write_pointer",
    }
}

fn rb_array_reader(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "read_array_of_int8",
        TypeRef::I16 => "read_array_of_int16",
        TypeRef::I32 | TypeRef::Bool | TypeRef::Enum(_) => "read_array_of_int32",
        TypeRef::U8 => "read_array_of_uint8",
        TypeRef::U16 => "read_array_of_uint16",
        TypeRef::U32 => "read_array_of_uint32",
        TypeRef::I64 => "read_array_of_int64",
        TypeRef::U64 => "read_array_of_uint64",
        TypeRef::F32 => "read_array_of_float",
        TypeRef::F64 => "read_array_of_double",
        TypeRef::Handle => "read_array_of_uint64",
        _ => "read_array_of_pointer",
    }
}

fn rb_array_writer(ty: &TypeRef) -> &'static str {
    match ty {
        TypeRef::I8 => "write_array_of_int8",
        TypeRef::I16 => "write_array_of_int16",
        TypeRef::I32 | TypeRef::Enum(_) => "write_array_of_int32",
        TypeRef::U8 => "write_array_of_uint8",
        TypeRef::U16 => "write_array_of_uint16",
        TypeRef::U32 => "write_array_of_uint32",
        TypeRef::I64 => "write_array_of_int64",
        TypeRef::U64 => "write_array_of_uint64",
        TypeRef::F32 => "write_array_of_float",
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
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
            vec![format!("{name}.handle")]
        }
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => vec![format!("{name}_c")],
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![name.to_string()],
            TypeRef::Record(_)
            | TypeRef::RichEnum(_)
            | TypeRef::TypedHandle(_)
            | TypeRef::Interface(_) => {
                vec![format!("{name}&.handle")]
            }
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
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn rb_element_expr(var: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("{var}.null? ? '' : {var}.read_string")
        }
        TypeRef::TypedHandle(name) => format!("{name}.new({var})"),
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            format!("{}.new({var})", local_type_name(name))
        }
        // Owned interface references wrap without re-running initialize.
        TypeRef::Interface(name) => format!("{}._from_ptr({var})", local_type_name(name)),
        TypeRef::Bool => format!("{var} != 0"),
        _ => var.to_string(),
    }
}

// ── Rendering ──

/// Emits a Ruby `# ...` doc comment at `indent`. Each input line is prefixed
/// with `# `; blank lines become `#`.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Hash);
}

// ── Typed errors ──

/// The snake_case stem of a domain's generated helpers: `KvError` becomes
/// `kv_error`, naming `kv_error_from` and `check_kv_error!`. Domain type
/// names are globally unique (validated), so the helpers can't collide.
fn rb_error_stem(eb: &ErrorBinding) -> String {
    eb.type_name.to_snake_case()
}

/// `{stem}_from`: builds the domain error matching an ABI code.
fn rb_error_factory_name(eb: &ErrorBinding) -> String {
    format!("{}_from", rb_error_stem(eb))
}

/// `check_{stem}!`: raises the typed domain error for a non-zero out-err slot.
fn rb_error_checker_name(eb: &ErrorBinding) -> String {
    format!("check_{}!", rb_error_stem(eb))
}

/// The error-check call a callable's out-err slot goes through, per the
/// function's [`ErrorStrategy`]: the module domain's typed checker for
/// [`ErrorStrategy::Throws`], the generic `check_error!` (plain `Error`;
/// producer panics and marshalling failures only) for
/// [`ErrorStrategy::Trap`].
fn rb_checker_name(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => rb_error_checker_name(eb),
        _ => "check_error!".to_string(),
    }
}

/// Escape a string for embedding in a single-quoted Ruby literal.
fn rb_str_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Render one module's declared error domain: a domain class subclassing the
/// generic `Error`, one nested subclass per code carrying its stable `CODE`
/// constant and default message, the code-to-class table, and the
/// factory/checker helpers throwing wrappers route their out-err slots
/// through. Nesting the code classes keeps `KvError::KeyNotFound` spellable
/// and unambiguous even across domains.
fn render_error(out: &mut String, module: &ModuleBinding, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    let factory = rb_error_factory_name(eb);
    let checker = rb_error_checker_name(eb);
    let table = format!("{}_CODES", eb.type_name.to_shouty_snake_case());

    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line(format!(
        "# Base error for the `{}` module's error domain.",
        module.path
    ));
    w.line(format!("class {domain} < Error"));
    w.scope(|w| {
        for (idx, c) in eb.codes.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            let class = weaveffi_core::errors::pascal(&c.name);
            let doc = c.doc.clone().unwrap_or_else(|| c.message.clone());
            let mut d = String::new();
            emit_doc(&mut d, &Some(doc), "    ");
            w.raw(d);
            w.block(format!("class {class} < {domain}"), "end", |w| {
                w.line(format!("CODE = {}", c.value));
                w.blank();
                w.block(
                    format!("def initialize(message = '{}')", rb_str_literal(&c.message)),
                    "end",
                    |w| {
                        w.line(format!("super({}, message)", c.value));
                    },
                );
            });
        }
    });
    w.line("end");

    w.blank();
    w.line(format!(
        "# Maps each ABI code of the {domain} domain to its error class."
    ));
    w.line(format!("{table} = {{"));
    w.scope(|w| {
        for c in &eb.codes {
            w.line(format!(
                "{} => {domain}::{},",
                c.value,
                weaveffi_core::errors::pascal(&c.name)
            ));
        }
    });
    w.line("}.freeze");

    w.blank();
    w.line(format!(
        "# Builds the {domain} subclass matching `code`, or a generic Error"
    ));
    w.line("# for codes outside the domain (panics, marshalling).");
    w.block(format!("def self.{factory}(code, message)"), "end", |w| {
        w.line(format!("cls = {table}[code]"));
        w.line("return Error.new(code, message) if cls.nil?");
        w.line("message.empty? ? cls.new : cls.new(message)");
    });

    w.blank();
    w.line(format!(
        "# Raises the typed {domain} for a non-zero error slot."
    ));
    w.block(format!("def self.{checker}(err)"), "end", |w| {
        w.line("return if err[:code].zero?");
        w.line("code = err[:code]");
        w.line("msg_ptr = err[:message]");
        w.line("msg = msg_ptr.null? ? '' : msg_ptr.read_string");
        w.line("weaveffi_error_clear(err.to_ptr)");
        w.line(format!("raise {factory}(code, msg)"));
    });
    out.push_str(&w.finish());
}

fn render_ruby_module(
    model: &BindingModel,
    module_name: &str,
    strip_module_prefix: bool,
    lib_filename: &str,
    input_basename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::Hash, input_basename);
    let has_listeners = model.modules.iter().any(|m| !m.listeners.is_empty());
    render_preamble(&mut out, module_name, has_listeners);
    for m in &model.modules {
        out.push_str(&format!("\n  # === Module: {} ===\n", m.path));
        // The typed error surface comes first so the domain class exists
        // before any wrapper references its checker.
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error(&mut out, m, eb);
        }
        for e in &m.enums {
            // A plain C-style enum is a module of integer constants; a rich
            // (algebraic) enum is an opaque-object wrapper like a struct, so it
            // emits FFI bindings here and a wrapper class further down.
            if e.is_rich() {
                render_rich_enum_ffi(&mut out, e);
            } else {
                render_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_struct_ffi(&mut out, s);
        }
        for i in &m.interfaces {
            render_interface_ffi(&mut out, i);
        }
        for c in &m.callbacks {
            render_callback_decl(&mut out, c);
        }
        for l in &m.listeners {
            render_listener_ffi(&mut out, l);
        }
        for f in &m.functions {
            render_attach_function(&mut out, f);
        }
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum_class(&mut out, e, module_name);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, s, module_name);
            if s.builder.is_some() {
                render_ruby_builder_class(&mut out, s, module_name);
            }
        }
        for i in &m.interfaces {
            render_interface_class(&mut out, m, i, module_name);
        }
        for l in &m.listeners {
            render_listener_wrapper(&mut out, m, l, strip_module_prefix);
        }
        for f in &m.functions {
            let scope = RbScope::Free {
                module_path: &m.path,
                strip_module_prefix,
            };
            render_callable(&mut out, m, f, &scope);
        }
    }
    out.push_str("end\n\n");
    out.push_str(&render_trailer(CommentStyle::Hash, lib_filename));
    out
}

fn render_preamble(out: &mut String, module_name: &str, has_listeners: bool) {
    out.push_str(&format!(
        "# frozen_string_literal: true
# {module_name} Ruby FFI bindings (auto-generated)

require 'ffi'

module {module_name}
  extend FFI::Library

  # An explicit path in WEAVEFFI_LIBRARY wins, so callers can point at a
  # specific build artifact regardless of its file name or location.
  _wv_override = ENV['WEAVEFFI_LIBRARY']
  if _wv_override && !_wv_override.empty?
    ffi_lib _wv_override
  else
    case FFI::Platform::OS
    when /darwin/
      ffi_lib 'libweaveffi.dylib'
    when /mswin|mingw/
      ffi_lib 'weaveffi.dll'
    else
      ffi_lib 'libweaveffi.so'
    end
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
    if has_listeners {
        out.push_str(
            "
  # Registered listener trampolines, keyed by subscription id. Holding the
  # FFI::Function objects here keeps them alive until unregistered; without
  # this the GC could collect a trampoline the producer still calls.
  @listener_refs = {}
",
        );
    }
}

fn render_enum(out: &mut String, e: &EnumBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "  ");
    w.raw(d);
    w.line(format!("module {}", e.name));
    w.scope(|w| {
        for v in &e.variants {
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "    ");
            w.raw(vd);
            w.line(format!("{} = {}", v.name.to_shouty_snake_case(), v.value));
        }
    });
    w.line("end");
    out.push_str(&w.finish());
}

fn render_struct_ffi(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line(format!(
        "attach_function :{}, [:pointer], :void",
        s.destroy_symbol
    ));
    // The builder's `build` calls the C `create`; only attach it when needed.
    if s.builder.is_some() {
        w.line(format!(
            "attach_function :{}, [{}], :pointer",
            s.create.symbol,
            rb_abi_types(&s.create.params, false).join(", ")
        ));
    }
    for field in &s.fields {
        let getter = &field.getter_symbol;
        let mut argtypes = vec![":pointer".to_string()];
        argtypes.extend(
            rb_return_out_params(&field.ty)
                .iter()
                .map(|s| s.to_string()),
        );
        let restype = rb_return_ffi_type(&field.ty);
        let mut d = String::new();
        emit_doc(&mut d, &field.doc, "  ");
        w.raw(d);
        w.line(format!(
            "attach_function :{getter}, [{}], {restype}",
            argtypes.join(", ")
        ));
    }
    out.push_str(&w.finish());
}

/// Declare the FFI bindings for a rich (algebraic) enum: the tag getter, the
/// destructor, and (per variant) the constructor and one getter per
/// associated field. Mirrors [`render_struct_ffi`]; the field getters lower
/// exactly like struct field getters (string getters return an owned
/// `:pointer`, bytes/list getters take a trailing `out_len`).
fn render_rich_enum_ffi(out: &mut String, e: &EnumBinding) {
    let rich = e
        .rich
        .as_ref()
        .expect("render_rich_enum_ffi requires a rich enum");
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line(format!(
        "attach_function :{}, [:pointer], :int32",
        rich.tag_symbol
    ));
    w.line(format!(
        "attach_function :{}, [:pointer], :void",
        rich.destroy_symbol
    ));
    for v in &rich.variants {
        // Constructor: the variant's field value slots, then out_err, returning
        // the opaque object pointer (a unit variant takes only out_err).
        w.line(format!(
            "attach_function :{}, [{}], :pointer",
            v.create.symbol,
            rb_abi_types(&v.create.params, false).join(", ")
        ));
        for field in &v.fields {
            let getter = &field.getter_symbol;
            let mut argtypes = vec![":pointer".to_string()];
            argtypes.extend(
                rb_return_out_params(&field.ty)
                    .iter()
                    .map(|s| s.to_string()),
            );
            let restype = rb_return_ffi_type(&field.ty);
            let mut d = String::new();
            emit_doc(&mut d, &field.doc, "  ");
            w.raw(d);
            w.line(format!(
                "attach_function :{getter}, [{}], {restype}",
                argtypes.join(", ")
            ));
        }
    }
    out.push_str(&w.finish());
}

/// Declare the FFI bindings for one interface: the destroy symbol plus every
/// constructor, method, and static through the shared attach path. Methods
/// carry their implicit leading `self` pointer slot in the precomputed ABI
/// signatures, so no special casing is needed here.
fn render_interface_ffi(out: &mut String, i: &InterfaceBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line(format!(
        "attach_function :{}, [:pointer], :void",
        i.destroy_symbol
    ));
    out.push_str(&w.finish());
    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        render_attach_function(out, f);
    }
}

/// Map lowered ABI slots onto Ruby FFI type tokens. `string_as_pointer`
/// applies to top-level `char*` slots (owned returns stay `:pointer` so the
/// wrapper can free them; borrowed inputs use `:string` auto-marshalling).
fn rb_abi_types(params: &[AbiParam], string_as_pointer: bool) -> Vec<String> {
    params
        .iter()
        .map(|p| rb_ffi_type(&p.ty, string_as_pointer).to_string())
        .collect()
}

/// `callback :{c_fn_type}, [...], :void` declaration for a module callback.
/// Listener `attach_function`s reference the type by this symbol. Borrowed
/// string params use `:string` so the ffi gem hands the block a Ruby String.
fn render_callback_decl(out: &mut String, c: &CallbackBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    let mut d = String::new();
    emit_doc(&mut d, &c.doc, "  ");
    w.raw(d);
    w.line(format!(
        "callback :{}, [{}], :void",
        c.c_fn_type,
        rb_abi_types(&c.abi_params, false).join(", ")
    ));
    out.push_str(&w.finish());
}

fn render_listener_ffi(out: &mut String, l: &ListenerBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.line(format!(
        "attach_function :{}, [:{}, :pointer], :uint64",
        l.register_symbol, l.callback_c_fn_type
    ));
    w.line(format!(
        "attach_function :{}, [:uint64], :void",
        l.unregister_symbol
    ));
    out.push_str(&w.finish());
}

fn render_attach_function(out: &mut String, f: &FnBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, "  ");
    w.raw(d);
    match &f.shape {
        CallShape::Sync(abi) => {
            w.line(format!(
                "attach_function :{}, [{}], {}",
                abi.symbol,
                rb_abi_types(&abi.params, false).join(", "),
                rb_ffi_type(&abi.ret, true)
            ));
        }
        CallShape::Async(a) => {
            // Completion callback: result strings/bytes stay `:pointer`
            // (the wrapper owns and frees them); the launcher takes the
            // declared callback type plus the opaque context.
            w.line(format!(
                "callback :{}, [{}], :void",
                a.callback_type,
                rb_abi_types(&a.callback_params, true).join(", ")
            ));
            let argtypes: Vec<String> = a
                .launch
                .params
                .iter()
                .map(|p| match &p.ty {
                    // The `callback` slot is lowered as a Named C type; bind
                    // it to the callback symbol declared above.
                    CType::Named(_) => format!(":{}", a.callback_type),
                    ty => rb_ffi_type(ty, false).to_string(),
                })
                .collect();
            w.line(format!(
                "attach_function :{}, [{}], :void",
                a.launch.symbol,
                argtypes.join(", ")
            ));
        }
        CallShape::Iterator(it) => {
            w.line(format!(
                "attach_function :{}, [{}], :pointer",
                it.launch.symbol,
                rb_abi_types(&it.launch.params, false).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [{}], :int32",
                it.next.symbol,
                // Every `next` slot is a pointer (iter, out_item, out lens, err).
                rb_abi_types(&it.next.params, true).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [:pointer], :void",
                it.destroy_symbol
            ));
        }
    }
    out.push_str(&w.finish());
}

fn render_struct_class(out: &mut String, s: &StructBinding, rb_module_name: &str) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.block(
        format!("class {}Ptr < FFI::AutoPointer", s.name),
        "end",
        |w| {
            w.block("def self.release(ptr)", "end", |w| {
                w.line(format!("{rb_module_name}.{}(ptr)", s.destroy_symbol));
            });
        },
    );
    w.blank();

    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "  ");
    w.raw(d);
    w.line(format!("class {}", s.name));
    w.scope(|w| {
        w.line("attr_reader :handle");
        w.blank();
        w.block("def initialize(handle)", "end", |w| {
            w.line(format!("@handle = {}Ptr.new(handle)", s.name));
        });
        w.blank();
        w.block("def self.create(handle)", "end", |w| {
            w.line("new(handle)");
        });
        w.blank();
        w.block("def destroy", "end", |w| {
            w.line("return if @handle.nil?");
            w.line("@handle.free");
            w.line("@handle = nil");
        });

        for field in &s.fields {
            let mut g = String::new();
            render_getter(&mut g, &field.name, field, rb_module_name);
            w.raw(g);
        }
    });
    w.line("end");
    out.push_str(&w.finish());
}

fn render_ruby_builder_class(out: &mut String, s: &StructBinding, rb_module_name: &str) {
    let builder = format!("{}Builder", s.name);
    let ind = "      ";
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "  ");
    w.raw(d);
    w.line(format!("class {builder}"));
    w.scope(|w| {
        w.block("def initialize", "end", |w| {
            // Zero-value defaults (the same contract as the other backends): scalars
            // start at 0/false/""/"".b, collections empty, optionals absent. Unset
            // fields therefore lower to valid C arguments instead of raising.
            for field in &s.fields {
                w.line(format!("@{} = {}", field.name, rb_field_default(&field.ty)));
            }
        });
        w.blank();

        for field in &s.fields {
            let mut fd = String::new();
            emit_doc(&mut fd, &field.doc, "    ");
            w.raw(fd);
            w.block(format!("def with_{}(value)", field.name), "end", |w| {
                w.line(format!("@{} = value", field.name));
                w.line("self");
            });
            w.blank();
        }

        // Build: marshal every field into the struct's C `create` call with the
        // same lowering used for function parameters, then wrap the handle.
        w.block("def build", "end", |w| {
            w.line(format!("err = {rb_module_name}::ErrorStruct.new"));
            for field in &s.fields {
                w.line(format!("{} = @{}", field.name, field.name));
                let mut pc = String::new();
                render_param_conversion(&mut pc, &field.name, &field.ty, ind);
                w.raw(pc);
            }
            let mut call_args: Vec<String> = Vec::new();
            for field in &s.fields {
                call_args.extend(rb_call_args(&field.name, &field.ty));
            }
            call_args.push("err".into());
            w.line(format!(
                "result = {rb_module_name}.{}({})",
                s.create.symbol,
                call_args.join(", ")
            ));
            w.line(format!("{rb_module_name}.check_error!(err)"));
            w.line(format!("{}.new(result)", s.name));
        });
    });
    w.line("end");
    out.push_str(&w.finish());
}

/// Render one interface as an opaque-object wrapper class, following the
/// struct wrapper's ownership pattern: a `{Name}Ptr < FFI::AutoPointer`
/// subclass releases the handle through the interface's C destroy symbol on
/// GC, and the wrapper class exposes `attr_reader :handle` plus an explicit
/// `destroy`. A constructor named `new` becomes `initialize`; every other
/// constructor becomes a class-method factory; methods pass `@handle` as the
/// leading C argument; statics are class methods. `_from_ptr` wraps an owned
/// pointer the producer already handed over (a C return value) without
/// re-running `initialize`.
fn render_interface_class(
    out: &mut String,
    module: &ModuleBinding,
    i: &InterfaceBinding,
    rb_module_name: &str,
) {
    let ptr_class = format!("{}Ptr", i.name);
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.block(
        format!("class {ptr_class} < FFI::AutoPointer"),
        "end",
        |w| {
            w.block("def self.release(ptr)", "end", |w| {
                w.line(format!("{rb_module_name}.{}(ptr)", i.destroy_symbol));
            });
        },
    );
    w.blank();

    let mut d = String::new();
    emit_doc(&mut d, &i.doc, "  ");
    w.raw(d);
    w.line(format!("class {}", i.name));
    w.scope(|w| {
        w.line("attr_reader :handle");
        w.blank();
        w.line("# Wraps an owned pointer the producer handed over, without");
        w.line("# re-running initialize.");
        w.block("def self._from_ptr(ptr)", "end", |w| {
            w.line("obj = allocate");
            w.line(format!(
                "obj.instance_variable_set(:@handle, {ptr_class}.new(ptr))"
            ));
            w.line("obj");
        });
        w.blank();
        w.block("def destroy", "end", |w| {
            w.line("return if @handle.nil?");
            w.line("@handle.free");
            w.line("@handle = nil");
        });
    });
    out.push_str(&w.finish());

    // Members render at class depth through the shared callable paths, so
    // sync, async, and iterator members reuse the free-function marshalling.
    if let Some(c) = i.constructors.iter().find(|c| c.name == "new") {
        let scope = RbScope::Init {
            module_name: rb_module_name,
            ptr_class: &ptr_class,
        };
        render_callable(out, module, c, &scope);
    }
    for c in i.constructors.iter().filter(|c| c.name != "new") {
        let scope = RbScope::Factory {
            module_name: rb_module_name,
        };
        render_callable(out, module, c, &scope);
    }
    for f in &i.methods {
        let scope = RbScope::Method {
            module_name: rb_module_name,
        };
        render_callable(out, module, f, &scope);
    }
    for f in &i.statics {
        let scope = RbScope::Static {
            module_name: rb_module_name,
        };
        render_callable(out, module, f, &scope);
    }

    let mut close = CodeWriter::two_space().with_depth(1);
    close.line("end");
    out.push_str(&close.finish());
}

/// The zero-value default for one Ruby builder slot.
fn rb_field_default(ty: &TypeRef) -> &'static str {
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
        | TypeRef::Enum(_) => "0",
        TypeRef::F32 | TypeRef::F64 => "0.0",
        TypeRef::Bool => "false",
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "\"\"",
        TypeRef::Bytes | TypeRef::BorrowedBytes => "\"\".b",
        TypeRef::List(_) => "[]",
        TypeRef::Map(_, _) => "{}",
        // Optionals are absent by default; struct/handle fields have no
        // synthesizable zero value, so the with_ setter is the only path.
        _ => "nil",
    }
}

fn render_getter(out: &mut String, method: &str, field: &FieldBinding, rb_module_name: &str) {
    let getter = &field.getter_symbol;
    let ind = "      ";

    let mut w = CodeWriter::two_space().with_depth(2);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &field.doc, "    ");
    w.raw(d);
    w.block(format!("def {}", method), "end", |w| {
        let out_params = rb_return_out_params(&field.ty);
        let is_map = get_map_kv(&field.ty).is_some();

        if is_map {
            w.line("out_keys = FFI::MemoryPointer.new(:pointer)");
            w.line("out_values = FFI::MemoryPointer.new(:pointer)");
            w.line("out_len = FFI::MemoryPointer.new(:size_t)");
            w.line(format!(
                "{rb_module_name}.{getter}(@handle, out_keys, out_values, out_len)"
            ));
            let (k, v) = get_map_kv(&field.ty).unwrap();
            let is_optional = matches!(&field.ty, TypeRef::Optional(_));
            let mut tmp = String::new();
            render_map_return_code(&mut tmp, k, v, ind, is_optional, Some(rb_module_name));
            w.raw(tmp);
        } else if !out_params.is_empty() {
            w.line("out_len = FFI::MemoryPointer.new(:size_t)");
            w.line(format!(
                "result = {rb_module_name}.{getter}(@handle, out_len)"
            ));
            let mut tmp = String::new();
            render_return_code(&mut tmp, &field.ty, ind, Some(rb_module_name));
            w.raw(tmp);
        } else {
            w.line(format!("result = {rb_module_name}.{getter}(@handle)"));
            let mut tmp = String::new();
            render_return_code(&mut tmp, &field.ty, ind, Some(rb_module_name));
            w.raw(tmp);
        }
    });
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an opaque-object wrapper class, mirroring
/// the struct wrapper: an `FFI::AutoPointer` subclass that frees the handle on
/// GC, an `attr_reader :handle` + `initialize`/`create`/`destroy` matching the
/// struct contract (so the existing function-wrapper marshalling, `x.handle`
/// in, `Shape.new(result)` out, works unchanged), integer tag constants and a
/// `tag` reader, one factory class method per variant (`Shape.circle(2.5)`),
/// and per-variant field accessors namespaced by variant (`circle_radius`).
fn render_rich_enum_class(out: &mut String, e: &EnumBinding, rb_module_name: &str) {
    let rich = e
        .rich
        .as_ref()
        .expect("render_rich_enum_class requires a rich enum");

    let mut w = CodeWriter::two_space().with_depth(1);
    // AutoPointer releases the handle through the enum's C destructor on GC,
    // the same ownership contract a struct wrapper uses.
    w.blank();
    w.block(
        format!("class {}Ptr < FFI::AutoPointer", e.name),
        "end",
        |w| {
            w.block("def self.release(ptr)", "end", |w| {
                w.line(format!("{rb_module_name}.{}(ptr)", rich.destroy_symbol));
            });
        },
    );
    w.blank();

    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "  ");
    w.raw(d);
    w.line(format!("class {}", e.name));
    w.scope(|w| {
        w.line("attr_reader :handle");
        w.blank();
        w.block("def initialize(handle)", "end", |w| {
            w.line(format!("@handle = {}Ptr.new(handle)", e.name));
        });
        w.blank();
        w.block("def self.create(handle)", "end", |w| {
            w.line("new(handle)");
        });
        w.blank();
        w.block("def destroy", "end", |w| {
            w.line("return if @handle.nil?");
            w.line("@handle.free");
            w.line("@handle = nil");
        });
        w.blank();

        // Tag constants (one per variant) plus the active-variant reader.
        for v in &e.variants {
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "    ");
            w.raw(vd);
            w.line(format!("{} = {}", v.name.to_shouty_snake_case(), v.value));
        }
        w.blank();
        w.block("def tag", "end", |w| {
            w.line(format!("{rb_module_name}.{}(@handle)", rich.tag_symbol));
        });

        // One factory class method per variant.
        for v in &rich.variants {
            let mut f = String::new();
            render_rich_variant_factory(&mut f, v, rb_module_name);
            w.raw(f);
        }

        // Per-variant field accessors, namespaced by variant (`circle_radius`) to
        // avoid collisions, reusing the struct getter marshalling verbatim.
        for v in &rich.variants {
            for field in &v.fields {
                let method = format!("{}_{}", v.name.to_snake_case(), field.name);
                let mut g = String::new();
                render_getter(&mut g, &method, field, rb_module_name);
                w.raw(g);
            }
        }
    });
    w.line("end");
    out.push_str(&w.finish());
}

/// Render one variant factory as a class method (`Shape.circle(radius)`). Marshals
/// each field with the same lowering struct `create`/builder calls use, invokes
/// the variant constructor with a shared `ErrorStruct`, raises on error, and
/// wraps the returned handle.
fn render_rich_variant_factory(out: &mut String, v: &RichVariantBinding, rb_module_name: &str) {
    let ind = "      ";
    let factory = v.name.to_snake_case();
    let params: Vec<String> = v.fields.iter().map(|f| f.name.to_snake_case()).collect();

    let mut w = CodeWriter::two_space().with_depth(2);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &v.doc, "    ");
    w.raw(d);
    let open = if params.is_empty() {
        format!("def self.{factory}")
    } else {
        format!("def self.{factory}({})", params.join(", "))
    };
    w.block(open, "end", |w| {
        w.line(format!("err = {rb_module_name}::ErrorStruct.new"));
        for f in &v.fields {
            let mut pc = String::new();
            render_param_conversion(&mut pc, &f.name.to_snake_case(), &f.ty, ind);
            w.raw(pc);
        }
        let mut call_args: Vec<String> = Vec::new();
        for f in &v.fields {
            call_args.extend(rb_call_args(&f.name.to_snake_case(), &f.ty));
        }
        call_args.push("err".into());
        w.line(format!(
            "result = {rb_module_name}.{}({})",
            v.create.symbol,
            call_args.join(", ")
        ));
        w.line(format!("{rb_module_name}.check_error!(err)"));
        w.line("new(result)");
    });
    out.push_str(&w.finish());
}

/// How a rendered Ruby callable is scoped and spelled in the generated
/// module: at module scope as a singleton method, or inside an interface
/// class as a constructor, instance method, or class method.
enum RbScope<'a> {
    /// A module-level free function (`def self.name` on the top-level module).
    Free {
        /// The owning module's underscore-joined path.
        module_path: &'a str,
        /// Whether the emitted name drops the module-path prefix.
        strip_module_prefix: bool,
    },
    /// An instance method on an interface class: `def name`, passing
    /// `@handle` as the leading C argument.
    Method {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// A static member of an interface class (`def self.name`).
    Static {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// A non-`new` constructor: a class method wrapping the returned owned
    /// pointer via `_from_ptr` (never re-running `initialize`).
    Factory {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// The canonical `new` constructor, emitted as `initialize`.
    Init {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
        /// The interface's `FFI::AutoPointer` subclass wrapping the handle.
        ptr_class: &'a str,
    },
}

impl<'a> RbScope<'a> {
    /// The top-level Ruby module name when calls must be explicitly
    /// qualified (inside a class body); `None` at module scope, where the
    /// implicit `self` already is the module.
    fn module_name(&self) -> Option<&'a str> {
        match self {
            RbScope::Free { .. } => None,
            RbScope::Method { module_name }
            | RbScope::Static { module_name }
            | RbScope::Factory { module_name }
            | RbScope::Init { module_name, .. } => Some(module_name),
        }
    }

    /// The receiver prefix for module singleton calls (attached C symbols,
    /// error checkers, `weaveffi_free_*`): `"{ModuleName}."` inside a class
    /// body, empty at module scope.
    fn qualifier(&self) -> String {
        self.module_name()
            .map(|m| format!("{m}."))
            .unwrap_or_default()
    }

    /// Two-space indent depth of the `def` line (1 at module scope, 2 inside
    /// an interface class).
    fn depth(&self) -> usize {
        if self.module_name().is_none() {
            1
        } else {
            2
        }
    }

    /// The `@handle` argument instance methods pass as the leading C slot.
    fn self_arg(&self) -> Option<&'static str> {
        matches!(self, RbScope::Method { .. }).then_some("@handle")
    }

    /// The `def` opener for `f` with the given formal parameter names.
    fn def_open(&self, f: &FnBinding, params: &[String]) -> String {
        let args = params.join(", ");
        match self {
            RbScope::Free {
                module_path,
                strip_module_prefix,
            } => format!(
                "def self.{}({args})",
                wrapper_name(module_path, &f.name, *strip_module_prefix).to_snake_case()
            ),
            RbScope::Method { .. } => format!("def {}({args})", f.name.to_snake_case()),
            RbScope::Static { .. } | RbScope::Factory { .. } => {
                format!("def self.{}({args})", f.name.to_snake_case())
            }
            RbScope::Init { .. } => format!("def initialize({args})"),
        }
    }
}

/// Render one callable: a free function or an interface member. `module`
/// supplies the error domain for throwing callables; `scope` picks the def
/// spelling, receiver, indent, and result handling. Sync, async, and
/// iterator shapes all route through here so members reuse the free-function
/// marshalling paths.
fn render_callable(out: &mut String, module: &ModuleBinding, f: &FnBinding, scope: &RbScope) {
    match &f.shape {
        CallShape::Sync(_) => render_sync_function_wrapper(out, module, f, scope),
        CallShape::Async(a) => render_async_function_wrapper(out, module, f, a, scope),
        CallShape::Iterator(it) => render_iterator_function_wrapper(out, module, f, it, scope),
    }
}

/// Idiomatic register/unregister pair for one listener. The user passes a
/// block; the trampoline converts each C argument and the `FFI::Function` is
/// pinned in `@listener_refs` until unregistered.
fn render_listener_wrapper(
    out: &mut String,
    module: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = module.callbacks.iter().find(|c| c.name == l.event_callback) else {
        unreachable!("listener '{}' references unknown callback", l.name);
    };
    let register_name = wrapper_name(
        &module.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    )
    .to_snake_case();
    let unregister_name = wrapper_name(
        &module.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_snake_case();

    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &l.doc, "  ");
    w.raw(d);
    w.line(format!(
        "# Registers a {} listener block. Returns a subscription id for",
        cb.name
    ));
    w.line(format!("# {unregister_name}."));

    // Trampoline formals: one per ABI slot, plus the ignored context.
    let tramp_formals: Vec<String> = cb
        .params
        .iter()
        .flat_map(|p| p.abi.iter().map(|s| s.name.to_snake_case()))
        .chain(std::iter::once("_context".to_string()))
        .collect();
    let tramp_types = rb_abi_types(&cb.abi_params, false);
    let call_args: Vec<String> = cb
        .params
        .iter()
        .map(|p| rb_cb_arg_expr(&p.name.to_snake_case(), &p.ty))
        .collect();
    w.block(format!("def self.{register_name}(&block)"), "end", |w| {
        w.block(
            format!(
                "trampoline = FFI::Function.new(:void, [{}]) do |{}|",
                tramp_types.join(", "),
                tramp_formals.join(", ")
            ),
            "end",
            |w| {
                w.line(format!("block.call({})", call_args.join(", ")));
            },
        );
        w.line(format!(
            "listener_id = {}(trampoline, FFI::Pointer::NULL)",
            l.register_symbol
        ));
        w.line("@listener_refs[listener_id] = trampoline");
        w.line("listener_id");
    });

    w.blank();
    w.line(format!(
        "# Unregisters a listener previously registered with {register_name}."
    ));
    w.block(
        format!("def self.{unregister_name}(listener_id)"),
        "end",
        |w| {
            w.line(format!("{}(listener_id)", l.unregister_symbol));
            w.line("@listener_refs.delete(listener_id)");
            w.line("nil");
        },
    );
    out.push_str(&w.finish());
}

/// The Ruby expression converting one callback parameter's trampoline
/// arguments into the idiomatic value passed to the user block. Slot names
/// derive from the parameter name, mirroring [`abi::lower_param`].
fn rb_cb_arg_expr(n: &str, ty: &TypeRef) -> String {
    match ty {
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
        | TypeRef::Handle => n.into(),
        TypeRef::Bool => format!("({n} != 0)"),
        // `:string` slots arrive as Ruby Strings (copied by ffi) or nil.
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => n.into(),
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            format!("({n}_ptr.null? ? ''.b : {n}_ptr.read_string({n}_len))")
        }
        // Enums surface as their integer constants in Ruby.
        TypeRef::Enum(_) => n.into(),
        // Borrowed by contract: the producer owns callback arguments for the
        // duration of the call, so opaque pointers pass through raw.
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => n.into(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => n.into(),
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                format!("({n}_ptr.null? ? nil : {n}_ptr.read_string({n}_len))")
            }
            TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::TypedHandle(_) => n.into(),
            TypeRef::Bool => format!("({n}.null? ? nil : ({n}.read_int32 != 0))"),
            TypeRef::List(_) | TypeRef::Map(_, _) => {
                format!("({n}.null? ? nil : {})", rb_cb_list_expr(n, inner))
            }
            _ => {
                let read = rb_read_method(inner);
                format!("({n}.null? ? nil : {n}.{read})")
            }
        },
        TypeRef::List(_) | TypeRef::Map(_, _) => rb_cb_list_expr(n, ty),
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// List/map callback-argument reader: the slots are a base pointer (or
/// parallel key/value pointers) plus `{n}_len`.
fn rb_cb_list_expr(n: &str, ty: &TypeRef) -> String {
    match ty {
        TypeRef::List(elem) => {
            let reader = rb_array_reader(elem);
            let map_suffix = match elem.as_ref() {
                TypeRef::Bool => ".map { |v| v != 0 }".to_string(),
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    ".map { |p| p.null? ? '' : p.read_string }".to_string()
                }
                _ => String::new(),
            };
            format!("({n}.null? ? [] : {n}.{reader}({n}_len){map_suffix})")
        }
        TypeRef::Map(k, v) => {
            let k_reader = rb_array_reader(k);
            let v_reader = rb_array_reader(v);
            let k_expr = rb_element_expr("k", k);
            let v_expr = rb_element_expr("v", v);
            format!(
                "({n}_keys.null? ? {{}} : {n}_keys.{k_reader}({n}_len)\
                 .zip({n}_values.{v_reader}({n}_len))\
                 .each_with_object({{}}) {{ |(k, v), h| h[{k_expr}] = {v_expr} }})"
            )
        }
        _ => unreachable!("rb_cb_list_expr only handles lists and maps"),
    }
}

fn render_sync_function_wrapper(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    scope: &RbScope,
) {
    let c_sym = &f.c_base;
    let depth = scope.depth();
    let ind = "  ".repeat(depth + 1);
    let doc_ind = "  ".repeat(depth);
    let q = scope.qualifier();
    let checker = rb_checker_name(f, module.error.as_ref());

    let params: Vec<String> = f.params.iter().map(|p| p.name.to_snake_case()).collect();
    let mut w = CodeWriter::two_space().with_depth(depth);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, &doc_ind);
    w.raw(d);
    for p in &f.params {
        if let Some(pdoc) = &p.doc {
            let trimmed = pdoc.trim();
            if trimmed.is_empty() {
                continue;
            }
            let mut lines = trimmed.lines();
            if let Some(first) = lines.next() {
                w.line(format!(
                    "# @param {} [Object] {}",
                    p.name.to_snake_case(),
                    first
                ));
            }
            for line in lines {
                if line.is_empty() {
                    w.line("#");
                } else {
                    w.line(format!("#   {}", line));
                }
            }
        }
    }
    w.block(scope.def_open(f, &params), "end", |w| {
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('"', "\\\"");
            w.line(format!("warn \"[DEPRECATED] {escaped}\""));
        }

        w.line("err = ErrorStruct.new");

        for p in &f.params {
            let mut pc = String::new();
            render_param_conversion(&mut pc, &p.name.to_snake_case(), &p.ty, &ind);
            w.raw(pc);
        }

        let is_map_ret = f.ret.as_ref().and_then(get_map_kv).is_some();
        let has_out_len = f
            .ret
            .as_ref()
            .is_some_and(|ty| !rb_return_out_params(ty).is_empty())
            && !is_map_ret;

        if is_map_ret {
            w.line("out_keys = FFI::MemoryPointer.new(:pointer)");
            w.line("out_values = FFI::MemoryPointer.new(:pointer)");
            w.line("out_len = FFI::MemoryPointer.new(:size_t)");
        } else if has_out_len {
            w.line("out_len = FFI::MemoryPointer.new(:size_t)");
        }

        let mut call_args: Vec<String> = Vec::new();
        if let Some(recv) = scope.self_arg() {
            call_args.push(recv.into());
        }
        for p in &f.params {
            call_args.extend(rb_call_args(&p.name.to_snake_case(), &p.ty));
        }
        if is_map_ret {
            call_args.extend(["out_keys".into(), "out_values".into(), "out_len".into()]);
        } else if has_out_len {
            call_args.push("out_len".into());
        }
        call_args.push("err".into());

        let call = format!("{q}{c_sym}({})", call_args.join(", "));
        if f.ret.is_some() && !is_map_ret {
            w.line(format!("result = {call}"));
        } else {
            w.line(call);
        }

        w.line(format!("{q}{checker}(err)"));

        match scope {
            // Constructors receive the owned pointer directly rather than
            // routing through the generic return path.
            RbScope::Init { ptr_class, .. } => {
                w.line("raise Error.new(-1, 'null pointer') if result.null?");
                w.line(format!("@handle = {ptr_class}.new(result)"));
            }
            RbScope::Factory { .. } => {
                w.line("raise Error.new(-1, 'null pointer') if result.null?");
                w.line("_from_ptr(result)");
            }
            _ => {
                if let Some(ret_ty) = &f.ret {
                    if is_map_ret {
                        let (k, v) = get_map_kv(ret_ty).unwrap();
                        let is_optional = matches!(ret_ty, TypeRef::Optional(_));
                        let mut tmp = String::new();
                        render_map_return_code(
                            &mut tmp,
                            k,
                            v,
                            &ind,
                            is_optional,
                            scope.module_name(),
                        );
                        w.raw(tmp);
                    } else {
                        let mut tmp = String::new();
                        render_return_code(&mut tmp, ret_ty, &ind, scope.module_name());
                        w.raw(tmp);
                    }
                }
            }
        }
    });
    out.push_str(&w.finish());
}

/// Async wrapper: launches the `_async` C symbol with an `FFI::Function`
/// completion trampoline and blocks on a `Queue` until it fires (`Queue#pop`
/// releases the GVL, and the ffi gem delivers cross-thread callbacks safely).
/// Blocking is the idiomatic Ruby surface; callers needing concurrency wrap
/// the call in their own Thread or Fiber scheduler.
///
/// The trampoline (`callback` local) stays referenced by the wrapper's stack
/// frame until `queue.pop` returns, which happens only after the producer has
/// invoked it, so the GC cannot collect it mid-flight. Per
/// [`weaveffi_core::plan::AsyncProtocol`], the trampoline copies borrowed
/// result buffers before returning and never frees them; the error slot
/// follows the function's [`ErrorStrategy`].
fn render_async_function_wrapper(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    a: &AsyncBinding,
    scope: &RbScope,
) {
    let depth = scope.depth();
    let ind = "  ".repeat(depth + 1);
    let doc_ind = "  ".repeat(depth);
    let q = scope.qualifier();
    // A completion error raises the typed domain error for throwing
    // callables; the generic Error otherwise (panics, marshalling).
    let error_expr = match (f.error_strategy(), module.error.as_ref()) {
        (ErrorStrategy::Throws, Some(eb)) => {
            format!("{q}{}(code, msg)", rb_error_factory_name(eb))
        }
        _ => "Error.new(code, msg)".to_string(),
    };
    let params: Vec<String> = f.params.iter().map(|p| p.name.to_snake_case()).collect();

    let mut w = CodeWriter::two_space().with_depth(depth);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, &doc_ind);
    w.raw(d);
    w.line("# Blocks the current thread until the async producer completes; the");
    w.line(format!(
        "# result (or error) is delivered through the completion callback{}.",
        if f.cancellable {
            " (cancellation token not exposed; pass-through is NULL)"
        } else {
            ""
        }
    ));
    w.block(scope.def_open(f, &params), "end", |w| {
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('"', "\\\"");
            w.line(format!("warn \"[DEPRECATED] {escaped}\""));
        }

        w.line("queue = Queue.new");

        // Completion trampoline: (context, err, <result slots>).
        let cb_types = rb_abi_types(&a.callback_params, true);
        let mut cb_formals: Vec<String> = vec!["_context".into(), "err_ptr".into()];
        cb_formals.extend(a.callback_params.iter().skip(2).map(|p| p.name.clone()));
        w.block(
            format!(
                "callback = FFI::Function.new(:void, [{}]) do |{}|",
                cb_types.join(", "),
                cb_formals.join(", ")
            ),
            "end",
            |w| {
                // Producers pass err = NULL on success, so guard before dereferencing.
                w.line("err = err_ptr.null? ? nil : ErrorStruct.new(err_ptr)");
                w.line("if err && err[:code] != 0");
                w.scope(|w| {
                    w.line("code = err[:code]");
                    w.line("msg = err[:message].null? ? '' : err[:message].read_string");
                    w.line(format!("{q}weaveffi_error_clear(err_ptr)"));
                    w.line(format!("queue << {error_expr}"));
                });
                w.line("else");
                w.scope(|w| {
                    let mut tmp = String::new();
                    render_async_result_push(&mut tmp, &f.ret, &format!("{ind}    "));
                    w.raw(tmp);
                });
                w.line("end");
            },
        );

        for p in &f.params {
            let mut pc = String::new();
            render_param_conversion(&mut pc, &p.name.to_snake_case(), &p.ty, &ind);
            w.raw(pc);
        }
        let mut call_args: Vec<String> = Vec::new();
        if let Some(recv) = scope.self_arg() {
            call_args.push(recv.into());
        }
        for p in &f.params {
            call_args.extend(rb_call_args(&p.name.to_snake_case(), &p.ty));
        }
        if f.cancellable {
            call_args.push("FFI::Pointer::NULL".into());
        }
        call_args.push("callback".into());
        call_args.push("FFI::Pointer::NULL".into());
        w.line(format!("{q}{}({})", a.launch.symbol, call_args.join(", ")));
        w.line("value = queue.pop");
        w.line("raise value if value.is_a?(Error)");
        w.line("value");
    });
    out.push_str(&w.finish());
}

/// Push the converted async result onto the queue. Result slots are named by
/// [`abi::callback_result_params`]: `result` (+ `result_len`, or
/// `result_keys`/`result_values`/`result_len` for maps).
///
/// Per the async completion contract ([`weaveffi_core::plan::AsyncProtocol`]),
/// string, bytes, array, and map result buffers are producer-owned and
/// borrowed for the callback's duration: the callback deep-copies them before
/// returning and never frees them. Owned-object results (records, rich enums,
/// interfaces) are the exception: the callback receives ownership, so the
/// pointer is adopted by a finalizer-bearing wrapper.
fn render_async_result_push(out: &mut String, ret: &Option<TypeRef>, ind: &str) {
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    let Some(ty) = ret else {
        w.line("queue << nil");
        out.push_str(&w.finish());
        return;
    };
    match ty {
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
            w.line("queue << result");
        }
        TypeRef::Bool => {
            w.line("queue << (result != 0)");
        }
        TypeRef::Enum(_) => {
            w.line("queue << result");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            // Borrowed for the callback's duration: copy, don't free.
            w.line("queue << (result.null? ? '' : result.read_string)");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            // Borrowed for the callback's duration: copy, don't free.
            w.line("queue << (result.null? ? ''.b : result.read_string(result_len))");
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let local = local_type_name(name);
            w.line("if result.null?");
            w.scope(|w| {
                w.line("queue << Error.new(-1, 'null pointer')");
            });
            w.line("else");
            w.scope(|w| {
                w.line(format!("queue << {local}.new(result)"));
            });
            w.line("end");
        }
        // A returned interface transfers ownership of a new object reference;
        // wrap it without re-running initialize.
        TypeRef::Interface(name) => {
            let local = local_type_name(name);
            w.line("if result.null?");
            w.scope(|w| {
                w.line("queue << Error.new(-1, 'null pointer')");
            });
            w.line("else");
            w.scope(|w| {
                w.line(format!("queue << {local}._from_ptr(result)"));
            });
            w.line("end");
        }
        TypeRef::List(elem) => {
            // Borrowed array buffer: copy the elements, don't free. Object
            // elements transfer ownership and are adopted by their wrappers.
            let reader = rb_array_reader(elem);
            let map_suffix = match elem.as_ref() {
                TypeRef::Bool => ".map { |v| v != 0 }".to_string(),
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    ".map { |p| p.null? ? '' : p.read_string }".to_string()
                }
                TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
                    format!(".map {{ |p| {}.new(p) }}", local_type_name(name))
                }
                TypeRef::Interface(name) => {
                    format!(".map {{ |p| {}._from_ptr(p) }}", local_type_name(name))
                }
                _ => String::new(),
            };
            w.line(format!(
                "queue << (result.null? ? [] : result.{reader}(result_len){map_suffix})"
            ));
        }
        TypeRef::Map(k, v) => {
            let k_reader = rb_array_reader(k);
            let v_reader = rb_array_reader(v);
            let k_expr = rb_element_expr("k", k);
            let v_expr = rb_element_expr("v", v);
            w.line(format!(
                "queue << (result_keys.null? ? {{}} : result_keys.{k_reader}(result_len)\
                 .zip(result_values.{v_reader}(result_len))\
                 .each_with_object({{}}) {{ |(k, v), h| h[{k_expr}] = {v_expr} }})"
            ));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                // Borrowed for the callback's duration: copy, don't free.
                w.line("queue << (result.null? ? nil : result.read_string)");
            }
            TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
                let local = local_type_name(name);
                w.line(format!(
                    "queue << (result.null? ? nil : {local}.new(result))"
                ));
            }
            TypeRef::Interface(name) => {
                let local = local_type_name(name);
                w.line(format!(
                    "queue << (result.null? ? nil : {local}._from_ptr(result))"
                ));
            }
            TypeRef::Bool => {
                w.line("queue << (result.null? ? nil : (result.read_int32 != 0))");
            }
            _ if !is_c_pointer_type(inner) => {
                let read = rb_read_method(inner);
                w.line(format!("queue << (result.null? ? nil : result.{read})"));
            }
            _ => {
                let mut tmp = String::new();
                render_async_result_push(&mut tmp, &Some((**inner).clone()), ind);
                w.raw(tmp);
            }
        },
        TypeRef::Iterator(_) => unreachable!("async iterator returns are rejected upstream"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

/// Iterator wrapper: returns a lazy `Enumerator` per the pull contract stated
/// by [`weaveffi_core::plan::IteratorProtocol`].
///
/// The producer iterator launches *inside* the enumerator block, on the first
/// pull, so a handle cannot leak when the returned enumerator is never
/// started (launch errors therefore raise on the first pull rather than at
/// call time). Each consumer step issues exactly one C `next` call, each
/// yielded element is released per its element plan (strings and bytes freed
/// after copying; record and rich-enum pointers adopted by their
/// finalizer-bearing wrappers), and `destroy` runs exactly once from an
/// `ensure` block, so an early `break` or an error raised mid-iteration still
/// releases the handle. Launch and per-`next` errors follow the function's
/// [`ErrorStrategy`].
fn render_iterator_function_wrapper(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    it: &IteratorBinding,
    scope: &RbScope,
) {
    let depth = scope.depth();
    let ind = "  ".repeat(depth + 1);
    let doc_ind = "  ".repeat(depth);
    let q = scope.qualifier();
    let checker = rb_checker_name(f, module.error.as_ref());
    let params: Vec<String> = f.params.iter().map(|p| p.name.to_snake_case()).collect();

    let mut w = CodeWriter::two_space().with_depth(depth);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, &doc_ind);
    w.raw(d);
    w.line("# Returns a lazy Enumerator that streams one element per pull; call");
    w.line("# `.to_a` to collect eagerly. The underlying producer iterator is");
    w.line("# launched on the first pull, so launch errors raise at that point");
    w.line("# rather than when this method returns. The iterator handle is");
    w.line("# released exactly once, when iteration finishes or is abandoned");
    w.line("# early (for example by `break`).");
    w.block(scope.def_open(f, &params), "end", |w| {
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('"', "\\\"");
            w.line(format!("warn \"[DEPRECATED] {escaped}\""));
        }
        for p in &f.params {
            let mut pc = String::new();
            render_param_conversion(&mut pc, &p.name.to_snake_case(), &p.ty, &ind);
            w.raw(pc);
        }
        let mut call_args: Vec<String> = Vec::new();
        if let Some(recv) = scope.self_arg() {
            call_args.push(recv.into());
        }
        for p in &f.params {
            call_args.extend(rb_call_args(&p.name.to_snake_case(), &p.ty));
        }
        call_args.push("err".into());
        // The block closes over the converted argument buffers above, so they
        // stay referenced (and un-collected) until the launch call runs.
        w.block("Enumerator.new do |y|", "end", |w| {
            w.line("err = ErrorStruct.new");
            w.line(format!(
                "iter = {q}{}({})",
                it.launch.symbol,
                call_args.join(", ")
            ));
            w.line("begin");
            w.scope(|w| {
                w.line(format!("{q}{checker}(err)"));
                w.line("unless iter.null?");
                w.scope(|w| {
                    w.block("loop do", "end", |w| {
                        // `next` params: (iter, out_item, <elem out slots>, out_err).
                        let elem = &it.elem;
                        let needs_len = matches!(elem, TypeRef::Bytes | TypeRef::BorrowedBytes);
                        let item_mem = rb_mem_type(elem);
                        w.line(format!("out_item = FFI::MemoryPointer.new({item_mem})"));
                        if needs_len {
                            w.line("out_item_len = FFI::MemoryPointer.new(:size_t)");
                        }
                        w.line("item_err = ErrorStruct.new");
                        let next_args = if needs_len {
                            "iter, out_item, out_item_len, item_err"
                        } else {
                            "iter, out_item, item_err"
                        };
                        w.line(format!("has_item = {q}{}({next_args})", it.next.symbol));
                        w.line(format!("{q}{checker}(item_err)"));
                        w.line("break if has_item.zero?");
                        let mut tmp = String::new();
                        render_iterator_item_yield(
                            &mut tmp,
                            elem,
                            &"  ".repeat(depth + 5),
                            scope.module_name(),
                        );
                        w.raw(tmp);
                    });
                });
                w.line("end");
            });
            w.line("ensure");
            w.scope(|w| {
                // Exactly one destroy per launched handle: this ensure runs
                // once whether iteration exhausts, raises, or is abandoned by
                // an early break from the consumer.
                w.line(format!("{q}{}(iter) unless iter.null?", it.destroy_symbol));
            });
            w.line("end");
        });
    });
    out.push_str(&w.finish());
}

/// Convert the value written into `out_item` and yield it to the enumerator's
/// yielder `y`, releasing the element per its [`weaveffi_core::plan::ElemFree`]
/// plan first (copy, free, then yield, so an early `break` during the yield
/// cannot leak the element). `qualifier` is the top-level Ruby module name
/// when rendering inside a class body, where `weaveffi_free_*` calls need an
/// explicit receiver.
fn render_iterator_item_yield(
    out: &mut String,
    elem: &TypeRef,
    ind: &str,
    qualifier: Option<&str>,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("item_ptr = out_item.read_pointer");
            w.line("if item_ptr.null?");
            w.scope(|w| {
                w.line("y << ''");
            });
            w.line("else");
            w.scope(|w| {
                w.line("item = item_ptr.read_string");
                w.line(format!("{m}weaveffi_free_string(item_ptr)"));
                w.line("y << item");
            });
            w.line("end");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("item_ptr = out_item.read_pointer");
            w.line("item_len = out_item_len.read(:size_t)");
            w.line("if item_ptr.null?");
            w.scope(|w| {
                w.line("y << ''.b");
            });
            w.line("else");
            w.scope(|w| {
                w.line("item = item_ptr.read_string(item_len)");
                w.line(format!("{m}weaveffi_free_bytes(item_ptr, item_len)"));
                w.line("y << item");
            });
            w.line("end");
        }
        // A yielded record or rich enum is a new owned reference; the wrapper
        // adopts the pointer and its finalizer runs the destroy symbol.
        TypeRef::Record(name) | TypeRef::RichEnum(name) | TypeRef::TypedHandle(name) => {
            let local = local_type_name(name);
            w.line("item_ptr = out_item.read_pointer");
            w.line(format!("y << {local}.new(item_ptr) unless item_ptr.null?"));
        }
        // A yielded interface is a new owned reference; wrap it without
        // re-running initialize.
        TypeRef::Interface(name) => {
            let local = local_type_name(name);
            w.line("item_ptr = out_item.read_pointer");
            w.line(format!(
                "y << {local}._from_ptr(item_ptr) unless item_ptr.null?"
            ));
        }
        TypeRef::Bool => {
            w.line("y << (out_item.read_int32 != 0)");
        }
        _ => {
            let read = rb_read_method(elem);
            w.line(format!("y << out_item.{read}"));
        }
    }
    out.push_str(&w.finish());
}

// ── Parameter conversion ──

fn render_param_conversion(out: &mut String, name: &str, ty: &TypeRef, ind: &str) {
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match ty {
        TypeRef::Bool => {
            w.line(format!("{name}_c = {name} ? 1 : 0"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "{name}_buf = FFI::MemoryPointer.new(:uint8, {name}.bytesize)"
            ));
            w.line(format!("{name}_buf.put_bytes(0, {name})"));
        }
        TypeRef::Optional(inner) if !is_c_pointer_type(inner) => {
            let mem = rb_mem_type(inner);
            let write = rb_write_method(inner);
            let val = match inner.as_ref() {
                TypeRef::Bool => format!("{name} ? 1 : 0"),
                _ => name.to_string(),
            };
            w.line(format!(
                "{name}_c = {name}.nil? ? FFI::Pointer::NULL : \
                 begin; p = FFI::MemoryPointer.new({mem}); p.{write}({val}); p; end"
            ));
        }
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Bytes | TypeRef::BorrowedBytes => {
                w.line(format!("if {name}.nil?"));
                w.scope(|w| {
                    w.line(format!("{name}_buf = FFI::Pointer::NULL"));
                    w.line(format!("{name}_len = 0"));
                });
                w.line("else");
                w.scope(|w| {
                    w.line(format!(
                        "{name}_buf = FFI::MemoryPointer.new(:uint8, {name}.bytesize)"
                    ));
                    w.line(format!("{name}_buf.put_bytes(0, {name})"));
                    w.line(format!("{name}_len = {name}.bytesize"));
                });
                w.line("end");
            }
            TypeRef::List(elem) => {
                w.line(format!("if {name}.nil?"));
                w.scope(|w| {
                    w.line(format!("{name}_buf = FFI::Pointer::NULL"));
                    w.line(format!("{name}_len = 0"));
                });
                w.line("else");
                w.scope(|w| {
                    let mut tmp = String::new();
                    render_list_buf(&mut tmp, name, elem, &format!("{ind}  "));
                    w.raw(tmp);
                    w.line(format!("{name}_len = {name}.length"));
                });
                w.line("end");
            }
            TypeRef::Map(k, v) => {
                w.line(format!("if {name}.nil?"));
                w.scope(|w| {
                    w.line(format!("{name}_keys_buf = FFI::Pointer::NULL"));
                    w.line(format!("{name}_vals_buf = FFI::Pointer::NULL"));
                    w.line(format!("{name}_len = 0"));
                });
                w.line("else");
                w.scope(|w| {
                    let mut tmp = String::new();
                    render_map_buf(&mut tmp, name, k, v, &format!("{ind}  "));
                    w.raw(tmp);
                });
                w.line("end");
            }
            _ => {}
        },
        TypeRef::List(elem) => {
            let mut tmp = String::new();
            render_list_buf(&mut tmp, name, elem, ind);
            w.raw(tmp);
        }
        TypeRef::Map(k, v) => {
            let mut tmp = String::new();
            render_map_buf(&mut tmp, name, k, v, ind);
            w.raw(tmp);
        }
        _ => {}
    }
    out.push_str(&w.finish());
}

/// Writes one element list into `{buf_name}_buf`. String/handle elements are
/// converted to pointers first, and the converted array is kept in a local
/// (`{buf_name}_ptrs`) so the per-element `MemoryPointer`s stay referenced,
/// and un-collected, until after the C call.
fn render_element_array_write(
    out: &mut String,
    buf_name: &str,
    list_expr: &str,
    elem: &TypeRef,
    ind: &str,
) {
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match elem {
        TypeRef::Bool => {
            w.line(format!(
                "{buf_name}_buf.write_array_of_int32({list_expr}.map {{ |v| v ? 1 : 0 }})"
            ));
        }
        TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::TypedHandle(_)
        | TypeRef::Interface(_) => {
            w.line(format!(
                "{buf_name}_buf.write_array_of_pointer({list_expr}.map(&:handle))"
            ));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "{buf_name}_ptrs = {list_expr}.map {{ |s| FFI::MemoryPointer.from_string(s) }}"
            ));
            w.line(format!(
                "{buf_name}_buf.write_array_of_pointer({buf_name}_ptrs)"
            ));
        }
        _ => {
            let write = rb_array_writer(elem);
            w.line(format!("{buf_name}_buf.{write}({list_expr})"));
        }
    }
    out.push_str(&w.finish());
}

fn render_list_buf(out: &mut String, name: &str, elem: &TypeRef, ind: &str) {
    let mem = rb_mem_type(elem);
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    w.line(format!(
        "{name}_buf = FFI::MemoryPointer.new({mem}, {name}.length)"
    ));
    out.push_str(&w.finish());
    render_element_array_write(out, name, name, elem, ind);
}

fn render_map_buf(out: &mut String, name: &str, k: &TypeRef, v: &TypeRef, ind: &str) {
    let k_mem = rb_mem_type(k);
    let v_mem = rb_mem_type(v);
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    w.line(format!("{name}_k = {name}.keys"));
    w.line(format!("{name}_v = {name}.values"));
    w.line(format!(
        "{name}_keys_buf = FFI::MemoryPointer.new({k_mem}, {name}_k.length)"
    ));
    w.line(format!(
        "{name}_vals_buf = FFI::MemoryPointer.new({v_mem}, {name}_v.length)"
    ));
    out.push_str(&w.finish());
    render_element_array_write(out, &format!("{name}_keys"), &format!("{name}_k"), k, ind);
    render_element_array_write(out, &format!("{name}_vals"), &format!("{name}_v"), v, ind);
}

// ── Return value rendering ──

fn render_return_code(out: &mut String, ty: &TypeRef, ind: &str, qualifier: Option<&str>) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match ty {
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
        | TypeRef::Handle
        | TypeRef::Enum(_) => {
            w.line("result");
        }
        TypeRef::Bool => {
            w.line("result != 0");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("return '' if result.null?");
            w.line("str = result.read_string");
            w.line(format!("{m}weaveffi_free_string(result)"));
            w.line("str");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("return ''.b if result.null?");
            w.line("len = out_len.read(:size_t)");
            w.line("data = result.read_string(len)");
            w.line(format!("{m}weaveffi_free_bytes(result, len)"));
            w.line("data");
        }
        TypeRef::TypedHandle(name) => {
            w.line("raise Error.new(-1, 'null pointer') if result.null?");
            w.line(format!("{name}.new(result)"));
        }
        // An owned object return is adopted by its finalizer-bearing wrapper,
        // which eventually runs the destroy symbol.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            w.line("raise Error.new(-1, 'null pointer') if result.null?");
            w.line(format!("{}.new(result)", local_type_name(name)));
        }
        TypeRef::Optional(inner) => {
            let mut tmp = String::new();
            render_optional_return_code(&mut tmp, inner, ind, qualifier);
            w.raw(tmp);
        }
        TypeRef::List(inner) => {
            w.line("return [] if result.null?");
            let mut tmp = String::new();
            render_list_return_body(&mut tmp, inner, ind, qualifier);
            w.raw(tmp);
        }
        TypeRef::Iterator(_) => {
            unreachable!("iterator returns render via render_iterator_function_wrapper")
        }
        TypeRef::Map(_, _) => {
            w.line("result");
        }
        // A returned interface transfers ownership of a new object reference;
        // wrap it without re-running initialize.
        TypeRef::Interface(name) => {
            let local = local_type_name(name);
            w.line("raise Error.new(-1, 'null pointer') if result.null?");
            w.line(format!("{local}._from_ptr(result)"));
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
    out.push_str(&w.finish());
}

fn render_optional_return_code(
    out: &mut String,
    inner: &TypeRef,
    ind: &str,
    qualifier: Option<&str>,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("return nil if result.null?");
            w.line("str = result.read_string");
            w.line(format!("{m}weaveffi_free_string(result)"));
            w.line("str");
        }
        TypeRef::TypedHandle(name) => {
            w.line("return nil if result.null?");
            w.line(format!("{name}.new(result)"));
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            w.line("return nil if result.null?");
            w.line(format!("{}.new(result)", local_type_name(name)));
        }
        // An absent optional interface is a null pointer; a present one is a
        // new owned reference wrapped without re-running initialize.
        TypeRef::Interface(name) => {
            w.line("return nil if result.null?");
            w.line(format!("{}._from_ptr(result)", local_type_name(name)));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("return nil if result.null?");
            w.line("len = out_len.read(:size_t)");
            w.line("data = result.read_string(len)");
            w.line(format!("{m}weaveffi_free_bytes(result, len)"));
            w.line("data");
        }
        // A present boxed scalar is a producer-allocated buffer: dereference,
        // then release it with the runtime's free_bytes.
        TypeRef::Bool => {
            w.line("return nil if result.null?");
            w.line("value = result.read_int32 != 0");
            w.line(format!(
                "{m}weaveffi_free_bytes(result, FFI.type_size(:int32))"
            ));
            w.line("value");
        }
        TypeRef::List(elem) => {
            w.line("return nil if result.null?");
            let mut tmp = String::new();
            render_list_return_body(&mut tmp, elem, ind, qualifier);
            w.raw(tmp);
        }
        TypeRef::Map(k, v) => {
            let mut tmp = String::new();
            render_map_return_code(&mut tmp, k, v, ind, true, qualifier);
            w.raw(tmp);
        }
        _ if !is_c_pointer_type(inner) => {
            let read = rb_read_method(inner);
            let mem = rb_mem_type(inner);
            w.line("return nil if result.null?");
            w.line(format!("value = result.{read}"));
            w.line(format!(
                "{m}weaveffi_free_bytes(result, FFI.type_size({mem}))"
            ));
            w.line("value");
        }
        _ => {
            w.line("result");
        }
    }
    out.push_str(&w.finish());
}

/// The `.each` line releasing one converted element array per its
/// [`weaveffi_core::plan::ElemFree`] plan: string elements are freed with the
/// runtime's `free_string`; record and rich-enum elements owe nothing here
/// because their wrappers adopted the pointers; by-value elements owe nothing.
fn rb_elem_free_line(list_var: &str, elem: &TypeRef, m: &str) -> Option<String> {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => Some(format!(
            "{list_var}.each {{ |p| {m}weaveffi_free_string(p) unless p.null? }}"
        )),
        _ => None,
    }
}

/// Render the body converting an array return (`result` + `out_len`) into a
/// Ruby Array, honoring [`weaveffi_core::plan::ReturnFree::Array`]: each
/// element is released per its element plan after copying (object elements
/// are adopted by their wrappers instead), then the array buffer itself is
/// released with the runtime's `free_bytes`.
fn render_list_return_body(out: &mut String, inner: &TypeRef, ind: &str, qualifier: Option<&str>) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    w.line("len = out_len.read(:size_t)");
    let reader = rb_array_reader(inner);
    let mem = rb_mem_type(inner);
    match inner {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("ptrs = result.{reader}(len)"));
            w.line("items = ptrs.map { |p| p.null? ? '' : p.read_string }");
            if let Some(free) = rb_elem_free_line("ptrs", inner, &m) {
                w.line(free);
            }
        }
        TypeRef::TypedHandle(name) => {
            w.line(format!(
                "items = result.{reader}(len).map {{ |p| {name}.new(p) }}"
            ));
        }
        // Record and rich-enum elements transfer ownership; the wrappers
        // adopt the pointers, so no per-element free is owed here.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            let local = local_type_name(name);
            w.line(format!(
                "items = result.{reader}(len).map {{ |p| {local}.new(p) }}"
            ));
        }
        // Listed interfaces are new owned references; wrap each without
        // re-running initialize.
        TypeRef::Interface(name) => {
            let local = local_type_name(name);
            w.line(format!(
                "items = result.{reader}(len).map {{ |p| {local}._from_ptr(p) }}"
            ));
        }
        TypeRef::Bool => {
            w.line(format!("items = result.{reader}(len).map {{ |v| v != 0 }}"));
        }
        _ => {
            w.line(format!("items = result.{reader}(len)"));
        }
    }
    w.line(format!(
        "{m}weaveffi_free_bytes(result, len * FFI.type_size({mem}))"
    ));
    w.line("items");
    out.push_str(&w.finish());
}

/// Render the body converting a map return (parallel `out_keys`/`out_values`
/// buffers + `out_len`) into a Ruby Hash, honoring
/// [`weaveffi_core::plan::ReturnFree::MapBuffers`]: each key and value is
/// released per its element plan after copying, then both parallel arrays are
/// released with the runtime's `free_bytes`.
fn render_map_return_code(
    out: &mut String,
    k: &TypeRef,
    v: &TypeRef,
    ind: &str,
    optional: bool,
    qualifier: Option<&str>,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let null_val = if optional { "nil" } else { "{}" };
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    w.line("len = out_len.read(:size_t)");
    w.line("keys_ptr = out_keys.read_pointer");
    w.line("vals_ptr = out_values.read_pointer");
    w.line(format!(
        "return {null_val} if keys_ptr.null? || vals_ptr.null?"
    ));
    let k_reader = rb_array_reader(k);
    let v_reader = rb_array_reader(v);
    let k_expr = rb_element_expr("k", k);
    let v_expr = rb_element_expr("v", v);
    w.line(format!("keys = keys_ptr.{k_reader}(len)"));
    w.line(format!("vals = vals_ptr.{v_reader}(len)"));
    w.line(format!(
        "hash = keys.zip(vals)\
         .each_with_object({{}}) {{ |(k, v), h| h[{k_expr}] = {v_expr} }}"
    ));
    if let Some(free) = rb_elem_free_line("keys", k, &m) {
        w.line(free);
    }
    if let Some(free) = rb_elem_free_line("vals", v, &m) {
        w.line(free);
    }
    w.line(format!(
        "{m}weaveffi_free_bytes(keys_ptr, len * FFI.type_size({}))",
        rb_mem_type(k)
    ));
    w.line(format!(
        "{m}weaveffi_free_bytes(vals_ptr, len * FFI.type_size({}))",
        rb_mem_type(v)
    ));
    w.line("hash");
    out.push_str(&w.finish());
}

fn render_gemspec(package: &ResolvedPackage, gem_file: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, gem_file);
    let name = &package.name;
    let version = &package.version;
    let summary = package.description_or_default().replace('\'', "\\'");
    let mut extra = String::new();
    if !package.authors.is_empty() {
        let authors = package
            .authors
            .iter()
            .map(|a| format!("'{}'", a.replace('\'', "\\'")))
            .collect::<Vec<_>>()
            .join(", ");
        extra.push_str(&format!("  s.authors     = [{authors}]\n"));
    }
    if let Some(license) = &package.license {
        extra.push_str(&format!("  s.license     = '{license}'\n"));
    }
    if let Some(homepage) = package.homepage.as_ref().or(package.repository.as_ref()) {
        extra.push_str(&format!("  s.homepage    = '{homepage}'\n"));
    }
    format!(
        "{prelude}Gem::Specification.new do |s|
  s.name        = '{name}'
  s.version     = '{version}'
  s.summary     = '{summary}'
{extra}  s.files       = Dir['lib/**/*.rb']
  s.require_paths = ['lib']

  s.add_dependency 'ffi', '~> 1.15'
end

{trailer}"
    )
}

fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let version = &package.version;
    let require_name = package.ident_name();
    format!(
        r#"{prelude}# {name} (Ruby)

Auto-generated Ruby bindings using the [ffi](https://github.com/ffi/ffi) gem.

## Prerequisites

- Ruby >= 2.7
- The compiled shared library (`libweaveffi.so`, `libweaveffi.dylib`, or `weaveffi.dll`) available on your library search path.

## Install

```bash
gem build {name}.gemspec
gem install {name}-{version}.gem
```

## Usage

```ruby
require '{require_name}'
```

{trailer}"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use camino::Utf8Path;
    use weaveffi_core::codegen::Generator;

    #[test]
    fn package_emits_platform_gems_and_swaps_loader() {
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = make_api(vec![simple_module(
            "calc",
            vec![Function {
                name: "ping".into(),
                params: vec![],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::LinuxX64, "/s/linux-x64/libcalculator.so");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &RubyGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &RubyConfig::default(),
        )
        .expect("ruby supports packaging");

        assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
        // Bundled under lib/native/ inside each per-platform gem dir.
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("ruby/darwin-arm64/lib/native/libcalculator.dylib")));
        // The gemspec stamps the RubyGems platform string.
        let gemspec = files
            .iter()
            .find(|f| f.path.as_str().ends_with("darwin-arm64/weaveffi.gemspec"))
            .expect("gemspec present");
        let FileContent::Text(spec) = &gemspec.content else {
            panic!("gemspec is text");
        };
        assert!(
            spec.contains("s.platform    = 'arm64-darwin'"),
            "platform: {spec}"
        );
        // The loader was rewritten to prefer the bundled library.
        let rb = files
            .iter()
            .find(|f| f.path.as_str().ends_with("darwin-arm64/lib/weaveffi.rb"))
            .expect("library module present");
        let FileContent::Text(src) = &rb.content else {
            panic!("module is text");
        };
        assert!(
            src.contains("File.exist?") && src.contains("libcalculator.dylib"),
            "packaged loader not applied: {src}"
        );
    }
    use weaveffi_ir::ir::{
        Api, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef, Module, Param,
        StructDef, StructField, TypeRef,
    };

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.5.0".to_string(),
            modules,
            generators: None,
            package: None,
        }
    }

    fn simple_module(name: &str, functions: Vec<Function>) -> Module {
        Module {
            name: name.into(),
            functions,
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    /// Build the model (test-only; the driver builds it in production) and
    /// render with the default naming (module-prefix stripping on).
    fn render(api: &Api, module_name: &str, prefix: &str) -> String {
        let model = BindingModel::build(api, prefix);
        render_ruby_module(&model, module_name, true, "weaveffi.rb", "weaveffi.yml")
    }

    /// A function literal with the boilerplate zeroed; tests override the
    /// fields they exercise.
    fn plain_fn(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws: false,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    fn str_param(name: &str) -> Param {
        Param {
            name: name.into(),
            ty: TypeRef::StringUtf8,
            mutable: false,
            doc: None,
        }
    }

    /// A `kv` module with a declared error domain, an interface with a `new`
    /// constructor, a factory constructor, methods (sync throwing, sync
    /// non-throwing, async), a static, plus throwing and non-throwing free
    /// functions.
    fn kv_api() -> Api {
        let mut m = simple_module(
            "kv",
            vec![
                {
                    let mut f = plain_fn(
                        "kv_lookup",
                        vec![str_param("key")],
                        Some(TypeRef::StringUtf8),
                    );
                    f.throws = true;
                    f
                },
                plain_fn("kv_ping", vec![], Some(TypeRef::Bool)),
            ],
        );
        m.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![
                ErrorCode {
                    name: "KeyNotFound".into(),
                    code: 1001,
                    message: "key not found".into(),
                    doc: Some("Raised when the key is absent.".into()),
                },
                ErrorCode {
                    name: "IoError".into(),
                    code: 1004,
                    message: "I/O failure".into(),
                    doc: None,
                },
            ],
        });
        m.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: Some("A key-value store.".into()),
            constructors: vec![
                {
                    let mut f = plain_fn("new", vec![str_param("path")], None);
                    f.throws = true;
                    f
                },
                {
                    let mut f = plain_fn("open", vec![str_param("path")], None);
                    f.throws = true;
                    f
                },
            ],
            methods: vec![
                {
                    let mut f = plain_fn("put", vec![str_param("key"), str_param("value")], None);
                    f.throws = true;
                    f
                },
                plain_fn("count", vec![], Some(TypeRef::U32)),
                {
                    let mut f = plain_fn("compact", vec![], Some(TypeRef::Bool));
                    f.r#async = true;
                    f.cancellable = true;
                    f.throws = true;
                    f
                },
            ],
            statics: vec![plain_fn("default_capacity", vec![], Some(TypeRef::U32))],
        }];
        make_api(vec![m])
    }

    #[test]
    fn name_returns_ruby() {
        assert_eq!(Generator::name(&RubyGenerator), "ruby");
    }

    #[test]
    fn interface_ffi_attaches_destroy_and_members() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        assert!(
            code.contains("attach_function :weaveffi_kv_Store_destroy, [:pointer], :void"),
            "destroy attach: {code}"
        );
        assert!(
            code.contains("attach_function :weaveffi_kv_Store_new, [:string, :pointer], :pointer"),
            "ctor attach: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_kv_Store_put, [:pointer, :string, :string, :pointer], :void"
            ),
            "method attach includes self slot: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_kv_Store_default_capacity, [:pointer], :uint32"
            ),
            "static attach has no self slot: {code}"
        );
    }

    #[test]
    fn interface_class_wraps_pointer_with_auto_pointer() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        assert!(
            code.contains("class StorePtr < FFI::AutoPointer"),
            "AutoPointer subclass: {code}"
        );
        assert!(
            code.contains("WeaveFFI.weaveffi_kv_Store_destroy(ptr)"),
            "release calls destroy symbol: {code}"
        );
        assert!(code.contains("def destroy"), "explicit destroy: {code}");
        assert!(code.contains("@handle.free"), "destroy frees: {code}");
        assert!(
            code.contains("def self._from_ptr(ptr)") && code.contains("obj = allocate"),
            "_from_ptr avoids initialize: {code}"
        );
    }

    #[test]
    fn interface_new_ctor_maps_to_initialize() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        assert!(code.contains("def initialize(path)"), "initialize: {code}");
        assert!(
            code.contains("result = WeaveFFI.weaveffi_kv_Store_new(path, err)"),
            "ctor call: {code}"
        );
        assert!(
            code.contains("@handle = StorePtr.new(result)"),
            "handle assignment: {code}"
        );
    }

    #[test]
    fn interface_named_ctor_is_class_method_factory() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        assert!(code.contains("def self.open(path)"), "factory def: {code}");
        assert!(
            code.contains("result = WeaveFFI.weaveffi_kv_Store_open(path, err)"),
            "factory call: {code}"
        );
        assert!(
            code.contains("_from_ptr(result)"),
            "factory wraps without initialize: {code}"
        );
    }

    #[test]
    fn interface_method_passes_handle_first() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        assert!(code.contains("def put(key, value)"), "method def: {code}");
        assert!(
            code.contains("WeaveFFI.weaveffi_kv_Store_put(@handle, key, value, err)"),
            "self slot leads: {code}"
        );
    }

    #[test]
    fn interface_static_is_class_method() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        assert!(
            code.contains("def self.default_capacity()"),
            "static def: {code}"
        );
        assert!(
            code.contains("result = WeaveFFI.weaveffi_kv_Store_default_capacity(err)"),
            "static call has no self slot: {code}"
        );
    }

    #[test]
    fn typed_error_classes_and_helpers() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        assert!(code.contains("class KvError < Error"), "domain: {code}");
        assert!(
            code.contains("class KeyNotFound < KvError"),
            "code subclass: {code}"
        );
        assert!(code.contains("CODE = 1001"), "code constant: {code}");
        assert!(
            code.contains("def initialize(message = 'key not found')"),
            "default message: {code}"
        );
        assert!(
            code.contains("1004 => KvError::IoError,"),
            "code table: {code}"
        );
        assert!(
            code.contains("def self.kv_error_from(code, message)"),
            "factory helper: {code}"
        );
        assert!(
            code.contains("def self.check_kv_error!(err)"),
            "checker helper: {code}"
        );
        assert!(
            code.contains("raise kv_error_from(code, msg)"),
            "checker raises typed: {code}"
        );
    }

    #[test]
    fn throwing_function_uses_typed_checker() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        let lookup = code
            .split("def self.kv_lookup(key)")
            .nth(1)
            .expect("kv_lookup wrapper");
        assert!(
            lookup.contains("check_kv_error!(err)"),
            "typed checker: {code}"
        );
    }

    #[test]
    fn non_throwing_function_uses_generic_checker() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        let ping = code
            .split("def self.kv_ping()")
            .nth(1)
            .expect("kv_ping wrapper");
        let body = ping.split("\n  end").next().expect("wrapper body");
        assert!(body.contains("check_error!(err)"), "generic: {code}");
        assert!(
            !body.contains("check_kv_error!"),
            "no typed checker: {code}"
        );
    }

    #[test]
    fn non_throwing_method_uses_generic_checker() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        let count = code.split("def count()").nth(1).expect("count wrapper");
        let body = count.split("\n    end").next().expect("method body");
        assert!(
            body.contains("WeaveFFI.check_error!(err)"),
            "generic qualified: {code}"
        );
        assert!(
            !body.contains("check_kv_error!"),
            "no typed checker: {code}"
        );
    }

    #[test]
    fn async_member_routes_typed_error_and_self_slot() {
        let code = render(&kv_api(), "WeaveFFI", "weaveffi");
        let compact = code.split("def compact()").nth(1).expect("compact wrapper");
        assert!(
            compact.contains("queue << WeaveFFI.kv_error_from(code, msg)"),
            "typed async error: {code}"
        );
        assert!(
            compact.contains(
                "WeaveFFI.weaveffi_kv_Store_compact_async(@handle, FFI::Pointer::NULL, callback, FFI::Pointer::NULL)"
            ),
            "self slot then cancel token: {code}"
        );
    }

    #[test]
    fn interface_params_borrow_and_returns_wrap() {
        let mut m = simple_module(
            "kv",
            vec![
                plain_fn(
                    "clone_store",
                    vec![Param {
                        name: "store".into(),
                        ty: TypeRef::Interface("Store".into()),
                        mutable: false,
                        doc: None,
                    }],
                    Some(TypeRef::Interface("Store".into())),
                ),
                plain_fn(
                    "find_store",
                    vec![],
                    Some(TypeRef::Optional(Box::new(TypeRef::Interface(
                        "Store".into(),
                    )))),
                ),
            ],
        );
        m.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![plain_fn("new", vec![], None)],
            methods: vec![],
            statics: vec![],
        }];
        let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
        assert!(
            code.contains("weaveffi_kv_clone_store(store.handle, err)"),
            "param borrows handle: {code}"
        );
        assert!(
            code.contains("Store._from_ptr(result)"),
            "return wraps owned pointer: {code}"
        );
        let find = code
            .split("def self.find_store()")
            .nth(1)
            .expect("find_store wrapper");
        assert!(
            find.contains("return nil if result.null?"),
            "optional interface nil: {code}"
        );
    }

    #[test]
    fn naming_strips_module_prefix_by_default() {
        let api = make_api(vec![simple_module(
            "kv",
            vec![plain_fn("open_store", vec![], None)],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(
            code.contains("def self.open_store()"),
            "stripped name: {code}"
        );
        assert!(
            !code.contains("def self.kv_open_store()"),
            "no prefixed wrapper: {code}"
        );
        // The C symbol stays fully qualified regardless of wrapper naming.
        assert!(
            code.contains("weaveffi_kv_open_store(err)"),
            "C symbol: {code}"
        );
    }

    #[test]
    fn naming_knob_restores_prefixed_wrappers() {
        let api = make_api(vec![simple_module(
            "kv",
            vec![plain_fn("open_store", vec![], None)],
        )]);
        let model = BindingModel::build(&api, "weaveffi");
        let code = render_ruby_module(&model, "WeaveFFI", false, "weaveffi.rb", "weaveffi.yml");
        assert!(
            code.contains("def self.kv_open_store()"),
            "prefixed name: {code}"
        );
    }

    #[test]
    fn throwing_iterator_uses_typed_checker() {
        let mut m = simple_module("kv", {
            let mut f = plain_fn(
                "scan",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            );
            f.throws = true;
            vec![f]
        });
        m.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![ErrorCode {
                name: "IoError".into(),
                code: 1004,
                message: "I/O failure".into(),
                doc: None,
            }],
        });
        let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
        let scan = code.split("def self.scan()").nth(1).expect("scan wrapper");
        assert!(
            scan.contains("check_kv_error!(err)"),
            "launch checker: {code}"
        );
        assert!(
            scan.contains("check_kv_error!(item_err)"),
            "next checker: {code}"
        );
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        RubyGenerator
            .generate(&api, out_dir, &RubyConfig::default())
            .unwrap();

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
        let files = RubyGenerator.output_files(&api, out_dir, &RubyConfig::default());
        assert_eq!(
            files,
            vec![
                format!("{out_dir}/ruby/README.md"),
                format!("{out_dir}/ruby/lib/weaveffi.rb"),
                format!("{out_dir}/ruby/weaveffi.gemspec"),
            ]
        );
    }

    #[test]
    fn ruby_generates_gemspec() {
        let api = make_api(vec![simple_module("math", vec![])]);
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        RubyGenerator
            .generate(&api, out_dir, &RubyConfig::default())
            .unwrap();

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
            interfaces: vec![],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
                variants: vec![
                    EnumVariant {
                        name: "Red".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "DarkBlue".into(),
                        value: 1,
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

        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(code.contains("module Color"), "enum module: {code}");
        assert!(code.contains("RED = 0"), "RED: {code}");
        assert!(code.contains("DARK_BLUE = 1"), "DARK_BLUE: {code}");
    }

    #[test]
    fn renders_struct_with_auto_pointer() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            interfaces: vec![],
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

        let code = render(&api, "WeaveFFI", "weaveffi");
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
            interfaces: vec![],
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

        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(code.contains("class PointBuilder"), "builder class: {code}");
        assert!(code.contains("def with_x(value)"), "with_x: {code}");
        // Unset fields default to zero values rather than raising.
        assert!(code.contains("@x = 0.0"), "zero default: {code}");
        // Build is FFI-backed: it attaches and calls the C create symbol,
        // checks the error, and wraps the returned handle.
        assert!(
            code.contains("attach_function :weaveffi_geo_Point_create"),
            "create attach: {code}"
        );
        assert!(
            code.contains("result = WeaveFFI.weaveffi_geo_Point_create(x, err)"),
            "create call: {code}"
        );
        assert!(
            code.contains("WeaveFFI.check_error!(err)"),
            "error check: {code}"
        );
        assert!(code.contains("Point.new(result)"), "wrap handle: {code}");
        assert!(
            !code.contains("requires FFI backing"),
            "stub must be gone: {code}"
        );
    }

    #[test]
    fn struct_getter_frees_string() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![],
            interfaces: vec![],
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

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                    doc: None,
                }],
                returns: Some(TypeRef::Bool),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(code.contains("out_keys"), "out_keys: {code}");
        assert!(code.contains("out_values"), "out_values: {code}");
        assert!(code.contains("each_with_object"), "hash build: {code}");
    }

    #[test]
    fn list_of_strings_return_frees_elements_and_buffer() {
        let api = make_api(vec![simple_module(
            "data",
            vec![plain_fn(
                "list_names",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            )],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        // Each string element is freed after copying, then the array buffer
        // itself is released.
        assert!(
            code.contains("ptrs.each { |p| weaveffi_free_string(p) unless p.null? }"),
            "element strings freed: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(result, len * FFI.type_size(:pointer))"),
            "array buffer freed: {code}"
        );
    }

    #[test]
    fn scalar_list_return_frees_buffer() {
        let api = make_api(vec![simple_module(
            "data",
            vec![plain_fn(
                "list_ids",
                vec![],
                Some(TypeRef::List(Box::new(TypeRef::I32))),
            )],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(
            code.contains("weaveffi_free_bytes(result, len * FFI.type_size(:int32))"),
            "array buffer freed: {code}"
        );
    }

    #[test]
    fn map_return_frees_key_strings_and_both_buffers() {
        let api = make_api(vec![simple_module(
            "data",
            vec![plain_fn(
                "get_metadata",
                vec![],
                Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
            )],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(
            code.contains("keys.each { |p| weaveffi_free_string(p) unless p.null? }"),
            "key strings freed: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(keys_ptr, len * FFI.type_size(:pointer))"),
            "keys buffer freed: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(vals_ptr, len * FFI.type_size(:int32))"),
            "values buffer freed: {code}"
        );
    }

    #[test]
    fn boxed_optional_scalar_return_is_freed() {
        let api = make_api(vec![simple_module(
            "data",
            vec![plain_fn(
                "find_count",
                vec![],
                Some(TypeRef::Optional(Box::new(TypeRef::I32))),
            )],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        // The producer boxes an optional scalar; the wrapper dereferences,
        // then releases the box.
        assert!(
            code.contains("value = result.read_int32"),
            "boxed scalar dereferenced: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(result, FFI.type_size(:int32))"),
            "boxed scalar freed: {code}"
        );
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
                    doc: None,
                }],
                returns: Some(TypeRef::Record("Item".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
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

        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(code.contains("Item.new(result)"), "struct wrap: {code}");
        assert!(
            code.contains("raise Error.new(-1, 'null pointer') if result.null?"),
            "null ptr: {code}"
        );
    }

    #[test]
    fn async_function_generates_blocking_wrapper() {
        let api = make_api(vec![simple_module(
            "io",
            vec![Function {
                name: "read".into(),
                params: vec![],
                returns: Some(TypeRef::StringUtf8),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
        // Completion callback type + launcher attach.
        assert!(
            code.contains(
                "callback :weaveffi_io_read_callback, [:pointer, :pointer, :pointer], :void"
            ),
            "async callback decl: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_io_read_async, [:weaveffi_io_read_callback, :pointer], :void"
            ),
            "async launcher attach: {code}"
        );
        // Blocking wrapper: trampoline pinned in a local, Queue rendezvous,
        // error re-raised on the caller thread.
        assert!(code.contains("def self.read()"), "wrapper: {code}");
        assert!(code.contains("queue = Queue.new"), "queue: {code}");
        assert!(
            code.contains("callback = FFI::Function.new(:void, [:pointer, :pointer, :pointer])"),
            "trampoline: {code}"
        );
        assert!(
            code.contains("weaveffi_io_read_async(callback, FFI::Pointer::NULL)"),
            "launch call: {code}"
        );
        assert!(code.contains("value = queue.pop"), "blocking pop: {code}");
        assert!(
            code.contains("raise value if value.is_a?(Error)"),
            "error re-raise: {code}"
        );
        // The generated doc states plainly that the call blocks.
        assert!(
            code.contains("# Blocks the current thread until the async producer completes"),
            "blocking doc: {code}"
        );
        // The completion callback copies the borrowed result buffer and must
        // not free it: the producer owns callback result buffers.
        assert!(
            code.contains("result.read_string"),
            "result copied in callback: {code}"
        );
        assert!(
            !code.contains("weaveffi_free_string(result)"),
            "borrowed callback buffer must not be freed: {code}"
        );
    }

    #[test]
    fn async_bytes_result_copied_not_freed() {
        let api = make_api(vec![simple_module(
            "io",
            vec![Function {
                name: "fetch".into(),
                params: vec![],
                returns: Some(TypeRef::Bytes),
                doc: None,
                throws: false,
                r#async: true,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(
            code.contains("result.read_string(result_len)"),
            "bytes copied in callback: {code}"
        );
        assert!(
            !code.contains("weaveffi_free_bytes(result, result_len)"),
            "borrowed callback bytes must not be freed: {code}"
        );
    }

    #[test]
    fn iterator_uses_next_destroy_protocol() {
        let api = make_api(vec![simple_module(
            "events",
            vec![Function {
                name: "get_messages".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        // Launch returns the opaque iterator; next/destroy attached.
        assert!(
            code.contains("attach_function :weaveffi_events_get_messages, [:pointer], :pointer"),
            "launch attach: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_events_GetMessagesIterator_next, [:pointer, :pointer, :pointer], :int32"
            ),
            "next attach: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_events_GetMessagesIterator_destroy, [:pointer], :void"
            ),
            "destroy attach: {code}"
        );
        // The wrapper pulls via the iterator protocol, not the list ABI
        // (the old lowering wrongly passed an out_len the symbol lacks).
        assert!(
            code.contains(
                "has_item = weaveffi_events_GetMessagesIterator_next(iter, out_item, item_err)"
            ),
            "pull loop: {code}"
        );
        assert!(
            code.contains("weaveffi_events_GetMessagesIterator_destroy(iter) unless iter.null?"),
            "destroy on disposal: {code}"
        );
        assert!(!code.contains("out_len"), "no stray out_len: {code}");
    }

    #[test]
    fn iterator_returns_lazy_enumerator_with_ensured_destroy() {
        let api = make_api(vec![simple_module(
            "events",
            vec![Function {
                name: "get_messages".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        let body = code
            .split("def self.get_messages()")
            .nth(1)
            .expect("wrapper body");
        let body = body.split("\n  end\n").next().expect("wrapper body end");
        // Lazy Enumerator, never a hidden drain into an Array.
        assert!(
            body.contains("Enumerator.new do |y|"),
            "lazy Enumerator: {code}"
        );
        assert!(!body.contains("items = []"), "no eager drain: {code}");
        assert!(!body.contains(".to_a"), "no hidden collect: {code}");
        // The launch happens inside the block, so an unstarted enumerator
        // never acquires (and thus can never leak) a handle.
        let launch = body
            .find("iter = weaveffi_events_get_messages(err)")
            .expect("launch");
        let enum_open = body.find("Enumerator.new do |y|").expect("enumerator");
        assert!(enum_open < launch, "launch inside enumerator block: {code}");
        // Destroy runs from an ensure block, guarding early break, and each
        // yielded string is freed after copying.
        let ensure_pos = body.find("ensure").expect("ensure block");
        let destroy_pos = body
            .find("weaveffi_events_GetMessagesIterator_destroy(iter)")
            .expect("destroy call");
        assert!(ensure_pos < destroy_pos, "destroy inside ensure: {code}");
        assert!(
            body.contains("weaveffi_free_string(item_ptr)"),
            "yielded string freed after copy: {code}"
        );
        assert!(body.contains("y << item"), "yields through yielder: {code}");
        // The generated docs describe the lazy contract.
        assert!(
            code.contains("# Returns a lazy Enumerator"),
            "doc states Enumerator return: {code}"
        );
    }

    #[test]
    fn iterator_of_records_adopts_each_element() {
        let api = make_api(vec![Module {
            name: "kv".into(),
            functions: vec![Function {
                name: "scan_entries".into(),
                params: vec![],
                returns: Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Entry".into(),
                doc: None,
                builder: false,
                fields: vec![StructField {
                    name: "key".into(),
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
        let code = render(&api, "WeaveFFI", "weaveffi");
        // Each yielded record pointer is adopted by the wrapper class, whose
        // AutoPointer finalizer owns the destroy.
        assert!(
            code.contains("y << Entry.new(item_ptr) unless item_ptr.null?"),
            "record element adopted: {code}"
        );
        assert!(
            code.contains("Enumerator.new do |y|"),
            "record iterator is lazy: {code}"
        );
    }

    #[test]
    fn interface_iterator_method_is_lazy_and_qualified() {
        let mut m = simple_module("kv", vec![]);
        m.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![plain_fn("new", vec![], None)],
            methods: vec![plain_fn(
                "keys",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            )],
            statics: vec![],
        }];
        let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
        let body = code.split("def keys()").nth(1).expect("keys wrapper");
        assert!(
            body.contains("Enumerator.new do |y|"),
            "method iterator is lazy: {code}"
        );
        assert!(
            body.contains("iter = WeaveFFI.weaveffi_kv_Store_keys(@handle, err)"),
            "launch passes self and qualifies: {code}"
        );
        assert!(
            body.contains(
                "WeaveFFI.weaveffi_kv_Store_KeysIterator_destroy(iter) unless iter.null?"
            ),
            "qualified ensure destroy: {code}"
        );
    }

    #[test]
    fn listener_register_unregister_wrappers() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            callbacks: vec![CallbackDef {
                name: "OnMessage".into(),
                params: vec![Param {
                    name: "message".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            ..simple_module("events", vec![])
        }]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(
            code.contains("callback :weaveffi_events_OnMessage_fn, [:string, :pointer], :void"),
            "callback decl: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_events_register_message_listener, [:weaveffi_events_OnMessage_fn, :pointer], :uint64"
            ),
            "register attach: {code}"
        );
        assert!(
            code.contains("def self.register_message_listener(&block)"),
            "register wrapper: {code}"
        );
        assert!(
            code.contains("@listener_refs[listener_id] = trampoline"),
            "trampoline pinned: {code}"
        );
        assert!(
            code.contains("def self.unregister_message_listener(listener_id)"),
            "unregister wrapper: {code}"
        );
        assert!(
            code.contains("@listener_refs.delete(listener_id)"),
            "trampoline released: {code}"
        );
    }

    #[test]
    fn preamble_has_platform_detection() {
        let code = render(&make_api(vec![]), "WeaveFFI", "weaveffi");
        assert!(code.contains("FFI::Platform::OS"), "platform: {code}");
        assert!(code.contains("libweaveffi.dylib"), "darwin: {code}");
        assert!(code.contains("weaveffi.dll"), "windows: {code}");
        assert!(code.contains("libweaveffi.so"), "linux: {code}");
    }

    #[test]
    fn error_class_structure() {
        let code = render(&make_api(vec![]), "WeaveFFI", "weaveffi");
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
        assert!(code.contains(":uint64"), "handle type: {code}");
    }

    #[test]
    fn ffi_type_mapping() {
        let types = |ty: &TypeRef| rb_abi_types(&abi::lower_param("_", ty, "", false), false);
        assert_eq!(types(&TypeRef::I32), vec![":int32"]);
        assert_eq!(types(&TypeRef::U32), vec![":uint32"]);
        assert_eq!(types(&TypeRef::I64), vec![":int64"]);
        assert_eq!(types(&TypeRef::F64), vec![":double"]);
        assert_eq!(types(&TypeRef::Bool), vec![":int32"]);
        assert_eq!(types(&TypeRef::Handle), vec![":uint64"]);
        assert_eq!(types(&TypeRef::StringUtf8), vec![":string"]);
        assert_eq!(types(&TypeRef::Enum("Color".into())), vec![":int32"]);
        assert_eq!(types(&TypeRef::Record("Foo".into())), vec![":pointer"]);
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
                    doc: None,
                }],
                returns: None,
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                returns: Some(TypeRef::List(Box::new(TypeRef::Record("Item".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
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

        let code = render(&api, "WeaveFFI", "weaveffi");
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
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record("Item".into())))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "weaveffi");
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
            version: "0.5.0".into(),
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
                        throws: false,
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
                        returns: Some(TypeRef::Record("Contact".into())),
                        doc: None,
                        throws: false,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                    Function {
                        name: "list_contacts".into(),
                        params: vec![],
                        returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                        doc: None,
                        throws: false,
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
                        throws: false,
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
                        throws: false,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                        since: None,
                    },
                ],
                interfaces: vec![],
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
            }],
            generators: None,
            package: None,
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate(&api, out_dir, &RubyConfig::default())
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
                    doc: None,
                }],
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
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
            .generate(&api, out_dir, &RubyConfig::default())
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
                    doc: None,
                }],
                returns: Some(TypeRef::Enum("ContactType".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
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

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        RubyGenerator
            .generate(&api, out_dir, &RubyConfig::default())
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
                        doc: None,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
                    doc: None,
                    throws: false,
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
                        doc: None,
                    }],
                    returns: Some(TypeRef::Optional(Box::new(TypeRef::I32))),
                    doc: None,
                    throws: false,
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
            .generate(&api, out_dir, &RubyConfig::default())
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
                    throws: false,
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
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    throws: false,
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
            .generate(&api, out_dir, &RubyConfig::default())
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
            .generate(&api, out_dir, &RubyConfig::default())
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let tmp = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(tmp.path()).expect("valid UTF-8");

        let config = RubyConfig {
            module_name: Some("MyBindings".into()),
            gem_name: Some("my_bindings".into()),
            ..RubyConfig::default()
        };
        RubyGenerator.generate(&api, out_dir, &config).unwrap();

        let rb = std::fs::read_to_string(tmp.path().join("ruby/lib/my_bindings.rb")).unwrap();
        assert!(rb.contains("module MyBindings"), "custom module name: {rb}");
        assert!(
            !rb.contains("module WeaveFFI"),
            "should not contain default module name: {rb}"
        );

        let gemspec = std::fs::read_to_string(tmp.path().join("ruby/my_bindings.gemspec")).unwrap();
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
            interfaces: vec![],
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
                returns: Some(TypeRef::Record("Contact".into())),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            errors: None,
            modules: vec![],
        }]);

        let rb = render(&api, "WeaveFFI", "weaveffi");

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
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                doc: None,
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);

        let rb = render(&api, "WeaveFFI", "weaveffi");

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

    fn doc_api() -> Api {
        make_api(vec![Module {
            name: "docs".into(),
            functions: vec![Function {
                name: "do_thing".into(),
                params: vec![Param {
                    name: "x".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: Some("the input value".into()),
                }],
                returns: Some(TypeRef::I32),
                doc: Some("Performs a thing.".into()),
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
            interfaces: vec![],
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
    fn ruby_emits_doc_on_function() {
        let rb = render(&doc_api(), "Weaveffi", "weaveffi");
        assert!(rb.contains("# Performs a thing."), "{rb}");
    }

    #[test]
    fn ruby_emits_doc_on_struct() {
        let rb = render(&doc_api(), "Weaveffi", "weaveffi");
        assert!(rb.contains("# An item we track."), "{rb}");
    }

    #[test]
    fn ruby_emits_doc_on_enum_variant() {
        let rb = render(&doc_api(), "Weaveffi", "weaveffi");
        assert!(rb.contains("# Kind of item."), "{rb}");
        assert!(rb.contains("# A small one"), "{rb}");
    }

    #[test]
    fn ruby_emits_doc_on_field() {
        let rb = render(&doc_api(), "Weaveffi", "weaveffi");
        assert!(rb.contains("# Stable id"), "{rb}");
    }

    #[test]
    fn ruby_emits_doc_on_param() {
        let rb = render(&doc_api(), "Weaveffi", "weaveffi");
        assert!(rb.contains("# @param x [Object] the input value"), "{rb}");
    }

    #[test]
    fn ruby_custom_prefix_threads_to_user_symbols() {
        let api = make_api(vec![simple_module(
            "math",
            vec![Function {
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
                throws: false,
                r#async: false,
                cancellable: false,
                deprecated: None,
                since: None,
            }],
        )]);

        let code = render(&api, "WeaveFFI", "myffi");

        assert!(
            code.contains("attach_function :myffi_math_add"),
            "user symbol should adopt custom prefix: {code}"
        );
        assert!(
            !code.contains("weaveffi_math_add"),
            "user symbol must not retain default prefix: {code}"
        );
        assert!(
            code.contains("weaveffi_error_clear"),
            "runtime ABI helper must stay literal: {code}"
        );
    }

    fn shapes_api() -> Api {
        make_api(vec![Module {
            name: "shapes".into(),
            functions: vec![
                Function {
                    name: "describe".into(),
                    params: vec![Param {
                        name: "shape".into(),
                        ty: TypeRef::RichEnum("Shape".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::StringUtf8),
                    doc: None,
                    throws: false,
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
                            ty: TypeRef::RichEnum("Shape".into()),
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
                    returns: Some(TypeRef::RichEnum("Shape".into())),
                    doc: None,
                    throws: false,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                },
            ],
            interfaces: vec![],
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
                                doc: None,
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
    fn rich_enum_renders_opaque_wrapper_class() {
        let code = render(&shapes_api(), "Shapes", "weaveffi");

        // A rich enum is an opaque-object class, never a plain constants module.
        assert!(
            !code.contains("module Shape\n"),
            "rich enum must not be a plain enum module: {code}"
        );
        assert!(code.contains("class Shape\n"), "rich enum class: {code}");

        // AutoPointer ownership + struct-compatible surface.
        assert!(
            code.contains("class ShapePtr < FFI::AutoPointer"),
            "AutoPointer: {code}"
        );
        assert!(
            code.contains("Shapes.weaveffi_shapes_Shape_destroy(ptr)"),
            "release via destroy: {code}"
        );
        assert!(code.contains("attr_reader :handle"), "handle attr: {code}");
        assert!(
            code.contains("@handle = ShapePtr.new(handle)"),
            "init wraps handle: {code}"
        );

        // Tag constants + reader.
        assert!(code.contains("EMPTY = 0"), "tag const EMPTY: {code}");
        assert!(code.contains("CIRCLE = 1"), "tag const CIRCLE: {code}");
        assert!(code.contains("LABELED = 3"), "tag const LABELED: {code}");
        assert!(
            code.contains("def tag\n      Shapes.weaveffi_shapes_Shape_tag(@handle)"),
            "tag reader: {code}"
        );

        // Plain sibling enum still renders as a constants module.
        assert!(
            code.contains("module Channel"),
            "plain enum still a module: {code}"
        );
    }

    #[test]
    fn rich_enum_factories_and_getters() {
        let code = render(&shapes_api(), "Shapes", "weaveffi");

        // FFI bindings for tag, destroy, constructors, and field getters.
        assert!(
            code.contains("attach_function :weaveffi_shapes_Shape_tag, [:pointer], :int32"),
            "tag attach: {code}"
        );
        assert!(
            code.contains("attach_function :weaveffi_shapes_Shape_Empty_new, [:pointer], :pointer"),
            "unit ctor attach (out_err only): {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_shapes_Shape_Circle_new, [:double, :pointer], :pointer"
            ),
            "circle ctor attach: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_shapes_Shape_Rectangle_new, [:float, :float, :pointer], :pointer"
            ),
            "rectangle ctor attach: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_shapes_Shape_Labeled_new, [:string, :uint8, :pointer], :pointer"
            ),
            "labeled ctor attach: {code}"
        );
        assert!(
            code.contains(
                "attach_function :weaveffi_shapes_Shape_Labeled_get_label, [:pointer], :pointer"
            ),
            "string getter attach: {code}"
        );

        // Idiomatic factory class methods.
        assert!(code.contains("def self.empty\n"), "empty factory: {code}");
        assert!(
            code.contains("def self.circle(radius)"),
            "circle factory: {code}"
        );
        assert!(
            code.contains("def self.rectangle(width, height)"),
            "rectangle factory: {code}"
        );
        assert!(
            code.contains("def self.labeled(label, count)"),
            "labeled factory: {code}"
        );
        assert!(
            code.contains("result = Shapes.weaveffi_shapes_Shape_Circle_new(radius, err)"),
            "circle ctor call: {code}"
        );
        assert!(
            code.contains("Shapes.check_error!(err)"),
            "factory checks error: {code}"
        );
        assert!(code.contains("new(result)"), "factory wraps handle: {code}");

        // Variant-namespaced getters; string getter still frees the owned C string.
        assert!(code.contains("def circle_radius"), "circle_radius: {code}");
        assert!(
            code.contains("def rectangle_width") && code.contains("def rectangle_height"),
            "rectangle getters: {code}"
        );
        assert!(
            code.contains("def labeled_label") && code.contains("def labeled_count"),
            "labeled getters: {code}"
        );
        assert!(
            code.contains("Shapes.weaveffi_free_string(result)"),
            "string getter frees: {code}"
        );

        // Functions taking/returning the rich enum reuse the struct path.
        assert!(
            code.contains("def self.describe(shape)") && code.contains("shape.handle"),
            "describe passes handle: {code}"
        );
        assert!(
            code.contains("Shape.new(result)"),
            "scale wraps returned Shape: {code}"
        );
    }
}
