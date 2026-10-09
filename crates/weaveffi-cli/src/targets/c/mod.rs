//! C header generator for WeaveFFI.
//!
//! Emits `{library}.h`, describing the stable C ABI surface of a [`Model`].
//! This is the canonical backend: the header it emits *is* the C ABI every
//! other language binds to, and a C or C++ producer implementing
//! an IDL by hand implements exactly the functions it declares.
//!
//! When the API passes any value buffers and [`CConfig::buffer_helpers`] is
//! on (the default), it also emits `{library}_buffer.h`: C structs for the
//! API's records, rich enums, lists, and maps, with `static inline` codecs
//! that encode and decode them (see [`render_buffer_header`]).
//!
//! Like every WeaveFFI backend it renders from the shared [`Model`], so
//! symbol names and parameter lowering are computed once and shared, never
//! re-derived here.

mod buffer;
mod header;
mod package;
use crate::codegen::OutputFile;
use crate::package::{per_platform_libraries, Artifact, PackageContext, PackagedFile};
use crate::targets::{Linkage, Target};
use miette::Result;
use serde::{Deserialize, Serialize};
use weaveffi_model::model::Model;

pub(crate) use buffer::{buffer_header_name, render_buffer_header};
pub(crate) use header::render_c_header_from_model;

use package::{render_packaged_cmake, render_packaged_readme};

/// Per-target configuration for [`CGenerator`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CConfig {
    /// Emit `{library}_buffer.h`, the value-buffer helper header, next to
    /// `{library}.h` when the API passes any value buffers. On by default.
    pub buffer_helpers: bool,
}

impl Default for CConfig {
    fn default() -> Self {
        Self {
            buffer_helpers: true,
        }
    }
}

/// The header file name for an API: `{library}.h`.
#[must_use]
pub fn header_name(model: &Model) -> String {
    format!("{}.h", model.identity.library)
}

/// The helper header's name and contents, when the configuration asks for it
/// and the API has value buffers to describe.
fn buffer_file(model: &Model, config: &CConfig) -> Option<(String, String)> {
    if !config.buffer_helpers {
        return None;
    }
    let name = buffer_header_name(&model.identity.library);
    render_buffer_header(model, &header_name(model), &name).map(|contents| (name, contents))
}

/// The C header target.
pub struct CGenerator {
    config: CConfig,
}

impl From<CConfig> for CGenerator {
    fn from(config: CConfig) -> Self {
        Self { config }
    }
}

impl Target for CGenerator {
    fn name(&self) -> &'static str {
        "c"
    }

    fn render(&self, model: &Model) -> Vec<OutputFile> {
        let header = header_name(model);
        let mut files = vec![OutputFile::new(
            &header,
            render_c_header_from_model(model, &header),
        )];
        if let Some((name, contents)) = buffer_file(model, &self.config) {
            files.push(OutputFile::new(name, contents));
        }
        files
    }

    fn linkage(&self) -> Linkage {
        Linkage::Link
    }

    /// One `{library}-{version}-c.tar.gz` holding the headers under
    /// `include/`, every platform's library under `lib/<platform>/`, and a
    /// `CMakeLists.txt` exposing the host's library as an imported target.
    fn package(&self, model: &Model, ctx: &PackageContext<'_>) -> Result<Vec<Artifact>> {
        let config = &self.config;
        let header_name = header_name(model);
        let lib = &ctx.binaries.lib_name;
        let mut files = vec![
            PackagedFile::text(
                format!("include/{header_name}"),
                render_c_header_from_model(model, &header_name),
            ),
            PackagedFile::text("CMakeLists.txt", render_packaged_cmake(lib)),
            PackagedFile::text("README.md", render_packaged_readme(lib, &header_name, ctx)),
        ];
        if let Some((name, contents)) = buffer_file(model, config) {
            files.push(PackagedFile::text(format!("include/{name}"), contents));
        }
        files.extend(per_platform_libraries(ctx.binaries, "lib", |_| true));
        let stem = format!("{lib}-{}", model.identity.version);
        Ok(vec![Artifact::tar_gz(
            format!("c/{stem}-c.tar.gz"),
            stem,
            files,
        )])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::test_model;
    use crate::package::ArtifactKind;
    use crate::platform::{BinarySet, NativeBinary, Platform};

    const SHOP: &str = r#"
version: "0.12.0"
modules:
  - name: shop
    structs:
      - name: Item
        fields: [{ name: name, type: string }]
    functions:
      - { name: first, params: [], return: Item }
"#;

    #[test]
    fn package_bundles_headers_cmake_and_every_platform() {
        let model = test_model(SHOP);
        let mut binaries = BinarySet::new("kv");
        for p in [Platform::MacosArm64, Platform::LinuxX64] {
            binaries.insert(NativeBinary::new(p, format!("/prebuilt/{}/lib", p.id())));
        }
        let artifacts = CGenerator::from(CConfig::default())
            .package(&model, &PackageContext::new(&binaries))
            .expect("c packages");
        assert_eq!(artifacts.len(), 1);
        let artifact = &artifacts[0];
        assert_eq!(artifact.path, "c/kv-0.1.0-c.tar.gz");
        assert!(matches!(&artifact.kind, ArtifactKind::TarGz { prefix } if prefix == "kv-0.1.0"));
        let mut paths: Vec<&str> = artifact.files.iter().map(|f| f.path.as_str()).collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            [
                "CMakeLists.txt",
                "README.md",
                "include/kv.h",
                "include/kv_buffer.h",
                "lib/darwin-arm64/libkv.dylib",
                "lib/linux-x64/libkv.so",
            ]
        );
    }

    #[test]
    fn enums_cross_as_int32_with_anonymous_constants() {
        let model = test_model(
            r#"
version: "0.12.0"
modules:
  - name: m
    enums:
      - name: Mode
        variants: [{ name: Fast, value: 0 }, { name: Slow, value: 7 }]
    errors:
      - name: Oops
        codes: [{ name: Broke, code: 3, message: broke }]
    functions:
      - { name: pick, params: [{ name: m, type: "Mode?" }], return: Mode, throws: Oops }
"#,
        );
        let h = render_c_header_from_model(&model, "kv.h");
        assert!(h.contains(
            "typedef int32_t kv_m_Mode;\nenum { kv_m_Mode_Fast = 0, kv_m_Mode_Slow = 7 };"
        ));
        assert!(h.contains("typedef int32_t kv_m_Oops;\nenum { kv_m_Oops_Broke = 3 };"));
        assert!(!h.contains("typedef enum"));
        assert!(h.contains("kv_m_Mode kv_m_pick(bool has_m, kv_m_Mode m, kv_error* out_err);"));
        assert!(h.contains("Fails with a `kv_m_Oops` code"));
    }
}
