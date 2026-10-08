//! The target-platform model that the packaging pipeline shares.
//!
//! `weaveffi build` cross-compiles a producer once per platform and
//! `weaveffi package` bundles the results into each ecosystem's artifacts. To
//! do that without every backend re-deriving the same facts, this module is
//! the single source of truth for the supported platforms and the
//! per-ecosystem identifiers each one maps to:
//!
//! * the Rust target triple (`aarch64-apple-darwin`, …);
//! * the shared-library file name (`libfoo.dylib`, `foo.dll`, …);
//! * the NuGet runtime identifier (`osx-arm64`, …);
//! * the Node.js `process.platform`/`process.arch` tokens;
//! * the Python wheel platform tag (`macosx_11_0_arm64`, …); and
//! * the RubyGems platform string (`arm64-darwin`, …).
//!
//! A [`BinarySet`] records, per [`Platform`], the files `weaveffi build` laid
//! out in `target/weaveffi/<platform>/`: the producer library plus any
//! prebuilt glue (the Node.js addon, the JNI shim, the static library an
//! `XCFramework` needs). The [`crate::package`] driver and every packaging
//! backend consume it.

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

/// The operating-system family of a [`Platform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Os {
    /// Apple platforms (macOS); shared libraries are `.dylib`.
    MacOs,
    /// Linux with the GNU C library (glibc); shared libraries are `.so`.
    Linux,
    /// Windows; shared libraries are `.dll`.
    Windows,
    /// iOS devices and simulators; libraries are static (`.a`), assembled
    /// into an `XCFramework`.
    Ios,
    /// Android (Bionic); shared libraries are `.so` bundled under `jniLibs`.
    Android,
    /// WebAssembly; the "library" is a standalone `.wasm` module.
    Wasm,
}

/// The CPU architecture of a [`Platform`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Arch {
    /// 64-bit x86 (`x86_64` / `amd64`).
    X64,
    /// 64-bit ARM (`aarch64` / `arm64`).
    Arm64,
    /// 32-bit WebAssembly (`wasm32`).
    Wasm32,
}

/// A single native target platform WeaveFFI can build for and bundle a
/// prebuilt library into a published package.
///
/// The desktop matrix is macOS (arm64 and x64), Linux glibc (x64 and arm64),
/// and Windows (x64); the mobile and web additions are Android (arm64 and x64)
/// and `wasm32`. Each variant carries a stable [`id`](Self::id) used both as
/// the `--platforms` token and as the per-platform subdirectory name in the
/// `--binaries` input layout (`<dir>/<id>/<library>`).
///
/// Not every ecosystem publishes for every platform: a NuGet package has no
/// Android RID, a wheel has no wasm tag. The ecosystem accessors return
/// `Option` and a packaging backend skips binaries it has no slot for.
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
    /// iOS on device (`aarch64-apple-ios`), static library.
    IosArm64,
    /// iOS simulator on Apple silicon (`aarch64-apple-ios-sim`), static library.
    IosSimArm64,
    /// iOS simulator on Intel (`x86_64-apple-ios`), static library.
    IosSimX64,
    /// Android on ARM64 (`aarch64-linux-android`, jniLibs `arm64-v8a`).
    AndroidArm64,
    /// Android on x86-64 (`x86_64-linux-android`, jniLibs `x86_64`), the
    /// emulator target.
    AndroidX64,
    /// Bare WebAssembly (`wasm32-unknown-unknown`).
    Wasm32,
}

impl Platform {
    /// Every supported platform, in a stable order.
    pub const ALL: [Platform; 11] = [
        Platform::MacosArm64,
        Platform::MacosX64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
        Platform::WindowsX64,
        Platform::IosArm64,
        Platform::IosSimArm64,
        Platform::IosSimX64,
        Platform::AndroidArm64,
        Platform::AndroidX64,
        Platform::Wasm32,
    ];

    /// The desktop platforms every dynamic-library ecosystem (NuGet, npm,
    /// wheels, gems, JVM desktop) can bundle, in a stable order.
    pub const DESKTOP: [Platform; 5] = [
        Platform::MacosArm64,
        Platform::MacosX64,
        Platform::LinuxX64,
        Platform::LinuxArm64,
        Platform::WindowsX64,
    ];

    /// The platform this process runs on, if it is a packaging platform.
    #[must_use]
    pub fn host() -> Option<Platform> {
        let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
            ("macos", "aarch64") => "aarch64-apple-darwin",
            ("macos", "x86_64") => "x86_64-apple-darwin",
            ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
            ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
            ("windows", "x86_64") => "x86_64-pc-windows-msvc",
            _ => return None,
        };
        Platform::from_rust_target(triple)
    }

    /// The stable WeaveFFI platform identifier, used as the `--platforms` token
    /// and the `--binaries` subdirectory name (`darwin-arm64`, `darwin-x64`,
    /// `linux-x64`, `linux-arm64`, `windows-x64`, `android-arm64`,
    /// `android-x64`, `wasm32`).
    pub fn id(self) -> &'static str {
        match self {
            Platform::MacosArm64 => "darwin-arm64",
            Platform::MacosX64 => "darwin-x64",
            Platform::LinuxX64 => "linux-x64",
            Platform::LinuxArm64 => "linux-arm64",
            Platform::WindowsX64 => "windows-x64",
            Platform::IosArm64 => "ios-arm64",
            Platform::IosSimArm64 => "ios-sim-arm64",
            Platform::IosSimX64 => "ios-sim-x64",
            Platform::AndroidArm64 => "android-arm64",
            Platform::AndroidX64 => "android-x64",
            Platform::Wasm32 => "wasm32",
        }
    }

    /// Whether this is one of the [`DESKTOP`](Self::DESKTOP) platforms.
    pub fn is_desktop(self) -> bool {
        Platform::DESKTOP.contains(&self)
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
            Platform::IosArm64 | Platform::IosSimArm64 | Platform::IosSimX64 => Os::Ios,
            Platform::AndroidArm64 | Platform::AndroidX64 => Os::Android,
            Platform::Wasm32 => Os::Wasm,
        }
    }

    /// The CPU architecture.
    pub fn arch(self) -> Arch {
        match self {
            Platform::MacosArm64
            | Platform::LinuxArm64
            | Platform::AndroidArm64
            | Platform::IosArm64
            | Platform::IosSimArm64 => Arch::Arm64,
            Platform::MacosX64
            | Platform::LinuxX64
            | Platform::WindowsX64
            | Platform::AndroidX64
            | Platform::IosSimX64 => Arch::X64,
            Platform::Wasm32 => Arch::Wasm32,
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
            Platform::IosArm64 => "aarch64-apple-ios",
            Platform::IosSimArm64 => "aarch64-apple-ios-sim",
            Platform::IosSimX64 => "x86_64-apple-ios",
            Platform::AndroidArm64 => "aarch64-linux-android",
            Platform::AndroidX64 => "x86_64-linux-android",
            Platform::Wasm32 => "wasm32-unknown-unknown",
        }
    }

    /// Resolve a [`Platform`] from a Rust target triple, returning `None` for a
    /// triple outside the support matrix.
    pub fn from_rust_target(triple: &str) -> Option<Platform> {
        Platform::ALL
            .into_iter()
            .find(|p| p.rust_target() == triple)
    }

    /// The shared-library filename prefix: `"lib"` on Unix-like targets
    /// (including Android), empty on Windows and for a `.wasm` module.
    pub fn lib_prefix(self) -> &'static str {
        match self.os() {
            Os::MacOs | Os::Linux | Os::Android | Os::Ios => "lib",
            Os::Windows | Os::Wasm => "",
        }
    }

    /// The shared-library filename extension (without the dot): `dylib`, `so`,
    /// `dll`, or `wasm`.
    pub fn lib_extension(self) -> &'static str {
        match self.os() {
            Os::MacOs => "dylib",
            Os::Ios => "a",
            Os::Linux | Os::Android => "so",
            Os::Windows => "dll",
            Os::Wasm => "wasm",
        }
    }

    /// The Android `jniLibs/<abi>/` directory name (`arm64-v8a`, `x86_64`),
    /// or `None` off Android.
    pub fn android_abi(self) -> Option<&'static str> {
        match self {
            Platform::AndroidArm64 => Some("arm64-v8a"),
            Platform::AndroidX64 => Some("x86_64"),
            _ => None,
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
    /// layout (`osx-arm64`, `osx-x64`, `linux-x64`, `linux-arm64`, `win-x64`),
    /// or `None` for a platform NuGet has no RID for.
    pub fn nuget_rid(self) -> Option<&'static str> {
        match self {
            Platform::MacosArm64 => Some("osx-arm64"),
            Platform::MacosX64 => Some("osx-x64"),
            Platform::LinuxX64 => Some("linux-x64"),
            Platform::LinuxArm64 => Some("linux-arm64"),
            Platform::WindowsX64 => Some("win-x64"),
            Platform::AndroidArm64
            | Platform::AndroidX64
            | Platform::Wasm32
            | Platform::IosArm64
            | Platform::IosSimArm64
            | Platform::IosSimX64 => None,
        }
    }

    /// The Node.js `process.platform` value (`darwin`, `linux`, `win32`), or
    /// `None` where Node does not run.
    pub fn node_os(self) -> Option<&'static str> {
        match self.os() {
            Os::MacOs => Some("darwin"),
            Os::Linux => Some("linux"),
            Os::Windows => Some("win32"),
            Os::Android | Os::Wasm | Os::Ios => None,
        }
    }

    /// The Node.js `process.arch` value (`arm64`, `x64`), or `None` for an
    /// architecture Node has no token for.
    pub fn node_cpu(self) -> Option<&'static str> {
        match self.arch() {
            Arch::Arm64 => Some("arm64"),
            Arch::X64 => Some("x64"),
            Arch::Wasm32 => None,
        }
    }

    /// The Python wheel platform tag (the final segment of a wheel filename),
    /// or `None` for a platform wheels don't target.
    ///
    /// A macOS tag carries the deployment target the library was built for
    /// (`"11.0"` gives `macosx_11_0_arm64`); a Linux tag carries the newest
    /// glibc the library links against (`(2, 17)` gives
    /// `manylinux_2_17_x86_64`, see [`glibc_requirement`]).
    #[must_use]
    pub fn python_platform_tag(
        self,
        macos_deployment_target: &str,
        glibc: (u32, u32),
    ) -> Option<String> {
        let macos = |arch: &str| {
            let mut parts = macos_deployment_target.split('.');
            let major = parts.next().filter(|p| !p.is_empty()).unwrap_or("11");
            let minor = parts.next().unwrap_or("0");
            format!("macosx_{major}_{minor}_{arch}")
        };
        let linux = |arch: &str| format!("manylinux_{}_{}_{arch}", glibc.0, glibc.1);
        match self {
            Platform::MacosArm64 => Some(macos("arm64")),
            Platform::MacosX64 => Some(macos("x86_64")),
            Platform::LinuxX64 => Some(linux("x86_64")),
            Platform::LinuxArm64 => Some(linux("aarch64")),
            Platform::WindowsX64 => Some("win_amd64".to_string()),
            Platform::AndroidArm64
            | Platform::AndroidX64
            | Platform::Wasm32
            | Platform::IosArm64
            | Platform::IosSimArm64
            | Platform::IosSimX64 => None,
        }
    }

    /// Check that this host can build this platform.
    ///
    /// Apple platforms need a macOS host (the Apple SDKs), Windows needs a
    /// Windows host (MSVC), and Linux needs a Linux host (another
    /// architecture also needs a cross linker). Android builds anywhere the
    /// NDK runs and `wasm32` builds anywhere.
    ///
    /// # Errors
    ///
    /// Returns an error naming the host the platform needs.
    pub fn check_host(self) -> Result<()> {
        let (needed, host_name) = match self.os() {
            Os::MacOs | Os::Ios => ("macos", "macOS"),
            Os::Windows => ("windows", "Windows"),
            Os::Linux => ("linux", "Linux"),
            Os::Android | Os::Wasm => return Ok(()),
        };
        if std::env::consts::OS != needed {
            bail!(
                "{} ({}) can only be built on a {host_name} host; build it on a {host_name} \
                 machine or CI runner and hand the results to `weaveffi package --binaries`",
                self.id(),
                self.display_name(),
            );
        }
        Ok(())
    }

    /// Parse a comma-separated list of platform ids, dropping duplicates and
    /// keeping the first-seen order.
    ///
    /// # Errors
    ///
    /// Returns an error naming an unknown id (with the valid ones), or when
    /// the list is empty.
    pub fn parse_list<S: AsRef<str>>(ids: &[S]) -> Result<Vec<Platform>> {
        let mut out = Vec::new();
        for token in ids
            .iter()
            .map(|s| s.as_ref().trim())
            .filter(|s| !s.is_empty())
        {
            let Some(p) = Platform::from_id(token) else {
                let known: Vec<&str> = Platform::ALL.iter().map(|p| p.id()).collect();
                bail!(
                    "unknown platform `{token}`; expected one of: {}",
                    known.join(", ")
                );
            };
            if !out.contains(&p) {
                out.push(p);
            }
        }
        if out.is_empty() {
            bail!("the platform list is empty; expected one or more platform ids");
        }
        Ok(out)
    }

    /// The RubyGems platform string used for a precompiled platform gem, for
    /// example `arm64-darwin` or `x86_64-linux`, or `None` for a platform gems
    /// do not target.
    pub fn ruby_platform(self) -> Option<&'static str> {
        match self {
            Platform::MacosArm64 => Some("arm64-darwin"),
            Platform::MacosX64 => Some("x86_64-darwin"),
            Platform::LinuxX64 => Some("x86_64-linux"),
            Platform::LinuxArm64 => Some("aarch64-linux"),
            Platform::WindowsX64 => Some("x64-mingw-ucrt"),
            Platform::AndroidArm64
            | Platform::AndroidX64
            | Platform::Wasm32
            | Platform::IosArm64
            | Platform::IosSimArm64
            | Platform::IosSimX64 => None,
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
            Platform::IosArm64 => "iOS arm64",
            Platform::IosSimArm64 => "iOS simulator arm64",
            Platform::IosSimX64 => "iOS simulator x64",
            Platform::AndroidArm64 => "Android arm64",
            Platform::AndroidX64 => "Android x64",
            Platform::Wasm32 => "WebAssembly (wasm32)",
        }
    }
}

/// The Node.js addon's base name for a library: `{library}_node`, built as
/// `{library}_node.node`.
#[must_use]
pub fn node_addon_name(library: &str) -> String {
    format!("{library}_node")
}

/// The JNI shim's base name for a library: `{library}_jni`, built as
/// `lib{library}_jni.so` (or `.dylib`, or `.dll`).
#[must_use]
pub fn jni_shim_name(library: &str) -> String {
    format!("{library}_jni")
}

/// The newest glibc version a Linux shared library links against, read from
/// the `GLIBC_2.x` symbol-version names in its bytes, and never older than
/// 2.17 (the oldest glibc Rust supports).
///
/// This picks the `manylinux_2_N` tag of a wheel: a library built on a
/// recent distribution needs that distribution's glibc, whatever the target
/// triple's minimum.
#[must_use]
pub fn glibc_requirement(library: &[u8]) -> (u32, u32) {
    const NEEDLE: &[u8] = b"GLIBC_2.";
    let mut best = (2, 17);
    let mut rest = library;
    while let Some(at) = rest.windows(NEEDLE.len()).position(|w| w == NEEDLE) {
        rest = &rest[at + NEEDLE.len()..];
        let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
        if let Some(minor) = std::str::from_utf8(&rest[..digits])
            .ok()
            .and_then(|d| d.parse::<u32>().ok())
        {
            best = best.max((2, minor));
        }
    }
    best
}

/// The files `weaveffi build` laid out for one platform in
/// `target/weaveffi/<platform>/`.
///
/// Only [`library`](Self::library) is always present: the producer's shared
/// library (the `.wasm` module on `wasm32`, the static library on iOS). The
/// rest are prebuilt extras that packaging bundles when it finds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeBinary {
    /// The platform these files were built for.
    pub platform: Platform,
    /// The producer library: `lib{library}.dylib`, `lib{library}.so`,
    /// `{library}.dll`, `{library}.wasm`, or (iOS) `lib{library}.a`.
    pub library: Utf8PathBuf,
    /// The static library (`lib{library}.a`) an `XCFramework` slice is made
    /// from, on Apple platforms.
    pub staticlib: Option<Utf8PathBuf>,
    /// The import library (`{library}.dll.lib`) MSVC links a DLL through, on
    /// Windows.
    pub import_library: Option<Utf8PathBuf>,
    /// The prebuilt Node.js addon (`{library}_node.node`).
    pub node_addon: Option<Utf8PathBuf>,
    /// The prebuilt JNI shim (`lib{library}_jni.so`, `.dylib`, or `.dll`).
    pub jni_shim: Option<Utf8PathBuf>,
}

impl NativeBinary {
    /// A platform with only its producer library.
    pub fn new(platform: Platform, library: impl Into<Utf8PathBuf>) -> Self {
        Self {
            platform,
            library: library.into(),
            staticlib: None,
            import_library: None,
            node_addon: None,
            jni_shim: None,
        }
    }

    /// Read the files of one `target/weaveffi/<platform>/` directory, or
    /// `None` when it has no producer library named for `lib_name`.
    #[must_use]
    pub fn read_dir(dir: &Utf8Path, platform: Platform, lib_name: &str) -> Option<Self> {
        let library = dir.join(platform.lib_filename(lib_name));
        if !library.is_file() {
            return None;
        }
        let existing = |name: String| {
            let path = dir.join(name);
            path.is_file().then_some(path)
        };
        let mut binary = Self::new(platform, library);
        binary.staticlib = match platform.os() {
            Os::Ios => Some(binary.library.clone()),
            Os::MacOs => existing(format!("lib{lib_name}.a")),
            _ => None,
        };
        if platform.os() == Os::Windows {
            binary.import_library = existing(format!("{lib_name}.dll.lib"));
        }
        binary.node_addon = existing(format!("{}.node", node_addon_name(lib_name)));
        if !matches!(platform.os(), Os::Ios | Os::Wasm) {
            binary.jni_shim = existing(platform.lib_filename(&jni_shim_name(lib_name)));
        }
        Some(binary)
    }
}

/// The per-platform build outputs to bundle into packages, keyed by platform.
///
/// `lib_name` is the logical base name every generated loader, import name,
/// and bundled filename derives from (for example `contacts`, yielding
/// `libcontacts.dylib` and `contacts.dll`). It is the resolved package
/// identity's library, so the bundled file matches what the bindings load.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinarySet {
    /// Logical shared-library base name (the identity's `library`).
    pub lib_name: String,
    /// One entry per platform with a build.
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

    /// Read the build outputs under `dir` (laid out as `dir/<platform-id>/`)
    /// for each of `platforms`, or for every platform with a directory when
    /// `platforms` is `None`.
    ///
    /// # Errors
    ///
    /// Returns an error when `dir` isn't a directory, or when a requested
    /// platform has no producer library under `dir`.
    pub fn read_dir(
        dir: &Utf8Path,
        lib_name: &str,
        platforms: Option<&[Platform]>,
    ) -> Result<Self> {
        if !dir.is_dir() {
            bail!("{dir} isn't a directory of per-platform builds (`<dir>/<platform>/`)");
        }
        let mut set = Self::new(lib_name);
        for platform in platforms.unwrap_or(&Platform::ALL) {
            let platform_dir = dir.join(platform.id());
            match NativeBinary::read_dir(&platform_dir, *platform, lib_name) {
                Some(binary) => set.insert(binary),
                None if platforms.is_some() => bail!(
                    "no {} in {platform_dir}; build {} first (`weaveffi build --platforms {}`)",
                    platform.lib_filename(lib_name),
                    platform.id(),
                    platform.id()
                ),
                None => {}
            }
        }
        if set.is_empty() {
            bail!(
                "{dir} has no per-platform builds of `{lib_name}`; expected \
                 `{dir}/<platform>/{}` or similar",
                Platform::host()
                    .unwrap_or(Platform::LinuxX64)
                    .lib_filename(lib_name)
            );
        }
        Ok(set)
    }

    /// Record `binary`, replacing any previous entry for the same platform.
    pub fn insert(&mut self, binary: NativeBinary) {
        if let Some(existing) = self
            .binaries
            .iter_mut()
            .find(|b| b.platform == binary.platform)
        {
            *existing = binary;
        } else {
            self.binaries.push(binary);
        }
    }

    /// The build for `platform`, if present.
    pub fn get(&self, platform: Platform) -> Option<&NativeBinary> {
        self.binaries.iter().find(|b| b.platform == platform)
    }

    /// Every platform with a build, in insertion order.
    pub fn platforms(&self) -> impl Iterator<Item = Platform> + '_ {
        self.binaries.iter().map(|b| b.platform)
    }

    /// True when no builds have been recorded.
    pub fn is_empty(&self) -> bool {
        self.binaries.is_empty()
    }

    /// The bundled filename for `platform` under this set's `lib_name`
    /// (`libcontacts.dylib`, `contacts.dll`, …).
    pub fn bundled_filename(&self, platform: Platform) -> String {
        platform.lib_filename(&self.lib_name)
    }
}

/// Read a whole file, naming it in the error.
///
/// # Errors
///
/// Returns an error when the file can't be read.
pub fn read_file(path: &Utf8Path) -> Result<Vec<u8>> {
    std::fs::read(path.as_std_path()).with_context(|| format!("failed to read {path}"))
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
        assert_eq!(
            Platform::MacosArm64.lib_filename("contacts"),
            "libcontacts.dylib"
        );
        assert_eq!(
            Platform::LinuxX64.lib_filename("contacts"),
            "libcontacts.so"
        );
        assert_eq!(
            Platform::WindowsX64.lib_filename("contacts"),
            "contacts.dll"
        );
        assert_eq!(
            Platform::AndroidArm64.lib_filename("contacts"),
            "libcontacts.so"
        );
        assert_eq!(Platform::Wasm32.lib_filename("contacts"), "contacts.wasm");
    }

    #[test]
    fn ecosystem_identifiers() {
        assert_eq!(Platform::MacosArm64.nuget_rid(), Some("osx-arm64"));
        assert_eq!(Platform::WindowsX64.nuget_rid(), Some("win-x64"));
        assert_eq!(Platform::MacosX64.node_os(), Some("darwin"));
        assert_eq!(Platform::MacosX64.node_cpu(), Some("x64"));
        assert_eq!(Platform::WindowsX64.node_os(), Some("win32"));
        assert_eq!(
            Platform::LinuxArm64
                .python_platform_tag("11.0", (2, 17))
                .as_deref(),
            Some("manylinux_2_17_aarch64")
        );
        assert_eq!(
            Platform::MacosArm64
                .python_platform_tag("12.3", (2, 17))
                .as_deref(),
            Some("macosx_12_3_arm64")
        );
        assert_eq!(
            Platform::WindowsX64
                .python_platform_tag("11.0", (2, 17))
                .as_deref(),
            Some("win_amd64")
        );
        assert_eq!(Platform::MacosArm64.ruby_platform(), Some("arm64-darwin"));
        assert_eq!(Platform::LinuxX64.ruby_platform(), Some("x86_64-linux"));
        assert_eq!(Platform::AndroidArm64.android_abi(), Some("arm64-v8a"));
        assert_eq!(Platform::AndroidX64.android_abi(), Some("x86_64"));
        assert_eq!(Platform::MacosArm64.android_abi(), None);
        for p in [
            Platform::AndroidArm64,
            Platform::AndroidX64,
            Platform::Wasm32,
        ] {
            assert!(!p.is_desktop());
            assert_eq!(p.nuget_rid(), None);
            assert_eq!(p.python_platform_tag("11.0", (2, 17)), None);
            assert_eq!(p.ruby_platform(), None);
        }
        assert!(Platform::DESKTOP.iter().all(|p| p.is_desktop()));
    }

    #[test]
    fn binary_set_insert_get_and_replace() {
        let mut set = BinarySet::new("contacts");
        assert!(set.is_empty());
        set.insert(NativeBinary::new(
            Platform::MacosArm64,
            "/a/libcontacts.dylib",
        ));
        set.insert(NativeBinary::new(Platform::LinuxX64, "/b/libcontacts.so"));
        assert_eq!(set.binaries.len(), 2);

        // Re-inserting the same platform replaces rather than duplicates.
        set.insert(NativeBinary::new(
            Platform::MacosArm64,
            "/c/libcontacts.dylib",
        ));
        assert_eq!(set.binaries.len(), 2);
        assert_eq!(
            set.get(Platform::MacosArm64).unwrap().library.as_str(),
            "/c/libcontacts.dylib"
        );

        let platforms: Vec<Platform> = set.platforms().collect();
        assert_eq!(platforms, vec![Platform::MacosArm64, Platform::LinuxX64]);
        assert_eq!(set.bundled_filename(Platform::WindowsX64), "contacts.dll");
    }

    #[test]
    fn build_directories_are_read_with_their_extras() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap();
        let mac = root.join("darwin-arm64");
        let linux = root.join("linux-x64");
        std::fs::create_dir_all(&mac).unwrap();
        std::fs::create_dir_all(&linux).unwrap();
        for f in [
            "libcontacts.dylib",
            "libcontacts.a",
            "contacts_node.node",
            "libcontacts_jni.dylib",
        ] {
            std::fs::write(mac.join(f), b"x").unwrap();
        }
        std::fs::write(linux.join("libcontacts.so"), b"x").unwrap();

        let set = BinarySet::read_dir(root, "contacts", None).unwrap();
        assert_eq!(set.binaries.len(), 2);
        let m = set.get(Platform::MacosArm64).unwrap();
        assert_eq!(
            m.staticlib.as_deref(),
            Some(mac.join("libcontacts.a").as_path())
        );
        assert!(m.node_addon.is_some() && m.jni_shim.is_some());
        let l = set.get(Platform::LinuxX64).unwrap();
        assert!(l.staticlib.is_none() && l.node_addon.is_none() && l.jni_shim.is_none());

        let err = BinarySet::read_dir(root, "contacts", Some(&[Platform::WindowsX64])).unwrap_err();
        assert!(err.to_string().contains("contacts.dll"), "{err}");
        assert!(BinarySet::read_dir(&root.join("missing"), "contacts", None).is_err());
    }

    #[test]
    fn platform_lists_parse_and_reject_unknown_ids() {
        assert_eq!(
            Platform::parse_list(&["darwin-arm64", " linux-x64", "darwin-arm64"]).unwrap(),
            [Platform::MacosArm64, Platform::LinuxX64]
        );
        let err = Platform::parse_list(&["solaris-sparc"]).unwrap_err();
        assert!(
            err.to_string().contains("unknown platform `solaris-sparc`"),
            "{err}"
        );
        assert!(Platform::parse_list::<&str>(&[]).is_err());
    }

    #[test]
    fn host_checks_name_the_needed_host() {
        assert!(Platform::Wasm32.check_host().is_ok());
        assert!(Platform::AndroidArm64.check_host().is_ok());
        let foreign = if cfg!(target_os = "windows") {
            Platform::MacosArm64
        } else {
            Platform::WindowsX64
        };
        let err = foreign.check_host().unwrap_err();
        assert!(err.to_string().contains("can only be built on"), "{err}");
    }

    #[test]
    fn glibc_requirement_takes_the_newest_version_named() {
        assert_eq!(glibc_requirement(b"nothing here"), (2, 17));
        assert_eq!(
            glibc_requirement(b"\0GLIBC_2.2.5\0GLIBC_2.28\0GLIBC_2.3.4\0"),
            (2, 28)
        );
    }
}
