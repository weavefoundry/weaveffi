//! Archive writers for the installable artifacts: gzip-compressed tarballs
//! (npm, C and C++), zip archives (`XCFramework`), Python wheels, and RubyGems
//! packages.
//!
//! Every writer builds the archive in memory and is deterministic: entries
//! keep their given order, timestamps are fixed (npm's 1985-10-26), and
//! owners are root. The same inputs therefore always produce the same bytes,
//! so checksums (a SwiftPM binary target's, a wheel's `RECORD`) are stable
//! across runs.

use std::borrow::Cow;
use std::io::Write as _;

use miette::{bail, IntoDiagnostic, Result, WrapErr};
use sha2::{Digest, Sha256, Sha512};

use crate::package::PackagedFile;

/// The modification time of every archive entry: 1985-10-26T08:15:00Z, the
/// fixed timestamp `npm pack` uses.
const MTIME: u64 = 499_162_500;

/// [`MTIME`] as an MS-DOS date and time, for zip entries.
const DOS_DATE: u16 = ((1985 - 1980) << 9) | (10 << 5) | 26;
const DOS_TIME: u16 = (8 << 11) | (15 << 5);

/// One file inside an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry<'a> {
    /// The path inside the archive, `/`-separated.
    pub path: String,
    /// The file's bytes.
    pub data: Cow<'a, [u8]>,
    /// Whether the file is executable (native libraries and addons are).
    pub executable: bool,
}

impl<'a> Entry<'a> {
    /// An entry at `path` holding `data`, executable when its name looks like
    /// a native library or addon.
    pub fn new(path: impl Into<String>, data: impl Into<Cow<'a, [u8]>>) -> Self {
        let path = path.into();
        let executable = is_native(&path);
        Self {
            path,
            data: data.into(),
            executable,
        }
    }

    fn mode(&self) -> u32 {
        if self.executable {
            0o755
        } else {
            0o644
        }
    }
}

/// Whether `path` names a native library or addon, which archives mark
/// executable.
fn is_native(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    [".so", ".dylib", ".dll", ".node", ".a"]
        .iter()
        .any(|ext| name.ends_with(ext))
}

/// The archive entries for `files`, read from memory or disk, each placed
/// under `prefix/` when given.
///
/// # Errors
///
/// Returns an error when a copied file can't be read.
pub fn entries<'a>(files: &'a [PackagedFile], prefix: Option<&str>) -> Result<Vec<Entry<'a>>> {
    files
        .iter()
        .map(|f| {
            let path = match prefix {
                Some(p) => format!("{p}/{}", f.path),
                None => f.path.to_string(),
            };
            Ok(Entry::new(path, f.content.bytes()?))
        })
        .collect()
}

/// An uncompressed tar archive of `entries`.
///
/// # Errors
///
/// Returns an error when an entry's path can't be represented in a tar
/// header.
pub fn tar(entries: &[Entry<'_>]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for entry in entries {
        let mut header = tar::Header::new_ustar();
        header.set_size(entry.data.len() as u64);
        header.set_mode(entry.mode());
        header.set_mtime(MTIME);
        header.set_uid(0);
        header.set_gid(0);
        header.set_entry_type(tar::EntryType::Regular);
        builder
            .append_data(&mut header, &entry.path, entry.data.as_ref())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to add {} to a tar archive", entry.path))?;
    }
    builder
        .into_inner()
        .into_diagnostic()
        .wrap_err("failed to finish a tar archive")
}

/// `bytes` compressed with gzip (with a zeroed header timestamp).
///
/// # Errors
///
/// Returns an error when compression fails.
pub fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(bytes)
        .into_diagnostic()
        .wrap_err("failed to gzip")?;
    encoder
        .finish()
        .into_diagnostic()
        .wrap_err("failed to gzip")
}

/// A zip archive of `entries`, each deflated, with Unix permissions.
///
/// # Errors
///
/// Returns an error when an entry is too large for a non-Zip64 archive or
/// compression fails.
pub fn zip(entries: &[Entry<'_>]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(&entry.data)
            .into_diagnostic()
            .wrap_err("failed to deflate")?;
        let compressed = encoder
            .finish()
            .into_diagnostic()
            .wrap_err("failed to deflate")?;
        let crc = crc32fast::hash(&entry.data);
        let (Ok(size), Ok(packed), Ok(offset), Ok(name_len)) = (
            u32::try_from(entry.data.len()),
            u32::try_from(compressed.len()),
            u32::try_from(out.len()),
            u16::try_from(entry.path.len()),
        ) else {
            bail!("{} is too large for a zip archive", entry.path);
        };
        // Local file header: version 2.0, UTF-8 names, deflate.
        out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        out.extend_from_slice(&20u16.to_le_bytes());
        out.extend_from_slice(&0x0800u16.to_le_bytes());
        out.extend_from_slice(&8u16.to_le_bytes());
        out.extend_from_slice(&DOS_TIME.to_le_bytes());
        out.extend_from_slice(&DOS_DATE.to_le_bytes());
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&packed.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&name_len.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(entry.path.as_bytes());
        out.extend_from_slice(&compressed);
        // Central directory record: made by Unix (3) so the external
        // attributes carry the file mode.
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&((3u16 << 8) | 20).to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&8u16.to_le_bytes());
        central.extend_from_slice(&DOS_TIME.to_le_bytes());
        central.extend_from_slice(&DOS_DATE.to_le_bytes());
        central.extend_from_slice(&crc.to_le_bytes());
        central.extend_from_slice(&packed.to_le_bytes());
        central.extend_from_slice(&size.to_le_bytes());
        central.extend_from_slice(&name_len.to_le_bytes());
        central.extend_from_slice(&[0; 8]);
        central.extend_from_slice(&((0o100_000 | entry.mode()) << 16).to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(entry.path.as_bytes());
    }
    let (Ok(count), Ok(central_len), Ok(central_offset)) = (
        u16::try_from(entries.len()),
        u32::try_from(central.len()),
        u32::try_from(out.len()),
    ) else {
        bail!("too many entries for a zip archive");
    };
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&central_len.to_le_bytes());
    out.extend_from_slice(&central_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    Ok(out)
}

/// The lowercase hexadecimal SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// URL-safe base64 without padding, the digest encoding of a wheel's
/// `RECORD`.
fn base64_url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..=chunk.len() {
            out.push(char::from(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize]));
        }
    }
    out
}

/// The metadata of a Python wheel. The writer renders the `.dist-info`
/// directory (`METADATA`, `WHEEL`, `RECORD`) from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WheelMeta {
    /// The distribution name (`kvstore`).
    pub name: String,
    /// The version (`1.2.0`).
    pub version: String,
    /// The platform tag (`macosx_11_0_arm64`); the wheel is tagged
    /// `py3-none-{platform}`.
    pub platform: String,
    /// The one-line summary.
    pub summary: String,
    /// The `Requires-Python` specifier, if any.
    pub requires_python: Option<String>,
    /// The license expression, if any.
    pub license: Option<String>,
    /// The authors, joined with `, `, if any.
    pub author: Option<String>,
    /// `Project-URL` entries as `(label, url)` pairs.
    pub urls: Vec<(String, String)>,
    /// The long description (Markdown), if any.
    pub readme: Option<String>,
}

impl WheelMeta {
    /// The distribution name escaped for file names: runs of `-`, `_`, and
    /// `.` become `_`, lowercased.
    #[must_use]
    pub fn escaped_name(&self) -> String {
        let mut out = String::with_capacity(self.name.len());
        for c in self.name.chars() {
            if matches!(c, '-' | '_' | '.') {
                if !out.ends_with('_') {
                    out.push('_');
                }
            } else {
                out.extend(c.to_lowercase());
            }
        }
        out
    }

    /// The full compatibility tag, `py3-none-{platform}`.
    #[must_use]
    pub fn tag(&self) -> String {
        format!("py3-none-{}", self.platform)
    }

    /// The wheel's file name: `{name}-{version}-py3-none-{platform}.whl`.
    #[must_use]
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}-{}.whl",
            self.escaped_name(),
            self.version,
            self.tag()
        )
    }

    /// The `.dist-info` directory name.
    #[must_use]
    pub fn dist_info(&self) -> String {
        format!("{}-{}.dist-info", self.escaped_name(), self.version)
    }

    fn metadata(&self) -> String {
        let mut out = format!(
            "Metadata-Version: 2.1\nName: {}\nVersion: {}\nSummary: {}\n",
            self.name,
            self.version,
            one_line(&self.summary)
        );
        if let Some(r) = &self.requires_python {
            out.push_str(&format!("Requires-Python: {r}\n"));
        }
        if let Some(l) = &self.license {
            out.push_str(&format!("License: {}\n", one_line(l)));
        }
        if let Some(a) = &self.author {
            out.push_str(&format!("Author: {}\n", one_line(a)));
        }
        for (label, url) in &self.urls {
            out.push_str(&format!("Project-URL: {label}, {url}\n"));
        }
        if let Some(readme) = &self.readme {
            out.push_str("Description-Content-Type: text/markdown\n\n");
            out.push_str(readme);
        }
        out
    }

    fn wheel(&self) -> String {
        format!(
            "Wheel-Version: 1.0\nGenerator: weaveffi ({})\nRoot-Is-Purelib: false\nTag: {}\n",
            env!("CARGO_PKG_VERSION"),
            self.tag()
        )
    }
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A Python wheel holding `files` plus the `.dist-info` files rendered from
/// `meta`, with a `RECORD` of every file's SHA-256 and size.
///
/// # Errors
///
/// Returns an error when a file can't be read or the archive can't be built.
pub fn wheel(meta: &WheelMeta, files: &[PackagedFile]) -> Result<Vec<u8>> {
    let dist_info = meta.dist_info();
    let mut all = entries(files, None)?;
    all.push(Entry::new(
        format!("{dist_info}/METADATA"),
        meta.metadata().into_bytes(),
    ));
    all.push(Entry::new(
        format!("{dist_info}/WHEEL"),
        meta.wheel().into_bytes(),
    ));
    let mut record = String::new();
    for entry in &all {
        record.push_str(&format!(
            "{},sha256={},{}\n",
            csv_field(&entry.path),
            base64_url(&Sha256::digest(&entry.data)),
            entry.data.len()
        ));
    }
    let record_path = format!("{dist_info}/RECORD");
    record.push_str(&format!("{},,\n", csv_field(&record_path)));
    all.push(Entry::new(record_path, record.into_bytes()));
    zip(&all)
}

fn csv_field(s: &str) -> Cow<'_, str> {
    if s.contains([',', '"', '\n']) {
        Cow::Owned(format!("\"{}\"", s.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(s)
    }
}

/// The specification of a RubyGems package. The writer renders it as the
/// gem's YAML `metadata.gz`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GemSpec {
    /// The gem name.
    pub name: String,
    /// The version.
    pub version: String,
    /// The RubyGems platform (`x86_64-linux`, `arm64-darwin`), or `ruby` for
    /// a pure gem.
    pub platform: String,
    /// The one-line summary.
    pub summary: String,
    /// The authors (RubyGems requires at least one).
    pub authors: Vec<String>,
    /// The license, if any.
    pub license: Option<String>,
    /// The homepage, if any.
    pub homepage: Option<String>,
    /// The `required_ruby_version` requirement (`>= 2.7`).
    pub required_ruby_version: String,
    /// Runtime dependencies as `(name, requirement)` pairs
    /// (`("ffi", "~> 1.15")`).
    pub dependencies: Vec<(String, String)>,
}

impl GemSpec {
    /// The gem's file name: `{name}-{version}-{platform}.gem`, or
    /// `{name}-{version}.gem` for a pure gem.
    #[must_use]
    pub fn file_name(&self) -> String {
        if self.platform == "ruby" {
            format!("{}-{}.gem", self.name, self.version)
        } else {
            format!("{}-{}-{}.gem", self.name, self.version, self.platform)
        }
    }

    /// The specification as the YAML document RubyGems stores in
    /// `metadata.gz`, listing `files`.
    #[must_use]
    pub fn to_yaml(&self, files: &[String]) -> String {
        let requirement = |req: &str, indent: &str| {
            let (op, version) = req.split_once(' ').unwrap_or((">=", req));
            format!(
                "{indent}requirements:\n{indent}- - {}\n{indent}  - !ruby/object:Gem::Version\n\
                 {indent}    version: {}\n",
                yaml_str(op),
                yaml_str(version)
            )
        };
        let list = |items: &[String]| {
            if items.is_empty() {
                " []\n".to_string()
            } else {
                let mut out = "\n".to_string();
                for item in items {
                    out.push_str(&format!("- {}\n", yaml_str(item)));
                }
                out
            }
        };
        let mut out = format!(
            "--- !ruby/object:Gem::Specification\nname: {}\nversion: !ruby/object:Gem::Version\n  \
             version: {}\nplatform: {}\nauthors:{}autorequire:\nbindir: bin\ncert_chain: []\n\
             date: 1985-10-26 00:00:00.000000000 Z\ndependencies:",
            yaml_str(&self.name),
            yaml_str(&self.version),
            yaml_str(&self.platform),
            list(&self.authors),
        );
        if self.dependencies.is_empty() {
            out.push_str(" []\n");
        } else {
            out.push('\n');
            for (name, req) in &self.dependencies {
                out.push_str(&format!(
                    "- !ruby/object:Gem::Dependency\n  name: {}\n  requirement: \
                     !ruby/object:Gem::Requirement\n{}  type: :runtime\n  prerelease: false\n  \
                     version_requirements: !ruby/object:Gem::Requirement\n{}",
                    yaml_str(name),
                    requirement(req, "    "),
                    requirement(req, "    "),
                ));
            }
        }
        out.push_str(&format!(
            "description:\nemail:\nexecutables: []\nextensions: []\nextra_rdoc_files: []\nfiles:{}",
            list(files)
        ));
        out.push_str(&format!(
            "homepage:{}\nlicenses:{}metadata: {{}}\npost_install_message:\nrdoc_options: []\n\
             require_paths:\n- lib\nrequired_ruby_version: !ruby/object:Gem::Requirement\n{}\
             required_rubygems_version: !ruby/object:Gem::Requirement\n{}requirements: []\n\
             rubygems_version: 3.4.10\nsigning_key:\nspecification_version: 4\nsummary: {}\n\
             test_files: []\n",
            self.homepage
                .as_deref()
                .map(|h| format!(" {}", yaml_str(h)))
                .unwrap_or_default(),
            list(&self.license.iter().cloned().collect::<Vec<_>>()),
            requirement(&self.required_ruby_version, "  "),
            requirement(">= 0", "  "),
            yaml_str(&one_line(&self.summary)),
        ));
        out
    }
}

/// A single-quoted YAML scalar.
fn yaml_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A RubyGems package holding `files`: a tar of the gzipped YAML
/// specification (`metadata.gz`), the gzipped data tarball
/// (`data.tar.gz`), and their SHA-256 and SHA-512 checksums
/// (`checksums.yaml.gz`).
///
/// # Errors
///
/// Returns an error when a file can't be read or an archive can't be built.
pub fn gem(spec: &GemSpec, files: &[PackagedFile]) -> Result<Vec<u8>> {
    let names: Vec<String> = files.iter().map(|f| f.path.to_string()).collect();
    let metadata = gzip(spec.to_yaml(&names).as_bytes())?;
    let data = gzip(&tar(&entries(files, None)?)?)?;
    let checksums = format!(
        "---\nSHA256:\n  metadata.gz: '{}'\n  data.tar.gz: '{}'\nSHA512:\n  metadata.gz: '{}'\n  \
         data.tar.gz: '{}'\n",
        hex(&Sha256::digest(&metadata)),
        hex(&Sha256::digest(&data)),
        hex(&Sha512::digest(&metadata)),
        hex(&Sha512::digest(&data)),
    );
    let checksums = gzip(checksums.as_bytes())?;
    let mut outer = Vec::new();
    for (name, bytes) in [
        ("metadata.gz", metadata),
        ("data.tar.gz", data),
        ("checksums.yaml.gz", checksums),
    ] {
        let mut entry = Entry::new(name, bytes);
        entry.executable = false;
        outer.push(entry);
    }
    tar(&outer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// Parse a zip written by [`zip`] back into `(name, mode, bytes)`
    /// triples by walking its central directory.
    fn unzip(bytes: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
        let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
        let u32_at = |at: usize| {
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
        };
        let eocd = bytes.len() - 22;
        assert_eq!(u32_at(eocd), 0x0605_4b50);
        let count = u16_at(eocd + 10);
        let mut at = u32_at(eocd + 16) as usize;
        let mut out = Vec::new();
        for _ in 0..count {
            assert_eq!(u32_at(at), 0x0201_4b50);
            let packed = u32_at(at + 20) as usize;
            let name_len = u16_at(at + 28);
            let mode = u32_at(at + 38) >> 16;
            let offset = u32_at(at + 42) as usize;
            let name = String::from_utf8(bytes[at + 46..at + 46 + name_len].to_vec()).unwrap();
            let data_at = offset + 30 + u16_at(offset + 26) + u16_at(offset + 28);
            let mut data = Vec::new();
            flate2::read::DeflateDecoder::new(&bytes[data_at..data_at + packed])
                .read_to_end(&mut data)
                .unwrap();
            assert_eq!(crc32fast::hash(&data), u32_at(at + 16));
            out.push((name, mode, data));
            at += 46 + name_len;
        }
        out
    }

    fn untar(bytes: &[u8]) -> Vec<(String, u32, Vec<u8>)> {
        let mut archive = tar::Archive::new(bytes);
        archive
            .entries()
            .unwrap()
            .map(|e| {
                let mut e = e.unwrap();
                let path = e.path().unwrap().to_string_lossy().into_owned();
                let mode = e.header().mode().unwrap();
                let mut data = Vec::new();
                e.read_to_end(&mut data).unwrap();
                (path, mode, data)
            })
            .collect()
    }

    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        flate2::read::GzDecoder::new(bytes)
            .read_to_end(&mut out)
            .unwrap();
        out
    }

    fn meta() -> WheelMeta {
        WheelMeta {
            name: "My.Kv-Store".into(),
            version: "1.2.0".into(),
            platform: "macosx_11_0_arm64".into(),
            summary: "An embedded\nkey-value store".into(),
            requires_python: Some(">=3.9".into()),
            license: Some("MIT".into()),
            author: Some("Ada".into()),
            urls: vec![("Repository".into(), "https://example.com/kv".into())],
            readme: Some("# kv\n".into()),
        }
    }

    #[test]
    fn base64_matches_the_reference_encoding() {
        assert_eq!(base64_url(b""), "");
        assert_eq!(base64_url(b"f"), "Zg");
        assert_eq!(base64_url(b"fo"), "Zm8");
        assert_eq!(base64_url(b"foo"), "Zm9v");
        assert_eq!(base64_url(&[0xfb, 0xff]), "-_8");
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn zips_round_trip_with_unix_modes() {
        let bytes = zip(&[
            Entry::new("a/b.txt", &b"hello"[..]),
            Entry::new("lib/libx.so", &b"\x7fELF"[..]),
        ])
        .unwrap();
        let files = unzip(&bytes);
        assert_eq!(files[0], ("a/b.txt".into(), 0o100_644, b"hello".to_vec()));
        assert_eq!(files[1].1, 0o100_755);
        assert_eq!(
            bytes,
            zip(&[
                Entry::new("a/b.txt", &b"hello"[..]),
                Entry::new("lib/libx.so", &b"\x7fELF"[..])
            ])
            .unwrap()
        );
    }

    #[test]
    fn wheels_carry_dist_info_tags_and_a_verifiable_record() {
        let m = meta();
        assert_eq!(
            m.file_name(),
            "my_kv_store-1.2.0-py3-none-macosx_11_0_arm64.whl"
        );
        let files = vec![
            PackagedFile::text("kv/__init__.py", "from .kv import *\n"),
            PackagedFile::bytes("kv/libkv.dylib", b"\xcf\xfa\xed\xfe".to_vec()),
        ];
        let entries = unzip(&wheel(&m, &files).unwrap());
        let names: Vec<&str> = entries.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(
            names,
            [
                "kv/__init__.py",
                "kv/libkv.dylib",
                "my_kv_store-1.2.0.dist-info/METADATA",
                "my_kv_store-1.2.0.dist-info/WHEEL",
                "my_kv_store-1.2.0.dist-info/RECORD",
            ]
        );
        let text = |name: &str| {
            String::from_utf8(entries.iter().find(|e| e.0 == name).unwrap().2.clone()).unwrap()
        };
        let wheel_file = text("my_kv_store-1.2.0.dist-info/WHEEL");
        assert!(
            wheel_file.contains("Root-Is-Purelib: false\n"),
            "{wheel_file}"
        );
        assert!(
            wheel_file.contains("Tag: py3-none-macosx_11_0_arm64\n"),
            "{wheel_file}"
        );
        let metadata = text("my_kv_store-1.2.0.dist-info/METADATA");
        assert!(metadata.starts_with("Metadata-Version: 2.1\nName: My.Kv-Store\nVersion: 1.2.0\n"));
        assert!(metadata.contains("Summary: An embedded key-value store\n"));
        assert!(metadata.contains("Requires-Python: >=3.9\n"));
        assert!(metadata.contains("Project-URL: Repository, https://example.com/kv\n"));
        assert!(metadata.ends_with("\n\n# kv\n"));

        // Every RECORD line but its own names a file with its real digest
        // and size.
        let record = text("my_kv_store-1.2.0.dist-info/RECORD");
        let lines: Vec<&str> = record.lines().collect();
        assert_eq!(lines.len(), entries.len());
        assert_eq!(
            *lines.last().unwrap(),
            "my_kv_store-1.2.0.dist-info/RECORD,,"
        );
        for line in &lines[..lines.len() - 1] {
            let mut parts = line.split(',');
            let (path, hash, size) = (
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
            );
            let data = &entries.iter().find(|e| e.0 == path).unwrap().2;
            assert_eq!(
                hash,
                format!("sha256={}", base64_url(&Sha256::digest(data)))
            );
            assert_eq!(size, data.len().to_string());
        }
    }

    #[test]
    fn npm_tarballs_prefix_every_entry_with_package() {
        let files = vec![
            PackagedFile::text("package.json", "{}"),
            PackagedFile::bytes("kv_node.node", b"addon".to_vec()),
        ];
        let tgz = gzip(&tar(&entries(&files, Some("package")).unwrap()).unwrap()).unwrap();
        let unpacked = untar(&gunzip(&tgz));
        assert_eq!(
            unpacked,
            [
                ("package/package.json".into(), 0o644, b"{}".to_vec()),
                ("package/kv_node.node".into(), 0o755, b"addon".to_vec()),
            ]
        );
        assert_eq!(
            tgz,
            gzip(&tar(&entries(&files, Some("package")).unwrap()).unwrap()).unwrap()
        );
    }

    #[test]
    fn gems_hold_metadata_data_and_checksums() {
        let spec = GemSpec {
            name: "kvstore".into(),
            version: "1.2.0".into(),
            platform: "x86_64-linux".into(),
            summary: "It's a store".into(),
            authors: vec!["Ada".into()],
            license: Some("MIT".into()),
            homepage: Some("https://example.com".into()),
            required_ruby_version: ">= 2.7".into(),
            dependencies: vec![("ffi".into(), "~> 1.15".into())],
        };
        assert_eq!(spec.file_name(), "kvstore-1.2.0-x86_64-linux.gem");
        let files = vec![
            PackagedFile::text("lib/kvstore.rb", "require 'ffi'\n"),
            PackagedFile::bytes("lib/native/libkvstore.so", b"elf".to_vec()),
        ];
        let outer = untar(&gem(&spec, &files).unwrap());
        let names: Vec<&str> = outer.iter().map(|e| e.0.as_str()).collect();
        assert_eq!(names, ["metadata.gz", "data.tar.gz", "checksums.yaml.gz"]);

        let yaml = String::from_utf8(gunzip(&outer[0].2)).unwrap();
        for needle in [
            "--- !ruby/object:Gem::Specification\nname: 'kvstore'\n",
            "version: !ruby/object:Gem::Version\n  version: '1.2.0'\n",
            "platform: 'x86_64-linux'\n",
            "authors:\n- 'Ada'\n",
            "- !ruby/object:Gem::Dependency\n  name: 'ffi'\n",
            "    - - '~>'\n      - !ruby/object:Gem::Version\n        version: '1.15'\n",
            "files:\n- 'lib/kvstore.rb'\n- 'lib/native/libkvstore.so'\n",
            "licenses:\n- 'MIT'\n",
            "required_ruby_version: !ruby/object:Gem::Requirement\n  requirements:\n  - - '>='\n",
            "summary: 'It''s a store'\n",
        ] {
            assert!(yaml.contains(needle), "missing {needle:?} in\n{yaml}");
        }

        let data = untar(&gunzip(&outer[1].2));
        assert_eq!(data[0].0, "lib/kvstore.rb");
        assert_eq!(
            (data[1].0.as_str(), data[1].1),
            ("lib/native/libkvstore.so", 0o755)
        );

        let checksums = String::from_utf8(gunzip(&outer[2].2)).unwrap();
        assert!(checksums.contains(&format!(
            "SHA256:\n  metadata.gz: '{}'\n  data.tar.gz: '{}'\n",
            sha256_hex(&outer[0].2),
            sha256_hex(&outer[1].2)
        )));
        assert!(checksums.contains("SHA512:\n"));
    }
}
