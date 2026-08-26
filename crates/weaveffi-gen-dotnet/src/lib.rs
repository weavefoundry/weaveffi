//! .NET (P/Invoke) binding generator for WeaveFFI.
//!
//! Emits a C# project (`.csproj` + `.nuspec`) with P/Invoke declarations
//! and idiomatic wrappers over the C ABI. Async functions surface as
//! `Task<T>`-returning methods. Implements [`LanguageBackend`]; the shared
//! driver bridges it into the generator pipeline.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use camino::Utf8Path;
use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde::{Deserialize, Serialize};
use weaveffi_core::abi::{self, AbiParam, CType};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, EnumBinding, EnumVariantBinding, ErrorBinding,
    FieldBinding, FnBinding, InterfaceBinding, IteratorBinding, ListenerBinding, ModuleBinding,
    ParamBinding, StructBinding,
};
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::pkg::{self, ResolvedPackage};
use weaveffi_core::plan::ErrorStrategy;
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};
use weaveffi_ir::ir::{Api, TypeRef};

/// Per-target configuration for [`DotnetGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DotnetConfig {
    /// C# namespace (and on-disk basename used for `.cs`/`.csproj`/`.nuspec`).
    /// Defaults to `"WeaveFFI"`.
    pub namespace: Option<String>,
    /// When `true` (the default), strip the IR module name prefix from emitted
    /// C# method names; the per-module static class already namespaces them.
    /// Set to `false` to restore module-prefixed names.
    pub strip_module_prefix: bool,
    /// C ABI symbol prefix (default `"weaveffi"`). Normally set once globally
    /// via `[global] c_prefix`; honored so the P/Invoke bindings call the same
    /// exported symbols the producer emits.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl Default for DotnetConfig {
    fn default() -> Self {
        Self {
            namespace: None,
            strip_module_prefix: true,
            prefix: None,
            input_basename: None,
        }
    }
}

impl DotnetConfig {
    /// Returns the configured C# namespace, falling back to `"WeaveFFI"`.
    pub fn namespace(&self) -> &str {
        self.namespace.as_deref().unwrap_or("WeaveFFI")
    }

    /// Returns the configured C ABI symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// .NET backend: emits a C# project (`.csproj` and `.nuspec`) of P/Invoke
/// declarations and idiomatic wrappers over the C ABI exposed by the
/// underlying cdylib.
pub struct DotnetGenerator;

impl LanguageBackend for DotnetGenerator {
    type Config = DotnetConfig;

    fn name(&self) -> &'static str {
        "dotnet"
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
        let namespace = config.namespace();
        let input_basename = config.input_basename();
        let package = resolve_dotnet_package(api, config);
        let dir = out_dir.join("dotnet");
        let cs_filename = format!("{namespace}.cs");
        let csproj_filename = format!("{namespace}.csproj");
        let nuspec_filename = format!("{namespace}.nuspec");
        vec![
            OutputFile::new(
                dir.join(&cs_filename),
                render_csharp(
                    model,
                    namespace,
                    config.strip_module_prefix,
                    input_basename,
                    &cs_filename,
                ),
            ),
            OutputFile::new(
                dir.join(&csproj_filename),
                render_csproj(&package, input_basename, &csproj_filename),
            ),
            OutputFile::new(
                dir.join(&nuspec_filename),
                render_nuspec(&package, input_basename, &nuspec_filename),
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
        let namespace = config.namespace();
        let input_basename = config.input_basename();
        let package = resolve_dotnet_package(api, config);
        let dir = out_dir.join("dotnet");
        let lib_name = &ctx.binaries.lib_name;

        let cs_filename = format!("{namespace}.cs");
        let csproj_filename = format!("{namespace}.csproj");
        let nuspec_filename = format!("{namespace}.nuspec");

        // Rebind the P/Invoke library name from the WeaveFFI brand to the
        // bundled library's base name so `[DllImport]` resolves the file we
        // ship under `runtimes/<rid>/native/`.
        let cs = render_csharp(
            model,
            namespace,
            config.strip_module_prefix,
            input_basename,
            &cs_filename,
        )
        .replace(
            "private const string LibName = \"weaveffi\";",
            &format!("private const string LibName = \"{lib_name}\";"),
        );

        let native_assets = "  <ItemGroup>\n    \
             <Content Include=\"runtimes/**\" Pack=\"true\" PackagePath=\"runtimes/\">\n      \
             <CopyToOutputDirectory>PreserveNewest</CopyToOutputDirectory>\n    \
             </Content>\n  </ItemGroup>\n";

        let mut files = vec![
            PackagedFile::text(dir.join(&cs_filename), cs),
            PackagedFile::text(
                dir.join(&csproj_filename),
                render_csproj_with_assets(
                    &package,
                    input_basename,
                    &csproj_filename,
                    native_assets,
                ),
            ),
            PackagedFile::text(
                dir.join(&nuspec_filename),
                render_nuspec(&package, input_basename, &nuspec_filename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(&package, ctx, input_basename),
            ),
        ];

        // Bundle each prebuilt library under the NuGet `runtimes/<rid>/native/`
        // layout NuGet auto-resolves at restore time.
        for nb in &ctx.binaries.binaries {
            let dest = dir
                .join("runtimes")
                .join(nb.platform.nuget_rid())
                .join("native")
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }

        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(DotnetGenerator);

/// Resolve the NuGet/package identity for the .NET target, applying the
/// namespace-as-name fallback when nothing else identifies the package.
fn resolve_dotnet_package(api: &Api, config: &DotnetConfig) -> ResolvedPackage {
    let namespace = config.namespace();
    let mut p = pkg::resolve(
        api,
        config.namespace.as_deref(),
        config.input_basename.as_deref(),
    );
    // The C# namespace doubles as the file basename; when nothing identifies
    // the package, keep the PascalCase brand as the NuGet id so
    // `WeaveFFI.csproj` and `<PackageId>` stay consistent.
    if api.package.is_none() && config.namespace.is_none() && config.input_basename.is_none() {
        p.name = namespace.to_string();
    }
    p
}

/// Render the README for a packaged .NET artifact, listing the bundled
/// runtime identifiers so consumers know which platforms ship prebuilt.
fn render_packaged_readme(
    package: &ResolvedPackage,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    let rids: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `{}`", p.nuget_rid()))
        .collect();
    let rid_list = rids.join("\n");
    format!(
        r#"{prelude}# {name} (.NET)

Auto-generated P/Invoke bindings for the WeaveFFI native library, with a
prebuilt native library bundled for each supported runtime under `runtimes/`.

## Install

```bash
dotnet add package {name}
```

The native library loads automatically; no extra setup is required on a
bundled platform.

## Bundled runtimes

{rid_list}

## Pack

```bash
dotnet pack -c Release
```

{trailer}"#,
    )
}

/// The C# type of a `handle<T>` reference: a generated `{T}Handle` wrapper
/// struct named after the referent's bare local type name.
fn typed_handle_cs(name: &str) -> String {
    format!("{}Handle", local_type_name(name))
}

fn cs_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "sbyte".into(),
        TypeRef::I16 => "short".into(),
        TypeRef::I32 => "int".into(),
        TypeRef::U8 => "byte".into(),
        TypeRef::U16 => "ushort".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::U64 => "ulong".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        TypeRef::Bool => "bool".into(),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string".into(),
        TypeRef::Handle => "ulong".into(),
        // Typed handles surface as a generated `{T}Handle` wrapper struct; a
        // cross-module referent (e.g. `kv.Token`) uses the bare local name.
        TypeRef::TypedHandle(name) => typed_handle_cs(name),
        TypeRef::Bytes | TypeRef::BorrowedBytes => "byte[]".into(),
        // Records are plain data classes; rich enums are abstract sum types.
        // Both are value types decoded from value buffers.
        TypeRef::Record(name) | TypeRef::RichEnum(name) => local_type_name(name).into(),
        TypeRef::Enum(name) => local_type_name(name).into(),
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::I8 => "sbyte?".into(),
            TypeRef::I16 => "short?".into(),
            TypeRef::I32 => "int?".into(),
            TypeRef::U8 => "byte?".into(),
            TypeRef::U16 => "ushort?".into(),
            TypeRef::U32 => "uint?".into(),
            TypeRef::I64 => "long?".into(),
            TypeRef::U64 => "ulong?".into(),
            TypeRef::F32 => "float?".into(),
            TypeRef::F64 => "double?".into(),
            TypeRef::Bool => "bool?".into(),
            TypeRef::Handle => "ulong?".into(),
            TypeRef::TypedHandle(name) => format!("{}?", typed_handle_cs(name)),
            TypeRef::Enum(name) => format!("{}?", local_type_name(name)),
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => "string?".into(),
            TypeRef::Record(name) | TypeRef::RichEnum(name) => {
                format!("{}?", local_type_name(name))
            }
            _ => format!("{}?", cs_type(inner)),
        },
        TypeRef::List(inner) => format!("{}[]", cs_type(inner)),
        TypeRef::Iterator(inner) => format!("IEnumerable<{}>", cs_type(inner)),
        TypeRef::Map(k, v) => format!("Dictionary<{}, {}>", cs_type(k), cs_type(v)),
        // Interfaces surface as their opaque-handle wrapper class; a
        // cross-module reference (`kv.Store`) uses the bare local name.
        TypeRef::Interface(name) => local_type_name(name).into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

fn pinvoke_type(ty: &TypeRef) -> String {
    match ty {
        TypeRef::I8 => "sbyte".into(),
        TypeRef::I16 => "short".into(),
        TypeRef::I32 => "int".into(),
        TypeRef::U8 => "byte".into(),
        TypeRef::U16 => "ushort".into(),
        TypeRef::U32 => "uint".into(),
        TypeRef::I64 => "long".into(),
        TypeRef::U64 => "ulong".into(),
        TypeRef::F32 => "float".into(),
        TypeRef::F64 => "double".into(),
        // C `bool` is one byte; marshalling it as `int` would read past the
        // slot in arrays and leave garbage in the upper bits of returns.
        TypeRef::Bool => "byte".into(),
        TypeRef::StringUtf8
        | TypeRef::BorrowedStr
        | TypeRef::Bytes
        | TypeRef::BorrowedBytes
        | TypeRef::Record(_)
        | TypeRef::RichEnum(_)
        | TypeRef::Interface(_)
        | TypeRef::Optional(_)
        | TypeRef::List(_)
        | TypeRef::Iterator(_)
        | TypeRef::Map(_, _) => "IntPtr".into(),
        TypeRef::Handle => "ulong".into(),
        TypeRef::TypedHandle(_) => "IntPtr".into(),
        TypeRef::Enum(_) => "int".into(),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Maps a shared ABI [`CType`] to its P/Invoke spelling. All pointers collapse
/// to `IntPtr`; `size_t` becomes `UIntPtr`. The structural lowering (which slots
/// exist, in what order) comes from [`weaveffi_core::abi`].
fn cs_pinvoke_ctype(ty: &CType) -> String {
    match ty {
        CType::Int32 | CType::Enum { .. } => "int".into(),
        // C `bool` is one byte on every supported ABI.
        CType::Bool => "byte".into(),
        CType::Uint32 => "uint".into(),
        CType::Int64 => "long".into(),
        CType::Uint64 | CType::Handle => "ulong".into(),
        CType::Double => "double".into(),
        CType::Float => "float".into(),
        CType::Size => "UIntPtr".into(),
        CType::Void => "void".into(),
        CType::Int8 => "sbyte".into(),
        CType::Int16 => "short".into(),
        CType::Uint8 => "byte".into(),
        CType::Uint16 => "ushort".into(),
        CType::Char => "sbyte".into(),
        CType::Ptr { .. }
        | CType::StructTag { .. }
        | CType::CancelToken
        | CType::Error
        | CType::Named(_) => "IntPtr".into(),
    }
}

/// Renders a return out-param. C# expresses the trailing pointer level of a
/// `T*` out-slot with the `out` keyword on the pointee value type.
fn cs_out_param(p: &AbiParam) -> String {
    let pointee = match &p.ty {
        CType::Ptr { pointee, .. } => cs_pinvoke_ctype(pointee),
        other => cs_pinvoke_ctype(other),
    };
    format!("out {} {}", pointee, p.name)
}

fn pinvoke_param_list(p: &ParamBinding) -> Vec<String> {
    abi::lower_param(&p.name, &p.ty, "", false)
        .iter()
        .map(|slot| format!("{} {}", cs_pinvoke_ctype(&slot.ty), slot.name))
        .collect()
}

fn pinvoke_return_info(ty: &TypeRef) -> (String, Vec<String>) {
    let r = abi::lower_return(ty, "");
    (
        cs_pinvoke_ctype(&r.ret),
        r.out_params.iter().map(cs_out_param).collect(),
    )
}

/// True when `ty` surfaces as a C# value type, so its optional wrapper is
/// `Nullable<T>` and a present value is read through `.Value`. Strings, byte
/// arrays, records, rich enums, interfaces, and collections are reference
/// types and use plain `null` checks instead.
fn is_cs_value_type(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::I64
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_)
            | TypeRef::TypedHandle(_)
    )
}

/// Emit statements serializing `expr` (a C# expression of the C# type mapped
/// from `ty`) into the buffer writer named `writer_var`, following the wire
/// format in `docs/src/reference/value-buffers.md`. Nesting recurses;
/// `depth` uniquifies loop locals so nested lists and maps never collide.
fn emit_buffer_write(w: &mut CodeWriter, ty: &TypeRef, expr: &str, writer_var: &str, depth: usize) {
    match ty {
        TypeRef::I8 => {
            w.line(format!("{writer_var}.WriteI8({expr});"));
        }
        TypeRef::I16 => {
            w.line(format!("{writer_var}.WriteI16({expr});"));
        }
        TypeRef::I32 => {
            w.line(format!("{writer_var}.WriteI32({expr});"));
        }
        TypeRef::U8 => {
            w.line(format!("{writer_var}.WriteU8({expr});"));
        }
        TypeRef::U16 => {
            w.line(format!("{writer_var}.WriteU16({expr});"));
        }
        TypeRef::U32 => {
            w.line(format!("{writer_var}.WriteU32({expr});"));
        }
        TypeRef::I64 => {
            w.line(format!("{writer_var}.WriteI64({expr});"));
        }
        TypeRef::U64 => {
            w.line(format!("{writer_var}.WriteU64({expr});"));
        }
        TypeRef::F32 => {
            w.line(format!("{writer_var}.WriteF32({expr});"));
        }
        TypeRef::F64 => {
            w.line(format!("{writer_var}.WriteF64({expr});"));
        }
        TypeRef::Bool => {
            w.line(format!("{writer_var}.WriteBool({expr});"));
        }
        TypeRef::Handle => {
            w.line(format!("{writer_var}.WriteU64({expr});"));
        }
        // A typed handle serializes as the raw pointer value widened to u64.
        TypeRef::TypedHandle(_) => {
            w.line(format!("{writer_var}.WriteU64((ulong)(long){expr}.Raw);"));
        }
        TypeRef::Enum(_) => {
            w.line(format!("{writer_var}.WriteI32((int){expr});"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("{writer_var}.WriteString({expr});"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("{writer_var}.WriteBytes({expr});"));
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) => {
            w.line(format!("{expr}.WriteTo({writer_var});"));
        }
        TypeRef::Optional(inner) => {
            let value_expr = if is_cs_value_type(inner) {
                format!("{expr}.Value")
            } else {
                format!("{expr}!")
            };
            w.line(format!("if ({expr} != null)"));
            w.block("{", "}", |w| {
                w.line(format!("{writer_var}.WriteOptionFlag(true);"));
                emit_buffer_write(w, inner, &value_expr, writer_var, depth);
            });
            w.line("else");
            w.block("{", "}", |w| {
                w.line(format!("{writer_var}.WriteOptionFlag(false);"));
            });
        }
        TypeRef::List(inner) => {
            let item = format!("item{depth}");
            w.line(format!("{writer_var}.WriteLen({expr}.Length);"));
            w.line(format!("foreach (var {item} in {expr})"));
            w.block("{", "}", |w| {
                emit_buffer_write(w, inner, &item, writer_var, depth + 1);
            });
        }
        TypeRef::Map(k, v) => {
            let entry = format!("entry{depth}");
            w.line(format!("{writer_var}.WriteLen({expr}.Count);"));
            w.line(format!("foreach (var {entry} in {expr})"));
            w.block("{", "}", |w| {
                emit_buffer_write(w, k, &format!("{entry}.Key"), writer_var, depth + 1);
                emit_buffer_write(w, v, &format!("{entry}.Value"), writer_var, depth + 1);
            });
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("interfaces and iterators never appear inside a value buffer")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Emit statements declaring a local named `var` and decoding a value of `ty`
/// into it from the buffer reader named `reader_var`, the inverse of
/// [`emit_buffer_write`]. `depth` uniquifies loop counters across nesting.
fn emit_buffer_read(w: &mut CodeWriter, ty: &TypeRef, var: &str, reader_var: &str, depth: usize) {
    match ty {
        TypeRef::I8 => {
            w.line(format!("var {var} = {reader_var}.ReadI8();"));
        }
        TypeRef::I16 => {
            w.line(format!("var {var} = {reader_var}.ReadI16();"));
        }
        TypeRef::I32 => {
            w.line(format!("var {var} = {reader_var}.ReadI32();"));
        }
        TypeRef::U8 => {
            w.line(format!("var {var} = {reader_var}.ReadU8();"));
        }
        TypeRef::U16 => {
            w.line(format!("var {var} = {reader_var}.ReadU16();"));
        }
        TypeRef::U32 => {
            w.line(format!("var {var} = {reader_var}.ReadU32();"));
        }
        TypeRef::I64 => {
            w.line(format!("var {var} = {reader_var}.ReadI64();"));
        }
        TypeRef::U64 => {
            w.line(format!("var {var} = {reader_var}.ReadU64();"));
        }
        TypeRef::F32 => {
            w.line(format!("var {var} = {reader_var}.ReadF32();"));
        }
        TypeRef::F64 => {
            w.line(format!("var {var} = {reader_var}.ReadF64();"));
        }
        TypeRef::Bool => {
            w.line(format!("var {var} = {reader_var}.ReadBool();"));
        }
        TypeRef::Handle => {
            w.line(format!("var {var} = {reader_var}.ReadU64();"));
        }
        TypeRef::TypedHandle(name) => {
            let cn = typed_handle_cs(name);
            w.line(format!(
                "var {var} = new {cn}((IntPtr)(long){reader_var}.ReadU64());"
            ));
        }
        TypeRef::Enum(name) => {
            let cn = local_type_name(name);
            w.line(format!("var {var} = ({cn}){reader_var}.ReadI32();"));
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("var {var} = {reader_var}.ReadString();"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("var {var} = {reader_var}.ReadBytes();"));
        }
        TypeRef::Record(name) | TypeRef::RichEnum(name) => {
            let cn = local_type_name(name);
            w.line(format!("var {var} = {cn}.ReadFrom({reader_var});"));
        }
        TypeRef::Optional(inner) => {
            let cs = cs_type(ty);
            w.line(format!("{cs} {var} = null;"));
            w.line(format!("if ({reader_var}.ReadOptionFlag())"));
            w.block("{", "}", |w| {
                emit_buffer_read(w, inner, &format!("{var}Value"), reader_var, depth);
                w.line(format!("{var} = {var}Value;"));
            });
        }
        TypeRef::List(inner) => {
            let i = format!("i{depth}");
            let elem = cs_type(inner);
            w.line(format!("var {var}Count = {reader_var}.ReadLen();"));
            w.line(format!("var {var} = new {elem}[{var}Count];"));
            w.line(format!("for (int {i} = 0; {i} < {var}Count; {i}++)"));
            w.block("{", "}", |w| {
                emit_buffer_read(w, inner, &format!("{var}Item"), reader_var, depth + 1);
                w.line(format!("{var}[{i}] = {var}Item;"));
            });
        }
        TypeRef::Map(k, v) => {
            let i = format!("i{depth}");
            let k_cs = cs_type(k);
            let v_cs = cs_type(v);
            w.line(format!("var {var}Count = {reader_var}.ReadLen();"));
            w.line(format!(
                "var {var} = new Dictionary<{k_cs}, {v_cs}>({var}Count);"
            ));
            w.line(format!("for (int {i} = 0; {i} < {var}Count; {i}++)"));
            w.block("{", "}", |w| {
                emit_buffer_read(w, k, &format!("{var}Key"), reader_var, depth + 1);
                emit_buffer_read(w, v, &format!("{var}Val"), reader_var, depth + 1);
                w.line(format!("{var}[{var}Key] = {var}Val;"));
            });
        }
        TypeRef::Interface(_) | TypeRef::Iterator(_) => {
            unreachable!("interfaces and iterators never appear inside a value buffer")
        }
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    }
}

/// Collect the local referent names of every `handle<T>` used anywhere in
/// the model (parameters, returns, fields, variant fields, callback
/// parameters, and error payload fields), so one `{T}Handle` wrapper struct
/// is emitted per referent. The `BTreeSet` keeps emission order stable.
fn collect_typed_handles(model: &BindingModel) -> std::collections::BTreeSet<String> {
    fn visit(ty: &TypeRef, acc: &mut std::collections::BTreeSet<String>) {
        match ty {
            TypeRef::TypedHandle(name) => {
                acc.insert(local_type_name(name).to_string());
            }
            TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
                visit(inner, acc);
            }
            TypeRef::Map(k, v) => {
                visit(k, acc);
                visit(v, acc);
            }
            _ => {}
        }
    }
    let mut acc = std::collections::BTreeSet::new();
    for m in &model.modules {
        for f in m.callables() {
            for p in &f.params {
                visit(&p.ty, &mut acc);
            }
            if let Some(r) = &f.ret {
                visit(r, &mut acc);
            }
        }
        for cb in &m.callbacks {
            for p in &cb.params {
                visit(&p.ty, &mut acc);
            }
        }
        for s in &m.structs {
            for f in &s.fields {
                visit(&f.ty, &mut acc);
            }
        }
        for e in &m.enums {
            for v in &e.variants {
                for f in &v.fields {
                    visit(&f.ty, &mut acc);
                }
            }
        }
        if let Some(eb) = &m.error {
            for c in &eb.codes {
                for f in &c.fields {
                    visit(&f.ty, &mut acc);
                }
            }
        }
    }
    acc
}

/// Render the `{T}Handle` wrapper struct for one typed-handle referent: a
/// readonly struct over the raw native pointer token. The token is opaque to
/// the consumer; the producer interprets it.
fn render_typed_handle_struct(out: &mut String, referent: &str) {
    let name = typed_handle_cs(referent);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "/// <summary>A typed native handle referencing a {referent}.</summary>"
    ));
    w.line(format!("public readonly struct {name}"));
    w.block("{", "}", |w| {
        w.line("internal readonly IntPtr Raw;");
        w.blank();
        w.line(format!("internal {name}(IntPtr raw)"));
        w.block("{", "}", |w| {
            w.line("Raw = raw;");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the statements decoding a consumer-side copy of a value buffer
/// (`byte[]` local named `buf`) into a local named `var` of type `ty`,
/// validating that the buffer is fully consumed.
fn emit_buffer_decode(w: &mut CodeWriter, ty: &TypeRef, var: &str, buf: &str) {
    w.line(format!(
        "var {var}Reader = new WeaveFFIBufferReader({buf});"
    ));
    emit_buffer_read(w, ty, var, &format!("{var}Reader"), 0);
    w.line(format!("{var}Reader.ExpectEnd();"));
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn render_csproj(package: &ResolvedPackage, input_basename: &str, filename: &str) -> String {
    render_csproj_with_assets(package, input_basename, filename, "")
}

/// Render the `.csproj`, optionally injecting extra `<ItemGroup>` blocks
/// (`native_assets`) after the main `<PropertyGroup>`. The `weaveffi package`
/// path passes the `runtimes/**` native-asset item group here; `generate`
/// passes an empty string.
fn render_csproj_with_assets(
    package: &ResolvedPackage,
    input_basename: &str,
    filename: &str,
    native_assets: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, filename);
    let id = &package.name;
    let version = &package.version;
    let description = xml_escape(&package.description_or_default());
    let mut extra = format!("    <Description>{description}</Description>\n");
    if !package.authors.is_empty() {
        extra.push_str(&format!(
            "    <Authors>{}</Authors>\n",
            xml_escape(&package.authors.join(", "))
        ));
    }
    if let Some(license) = &package.license {
        extra.push_str(&format!(
            "    <PackageLicenseExpression>{}</PackageLicenseExpression>\n",
            xml_escape(license)
        ));
    }
    if let Some(url) = package.homepage.as_ref().or(package.repository.as_ref()) {
        extra.push_str(&format!(
            "    <PackageProjectUrl>{}</PackageProjectUrl>\n",
            xml_escape(url)
        ));
    }
    format!(
        r#"{prelude}<Project Sdk="Microsoft.NET.Sdk">

  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <PackageId>{id}</PackageId>
    <Version>{version}</Version>
{extra}    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>
  </PropertyGroup>
{native_assets}
</Project>

{trailer}"#,
    )
}

fn render_nuspec(package: &ResolvedPackage, input_basename: &str, filename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, filename);
    let id = &package.name;
    let version = &package.version;
    let authors = if package.authors.is_empty() {
        "WeaveFFI Contributors".to_string()
    } else {
        xml_escape(&package.authors.join(", "))
    };
    let description = xml_escape(&package.description_or_default());
    let license = package.license.clone().unwrap_or_else(|| "MIT".into());
    let project_url = package
        .homepage
        .clone()
        .or_else(|| package.repository.clone())
        .unwrap_or_else(|| "https://github.com/weavefoundry/weaveffi".into());
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
{prelude}<package xmlns="http://schemas.microsoft.com/packaging/2013/05/nuspec.xsd">
  <metadata>
    <id>{id}</id>
    <version>{version}</version>
    <authors>{authors}</authors>
    <description>{description}</description>
    <license type="expression">{license}</license>
    <projectUrl>{project_url}</projectUrl>
    <tags>ffi interop native pinvoke</tags>
  </metadata>
</package>

{trailer}"#,
    )
}

fn render_readme(package: &ResolvedPackage, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let name = &package.name;
    format!(
        r#"{prelude}# {name} (.NET)

Auto-generated P/Invoke bindings for the WeaveFFI native library.

## Build

```bash
dotnet build
```

## Pack

```bash
dotnet pack
```

The resulting `.nupkg` will be in `bin/Debug/` (or `bin/Release/` with `-c Release`).

{trailer}"#,
    )
}

/// Emits a C# XML doc comment at `indent`. Single-line docs collapse to
/// `/// <summary>text</summary>`; multi-line docs expand to a `<summary>`
/// block with each input line wrapped in its own line.
fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    let Some(doc) = doc else {
        return;
    };
    let doc = doc.trim();
    if doc.is_empty() {
        return;
    }
    if doc.contains('\n') {
        out.push_str(indent);
        out.push_str("/// <summary>\n");
        for line in doc.lines() {
            out.push_str(indent);
            out.push_str("/// ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(indent);
        out.push_str("/// </summary>\n");
    } else {
        out.push_str(indent);
        out.push_str("/// <summary>");
        out.push_str(doc);
        out.push_str("</summary>\n");
    }
}

/// Emits a full XML doc block: function `<summary>` plus a `<param>` element
/// per documented parameter. Skips entirely when there is nothing to emit.
fn emit_fn_doc(out: &mut String, doc: &Option<String>, params: &[ParamBinding], indent: &str) {
    let trimmed_doc = doc.as_ref().map(|d| d.trim()).filter(|d| !d.is_empty());
    let documented_params: Vec<&ParamBinding> = params
        .iter()
        .filter(|p| {
            p.doc
                .as_ref()
                .map(|d| !d.trim().is_empty())
                .unwrap_or(false)
        })
        .collect();
    if trimmed_doc.is_none() && documented_params.is_empty() {
        return;
    }
    if let Some(d) = trimmed_doc {
        if d.contains('\n') {
            out.push_str(indent);
            out.push_str("/// <summary>\n");
            for line in d.lines() {
                out.push_str(indent);
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("/// </summary>\n");
        } else {
            out.push_str(indent);
            out.push_str("/// <summary>");
            out.push_str(d);
            out.push_str("</summary>\n");
        }
    }
    for p in documented_params {
        let pdoc = p.doc.as_ref().unwrap().trim();
        let name = safe_cs_name(&p.name);
        if pdoc.contains('\n') {
            out.push_str(indent);
            out.push_str(&format!("/// <param name=\"{}\">\n", name));
            for line in pdoc.lines() {
                out.push_str(indent);
                out.push_str("/// ");
                out.push_str(line);
                out.push('\n');
            }
            out.push_str(indent);
            out.push_str("/// </param>\n");
        } else {
            out.push_str(indent);
            out.push_str(&format!("/// <param name=\"{}\">{}</param>\n", name, pdoc));
        }
    }
}

/// Emit [`emit_doc`] at the writer's current depth by rendering into a scratch
/// buffer and splicing it verbatim, so a [`CodeWriter`]-based renderer can
/// interleave XML doc comments without re-implementing their formatting.
fn writer_doc(w: &mut CodeWriter, doc: &Option<String>) {
    let mut tmp = String::new();
    emit_doc(&mut tmp, doc, &w.indent_str());
    w.raw(tmp);
}

/// Emit [`emit_fn_doc`] at the writer's current depth, splicing the rendered
/// `<summary>`/`<param>` block in verbatim. The [`CodeWriter`] companion to
/// [`emit_fn_doc`] used by the method renderers.
fn writer_fn_doc(w: &mut CodeWriter, doc: &Option<String>, params: &[ParamBinding]) {
    let mut tmp = String::new();
    emit_fn_doc(&mut tmp, doc, params, &w.indent_str());
    w.raw(tmp);
}

fn render_csharp(
    model: &BindingModel,
    namespace: &str,
    strip_module_prefix: bool,
    input_basename: &str,
    filename: &str,
) -> String {
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    // Opt the file into the nullable annotation context so the `string?`
    // signatures (optional strings) are valid regardless of the consuming
    // project's <Nullable> setting; without this, default projects warn CS8632.
    out.push_str("#nullable enable\n\n");
    out.push_str(
        "using System;\nusing System.Collections.Generic;\nusing System.Runtime.InteropServices;\n",
    );
    if model
        .modules
        .iter()
        .flat_map(|m| m.callables())
        .any(|f| f.is_async)
    {
        out.push_str("using System.Threading.Tasks;\n");
    }
    out.push('\n');
    out.push_str(&format!("namespace {namespace}\n{{\n"));

    // One typed exception per declaring module; inheriting submodules
    // reference the ancestor's type through `ModuleBinding::error`.
    let domains: Vec<&ErrorBinding> = model
        .modules
        .iter()
        .filter_map(|m| m.error.as_ref())
        .filter(|eb| eb.declared_here)
        .collect();

    render_exception_class(&mut out);
    for eb in &domains {
        render_domain_exception(&mut out, eb);
    }
    render_error_struct(&mut out, &domains);
    render_helpers_class(&mut out);
    render_buffer_classes(&mut out);
    for referent in collect_typed_handles(model) {
        render_typed_handle_struct(&mut out, &referent);
    }
    if model
        .modules
        .iter()
        .flat_map(|m| m.callables())
        .any(|f| matches!(f.shape, CallShape::Iterator(_)))
    {
        render_once_enumerable_class(&mut out);
    }

    for m in &model.modules {
        for e in &m.enums {
            // Rich (algebraic) enums are sum types, emitted as an abstract
            // class with one nested variant class each; only plain C-style
            // enums map to `enum`.
            if e.is_rich() {
                render_rich_enum_class(&mut out, e);
            } else {
                render_enum(&mut out, e);
            }
        }
        for s in &m.structs {
            render_struct_class(&mut out, s);
        }
        for i in &m.interfaces {
            render_interface_class(&mut out, i, m.error.as_ref());
        }
    }

    render_native_methods(&mut out, model);

    for m in &model.modules {
        render_wrapper_class(&mut out, m, strip_module_prefix);
    }

    out.push_str("}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// The C# exception class name for one error domain: the domain stem with
/// exactly one `Exception` suffix, so `KvError` becomes `KvException` rather
/// than `KvErrorException`.
fn dotnet_exception_name(eb: &ErrorBinding) -> String {
    errors::exception_type_name(&eb.type_name)
}

/// The per-domain error-check helper name on `WeaveFFIError`; `KvException`
/// is checked by `CheckKv`.
fn check_method_name(eb: &ErrorBinding) -> String {
    let exc = dotnet_exception_name(eb);
    let stem = exc.strip_suffix("Exception").unwrap_or(&exc).to_string();
    format!("Check{stem}")
}

/// Escapes a string for embedding in a C# string literal.
fn cs_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// How a wrapper surfaces a non-zero error slot, rendering
/// [`ErrorStrategy`]: [`ErrorStrategy::Throws`] with a domain in scope raises
/// the typed domain exception; everything else (a producer trap, or a
/// throwing function without a declared domain) raises the plain
/// `WeaveFFIException`, which no domain exception check can catch by type.
#[derive(Clone, Copy)]
enum ErrCtx<'a> {
    /// Throw the generic `WeaveFFIException`.
    Generic,
    /// Throw the domain's typed exception via its `FromCode` factory.
    Domain(&'a ErrorBinding),
}

impl<'a> ErrCtx<'a> {
    /// The error context for one function: typed when the function's
    /// [`ErrorStrategy`] is `Throws` and its module has an error domain in
    /// scope, generic otherwise (including every `Trap` function, whose only
    /// failures are producer bugs and must not wear the domain type).
    fn for_fn(f: &FnBinding, error: Option<&'a ErrorBinding>) -> Self {
        match (f.error_strategy(), error) {
            (ErrorStrategy::Throws, Some(eb)) => ErrCtx::Domain(eb),
            _ => ErrCtx::Generic,
        }
    }

    /// The check statement placed after a native call writing into `err`.
    fn check_stmt(&self) -> String {
        self.check_stmt_for("err")
    }

    /// The check statement for a named `WeaveFFIError` local.
    fn check_stmt_for(&self, var: &str) -> String {
        match self {
            ErrCtx::Generic => format!("WeaveFFIError.Check({var});"),
            ErrCtx::Domain(eb) => format!("WeaveFFIError.{}({var});", check_method_name(eb)),
        }
    }

    /// The exception expression an async completion callback faults its
    /// `TaskCompletionSource` with. A domain exception decodes the error's
    /// structured payload, copied from the borrowed error struct inside the
    /// callback (the producer releases the original afterward).
    fn async_exception_expr(&self) -> String {
        match self {
            ErrCtx::Generic => "new WeaveFFIException(wErr.Code, msg)".into(),
            ErrCtx::Domain(eb) => {
                format!(
                    "{}.FromCode(wErr.Code, msg, payload)",
                    dotnet_exception_name(eb)
                )
            }
        }
    }

    /// Emit the `<exception>` XML doc line for a throwing wrapper; generic
    /// wrappers document nothing extra.
    fn write_exception_doc(&self, w: &mut CodeWriter) {
        if let ErrCtx::Domain(eb) = self {
            w.line(format!(
                "/// <exception cref=\"{}\">Thrown when the call reports a {} code.</exception>",
                dotnet_exception_name(eb),
                eb.type_name
            ));
        }
    }
}

fn render_exception_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("public class WeaveFFIException : Exception");
    w.block("{", "}", |w| {
        w.line("public int Code { get; }");
        w.blank();
        w.line("public WeaveFFIException(int code, string message) : base(message)");
        w.block("{", "}", |w| {
            w.line("Code = code;");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// One typed exception class per declared error domain, extending the generic
/// brand exception. Each code surfaces as a `public const int` (PascalCase),
/// and `FromCode` maps a raw error slot to the typed exception, falling back
/// to the generic `WeaveFFIException` for unknown codes. When the matched
/// code declares payload fields, `FromCode` decodes them from the serialized
/// payload buffer and exposes each field in the exception's `Data`
/// dictionary, keyed by the IDL field name.
fn render_domain_exception(out: &mut String, eb: &ErrorBinding) {
    let exc = dotnet_exception_name(eb);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "/// <summary>Typed exception for the {} error domain (module {}).</summary>",
        eb.type_name,
        eb.owner_path.replace('_', ".")
    ));
    w.line(format!("public class {exc} : WeaveFFIException"));
    w.block("{", "}", |w| {
        for c in &eb.codes {
            if c.doc.is_some() {
                writer_doc(w, &c.doc);
            } else {
                w.line(format!("/// <summary>{}</summary>", xml_escape(&c.message)));
            }
            w.line(format!(
                "public const int {} = {};",
                errors::pascal(&c.name),
                c.value
            ));
        }
        w.blank();
        w.line(format!(
            "public {exc}(int code, string message) : base(code, message)"
        ));
        w.line("{");
        w.line("}");
        w.blank();
        w.line("/// <summary>Wraps a raw error slot in the typed exception, falling");
        w.line("/// back to <see cref=\"WeaveFFIException\"/> for unknown codes. Codes");
        w.line("/// declaring payload fields decode them into Data.</summary>");
        w.line("internal static WeaveFFIException FromCode(int code, string message, byte[]? payload)");
        w.block("{", "}", |w| {
            w.line("switch (code)");
            w.block("{", "}", |w| {
                for c in &eb.codes {
                    w.line(format!("case {}:", errors::pascal(&c.name)));
                    if c.fields.is_empty() {
                        w.indent();
                        w.line(format!(
                            "return new {exc}(code, string.IsNullOrEmpty(message) ? \"{}\" : message);",
                            cs_str(&c.message)
                        ));
                        w.dedent();
                    } else {
                        w.block("{", "}", |w| {
                            w.line(format!(
                                "var ex = new {exc}(code, string.IsNullOrEmpty(message) ? \"{}\" : message);",
                                cs_str(&c.message)
                            ));
                            w.line("if (payload != null)");
                            w.block("{", "}", |w| {
                                w.line("var reader = new WeaveFFIBufferReader(payload);");
                                for f in &c.fields {
                                    let var = format!("f{}", f.name.to_upper_camel_case());
                                    emit_buffer_read(w, &f.ty, &var, "reader", 0);
                                    w.line(format!("ex.Data[\"{}\"] = {var};", cs_str(&f.name)));
                                }
                                w.line("reader.ExpectEnd();");
                            });
                            w.line("return ex;");
                        });
                    }
                }
                w.line("default:");
                w.indent();
                w.line("return new WeaveFFIException(code, message);");
                w.dedent();
            });
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The raw error slot plus its check helpers: the generic `Check` (throws
/// `WeaveFFIException` on any non-zero code) and one `Check{Domain}` variant
/// per declared domain (throws the typed exception via `FromCode`). Every
/// check copies the message (and, for domains, the serialized payload) and
/// then calls `weaveffi_error_clear`, which frees both producer allocations,
/// before throwing.
fn render_error_struct(out: &mut String, domains: &[&ErrorBinding]) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("[StructLayout(LayoutKind.Sequential)]");
    w.line("internal struct WeaveFFIError");
    w.block("{", "}", |w| {
        w.line("public int Code;");
        w.line("public IntPtr Message;");
        w.line("public IntPtr PayloadPtr;");
        w.line("public UIntPtr PayloadLen;");
        w.blank();
        w.line("internal static byte[]? CopyPayload(WeaveFFIError err)");
        w.block("{", "}", |w| {
            w.line("if (err.PayloadPtr == IntPtr.Zero || (int)err.PayloadLen == 0)");
            w.block("{", "}", |w| {
                w.line("return null;");
            });
            w.line("var payload = new byte[(int)err.PayloadLen];");
            w.line("Marshal.Copy(err.PayloadPtr, payload, 0, (int)err.PayloadLen);");
            w.line("return payload;");
        });
        w.blank();
        w.line("internal static void Check(WeaveFFIError err)");
        w.block("{", "}", |w| {
            w.line("if (err.Code != 0)");
            w.block("{", "}", |w| {
                // The clear zeroes the slot, so capture code and message
                // before releasing the producer allocations.
                w.line("var code = err.Code;");
                w.line("var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";");
                w.line("NativeMethods.weaveffi_error_clear(ref err);");
                w.line("throw new WeaveFFIException(code, msg);");
            });
        });
        for eb in domains {
            let exc = dotnet_exception_name(eb);
            let check = check_method_name(eb);
            w.blank();
            w.line(format!("internal static void {check}(WeaveFFIError err)"));
            w.block("{", "}", |w| {
                w.line("if (err.Code != 0)");
                w.block("{", "}", |w| {
                    w.line("var code = err.Code;");
                    w.line("var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";");
                    w.line("var payload = CopyPayload(err);");
                    w.line("NativeMethods.weaveffi_error_clear(ref err);");
                    w.line(format!("throw {exc}.FromCode(code, msg, payload);"));
                });
            });
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

fn render_helpers_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("internal static class WeaveFFIHelpers");
    w.block("{", "}", |w| {
        w.line("internal static IntPtr StringToPtr(string? s)");
        w.block("{", "}", |w| {
            w.line("return s == null ? IntPtr.Zero : Marshal.StringToCoTaskMemUTF8(s);");
        });
        w.blank();
        w.line("internal static string? PtrToString(IntPtr ptr)");
        w.block("{", "}", |w| {
            w.line("return ptr == IntPtr.Zero ? null : Marshal.PtrToStringUTF8(ptr);");
        });
        w.blank();
        w.line("internal static void FreePtr(IntPtr ptr)");
        w.block("{", "}", |w| {
            w.line("Marshal.FreeCoTaskMem(ptr);");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The private buffer writer and reader implementing the WeaveFFI value-buffer
/// wire format (little-endian, packed, no alignment) over managed byte arrays.
/// The reader rejects malformed input (truncation, invalid bool or option
/// flags, oversized length prefixes, trailing bytes) by throwing
/// `InvalidOperationException`; a malformed buffer is always a producer or
/// consumer bug, never a recoverable domain error.
fn render_buffer_classes(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.block_raw(
        r#"/// <summary>Serializes values into the WeaveFFI value-buffer wire
/// format (little-endian, packed).</summary>
internal sealed class WeaveFFIBufferWriter
{
    private byte[] _buf = new byte[64];
    private int _len;

    private void Ensure(int extra)
    {
        if (_len + extra <= _buf.Length)
        {
            return;
        }
        var size = _buf.Length * 2;
        while (size < _len + extra)
        {
            size *= 2;
        }
        Array.Resize(ref _buf, size);
    }

    internal void WriteBool(bool v)
    {
        Ensure(1);
        _buf[_len++] = v ? (byte)1 : (byte)0;
    }

    internal void WriteI8(sbyte v)
    {
        Ensure(1);
        _buf[_len++] = (byte)v;
    }

    internal void WriteU8(byte v)
    {
        Ensure(1);
        _buf[_len++] = v;
    }

    internal void WriteU16(ushort v)
    {
        Ensure(2);
        _buf[_len++] = (byte)v;
        _buf[_len++] = (byte)(v >> 8);
    }

    internal void WriteI16(short v)
    {
        WriteU16((ushort)v);
    }

    internal void WriteU32(uint v)
    {
        Ensure(4);
        _buf[_len++] = (byte)v;
        _buf[_len++] = (byte)(v >> 8);
        _buf[_len++] = (byte)(v >> 16);
        _buf[_len++] = (byte)(v >> 24);
    }

    internal void WriteI32(int v)
    {
        WriteU32((uint)v);
    }

    internal void WriteU64(ulong v)
    {
        WriteU32((uint)v);
        WriteU32((uint)(v >> 32));
    }

    internal void WriteI64(long v)
    {
        WriteU64((ulong)v);
    }

    internal void WriteF32(float v)
    {
        WriteU32((uint)BitConverter.SingleToInt32Bits(v));
    }

    internal void WriteF64(double v)
    {
        WriteU64((ulong)BitConverter.DoubleToInt64Bits(v));
    }

    internal void WriteLen(int len)
    {
        WriteU32((uint)len);
    }

    internal void WriteOptionFlag(bool present)
    {
        WriteBool(present);
    }

    internal void WriteString(string v)
    {
        var bytes = System.Text.Encoding.UTF8.GetBytes(v);
        WriteLen(bytes.Length);
        Ensure(bytes.Length);
        Array.Copy(bytes, 0, _buf, _len, bytes.Length);
        _len += bytes.Length;
    }

    internal void WriteBytes(byte[] v)
    {
        WriteLen(v.Length);
        Ensure(v.Length);
        Array.Copy(v, 0, _buf, _len, v.Length);
        _len += v.Length;
    }

    internal byte[] ToArray()
    {
        var outBuf = new byte[_len];
        Array.Copy(_buf, outBuf, _len);
        return outBuf;
    }
}

/// <summary>Decodes values from the WeaveFFI value-buffer wire format.
/// A malformed buffer indicates a producer/consumer contract violation and
/// throws <see cref="InvalidOperationException"/>.</summary>
internal sealed class WeaveFFIBufferReader
{
    private static readonly System.Text.Encoding Utf8Strict =
        new System.Text.UTF8Encoding(false, true);

    private readonly byte[] _data;
    private int _pos;

    internal WeaveFFIBufferReader(byte[] data)
    {
        _data = data;
    }

    private void Require(int n)
    {
        if (_data.Length - _pos < n)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: buffer exhausted");
        }
    }

    internal bool ReadBool()
    {
        Require(1);
        var b = _data[_pos++];
        if (b > 1)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: invalid bool byte");
        }
        return b == 1;
    }

    internal sbyte ReadI8()
    {
        Require(1);
        return (sbyte)_data[_pos++];
    }

    internal byte ReadU8()
    {
        Require(1);
        return _data[_pos++];
    }

    internal ushort ReadU16()
    {
        Require(2);
        var v = (ushort)(_data[_pos] | (_data[_pos + 1] << 8));
        _pos += 2;
        return v;
    }

    internal short ReadI16()
    {
        return (short)ReadU16();
    }

    internal uint ReadU32()
    {
        Require(4);
        var v = (uint)_data[_pos]
            | ((uint)_data[_pos + 1] << 8)
            | ((uint)_data[_pos + 2] << 16)
            | ((uint)_data[_pos + 3] << 24);
        _pos += 4;
        return v;
    }

    internal int ReadI32()
    {
        return (int)ReadU32();
    }

    internal ulong ReadU64()
    {
        var lo = (ulong)ReadU32();
        var hi = (ulong)ReadU32();
        return lo | (hi << 32);
    }

    internal long ReadI64()
    {
        return (long)ReadU64();
    }

    internal float ReadF32()
    {
        return BitConverter.Int32BitsToSingle(ReadI32());
    }

    internal double ReadF64()
    {
        return BitConverter.Int64BitsToDouble(ReadI64());
    }

    internal int ReadLen()
    {
        var len = ReadU32();
        if (len > (uint)(_data.Length - _pos))
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: length prefix exceeds remaining bytes");
        }
        return (int)len;
    }

    internal bool ReadOptionFlag()
    {
        return ReadBool();
    }

    internal string ReadString()
    {
        var len = ReadLen();
        string s;
        try
        {
            s = Utf8Strict.GetString(_data, _pos, len);
        }
        catch (ArgumentException)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: string is not valid UTF-8");
        }
        _pos += len;
        return s;
    }

    internal byte[] ReadBytes()
    {
        var len = ReadLen();
        var outBuf = new byte[len];
        Array.Copy(_data, _pos, outBuf, 0, len);
        _pos += len;
        return outBuf;
    }

    internal void ExpectEnd()
    {
        if (_pos != _data.Length)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: trailing bytes");
        }
    }
}
"#,
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The single-use `IEnumerable<T>` wrapping every iterator return. The
/// native iterator is consumed (and destroyed) by its one enumerator, so a
/// second `GetEnumerator()` cannot yield anything; surfacing it as an
/// `InvalidOperationException` beats silently returning an empty or
/// double-destroyed sequence.
fn render_once_enumerable_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("/// <summary>A lazily streamed sequence backed by a native iterator.");
    w.line("/// It can be enumerated exactly once; enumerate it promptly (or call");
    w.line("/// a materializing operator such as ToList) and let the enumerator be");
    w.line("/// disposed to release the native iterator.</summary>");
    w.line("internal sealed class WeaveFFIOnceEnumerable<T> : IEnumerable<T>");
    w.block("{", "}", |w| {
        w.line("private IEnumerator<T>? _enumerator;");
        w.blank();
        w.line("internal WeaveFFIOnceEnumerable(IEnumerator<T> enumerator)");
        w.block("{", "}", |w| {
            w.line("_enumerator = enumerator;");
        });
        w.blank();
        w.line("public IEnumerator<T> GetEnumerator()");
        w.block("{", "}", |w| {
            w.line("var e = System.Threading.Interlocked.Exchange(ref _enumerator, null);");
            w.line("if (e == null)");
            w.block("{", "}", |w| {
                w.line(
                    "throw new InvalidOperationException(\"this sequence can be enumerated only once\");",
                );
            });
            w.line("return e;");
        });
        w.blank();
        w.line("System.Collections.IEnumerator System.Collections.IEnumerable.GetEnumerator()");
        w.block("{", "}", |w| {
            w.line("return GetEnumerator();");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

fn render_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum is not a plain C# `enum`; it surfaces as an
    // opaque-object class via `render_rich_enum_class`. Guard here so this
    // path only ever emits C-style enums.
    if e.is_rich() {
        return;
    }
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &e.doc);
    w.line(format!("public enum {}", e.name));
    w.block("{", "}", |w| {
        for v in &e.variants {
            writer_doc(w, &v.doc);
            w.line(format!("{} = {},", v.name, v.value));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the get-only properties and the positional constructor shared by
/// record classes and rich-enum variant classes: one PascalCase property per
/// field plus a public constructor taking every field in declaration order.
fn render_value_members(w: &mut CodeWriter, class_name: &str, fields: &[FieldBinding]) {
    for field in fields {
        writer_doc(w, &field.doc);
        w.line(format!(
            "public {} {} {{ get; }}",
            cs_type(&field.ty),
            field.name.to_upper_camel_case()
        ));
        w.blank();
    }
    let params_sig: Vec<String> = fields
        .iter()
        .map(|f| {
            format!(
                "{} {}",
                cs_type(&f.ty),
                safe_cs_name(&f.name.to_lower_camel_case())
            )
        })
        .collect();
    w.line(format!("public {class_name}({})", params_sig.join(", ")));
    w.block("{", "}", |w| {
        for f in fields {
            w.line(format!(
                "{} = {};",
                f.name.to_upper_camel_case(),
                safe_cs_name(&f.name.to_lower_camel_case())
            ));
        }
    });
}

/// The `new {Class}(fField1, fField2, ...)` argument list matching the locals
/// [`emit_buffer_read`] declares for each field in `ReadFrom`.
fn read_ctor_args(fields: &[FieldBinding]) -> String {
    fields
        .iter()
        .map(|f| format!("f{}", f.name.to_upper_camel_case()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render a record as a plain sealed data class: typed get-only properties, a
/// positional constructor, and the internal `WriteTo`/`ReadFrom` pair
/// implementing the record's value-buffer encoding (fields in declaration
/// order). Records own no native resources, so there is no handle, `Dispose`,
/// builder, or getter symbol.
fn render_struct_class(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &s.doc);
    w.line(format!("public sealed class {}", s.name));
    w.line("{");
    w.indent();
    render_value_members(&mut w, &s.name, &s.fields);
    w.blank();
    w.line("internal void WriteTo(WeaveFFIBufferWriter writer)");
    w.block("{", "}", |w| {
        for f in &s.fields {
            emit_buffer_write(w, &f.ty, &f.name.to_upper_camel_case(), "writer", 0);
        }
    });
    w.blank();
    w.line(format!(
        "internal static {} ReadFrom(WeaveFFIBufferReader reader)",
        s.name
    ));
    w.block("{", "}", |w| {
        for f in &s.fields {
            emit_buffer_read(
                w,
                &f.ty,
                &format!("f{}", f.name.to_upper_camel_case()),
                "reader",
                0,
            );
        }
        w.line(format!(
            "return new {}({});",
            s.name,
            read_ctor_args(&s.fields)
        ));
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as an idiomatic sum type: an abstract base
/// class with a private constructor and one nested sealed class per variant
/// (`Shape.Circle`), each carrying its fields as typed properties. The base
/// class hosts the internal `WriteTo`/`ReadFrom` pair implementing the
/// enum's value-buffer encoding: an `i32` tag followed by the active
/// variant's fields in declaration order. Rich enums own no native
/// resources and declare no C symbols.
fn render_rich_enum_class(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &e.doc);
    w.line(format!("public abstract class {name}"));
    w.line("{");
    w.indent();
    // The private constructor closes the hierarchy: only the nested variant
    // classes can derive from the base.
    w.line(format!("private {name}()"));
    w.line("{");
    w.line("}");
    w.blank();

    for v in &e.variants {
        let mut vw = CodeWriter::four_space().with_depth(2);
        render_rich_variant_class(&mut vw, name, v);
        w.raw(vw.finish());
    }

    w.line("internal void WriteTo(WeaveFFIBufferWriter writer)");
    w.block("{", "}", |w| {
        w.line("switch (this)");
        w.block("{", "}", |w| {
            for v in &e.variants {
                if v.fields.is_empty() {
                    w.line(format!("case {} _:", v.name));
                    w.indent();
                    w.line(format!("writer.WriteI32({});", v.value));
                    w.line("break;");
                    w.dedent();
                } else {
                    w.line(format!("case {} v:", v.name));
                    w.indent();
                    w.line(format!("writer.WriteI32({});", v.value));
                    for f in &v.fields {
                        emit_buffer_write(
                            w,
                            &f.ty,
                            &format!("v.{}", f.name.to_upper_camel_case()),
                            "writer",
                            0,
                        );
                    }
                    w.line("break;");
                    w.dedent();
                }
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "throw new InvalidOperationException(\"unknown {name} variant\");"
            ));
            w.dedent();
        });
    });
    w.blank();
    w.line(format!(
        "internal static {name} ReadFrom(WeaveFFIBufferReader reader)"
    ));
    w.block("{", "}", |w| {
        w.line("var tag = reader.ReadI32();");
        w.line("switch (tag)");
        w.block("{", "}", |w| {
            for v in &e.variants {
                w.line(format!("case {}:", v.value));
                if v.fields.is_empty() {
                    w.indent();
                    w.line(format!("return new {}();", v.name));
                    w.dedent();
                } else {
                    w.block("{", "}", |w| {
                        for f in &v.fields {
                            emit_buffer_read(
                                w,
                                &f.ty,
                                &format!("f{}", f.name.to_upper_camel_case()),
                                "reader",
                                0,
                            );
                        }
                        w.line(format!(
                            "return new {}({});",
                            v.name,
                            read_ctor_args(&v.fields)
                        ));
                    });
                }
            }
            w.line("default:");
            w.indent();
            w.line(format!(
                "throw new InvalidOperationException(\"malformed WeaveFFI value buffer: unknown {name} tag \" + tag);"
            ));
            w.dedent();
        });
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// One nested sealed variant class of a rich enum: typed get-only properties
/// and a positional constructor, exactly like a record. A unit variant has an
/// empty body with the compiler-provided constructor.
fn render_rich_variant_class(w: &mut CodeWriter, enum_name: &str, v: &EnumVariantBinding) {
    writer_doc(w, &v.doc);
    w.line(format!("public sealed class {} : {enum_name}", v.name));
    if v.fields.is_empty() {
        w.line("{");
        w.line("}");
    } else {
        w.line("{");
        w.indent();
        render_value_members(w, &v.name, &v.fields);
        w.dedent();
        w.line("}");
    }
    w.blank();
}

/// A copy of `f` whose parameter names are lowerCamelCase, the C# parameter
/// convention for public wrapper signatures. Only the wrapper signature and
/// its marshalling locals derive from these names; ABI slot names and the
/// P/Invoke declarations keep the IDL spelling.
fn camel_fn(f: &FnBinding) -> FnBinding {
    let mut f = f.clone();
    for p in &mut f.params {
        p.name = p.name.to_lower_camel_case();
    }
    f
}

/// Render one interface as an opaque-handle class following the struct-wrapper
/// pattern: a private `IntPtr` handle with `IDisposable` plus a finalizer
/// calling the interface's destroy symbol. The `new` constructor maps to a
/// real C# constructor, other constructors become static factories, instance
/// methods pass the handle as the leading native argument, and statics are
/// plain static methods. All member shapes reuse the free-function
/// marshalling paths.
fn render_interface_class(out: &mut String, i: &InterfaceBinding, error: Option<&ErrorBinding>) {
    let name = &i.name;
    let mut w = CodeWriter::four_space().with_depth(1);
    writer_doc(&mut w, &i.doc);
    w.line(format!("public class {name} : IDisposable"));
    w.line("{");
    w.indent();
    w.line("private IntPtr _handle;");
    w.line("private bool _disposed;");
    w.blank();
    w.line(format!("internal {name}(IntPtr handle)"));
    w.block("{", "}", |w| {
        w.line("_handle = handle;");
    });
    w.blank();
    w.line("internal IntPtr Handle => _handle;");
    w.blank();

    for c in &i.constructors {
        let err = ErrCtx::for_fn(c, error);
        let mut tmp = String::new();
        if c.name == "new" && matches!(c.shape, CallShape::Sync(_)) {
            render_interface_ctor(&mut tmp, i, c, err);
        } else {
            render_wrapper_method(&mut tmp, c, &c.name.to_upper_camel_case(), None, err);
        }
        w.raw(tmp);
    }
    for m in &i.methods {
        let err = ErrCtx::for_fn(m, error);
        let mut tmp = String::new();
        render_wrapper_method(
            &mut tmp,
            m,
            &m.name.to_upper_camel_case(),
            Some("_handle"),
            err,
        );
        w.raw(tmp);
    }
    for s in &i.statics {
        let err = ErrCtx::for_fn(s, error);
        let mut tmp = String::new();
        render_wrapper_method(&mut tmp, s, &s.name.to_upper_camel_case(), None, err);
        w.raw(tmp);
    }

    w.line("public void Dispose()");
    w.block("{", "}", |w| {
        w.line("if (!_disposed)");
        w.block("{", "}", |w| {
            w.line(format!("NativeMethods.{}(_handle);", i.destroy_symbol));
            w.line("_disposed = true;");
        });
        w.line("GC.SuppressFinalize(this);");
    });
    w.blank();
    w.line(format!("~{name}()"));
    w.block("{", "}", |w| {
        w.line("Dispose();");
    });
    w.dedent();
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render the `new` constructor as a real C# constructor: the sync call path
/// with the checked result assigned to `_handle` instead of returned.
fn render_interface_ctor(out: &mut String, i: &InterfaceBinding, f: &FnBinding, err: ErrCtx) {
    let f = camel_fn(f);
    let c_sym = &f.c_base;
    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }
    w.line(format!("public {}({})", i.name, params_sig.join(", ")));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");
        let call_args = build_call_args(&f.params);
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let call = format!("var result = NativeMethods.{c_sym}({args_part}ref err);");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(call.clone());
                w.line(err.check_stmt());
                w.line("_handle = result;");
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line(call);
            w.line(err.check_stmt());
            w.line("_handle = result;");
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

fn render_native_methods(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("internal static class NativeMethods");
    w.line("{");
    w.indent();
    w.line("private const string LibName = \"weaveffi\";");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_free_string(IntPtr ptr);");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);");
    w.blank();
    w.line("[DllImport(LibName, CallingConvention = CallingConvention.Cdecl)]");
    w.line("internal static extern void weaveffi_error_clear(ref WeaveFFIError err);");
    w.blank();
    w.dedent();

    // Records and rich enums are value types with no C symbols, so only
    // interfaces, callbacks, listeners, and functions declare P/Invokes.
    for m in &model.modules {
        for i in &m.interfaces {
            let mut tmp = String::new();
            render_interface_pinvoke(&mut tmp, i);
            w.raw(tmp);
        }
        for cb in &m.callbacks {
            let mut tmp = String::new();
            render_callback_pinvoke(&mut tmp, cb);
            w.raw(tmp);
        }
        for l in &m.listeners {
            let mut tmp = String::new();
            render_listener_pinvoke(&mut tmp, l);
            w.raw(tmp);
        }
        for f in &m.functions {
            let mut tmp = String::new();
            render_shaped_pinvoke(&mut tmp, f);
            w.raw(tmp);
        }
    }

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The unmanaged delegate type for one module callback declaration, shared by
/// every listener that fires it.
fn render_callback_pinvoke(out: &mut String, cb: &CallbackBinding) {
    let delegate_name = format!("Cb_{}", cb.c_fn_type);
    let params: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| format!("{} {}", cs_pinvoke_ctype(&slot.ty), slot.name))
        .collect();
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]");
    w.line(format!(
        "internal delegate void {delegate_name}({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

fn render_listener_pinvoke(out: &mut String, l: &ListenerBinding) {
    let delegate_name = format!("Cb_{}", l.callback_c_fn_type);
    let register_sym = &l.register_symbol;
    let unregister_sym = &l.unregister_symbol;

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{register_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern ulong {register_sym}({delegate_name} callback, IntPtr context);"
    ));
    w.blank();

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{unregister_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {unregister_sym}(ulong id);"
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the extern declaration set matching one callable's shape exactly:
/// sync, async (delegate + launcher), or iterator (constructor, `next`,
/// `destroy`). Shared by free functions and interface members.
fn render_shaped_pinvoke(out: &mut String, f: &FnBinding) {
    match &f.shape {
        CallShape::Sync(_) => render_function_pinvoke(out, f),
        CallShape::Async(_) => render_async_function_pinvoke(out, f),
        CallShape::Iterator(it) => render_iterator_pinvoke(out, it),
    }
}

/// The `[DllImport]` set backing one interface: the destroy symbol plus one
/// shape-matched extern set per member. Instance members carry the implicit
/// leading `self` slot.
fn render_interface_pinvoke(out: &mut String, i: &InterfaceBinding) {
    let destroy_sym = &i.destroy_symbol;
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{destroy_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {destroy_sym}(IntPtr self);"
    ));
    w.blank();
    out.push_str(&w.finish());

    for f in i
        .constructors
        .iter()
        .chain(i.methods.iter())
        .chain(i.statics.iter())
    {
        render_shaped_pinvoke(out, f);
    }
}

fn render_function_pinvoke(out: &mut String, f: &FnBinding) {
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_pinvoke(out, it);
        return;
    }
    let c_sym = &f.c_base;

    let mut params: Vec<String> = Vec::new();
    if f.has_self {
        params.push("IntPtr self".into());
    }
    params.extend(f.params.iter().flat_map(pinvoke_param_list));

    let ret_type = if let Some(ret) = &f.ret {
        let (ret_cs, extra) = pinvoke_return_info(ret);
        params.extend(extra);
        ret_cs
    } else {
        "void".into()
    };

    params.push("ref WeaveFFIError err".into());

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{c_sym}\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern {ret_type} {c_sym}({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// Whether an ABI slot is the trailing `{prefix}_error* out_err`.
fn is_error_slot(slot: &AbiParam) -> bool {
    matches!(&slot.ty, CType::Ptr { pointee, .. } if matches!(pointee.as_ref(), CType::Error))
}

/// One P/Invoke parameter for an iterator-shape ABI slot: the trailing error
/// slot becomes `ref WeaveFFIError`, `out_*` pointer slots become `out`
/// pointee values, everything else is passed by value.
fn iterator_slot_param(slot: &AbiParam) -> String {
    if is_error_slot(slot) {
        return format!("ref WeaveFFIError {}", slot.name);
    }
    match &slot.ty {
        CType::Ptr { .. } if slot.name.starts_with("out_") => cs_out_param(slot),
        ty => format!("{} {}", cs_pinvoke_ctype(ty), slot.name),
    }
}

/// The three entry points behind one `iter<T>` function: the constructor
/// returning the opaque iterator handle, `_next`, and `_destroy`.
fn render_iterator_pinvoke(out: &mut String, it: &IteratorBinding) {
    let launch_params: Vec<String> = it.launch.params.iter().map(iterator_slot_param).collect();
    let mut w = CodeWriter::four_space().with_depth(2);
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.launch.symbol
    ));
    w.line(format!(
        "internal static extern IntPtr {}({});",
        it.launch.symbol,
        launch_params.join(", ")
    ));
    w.blank();

    let next_params: Vec<String> = it.next.params.iter().map(iterator_slot_param).collect();
    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.next.symbol
    ));
    w.line(format!(
        "internal static extern int {}({});",
        it.next.symbol,
        next_params.join(", ")
    ));
    w.blank();

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{0}\", CallingConvention = CallingConvention.Cdecl)]",
        it.destroy_symbol
    ));
    w.line(format!(
        "internal static extern void {}(IntPtr iter);",
        it.destroy_symbol
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// True when an async result crosses the completion callback as a borrowed
/// `ptr` + `len` pair: bytes and every buffered type (records, rich enums,
/// lists, maps, and non-interface optionals).
fn async_result_is_ptr_len(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::Bytes | TypeRef::BorrowedBytes) || abi::is_buffered(ty)
}

fn async_cb_delegate_result_params(ret: &Option<TypeRef>) -> String {
    match ret {
        None => String::new(),
        Some(ty) if async_result_is_ptr_len(ty) => ", IntPtr result, UIntPtr resultLen".into(),
        Some(ty) => format!(", {} result", pinvoke_type(ty)),
    }
}

fn async_cb_lambda_params(ret: &Option<TypeRef>) -> &'static str {
    match ret {
        None => "(context, err)",
        Some(ty) if async_result_is_ptr_len(ty) => "(context, err, result, resultLen)",
        Some(_) => "(context, err, result)",
    }
}

fn render_async_function_pinvoke(out: &mut String, f: &FnBinding) {
    let c_sym = &f.c_base;
    let delegate_name = format!("AsyncCb_{c_sym}");
    let cb_params = async_cb_delegate_result_params(&f.ret);

    let mut w = CodeWriter::four_space().with_depth(2);
    w.line("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]");
    w.line(format!(
        "internal delegate void {delegate_name}(IntPtr context, IntPtr err{cb_params});"
    ));
    w.blank();

    let mut params: Vec<String> = Vec::new();
    if f.has_self {
        params.push("IntPtr self".into());
    }
    params.extend(f.params.iter().flat_map(pinvoke_param_list));
    if f.cancellable {
        params.push("IntPtr cancel_token".into());
    }
    params.push(format!("{delegate_name} callback"));
    params.push("IntPtr context".into());

    w.line(format!(
        "[DllImport(LibName, EntryPoint = \"{c_sym}_async\", CallingConvention = CallingConvention.Cdecl)]"
    ));
    w.line(format!(
        "internal static extern void {c_sym}_async({});",
        params.join(", ")
    ));
    w.blank();
    out.push_str(&w.finish());
}

/// Statements (appended to `out`) plus the expression converting one callback
/// parameter's delegate slots into the value handed to the user callback.
/// Buffered parameters arrive as a borrowed `ptr` + `len` pair valid only for
/// the dispatch, so the bytes are copied and decoded before the user's
/// delegate runs, and never freed here.
fn render_cb_arg(out: &mut String, p: &ParamBinding, idx: usize, indent: &str) -> String {
    let slots = abi::lower_param(&p.name, &p.ty, "", false);
    let n0 = safe_cs_name(&slots[0].name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if abi::is_buffered(&p.ty) {
        let len = safe_cs_name(&slots[1].name);
        let arg = format!("arg{idx}");
        w.line(format!("var {arg}Buf = new byte[(int){len}];"));
        w.line(format!(
            "if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}Buf, 0, (int){len});"
        ));
        emit_buffer_decode(&mut w, &p.ty, &arg, &format!("{arg}Buf"));
        out.push_str(&w.finish());
        return arg;
    }
    let expr = match &p.ty {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64 => n0,
        TypeRef::Handle => n0,
        TypeRef::Bool => format!("{n0} != 0"),
        TypeRef::Enum(name) => format!("({}){n0}", local_type_name(name)),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            format!("Marshal.PtrToStringUTF8({n0}) ?? \"\"")
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let len = safe_cs_name(&slots[1].name);
            let arg = format!("arg{idx}");
            w.line(format!("var {arg} = new byte[(int){len}];"));
            w.line(format!(
                "if ({n0} != IntPtr.Zero && (int){len} > 0) Marshal.Copy({n0}, {arg}, 0, (int){len});"
            ));
            arg
        }
        TypeRef::TypedHandle(name) => {
            format!("new {}({n0})", typed_handle_cs(name))
        }
        // Borrowed for the duration of the callback; the consumer must not
        // Dispose() the wrapper.
        TypeRef::Interface(name) => {
            format!("new {}({n0})", local_type_name(name))
        }
        // Only `Interface?` reaches here: every other optional is buffered.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                let cn = local_type_name(name);
                format!("{n0} == IntPtr.Zero ? null : new {cn}({n0})")
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered callback parameter handled above")
        }
        TypeRef::Iterator(_) => unreachable!("iterator not valid as callback parameter"),
        TypeRef::Named(_) => unreachable!("unresolved type reference"),
    };
    out.push_str(&w.finish());
    expr
}

/// The register/unregister method pair for one listener, emitted into the
/// module's wrapper class alongside `_listenerRefs`.
fn render_listener_methods(
    out: &mut String,
    mb: &ModuleBinding,
    l: &ListenerBinding,
    strip_module_prefix: bool,
) {
    let Some(cb) = mb.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };
    let register_name = wrapper_name(
        &mb.path,
        &format!("register_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let unregister_name = wrapper_name(
        &mb.path,
        &format!("unregister_{}", l.name),
        strip_module_prefix,
    )
    .to_upper_camel_case();
    let delegate_name = format!("NativeMethods.Cb_{}", cb.c_fn_type);

    let action_type = if cb.params.is_empty() {
        "Action".to_string()
    } else {
        format!(
            "Action<{}>",
            cb.params
                .iter()
                .map(|p| cs_type(&p.ty))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let lambda_formals: Vec<String> = cb
        .abi_params
        .iter()
        .map(|slot| safe_cs_name(&slot.name))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_doc(&mut w, &l.doc);
    w.line(format!(
        "/// <returns>A subscription id for {unregister_name}().</returns>"
    ));
    w.line(format!(
        "public static ulong {register_name}({action_type} callback)"
    ));
    w.line("{");
    w.scope(|w| {
        w.line(format!(
            "{delegate_name} trampoline = ({}) =>",
            lambda_formals.join(", ")
        ));
        w.line("{");
        w.scope(|w| {
            let mut stmts = String::new();
            let mut args = Vec::new();
            for (idx, p) in cb.params.iter().enumerate() {
                args.push(render_cb_arg(&mut stmts, p, idx, "                "));
            }
            w.raw(stmts);
            w.line(format!("callback({});", args.join(", ")));
        });
        w.line("};");
        w.line("ulong id;");
        w.line("lock (_listenerLock)");
        w.line("{");
        w.scope(|w| {
            w.line(format!(
                "id = NativeMethods.{}(trampoline, IntPtr.Zero);",
                l.register_symbol
            ));
            w.line("_listenerRefs[id] = trampoline;");
        });
        w.line("}");
        w.line("return id;");
    });
    w.line("}");
    w.blank();

    w.line(format!(
        "/// <summary>Unregisters a listener previously registered with {register_name}().</summary>"
    ));
    w.line(format!("public static void {unregister_name}(ulong id)"));
    w.line("{");
    w.scope(|w| {
        w.line(format!("NativeMethods.{}(id);", l.unregister_symbol));
        w.line("lock (_listenerLock)");
        w.line("{");
        w.scope(|w| {
            w.line("_listenerRefs.Remove(id);");
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Renders one module's static wrapper class. Submodules become sibling
/// classes named by their full path (`KvStats`, not a nested `Kv.Stats`):
/// flat classes keep generated type names (`Stats`) unambiguous, since a
/// nested module class with the same name as a struct wrapper would shadow it.
fn render_wrapper_class(out: &mut String, mb: &ModuleBinding, strip_module_prefix: bool) {
    let class_name: String = mb
        .segments
        .iter()
        .map(|s| s.to_upper_camel_case())
        .collect();
    out.push_str(&format!("    public static class {class_name}\n    {{\n"));

    if !mb.listeners.is_empty() {
        out.push_str("        private static readonly object _listenerLock = new object();\n");
        out.push_str(
            "        // Live listener delegates by subscription id. Holding the delegate\n",
        );
        out.push_str(
            "        // here keeps its native thunk alive until unregistered; without this\n",
        );
        out.push_str("        // the GC could collect a delegate the producer still calls.\n");
        out.push_str(
            "        private static readonly Dictionary<ulong, Delegate> _listenerRefs = new Dictionary<ulong, Delegate>();\n\n",
        );
        for l in &mb.listeners {
            render_listener_methods(out, mb, l, strip_module_prefix);
        }
    }
    for f in &mb.functions {
        let method_name =
            wrapper_name(&mb.path, &f.name, strip_module_prefix).to_upper_camel_case();
        let err = ErrCtx::for_fn(f, mb.error.as_ref());
        render_wrapper_method(out, f, &method_name, None, err);
    }

    out.push_str("    }\n\n");
}

/// True when a parameter needs setup/cleanup statements around the native
/// call: strings (`CoTaskMem` UTF-8 copies), bytes (pinned arrays), and every
/// buffered type (encoded into a pinned value buffer).
fn param_needs_marshal(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::StringUtf8 | TypeRef::BorrowedStr | TypeRef::Bytes | TypeRef::BorrowedBytes
    ) || abi::is_buffered(ty)
}

/// Render one wrapper method (any shape) named `method_name`. `self_expr` is
/// the receiver's handle expression for interface instance methods (`None`
/// for free functions, statics, and factories, which render as `static`);
/// `err` selects the typed or generic error surface.
fn render_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    if f.is_async {
        render_async_wrapper_method(out, f, method_name, self_expr, err);
        return;
    }
    if let CallShape::Iterator(it) = &f.shape {
        render_iterator_wrapper_method(out, f, it, method_name, self_expr, err);
        return;
    }
    let f = camel_fn(f);
    let ret_cs = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "void".into());
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    w.line(format!(
        "public {staticness}{ret_cs} {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));

        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            let mut tmp = String::new();
            render_pinvoke_call_and_return(&mut tmp, &f, self_expr, err, "                ");
            w.raw(tmp);
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            let mut tmp = String::new();
            render_pinvoke_call_and_return(&mut tmp, &f, self_expr, err, "            ");
            w.raw(tmp);
        }
    });

    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// The statements converting one `_next` out-item into the yielded C# value,
/// freeing any producer-allocated memory along the way (`ElemFree::String`
/// via `weaveffi_free_string`, `ElemFree::Bytes` via `weaveffi_free_bytes`
/// for both bytes and buffered elements). Returns the expression to
/// `yield return`.
fn iterator_item_conversion(out: &mut String, elem: &TypeRef, indent: &str) -> String {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if abi::is_buffered(elem) {
        w.line("var itemBuf = new byte[(int)out_len];");
        w.line("if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, itemBuf, 0, (int)out_len);");
        w.line("NativeMethods.weaveffi_free_bytes(out_item, out_len);");
        emit_buffer_decode(&mut w, elem, "item", "itemBuf");
        out.push_str(&w.finish());
        return "item".into();
    }
    let expr = match elem {
        TypeRef::I8
        | TypeRef::I16
        | TypeRef::I32
        | TypeRef::U8
        | TypeRef::U16
        | TypeRef::U32
        | TypeRef::I64
        | TypeRef::U64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::Handle => "out_item".into(),
        TypeRef::Bool => "out_item != 0".into(),
        TypeRef::Enum(name) => format!("({})out_item", local_type_name(name)),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("var item = Marshal.PtrToStringUTF8(out_item) ?? \"\";");
            w.line("NativeMethods.weaveffi_free_string(out_item);");
            "item".into()
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("var item = new byte[(int)out_len];");
            w.line("if (out_item != IntPtr.Zero && (int)out_len > 0) Marshal.Copy(out_item, item, 0, (int)out_len);");
            w.line("NativeMethods.weaveffi_free_bytes(out_item, out_len);");
            "item".into()
        }
        TypeRef::TypedHandle(name) => {
            format!("new {}(out_item)", typed_handle_cs(name))
        }
        // The consumer owns each yielded wrapper; Dispose() destroys it
        // (owned-object elements are adopted rather than freed eagerly).
        TypeRef::Interface(name) => {
            format!("new {}(out_item)", local_type_name(name))
        }
        other => unreachable!("unsupported iterator element type {other:?}"),
    };
    out.push_str(&w.finish());
    expr
}

/// An `iter<T>` function surfaces as `IEnumerable<T>`, rendering the
/// `IteratorProtocol` pull contract: an eager launcher call (so launch errors
/// throw immediately, per the function's `ErrorStrategy`), then a lazy
/// `yield return` enumerator issuing exactly one C `next` call per
/// `MoveNext`. Each yielded element is released per its `ElemFree` plan after
/// conversion, and the compiler-generated `finally` destroys the native
/// iterator exactly once, whether enumeration runs to exhaustion or is
/// abandoned early (C# `foreach` disposes the enumerator). Wrapping the
/// single enumerator in `WeaveFFIOnceEnumerable` makes a second enumeration
/// throw instead of double-destroying the consumed handle.
fn render_iterator_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    let f = camel_fn(f);
    let elem_cs = cs_type(&it.elem);
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let call_args = full_call_args(&f, self_expr);
    let args_part = if call_args.is_empty() {
        String::new()
    } else {
        format!("{call_args}, ")
    };
    let launch_call = format!(
        "var iter = NativeMethods.{}({args_part}ref err);",
        it.launch.symbol
    );

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    w.line("/// <remarks>Streams lazily: each element is pulled from the native");
    w.line("/// iterator on demand, and the iterator is destroyed when enumeration");
    w.line("/// completes or the enumerator is disposed (a <c>foreach</c> disposes it");
    w.line("/// automatically, including on early exit). The returned sequence can be");
    w.line("/// enumerated only once.</remarks>");
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    let wrap_return =
        format!("return new WeaveFFIOnceEnumerable<{elem_cs}>(Enumerate{method_name}(iter));");
    w.line(format!(
        "public {staticness}IEnumerable<{elem_cs}> {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line("var err = new WeaveFFIError();");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(launch_call.clone());
                w.line(err.check_stmt());
                w.line(wrap_return.clone());
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line(launch_call.clone());
            w.line(err.check_stmt());
            w.line(wrap_return.clone());
        }
    });
    w.line("}");
    w.blank();

    // The `_next` out-slots after the iterator handle, excluding the error.
    let next_out_args: Vec<String> = it
        .next
        .params
        .iter()
        .skip(1)
        .filter(|slot| !is_error_slot(slot))
        .map(|slot| format!("out var {}", slot.name))
        .collect();

    // A `yield return` iterator method: the compiler emits the `finally`
    // into Dispose(), so the destroy below runs exactly once, on exhaustion
    // or when the consumer abandons enumeration early.
    w.line(format!(
        "private static IEnumerator<{elem_cs}> Enumerate{method_name}(IntPtr iter)"
    ));
    w.line("{");
    w.scope(|w| {
        w.line("try");
        w.line("{");
        w.scope(|w| {
            w.line("while (true)");
            w.line("{");
            w.scope(|w| {
                w.line("var iterErr = new WeaveFFIError();");
                w.line(format!(
                    "if (NativeMethods.{}(iter, {}, ref iterErr) == 0)",
                    it.next.symbol,
                    next_out_args.join(", ")
                ));
                w.line("{");
                w.scope(|w| {
                    w.line(err.check_stmt_for("iterErr"));
                    w.line("yield break;");
                });
                w.line("}");
                w.line(err.check_stmt_for("iterErr"));
                let mut conv = String::new();
                let item_expr =
                    iterator_item_conversion(&mut conv, &it.elem, "                    ");
                w.raw(conv);
                w.line(format!("yield return {item_expr};"));
            });
            w.line("}");
        });
        w.line("}");
        w.line("finally");
        w.line("{");
        w.scope(|w| {
            w.line(format!("NativeMethods.{}(iter);", it.destroy_symbol));
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render an async wrapper returning `Task`/`Task<T>` via a
/// `TaskCompletionSource` resolved from the native completion callback. A
/// non-zero error slot faults the task with the typed or generic exception
/// according to `err`.
fn render_async_wrapper_method(
    out: &mut String,
    f: &FnBinding,
    method_name: &str,
    self_expr: Option<&str>,
    err: ErrCtx,
) {
    let f = camel_fn(f);
    let c_sym = &f.c_base;
    let delegate_name = format!("NativeMethods.AsyncCb_{c_sym}");
    let staticness = if self_expr.is_none() { "static " } else { "" };

    let task_ret = f
        .ret
        .as_ref()
        .map(|ty| format!("Task<{}>", cs_type(ty)))
        .unwrap_or_else(|| "Task".into());

    let tcs_type = f.ret.as_ref().map(cs_type).unwrap_or_else(|| "bool".into());

    let params_sig: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", cs_type(&p.ty), safe_cs_name(&p.name)))
        .collect();

    let mut w = CodeWriter::four_space().with_depth(2);
    writer_fn_doc(&mut w, &f.doc, &f.params);
    err.write_exception_doc(&mut w);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[Obsolete(\"{}\")]", msg.replace('"', "\\\"")));
    }

    w.line(format!(
        "public {staticness}async {task_ret} {method_name}({})",
        params_sig.join(", ")
    ));
    w.line("{");
    w.scope(|w| {
        w.line(format!(
            "var tcs = new TaskCompletionSource<{tcs_type}>(TaskCreationOptions.RunContinuationsAsynchronously);"
        ));

        let cb_lambda_params = async_cb_lambda_params(&f.ret);
        w.line(format!("{delegate_name} callback = {cb_lambda_params} =>"));
        w.line("{");
        w.scope(|w| {
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line("if (err != IntPtr.Zero)");
                w.line("{");
                w.scope(|w| {
                    w.line("var wErr = Marshal.PtrToStructure<WeaveFFIError>(err);");
                    w.line("if (wErr.Code != 0)");
                    w.line("{");
                    w.scope(|w| {
                        // The error struct is borrowed for the callback's
                        // duration (the producer releases it afterward), so
                        // the message and payload are copied here and the
                        // error is never cleared by the consumer.
                        w.line("var msg = Marshal.PtrToStringUTF8(wErr.Message) ?? \"\";");
                        if matches!(err, ErrCtx::Domain(_)) {
                            w.line("var payload = WeaveFFIError.CopyPayload(wErr);");
                        }
                        w.line(format!("tcs.SetException({});", err.async_exception_expr()));
                        w.line("return;");
                    });
                    w.line("}");
                });
                w.line("}");

                let mut tmp = String::new();
                render_async_set_result(&mut tmp, &f.ret, "                    ");
                w.raw(tmp);
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            w.scope(|w| {
                w.line("if (context != IntPtr.Zero)");
                w.line("{");
                w.scope(|w| {
                    w.line("GCHandle.FromIntPtr(context).Free();");
                });
                w.line("}");
            });
            w.line("}");
        });
        w.line("};");
        w.line("var gcHandle = GCHandle.Alloc(callback, GCHandleType.Normal);");
        w.line("var ctx = GCHandle.ToIntPtr(gcHandle);");

        let needs_try = f.params.iter().any(|p| param_needs_marshal(&p.ty));
        let call_args = full_call_args(&f, self_expr);
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let cancel_arg = if f.cancellable { "IntPtr.Zero, " } else { "" };

        if needs_try {
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_setup(&mut tmp, p, "            ");
                w.raw(tmp);
            }
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line("try");
                w.line("{");
                w.scope(|w| {
                    w.line(format!(
                        "NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);"
                    ));
                });
                w.line("}");
                w.line("catch");
                w.line("{");
                w.scope(|w| {
                    w.line("if (gcHandle.IsAllocated) gcHandle.Free();");
                    w.line("throw;");
                });
                w.line("}");
            });
            w.line("}");
            w.line("finally");
            w.line("{");
            for p in &f.params {
                let mut tmp = String::new();
                render_marshal_cleanup(&mut tmp, p, "                ");
                w.raw(tmp);
            }
            w.line("}");
        } else {
            w.line("try");
            w.line("{");
            w.scope(|w| {
                w.line(format!(
                    "NativeMethods.{c_sym}_async({args_part}{cancel_arg}callback, ctx);"
                ));
            });
            w.line("}");
            w.line("catch");
            w.line("{");
            w.scope(|w| {
                w.line("if (gcHandle.IsAllocated) gcHandle.Free();");
                w.line("throw;");
            });
            w.line("}");
        }

        if f.ret.is_some() {
            w.line("return await tcs.Task;");
        } else {
            w.line("await tcs.Task;");
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the statements resolving the `TaskCompletionSource` from the
/// completion callback's result slots, honoring the `AsyncProtocol` borrowed
/// results clause: string, bytes, and buffered result buffers are owned by
/// the producer and valid only for the callback's duration, so they are
/// deep-copied (and buffered results decoded) here and never freed.
/// Owned-object results (interfaces, typed handles) are the exception: the
/// callback receives ownership and the wrapper adopts the pointer.
fn render_async_set_result(out: &mut String, ret: &Option<TypeRef>, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if let Some(ty) = ret {
        if abi::is_buffered(ty) {
            w.line("var resultBuf = new byte[(int)resultLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)resultLen > 0) Marshal.Copy(result, resultBuf, 0, (int)resultLen);",
            );
            emit_buffer_decode(&mut w, ty, "value", "resultBuf");
            w.line("tcs.SetResult(value);");
            out.push_str(&w.finish());
            return;
        }
    }
    match ret {
        None => {
            w.line("tcs.SetResult(true);");
        }
        Some(TypeRef::Bool) => {
            w.line("tcs.SetResult(result != 0);");
        }
        Some(TypeRef::StringUtf8 | TypeRef::BorrowedStr) => {
            w.line("tcs.SetResult(Marshal.PtrToStringUTF8(result) ?? \"\");");
        }
        Some(TypeRef::Enum(name)) => {
            let cn = local_type_name(name);
            w.line(format!("tcs.SetResult(({cn})result);"));
        }
        Some(TypeRef::TypedHandle(name)) => {
            let cn = typed_handle_cs(name);
            w.line(format!("tcs.SetResult(new {cn}(result));"));
        }
        Some(TypeRef::Interface(name)) => {
            let cn = local_type_name(name);
            w.line(format!("tcs.SetResult(new {cn}(result));"));
        }
        Some(TypeRef::Bytes | TypeRef::BorrowedBytes) => {
            w.line("var arr = new byte[(int)resultLen];");
            w.line(
                "if (result != IntPtr.Zero && (int)resultLen > 0) Marshal.Copy(result, arr, 0, (int)resultLen);",
            );
            w.line("tcs.SetResult(arr);");
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        Some(TypeRef::Optional(inner)) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                let cn = local_type_name(name);
                w.line(format!(
                    "tcs.SetResult(result == IntPtr.Zero ? null : new {cn}(result));"
                ));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        // Remaining scalars pass by value in the result slot.
        Some(_) => {
            w.line("tcs.SetResult(result);");
        }
    }
    out.push_str(&w.finish());
}

/// Emit the setup statements for one parameter before the native call.
/// Strings copy to `CoTaskMem` UTF-8; bytes pin the managed array; buffered
/// parameters encode into a `byte[]` value buffer (`{name}Buf`) and pin it
/// (`{name}Pin`), which the caller owns for the duration of the call.
fn render_marshal_setup(out: &mut String, p: &ParamBinding, indent: &str) {
    let name = safe_cs_name(&p.name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if abi::is_buffered(&p.ty) {
        w.line(format!("var {name}Writer = new WeaveFFIBufferWriter();"));
        emit_buffer_write(&mut w, &p.ty, &name, &format!("{name}Writer"), 0);
        w.line(format!("var {name}Buf = {name}Writer.ToArray();"));
        w.line(format!(
            "var {name}Pin = GCHandle.Alloc({name}Buf, GCHandleType.Pinned);"
        ));
        out.push_str(&w.finish());
        return;
    }
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!(
                "var {name}Ptr = Marshal.StringToCoTaskMemUTF8({name});"
            ));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!(
                "var {name}Pin = GCHandle.Alloc({name}, GCHandleType.Pinned);"
            ));
        }
        _ => {}
    }
    out.push_str(&w.finish());
}

fn render_marshal_cleanup(out: &mut String, p: &ParamBinding, indent: &str) {
    let name = safe_cs_name(&p.name);
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if abi::is_buffered(&p.ty) {
        w.line(format!("{name}Pin.Free();"));
        out.push_str(&w.finish());
        return;
    }
    match &p.ty {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line(format!("Marshal.FreeCoTaskMem({name}Ptr);"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line(format!("{name}Pin.Free();"));
        }
        _ => {}
    }
    out.push_str(&w.finish());
}

/// The joined native-call argument list: the implicit self handle (when
/// `self_expr` is given) followed by every lowered parameter slot.
fn full_call_args(f: &FnBinding, self_expr: Option<&str>) -> String {
    let args = build_call_args(&f.params);
    match self_expr {
        Some(s) if args.is_empty() => s.to_string(),
        Some(s) => format!("{s}, {args}"),
        None => args,
    }
}

fn render_pinvoke_call_and_return(
    out: &mut String,
    f: &FnBinding,
    self_expr: Option<&str>,
    err: ErrCtx,
    indent: &str,
) {
    let c_sym = &f.c_base;
    let call_args = full_call_args(f, self_expr);

    // Bytes and buffered returns deliver their length through the trailing
    // `size_t* out_len` slot.
    let has_out_len = f.ret.as_ref().is_some_and(|r| {
        matches!(r, TypeRef::Bytes | TypeRef::BorrowedBytes) || abi::is_buffered(r)
    });

    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    if f.ret.is_some() {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        let out_len_part = if has_out_len { "out var outLen, " } else { "" };
        w.line(format!(
            "var result = NativeMethods.{c_sym}({args_part}{out_len_part}ref err);"
        ));
    } else {
        let args_part = if call_args.is_empty() {
            String::new()
        } else {
            format!("{call_args}, ")
        };
        w.line(format!("NativeMethods.{c_sym}({args_part}ref err);"));
    }

    w.line(err.check_stmt());
    out.push_str(&w.finish());

    if let Some(ret_ty) = &f.ret {
        render_return_conversion(out, ret_ty, indent);
    }
}

fn build_call_args(params: &[ParamBinding]) -> String {
    params
        .iter()
        .flat_map(|p| {
            let name = safe_cs_name(&p.name);
            // A buffered parameter passes its pinned value buffer as the
            // borrowed (ptr, len) pair; the caller owns and frees the pin.
            if abi::is_buffered(&p.ty) {
                return vec![
                    format!("{name}Pin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}Buf.Length"),
                ];
            }
            match &p.ty {
                TypeRef::Bool => vec![format!("(byte)({name} ? 1 : 0)")],
                TypeRef::Enum(_) => vec![format!("(int){name}")],
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => vec![format!("{name}Ptr")],
                // A typed handle passes its raw pointer token by value.
                TypeRef::TypedHandle(_) => vec![format!("{name}.Raw")],
                // Interface parameters borrow: pass the handle, ownership
                // stays with the caller's wrapper.
                TypeRef::Interface(_) => {
                    vec![format!("{name}.Handle")]
                }
                TypeRef::Bytes | TypeRef::BorrowedBytes => vec![
                    format!("{name}Pin.AddrOfPinnedObject()"),
                    format!("(UIntPtr){name}.Length"),
                ],
                // Only `Interface?` reaches here: a nullable borrowed pointer.
                TypeRef::Optional(inner) => match inner.as_ref() {
                    TypeRef::Interface(_) => {
                        vec![format!("{name}?.Handle ?? IntPtr.Zero")]
                    }
                    _ => unreachable!("non-interface optionals are buffered"),
                },
                _ => vec![name],
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_return_conversion(out: &mut String, ty: &TypeRef, indent: &str) {
    let mut w = CodeWriter::four_space().with_depth(indent.len() / 4);
    // A buffered return is a producer-allocated value buffer: copy the bytes,
    // release them with `weaveffi_free_bytes`, then decode the copy.
    if abi::is_buffered(ty) {
        w.line("var resultBuf = new byte[(int)outLen];");
        w.line(
            "if (result != IntPtr.Zero && (int)outLen > 0) Marshal.Copy(result, resultBuf, 0, (int)outLen);",
        );
        w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
        emit_buffer_decode(&mut w, ty, "value", "resultBuf");
        w.line("return value;");
        out.push_str(&w.finish());
        return;
    }
    match ty {
        TypeRef::Bool => {
            w.line("return result != 0;");
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            w.line("var str = Marshal.PtrToStringUTF8(result);");
            w.line("NativeMethods.weaveffi_free_string(result);");
            w.line("return str ?? \"\";");
        }
        TypeRef::Enum(name) => {
            let cn = local_type_name(name);
            w.line(format!("return ({cn})result;"));
        }
        TypeRef::TypedHandle(name) => {
            let cn = typed_handle_cs(name);
            w.line(format!("return new {cn}(result);"));
        }
        // An interface return transfers ownership (`ReturnFree::OwnedObject`):
        // wrap the pointer in a new instance whose Dispose() releases it.
        TypeRef::Interface(name) => {
            let cn = local_type_name(name);
            w.line(format!("return new {cn}(result);"));
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            w.line("if (result == IntPtr.Zero) return Array.Empty<byte>();");
            w.line("var arr = new byte[(int)outLen];");
            w.line("Marshal.Copy(result, arr, 0, (int)outLen);");
            w.line("NativeMethods.weaveffi_free_bytes(result, outLen);");
            w.line("return arr;");
        }
        // Only `Interface?` reaches here: a nullable owned object pointer.
        TypeRef::Optional(inner) => match inner.as_ref() {
            TypeRef::Interface(name) => {
                let cn = local_type_name(name);
                w.line(format!(
                    "return result == IntPtr.Zero ? null : new {cn}(result);"
                ));
            }
            _ => unreachable!("non-interface optionals are buffered"),
        },
        TypeRef::Iterator(_) => unreachable!("iterator functions render via CallShape::Iterator"),
        TypeRef::Record(_) | TypeRef::RichEnum(_) | TypeRef::List(_) | TypeRef::Map(_, _) => {
            unreachable!("buffered return handled above")
        }
        _ => {
            w.line("return result;");
        }
    }
    out.push_str(&w.finish());
}

fn safe_cs_name(name: &str) -> String {
    match name {
        "string" | "int" | "long" | "double" | "float" | "bool" | "byte" | "object" | "class"
        | "struct" | "enum" | "event" | "delegate" | "namespace" | "ref" | "out" | "in"
        | "params" | "is" | "as" | "new" | "this" | "base" | "null" | "true" | "false"
        | "return" | "void" | "if" | "else" | "for" | "while" | "do" | "switch" | "case"
        | "break" | "continue" | "try" | "catch" | "finally" | "throw" | "using" | "static"
        | "const" | "readonly" | "override" | "virtual" | "abstract" | "sealed" | "public"
        | "private" | "protected" | "internal" => format!("@{name}"),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField};

    /// Test shim matching the pre-0.5.0 signature: builds the [`BindingModel`]
    /// here so the production `render_csharp` stays model-only.
    fn render_csharp(
        api: &Api,
        namespace: &str,
        strip_module_prefix: bool,
        prefix: &str,
        input_basename: &str,
        filename: &str,
    ) -> String {
        let model = BindingModel::build(api, prefix);
        super::render_csharp(
            &model,
            namespace,
            strip_module_prefix,
            input_basename,
            filename,
        )
    }

    #[test]
    fn package_emits_runtimes_and_rebinds_libname() {
        use camino::Utf8Path;
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = make_api(vec![simple_module(vec![Function {
            name: "ping".into(),
            params: vec![],
            returns: None,
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }])]);
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::WindowsX64, "/s/windows-x64/calculator.dll");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &DotnetGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &DotnetConfig::default(),
        )
        .expect("dotnet supports packaging");

        // NuGet `runtimes/<rid>/native/` layout.
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("runtimes/osx-arm64/native/libcalculator.dylib")));
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("runtimes/win-x64/native/calculator.dll")));
        // The P/Invoke library name is rebound to the bundled base name.
        let cs = files
            .iter()
            .find(|f| f.path.as_str().ends_with(".cs"))
            .expect("C# source present");
        let FileContent::Text(src) = &cs.content else {
            panic!("C# source is text");
        };
        assert!(
            src.contains("private const string LibName = \"calculator\";"),
            "DllImport name not rebound: {src}"
        );
        let csproj = files
            .iter()
            .find(|f| f.path.as_str().ends_with(".csproj"))
            .expect("csproj present");
        let FileContent::Text(proj) = &csproj.content else {
            panic!("csproj is text");
        };
        assert!(
            proj.contains("runtimes/**"),
            "native asset item group missing: {proj}"
        );
    }

    fn make_api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.6.0".into(),
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
            interfaces: vec![],
            modules: vec![],
        }
    }

    #[test]
    fn generator_name_is_dotnet() {
        assert_eq!(Generator::name(&DotnetGenerator), "dotnet");
    }

    #[test]
    fn output_files_lists_all() {
        let api = make_api(vec![]);
        let out = Utf8Path::new("/tmp/out");
        let files = DotnetGenerator.output_files(&api, out, &DotnetConfig::default());
        assert_eq!(
            files,
            vec![
                format!("{out}/dotnet/README.md"),
                format!("{out}/dotnet/WeaveFFI.cs"),
                format!("{out}/dotnet/WeaveFFI.csproj"),
                format!("{out}/dotnet/WeaveFFI.nuspec"),
            ]
        );
    }

    #[test]
    fn generate_creates_output_file() {
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_gen_output");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate(&api, out_dir, &DotnetConfig::default())
            .unwrap();

        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(cs.contains("namespace WeaveFFI"));
        assert!(cs.contains("DllImport"));
        assert!(cs.contains("weaveffi_math_add"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn listeners_generate_register_unregister() {
        use weaveffi_ir::ir::{CallbackDef, ListenerDef};
        let api = make_api(vec![Module {
            name: "events".into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![CallbackDef {
                name: "OnMessage".into(),
                doc: None,
                params: vec![Param {
                    name: "message".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
            }],
            listeners: vec![ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        DotnetGenerator
            .generate(&api, out, &DotnetConfig::default())
            .unwrap();
        let cs = std::fs::read_to_string(dir.path().join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(
            cs.contains("internal delegate void Cb_weaveffi_events_OnMessage_fn"),
            "unmanaged delegate type must be declared: {cs}"
        );
        assert!(
            cs.contains("[UnmanagedFunctionPointer(CallingConvention.Cdecl)]"),
            "delegate must use cdecl: {cs}"
        );
        assert!(
            cs.contains("internal static extern ulong weaveffi_events_register_message_listener"),
            "register pinvoke missing: {cs}"
        );
        assert!(
            cs.contains("public static ulong RegisterMessageListener(Action<string> callback)"),
            "register wrapper missing: {cs}"
        );
        assert!(
            cs.contains("public static void UnregisterMessageListener(ulong id)"),
            "unregister wrapper missing: {cs}"
        );
        assert!(
            cs.contains("_listenerRefs[id] = trampoline;"),
            "delegate must be pinned in the registry: {cs}"
        );
        assert!(
            cs.contains("Marshal.PtrToStringUTF8(message) ?? \"\""),
            "string arg must be marshaled: {cs}"
        );
    }

    #[test]
    fn dotnet_record_is_plain_value_class() {
        let api = Api {
            version: "0.6.0".into(),
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
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                interfaces: vec![],
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        DotnetGenerator
            .generate(&api, out, &DotnetConfig::default())
            .unwrap();
        let dotnet_dir = out.join("dotnet");
        let cs_files: Vec<_> = std::fs::read_dir(&dotnet_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "cs").unwrap_or(false))
            .collect();
        assert!(!cs_files.is_empty(), "expected .cs files");
        let cs = std::fs::read_to_string(cs_files[0].path()).unwrap();
        // A record is a plain sealed data class with typed get-only
        // properties and a positional constructor; builders are gone.
        assert!(
            cs.contains("public sealed class Contact"),
            "missing record class: {cs}"
        );
        assert!(
            cs.contains("public string Name { get; }") && cs.contains("public int Age { get; }"),
            "missing typed properties: {cs}"
        );
        assert!(
            cs.contains("public Contact(string name, int age)"),
            "missing positional constructor: {cs}"
        );
        // The value-buffer pack/unpack pair replaces every C symbol.
        assert!(
            cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)")
                && cs.contains("internal static Contact ReadFrom(WeaveFFIBufferReader reader)"),
            "missing pack/unpack pair: {cs}"
        );
        assert!(
            cs.contains("writer.WriteString(Name);") && cs.contains("writer.WriteI32(Age);"),
            "missing field encoding: {cs}"
        );
        assert!(
            !cs.contains("ContactBuilder") && !cs.contains("weaveffi_contacts_Contact_create"),
            "builder machinery must be gone: {cs}"
        );
        assert!(
            !cs.contains("class Contact : IDisposable"),
            "records must not be disposable: {cs}"
        );
    }

    #[test]
    fn dotnet_generates_csproj() {
        let api = make_api(vec![simple_module(vec![])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_csproj");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate(&api, out_dir, &DotnetConfig::default())
            .unwrap();

        let csproj_path = tmp.join("dotnet/WeaveFFI.csproj");
        assert!(csproj_path.exists(), ".csproj file must exist");
        let csproj = std::fs::read_to_string(&csproj_path).unwrap();
        assert!(
            csproj.contains(r#"Sdk="Microsoft.NET.Sdk""#),
            "missing SDK attribute: {csproj}"
        );
        assert!(
            csproj.contains("<TargetFramework>net8.0</TargetFramework>"),
            "missing target framework: {csproj}"
        );
        assert!(
            csproj.contains("<PackageId>WeaveFFI</PackageId>"),
            "missing package id: {csproj}"
        );
        assert!(
            csproj.contains("<Version>0.1.0</Version>"),
            "missing version: {csproj}"
        );

        let nuspec_path = tmp.join("dotnet/WeaveFFI.nuspec");
        assert!(nuspec_path.exists(), ".nuspec file must exist");
        let nuspec = std::fs::read_to_string(&nuspec_path).unwrap();
        assert!(
            nuspec.contains("<id>WeaveFFI</id>"),
            "missing nuspec id: {nuspec}"
        );

        let readme_path = tmp.join("dotnet/README.md");
        assert!(readme_path.exists(), "README.md must exist");
        let readme = std::fs::read_to_string(&readme_path).unwrap();
        assert!(
            readme.contains("dotnet build"),
            "missing build instructions: {readme}"
        );
        assert!(
            readme.contains("dotnet pack"),
            "missing pack instructions: {readme}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn cs_type_mapping() {
        assert_eq!(cs_type(&TypeRef::I32), "int");
        assert_eq!(cs_type(&TypeRef::U32), "uint");
        assert_eq!(cs_type(&TypeRef::I64), "long");
        assert_eq!(cs_type(&TypeRef::F64), "double");
        assert_eq!(cs_type(&TypeRef::Bool), "bool");
        assert_eq!(cs_type(&TypeRef::StringUtf8), "string");
        assert_eq!(cs_type(&TypeRef::Handle), "ulong");
        assert_eq!(cs_type(&TypeRef::Bytes), "byte[]");
        assert_eq!(cs_type(&TypeRef::Record("Foo".into())), "Foo");
        assert_eq!(cs_type(&TypeRef::Enum("Bar".into())), "Bar");
        assert_eq!(cs_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "int?");
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "string?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Record("X".into())))),
            "X?"
        );
        assert_eq!(cs_type(&TypeRef::List(Box::new(TypeRef::I32))), "int[]");
        assert_eq!(
            cs_type(&TypeRef::Map(
                Box::new(TypeRef::StringUtf8),
                Box::new(TypeRef::I32)
            )),
            "Dictionary<string, int>"
        );
    }

    #[test]
    fn pinvoke_type_mapping() {
        assert_eq!(pinvoke_type(&TypeRef::I32), "int");
        // C `bool` is one byte, not int-widened.
        assert_eq!(pinvoke_type(&TypeRef::Bool), "byte");
        assert_eq!(pinvoke_type(&TypeRef::RichEnum("Foo".into())), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::StringUtf8), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Handle), "ulong");
        assert_eq!(pinvoke_type(&TypeRef::Bytes), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Record("Foo".into())), "IntPtr");
        assert_eq!(pinvoke_type(&TypeRef::Enum("Bar".into())), "int");
        assert_eq!(
            pinvoke_type(&TypeRef::Optional(Box::new(TypeRef::I32))),
            "IntPtr"
        );
    }

    #[test]
    fn simple_i32_function() {
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(cs.contains("namespace WeaveFFI"), "missing namespace: {cs}");
        assert!(cs.contains("DllImport"), "missing DllImport: {cs}");
        assert!(cs.contains("weaveffi_math_add"), "missing C symbol: {cs}");
        assert!(
            cs.contains("CallingConvention.Cdecl"),
            "missing Cdecl: {cs}"
        );
        assert!(
            cs.contains("public static int Add("),
            "missing wrapper method: {cs}"
        );
        assert!(
            cs.contains("WeaveFFIError.Check(err)"),
            "missing error check: {cs}"
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public static void Reset()"),
            "missing void wrapper: {cs}"
        );
        assert!(
            cs.contains("static extern void weaveffi_math_reset"),
            "missing void P/Invoke: {cs}"
        );
    }

    #[test]
    fn bool_uses_int_marshaling() {
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("flag ? 1 : 0"),
            "missing bool-to-int conversion: {cs}"
        );
        assert!(
            cs.contains("result != 0"),
            "missing int-to-bool conversion: {cs}"
        );
    }

    #[test]
    fn enum_generation() {
        let api = make_api(vec![Module {
            name: "paint".into(),
            functions: vec![Function {
                name: "mix".into(),
                params: vec![Param {
                    name: "a".into(),
                    ty: TypeRef::Enum("Color".into()),
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::Enum("Color".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(cs.contains("public enum Color"), "missing enum: {cs}");
        assert!(cs.contains("Red = 0"), "missing Red: {cs}");
        assert!(cs.contains("Green = 1"), "missing Green: {cs}");
        assert!(cs.contains("Blue = 2"), "missing Blue: {cs}");
        assert!(
            cs.contains("<summary>Primary colors</summary>"),
            "missing doc: {cs}"
        );
        assert!(cs.contains("(int)a"), "missing enum-to-int cast: {cs}");
        assert!(
            cs.contains("(Color)result"),
            "missing int-to-enum cast: {cs}"
        );
    }

    #[test]
    fn struct_is_sealed_value_class_with_doc() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact record".into()),
                fields: vec![
                    StructField {
                        name: "first_name".into(),
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
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public sealed class Contact"),
            "missing sealed value class: {cs}"
        );
        assert!(
            cs.contains("public Contact(string firstName, int age)"),
            "missing positional constructor: {cs}"
        );
        assert!(
            cs.contains("<summary>A contact record</summary>"),
            "missing doc: {cs}"
        );
        // Records hold no native resources: no handle, no IDisposable.
        assert!(
            !cs.contains("Contact : IDisposable") && !cs.contains("internal Contact(IntPtr"),
            "record must not wrap a handle: {cs}"
        );
    }

    #[test]
    fn struct_has_property_getters() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
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
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "active".into(),
                        ty: TypeRef::Bool,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "role".into(),
                        ty: TypeRef::Enum("Role".into()),
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public string FirstName { get; }"),
            "missing FirstName property: {cs}"
        );
        assert!(
            cs.contains("public int Age { get; }"),
            "missing Age property: {cs}"
        );
        assert!(
            cs.contains("public bool Active { get; }"),
            "missing Active property: {cs}"
        );
        assert!(
            cs.contains("public Role Role { get; }"),
            "missing Role property: {cs}"
        );
        // WriteTo serializes each field per the wire format; ReadFrom is the
        // exact inverse. No getter symbols cross the ABI anymore.
        assert!(
            cs.contains("writer.WriteString(FirstName);")
                && cs.contains("writer.WriteI32(Age);")
                && cs.contains("writer.WriteBool(Active);")
                && cs.contains("writer.WriteI32((int)Role);"),
            "missing field encodings: {cs}"
        );
        assert!(
            cs.contains("var fFirstName = reader.ReadString();")
                && cs.contains("var fAge = reader.ReadI32();")
                && cs.contains("var fActive = reader.ReadBool();")
                && cs.contains("var fRole = (Role)reader.ReadI32();"),
            "missing field decodings: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_contacts_Contact_get_first_name"),
            "getter symbols must be gone: {cs}"
        );
    }

    #[test]
    fn struct_has_no_dispose_or_finalizer() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
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
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // A record owns no native memory, so nothing to dispose or finalize.
        assert!(
            cs.contains("public sealed class Contact"),
            "missing record class: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_contacts_Contact_destroy"),
            "destroy symbol must be gone: {cs}"
        );
        assert!(!cs.contains("~Contact()"), "finalizer must be gone: {cs}");
    }

    #[test]
    fn struct_emits_no_pinvoke_declarations() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
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
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // Records cross the ABI only inside value buffers, so no per-record
        // P/Invoke declarations exist. The shared runtime imports remain.
        assert!(
            !cs.contains("weaveffi_contacts_Contact_"),
            "record symbols must be gone: {cs}"
        );
        assert!(
            cs.contains(
                "internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);"
            ),
            "missing free_bytes runtime import: {cs}"
        );
        assert!(
            cs.contains("internal static extern void weaveffi_error_clear(ref WeaveFFIError err);"),
            "missing error_clear runtime import: {cs}"
        );
    }

    #[test]
    fn string_function_uses_utf8() {
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
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("Marshal.PtrToStringUTF8(result)"),
            "missing PtrToStringUTF8: {cs}"
        );
        assert!(
            cs.contains("Marshal.StringToCoTaskMemUTF8(msg)"),
            "missing StringToCoTaskMemUTF8: {cs}"
        );
        assert!(
            cs.contains("Marshal.FreeCoTaskMem(msgPtr)"),
            "missing FreeCoTaskMem: {cs}"
        );
        assert!(
            cs.contains("weaveffi_free_string(result)"),
            "missing free_string: {cs}"
        );
    }

    #[test]
    fn safe_cs_name_escapes_keywords() {
        assert_eq!(safe_cs_name("string"), "@string");
        assert_eq!(safe_cs_name("class"), "@class");
        assert_eq!(safe_cs_name("return"), "@return");
        assert_eq!(safe_cs_name("name"), "name");
        assert_eq!(safe_cs_name("id"), "id");
    }

    #[test]
    fn native_methods_class() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add".into(),
            params: vec![],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("internal static class NativeMethods"),
            "missing NativeMethods: {cs}"
        );
        assert!(
            cs.contains("weaveffi_free_string"),
            "missing free_string P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_free_bytes"),
            "missing free_bytes P/Invoke: {cs}"
        );
    }

    #[test]
    fn pinvoke_has_error_param() {
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("ref WeaveFFIError err"),
            "missing error param in P/Invoke: {cs}"
        );
    }

    #[test]
    fn header_has_using_statements() {
        let api = make_api(vec![]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(cs.contains("using System;"), "missing System: {cs}");
        assert!(
            cs.contains("using System.Runtime.InteropServices;"),
            "missing InteropServices: {cs}"
        );
        assert!(
            cs.contains("using System.Collections.Generic;"),
            "missing Collections.Generic: {cs}"
        );
    }

    #[test]
    fn optional_types() {
        assert_eq!(cs_type(&TypeRef::Optional(Box::new(TypeRef::I32))), "int?");
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Bool))),
            "bool?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::StringUtf8))),
            "string?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Enum("Foo".into())))),
            "Foo?"
        );
        assert_eq!(
            cs_type(&TypeRef::Optional(Box::new(TypeRef::Record("Bar".into())))),
            "Bar?"
        );
    }

    #[test]
    fn struct_return_decodes_value_buffer() {
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
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public static Contact GetContact(ulong id)"),
            "missing method sig: {cs}"
        );
        // A record return arrives as a producer-owned value buffer: the
        // wrapper copies it, frees it, then decodes the copy.
        assert!(cs.contains("out var outLen"), "missing outLen slot: {cs}");
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
            "missing buffer release: {cs}"
        );
        assert!(
            cs.contains("var value = Contact.ReadFrom(valueReader);")
                && cs.contains("valueReader.ExpectEnd();")
                && cs.contains("return value;"),
            "missing buffer decode: {cs}"
        );
    }

    #[test]
    fn list_return_type() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "get_ids".into(),
                params: vec![],
                returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public static int[] GetIds()"),
            "missing list return method: {cs}"
        );
        assert!(cs.contains("out var outLen"), "missing outLen: {cs}");
        // The list crosses as one value buffer, not parallel arrays: the
        // wrapper copies it, frees it, then decodes count-prefixed elements.
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
            "missing value-buffer release: {cs}"
        );
        assert!(
            cs.contains("var valueCount = valueReader.ReadLen();")
                && cs.contains("var value = new int[valueCount];")
                && cs.contains("var valueItem = valueReader.ReadI32();"),
            "missing list decode loop: {cs}"
        );
    }

    #[test]
    fn map_return_type() {
        let api = make_api(vec![Module {
            name: "store".into(),
            functions: vec![Function {
                name: "get_scores".into(),
                params: vec![],
                returns: Some(TypeRef::Map(Box::new(TypeRef::I32), Box::new(TypeRef::F64))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public static Dictionary<int, double> GetScores()"),
            "missing map return: {cs}"
        );
        // Parallel key/value buffers are gone: the map crosses as one value
        // buffer decoded as count-prefixed alternating pairs.
        assert!(
            !cs.contains("out var outKeys") && !cs.contains("out var outValues"),
            "parallel buffers must be gone: {cs}"
        );
        assert!(
            cs.contains("var value = new Dictionary<int, double>(valueCount);")
                && cs.contains("var valueKey = valueReader.ReadI32();")
                && cs.contains("var valueVal = valueReader.ReadF64();")
                && cs.contains("value[valueKey] = valueVal;"),
            "missing map decode loop: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
            "missing value-buffer release: {cs}"
        );
    }

    #[test]
    fn struct_optional_string_field() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "email".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public string? Email { get; }"),
            "missing optional string property: {cs}"
        );
        // The optional encodes as a flag byte plus the value when present,
        // and decodes back into a nullable local.
        assert!(
            cs.contains("if (Email != null)")
                && cs.contains("writer.WriteOptionFlag(true);")
                && cs.contains("writer.WriteString(Email!);")
                && cs.contains("writer.WriteOptionFlag(false);"),
            "missing optional encode: {cs}"
        );
        assert!(
            cs.contains("string? fEmail = null;")
                && cs.contains("if (reader.ReadOptionFlag())")
                && cs.contains("var fEmailValue = reader.ReadString();"),
            "missing optional decode: {cs}"
        );
    }

    #[test]
    fn optional_string_param_marshalling() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "create".into(),
                params: vec![
                    Param {
                        name: "name".into(),
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
                ],
                returns: Some(TypeRef::Handle),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // Plain strings still cross as C strings.
        assert!(
            cs.contains("StringToCoTaskMemUTF8(name)"),
            "missing name marshal: {cs}"
        );
        assert!(
            cs.contains("FreeCoTaskMem(namePtr)"),
            "missing name cleanup: {cs}"
        );
        // The optional string is buffered: flag byte plus value, pinned and
        // passed as (ptr, len), then unpinned.
        assert!(
            cs.contains("var emailWriter = new WeaveFFIBufferWriter();")
                && cs.contains("if (email != null)")
                && cs.contains("emailWriter.WriteOptionFlag(true);")
                && cs.contains("emailWriter.WriteString(email!);"),
            "missing optional buffer encode: {cs}"
        );
        assert!(
            cs.contains("emailPin.AddrOfPinnedObject(), (UIntPtr)emailBuf.Length"),
            "missing (ptr, len) call args: {cs}"
        );
        assert!(cs.contains("emailPin.Free();"), "missing unpin: {cs}");
    }

    #[test]
    fn comprehensive_contacts_api() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
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
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
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
                    throws: false,
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
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_contacts_v2");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate(
                &api,
                out_dir,
                &DotnetConfig {
                    strip_module_prefix: true,
                    ..DotnetConfig::default()
                },
            )
            .unwrap();

        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

        assert!(cs.contains("public enum ContactType"));
        assert!(cs.contains("Personal = 0"));
        assert!(cs.contains("Work = 1"));

        // The record is a plain value class with typed properties and the
        // value-buffer pack/unpack pair; no handle or C symbols remain.
        assert!(cs.contains("public sealed class Contact"));
        assert!(cs.contains(
            "public Contact(long id, string firstName, string? email, ContactType contactType)"
        ));
        assert!(cs.contains("public long Id { get; }"));
        assert!(cs.contains("public string FirstName { get; }"));
        assert!(cs.contains("public string? Email { get; }"));
        assert!(cs.contains("public ContactType ContactType { get; }"));
        assert!(cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)"));
        assert!(cs.contains("internal static Contact ReadFrom(WeaveFFIBufferReader reader)"));
        assert!(!cs.contains("weaveffi_contacts_Contact_"));

        assert!(cs.contains("weaveffi_contacts_create_contact("));
        assert!(cs.contains("weaveffi_contacts_get_contact("));
        assert!(cs.contains("weaveffi_contacts_count_contacts("));

        assert!(cs.contains("public static class Contacts"));
        assert!(cs.contains("public static ulong CreateContact("));
        assert!(cs.contains("public static Contact GetContact("));
        assert!(cs.contains("public static int CountContacts("));

        assert!(cs.contains("internal static class NativeMethods"));
        assert!(cs.contains("CallingConvention.Cdecl"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dotnet_basic() {
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_basic");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate(
                &api,
                out_dir,
                &DotnetConfig {
                    strip_module_prefix: true,
                    ..DotnetConfig::default()
                },
            )
            .unwrap();
        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

        assert!(
            cs.contains("EntryPoint = \"weaveffi_math_add\""),
            "missing P/Invoke EntryPoint: {cs}"
        );
        assert!(
            cs.contains(
                "internal static extern int weaveffi_math_add(int a, int b, ref WeaveFFIError err)"
            ),
            "missing P/Invoke declaration: {cs}"
        );
        assert!(
            cs.contains("public static int Add(int a, int b)"),
            "missing wrapper method signature: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_math_add(a, b, ref err)"),
            "missing P/Invoke call in wrapper: {cs}"
        );
        assert!(
            cs.contains("WeaveFFIError.Check(err)"),
            "missing error check in wrapper: {cs}"
        );
        assert!(
            cs.contains("return result;"),
            "missing return statement: {cs}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn generate_dotnet_with_structs() {
        let api = make_api(vec![Module {
            name: "crm".into(),
            functions: vec![],
            structs: vec![StructDef {
                name: "Person".into(),
                doc: Some("A person record".into()),
                fields: vec![
                    StructField {
                        name: "full_name".into(),
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
                    StructField {
                        name: "score".into(),
                        ty: TypeRef::F64,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "active".into(),
                        ty: TypeRef::Bool,
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

        assert!(
            cs.contains("public sealed class Person"),
            "missing sealed value class: {cs}"
        );
        assert!(
            cs.contains("<summary>A person record</summary>"),
            "missing doc summary: {cs}"
        );
        assert!(
            cs.contains("public Person(string fullName, int age, double score, bool active)"),
            "missing positional constructor: {cs}"
        );

        assert!(
            cs.contains("public string FullName { get; }"),
            "missing FullName property: {cs}"
        );
        assert!(
            cs.contains("public int Age { get; }"),
            "missing Age property: {cs}"
        );
        assert!(
            cs.contains("public double Score { get; }"),
            "missing Score property: {cs}"
        );
        assert!(
            cs.contains("public bool Active { get; }"),
            "missing Active property: {cs}"
        );

        // The pack/unpack pair covers every field in declaration order.
        assert!(
            cs.contains("writer.WriteString(FullName);")
                && cs.contains("writer.WriteI32(Age);")
                && cs.contains("writer.WriteF64(Score);")
                && cs.contains("writer.WriteBool(Active);"),
            "missing field encodings: {cs}"
        );
        assert!(
            cs.contains("var fFullName = reader.ReadString();")
                && cs.contains("var fAge = reader.ReadI32();")
                && cs.contains("var fScore = reader.ReadF64();")
                && cs.contains("var fActive = reader.ReadBool();")
                && cs.contains("return new Person(fFullName, fAge, fScore, fActive);"),
            "missing field decodings: {cs}"
        );

        // No native lifecycle remains for records.
        assert!(
            !cs.contains("weaveffi_crm_Person_") && !cs.contains("~Person()"),
            "record C symbols must be gone: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_with_enums() {
        let api = make_api(vec![Module {
            name: "status".into(),
            functions: vec![Function {
                name: "get_status".into(),
                params: vec![],
                returns: Some(TypeRef::Enum("Priority".into())),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![EnumDef {
                name: "Priority".into(),
                doc: Some("Task priority levels".into()),
                variants: vec![
                    EnumVariant {
                        name: "Low".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Medium".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "High".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Critical".into(),
                        value: 3,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

        assert!(
            cs.contains("<summary>Task priority levels</summary>"),
            "missing enum doc: {cs}"
        );
        assert!(
            cs.contains("public enum Priority"),
            "missing enum declaration: {cs}"
        );
        assert!(cs.contains("Low = 0,"), "missing Low variant: {cs}");
        assert!(cs.contains("Medium = 1,"), "missing Medium variant: {cs}");
        assert!(cs.contains("High = 2,"), "missing High variant: {cs}");
        assert!(
            cs.contains("Critical = 3,"),
            "missing Critical variant: {cs}"
        );

        assert!(
            cs.contains("(Priority)result"),
            "missing enum return cast: {cs}"
        );
        assert!(
            cs.contains("public static Priority GetStatus()"),
            "missing wrapper returning enum: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_with_optionals() {
        let api = make_api(vec![Module {
            name: "config".into(),
            functions: vec![Function {
                name: "update".into(),
                params: vec![
                    Param {
                        name: "label".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        mutable: false,
                        doc: None,
                    },
                    Param {
                        name: "count".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        mutable: false,
                        doc: None,
                    },
                ],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::I64))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![StructDef {
                name: "Settings".into(),
                doc: None,
                fields: vec![
                    StructField {
                        name: "nickname".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::StringUtf8)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "max_retries".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "threshold".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::F64)),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "enabled".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Bool)),
                        doc: None,
                        default: None,
                    },
                ],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

        assert!(
            cs.contains("public static long? Update(string? label, int? count)"),
            "missing Nullable wrapper sig: {cs}"
        );

        // Optional parameters are buffered: a flag byte plus the value when
        // present, pinned and passed as (ptr, len).
        assert!(
            cs.contains("var labelWriter = new WeaveFFIBufferWriter();")
                && cs.contains("labelWriter.WriteString(label!);"),
            "missing optional string param encode: {cs}"
        );
        assert!(
            cs.contains("var countWriter = new WeaveFFIBufferWriter();")
                && cs.contains("countWriter.WriteI32(count.Value);"),
            "missing optional int param encode: {cs}"
        );

        // The optional return decodes from the freed-after-copy value buffer.
        assert!(
            cs.contains("long? value = null;")
                && cs.contains("if (valueReader.ReadOptionFlag())")
                && cs.contains("var valueValue = valueReader.ReadI64();"),
            "missing optional return decode: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
            "missing return buffer release: {cs}"
        );

        // Optional record fields become nullable properties with flag-byte
        // encodings; no boxed-scalar pointers remain.
        assert!(
            cs.contains("public string? Nickname { get; }"),
            "missing optional string property: {cs}"
        );
        assert!(
            cs.contains("public int? MaxRetries { get; }"),
            "missing optional int property: {cs}"
        );
        assert!(
            cs.contains("public double? Threshold { get; }"),
            "missing optional f64 property: {cs}"
        );
        assert!(
            cs.contains("public bool? Enabled { get; }"),
            "missing optional bool property: {cs}"
        );
        assert!(
            cs.contains("writer.WriteF64(Threshold.Value);")
                && cs.contains("writer.WriteBool(Enabled.Value);"),
            "missing optional field encodings: {cs}"
        );
        assert!(
            cs.contains("bool? fEnabled = null;") && cs.contains("double? fThreshold = null;"),
            "missing optional field decodings: {cs}"
        );
        assert!(
            !cs.contains("Marshal.ReadByte(ptr)"),
            "boxed-scalar pointers must be gone: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_with_lists() {
        let api = make_api(vec![Module {
            name: "data".into(),
            functions: vec![
                Function {
                    name: "get_ids".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I32))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_values".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::F64))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "get_timestamps".into(),
                    params: vec![],
                    returns: Some(TypeRef::List(Box::new(TypeRef::I64))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![StructDef {
                name: "Record".into(),
                doc: None,
                fields: vec![StructField {
                    name: "tags".into(),
                    ty: TypeRef::List(Box::new(TypeRef::I32)),
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

        assert!(
            cs.contains("public static int[] GetIds()"),
            "missing int[] return: {cs}"
        );
        assert!(
            cs.contains("public static double[] GetValues()"),
            "missing double[] return: {cs}"
        );
        assert!(
            cs.contains("public static long[] GetTimestamps()"),
            "missing long[] return: {cs}"
        );
        assert!(
            cs.contains("out var outLen"),
            "missing outLen parameter: {cs}"
        );
        // Each list decodes from its own value buffer: count prefix, typed
        // elements, then the producer buffer is released.
        assert!(
            cs.contains("var value = new int[valueCount];")
                && cs.contains("var value = new double[valueCount];")
                && cs.contains("var value = new long[valueCount];"),
            "missing typed element arrays: {cs}"
        );
        assert!(
            cs.contains("var valueItem = valueReader.ReadI32();")
                && cs.contains("var valueItem = valueReader.ReadF64();")
                && cs.contains("var valueItem = valueReader.ReadI64();"),
            "missing element decodes: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
            "missing value-buffer release: {cs}"
        );

        // A list-typed record field is a plain typed property with a
        // count-prefixed encoding.
        assert!(
            cs.contains("public int[] Tags { get; }"),
            "missing list property: {cs}"
        );
        assert!(
            cs.contains("writer.WriteLen(Tags.Length);"),
            "missing list field encode: {cs}"
        );
    }

    #[test]
    fn generate_dotnet_full_contacts() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            enums: vec![EnumDef {
                name: "ContactType".into(),
                doc: Some("Type of contact".into()),
                variants: vec![
                    EnumVariant {
                        name: "Personal".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Business".into(),
                        value: 1,
                        doc: None,
                        fields: vec![],
                    },
                    EnumVariant {
                        name: "Government".into(),
                        value: 2,
                        doc: None,
                        fields: vec![],
                    },
                ],
            }],
            callbacks: vec![],
            listeners: vec![],
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: Some("A contact entry".into()),
                fields: vec![
                    StructField {
                        name: "id".into(),
                        ty: TypeRef::Handle,
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
                        name: "age".into(),
                        ty: TypeRef::I32,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "active".into(),
                        ty: TypeRef::Bool,
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "contact_type".into(),
                        ty: TypeRef::Enum("ContactType".into()),
                        doc: None,
                        default: None,
                    },
                    StructField {
                        name: "scores".into(),
                        ty: TypeRef::List(Box::new(TypeRef::I32)),
                        doc: None,
                        default: None,
                    },
                ],
            }],
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
                    throws: false,
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
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "list_contacts".into(),
                    params: vec![Param {
                        name: "contact_type".into(),
                        ty: TypeRef::Optional(Box::new(TypeRef::Enum("ContactType".into()))),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_full_contacts");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator
            .generate(
                &api,
                out_dir,
                &DotnetConfig {
                    strip_module_prefix: true,
                    ..DotnetConfig::default()
                },
            )
            .unwrap();
        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();

        // Enum
        assert!(cs.contains("public enum ContactType"), "missing enum: {cs}");
        assert!(cs.contains("Personal = 0,"), "missing Personal: {cs}");
        assert!(cs.contains("Business = 1,"), "missing Business: {cs}");
        assert!(cs.contains("Government = 2,"), "missing Government: {cs}");
        assert!(
            cs.contains("<summary>Type of contact</summary>"),
            "missing enum doc: {cs}"
        );

        // Struct as a plain value class
        assert!(
            cs.contains("public sealed class Contact"),
            "missing sealed value class: {cs}"
        );
        assert!(
            cs.contains("<summary>A contact entry</summary>"),
            "missing struct doc: {cs}"
        );
        assert!(
            !cs.contains("~Contact()") && !cs.contains("weaveffi_contacts_Contact_"),
            "record lifecycle symbols must be gone: {cs}"
        );

        // Typed properties
        assert!(
            cs.contains("public ulong Id { get; }"),
            "missing Id property: {cs}"
        );
        assert!(
            cs.contains("public string FirstName { get; }"),
            "missing FirstName: {cs}"
        );
        assert!(
            cs.contains("public string LastName { get; }"),
            "missing LastName: {cs}"
        );
        assert!(
            cs.contains("public string? Email { get; }"),
            "missing optional Email: {cs}"
        );
        assert!(cs.contains("public int Age { get; }"), "missing Age: {cs}");
        assert!(
            cs.contains("public bool Active { get; }"),
            "missing Active: {cs}"
        );
        assert!(
            cs.contains("public ContactType ContactType { get; }"),
            "missing ContactType property: {cs}"
        );
        assert!(
            cs.contains("public int[] Scores { get; }"),
            "missing Scores list property: {cs}"
        );

        // Pack/unpack pair
        assert!(
            cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)")
                && cs.contains("internal static Contact ReadFrom(WeaveFFIBufferReader reader)"),
            "missing pack/unpack pair: {cs}"
        );

        // P/Invoke declarations
        assert!(
            cs.contains("weaveffi_contacts_create_contact("),
            "missing create_contact P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_get_contact("),
            "missing get_contact P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_list_contacts("),
            "missing list_contacts P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_delete_contact("),
            "missing delete_contact P/Invoke: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_count_contacts("),
            "missing count_contacts P/Invoke: {cs}"
        );

        // Wrapper class
        assert!(
            cs.contains("public static class Contacts"),
            "missing Contacts wrapper class: {cs}"
        );
        assert!(
            cs.contains("public static ulong CreateContact("),
            "missing CreateContact wrapper: {cs}"
        );
        assert!(
            cs.contains("public static Contact GetContact(ulong id)"),
            "missing GetContact wrapper: {cs}"
        );
        assert!(
            cs.contains("public static Contact[] ListContacts("),
            "missing ListContacts wrapper: {cs}"
        );
        assert!(
            cs.contains("public static void DeleteContact(ulong id)"),
            "missing DeleteContact wrapper: {cs}"
        );
        assert!(
            cs.contains("public static int CountContacts()"),
            "missing CountContacts wrapper: {cs}"
        );

        // Supporting output files
        assert!(
            tmp.join("dotnet/WeaveFFI.csproj").exists(),
            ".csproj must exist"
        );
        assert!(
            tmp.join("dotnet/WeaveFFI.nuspec").exists(),
            ".nuspec must exist"
        );
        assert!(tmp.join("dotnet/README.md").exists(), "README must exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dotnet_has_memory_helpers() {
        let api = make_api(vec![simple_module(vec![])]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("internal static class WeaveFFIHelpers"),
            "missing WeaveFFIHelpers class: {cs}"
        );
        assert!(
            cs.contains("internal static IntPtr StringToPtr(string? s)"),
            "missing StringToPtr: {cs}"
        );
        assert!(
            cs.contains("internal static string? PtrToString(IntPtr ptr)"),
            "missing PtrToString: {cs}"
        );
        assert!(
            cs.contains("internal static void FreePtr(IntPtr ptr)"),
            "missing FreePtr: {cs}"
        );
        assert!(
            cs.contains("Marshal.StringToCoTaskMemUTF8(s)"),
            "missing StringToCoTaskMemUTF8 in helper: {cs}"
        );
        assert!(
            cs.contains("Marshal.FreeCoTaskMem(ptr)"),
            "missing FreeCoTaskMem in helper: {cs}"
        );
    }

    #[test]
    fn dotnet_custom_namespace() {
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let config = DotnetConfig {
            namespace: Some("MyCompany.Bindings".into()),
            ..DotnetConfig::default()
        };

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_custom_ns");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir, &config).unwrap();

        let cs_path = tmp.join("dotnet/MyCompany.Bindings.cs");
        assert!(
            cs_path.exists(),
            ".cs file should use custom namespace name"
        );
        let cs = std::fs::read_to_string(&cs_path).unwrap();
        assert!(
            cs.contains("namespace MyCompany.Bindings"),
            "namespace should use custom name: {cs}"
        );

        let csproj_path = tmp.join("dotnet/MyCompany.Bindings.csproj");
        assert!(csproj_path.exists(), ".csproj should use custom namespace");
        let csproj = std::fs::read_to_string(&csproj_path).unwrap();
        assert!(
            csproj.contains("<PackageId>MyCompany.Bindings</PackageId>"),
            "PackageId should use custom namespace: {csproj}"
        );

        let nuspec_path = tmp.join("dotnet/MyCompany.Bindings.nuspec");
        assert!(nuspec_path.exists(), ".nuspec should use custom namespace");
        let nuspec = std::fs::read_to_string(&nuspec_path).unwrap();
        assert!(
            nuspec.contains("<id>MyCompany.Bindings</id>"),
            "nuspec id should use custom namespace: {nuspec}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dotnet_strip_module_prefix() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "create_contact".into(),
                params: vec![Param {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);

        // Stripping is the default: the per-module static class already
        // namespaces the method.
        let config = DotnetConfig::default();

        let tmp = std::env::temp_dir().join("weaveffi_test_dotnet_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let out_dir = Utf8Path::from_path(&tmp).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir, &config).unwrap();

        let cs = std::fs::read_to_string(tmp.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(
            cs.contains("CreateContact("),
            "stripped name should be CreateContact: {cs}"
        );
        assert!(
            !cs.contains("ContactsCreateContact("),
            "should not contain module-prefixed name: {cs}"
        );
        assert!(
            cs.contains("weaveffi_contacts_create_contact"),
            "C ABI call should still use full name: {cs}"
        );

        let no_strip = DotnetConfig {
            strip_module_prefix: false,
            ..DotnetConfig::default()
        };
        let tmp2 = std::env::temp_dir().join("weaveffi_test_dotnet_no_strip_prefix");
        let _ = std::fs::remove_dir_all(&tmp2);
        std::fs::create_dir_all(&tmp2).unwrap();
        let out_dir2 = Utf8Path::from_path(&tmp2).expect("valid UTF-8");

        DotnetGenerator.generate(&api, out_dir2, &no_strip).unwrap();

        let cs2 = std::fs::read_to_string(tmp2.join("dotnet/WeaveFFI.cs")).unwrap();
        assert!(
            cs2.contains("ContactsCreateContact("),
            "strip_module_prefix: false should restore module-prefixed names: {cs2}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[test]
    fn dotnet_deeply_nested_optional() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "data".into(),
                    ty: TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::Optional(
                        Box::new(TypeRef::Record("Contact".into())),
                    ))))),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("Contact?[]?"),
            "should contain deeply nested optional type: {cs}"
        );
    }

    #[test]
    fn dotnet_map_of_lists() {
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
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("Dictionary<string, int[]>"),
            "should contain map of lists type: {cs}"
        );
    }

    #[test]
    fn dotnet_enum_keyed_map() {
        let api = make_api(vec![Module {
            name: "edge".into(),
            functions: vec![Function {
                name: "process".into(),
                params: vec![Param {
                    name: "contacts".into(),
                    ty: TypeRef::Map(
                        Box::new(TypeRef::Enum("Color".into())),
                        Box::new(TypeRef::Record("Contact".into())),
                    ),
                    mutable: false,
                    doc: None,
                }],
                returns: None,
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            }],
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
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("Dictionary<Color, Contact>"),
            "should contain enum-keyed map type: {cs}"
        );
    }

    #[test]
    fn dotnet_typed_handle_type() {
        let api = Api {
            version: "0.6.0".into(),
            modules: vec![Module {
                name: "contacts".into(),
                functions: vec![Function {
                    name: "get_info".into(),
                    params: vec![Param {
                        name: "contact".into(),
                        ty: TypeRef::TypedHandle("Contact".into()),
                        mutable: false,
                        doc: None,
                    }],
                    returns: None,
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                }],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                interfaces: vec![],
                modules: vec![],
            }],
            generators: None,
            package: None,
        };
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // A typed handle renders as a dedicated readonly wrapper struct, not
        // a bare ulong and not the record class.
        assert!(
            cs.contains("ContactHandle contact"),
            "TypedHandle should use the wrapper struct: {cs}"
        );
        assert!(
            cs.contains("public readonly struct ContactHandle"),
            "missing handle wrapper struct: {cs}"
        );
        assert!(
            cs.contains("contact.Raw"),
            "wrapper should pass the raw pointer: {cs}"
        );
    }

    #[test]
    fn dotnet_no_double_free_on_error() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
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
                r#async: false,
                cancellable: false,
                throws: false,
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
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("StringToCoTaskMemUTF8"),
            "string param should be marshalled to unmanaged memory: {cs}"
        );
        assert!(
            cs.contains("finally") && cs.contains("FreeCoTaskMem"),
            "marshalled string should be freed in finally (no double-free of managed string): {cs}"
        );
        let find = cs.find("FindContact").expect("FindContact wrapper");
        let slice = &cs[find..];
        let check_rel = slice
            .find("WeaveFFIError.Check(err)")
            .expect("WeaveFFIError.Check in FindContact");
        let free_rel = slice
            .find("NativeMethods.weaveffi_free_bytes(result, outLen);")
            .expect("value-buffer release in FindContact");
        let decode_rel = slice
            .find("Contact.ReadFrom(valueReader)")
            .expect("Contact.ReadFrom in FindContact");
        assert!(
            check_rel < free_rel && free_rel < decode_rel,
            "error must be checked, the buffer freed once, then decoded: {cs}"
        );
        // The record is a value type: nothing to dispose, so no double-free
        // hazard on the return path.
        assert!(
            !cs.contains("Contact : IDisposable") && !cs.contains("~Contact()"),
            "record must not be disposable: {cs}"
        );
    }

    #[test]
    fn dotnet_null_check_on_optional_return() {
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
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Record(
                    "Contact".into(),
                )))),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
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
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // The optional record decodes from the value buffer: a flag byte
        // gates the nested record read, and absent means null.
        assert!(
            cs.contains("public static Contact? FindContact(int id)"),
            "missing nullable wrapper sig: {cs}"
        );
        assert!(
            cs.contains("Contact? value = null;")
                && cs.contains("if (valueReader.ReadOptionFlag())")
                && cs.contains("var valueValue = Contact.ReadFrom(valueReader);"),
            "optional record return should decode via flag byte: {cs}"
        );
    }

    #[test]
    fn dotnet_async_returns_task() {
        let api = make_api(vec![Module {
            name: "tasks".into(),
            functions: vec![Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("async Task<"),
            "missing async Task< in signature: {cs}"
        );
    }

    #[test]
    fn dotnet_async_uses_tcs() {
        let api = make_api(vec![Module {
            name: "tasks".into(),
            functions: vec![Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("TaskCompletionSource"),
            "missing TaskCompletionSource: {cs}"
        );
    }

    /// `GCHandle.Alloc(callback, GCHandleType.Normal)` (the .NET equivalent
    /// of pinning the delegate so the GC won't reclaim it while the C side
    /// owns a function pointer to it) must be balanced by exactly one
    /// `GCHandle.FromIntPtr(context).Free()` in the C callback after the
    /// `TaskCompletionSource` is resolved.
    #[test]
    fn dotnet_async_pins_callback_for_lifetime() {
        let api = make_api(vec![Module {
            name: "tasks".into(),
            functions: vec![Function {
                name: "run".into(),
                params: vec![Param {
                    name: "id".into(),
                    ty: TypeRef::I32,
                    mutable: false,
                    doc: None,
                }],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("GCHandle.Alloc(callback, GCHandleType.Normal)"),
            "missing GCHandle.Alloc(..., Normal): {cs}"
        );
        assert!(
            cs.contains("GCHandle.ToIntPtr(gcHandle)"),
            "GCHandle must be passed as the C context: {cs}"
        );
        assert!(
            cs.contains("GCHandle.FromIntPtr(context).Free()"),
            "missing GCHandle.Free in callback: {cs}"
        );
    }

    /// A module with one async function per given return type, named `run0`,
    /// `run1`, ... in order, plus a `Contact` record for object results.
    fn async_api(returns: Vec<Option<TypeRef>>) -> Api {
        let functions = returns
            .into_iter()
            .enumerate()
            .map(|(i, ret)| Function {
                name: format!("run{i}"),
                params: vec![],
                returns: ret,
                doc: None,
                r#async: true,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            })
            .collect();
        make_api(vec![Module {
            name: "tasks".into(),
            functions,
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "id".into(),
                    ty: TypeRef::Handle,
                    doc: None,
                    default: None,
                }],
            }],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }])
    }

    /// Async result buffers are borrowed for the callback's duration
    /// (`AsyncProtocol` clause 2): strings and bytes are deep-copied inside
    /// the callback and never freed by the consumer.
    #[test]
    fn dotnet_async_borrowed_results_copied_never_freed() {
        let cs = render_csharp(
            &async_api(vec![
                Some(TypeRef::StringUtf8),
                Some(TypeRef::Bytes),
                Some(TypeRef::Optional(Box::new(TypeRef::I64))),
            ]),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // String result: copied, not freed.
        assert!(
            cs.contains("tcs.SetResult(Marshal.PtrToStringUTF8(result) ?? \"\");"),
            "async string result must copy: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_free_string(result)"),
            "async string result must not be freed by the consumer: {cs}"
        );
        // Bytes result: copied via the (result, resultLen) pair, not freed.
        assert!(
            cs.contains("Marshal.Copy(result, arr, 0, (int)resultLen);"),
            "async bytes result must copy: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_free_bytes(result"),
            "async bytes result must not be freed by the consumer: {cs}"
        );
        // Buffered optional result: the borrowed buffer is copied and
        // decoded inside the callback, never freed by the consumer.
        assert!(
            cs.contains("Marshal.Copy(result, resultBuf, 0, (int)resultLen);")
                && cs.contains("long? value = null;")
                && cs.contains("var valueValue = valueReader.ReadI64();")
                && cs.contains("tcs.SetResult(value);"),
            "async optional result must decode the borrowed buffer: {cs}"
        );
    }

    /// Record, list, and map async results all arrive as one borrowed
    /// `(result, resultLen)` value buffer: the callback copies and decodes
    /// it before completing the task, and never frees it.
    #[test]
    fn dotnet_async_buffered_results_decoded() {
        let cs = render_csharp(
            &async_api(vec![
                Some(TypeRef::Record("Contact".into())),
                Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
                Some(TypeRef::List(Box::new(TypeRef::Record("Contact".into())))),
                Some(TypeRef::Map(
                    Box::new(TypeRef::StringUtf8),
                    Box::new(TypeRef::I32),
                )),
            ]),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // Every buffered result arrives through the (result, resultLen) pair.
        assert!(
            cs.contains("IntPtr result, UIntPtr resultLen"),
            "async buffered delegate must carry the length slot: {cs}"
        );
        // Record result decoded from the borrowed copy.
        assert!(
            cs.contains("var value = Contact.ReadFrom(valueReader);")
                && cs.contains("tcs.SetResult(value);"),
            "async record result must decode: {cs}"
        );
        // List elements decode in place: strings copy, records recurse.
        assert!(
            cs.contains("var valueItem = valueReader.ReadString();"),
            "async string list elements must decode: {cs}"
        );
        assert!(
            cs.contains("var valueItem = Contact.ReadFrom(valueReader);"),
            "async record list elements must decode: {cs}"
        );
        // Map results decode from the same single buffer as alternating
        // key/value pairs; no parallel buffers remain.
        assert!(
            cs.contains("var value = new Dictionary<string, int>(valueCount);")
                && cs.contains("var valueKey = valueReader.ReadString();")
                && cs.contains("var valueVal = valueReader.ReadI32();"),
            "async map result must decode pairs: {cs}"
        );
        assert!(
            !cs.contains("resultKeys") && !cs.contains("resultValues"),
            "parallel map buffers must be gone: {cs}"
        );
        // No release calls anywhere in this API: every native buffer here is
        // an async result, borrowed for the callback's duration.
        assert!(
            !cs.contains("NativeMethods.weaveffi_free_string(")
                && !cs.contains("NativeMethods.weaveffi_free_bytes("),
            "async result buffers are borrowed and must not be freed: {cs}"
        );
    }

    /// The iterator contract (`IteratorProtocol`): the sequence streams
    /// through a single `yield return` enumerator (one C `next` per
    /// `MoveNext`), frees each string element after conversion, destroys the
    /// native iterator exactly once from the compiler-generated `finally`,
    /// and refuses a second enumeration instead of double-destroying.
    #[test]
    fn iterator_streams_lazily_and_destroys_once() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // The single-use wrapper class and the wrapped return.
        assert!(
            cs.contains("internal sealed class WeaveFFIOnceEnumerable<T> : IEnumerable<T>"),
            "once-enumerable class missing: {cs}"
        );
        assert!(
            cs.contains("return new WeaveFFIOnceEnumerable<string>(EnumerateListKeys(iter));"),
            "iterator wrapper must return the once-enumerable: {cs}"
        );
        assert!(
            cs.contains("this sequence can be enumerated only once"),
            "second enumeration must throw: {cs}"
        );
        // One C next call per MoveNext, inside a lazy yield-return method.
        assert_eq!(
            cs.matches(
                "weaveffi_kv_Store_ListKeysIterator_next(iter, out var out_item, ref iterErr)"
            )
            .count(),
            1,
            "exactly one next call site expected: {cs}"
        );
        assert!(
            cs.contains("yield return item;"),
            "enumerator must stream lazily: {cs}"
        );
        // Each yielded string is freed after conversion (ElemFree::String).
        assert!(
            cs.contains("NativeMethods.weaveffi_free_string(out_item);"),
            "string elements must be freed: {cs}"
        );
        // Destroy exactly once, from the enumerator's finally (which C#'s
        // foreach reaches through Dispose() on early abandonment too).
        assert_eq!(
            cs.matches("NativeMethods.weaveffi_kv_Store_ListKeysIterator_destroy(iter);")
                .count(),
            1,
            "exactly one destroy call site expected: {cs}"
        );
        assert!(cs.contains("finally"), "destroy must run in finally: {cs}");
    }

    /// A list-of-strings return arrives as one value buffer: the strings
    /// decode from the copy and the producer buffer is released exactly once
    /// with `weaveffi_free_bytes`; no per-element frees remain.
    #[test]
    fn string_list_return_decodes_and_frees_buffer_once() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "names".into(),
            params: vec![],
            returns: Some(TypeRef::List(Box::new(TypeRef::StringUtf8))),
            doc: None,
            r#async: false,
            cancellable: false,
            throws: false,
            deprecated: None,
            since: None,
        }])]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("var value = new string[valueCount];")
                && cs.contains("var valueItem = valueReader.ReadString();"),
            "string elements must decode from the buffer: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_free_bytes(result, outLen);"),
            "value buffer must be released: {cs}"
        );
        let names = cs.find("static string[] Names()").expect("Names wrapper");
        assert!(
            !cs[names..].contains("weaveffi_free_string("),
            "no per-element frees may remain in the wrapper: {cs}"
        );
    }

    #[test]
    fn dotnet_nested_module_output() {
        let api = make_api(vec![Module {
            name: "parent".to_string(),
            functions: vec![Function {
                name: "outer_fn".to_string(),
                params: vec![],
                returns: Some(TypeRef::I32),
                doc: None,
                r#async: false,
                cancellable: false,
                throws: false,
                deprecated: None,
                since: None,
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![Module {
                name: "child".to_string(),
                functions: vec![Function {
                    name: "inner_fn".to_string(),
                    params: vec![],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                interfaces: vec![],
                modules: vec![],
            }],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public static class Parent"),
            "top-level wrapper class missing: {cs}"
        );
        assert!(
            cs.contains("public static class ParentChild"),
            "submodule wrapper class must be flattened to its full path: {cs}"
        );
        assert!(
            cs.contains("weaveffi_parent_outer_fn"),
            "parent P/Invoke missing: {cs}"
        );
        assert!(
            cs.contains("weaveffi_parent_child_inner_fn"),
            "nested child P/Invoke missing: {cs}"
        );
    }

    #[test]
    fn deprecated_function_generates_annotation() {
        let api = make_api(vec![simple_module(vec![Function {
            name: "add_old".into(),
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
            throws: false,
            deprecated: Some("Use AddV2 instead".into()),
            since: Some("0.1.0".into()),
        }])]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("[Obsolete(\"Use AddV2 instead\")]"),
            "missing Obsolete attribute: {cs}"
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
                r#async: false,
                cancellable: false,
                throws: false,
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
            interfaces: vec![],
            modules: vec![],
        }])
    }

    #[test]
    fn dotnet_emits_doc_on_function() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("/// <summary>Performs a thing.</summary>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_emits_doc_on_struct() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("/// <summary>An item we track.</summary>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_emits_doc_on_enum_variant() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(cs.contains("/// <summary>Kind of item.</summary>"), "{cs}");
        assert!(cs.contains("/// <summary>A small one</summary>"), "{cs}");
    }

    #[test]
    fn dotnet_emits_doc_on_field() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(cs.contains("/// <summary>Stable id</summary>"), "{cs}");
    }

    #[test]
    fn dotnet_emits_doc_on_param() {
        let cs = render_csharp(
            &doc_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("/// <param name=\"x\">the input value</param>"),
            "{cs}"
        );
    }

    #[test]
    fn dotnet_custom_prefix_threads_to_user_symbols() {
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
            throws: false,
            deprecated: None,
            since: None,
        }])]);

        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "myffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );

        // User symbols pick up the configured ABI prefix...
        assert!(
            cs.contains("myffi_math_add"),
            "user symbol must honor the custom prefix: {cs}"
        );
        assert!(
            !cs.contains("weaveffi_math_add"),
            "user symbol must not retain the default prefix: {cs}"
        );
        // ...while runtime ABI helpers stay literally `weaveffi_*`.
        assert!(
            cs.contains("weaveffi_free_string"),
            "runtime ABI helper must stay literal: {cs}"
        );
    }

    fn shapes_api() -> Api {
        let shape = EnumDef {
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
        };
        let channel = EnumDef {
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
        };
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
                    r#async: false,
                    cancellable: false,
                    throws: false,
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
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
                Function {
                    name: "sum_bytes".into(),
                    params: vec![Param {
                        name: "values".into(),
                        ty: TypeRef::List(Box::new(TypeRef::U8)),
                        mutable: false,
                        doc: None,
                    }],
                    returns: Some(TypeRef::U64),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    throws: false,
                    deprecated: None,
                    since: None,
                },
            ],
            structs: vec![],
            enums: vec![shape, channel],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }])
    }

    #[test]
    fn rich_enum_generates_sum_type() {
        let cs = render_csharp(
            &shapes_api(),
            "Shapes",
            false,
            "weaveffi",
            "shapes.yml",
            "Shapes.cs",
        );

        // Rich enum becomes an abstract sum type, not a C# enum and not a
        // disposable handle wrapper.
        assert!(
            cs.contains("public abstract class Shape"),
            "rich enum must be an abstract class: {cs}"
        );
        assert!(
            !cs.contains("public enum Shape"),
            "rich enum must not be a plain enum: {cs}"
        );
        assert!(
            !cs.contains("Shape : IDisposable"),
            "rich enum must not be disposable: {cs}"
        );
        // Plain enum is still a value enum.
        assert!(
            cs.contains("public enum Channel"),
            "plain enum must stay an enum: {cs}"
        );

        // One nested sealed class per variant, with typed properties and
        // positional constructors instead of factories and getters.
        assert!(
            cs.contains("public sealed class Empty : Shape"),
            "Empty variant class: {cs}"
        );
        assert!(
            cs.contains("public sealed class Circle : Shape")
                && cs.contains("public Circle(double radius)")
                && cs.contains("public double Radius { get; }"),
            "Circle variant class: {cs}"
        );
        assert!(
            cs.contains("public sealed class Rectangle : Shape")
                && cs.contains("public Rectangle(float width, float height)")
                && cs.contains("public float Width { get; }")
                && cs.contains("public float Height { get; }"),
            "Rectangle variant class: {cs}"
        );
        assert!(
            cs.contains("public sealed class Labeled : Shape")
                && cs.contains("public Labeled(string label, byte count)")
                && cs.contains("public string Label { get; }")
                && cs.contains("public byte Count { get; }"),
            "Labeled variant class: {cs}"
        );

        // The pack pair writes the i32 tag then the active variant's fields.
        assert!(
            cs.contains("internal void WriteTo(WeaveFFIBufferWriter writer)")
                && cs.contains("case Circle v:")
                && cs.contains("writer.WriteI32(1);")
                && cs.contains("writer.WriteF64(v.Radius);"),
            "tag-dispatched encode: {cs}"
        );
        assert!(
            cs.contains("internal static Shape ReadFrom(WeaveFFIBufferReader reader)")
                && cs.contains("var tag = reader.ReadI32();")
                && cs.contains("return new Empty();")
                && cs.contains("return new Labeled(fLabel, fCount);"),
            "tag-dispatched decode: {cs}"
        );

        // Rich enums declare no C symbols at all.
        assert!(
            !cs.contains("weaveffi_shapes_Shape_"),
            "rich enum C symbols must be gone: {cs}"
        );

        // Functions taking the enum pack it into a pinned value buffer and
        // pass (ptr, len); returns decode from the freed-after-copy buffer.
        assert!(
            cs.contains("public static string ShapesDescribe(Shape shape)")
                && cs.contains("shape.WriteTo(shapeWriter);")
                && cs.contains(
                    "weaveffi_shapes_describe(shapePin.AddrOfPinnedObject(), (UIntPtr)shapeBuf.Length, ref err)"
                ),
            "describe via buffered param: {cs}"
        );
        assert!(
            cs.contains("public static Shape ShapesScale(Shape shape, double factor)")
                && cs.contains("var value = Shape.ReadFrom(valueReader);"),
            "scale via buffered return: {cs}"
        );
        // Numerics smoke: list<u8> in, u64 out (plain function path).
        assert!(
            cs.contains("public static ulong ShapesSumBytes(byte[] values)"),
            "sum_bytes wrapper: {cs}"
        );
    }

    /// A `kv` module exercising the 0.5.0 surface: a declared error domain, a
    /// `Store` interface (real ctor, named factory, sync/iterator/async
    /// methods, a static), and free functions with mixed `throws`.
    fn kv_api() -> Api {
        use weaveffi_ir::ir::{ErrorCode, ErrorDomain, InterfaceDef};
        let f = |name: &str,
                 params: Vec<Param>,
                 returns: Option<TypeRef>,
                 throws: bool,
                 is_async: bool,
                 cancellable: bool| Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            throws,
            r#async: is_async,
            cancellable,
            deprecated: None,
            since: None,
        };
        let p = |name: &str, ty: TypeRef| Param {
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
        };
        make_api(vec![Module {
            name: "kv".into(),
            functions: vec![
                f(
                    "lookup_store",
                    vec![p("store", TypeRef::Interface("Store".into()))],
                    Some(TypeRef::U64),
                    true,
                    false,
                    false,
                ),
                f("ping", vec![], Some(TypeRef::Bool), false, false, false),
            ],
            interfaces: vec![InterfaceDef {
                name: "Store".into(),
                doc: Some("A key-value store.".into()),
                constructors: vec![
                    f(
                        "new",
                        vec![p("path", TypeRef::StringUtf8)],
                        None,
                        true,
                        false,
                        false,
                    ),
                    f(
                        "open_readonly",
                        vec![p("path", TypeRef::StringUtf8)],
                        None,
                        false,
                        false,
                        false,
                    ),
                ],
                methods: vec![
                    f(
                        "get",
                        vec![p("store_key", TypeRef::StringUtf8)],
                        Some(TypeRef::StringUtf8),
                        true,
                        false,
                        false,
                    ),
                    f("count", vec![], Some(TypeRef::U64), false, false, false),
                    f(
                        "list_keys",
                        vec![],
                        Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                        true,
                        false,
                        false,
                    ),
                    f("compact", vec![], None, true, true, true),
                ],
                statics: vec![f(
                    "default_capacity",
                    vec![],
                    Some(TypeRef::U32),
                    false,
                    false,
                    false,
                )],
            }],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: Some(ErrorDomain {
                name: "KvError".into(),
                codes: vec![
                    ErrorCode {
                        name: "KEY_NOT_FOUND".into(),
                        code: 1001,
                        message: "Key not found".into(),
                        doc: None,
                        // Structured payload: the missing key and the attempt
                        // count, exercising the payload decode in FromCode.
                        fields: vec![
                            StructField {
                                name: "key".into(),
                                ty: TypeRef::StringUtf8,
                                doc: None,
                                default: None,
                            },
                            StructField {
                                name: "attempts".into(),
                                ty: TypeRef::I32,
                                doc: None,
                                default: None,
                            },
                        ],
                    },
                    ErrorCode {
                        name: "IO_ERROR".into(),
                        code: 1004,
                        message: "I/O failure".into(),
                        doc: Some("Underlying storage failed.".into()),
                        fields: vec![],
                    },
                ],
            }),
            modules: vec![],
        }])
    }

    #[test]
    fn typed_exception_rendering() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // The domain exception extends the generic brand exception and drops
        // the doubled suffix (KvException, not KvErrorException).
        assert!(
            cs.contains("public class KvException : WeaveFFIException"),
            "typed exception class missing: {cs}"
        );
        assert!(
            !cs.contains("KvErrorException"),
            "doubled suffix must not appear: {cs}"
        );
        // Codes surface as PascalCase constants with their ABI values.
        assert!(
            cs.contains("public const int KeyNotFound = 1001;"),
            "code constant missing: {cs}"
        );
        assert!(
            cs.contains("public const int IoError = 1004;"),
            "code constant missing: {cs}"
        );
        // FromCode maps known codes to the typed exception and falls back to
        // the generic exception for unknown codes, with the default message
        // filling an empty slot message.
        assert!(
            cs.contains(
                "internal static WeaveFFIException FromCode(int code, string message, byte[]? payload)"
            ),
            "FromCode factory missing: {cs}"
        );
        // A code with payload fields decodes them into the exception's Data
        // dictionary in declaration order.
        assert!(
            cs.contains("case KeyNotFound:")
                && cs.contains(
                    "var ex = new KvException(code, string.IsNullOrEmpty(message) ? \"Key not found\" : message);"
                ),
            "typed mapping missing: {cs}"
        );
        assert!(
            cs.contains("var reader = new WeaveFFIBufferReader(payload);")
                && cs.contains("var fKey = reader.ReadString();")
                && cs.contains("ex.Data[\"key\"] = fKey;")
                && cs.contains("var fAttempts = reader.ReadI32();")
                && cs.contains("ex.Data[\"attempts\"] = fAttempts;"),
            "payload field decode missing: {cs}"
        );
        // A code without fields maps directly.
        assert!(
            cs.contains("case IoError:")
                && cs.contains(
                    "return new KvException(code, string.IsNullOrEmpty(message) ? \"I/O failure\" : message);"
                ),
            "fieldless mapping missing: {cs}"
        );
        assert!(
            cs.contains("default:") && cs.contains("return new WeaveFFIException(code, message);"),
            "generic fallback missing: {cs}"
        );
        // The error-check helper gains a per-domain variant that copies the
        // payload, clears the slot, and throws through FromCode.
        assert!(
            cs.contains("internal static void CheckKv(WeaveFFIError err)")
                && cs.contains("var payload = CopyPayload(err);")
                && cs.contains("NativeMethods.weaveffi_error_clear(ref err);")
                && cs.contains("throw KvException.FromCode(code, msg, payload);"),
            "per-domain check missing: {cs}"
        );
    }

    #[test]
    fn interface_class_rendering() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // Opaque-handle wrapper following the struct pattern.
        assert!(
            cs.contains("public class Store : IDisposable"),
            "interface class missing: {cs}"
        );
        assert!(
            cs.contains("internal Store(IntPtr handle)"),
            "internal handle ctor missing: {cs}"
        );
        assert!(
            cs.contains("internal IntPtr Handle => _handle;"),
            "Handle accessor missing: {cs}"
        );
        // The `new` constructor is a real C# constructor assigning _handle.
        assert!(
            cs.contains("public Store(string path)"),
            "real constructor missing: {cs}"
        );
        assert!(
            cs.contains("_handle = result;"),
            "constructor must assign the checked handle: {cs}"
        );
        // Other constructors become static factories wrapping the pointer.
        assert!(
            cs.contains("public static Store OpenReadonly(string path)"),
            "factory missing: {cs}"
        );
        assert!(
            cs.contains("return new Store(result);"),
            "factory must wrap the owned pointer: {cs}"
        );
        // Instance method: non-static, handle as the leading argument.
        assert!(
            cs.contains("public string Get(string storeKey)"),
            "instance method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_Store_get(_handle, storeKeyPtr, ref err);"),
            "method must pass _handle first: {cs}"
        );
        // Static member is a plain static method.
        assert!(
            cs.contains("public static uint DefaultCapacity()"),
            "interface static missing: {cs}"
        );
        // Iterator method surfaces as IEnumerable with the handle prepended.
        assert!(
            cs.contains("public IEnumerable<string> ListKeys()"),
            "iterator method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_Store_list_keys(_handle, ref err);"),
            "iterator launch must pass _handle: {cs}"
        );
        // Async method returns Task and passes the handle to the launcher.
        assert!(
            cs.contains("public async Task Compact()"),
            "async method missing: {cs}"
        );
        assert!(
            cs.contains(
                "NativeMethods.weaveffi_kv_Store_compact_async(_handle, IntPtr.Zero, callback, ctx);"
            ),
            "async launch must pass _handle and the cancel slot: {cs}"
        );
        // Disposal: Dispose + finalizer calling the destroy symbol once.
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_Store_destroy(_handle);")
                && cs.contains("~Store()"),
            "dispose/finalizer missing: {cs}"
        );
        // Externs: destroy plus shape-matched member declarations with the
        // implicit self slot on instance members.
        for sym in [
            "internal static extern void weaveffi_kv_Store_destroy(IntPtr self);",
            "internal static extern IntPtr weaveffi_kv_Store_new(IntPtr path, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_kv_Store_open_readonly(IntPtr path, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_kv_Store_get(IntPtr self, IntPtr store_key, ref WeaveFFIError err);",
            "internal static extern ulong weaveffi_kv_Store_count(IntPtr self, ref WeaveFFIError err);",
            "internal static extern IntPtr weaveffi_kv_Store_list_keys(IntPtr self, ref WeaveFFIError out_err);",
            "internal static extern int weaveffi_kv_Store_ListKeysIterator_next(",
            "internal static extern void weaveffi_kv_Store_ListKeysIterator_destroy(IntPtr iter);",
            "internal static extern void weaveffi_kv_Store_compact_async(IntPtr self, IntPtr cancel_token, AsyncCb_weaveffi_kv_Store_compact callback, IntPtr context);",
            "internal static extern uint weaveffi_kv_Store_default_capacity(ref WeaveFFIError err);",
        ] {
            assert!(cs.contains(sym), "missing P/Invoke `{sym}`: {cs}");
        }
        // No stray sync extern for the async-only member.
        assert!(
            !cs.contains("weaveffi_kv_Store_compact(IntPtr self, ref WeaveFFIError err)"),
            "async member must not declare a sync extern: {cs}"
        );
        // Interface parameters borrow the handle.
        assert!(
            cs.contains("public static ulong LookupStore(Store store)"),
            "interface param wrapper missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_kv_lookup_store(store.Handle, ref err);"),
            "interface param must pass obj.Handle: {cs}"
        );
    }

    /// Extract the body of the method whose signature contains `sig`, up to
    /// the next method boundary (a blank line followed by a doc comment or
    /// declaration at the same depth). Good enough to scope error-check
    /// assertions to one wrapper.
    fn method_slice<'a>(cs: &'a str, sig: &str) -> &'a str {
        let start = cs
            .find(sig)
            .unwrap_or_else(|| panic!("signature `{sig}` not found in: {cs}"));
        let rest = &cs[start..];
        let end = rest.find("\n\n").unwrap_or(rest.len());
        &rest[..end]
    }

    #[test]
    fn throws_split_typed_vs_generic() {
        let cs = render_csharp(
            &kv_api(),
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        // throws == true: sync method reports through the typed check.
        let get = method_slice(&cs, "public string Get(string storeKey)");
        assert!(
            get.contains("WeaveFFIError.CheckKv(err);"),
            "throwing method must use the typed check: {get}"
        );
        // throws == false: generic check only (panics/marshalling).
        let count = method_slice(&cs, "public ulong Count()");
        assert!(
            count.contains("WeaveFFIError.Check(err);") && !count.contains("CheckKv"),
            "non-throwing method must use the generic check: {count}"
        );
        // Free functions follow the same split.
        let lookup = method_slice(&cs, "public static ulong LookupStore(Store store)");
        assert!(
            lookup.contains("WeaveFFIError.CheckKv(err);"),
            "throwing free function must use the typed check: {lookup}"
        );
        let ping = method_slice(&cs, "public static bool Ping()");
        assert!(
            ping.contains("WeaveFFIError.Check(err);") && !ping.contains("CheckKv"),
            "non-throwing free function must use the generic check: {ping}"
        );
        // The real constructor throws the typed exception too.
        let ctor = method_slice(&cs, "public Store(string path)");
        assert!(
            ctor.contains("WeaveFFIError.CheckKv(err);"),
            "throwing constructor must use the typed check: {ctor}"
        );
        // Async completion faults the task with the typed exception; the
        // iterator's next-checks are typed as well.
        assert!(
            cs.contains("var payload = WeaveFFIError.CopyPayload(wErr);")
                && cs.contains("tcs.SetException(KvException.FromCode(wErr.Code, msg, payload));"),
            "async throws must fault with the typed exception and payload: {cs}"
        );
        let iter = method_slice(&cs, "private static IEnumerator<string> EnumerateListKeys(");
        assert!(
            iter.contains("WeaveFFIError.CheckKv(iterErr);"),
            "iterator next-check must be typed: {iter}"
        );
        // Throwing wrappers document the exception type.
        assert!(
            cs.contains(
                "/// <exception cref=\"KvException\">Thrown when the call reports a KvError code.</exception>"
            ),
            "exception doc missing: {cs}"
        );
    }

    #[test]
    fn wrapper_params_are_camel_case() {
        let api = make_api(vec![Module {
            name: "contacts".into(),
            functions: vec![Function {
                name: "create_contact".into(),
                params: vec![
                    Param {
                        name: "first_name".into(),
                        ty: TypeRef::StringUtf8,
                        mutable: false,
                        doc: Some("Given name.".into()),
                    },
                    Param {
                        name: "contact_type".into(),
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
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            interfaces: vec![],
            modules: vec![],
        }]);
        let cs = render_csharp(
            &api,
            "WeaveFFI",
            true,
            "weaveffi",
            "weaveffi.yml",
            "WeaveFFI.cs",
        );
        assert!(
            cs.contains("public static int CreateContact(string firstName, int contactType)"),
            "wrapper params must be camelCase: {cs}"
        );
        assert!(
            cs.contains("Marshal.StringToCoTaskMemUTF8(firstName)")
                && cs.contains("Marshal.FreeCoTaskMem(firstNamePtr);"),
            "marshalling locals must follow the camelCase name: {cs}"
        );
        assert!(
            cs.contains("/// <param name=\"firstName\">Given name.</param>"),
            "param docs must use the camelCase name: {cs}"
        );
        // The P/Invoke extern keeps the IDL spelling.
        assert!(
            cs.contains("internal static extern int weaveffi_contacts_create_contact(IntPtr first_name, int contact_type, ref WeaveFFIError err);"),
            "extern must keep IDL parameter names: {cs}"
        );
    }

    #[test]
    fn default_config_strips_module_prefix() {
        let config = DotnetConfig::default();
        assert!(
            config.strip_module_prefix,
            "strip_module_prefix must default to true"
        );
    }

    /// Parse, validate, and render a CLI fixture IDL end to end. Stands in
    /// for the CLI-driven generation while `weaveffi-cli` is blocked on other
    /// generators mid-overhaul: same parse, validation, model build, and
    /// render path the CLI runs, minus the argument plumbing.
    fn render_fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../weaveffi-cli/tests/fixtures")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
        let mut api = weaveffi_ir::parse::parse_api_str(&text, "yml").expect("fixture must parse");
        weaveffi_core::validate::validate_api(&mut api, None).expect("fixture must validate");
        render_csharp(&api, "WeaveFFI", true, "weaveffi", name, "WeaveFFI.cs")
    }

    #[test]
    fn fixture_contacts_renders_new_surface() {
        let cs = render_fixture("02_contacts.yml");
        // Interface class: real ctor for `new`, PascalCase methods with
        // camelCase parameters, disposal through the destroy symbol.
        assert!(
            cs.contains("public class ContactBook : IDisposable"),
            "ContactBook class missing: {cs}"
        );
        assert!(
            cs.contains("public ContactBook()"),
            "real constructor missing: {cs}"
        );
        assert!(
            cs.contains(
                "public Contact Add(string firstName, string lastName, string? email, \
                 ContactType contactType)"
            ),
            "Add method missing: {cs}"
        );
        assert!(
            cs.contains("public Contact Get(long id)"),
            "Get method missing: {cs}"
        );
        assert!(
            cs.contains("public Contact[] List()"),
            "List method missing: {cs}"
        );
        assert!(
            cs.contains("public bool Remove(long id)"),
            "Remove method missing: {cs}"
        );
        assert!(
            cs.contains("public int Count()"),
            "Count method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_contacts_ContactBook_destroy(_handle);")
                && cs.contains("~ContactBook()"),
            "dispose/finalizer missing: {cs}"
        );
        // Typed errors: domain exception with code constants, typed checks in
        // throwing methods, generic checks elsewhere.
        assert!(
            cs.contains("public class ContactsException : WeaveFFIException"),
            "ContactsException missing: {cs}"
        );
        assert!(
            cs.contains("public const int InvalidName = 1;")
                && cs.contains("public const int NotFound = 2;"),
            "code constants missing: {cs}"
        );
        let get = method_slice(&cs, "public Contact Get(long id)");
        assert!(
            get.contains("WeaveFFIError.CheckContacts(err);"),
            "throwing method must use the typed check: {get}"
        );
        let count = method_slice(&cs, "public int Count()");
        assert!(
            count.contains("WeaveFFIError.Check(err);") && !count.contains("CheckContacts"),
            "non-throwing method must use the generic check: {count}"
        );
    }

    #[test]
    fn fixture_inventory_renders_two_domains() {
        let cs = render_fixture("03_inventory.yml");
        // The products module owns the Catalog interface.
        assert!(
            cs.contains("public class Catalog : IDisposable"),
            "Catalog class missing: {cs}"
        );
        assert!(
            cs.contains("public Product AddProduct(string name, double price, Category category)"),
            "AddProduct method missing: {cs}"
        );
        assert!(
            cs.contains("public Product GetProduct(long id)"),
            "GetProduct method missing: {cs}"
        );
        assert!(
            cs.contains("NativeMethods.weaveffi_products_Catalog_destroy(_handle);"),
            "Catalog destroy missing: {cs}"
        );
        // Two error domains, each with its own exception and check helper.
        assert!(
            cs.contains("public class ProductsException : WeaveFFIException")
                && cs.contains("public class OrdersException : WeaveFFIException"),
            "both domain exceptions must render: {cs}"
        );
        assert!(
            cs.contains("public const int InvalidPrice = 1;")
                && cs.contains("public const int ProductNotFound = 2;")
                && cs.contains("public const int OrderNotFound = 1;")
                && cs.contains("public const int EmptyOrder = 2;"),
            "per-domain code constants missing: {cs}"
        );
        let add = method_slice(
            &cs,
            "public Product AddProduct(string name, double price, Category category)",
        );
        assert!(
            add.contains("WeaveFFIError.CheckProducts(err);"),
            "Catalog methods must use the products check: {add}"
        );
        // The orders module's free functions use their own domain.
        let create = method_slice(&cs, "public static long CreateOrder(OrderItem[] items)");
        assert!(
            create.contains("WeaveFFIError.CheckOrders(err);"),
            "orders functions must use the orders check: {create}"
        );
        let cancel = method_slice(&cs, "public static bool CancelOrder(long id)");
        assert!(
            cancel.contains("WeaveFFIError.Check(err);") && !cancel.contains("CheckOrders"),
            "non-throwing orders function must use the generic check: {cancel}"
        );
        // Per-module static classes with stripped method names.
        assert!(
            cs.contains("public static class Orders"),
            "orders wrapper class missing: {cs}"
        );
    }
}
