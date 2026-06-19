//! The target-platform model that the packaging pipeline shares.
//!
//! `weaveffi generate` emits binding *source*; `weaveffi package` goes further
//! and assembles publishable packages that bundle a prebuilt native library for
//! each platform. To do that without every backend re-deriving the same facts,
//! this module is the single source of truth for the supported platforms and
//! the per-ecosystem identifiers each one maps to:
//!
//! * the Rust target triple (`aarch64-apple-darwin`, …) used by `--build`;
//! * the shared-library file name (`libfoo.dylib`, `foo.dll`, …);
//! * the NuGet runtime identifier (`osx-arm64`, …);
//! * the Node.js `process.platform`/`process.arch` tokens;
//! * the Python wheel platform tag (`macosx_11_0_arm64`, …); and
//! * the RubyGems platform string (`arm64-darwin`, …).
//!
//! A [`BinarySet`] pairs each [`Platform`] with the on-disk path to its
//! prebuilt library; the [`crate::package`] driver and every packaging backend
//! consume it.

use camino::Utf8PathBuf;

/// The operating-system family of a [`Platform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Os {
    /// Apple platforms (macOS); shared libraries are `.dylib`.
    MacOs,
    /// Linux with the GNU C library (glibc); shared libraries are `.so`.
    Linux,
    /// Windows; shared libraries are `.dll`.
    Windows,
}

/// The CPU architecture of a [`Platform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Arch {
    /// 64-bit x86 (`x86_64` / `amd64`).
    X64,
    /// 64-bit ARM (`aarch64` / `arm64`).
    Arm64,
}

/// A single native target platform WeaveFFI can build for and bundle a
/// prebuilt library into a published package.
///
/// The v1 matrix is macOS (arm64 and x64), Linux glibc (x64 and arm64), and
/// Windows (x64). Each variant carries a stable [`id`](Self::id) used both as
/// the `--platforms` token and as the per-platform subdirectory name in the
/// `--binaries` input layout (`<dir>/<id>/<library>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    /// macOS on Apple silicon (`aarch64-apple-darwin`).
    MacosArm64,
    /// macOS on Intel (`x86_64-apple-darwin`).
    MacosX64,
    /// Linux glibc on x86-64 (`x86_64-unknown-linux-gnu`).
    LinuxX64,
    /// Linux glibc on ARM64 (`aarch64-unknown-linux-gnu`).
    LinuxArm64,
    /// Windows on x86-64 (`x86_64-pc-windows-msvc`).
    WindowsX64,
}

impl Platform {
    /// Every platform in the v1 support matrix, in a stable order.
    pub const ALL: [Platform; 5] = [
        Platform::MacosArm64,
        Platform::MacosX64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
        Platform::WindowsX64,
    ];

    /// The stable WeaveFFI platform identifier, used as the `--platforms` token
    /// and the `--binaries` subdirectory name (`darwin-arm64`, `darwin-x64`,
    /// `linux-x64`, `linux-arm64`, `windows-x64`).
    pub fn id(self) -> &'static str {
        match self {
            Platform::MacosArm64 => "darwin-arm64",
            Platform::MacosX64 => "darwin-x64",
            Platform::LinuxX64 => "linux-x64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::WindowsX64 => "windows-x64",
        }
    }

    /// Parse a [`Platform`] from its [`id`](Self::id), returning `None` for an
    /// unrecognized token.
    pub fn from_id(s: &str) -> Option<Platform> {
        Platform::ALL.into_iter().find(|p| p.id() == s)
    }

    /// The operating-system family.
    pub fn os(self) -> Os {
        match self {
            Platform::MacosArm64 | Platform::MacosX64 => Os::MacOs,
            Platform::LinuxX64 | Platform::LinuxArm64 => Os::Linux,
            Platform::WindowsX64 => Os::Windows,
        }
    }

    /// The CPU architecture.
    pub fn arch(self) -> Arch {
        match self {
            Platform::MacosArm64 | Platform::LinuxArm64 => Arch::Arm64,
            Platform::MacosX64 | Platform::LinuxX64 | Platform::WindowsX64 => Arch::X64,
        }
    }

    /// The Rust target triple used to cross-compile a producer for this
    /// platform with `weaveffi package --build`.
    pub fn rust_target(self) -> &'static str {
        match self {
            Platform::MacosArm64 => "aarch64-apple-darwin",
            Platform::MacosX64 => "x86_64-apple-darwin",
            Platform::LinuxX64 => "x86_64-unknown-linux-gnu",
            Platform::LinuxArm64 => "aarch64-unknown-linux-gnu",
            Platform::WindowsX64 => "x86_64-pc-windows-msvc",
        }
    }

    /// Resolve a [`Platform`] from a Rust target triple, returning `None` for a
    /// triple outside the support matrix.
    pub fn from_rust_target(triple: &str) -> Option<Platform> {
        Platform::ALL.into_iter().find(|p| p.rust_target() == triple)
    }

    /// The shared-library filename prefix: `"lib"` on Unix, empty on Windows.
    pub fn lib_prefix(self) -> &'static str {
        match self.os() {
            Os::MacOs | Os::Linux => "lib",
            Os::Windows => "",
        }
    }

    /// The shared-library filename extension (without the dot): `dylib`, `so`,
    /// or `dll`.
    pub fn lib_extension(self) -> &'static str {
        match self.os() {
            Os::MacOs => "dylib",
            Os::Linux => "so",
            Os::Windows => "dll",
        }
    }

    /// The platform-correct shared-library filename for a logical base name.
    ///
    /// `Platform::MacosArm64.lib_filename("contacts")` is `libcontacts.dylib`;
    /// `Platform::WindowsX64.lib_filename("contacts")` is `contacts.dll`.
    pub fn lib_filename(self, base: &str) -> String {
        format!("{}{base}.{}", self.lib_prefix(), self.lib_extension())
    }

    /// The NuGet runtime identifier (RID) for the `runtimes/<rid>/native/`
    /// layout: `osx-arm64`, `osx-x64`, `linux-x64`, `linux-arm64`, `win-x64`.
    pub fn nuget_rid(self) -> &'static str {
        match self {
            Platform::MacosArm64 => "osx-arm64",
            Platform::MacosX64 => "osx-x64",
            Platform::LinuxX64 => "linux-x64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::WindowsX64 => "win-x64",
        }
    }

    /// The Node.js `process.platform` value (`darwin`, `linux`, `win32`).
    pub fn node_os(self) -> &'static str {
        match self.os() {
            Os::MacOs => "darwin",
            Os::Linux => "linux",
            Os::Windows => "win32",
        }
    }

    /// The Node.js `process.arch` value (`arm64`, `x64`).
    pub fn node_cpu(self) -> &'static str {
        match self.arch() {
            Arch::Arm64 => "arm64",
            Arch::X64 => "x64",
        }
    }

    /// The Python wheel platform tag (the final segment of a wheel filename),
    /// for example `macosx_11_0_arm64` or `manylinux2014_x86_64`.
    pub fn python_platform_tag(self) -> &'static str {
        match self {
            Platform::MacosArm64 => "macosx_11_0_arm64",
            Platform::MacosX64 => "macosx_10_12_x86_64",
            Platform::LinuxX64 => "manylinux2014_x86_64",
            Platform::LinuxArm64 => "manylinux2014_aarch64",
            Platform::WindowsX64 => "win_amd64",
        }
    }

    /// The RubyGems platform string used for a precompiled platform gem, for
    /// example `arm64-darwin` or `x86_64-linux`.
    pub fn ruby_platform(self) -> &'static str {
        match self {
            Platform::MacosArm64 => "arm64-darwin",
            Platform::MacosX64 => "x86_64-darwin",
            Platform::LinuxX64 => "x86_64-linux",
            Platform::LinuxArm64 => "aarch64-linux",
            Platform::WindowsX64 => "x64-mingw-ucrt",
        }
    }

    /// A short human-readable label (`macOS arm64`, `Linux x64`, …) for
    /// progress and diagnostic messages.
    pub fn display_name(self) -> &'static str {
        match self {
            Platform::MacosArm64 => "macOS arm64",
            Platform::MacosX64 => "macOS x64",
            Platform::LinuxX64 => "Linux x64",
            Platform::LinuxArm64 => "Linux arm64",
            Platform::WindowsX64 => "Windows x64",
        }
    }
}

/// One prebuilt native library: the [`Platform`] it targets and the on-disk
/// path to the shared library file to bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeBinary {
    /// The platform this library was built for.
    pub platform: Platform,
    /// Absolute or relative path to the shared library file on disk.
    pub source: Utf8PathBuf,
}

/// The set of prebuilt native libraries to bundle into a package, keyed by
/// platform.
///
/// `lib_name` is the logical base name every generated loader, import name, and
/// bundled filename is derived from (for example `contacts`, yielding
/// `libcontacts.dylib` / `contacts.dll`). It is the resolved package identity,
/// not the WeaveFFI brand, so the bundled file matches what the producer emits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySet {
    /// Logical shared-library base name (resolved package identity).
    pub lib_name: String,
    /// One entry per platform that has a prebuilt library available.
    pub binaries: Vec<NativeBinary>,
}

impl BinarySet {
    /// Create a set with the given logical library base name and no binaries.
    pub fn new(lib_name: impl Into<String>) -> Self {
        Self {
            lib_name: lib_name.into(),
            binaries: Vec::new(),
        }
    }

    /// Record the prebuilt library `source` for `platform`, replacing any
    /// previous entry for the same platform.
    pub fn insert(&mut self, platform: Platform, source: impl Into<Utf8PathBuf>) {
        let source = source.into();
        if let Some(existing) = self.binaries.iter_mut().find(|b| b.platform == platform) {
            existing.source = source;
        } else {
            self.binaries.push(NativeBinary { platform, source });
        }
    }

    /// The library built for `platform`, if present.
    pub fn get(&self, platform: Platform) -> Option<&NativeBinary> {
        self.binaries.iter().find(|b| b.platform == platform)
    }

    /// Every platform with a bundled library, in insertion order.
    pub fn platforms(&self) -> impl Iterator<Item = Platform> + '_ {
        self.binaries.iter().map(|b| b.platform)
    }

    /// True when no binaries have been recorded.
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
    }

    /// The bundled filename for `platform` under this set's `lib_name`
    /// (`libcontacts.dylib`, `contacts.dll`, …).
    pub fn bundled_filename(&self, platform: Platform) -> String {
        platform.lib_filename(&self.lib_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for p in Platform::ALL {
            assert_eq!(Platform::from_id(p.id()), Some(p));
        }
        assert_eq!(Platform::from_id("nonsense"), None);
    }

    #[test]
    fn rust_targets_round_trip() {
        for p in Platform::ALL {
            assert_eq!(Platform::from_rust_target(p.rust_target()), Some(p));
        }
        assert_eq!(Platform::from_rust_target("mips-unknown-linux-gnu"), None);
    }

    #[test]
    fn lib_filenames_are_platform_correct() {
        assert_eq!(Platform::MacosArm64.lib_filename("contacts"), "libcontacts.dylib");
        assert_eq!(Platform::LinuxX64.lib_filename("contacts"), "libcontacts.so");
        assert_eq!(Platform::WindowsX64.lib_filename("contacts"), "contacts.dll");
    }

    #[test]
    fn ecosystem_identifiers() {
        assert_eq!(Platform::MacosArm64.nuget_rid(), "osx-arm64");
        assert_eq!(Platform::WindowsX64.nuget_rid(), "win-x64");
        assert_eq!(Platform::MacosX64.node_os(), "darwin");
        assert_eq!(Platform::MacosX64.node_cpu(), "x64");
        assert_eq!(Platform::WindowsX64.node_os(), "win32");
        assert_eq!(Platform::LinuxArm64.python_platform_tag(), "manylinux2014_aarch64");
        assert_eq!(Platform::MacosArm64.ruby_platform(), "arm64-darwin");
        assert_eq!(Platform::LinuxX64.ruby_platform(), "x86_64-linux");
    }

    #[test]
    fn binary_set_insert_get_and_replace() {
        let mut set = BinarySet::new("contacts");
        assert!(set.is_empty());
        set.insert(Platform::MacosArm64, "/tmp/a/libcontacts.dylib");
        set.insert(Platform::LinuxX64, "/tmp/b/libcontacts.so");
        assert_eq!(set.binaries.len(), 2);

        // Re-inserting the same platform replaces rather than duplicates.
        set.insert(Platform::MacosArm64, "/tmp/c/libcontacts.dylib");
        assert_eq!(set.binaries.len(), 2);
        assert_eq!(
            set.get(Platform::MacosArm64).unwrap().source.as_str(),
            "/tmp/c/libcontacts.dylib"
        );

        let platforms: Vec<Platform> = set.platforms().collect();
        assert_eq!(platforms, vec![Platform::MacosArm64, Platform::LinuxX64]);
        assert_eq!(set.bundled_filename(Platform::WindowsX64), "contacts.dll");
    }
}
