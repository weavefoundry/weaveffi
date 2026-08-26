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
use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    AsyncBinding, BindingModel, CallShape, CallbackBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding, StructBinding,
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

/// The Ruby argument expressions one wrapper parameter contributes to the C
/// call, mirroring [`abi::lower_param`]'s slot expansion. A buffered
/// parameter contributes its packed `(ptr, len)` pair, bytes contribute a
/// copied native buffer plus its length, and everything else is a single
/// expression.
fn rb_call_args(name: &str, ty: &TypeRef) -> Vec<String> {
    if abi::is_buffered(ty) {
        return vec![format!("{name}_buf"), format!("{name}_data.bytesize")];
    }
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
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => {
            vec![format!("{name}.handle")]
        }
        // Only `Interface?` is direct (a nullable borrowed object pointer);
        // every other optional is buffered and handled above.
        TypeRef::Optional(_) => vec![format!("{name}&.handle")],
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

// ── Value-buffer codegen ──

/// The snake_case stem naming a record's or rich enum's generated pack and
/// unpack helpers: `Contact` (or `other.Contact`) becomes `contact`, naming
/// `_wv_write_contact` and `_wv_read_contact`.
fn wv_stem(name: &str) -> String {
    local_type_name(name).to_snake_case()
}

/// The `WvBufferWriter` method encoding one scalar wire type, or `None` for
/// composite types that need statement-level rendering. C-style enums encode
/// as their `i32` discriminant; handles (typed or not) as raw `u64` values.
fn wv_scalar_writer(ty: &TypeRef) -> Option<&'static str> {
    Some(match ty {
        TypeRef::Bool => "write_bool",
        TypeRef::I8 => "write_i8",
        TypeRef::U8 => "write_u8",
        TypeRef::I16 => "write_i16",
        TypeRef::U16 => "write_u16",
        TypeRef::I32 | TypeRef::Enum(_) => "write_i32",
        TypeRef::U32 => "write_u32",
        TypeRef::I64 => "write_i64",
        TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => "write_u64",
        TypeRef::F32 => "write_f32",
        TypeRef::F64 => "write_f64",
        TypeRef::StringUtf8 => "write_string",
        TypeRef::Bytes => "write_bytes",
        _ => return None,
    })
}

/// The `WvBufferReader` method decoding one scalar wire type, mirroring
/// [`wv_scalar_writer`].
fn wv_scalar_reader(ty: &TypeRef) -> Option<&'static str> {
    Some(match ty {
        TypeRef::Bool => "read_bool",
        TypeRef::I8 => "read_i8",
        TypeRef::U8 => "read_u8",
        TypeRef::I16 => "read_i16",
        TypeRef::U16 => "read_u16",
        TypeRef::I32 | TypeRef::Enum(_) => "read_i32",
        TypeRef::U32 => "read_u32",
        TypeRef::I64 => "read_i64",
        TypeRef::U64 | TypeRef::Handle | TypeRef::TypedHandle(_) => "read_u64",
        TypeRef::F32 => "read_f32",
        TypeRef::F64 => "read_f64",
        TypeRef::StringUtf8 => "read_string",
        TypeRef::Bytes => "read_bytes",
        _ => return None,
    })
}

/// Emit the Ruby statements appending `expr` (a value of IR type `ty`) to
/// the buffer writer named `wvar`, following the value-buffer wire format.
/// `q` is the dotted receiver (`"WeaveFFI."` or `""`) qualifying
/// module-singleton codec calls inside class bodies.
fn render_wv_write(
    w: &mut CodeWriter,
    wvar: &str,
    expr: &str,
    ty: &TypeRef,
    depth: usize,
    q: &str,
) {
    if let Some(m) = wv_scalar_writer(ty) {
        w.line(format!("{wvar}.{m}({expr})"));
        return;
    }
    match ty {
        TypeRef::Optional(inner) => {
            w.line(format!("if {expr}.nil?"));
            w.scope(|w| {
                w.line(format!("{wvar}.write_flag(false)"));
            });
            w.line("else");
            w.scope(|w| {
                w.line(format!("{wvar}.write_flag(true)"));
                render_wv_write(w, wvar, expr, inner, depth, q);
            });
            w.line("end");
        }
        TypeRef::List(elem) => {
            let e = format!("_wv_e{depth}");
            w.line(format!("{wvar}.write_len({expr}.length)"));
            w.block(format!("{expr}.each do |{e}|"), "end", |w| {
                render_wv_write(w, wvar, &e, elem, depth + 1, q);
            });
        }
        TypeRef::Map(k, v) => {
            let kn = format!("_wv_k{depth}");
            let vn = format!("_wv_v{depth}");
            w.line(format!("{wvar}.write_len({expr}.length)"));
            w.block(format!("{expr}.each do |{kn}, {vn}|"), "end", |w| {
                render_wv_write(w, wvar, &kn, k, depth + 1, q);
                render_wv_write(w, wvar, &vn, v, depth + 1, q);
            });
        }
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            w.line(format!("{q}_wv_write_{}({wvar}, {expr})", wv_stem(n)));
        }
        TypeRef::BorrowedStr | TypeRef::BorrowedBytes => {
            unreachable!("borrowed views rejected in buffered positions")
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("objects rejected in buffered positions")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => unreachable!("scalar handled above"),
    }
}

/// Emit the Ruby statements decoding one `ty` value from the buffer reader
/// named `rvar` into the local `var`. `q` is the dotted receiver qualifying
/// module-singleton codec calls inside class bodies.
fn render_wv_read(w: &mut CodeWriter, rvar: &str, var: &str, ty: &TypeRef, depth: usize, q: &str) {
    if let Some(m) = wv_scalar_reader(ty) {
        w.line(format!("{var} = {rvar}.{m}"));
        return;
    }
    match ty {
        TypeRef::Optional(inner) => {
            w.line(format!("if {rvar}.read_flag"));
            w.scope(|w| {
                render_wv_read(w, rvar, var, inner, depth, q);
            });
            w.line("else");
            w.scope(|w| {
                w.line(format!("{var} = nil"));
            });
            w.line("end");
        }
        TypeRef::List(elem) => {
            let e = format!("_wv_e{depth}");
            w.block(
                format!("{var} = Array.new({rvar}.read_len) do"),
                "end",
                |w| {
                    render_wv_read(w, rvar, &e, elem, depth + 1, q);
                    w.line(e.clone());
                },
            );
        }
        TypeRef::Map(k, v) => {
            let kn = format!("_wv_k{depth}");
            let vn = format!("_wv_v{depth}");
            w.line(format!("{var} = {{}}"));
            w.block(format!("{rvar}.read_len.times do"), "end", |w| {
                render_wv_read(w, rvar, &kn, k, depth + 1, q);
                render_wv_read(w, rvar, &vn, v, depth + 1, q);
                w.line(format!("{var}[{kn}] = {vn}"));
            });
        }
        TypeRef::Record(n) | TypeRef::RichEnum(n) => {
            w.line(format!("{var} = {q}_wv_read_{}({rvar})", wv_stem(n)));
        }
        TypeRef::BorrowedStr | TypeRef::BorrowedBytes => {
            unreachable!("borrowed views rejected in buffered positions")
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("objects rejected in buffered positions")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
        _ => unreachable!("scalar handled above"),
    }
}

/// Render the private pack/unpack pair for one record: module singleton
/// methods `_wv_write_{stem}(w, v)` and `_wv_read_{stem}(r)` serializing the
/// fields in declaration (wire) order.
fn render_struct_codec(out: &mut String, s: &StructBinding) {
    let stem = wv_stem(&s.name);
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Packs a {} into the value-buffer wire format.",
        s.name
    ));
    w.block(format!("def self._wv_write_{stem}(w, v)"), "end", |w| {
        for f in &s.fields {
            render_wv_write(w, "w", &format!("v.{}", f.name), &f.ty, 0, "");
        }
    });
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Unpacks a {} from the value-buffer wire format.",
        s.name
    ));
    w.block(format!("def self._wv_read_{stem}(r)"), "end", |w| {
        for f in &s.fields {
            render_wv_read(w, "r", &format!("_wv_{}", f.name), &f.ty, 0, "");
        }
        let kwargs = s
            .fields
            .iter()
            .map(|f| format!("{}: _wv_{}", f.name, f.name))
            .collect::<Vec<_>>()
            .join(", ");
        if kwargs.is_empty() {
            w.line(format!("{}.new", s.name));
        } else {
            w.line(format!("{}.new({kwargs})", s.name));
        }
    });
    out.push_str(&w.finish());
}

/// Render the private pack/unpack pair for one rich enum: `_wv_write_{stem}`
/// dispatches on the variant class and writes the `i32` tag followed by the
/// variant's fields; `_wv_read_{stem}` switches on the decoded tag.
fn render_rich_enum_codec(out: &mut String, e: &EnumBinding) {
    let stem = wv_stem(&e.name);
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Packs a {} into the value-buffer wire format.",
        e.name
    ));
    w.block(format!("def self._wv_write_{stem}(w, v)"), "end", |w| {
        w.line("case v");
        for v in &e.variants {
            w.line(format!("when {}::{}", e.name, v.name));
            w.scope(|w| {
                w.line(format!("w.write_i32({})", v.value));
                for f in &v.fields {
                    render_wv_write(w, "w", &format!("v.{}", f.name), &f.ty, 0, "");
                }
            });
        }
        w.line("else");
        w.scope(|w| {
            w.line(format!("raise Error.new(-1, 'unknown {} variant')", e.name));
        });
        w.line("end");
    });
    w.blank();
    w.line("# @api private");
    w.line(format!(
        "# Unpacks a {} from the value-buffer wire format.",
        e.name
    ));
    w.block(format!("def self._wv_read_{stem}(r)"), "end", |w| {
        w.line("tag = r.read_i32");
        w.line("case tag");
        for v in &e.variants {
            w.line(format!("when {}", v.value));
            w.scope(|w| {
                for f in &v.fields {
                    render_wv_read(w, "r", &format!("_wv_{}", f.name), &f.ty, 0, "");
                }
                let kwargs = v
                    .fields
                    .iter()
                    .map(|f| format!("{}: _wv_{}", f.name, f.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                if kwargs.is_empty() {
                    w.line(format!("{}::{}.new", e.name, v.name));
                } else {
                    w.line(format!("{}::{}.new({kwargs})", e.name, v.name));
                }
            });
        }
        w.line("else");
        w.scope(|w| {
            w.line(format!(
                "raise Error.new(-1, \"malformed value buffer: unknown {} tag #{{tag}}\")",
                e.name
            ));
        });
        w.line("end");
    });
    out.push_str(&w.finish());
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
/// constant, default message, and any declared payload fields as attributes,
/// the code-to-class table, and the factory/checker helpers throwing wrappers
/// route their out-err slots through. Nesting the code classes keeps
/// `KvError::KeyNotFound` spellable and unambiguous even across domains.
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
                if !c.fields.is_empty() {
                    w.blank();
                    for f in &c.fields {
                        let mut fd = String::new();
                        emit_doc(&mut fd, &f.doc, "      ");
                        w.raw(fd);
                        w.line(format!("attr_reader :{}", f.name));
                    }
                }
                w.blank();
                let kw: String = c
                    .fields
                    .iter()
                    .map(|f| format!(", {}: nil", f.name))
                    .collect();
                w.block(
                    format!(
                        "def initialize(message = '{}'{kw})",
                        rb_str_literal(&c.message)
                    ),
                    "end",
                    |w| {
                        for f in &c.fields {
                            w.line(format!("@{} = {}", f.name, f.name));
                        }
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
        "# Builds the {domain} subclass matching `code`, decoding any payload"
    ));
    w.line("# fields the code declares, or a generic Error for codes outside");
    w.line("# the domain (panics, marshalling).");
    w.block(
        format!("def self.{factory}(code, message, payload = nil)"),
        "end",
        |w| {
            if eb.codes.iter().any(|c| !c.fields.is_empty()) {
                w.line("case code");
                for c in eb.codes.iter().filter(|c| !c.fields.is_empty()) {
                    let class = weaveffi_core::errors::pascal(&c.name);
                    w.line(format!("when {}", c.value));
                    w.scope(|w| {
                        w.line("r = WvBufferReader.new(payload || ''.b)");
                        for f in &c.fields {
                            render_wv_read(w, "r", &format!("_wv_{}", f.name), &f.ty, 0, "");
                        }
                        w.line("r.expect_end!");
                        let kwargs = c
                            .fields
                            .iter()
                            .map(|f| format!("{}: _wv_{}", f.name, f.name))
                            .collect::<Vec<_>>()
                            .join(", ");
                        w.line(format!(
                            "return {domain}::{class}.new({kwargs}) if message.empty?"
                        ));
                        w.line(format!("return {domain}::{class}.new(message, {kwargs})"));
                    });
                }
                w.line("end");
            }
            w.line(format!("cls = {table}[code]"));
            w.line("return Error.new(code, message) if cls.nil?");
            w.line("message.empty? ? cls.new : cls.new(message)");
        },
    );

    w.blank();
    w.line(format!(
        "# Raises the typed {domain} for a non-zero error slot."
    ));
    w.block(format!("def self.{checker}(err)"), "end", |w| {
        w.line("return if err[:code].zero?");
        w.line("code = err[:code]");
        w.line("msg_ptr = err[:message]");
        w.line("msg = msg_ptr.null? ? '' : msg_ptr.read_string");
        w.line("payload_ptr = err[:payload_ptr]");
        w.line("payload = payload_ptr.null? ? nil : payload_ptr.read_string(err[:payload_len])");
        w.line("weaveffi_error_clear(err.to_ptr)");
        w.line(format!("raise {factory}(code, msg, payload)"));
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
            // (algebraic) enum is a tagged value-class hierarchy packed into
            // value buffers by the codec helpers below.
            if e.is_rich() {
                render_rich_enum_class(&mut out, e);
            } else {
                render_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, s);
        }
        // Value-buffer codecs: one pack/unpack pair per record and rich enum.
        for s in &m.structs {
            render_struct_codec(&mut out, s);
        }
        for e in &m.enums {
            if e.is_rich() {
                render_rich_enum_codec(&mut out, e);
            }
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
           :message, :pointer,
           :payload_ptr, :pointer,
           :payload_len, :size_t
  end

  class Error < StandardError
    attr_reader :code

    def initialize(code, message)
      @code = code
      super(message)
    end
  end
"
    ));
    out.push_str(RUBY_BUFFER_RUNTIME);
    out.push_str(
        "
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
",
    );
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

/// The private Ruby runtime implementing the value-buffer wire format
/// (little-endian, packed, no alignment): a writer building a binary String
/// and a reader that raises `Error` on any malformed buffer (truncation, bad
/// flag bytes, invalid UTF-8, length prefixes past the end, trailing bytes).
const RUBY_BUFFER_RUNTIME: &str = r#"
  # @api private
  # Appends values in the WeaveFFI value-buffer wire format: little-endian,
  # packed, no alignment.
  class WvBufferWriter
    def initialize
      @buf = +''.b
    end

    # The encoded bytes as a binary String.
    def data
      @buf
    end

    def write_bool(v)
      @buf << (v ? "\x01".b : "\x00".b)
    end

    def write_flag(v)
      write_bool(v)
    end

    def write_i8(v)
      @buf << [v].pack('c')
    end

    def write_u8(v)
      @buf << [v].pack('C')
    end

    def write_i16(v)
      @buf << [v].pack('s<')
    end

    def write_u16(v)
      @buf << [v].pack('S<')
    end

    def write_i32(v)
      @buf << [v].pack('l<')
    end

    def write_u32(v)
      @buf << [v].pack('L<')
    end

    def write_len(v)
      write_u32(v)
    end

    def write_i64(v)
      @buf << [v].pack('q<')
    end

    def write_u64(v)
      @buf << [v].pack('Q<')
    end

    def write_f32(v)
      @buf << [v].pack('e')
    end

    def write_f64(v)
      @buf << [v].pack('E')
    end

    def write_string(v)
      b = v.to_s.encode(Encoding::UTF_8).b
      write_u32(b.bytesize)
      @buf << b
    end

    def write_bytes(v)
      b = v.to_s.b
      write_u32(b.bytesize)
      @buf << b
    end
  end

  # @api private
  # Reads values in the WeaveFFI value-buffer wire format, raising Error on
  # any malformed buffer.
  class WvBufferReader
    def initialize(data)
      @data = data.to_s.b
      @pos = 0
    end

    def take(n, what)
      raise Error.new(-1, "malformed value buffer: #{what}") if @pos + n > @data.bytesize
      s = @data.byteslice(@pos, n)
      @pos += n
      s
    end

    def read_bool
      b = take(1, 'bool').unpack1('C')
      raise Error.new(-1, 'malformed value buffer: bool byte out of range') if b > 1
      b == 1
    end

    def read_flag
      b = take(1, 'option flag').unpack1('C')
      raise Error.new(-1, 'malformed value buffer: option flag out of range') if b > 1
      b == 1
    end

    def read_i8
      take(1, 'i8').unpack1('c')
    end

    def read_u8
      take(1, 'u8').unpack1('C')
    end

    def read_i16
      take(2, 'i16').unpack1('s<')
    end

    def read_u16
      take(2, 'u16').unpack1('S<')
    end

    def read_i32
      take(4, 'i32').unpack1('l<')
    end

    def read_u32
      take(4, 'u32').unpack1('L<')
    end

    def read_len
      len = read_u32
      if len > @data.bytesize - @pos
        raise Error.new(-1, 'malformed value buffer: length prefix exceeds remaining bytes')
      end
      len
    end

    def read_i64
      take(8, 'i64').unpack1('q<')
    end

    def read_u64
      take(8, 'u64').unpack1('Q<')
    end

    def read_f32
      take(4, 'f32').unpack1('e')
    end

    def read_f64
      take(8, 'f64').unpack1('E')
    end

    def read_string
      s = take(read_len, 'string bytes').force_encoding(Encoding::UTF_8)
      raise Error.new(-1, 'malformed value buffer: string is not valid UTF-8') unless s.valid_encoding?
      s
    end

    def read_bytes
      take(read_len, 'byte buffer')
    end

    def expect_end!
      return if @pos == @data.bytesize
      raise Error.new(-1, 'malformed value buffer: trailing bytes after value')
    end
  end
"#;

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

/// Render one record as a plain Ruby value class: one documented
/// `attr_reader` per field, a keyword-argument `initialize`, and structural
/// `==`. Records are value types: they own no C symbols, no destroy, and no
/// builders; they cross the ABI packed into value buffers by the module's
/// `_wv_write_*`/`_wv_read_*` codec helpers.
fn render_struct_class(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &s.doc, "  ");
    w.raw(d);
    w.line(format!("class {}", s.name));
    w.scope(|w| {
        for (idx, f) in s.fields.iter().enumerate() {
            if idx > 0 {
                w.blank();
            }
            let mut fd = String::new();
            emit_doc(&mut fd, &f.doc, "    ");
            w.raw(fd);
            w.line(format!("attr_reader :{}", f.name));
        }
        w.blank();
        let kw = s
            .fields
            .iter()
            .map(|f| format!("{}:", f.name))
            .collect::<Vec<_>>()
            .join(", ");
        let open = if kw.is_empty() {
            "def initialize".to_string()
        } else {
            format!("def initialize({kw})")
        };
        w.block(open, "end", |w| {
            for f in &s.fields {
                w.line(format!("@{} = {}", f.name, f.name));
            }
        });
        w.blank();
        w.line("# Structural equality over every field.");
        w.block("def ==(other)", "end", |w| {
            w.line(format!("return false unless other.is_a?({})", s.name));
            for f in &s.fields {
                w.line(format!(
                    "return false unless {} == other.{}",
                    f.name, f.name
                ));
            }
            w.line("true");
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

/// Render one rich (algebraic) enum as an idiomatic tagged class hierarchy:
/// a base class exposing `tag`, plus one nested value-class subclass per
/// variant carrying that variant's fields (documented `attr_reader`s, a
/// keyword-argument `initialize`, structural `==`). Rich enums own no C
/// symbols; they cross the ABI packed into value buffers as an `i32` tag
/// followed by the active variant's fields in declaration order.
fn render_rich_enum_class(out: &mut String, e: &EnumBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &e.doc, "  ");
    w.raw(d);
    w.line(format!("class {}", e.name));
    w.scope(|w| {
        w.line("# The active variant's integer tag.");
        w.block("def tag", "end", |w| {
            w.line("self.class::TAG");
        });
        for v in &e.variants {
            w.blank();
            let mut vd = String::new();
            emit_doc(&mut vd, &v.doc, "    ");
            w.raw(vd);
            w.line(format!("class {} < {}", v.name, e.name));
            w.scope(|w| {
                w.line(format!("TAG = {}", v.value));
                if !v.fields.is_empty() {
                    for f in &v.fields {
                        w.blank();
                        let mut fd = String::new();
                        emit_doc(&mut fd, &f.doc, "      ");
                        w.raw(fd);
                        w.line(format!("attr_reader :{}", f.name));
                    }
                    w.blank();
                    let kw = v
                        .fields
                        .iter()
                        .map(|f| format!("{}:", f.name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    w.block(format!("def initialize({kw})"), "end", |w| {
                        for f in &v.fields {
                            w.line(format!("@{} = {}", f.name, f.name));
                        }
                    });
                }
                w.blank();
                w.line("# Structural equality over the variant and its fields.");
                w.block("def ==(other)", "end", |w| {
                    w.line(format!("return false unless other.is_a?({})", v.name));
                    for f in &v.fields {
                        w.line(format!(
                            "return false unless {} == other.{}",
                            f.name, f.name
                        ));
                    }
                    w.line("true");
                });
            });
            w.line("end");
        }
    });
    w.line("end");
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
    let buffered_params: Vec<_> = cb
        .params
        .iter()
        .filter(|p| abi::is_buffered(&p.ty))
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
                // Borrowed buffered arguments are only valid during the
                // dispatch: decode them before invoking the user's block. A
                // malformed buffer can't raise across the C boundary, so the
                // event is dropped with a warning instead.
                if !buffered_params.is_empty() {
                    w.line("begin");
                    w.scope(|w| {
                        for p in &buffered_params {
                            let n = p.name.to_snake_case();
                            w.line(format!(
                                "{n}_r = WvBufferReader.new({n}_ptr.null? ? ''.b : \
                                 {n}_ptr.read_string({n}_len))"
                            ));
                            render_wv_read(w, &format!("{n}_r"), &format!("{n}_v"), &p.ty, 0, "");
                            w.line(format!("{n}_r.expect_end!"));
                        }
                    });
                    w.line("rescue Error => e");
                    w.scope(|w| {
                        w.line(format!(
                            "warn \"weaveffi: dropped {} event: #{{e.message}}\"",
                            cb.name
                        ));
                        w.line("next");
                    });
                    w.line("end");
                }
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
/// derive from the parameter name, mirroring [`abi::lower_param`]. Buffered
/// parameters are decoded into a `{n}_v` local before the dispatch, so their
/// expression is just that local.
fn rb_cb_arg_expr(n: &str, ty: &TypeRef) -> String {
    if abi::is_buffered(ty) {
        return format!("{n}_v");
    }
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
        TypeRef::TypedHandle(_) | TypeRef::Interface(_) => n.into(),
        // Only `Interface?` reaches here; every other optional is buffered.
        TypeRef::Optional(_) => format!("({n}.null? ? nil : {n})"),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
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
            render_param_conversion(
                &mut pc,
                &p.name.to_snake_case(),
                &p.ty,
                &ind,
                scope.module_name(),
            );
            w.raw(pc);
        }

        let has_out_len = f
            .ret
            .as_ref()
            .is_some_and(|ty| !rb_return_out_params(ty).is_empty());

        if has_out_len {
            w.line("out_len = FFI::MemoryPointer.new(:size_t)");
        }

        let mut call_args: Vec<String> = Vec::new();
        if let Some(recv) = scope.self_arg() {
            call_args.push(recv.into());
        }
        for p in &f.params {
            call_args.extend(rb_call_args(&p.name.to_snake_case(), &p.ty));
        }
        if has_out_len {
            call_args.push("out_len".into());
        }
        call_args.push("err".into());

        let call = format!("{q}{c_sym}({})", call_args.join(", "));
        if f.ret.is_some() {
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
                    let mut tmp = String::new();
                    render_return_code(&mut tmp, ret_ty, &ind, scope.module_name());
                    w.raw(tmp);
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
    // callables; the generic Error otherwise (panics, marshalling). Typed
    // errors also copy the borrowed payload buffer so declared fields decode.
    let typed_error = matches!(
        (f.error_strategy(), module.error.as_ref()),
        (ErrorStrategy::Throws, Some(_))
    );
    let error_expr = match (f.error_strategy(), module.error.as_ref()) {
        (ErrorStrategy::Throws, Some(eb)) => {
            format!("{q}{}(code, msg, payload)", rb_error_factory_name(eb))
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
                    if typed_error {
                        // The payload buffer is borrowed for the callback's
                        // duration: copy it before clearing the slot.
                        w.line(
                            "payload = err[:payload_ptr].null? ? nil : \
                             err[:payload_ptr].read_string(err[:payload_len])",
                        );
                    }
                    w.line(format!("{q}weaveffi_error_clear(err_ptr)"));
                    w.line(format!("queue << {error_expr}"));
                });
                w.line("else");
                w.scope(|w| {
                    let mut tmp = String::new();
                    render_async_result_push(
                        &mut tmp,
                        &f.ret,
                        &format!("{ind}    "),
                        scope.module_name(),
                    );
                    w.raw(tmp);
                });
                w.line("end");
            },
        );

        for p in &f.params {
            let mut pc = String::new();
            render_param_conversion(
                &mut pc,
                &p.name.to_snake_case(),
                &p.ty,
                &ind,
                scope.module_name(),
            );
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
/// [`abi::callback_result_params`]: `result` (plus `result_len` for bytes),
/// or `result_ptr`/`result_len` for a buffered value.
///
/// Per the async completion contract ([`weaveffi_core::plan::AsyncProtocol`]),
/// string, bytes, and buffered result buffers are producer-owned and borrowed
/// for the callback's duration: the callback deep-copies or decodes them
/// before returning and never frees them. Owned interface results are the
/// exception: the callback receives ownership, so the pointer is adopted by
/// a finalizer-bearing wrapper.
fn render_async_result_push(
    out: &mut String,
    ret: &Option<TypeRef>,
    ind: &str,
    qualifier: Option<&str>,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    let Some(ty) = ret else {
        w.line("queue << nil");
        out.push_str(&w.finish());
        return;
    };
    if abi::is_buffered(ty) {
        // Borrowed buffer: decode inside the callback, never free. A decode
        // failure surfaces through the queue so the caller thread raises it.
        w.line("begin");
        w.scope(|w| {
            w.line(
                "_wv_r = WvBufferReader.new(result_ptr.null? ? ''.b : \
                 result_ptr.read_string(result_len))",
            );
            render_wv_read(w, "_wv_r", "_wv_v", ty, 0, &m);
            w.line("_wv_r.expect_end!");
            w.line("queue << _wv_v");
        });
        w.line("rescue Error => e");
        w.scope(|w| {
            w.line("queue << e");
        });
        w.line("end");
        out.push_str(&w.finish());
        return;
    }
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
            w.line("queue << result");
        }
        TypeRef::Bool => {
            w.line("queue << (result != 0)");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            // Borrowed for the callback's duration: copy, don't free.
            w.line("queue << (result.null? ? '' : result.read_string)");
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            // Borrowed for the callback's duration: copy, don't free.
            w.line("queue << (result.null? ? ''.b : result.read_string(result_len))");
        }
        TypeRef::TypedHandle(name) => {
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
        // Only `Interface?` reaches here (every other optional is buffered):
        // a nullable adopted object pointer, null meaning none.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                let local = local_type_name(name);
                w.line(format!(
                    "queue << (result.null? ? nil : {local}._from_ptr(result))"
                ));
            }
            _ => unreachable!("buffered optional handled above"),
        },
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
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
            render_param_conversion(
                &mut pc,
                &p.name.to_snake_case(),
                &p.ty,
                &ind,
                scope.module_name(),
            );
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
                        let needs_len = matches!(elem, TypeRef::Bytes | TypeRef::BorrowedBytes)
                            || abi::is_buffered(elem);
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
    if abi::is_buffered(elem) {
        // A buffered element is a producer-allocated value buffer: copy the
        // bytes, release them, then decode and yield the value.
        w.line("item_ptr = out_item.read_pointer");
        w.line("item_len = out_item_len.read(:size_t)");
        w.line("_wv_data = item_ptr.null? ? ''.b : item_ptr.read_string(item_len)");
        w.line(format!(
            "{m}weaveffi_free_bytes(item_ptr, item_len) unless item_ptr.null?"
        ));
        w.line("_wv_r = WvBufferReader.new(_wv_data)");
        render_wv_read(&mut w, "_wv_r", "_wv_item", elem, 0, &m);
        w.line("_wv_r.expect_end!");
        w.line("y << _wv_item");
        out.push_str(&w.finish());
        return;
    }
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
        // A yielded typed handle is a new owned reference; the wrapper adopts
        // the pointer.
        TypeRef::TypedHandle(name) => {
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

/// Emit the statements converting one wrapper parameter into the locals its
/// C call slots reference (see [`rb_call_args`]). A buffered parameter is
/// packed into its value-buffer encoding and copied into a `MemoryPointer`
/// the C call borrows for its duration; the caller keeps ownership and the
/// callee never frees it. `qualifier` names the top-level Ruby module when
/// rendering inside a class body.
fn render_param_conversion(
    out: &mut String,
    name: &str,
    ty: &TypeRef,
    ind: &str,
    qualifier: Option<&str>,
) {
    let q = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    if abi::is_buffered(ty) {
        w.line(format!("{name}_w = WvBufferWriter.new"));
        render_wv_write(&mut w, &format!("{name}_w"), name, ty, 0, &q);
        w.line(format!("{name}_data = {name}_w.data"));
        w.line(format!(
            "{name}_buf = FFI::MemoryPointer.new(:uint8, {name}_data.bytesize)"
        ));
        w.line(format!("{name}_buf.put_bytes(0, {name}_data)"));
        out.push_str(&w.finish());
        return;
    }
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
        _ => {}
    }
    out.push_str(&w.finish());
}

// ── Return value rendering ──

/// Emit the statements converting the raw C `result` (plus any out-params)
/// into the wrapper's idiomatic Ruby return value. A buffered return is a
/// producer-allocated value buffer paired with `out_len`: the bytes are
/// copied, released with `weaveffi_free_bytes`, then decoded.
fn render_return_code(out: &mut String, ty: &TypeRef, ind: &str, qualifier: Option<&str>) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    if abi::is_buffered(ty) {
        w.line("len = out_len.read(:size_t)");
        w.line("data = result.null? ? ''.b : result.read_string(len)");
        w.line(format!(
            "{m}weaveffi_free_bytes(result, len) unless result.null?"
        ));
        w.line("_wv_r = WvBufferReader.new(data)");
        render_wv_read(&mut w, "_wv_r", "_wv_value", ty, 0, &m);
        w.line("_wv_r.expect_end!");
        w.line("_wv_value");
        out.push_str(&w.finish());
        return;
    }
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
        // Only `Interface?` reaches here (an absent value is a null pointer;
        // a present one is a new owned reference); every other optional is
        // buffered and handled above.
        TypeRef::Optional(inner) => {
            let TypeRef::Interface(name) = inner.as_ref() else {
                unreachable!("buffered optional handled above")
            };
            w.line("return nil if result.null?");
            w.line(format!("{}._from_ptr(result)", local_type_name(name)));
        }
        TypeRef::Iterator(_) => {
            unreachable!("iterator returns render via render_iterator_function_wrapper")
        }
        // A returned interface transfers ownership of a new object reference;
        // wrap it without re-running initialize.
        TypeRef::Interface(name) => {
            let local = local_type_name(name);
            w.line("raise Error.new(-1, 'null pointer') if result.null?");
            w.line(format!("{local}._from_ptr(result)"));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered type handled above")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
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
            version: "0.6.0".to_string(),
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
                    fields: vec![],
                },
                ErrorCode {
                    name: "IoError".into(),
                    code: 1004,
                    message: "I/O failure".into(),
                    doc: None,
                    fields: vec![],
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
            code.contains("def self.kv_error_from(code, message, payload = nil)"),
            "factory helper: {code}"
        );
        assert!(
            code.contains("def self.check_kv_error!(err)"),
            "checker helper: {code}"
        );
        assert!(
            code.contains("raise kv_error_from(code, msg, payload)"),
            "checker raises typed: {code}"
        );
        assert!(
            code.contains(
                "payload = payload_ptr.null? ? nil : payload_ptr.read_string(err[:payload_len])"
            ),
            "checker copies payload before clearing: {code}"
        );
    }

    #[test]
    fn error_payload_fields_decode_into_attributes() {
        let mut m = simple_module("kv", {
            let mut f = plain_fn("kv_load", vec![str_param("key")], None);
            f.throws = true;
            vec![f]
        });
        m.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![ErrorCode {
                name: "KeyNotFound".into(),
                code: 1001,
                message: "key not found".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "key".into(),
                        ty: TypeRef::StringUtf8,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "attempts".into(),
                        ty: TypeRef::U32,
                        doc: None,
                        default: None,
                    },
                ],
            }],
        });
        let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
        // The exception class exposes the payload fields as attributes.
        assert!(
            code.contains("class KeyNotFound < KvError"),
            "code subclass: {code}"
        );
        assert!(
            code.contains("attr_reader :key") && code.contains("attr_reader :attempts"),
            "payload attrs: {code}"
        );
        assert!(
            code.contains("def initialize(message = 'key not found', key: nil, attempts: nil)"),
            "kwargs initialize: {code}"
        );
        // The factory decodes the value-buffer payload in declaration order.
        assert!(code.contains("when 1001"), "payload dispatch: {code}");
        assert!(
            code.contains("r = WvBufferReader.new(payload || ''.b)"),
            "payload reader: {code}"
        );
        assert!(
            code.contains("_wv_key = r.read_string") && code.contains("_wv_attempts = r.read_u32"),
            "field decode: {code}"
        );
        assert!(
            code.contains(
                "KvError::KeyNotFound.new(message, key: _wv_key, attempts: _wv_attempts)"
            ),
            "typed construction: {code}"
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
            compact.contains("queue << WeaveFFI.kv_error_from(code, msg, payload)"),
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
                fields: vec![],
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
    fn renders_struct_as_value_class() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
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
        assert!(code.contains("class Contact"), "class: {code}");
        assert!(code.contains("attr_reader :id"), "id attr: {code}");
        assert!(code.contains("attr_reader :name"), "name attr: {code}");
        assert!(
            code.contains("def initialize(id:, name:)"),
            "kwargs initialize: {code}"
        );
        assert!(code.contains("def ==(other)"), "structural eq: {code}");
        assert!(
            code.contains("return false unless other.is_a?(Contact)"),
            "eq type guard: {code}"
        );
        // A record is a value type: no FFI pointer wrapping, no destroy, no
        // create, and no C symbols at all.
        assert!(
            !code.contains("ContactPtr") && !code.contains("FFI::AutoPointer"),
            "no pointer wrapper: {code}"
        );
        assert!(
            !code.contains("weaveffi_contacts_Contact_destroy"),
            "no destroy symbol: {code}"
        );
        assert!(
            !code.contains("attach_function :weaveffi_contacts_Contact"),
            "no record C symbols: {code}"
        );
    }

    #[test]
    fn struct_codec_packs_and_unpacks_fields_in_order() {
        let api = make_api(vec![Module {
            name: "geo".into(),
            functions: vec![],
            interfaces: vec![],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "x".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "label".into(),
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
        // One private pack/unpack pair per record, fields in wire order.
        assert!(
            code.contains("def self._wv_write_point(w, v)"),
            "pack helper: {code}"
        );
        let pack = code
            .split("def self._wv_write_point(w, v)")
            .nth(1)
            .expect("pack body");
        let x_write = pack.find("w.write_f64(v.x)").expect("x write");
        let label_write = pack.find("w.write_string(v.label)").expect("label write");
        assert!(x_write < label_write, "declaration order: {code}");
        assert!(
            code.contains("def self._wv_read_point(r)"),
            "unpack helper: {code}"
        );
        assert!(
            code.contains("_wv_x = r.read_f64") && code.contains("_wv_label = r.read_string"),
            "field reads: {code}"
        );
        assert!(
            code.contains("Point.new(x: _wv_x, label: _wv_label)"),
            "unpack constructs value class: {code}"
        );
        // Builders are gone entirely.
        assert!(!code.contains("PointBuilder"), "no builder class: {code}");
        assert!(
            !code.contains("weaveffi_geo_Point_create"),
            "no create symbol: {code}"
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
        // An optional string is buffered: a flag byte selects nil or the value.
        assert!(code.contains("if _wv_r.read_flag"), "flag byte: {code}");
        assert!(
            code.contains("_wv_value = _wv_r.read_string"),
            "present decode: {code}"
        );
        assert!(code.contains("_wv_value = nil"), "absent is nil: {code}");
        assert!(
            code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
            "returned buffer freed: {code}"
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
        // A list return is one value buffer: count prefix, then elements.
        assert!(
            code.contains("_wv_value = Array.new(_wv_r.read_len) do"),
            "count-driven array: {code}"
        );
        assert!(
            code.contains("_wv_e0 = _wv_r.read_i32"),
            "element decode: {code}"
        );
        assert!(
            code.contains("_wv_r.expect_end!"),
            "trailing bytes rejected: {code}"
        );
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
        // A map return is one value buffer: count, then alternating key/value.
        assert!(code.contains("_wv_value = {}"), "hash init: {code}");
        assert!(
            code.contains("_wv_r.read_len.times do"),
            "count-driven loop: {code}"
        );
        assert!(
            code.contains("_wv_k0 = _wv_r.read_string") && code.contains("_wv_v0 = _wv_r.read_i32"),
            "key/value decode: {code}"
        );
        assert!(
            code.contains("_wv_value[_wv_k0] = _wv_v0"),
            "hash insert: {code}"
        );
        // No parallel-array ABI remains.
        assert!(!code.contains("out_keys"), "no out_keys: {code}");
        assert!(!code.contains("out_values"), "no out_values: {code}");
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
        // String elements are decoded from the single value buffer (copies),
        // and only that buffer itself is released.
        assert!(
            code.contains("_wv_e0 = _wv_r.read_string"),
            "string elements decoded: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
            "value buffer freed: {code}"
        );
        assert!(
            !code.contains("weaveffi_free_string("),
            "no per-element frees remain: {code}"
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
            code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
            "value buffer freed: {code}"
        );
    }

    #[test]
    fn map_return_decodes_from_one_buffer() {
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
        // Keys and values decode from the single buffer; only it is freed.
        assert!(
            code.contains("_wv_k0 = _wv_r.read_string"),
            "key decode: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
            "value buffer freed: {code}"
        );
        assert!(
            !code.contains("keys_ptr") && !code.contains("vals_ptr"),
            "no parallel buffers remain: {code}"
        );
    }

    #[test]
    fn optional_scalar_return_decodes_flag_byte() {
        let api = make_api(vec![simple_module(
            "data",
            vec![plain_fn(
                "find_count",
                vec![],
                Some(TypeRef::Optional(Box::new(TypeRef::I32))),
            )],
        )]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        // An optional scalar is buffered: flag byte, then the value.
        assert!(code.contains("if _wv_r.read_flag"), "flag byte: {code}");
        assert!(
            code.contains("_wv_value = _wv_r.read_i32"),
            "present decode: {code}"
        );
        assert!(code.contains("_wv_value = nil"), "absent is nil: {code}");
        assert!(
            code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
            "value buffer freed: {code}"
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
        // A record return is decoded from its value buffer, which is then
        // released with the runtime's free_bytes.
        assert!(
            code.contains("out_len = FFI::MemoryPointer.new(:size_t)"),
            "out_len allocated: {code}"
        );
        assert!(
            code.contains("_wv_value = _wv_read_item(_wv_r)"),
            "record decode: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(result, len) unless result.null?"),
            "value buffer freed: {code}"
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
        // Each yielded record arrives as a producer-allocated value buffer:
        // the wrapper copies the bytes, frees them, then decodes and yields.
        assert!(
            code.contains("out_item_len = FFI::MemoryPointer.new(:size_t)"),
            "element length out-param: {code}"
        );
        assert!(
            code.contains("weaveffi_free_bytes(item_ptr, item_len) unless item_ptr.null?"),
            "element buffer freed: {code}"
        );
        assert!(
            code.contains("_wv_item = _wv_read_entry(_wv_r)"),
            "element decoded: {code}"
        );
        assert!(code.contains("y << _wv_item"), "decoded yield: {code}");
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
        // The error struct layout carries the structured payload slots.
        assert!(
            code.contains(":payload_ptr, :pointer") && code.contains(":payload_len, :size_t"),
            "payload slots in ErrorStruct: {code}"
        );
    }

    #[test]
    fn preamble_has_buffer_runtime() {
        let code = render(&make_api(vec![]), "WeaveFFI", "weaveffi");
        assert!(
            code.contains("class WvBufferWriter"),
            "buffer writer: {code}"
        );
        assert!(
            code.contains("class WvBufferReader"),
            "buffer reader: {code}"
        );
        // Little-endian packed directives and strict decoding guards.
        assert!(code.contains("[v].pack('l<')"), "LE i32 pack: {code}");
        assert!(code.contains("unpack1('E')"), "f64 unpack: {code}");
        assert!(
            code.contains("'malformed value buffer: trailing bytes after value'"),
            "trailing byte guard: {code}"
        );
        assert!(
            code.contains("'malformed value buffer: string is not valid UTF-8'"),
            "UTF-8 guard: {code}"
        );
        assert!(
            code.contains("'malformed value buffer: length prefix exceeds remaining bytes'"),
            "length guard: {code}"
        );
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
        // Buffered types lower to a (ptr, len) slot pair.
        assert_eq!(
            types(&TypeRef::Record("Foo".into())),
            vec![":pointer", ":size_t"]
        );
        assert_eq!(
            types(&TypeRef::List(Box::new(TypeRef::I32))),
            vec![":pointer", ":size_t"]
        );
    }

    #[test]
    fn return_type_string_is_pointer() {
        let ret = abi::lower_return(&TypeRef::StringUtf8, "");
        assert_eq!(rb_ffi_type(&ret.ret, true), ":pointer");
    }

    #[test]
    fn return_type_map_is_buffer_with_out_len() {
        let ret = abi::lower_return(
            &TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32)),
            "",
        );
        assert_eq!(rb_ffi_type(&ret.ret, true), ":pointer");
        assert_eq!(
            rb_return_out_params(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            vec![":pointer"]
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
        // Record list elements decode recursively through the record codec.
        assert!(
            code.contains("_wv_e0 = _wv_read_item(_wv_r)"),
            "struct list element: {code}"
        );
        assert!(
            code.contains("_wv_value = Array.new(_wv_r.read_len) do"),
            "count-driven array: {code}"
        );
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
        // An optional record is buffered: the flag byte selects nil or a
        // decoded value class instance.
        assert!(code.contains("if _wv_r.read_flag"), "flag byte: {code}");
        assert!(
            code.contains("_wv_value = _wv_read_item(_wv_r)"),
            "present decode: {code}"
        );
        assert!(code.contains("_wv_value = nil"), "absent is nil: {code}");
    }

    // ── Comprehensive tests ──

    fn contacts_api() -> Api {
        Api {
            version: "0.6.0".into(),
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
        assert!(rb.contains("class Contact"), "struct class: {rb}");
        assert!(
            rb.contains("attr_reader :first_name"),
            "first_name attr: {rb}"
        );
        assert!(
            rb.contains("attr_reader :last_name"),
            "last_name attr: {rb}"
        );
        assert!(
            rb.contains("def initialize(first_name:, last_name:)"),
            "kwargs initialize: {rb}"
        );
        assert!(
            rb.contains("_wv_value = _wv_read_contact(_wv_r)"),
            "struct return decode: {rb}"
        );
        assert!(
            !rb.contains("FFI::AutoPointer"),
            "no pointer wrapping remains: {rb}"
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
        // Optional returns decode a flag byte from the value buffer.
        assert!(
            rb.contains("if _wv_r.read_flag"),
            "flag byte on optional return: {rb}"
        );
        assert!(rb.contains("_wv_value = nil"), "absent is nil: {rb}");
        // An optional parameter packs a flag byte (plus the value when
        // present) into the value buffer handed to the C call.
        assert!(
            rb.contains("key_w = WvBufferWriter.new"),
            "optional param writer: {rb}"
        );
        assert!(
            rb.contains("key_w.write_flag(false)") && rb.contains("key_w.write_flag(true)"),
            "optional param flag: {rb}"
        );
        assert!(
            rb.contains("key_w.write_i32(key)"),
            "optional param value: {rb}"
        );
        assert!(
            rb.contains("key_buf, key_data.bytesize"),
            "optional param slot pair: {rb}"
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
        // List returns decode count-prefixed elements from the value buffer.
        assert!(
            rb.contains("_wv_value = Array.new(_wv_r.read_len) do"),
            "list return decode: {rb}"
        );
        // A list parameter packs count then elements, and hands the C call a
        // MemoryPointer copy of the encoding.
        assert!(
            rb.contains("names_w.write_len(names.length)"),
            "list param count: {rb}"
        );
        assert!(
            rb.contains("names.each do |_wv_e0|"),
            "list param elements: {rb}"
        );
        assert!(
            rb.contains("names_w.write_string(_wv_e0)"),
            "list param element write: {rb}"
        );
        assert!(
            rb.contains("names_buf = FFI::MemoryPointer.new(:uint8, names_data.bytesize)"),
            "list param buffer copy: {rb}"
        );
        assert!(
            rb.contains("names_buf, names_data.bytesize"),
            "list param slot pair: {rb}"
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
        assert!(rb.contains("attr_reader :id"), "id attr: {rb}");
        assert!(
            rb.contains("attr_reader :first_name"),
            "first_name attr: {rb}"
        );
        assert!(rb.contains("attr_reader :email"), "email attr: {rb}");
        assert!(
            rb.contains("attr_reader :contact_type"),
            "contact_type attr: {rb}"
        );

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
        let buffer_free = fn_text
            .find("weaveffi_free_bytes(result, len)")
            .expect("free_bytes in find_contact");
        let decode = fn_text
            .find("_wv_read_contact(_wv_r)")
            .expect("decode in find_contact");
        assert!(
            err_check < buffer_free,
            "error must be checked before touching the result buffer: {fn_text}"
        );
        assert!(
            buffer_free < decode,
            "buffer is copied and freed exactly once before decoding: {fn_text}"
        );
        assert_eq!(
            fn_text.matches("weaveffi_free_bytes(result").count(),
            1,
            "result buffer freed exactly once: {fn_text}"
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

        // The flag byte gates decoding: the record codec only runs for a
        // present value, and an absent one yields nil.
        let flag_check = fn_text
            .find("if _wv_r.read_flag")
            .expect("flag check in find_contact");
        let contact_decode = fn_text
            .find("_wv_read_contact(_wv_r)")
            .expect("decode in find_contact");
        assert!(
            flag_check < contact_decode,
            "optional record return should check the flag before decoding: {fn_text}"
        );
        assert!(
            fn_text.contains("_wv_value = nil"),
            "absent optional is nil: {fn_text}"
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
    fn rich_enum_renders_tagged_class_hierarchy() {
        let code = render(&shapes_api(), "Shapes", "weaveffi");

        // A rich enum is a tagged class hierarchy, never a plain constants
        // module and never an opaque pointer wrapper.
        assert!(
            !code.contains("module Shape\n"),
            "rich enum must not be a plain enum module: {code}"
        );
        assert!(code.contains("class Shape\n"), "rich enum base: {code}");
        assert!(
            !code.contains("ShapePtr") && !code.contains("FFI::AutoPointer"),
            "no pointer wrapper: {code}"
        );

        // One subclass per variant, carrying its TAG and fields.
        assert!(
            code.contains("class Empty < Shape") && code.contains("TAG = 0"),
            "unit variant: {code}"
        );
        assert!(
            code.contains("class Circle < Shape") && code.contains("TAG = 1"),
            "circle variant: {code}"
        );
        assert!(
            code.contains("class Labeled < Shape") && code.contains("TAG = 3"),
            "labeled variant: {code}"
        );
        assert!(code.contains("attr_reader :radius"), "circle field: {code}");
        assert!(
            code.contains("def initialize(radius:)"),
            "circle kwargs initialize: {code}"
        );
        assert!(
            code.contains("def initialize(width:, height:)"),
            "rectangle kwargs initialize: {code}"
        );
        assert!(
            code.contains("def tag") && code.contains("self.class::TAG"),
            "tag reader: {code}"
        );
        assert!(code.contains("def ==(other)"), "structural eq: {code}");

        // Rich enums own no C symbols at all.
        assert!(
            !code.contains("attach_function :weaveffi_shapes_Shape"),
            "no rich enum C symbols: {code}"
        );

        // Plain sibling enum still renders as a constants module.
        assert!(
            code.contains("module Channel"),
            "plain enum still a module: {code}"
        );
    }

    #[test]
    fn rich_enum_codec_and_wrappers_use_value_buffers() {
        let code = render(&shapes_api(), "Shapes", "weaveffi");

        // The pack helper dispatches on the variant class and writes the tag
        // followed by the variant's fields; unknown objects trap.
        assert!(
            code.contains("def self._wv_write_shape(w, v)"),
            "pack helper: {code}"
        );
        assert!(
            code.contains("when Shape::Circle"),
            "variant dispatch: {code}"
        );
        let circle_pack = code
            .split("when Shape::Circle")
            .nth(1)
            .expect("circle pack arm");
        assert!(
            circle_pack.contains("w.write_i32(1)") && circle_pack.contains("w.write_f64(v.radius)"),
            "tag then fields: {code}"
        );
        assert!(
            code.contains("raise Error.new(-1, 'unknown Shape variant')"),
            "unknown variant trap: {code}"
        );

        // The unpack helper switches on the decoded tag and constructs the
        // matching subclass; unknown tags trap.
        assert!(
            code.contains("def self._wv_read_shape(r)"),
            "unpack helper: {code}"
        );
        assert!(code.contains("tag = r.read_i32"), "tag decode: {code}");
        assert!(
            code.contains("Shape::Circle.new(radius: _wv_radius)"),
            "circle construction: {code}"
        );
        assert!(
            code.contains("Shape::Rectangle.new(width: _wv_width, height: _wv_height)"),
            "rectangle construction: {code}"
        );
        assert!(
            code.contains("Shape::Empty.new"),
            "unit construction: {code}"
        );

        // A rich enum parameter packs into a value buffer and passes the
        // (ptr, len) slot pair; a rich enum return decodes from one.
        assert!(
            code.contains("def self.describe(shape)"),
            "describe wrapper: {code}"
        );
        assert!(
            code.contains("_wv_write_shape(shape_w, shape)"),
            "describe packs param: {code}"
        );
        assert!(
            code.contains("shape_buf, shape_data.bytesize"),
            "describe slot pair: {code}"
        );
        assert!(
            code.contains("_wv_value = _wv_read_shape(_wv_r)"),
            "scale decodes return: {code}"
        );
    }

    /// A mixed module exercising every buffered surface at once: records
    /// nested in rich enums, buffered parameters and returns at module and
    /// interface scope, a typed error with payload fields, a buffered async
    /// result, a buffered iterator element, and a buffered listener argument.
    #[test]
    fn kitchen_sink_module_renders_coherently() {
        let mut m = simple_module(
            "kv",
            vec![
                {
                    let mut f = plain_fn(
                        "kv_lookup",
                        vec![str_param("key")],
                        Some(TypeRef::Record("Entry".into())),
                    );
                    f.throws = true;
                    f
                },
                plain_fn(
                    "kv_tags",
                    vec![Param {
                        name: "filter".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    }],
                    Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                ),
                plain_fn(
                    "kv_meta",
                    vec![Param {
                        name: "entries".into(),
                        ty: TypeRef::List(Box::new(TypeRef::Record("Entry".into()))),
                        mutable: false,
                        doc: None,
                    }],
                    Some(TypeRef::Map(
                        Box::new(TypeRef::StringUtf8),
                        Box::new(TypeRef::I32),
                    )),
                ),
                {
                    let mut f = plain_fn(
                        "kv_load",
                        vec![],
                        Some(TypeRef::List(Box::new(TypeRef::RichEnum("Event".into())))),
                    );
                    f.r#async = true;
                    f.throws = true;
                    f
                },
                {
                    let mut f = plain_fn(
                        "kv_scan",
                        vec![],
                        Some(TypeRef::Iterator(Box::new(TypeRef::Record("Entry".into())))),
                    );
                    f.throws = true;
                    f
                },
            ],
        );
        m.structs = vec![StructDef {
            name: "Entry".into(),
            doc: None,
            fields: vec![
                StructField {
                    name: "key".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                },
                StructField {
                    name: "hits".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::U32)),
                    doc: None,
                    default: None,
                },
            ],
        }];
        m.enums = vec![EnumDef {
            name: "Event".into(),
            doc: None,
            variants: vec![
                EnumVariant {
                    name: "Added".into(),
                    value: 0,
                    doc: None,
                    fields: vec![StructField {
                        name: "entry".into(),
                        ty: TypeRef::Record("Entry".into()),
                        doc: None,
                        default: None,
                    }],
                },
                EnumVariant {
                    name: "Cleared".into(),
                    value: 1,
                    doc: None,
                    fields: vec![],
                },
            ],
        }];
        m.errors = Some(ErrorDomain {
            name: "KvError".into(),
            codes: vec![ErrorCode {
                name: "KeyNotFound".into(),
                code: 1001,
                message: "key not found".into(),
                doc: None,
                fields: vec![StructField {
                    name: "key".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
        });
        m.interfaces = vec![InterfaceDef {
            name: "Store".into(),
            doc: None,
            constructors: vec![plain_fn("new", vec![str_param("path")], None)],
            methods: vec![plain_fn(
                "put",
                vec![Param {
                    name: "entry".into(),
                    ty: TypeRef::Record("Entry".into()),
                    mutable: false,
                    doc: None,
                }],
                None,
            )],
            statics: vec![],
        }];
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        m.callbacks = vec![CallbackDef {
            name: "OnEvent".into(),
            params: vec![Param {
                name: "event".into(),
                ty: TypeRef::RichEnum("Event".into()),
                mutable: false,
                doc: None,
            }],
            doc: None,
        }];
        m.listeners = vec![ListenerDef {
            name: "event_listener".into(),
            event_callback: "OnEvent".into(),
            doc: None,
        }];
        let code = render(&make_api(vec![m]), "WeaveFFI", "weaveffi");
        // Codec calls inside a class body qualify the module receiver; at
        // module scope they stay bare.
        assert!(
            code.contains("WeaveFFI._wv_write_entry(entry_w, entry)"),
            "qualified codec call in interface method: {code}"
        );
        assert!(
            code.contains("_wv_write_entry(entries_w, _wv_e0)"),
            "list element pack at module scope: {code}"
        );
        // The rich enum codec recurses into the record codec for its
        // record-typed variant field.
        assert!(
            code.contains("_wv_entry = _wv_read_entry(r)"),
            "nested record decode in rich enum codec: {code}"
        );
        // The async list-of-rich-enum result decodes elementwise inside the
        // completion callback.
        assert!(
            code.contains("_wv_e0 = _wv_read_event(_wv_r)"),
            "async rich enum element decode: {code}"
        );
        // The iterator's buffered elements route through the record codec.
        assert!(
            code.contains("_wv_item = _wv_read_entry(_wv_r)"),
            "iterator element decode: {code}"
        );
        // The listener decodes the borrowed rich enum before the dispatch.
        assert!(
            code.contains("event_v = _wv_read_event(event_r)"),
            "listener rich enum decode: {code}"
        );
    }

    #[test]
    fn async_buffered_result_decoded_inside_callback() {
        let api = make_api(vec![Module {
            name: "io".into(),
            functions: vec![{
                let mut f = plain_fn(
                    "load_tags",
                    vec![],
                    Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                );
                f.r#async = true;
                f
            }],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        // The borrowed result buffer is decoded inside the callback and
        // never freed (the producer frees after the callback returns).
        assert!(
            code.contains(
                "_wv_r = WvBufferReader.new(result_ptr.null? ? ''.b : result_ptr.read_string(result_len))"
            ),
            "borrowed buffer copied and decoded: {code}"
        );
        assert!(code.contains("queue << _wv_v"), "decoded push: {code}");
        assert!(
            !code.contains("weaveffi_free_bytes(result_ptr"),
            "borrowed callback buffer must not be freed: {code}"
        );
        // A decode failure surfaces through the queue rather than raising
        // across the C callback boundary.
        assert!(
            code.contains("rescue Error => e") && code.contains("queue << e"),
            "decode errors queued: {code}"
        );
    }

    #[test]
    fn listener_buffered_argument_decoded_before_dispatch() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            callbacks: vec![CallbackDef {
                name: "OnUpdate".into(),
                params: vec![Param {
                    name: "entry".into(),
                    ty: TypeRef::Record("Entry".into()),
                    mutable: false,
                    doc: None,
                }],
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "update_listener".into(),
                event_callback: "OnUpdate".into(),
                doc: None,
            }],
            structs: vec![StructDef {
                name: "Entry".into(),
                doc: None,
                fields: vec![StructField {
                    name: "key".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
            }],
            ..simple_module("events", vec![])
        }]);
        let code = render(&api, "WeaveFFI", "weaveffi");
        // The callback type declares the borrowed (ptr, len) slot pair.
        assert!(
            code.contains(
                "callback :weaveffi_events_OnUpdate_fn, [:pointer, :size_t, :pointer], :void"
            ),
            "callback decl: {code}"
        );
        // The trampoline decodes the borrowed buffer before the dispatch and
        // hands the block the decoded value.
        assert!(
            code.contains(
                "entry_r = WvBufferReader.new(entry_ptr.null? ? ''.b : entry_ptr.read_string(entry_len))"
            ),
            "borrowed arg decoded: {code}"
        );
        assert!(
            code.contains("_wv_entry_v = _wv_read_entry(entry_r)")
                || code.contains("entry_v = _wv_read_entry(entry_r)"),
            "record decode: {code}"
        );
        assert!(code.contains("block.call(entry_v)"), "dispatch: {code}");
        // A malformed buffer drops the event instead of raising across the
        // C callback boundary.
        assert!(
            code.contains("warn \"weaveffi: dropped OnUpdate event: #{e.message}\""),
            "malformed event dropped: {code}"
        );
    }
}
