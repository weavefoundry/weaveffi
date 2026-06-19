//! The packaging layer: the data a backend needs to assemble a distributable
//! package, and the driver that materializes one to disk.
//!
//! `weaveffi generate` emits binding *source* that a consumer must compile or
//! point at a native library themselves. `weaveffi package` produces the next
//! artifact up: a ready-to-publish package for an ecosystem (an npm tarball
//! tree, a NuGet-ready project, a Python wheel tree, …) with a prebuilt native
//! library bundled for each [`Platform`](crate::platform::Platform) so
//! `npm install` / `pip install` / `dotnet add package` "just works" with no
//! local toolchain.
//!
//! A backend opts in by overriding
//! [`LanguageBackend::package`](crate::backend::LanguageBackend::package),
//! returning the full set of [`PackagedFile`]s that make up the package. The
//! [`write_package`] driver then writes the rendered text and copies the
//! bundled binaries into place. Rendering stays pure (it returns values, it
//! does no I/O), so package layouts are snapshot-testable exactly like
//! generated source.

use anyhow::{Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::platform::BinarySet;

/// The contents of one [`PackagedFile`]: either rendered text or a native
/// binary to copy in from elsewhere on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// Rendered text (a manifest, loader, README, or binding source file)
    /// written verbatim.
    Text(String),
    /// A native library copied byte-for-byte from this source path. Used for
    /// the prebuilt shared libraries a package bundles; keeping them out of
    /// [`Text`](Self::Text) means package rendering never has to hold a
    /// multi-megabyte binary in memory as a `String`.
    Copy(Utf8PathBuf),
}

/// One file in a packaged artifact: where to write it and what it contains.
///
/// `path` is the full destination path (anchored under the package output
/// directory), mirroring [`OutputFile`](crate::backend::OutputFile) so backends
/// build paths with the same `out_dir.join(...)` idiom they already use in
/// [`files`](crate::backend::LanguageBackend::files).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagedFile {
    /// Full destination path, anchored under the package output directory.
    pub path: Utf8PathBuf,
    /// What to materialize at `path`.
    pub content: FileContent,
}

impl PackagedFile {
    /// A file whose contents are rendered text.
    pub fn text(path: impl Into<Utf8PathBuf>, contents: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            content: FileContent::Text(contents.into()),
        }
    }

    /// A file copied from a prebuilt native library at `source`.
    pub fn copy(path: impl Into<Utf8PathBuf>, source: impl Into<Utf8PathBuf>) -> Self {
        Self {
            path: path.into(),
            content: FileContent::Copy(source.into()),
        }
    }

    /// True when this entry copies in a native binary rather than writing text.
    pub fn is_binary(&self) -> bool {
        matches!(self.content, FileContent::Copy(_))
    }
}

/// Everything a backend's [`package`](crate::backend::LanguageBackend::package)
/// hook needs beyond the [`Api`](weaveffi_ir::ir::Api) and
/// [`BindingModel`](crate::model::BindingModel) it already receives.
///
/// The prebuilt libraries to bundle live in [`binaries`](Self::binaries);
/// `input_basename` is the IDL file stem, used (as in `generate`) as the
/// fallback package name when the IDL omits a `package:` block.
#[derive(Debug, Clone, Copy)]
pub struct PackageContext<'a> {
    /// The prebuilt native libraries to bundle, one per platform, plus the
    /// logical library base name every loader and bundled filename derives
    /// from.
    pub binaries: &'a BinarySet,
    /// The IDL file stem, used as the fallback package name. `None` when the
    /// package identity comes entirely from the `package:` block or a config
    /// override.
    pub input_basename: Option<&'a str>,
}

/// Write a rendered package to disk: create parent directories, write every
/// [`FileContent::Text`] verbatim, and copy every [`FileContent::Copy`] native
/// binary into place.
///
/// # Errors
///
/// Returns an error if a parent directory cannot be created, a text file cannot
/// be written, or a bundled binary's source path cannot be read or copied.
pub fn write_package(files: &[PackagedFile]) -> Result<()> {
    for file in files {
        if let Some(parent) = file.path.parent() {
            std::fs::create_dir_all(parent.as_std_path())
                .with_context(|| format!("failed to create directory {parent}"))?;
        }
        match &file.content {
            FileContent::Text(contents) => {
                std::fs::write(file.path.as_std_path(), contents)
                    .with_context(|| format!("failed to write {}", file.path))?;
            }
            FileContent::Copy(source) => copy_binary(source, &file.path)?,
        }
    }
    Ok(())
}

fn copy_binary(source: &Utf8Path, dest: &Utf8Path) -> Result<()> {
    std::fs::copy(source.as_std_path(), dest.as_std_path())
        .with_context(|| format!("failed to copy native library {source} -> {dest}"))?;
    Ok(())
}

/// Count the text files and bundled binaries in a rendered package, for the
/// CLI's end-of-run summary.
///
/// Returns `(text_files, bundled_binaries)`.
pub fn summarize(files: &[PackagedFile]) -> (usize, usize) {
    let binaries = files.iter().filter(|f| f.is_binary()).count();
    (files.len() - binaries, binaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_package_writes_text_and_copies_binaries() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();

        // A source binary to copy.
        let src = root.join("src-lib.bin");
        std::fs::write(src.as_std_path(), b"\x00native\x01").unwrap();

        let files = vec![
            PackagedFile::text(root.join("pkg/manifest.json"), "{\"name\":\"x\"}"),
            PackagedFile::copy(root.join("pkg/native/lib.bin"), src.clone()),
        ];
        write_package(&files).unwrap();

        assert_eq!(
            std::fs::read_to_string(root.join("pkg/manifest.json")).unwrap(),
            "{\"name\":\"x\"}"
        );
        assert_eq!(
            std::fs::read(root.join("pkg/native/lib.bin")).unwrap(),
            b"\x00native\x01"
        );
        assert_eq!(summarize(&files), (1, 1));
    }

    #[test]
    fn missing_binary_source_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let files = vec![PackagedFile::copy(
            root.join("pkg/native/lib.bin"),
            root.join("does-not-exist.bin"),
        )];
        let err = write_package(&files).unwrap_err();
        assert!(err.to_string().contains("failed to copy native library"));
    }
}
