//! Reading a Rust producer's API out of its built library.
//!
//! The `#[weaveffi::module]` macro embeds one metadata
//! [`Frame`] per declaration in the producer's
//! library. [`read_frames`] finds them with the `object` crate: in a native
//! library (ELF, Mach-O, or PE, or every member of a static archive) by
//! walking the symbol tables for the `{PREFIX}_META_{HASH16}` statics and
//! reading each one's bytes from the section that holds it; in a `.wasm`
//! module from the `weaveffi_meta` custom section, which the linker fills
//! with every frame back to back.

use std::collections::BTreeSet;

use camino::Utf8Path;
use miette::{bail, IntoDiagnostic, Result, WrapErr};
use object::read::archive::ArchiveFile;
use object::{BinaryFormat, Object, ObjectSection, ObjectSymbol, SymbolSection};
use weaveffi_model::meta::{self, Frame};

/// Every metadata frame in the library at `path`: a shared library
/// (`.so`, `.dylib`, `.dll`), a static archive (`.a`, `.lib`), or a `.wasm`
/// module. The frames of every crate linked into the library are returned;
/// [`meta::assemble`] keeps one crate's.
///
/// # Errors
///
/// Returns an error when the file can't be read or isn't an object file
/// `object` understands, or when a frame is malformed.
pub(crate) fn read_frames(path: &Utf8Path) -> Result<Vec<Frame>> {
    let data = std::fs::read(path)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to read {path}"))?;
    let mut frames = Vec::new();
    if let Ok(archive) = ArchiveFile::parse(&*data) {
        for member in archive.members() {
            let member = member
                .into_diagnostic()
                .wrap_err_with(|| format!("{path} is a malformed archive"))?;
            let bytes = member
                .data(&*data)
                .into_diagnostic()
                .wrap_err_with(|| format!("{path} is a malformed archive"))?;
            // Archives also carry symbol tables and other non-object members.
            if let Ok(file) = object::File::parse(bytes) {
                frames.extend(object_frames(&file).wrap_err_with(|| {
                    format!(
                        "{path}({}) has malformed WeaveFFI metadata",
                        String::from_utf8_lossy(member.name())
                    )
                })?);
            }
        }
        return Ok(frames);
    }
    let file = object::File::parse(&*data)
        .into_diagnostic()
        .wrap_err_with(|| format!("{path} isn't a library WeaveFFI can read"))?;
    object_frames(&file).wrap_err_with(|| format!("{path} has malformed WeaveFFI metadata"))
}

/// The frames in one object file.
fn object_frames(file: &object::File<'_>) -> Result<Vec<Frame>> {
    if file.format() == BinaryFormat::Wasm {
        return match file.section_by_name(meta::SECTION) {
            Some(section) => meta::decode(section.data().into_diagnostic()?).into_diagnostic(),
            None => Ok(Vec::new()),
        };
    }
    // Statically linked and stripped libraries keep the statics in different
    // tables (the symbol table, the dynamic symbol table, a PE export
    // table), so look in all of them and read each symbol once.
    let mut candidates: Vec<(String, u64, Option<object::SectionIndex>)> = Vec::new();
    for symbol in file.symbols().chain(file.dynamic_symbols()) {
        let Ok(name) = symbol.name() else { continue };
        if !meta::is_symbol(name) {
            continue;
        }
        let section = match symbol.section() {
            SymbolSection::Section(index) => Some(index),
            _ => continue,
        };
        candidates.push((name.to_string(), symbol.address(), section));
    }
    for export in file.exports().unwrap_or_default() {
        let name = String::from_utf8_lossy(export.name()).into_owned();
        if meta::is_symbol(&name) {
            candidates.push((name, export.address(), None));
        }
    }
    let mut seen = BTreeSet::new();
    let mut frames = Vec::new();
    for (name, address, section) in candidates {
        if !seen.insert(name.trim_start_matches('_').to_string()) {
            continue;
        }
        let Some(bytes) = data_at(file, address, section) else {
            bail!("the data of `{name}` isn't in the file");
        };
        let len = meta::frame_len(bytes).into_diagnostic()?;
        let Some(frame) = bytes.get(..len) else {
            bail!("`{name}` is truncated");
        };
        frames.extend(meta::decode(frame).into_diagnostic()?);
    }
    Ok(frames)
}

/// The file's bytes from `address` to the end of the section holding it
/// (`section`, when the symbol names it).
fn data_at<'d>(
    file: &object::File<'d>,
    address: u64,
    section: Option<object::SectionIndex>,
) -> Option<&'d [u8]> {
    let section = match section {
        Some(index) => file.section_by_index(index).ok()?,
        None => file
            .sections()
            .find(|s| s.address() <= address && address < s.address().saturating_add(s.size()))?,
    };
    let data = section.data().ok()?;
    let offset = usize::try_from(address.checked_sub(section.address())?).ok()?;
    data.get(offset..)
}

/// The base name a library file is loaded by: `libkv.so`, `libkv.dylib`,
/// `libkv.a`, `kv.dll`, `kv.lib`, and `kv.wasm` are all `kv`.
#[must_use]
pub(crate) fn library_name(path: &Utf8Path) -> String {
    let stem = path.file_stem().unwrap_or_default();
    match path.extension() {
        Some("so" | "dylib" | "a") => stem.strip_prefix("lib").unwrap_or(stem).to_string(),
        _ => stem.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_names_drop_platform_decoration() {
        for (file, name) in [
            ("target/debug/libkv_store.so", "kv_store"),
            ("libkv.dylib", "kv"),
            ("libkv.a", "kv"),
            ("kv.dll", "kv"),
            ("kv.lib", "kv"),
            ("out/kv.wasm", "kv"),
        ] {
            assert_eq!(library_name(Utf8Path::new(file)), name, "{file}");
        }
    }

    #[test]
    fn files_without_metadata_have_no_frames_and_junk_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let junk = Utf8Path::from_path(dir.path()).unwrap().join("libjunk.so");
        std::fs::write(&junk, b"\x00fake-native\x01").unwrap();
        let err = read_frames(&junk).unwrap_err();
        assert!(format!("{err:#}").contains("isn't a library"), "{err:#}");
        // A real object file with no frames: this test binary.
        let me = std::env::current_exe().unwrap();
        let me = Utf8Path::from_path(&me).unwrap();
        assert!(read_frames(me).unwrap().is_empty());
    }
}
