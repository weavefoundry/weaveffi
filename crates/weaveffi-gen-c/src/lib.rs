//! C header generator for WeaveFFI.
//!
//! Emits a single `{prefix}.h` describing the stable C ABI surface of an
//! [`Api`], plus a companion `{prefix}.c` placeholder for future convenience
//! wrappers. This is the canonical backend: the header it emits *is* the C ABI
//! every other language binds to.
//!
//! Like every WeaveFFI backend it renders from the shared
//! [`weaveffi_core::model::BindingModel`], so symbol names and parameter
//! lowering are computed once and shared, never re-derived here.
#![deny(missing_docs)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::doc_markdown)]

use std::fmt::Write;

use camino::Utf8Path;
use serde::{Deserialize, Serialize};
use weaveffi_core::backend::{LanguageBackend, OutputFile};
use weaveffi_core::cabi;
use weaveffi_core::capabilities::TargetCapabilities;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::{PackageContext, PackagedFile};
use weaveffi_core::utils::{
    render_abi_prefix_aliases, render_prelude, render_trailer, CommentStyle,
};
use weaveffi_ir::ir::Api;

/// Per-target configuration for [`CGenerator`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CConfig {
    /// Prefix applied to every emitted C symbol (default `"weaveffi"`).
    /// Renames produce both `prefix_*` user symbols and
    /// `#define prefix_runtime weaveffi_runtime` aliases for the ABI helpers.
    pub prefix: Option<String>,
    /// Basename of the IDL the CLI was invoked with (e.g. `weaveffi.yml`).
    /// Embedded in the prelude header of every generated file. Populated
    /// by the CLI; not user-configurable via the `[c]` config section.
    #[serde(skip)]
    pub input_basename: Option<String>,
}

impl CConfig {
    /// Returns the configured symbol prefix, falling back to `"weaveffi"`.
    pub fn prefix(&self) -> &str {
        self.prefix.as_deref().unwrap_or("weaveffi")
    }

    /// Returns the input IDL basename embedded in generated file headers,
    /// falling back to `"weaveffi.yml"`.
    pub fn input_basename(&self) -> &str {
        self.input_basename.as_deref().unwrap_or("weaveffi.yml")
    }
}

/// C backend: emits the canonical `{prefix}.h` header describing the stable
/// C ABI surface, plus a `{prefix}.c` companion for future wrappers.
pub struct CGenerator;

impl LanguageBackend for CGenerator {
    type Config = CConfig;

    fn name(&self) -> &'static str {
        "c"
    }

    fn capabilities(&self) -> TargetCapabilities {
        TargetCapabilities::full()
    }

    fn prefix<'a>(&self, config: &'a Self::Config) -> &'a str {
        config.prefix()
    }

    fn files(
        &self,
        _api: &Api,
        model: &BindingModel,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Vec<OutputFile> {
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("c");
        let header_name = format!("{prefix}.h");
        let source_name = format!("{prefix}.c");
        vec![
            OutputFile::new(
                dir.join(&header_name),
                render_c_header_from_model(model, input_basename, &header_name),
            ),
            OutputFile::new(
                dir.join(&source_name),
                render_c_convenience_c(prefix, input_basename, &source_name),
            ),
        ]
    }

    fn package(
        &self,
        _api: &Api,
        model: &BindingModel,
        ctx: &PackageContext,
        out_dir: &Utf8Path,
        config: &Self::Config,
    ) -> Option<Vec<PackagedFile>> {
        let prefix = config.prefix();
        let input_basename = config.input_basename();
        let dir = out_dir.join("c");
        let header_name = format!("{prefix}.h");
        let lib = &ctx.binaries.lib_name;

        let mut files = vec![
            PackagedFile::text(
                dir.join("include").join(&header_name),
                render_c_header_from_model(model, input_basename, &header_name),
            ),
            PackagedFile::text(
                dir.join("CMakeLists.txt"),
                render_packaged_cmake(lib, input_basename),
            ),
            PackagedFile::text(
                dir.join("README.md"),
                render_packaged_readme(lib, &header_name, ctx, input_basename),
            ),
        ];
        for nb in &ctx.binaries.binaries {
            let dest = dir
                .join("lib")
                .join(nb.platform.id())
                .join(ctx.binaries.bundled_filename(nb.platform));
            files.push(PackagedFile::copy(dest, nb.source.clone()));
        }
        Some(files)
    }
}

weaveffi_core::impl_generator_via_backend!(CGenerator);

/// Render a `CMakeLists.txt` that exposes the bundled per-platform library as
/// an `IMPORTED` target (`<lib>::<lib>`) with the header's include directory,
/// selecting the right library for the host platform and architecture.
fn render_packaged_cmake(lib: &str, input_basename: &str) -> String {
    let prelude = render_prelude(CommentStyle::Hash, input_basename);
    let trailer = render_trailer(CommentStyle::Hash, "CMakeLists.txt");
    let body = r#"cmake_minimum_required(VERSION 3.15)
project(@LIB@ C)

# Select the prebuilt library bundled for the host platform and architecture.
if(APPLE)
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "arm64|aarch64")
    set(_wv_plat "darwin-arm64")
  else()
    set(_wv_plat "darwin-x64")
  endif()
  set(_wv_libfile "lib@LIB@.dylib")
elseif(WIN32)
  set(_wv_plat "windows-x64")
  set(_wv_libfile "@LIB@.dll")
else()
  if(CMAKE_SYSTEM_PROCESSOR MATCHES "aarch64|arm64")
    set(_wv_plat "linux-arm64")
  else()
    set(_wv_plat "linux-x64")
  endif()
  set(_wv_libfile "lib@LIB@.so")
endif()

add_library(@LIB@ SHARED IMPORTED GLOBAL)
set_target_properties(@LIB@ PROPERTIES
  IMPORTED_LOCATION "${CMAKE_CURRENT_LIST_DIR}/lib/${_wv_plat}/${_wv_libfile}"
  INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_LIST_DIR}/include")
add_library(@LIB@::@LIB@ ALIAS @LIB@)
"#
    .replace("@LIB@", lib);
    format!("{prelude}{body}\n{trailer}")
}

/// README for a packaged C artifact bundling the header and per-platform libs.
fn render_packaged_readme(
    lib: &str,
    header_name: &str,
    ctx: &PackageContext,
    input_basename: &str,
) -> String {
    let prelude = render_prelude(CommentStyle::Xml, input_basename);
    let trailer = render_trailer(CommentStyle::Xml, "README.md");
    let platforms: Vec<String> = ctx
        .binaries
        .platforms()
        .map(|p| format!("- `lib/{}/`", p.id()))
        .collect();
    let platform_list = platforms.join("\n");
    format!(
        r#"{prelude}# {lib} (C)

The stable C ABI header (`include/{header_name}`) plus a prebuilt shared library
for each supported platform under `lib/<platform>/`.

## Use with CMake

```cmake
add_subdirectory(path/to/{lib})
target_link_libraries(your_app PRIVATE {lib}::{lib})
```

`CMakeLists.txt` selects the right library for the host platform and exposes the
include directory automatically.

## Bundled platforms

{platform_list}

{trailer}"#,
    )
}

/// Render the complete `{prefix}.h` for `api` using `prefix` for every symbol.
///
/// Thin `Api`-based wrapper over [`render_c_header_from_model`] for tests and
/// callers that only hold an [`Api`]; the production path renders directly from
/// the driver-built [`BindingModel`] without re-deriving it.
pub fn render_c_header(api: &Api, prefix: &str, input_basename: &str, filename: &str) -> String {
    render_c_header_from_model(&BindingModel::build(api, prefix), input_basename, filename)
}

/// Render the complete header from the shared [`BindingModel`].
///
/// The per-declaration rendering is shared with the C++ backend via
/// [`weaveffi_core::cabi`]; this function only adds the header framing
/// (include guard, includes, prefix aliases, the map-convention comment). The
/// C symbol prefix is read from [`BindingModel::prefix`], so every name already
/// agrees with the symbols baked into the model.
pub fn render_c_header_from_model(
    model: &BindingModel,
    input_basename: &str,
    filename: &str,
) -> String {
    let prefix = model.prefix.as_str();
    let guard = format!("{}_H", prefix.to_uppercase());
    let mut out = String::with_capacity(2048 + model.modules.len() * 4096);
    out.push_str(&render_prelude(CommentStyle::DoubleSlash, input_basename));
    let _ = write!(out, "#ifndef {guard}\n#define {guard}\n\n");
    out.push_str("#include <stdint.h>\n");
    out.push_str("#include <stddef.h>\n");
    out.push_str("#include <stdbool.h>\n\n");
    cabi::render_visibility_macros(&mut out, prefix);
    out.push_str(&render_abi_prefix_aliases(prefix));
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    cabi::render_runtime_decls(&mut out, prefix);
    out.push_str("/*\n");
    out.push_str(" * Map convention: Maps are passed as parallel arrays of keys and values.\n");
    out.push_str(" * A map parameter {K:V} named \"m\" expands to:\n");
    out.push_str(" *   const K* m_keys, const V* m_values, size_t m_len\n");
    out.push_str(" * A map return value expands to out-parameters that receive callee-\n");
    out.push_str(" * allocated arrays; the caller passes the address of its own pointers:\n");
    out.push_str(" *   K** out_keys, V** out_values, size_t* out_len\n");
    out.push_str(" */\n\n");

    cabi::render_decls(&mut out, &model.modules, prefix, true);

    out.push_str("\n#ifdef __cplusplus\n}\n#endif\n\n");
    let _ = write!(out, "#endif // {guard}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

fn render_c_convenience_c(prefix: &str, input_basename: &str, filename: &str) -> String {
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    let _ = write!(
        out,
        "#include \"{prefix}.h\"\n\n// Optional convenience wrappers can be added here in future versions.\n\n"
    );
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_core::codegen::Generator;
    use weaveffi_ir::ir::{
        CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
        StructField, TypeRef,
    };

    #[test]
    fn package_bundles_header_libs_and_cmake() {
        use camino::Utf8Path;
        use weaveffi_core::package::{FileContent, PackageContext};
        use weaveffi_core::platform::{BinarySet, Platform};

        let api = Api {
            version: "0.4.0".into(),
            modules: vec![module("calc")],
            generators: None,
            package: None,
        };
        let model = BindingModel::build(&api, "weaveffi");
        let mut bins = BinarySet::new("calculator");
        bins.insert(Platform::MacosArm64, "/s/darwin-arm64/libcalculator.dylib");
        bins.insert(Platform::LinuxX64, "/s/linux-x64/libcalculator.so");
        let ctx = PackageContext {
            binaries: &bins,
            input_basename: Some("calculator.yml"),
        };
        let files = LanguageBackend::package(
            &CGenerator,
            &api,
            &model,
            &ctx,
            Utf8Path::new("/out"),
            &CConfig::default(),
        )
        .expect("c supports packaging");

        assert_eq!(files.iter().filter(|f| f.is_binary()).count(), 2);
        assert!(files
            .iter()
            .any(|f| f.path.as_str().ends_with("c/include/weaveffi.h")));
        assert!(files.iter().any(|f| f
            .path
            .as_str()
            .ends_with("c/lib/darwin-arm64/libcalculator.dylib")));
        let cmake = files
            .iter()
            .find(|f| f.path.as_str().ends_with("c/CMakeLists.txt"))
            .expect("CMakeLists present");
        let FileContent::Text(txt) = &cmake.content else {
            panic!("CMakeLists is text");
        };
        assert!(
            txt.contains("IMPORTED") && txt.contains("calculator::calculator"),
            "imported target missing: {txt}"
        );
    }

    fn param(name: &str, ty: TypeRef) -> Param {
        Param {
            name: name.into(),
            ty,
            mutable: false,
            doc: None,
        }
    }

    fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params,
            returns,
            doc: None,
            r#async: false,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
            modules,
            generators: None,
            package: None,
        }
    }

    fn header(api: &Api, prefix: &str) -> String {
        render_c_header(api, prefix, "weaveffi.yml", "weaveffi.h")
    }

    #[test]
    fn emits_guard_and_runtime_decls() {
        let h = header(&api(vec![module("math")]), "weaveffi");
        assert!(h.contains("#ifndef WEAVEFFI_H"));
        assert!(h.contains("typedef uint64_t weaveffi_handle_t;"));
        assert!(h.contains("void weaveffi_free_string(const char* ptr);"));
    }

    #[test]
    fn sync_function_signature() {
        let m = Module {
            functions: vec![func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
            )],
            ..module("math")
        };
        let h = header(&api(vec![m]), "weaveffi");
        assert!(
            h.contains("int32_t weaveffi_math_add(int32_t a, int32_t b, weaveffi_error* out_err);")
        );
    }

    #[test]
    fn custom_prefix_is_honored() {
        let m = Module {
            functions: vec![func("ping", vec![], None)],
            ..module("net")
        };
        let h = header(&api(vec![m]), "acme");
        assert!(h.contains("#ifndef ACME_H"));
        assert!(h.contains("void acme_net_ping(acme_error* out_err);"));
        assert!(h.contains("#define acme_error weaveffi_error"));
    }

    #[test]
    fn visibility_macro_defined_and_applied_to_prototypes() {
        let m = Module {
            functions: vec![func(
                "add",
                vec![param("a", TypeRef::I32), param("b", TypeRef::I32)],
                Some(TypeRef::I32),
            )],
            ..module("math")
        };
        let h = header(&api(vec![m]), "weaveffi");
        // The macro is defined behind an include guard with the three branches.
        assert!(h.contains("#ifndef WEAVEFFI_API"));
        assert!(h.contains("#      define WEAVEFFI_API __declspec(dllexport)"));
        assert!(h.contains("#      define WEAVEFFI_API __declspec(dllimport)"));
        assert!(h.contains("#    define WEAVEFFI_API __attribute__((visibility(\"default\")))"));
        // Both runtime helpers and user functions carry the export tag.
        assert!(h.contains("WEAVEFFI_API void weaveffi_free_string(const char* ptr);"));
        assert!(h.contains(
            "WEAVEFFI_API int32_t weaveffi_math_add(int32_t a, int32_t b, weaveffi_error* out_err);"
        ));
        // Type definitions are never tagged: they declare no exported symbol.
        assert!(h.contains("typedef uint64_t weaveffi_handle_t;"));
        assert!(!h.contains("WEAVEFFI_API typedef"));
    }

    #[test]
    fn visibility_macro_follows_custom_prefix() {
        let m = Module {
            functions: vec![func("ping", vec![], None)],
            ..module("net")
        };
        let h = header(&api(vec![m]), "acme");
        assert!(h.contains("#ifndef ACME_API"));
        assert!(h.contains("ifdef ACME_BUILD"));
        assert!(h.contains("ACME_API void acme_net_ping(acme_error* out_err);"));
        // The default-prefixed macro must not leak when a prefix is configured.
        assert!(!h.contains("WEAVEFFI_API"));
    }

    #[test]
    fn deprecated_uses_portable_macro_not_bare_attribute() {
        let m = Module {
            functions: vec![Function {
                deprecated: Some("use bar instead".into()),
                ..func("foo", vec![], None)
            }],
            ..module("legacy")
        };
        let h = header(&api(vec![m]), "weaveffi");
        assert!(h.contains("#ifndef WEAVEFFI_DEPRECATED"));
        assert!(h.contains("WEAVEFFI_DEPRECATED(\"use bar instead\")"));
        // The message must travel through the macro, never a bare GCC attribute
        // (which MSVC cannot parse).
        assert!(!h.contains("__attribute__((deprecated(\"use bar instead\")))"));
    }

    #[test]
    fn struct_with_builder() {
        let m = Module {
            structs: vec![StructDef {
                name: "Contact".into(),
                doc: None,
                fields: vec![StructField {
                    name: "name".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                    default: None,
                }],
                builder: true,
            }],
            ..module("contacts")
        };
        let h = header(&api(vec![m]), "weaveffi");
        assert!(h.contains("typedef struct weaveffi_contacts_Contact weaveffi_contacts_Contact;"));
        assert!(h.contains(
            "const char* weaveffi_contacts_Contact_get_name(const weaveffi_contacts_Contact* ptr);"
        ));
        assert!(h.contains(
            "weaveffi_contacts_ContactBuilder* weaveffi_contacts_Contact_Builder_new(void);"
        ));
    }

    #[test]
    fn enum_constants() {
        let m = Module {
            enums: vec![EnumDef {
                name: "Color".into(),
                doc: None,
                variants: vec![EnumVariant {
                    name: "Red".into(),
                    value: 0,
                    doc: None,
                    fields: vec![],
                }],
            }],
            ..module("gfx")
        };
        let h = header(&api(vec![m]), "weaveffi");
        assert!(h.contains("weaveffi_gfx_Color_Red = 0"));
    }

    #[test]
    fn iterator_emits_next_and_destroy() {
        let m = Module {
            functions: vec![func(
                "get_messages",
                vec![],
                Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
            )],
            ..module("events")
        };
        let h = header(&api(vec![m]), "weaveffi");
        assert!(h.contains(
            "weaveffi_events_GetMessagesIterator* weaveffi_events_get_messages(weaveffi_error* out_err);"
        ));
        assert!(h.contains("weaveffi_events_GetMessagesIterator_next("));
        assert!(h.contains("void weaveffi_events_GetMessagesIterator_destroy(weaveffi_events_GetMessagesIterator* iter);"));
    }

    #[test]
    fn callback_and_listener() {
        let m = Module {
            callbacks: vec![CallbackDef {
                name: "on_message".into(),
                params: vec![param("text", TypeRef::StringUtf8)],
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "messages".into(),
                event_callback: "on_message".into(),
                doc: None,
            }],
            ..module("events")
        };
        let h = header(&api(vec![m]), "weaveffi");
        assert!(h.contains(
            "typedef void (*weaveffi_events_on_message_fn)(const char* text, void* context);"
        ));
        assert!(h.contains("uint64_t weaveffi_events_register_messages(weaveffi_events_on_message_fn callback, void* context);"));
        assert!(h.contains("void weaveffi_events_unregister_messages(uint64_t id);"));
    }

    #[test]
    fn async_emits_callback_typedef_and_launcher() {
        let m = Module {
            functions: vec![Function {
                r#async: true,
                cancellable: true,
                ..func(
                    "fetch",
                    vec![param("id", TypeRef::I64)],
                    Some(TypeRef::StringUtf8),
                )
            }],
            ..module("net")
        };
        let h = header(&api(vec![m]), "weaveffi");
        assert!(h.contains("typedef void (*weaveffi_net_fetch_callback)(void* context, weaveffi_error* err, const char* result);"));
        assert!(h.contains("weaveffi_net_fetch_async("));
        assert!(h.contains("weaveffi_cancel_token* cancel_token"));
    }

    #[test]
    fn output_files_lists_header_and_source() {
        let tmp = std::env::temp_dir().join("weaveffi_c_outfiles");
        let out_dir = Utf8Path::from_path(&tmp).unwrap();
        let files = CGenerator.output_files(&api(vec![module("m")]), out_dir, &CConfig::default());
        assert!(files.iter().any(|f| f.ends_with("c/weaveffi.h")));
        assert!(files.iter().any(|f| f.ends_with("c/weaveffi.c")));
    }
}
