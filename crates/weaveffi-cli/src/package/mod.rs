//! The packaging layer: the artifacts a backend assembles from a set of
//! per-platform builds, and the driver that writes them to the dist
//! directory.
//!
//! `weaveffi generate` emits binding *source* that a consumer must compile or
//! point at a native library themselves. `weaveffi package` produces the
//! next artifact up: an installable package for each ecosystem (a wheel, an
//! npm tarball, a gem, a SwiftPM `XCFramework` archive, …) with the prebuilt
//! native library for each [`Platform`] inside, so
//! `pip install`, `npm install`, and `gem install` work with no local
//! toolchain.
//!
//! A backend opts in by overriding
//! [`LanguageBackend::package`](crate::backend::LanguageBackend::package),
//! returning [`Artifact`]s: a directory tree or an archive, each a list of
//! [`PackagedFile`]s. Rendering stays pure (it returns values and does no
//! I/O), so package layouts are testable exactly like generated source, and
//! [`write_artifact`] does the I/O with the writers in [`archive`].

pub mod archive;

use std::borrow::Cow;

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::platform::{read_file, BinarySet, Platform};

pub use archive::{GemSpec, WheelMeta};

/// The contents of one [`PackagedFile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// Rendered text (a manifest, loader, README, or binding source file).
    Text(String),
    /// Raw bytes rendered in memory.
    Bytes(Vec<u8>),
    /// A file copied byte-for-byte from this path, such as a prebuilt native
    /// library. Keeping binaries out of memory until the artifact is written
    /// means rendering never holds a multi-megabyte library as a value.
    Copy(Utf8PathBuf),
}

impl FileContent {
    /// The file's bytes, reading a [`Copy`](Self::Copy) source from disk.
    ///
    /// # Errors
    ///
    /// Returns an error when a copied file can't be read.
    pub fn bytes(&self) -> Result<Cow<'_, [u8]>> {
        Ok(match self {
            Self::Text(text) => Cow::Borrowed(text.as_bytes()),
            Self::Bytes(bytes) => Cow::Borrowed(bytes),
            Self::Copy(source) => Cow::Owned(read_file(source)?),
        })
    }
}

/// One file in an [`Artifact`]: its path inside the artifact and its
/// contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedFile {
    /// Path relative to the artifact's root, with `/` separators.
    pub path: Utf8PathBuf,
    /// What the file contains.
    pub content: FileContent,
}

impl PackagedFile {
    /// A file whose contents are rendered text.
    pub fn text(path: impl Into<Utf8PathBuf>, contents: impl Into<String>) -> Self {
        Self {
            path: normalize_separators(path.into()),
            content: FileContent::Text(contents.into()),
        }
    }

    /// A file whose contents are bytes rendered in memory.
    pub fn bytes(path: impl Into<Utf8PathBuf>, contents: Vec<u8>) -> Self {
        Self {
            path: normalize_separators(path.into()),
            content: FileContent::Bytes(contents),
        }
    }

    /// A file copied from `source` on disk (a prebuilt native library).
    pub fn copy(path: impl Into<Utf8PathBuf>, source: impl Into<Utf8PathBuf>) -> Self {
        Self {
            path: normalize_separators(path.into()),
            content: FileContent::Copy(source.into()),
        }
    }

    /// True when this entry copies in a file from disk rather than holding
    /// rendered contents.
    pub fn is_binary(&self) -> bool {
        matches!(self.content, FileContent::Copy(_))
    }
}

/// Normalize a package path to use `/` separators on every host, so package
/// layouts are identical (and testable) across platforms.
fn normalize_separators(path: Utf8PathBuf) -> Utf8PathBuf {
    if path.as_str().contains('\\') {
        Utf8PathBuf::from(path.as_str().replace('\\', "/"))
    } else {
        path
    }
}

/// How an [`Artifact`]'s files are materialized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactKind {
    /// A directory tree (a Gradle project, a Go module, a pub package).
    Directory,
    /// A gzip-compressed tarball whose entries all sit under `prefix/`
    /// (`package/` for npm).
    TarGz {
        /// The directory every entry is placed under.
        prefix: String,
    },
    /// A zip archive.
    Zip,
    /// A Python wheel: a zip whose `.dist-info` directory the writer adds.
    Wheel(WheelMeta),
    /// A RubyGems package: a tar of the gzipped specification and data.
    Gem(GemSpec),
}

/// One installable artifact a target produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    /// Where the artifact goes, relative to the dist directory: the
    /// directory for [`ArtifactKind::Directory`], else the archive's file.
    pub path: Utf8PathBuf,
    /// How the files are written.
    pub kind: ArtifactKind,
    /// The artifact's files, with paths relative to its root.
    pub files: Vec<PackagedFile>,
}

impl Artifact {
    /// A directory artifact at `path`.
    pub fn directory(path: impl Into<Utf8PathBuf>, files: Vec<PackagedFile>) -> Self {
        Self {
            path: normalize_separators(path.into()),
            kind: ArtifactKind::Directory,
            files,
        }
    }

    /// A `.tar.gz` artifact at `path` with every entry under `prefix/`.
    pub fn tar_gz(
        path: impl Into<Utf8PathBuf>,
        prefix: impl Into<String>,
        files: Vec<PackagedFile>,
    ) -> Self {
        Self {
            path: normalize_separators(path.into()),
            kind: ArtifactKind::TarGz {
                prefix: prefix.into(),
            },
            files,
        }
    }

    /// The file at `path` inside this artifact, if any.
    pub fn file(&self, path: &str) -> Option<&PackagedFile> {
        self.files.iter().find(|f| f.path == path)
    }
}

/// The prebuilt `XCFramework` archive a Swift package's binary target points
/// at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XcframeworkArchive {
    /// The archive's file name (`CKvstore.xcframework.zip`).
    pub file_name: String,
    /// The SHA-256 checksum of the archive, as `swift package
    /// compute-checksum` prints it.
    pub checksum: String,
}

/// Everything a backend's [`package`](crate::backend::LanguageBackend::package)
/// needs beyond the [`Model`](weaveffi_model::model::Model) and its own
/// configuration.
#[derive(Debug, Clone, Copy)]
pub struct PackageContext<'a> {
    /// The per-platform builds to bundle.
    pub binaries: &'a BinarySet,
    /// The macOS deployment target the libraries were built for (`"11.0"`),
    /// which wheel tags and Swift platforms carry.
    pub macos_deployment_target: &'a str,
    /// The iOS deployment target the libraries were built for (`"13.0"`).
    pub ios_deployment_target: &'a str,
    /// The assembled `XCFramework` archive, when the Swift target packages
    /// Apple static libraries.
    pub xcframework: Option<&'a XcframeworkArchive>,
}

impl<'a> PackageContext<'a> {
    /// A context for `binaries` built for the default deployment targets
    /// (macOS 11.0, iOS 13.0), without an `XCFramework` archive.
    #[must_use]
    pub fn new(binaries: &'a BinarySet) -> Self {
        Self {
            binaries,
            macos_deployment_target: "11.0",
            ios_deployment_target: "13.0",
            xcframework: None,
        }
    }
}

/// The producer library of every build `keep` accepts, laid out as
/// `{dir}/<platform-id>/<library file>`, plus the import library MSVC links
/// a Windows DLL through.
pub fn per_platform_libraries(
    binaries: &BinarySet,
    dir: &str,
    keep: impl Fn(Platform) -> bool,
) -> Vec<PackagedFile> {
    let mut files = Vec::new();
    for nb in binaries.binaries.iter().filter(|nb| keep(nb.platform)) {
        let platform_dir = format!("{dir}/{}", nb.platform.id());
        files.push(PackagedFile::copy(
            format!("{platform_dir}/{}", binaries.bundled_filename(nb.platform)),
            nb.library.clone(),
        ));
        if let Some(import) = &nb.import_library {
            files.push(PackagedFile::copy(
                format!("{platform_dir}/{}.dll.lib", binaries.lib_name),
                import.clone(),
            ));
        }
    }
    files
}

/// Write `artifact` under `dist`, returning the path written.
///
/// A directory artifact replaces any previous directory at its path, so
/// files from an earlier run never linger; an archive is built in memory and
/// written in one go.
///
/// # Errors
///
/// Returns an error when a directory can't be created or replaced, a file
/// can't be read or written, or an archive can't be assembled.
pub fn write_artifact(dist: &Utf8Path, artifact: &Artifact) -> Result<Utf8PathBuf> {
    let path = dist.join(&artifact.path);
    if let ArtifactKind::Directory = artifact.kind {
        if path.exists() {
            std::fs::remove_dir_all(path.as_std_path())
                .with_context(|| format!("failed to replace {path}"))?;
        }
        for file in &artifact.files {
            let dest = path.join(&file.path);
            create_parent(&dest)?;
            match &file.content {
                FileContent::Copy(source) => {
                    std::fs::copy(source.as_std_path(), dest.as_std_path())
                        .with_context(|| format!("failed to copy {source} to {dest}"))?;
                }
                content => std::fs::write(dest.as_std_path(), content.bytes()?)
                    .with_context(|| format!("failed to write {dest}"))?,
            }
        }
        return Ok(path);
    }
    let bytes = match &artifact.kind {
        ArtifactKind::Directory => unreachable!("handled above"),
        ArtifactKind::TarGz { prefix } => {
            let entries = archive::entries(&artifact.files, Some(prefix))?;
            archive::gzip(&archive::tar(&entries)?)?
        }
        ArtifactKind::Zip => archive::zip(&archive::entries(&artifact.files, None)?)?,
        ArtifactKind::Wheel(meta) => archive::wheel(meta, &artifact.files)?,
        ArtifactKind::Gem(spec) => archive::gem(spec, &artifact.files)?,
    };
    create_parent(&path)?;
    std::fs::write(path.as_std_path(), bytes).with_context(|| format!("failed to write {path}"))?;
    Ok(path)
}

fn create_parent(path: &Utf8Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent.as_std_path())
            .with_context(|| format!("failed to create {parent}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_are_replaced_and_binaries_copied() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let src = root.join("src-lib.bin");
        std::fs::write(&src, b"\x00native\x01").unwrap();
        std::fs::create_dir_all(root.join("dist/pkg")).unwrap();
        std::fs::write(root.join("dist/pkg/stale.txt"), "old").unwrap();

        let artifact = Artifact::directory(
            "pkg",
            vec![
                PackagedFile::text("manifest.json", "{\"name\":\"x\"}"),
                PackagedFile::copy("native/lib.bin", src.clone()),
            ],
        );
        let written = write_artifact(&root.join("dist"), &artifact).unwrap();
        assert_eq!(written, root.join("dist/pkg"));
        assert_eq!(
            std::fs::read_to_string(written.join("manifest.json")).unwrap(),
            "{\"name\":\"x\"}"
        );
        assert_eq!(
            std::fs::read(written.join("native/lib.bin")).unwrap(),
            b"\x00native\x01"
        );
        assert!(!written.join("stale.txt").exists());
    }

    #[test]
    fn missing_copy_sources_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let artifact = Artifact::tar_gz(
            "x.tar.gz",
            "x",
            vec![PackagedFile::copy("lib.bin", root.join("missing.bin"))],
        );
        let err = write_artifact(root, &artifact).unwrap_err();
        assert!(format!("{err:#}").contains("missing.bin"), "{err:#}");
    }

    #[test]
    fn paths_are_forward_slashed() {
        let text = PackagedFile::text(Utf8Path::new("dotnet").join("x.cs"), "x");
        assert_eq!(text.path.as_str(), "dotnet/x.cs");
        let artifact = Artifact::directory("a\\b", vec![]);
        assert_eq!(artifact.path.as_str(), "a/b");
    }
}
