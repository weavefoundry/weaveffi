//! Cross-compiling a Rust producer for each platform: `weaveffi build`.
//!
//! [`Builder`] runs `cargo rustc --lib` once per [`Platform`] with the
//! artifact kinds that platform's packages need (a `cdylib` for desktop and
//! Android, plus a `staticlib` for an `XCFramework` on Apple platforms, a
//! `staticlib` alone for iOS, and a `.wasm` module for `wasm32`), so a
//! producer's `Cargo.toml` needs no particular `crate-type`. It fixes what a
//! plain `cargo build` leaves machine-specific (a macOS library's install
//! name becomes `@rpath/lib{library}.dylib`, a Linux or Android library's
//! soname `lib{library}.so`), sets Apple deployment targets and the Android
//! NDK toolchain, and lays the results out in
//! `{target_dir}/weaveffi/<platform>/`, where [`glue`] adds the prebuilt
//! Node.js addon and JNI shim and `weaveffi package` reads them back as a
//! [`BinarySet`](crate::platform::BinarySet).

pub(crate) mod glue;
pub(crate) mod ndk;

use std::process::{Command, Stdio};
use std::sync::OnceLock;

use camino::Utf8PathBuf;
use miette::{bail, IntoDiagnostic, Result, WrapErr};

use crate::cargo::{cargo_bin, CargoCrate};
use crate::platform::{NativeBinary, Os, Platform};

use self::ndk::Ndk;

/// Build settings shared by every platform (the `[build]` table).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BuildSettings {
    /// The Cargo profile to build with (`--profile`).
    pub(crate) profile: String,
    /// The minimum macOS version (`MACOSX_DEPLOYMENT_TARGET`).
    pub(crate) macos_deployment_target: String,
    /// The minimum iOS version (`IPHONEOS_DEPLOYMENT_TARGET`).
    pub(crate) ios_deployment_target: String,
    /// The minimum Android API level, which selects the NDK compiler.
    pub(crate) android_api: u32,
}

impl Default for BuildSettings {
    /// Release builds for macOS 11.0, iOS 13.0, and Android API 21.
    fn default() -> Self {
        Self {
            profile: "release".into(),
            macos_deployment_target: "11.0".into(),
            ios_deployment_target: "13.0".into(),
            android_api: 21,
        }
    }
}

/// Cross-compiles one producer crate into `{target_dir}/weaveffi/<platform>/`.
pub(crate) struct Builder<'a> {
    krate: &'a CargoCrate,
    library: &'a str,
    settings: &'a BuildSettings,
    quiet: bool,
    installed_targets: OnceLock<Option<Vec<String>>>,
    ndk: OnceLock<Result<Ndk, String>>,
}

impl<'a> Builder<'a> {
    /// A builder for `krate` whose outputs are named after `library` (the
    /// identity's library, which the generated bindings load).
    pub(crate) fn new(
        krate: &'a CargoCrate,
        library: &'a str,
        settings: &'a BuildSettings,
    ) -> Self {
        Self {
            krate,
            library,
            settings,
            quiet: false,
            installed_targets: OnceLock::new(),
            ndk: OnceLock::new(),
        }
    }

    /// Pass `--quiet` to Cargo.
    #[must_use]
    pub(crate) fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// The directory a platform's outputs are laid out in:
    /// `{target_dir}/weaveffi/<platform-id>`.
    #[must_use]
    pub(crate) fn output_dir(&self, platform: Platform) -> Utf8PathBuf {
        self.krate.weaveffi_dir().join(platform.id())
    }

    /// The Android NDK, located once.
    ///
    /// # Errors
    ///
    /// Returns an error when no NDK is found.
    pub(crate) fn ndk(&self) -> Result<&Ndk> {
        self.ndk
            .get_or_init(|| Ndk::locate().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| miette::miette!("{e}"))
    }

    /// Check, before compiling anything, that `platform` can be built here:
    /// a suitable host, an installed rustup target, and (for Android) an
    /// NDK.
    ///
    /// # Errors
    ///
    /// Returns an error naming the missing piece and how to add it.
    pub(crate) fn check(&self, platform: Platform) -> Result<()> {
        platform.check_host()?;
        let triple = platform.rust_target();
        if let Some(installed) = self.installed_targets() {
            if !installed.iter().any(|t| t == triple) {
                bail!(
                    "the Rust target `{triple}` ({}) isn't installed; run `rustup target add \
                     {triple}`",
                    platform.id()
                );
            }
        }
        if platform.os() == Os::Android {
            self.ndk()?.clang(platform, self.settings.android_api)?;
        }
        Ok(())
    }

    /// The rustup targets installed for the crate's toolchain, or `None`
    /// when rustup isn't available (a distribution's Rust, for example).
    fn installed_targets(&self) -> Option<&Vec<String>> {
        self.installed_targets
            .get_or_init(|| {
                let dir = self.krate.manifest_path.parent()?;
                let out = Command::new("rustup")
                    .args(["target", "list", "--installed"])
                    .current_dir(dir)
                    .output()
                    .ok()?;
                out.status.success().then(|| {
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .map(|l| l.trim().to_string())
                        .collect()
                })
            })
            .as_ref()
    }

    /// Cross-compile the crate for `platform` and lay the results out in
    /// [`output_dir`](Self::output_dir), replacing anything there.
    ///
    /// # Errors
    ///
    /// Returns an error when [`check`](Self::check) fails, Cargo fails, or
    /// Cargo produces none of the expected libraries.
    pub(crate) fn build(&self, platform: Platform) -> Result<NativeBinary> {
        self.check(platform)?;
        let triple = platform.rust_target();
        let library = self.library;
        let crate_types = match platform.os() {
            Os::Ios => "staticlib",
            Os::MacOs => "cdylib,staticlib",
            _ => "cdylib",
        };
        let mut link_args: Vec<String> = Vec::new();
        match platform.os() {
            Os::MacOs => {
                link_args.push(format!("-Wl,-install_name,@rpath/lib{library}.dylib"));
            }
            Os::Linux => link_args.push(format!("-Wl,-soname,lib{library}.so")),
            Os::Android => {
                link_args.push(format!("-Wl,-soname,lib{library}.so"));
                link_args.push("-Wl,-z,max-page-size=16384".into());
            }
            Os::Wasm => {
                link_args.push("--export-table".into());
                link_args.push("--growable-table".into());
            }
            Os::Windows | Os::Ios => {}
        }

        let mut cmd = Command::new(cargo_bin());
        cmd.args(["rustc", "--lib", "--message-format=json-render-diagnostics"])
            .arg("--manifest-path")
            .arg(self.krate.manifest_path.as_str())
            .args(["--target", triple, "--crate-type", crate_types]);
        cmd.args(["--profile", &self.settings.profile]);
        if self.quiet {
            cmd.arg("--quiet");
        }
        if !link_args.is_empty() {
            cmd.arg("--");
            for arg in &link_args {
                cmd.arg("-C").arg(format!("link-arg={arg}"));
            }
        }
        match platform.os() {
            Os::MacOs => {
                cmd.env(
                    "MACOSX_DEPLOYMENT_TARGET",
                    &self.settings.macos_deployment_target,
                );
            }
            Os::Ios => {
                cmd.env(
                    "IPHONEOS_DEPLOYMENT_TARGET",
                    &self.settings.ios_deployment_target,
                );
            }
            Os::Android => {
                let ndk = self.ndk()?;
                let clang = ndk.clang(platform, self.settings.android_api)?;
                let env_triple = triple.replace('-', "_");
                cmd.env(
                    format!("CARGO_TARGET_{}_LINKER", env_triple.to_uppercase()),
                    clang.as_str(),
                )
                .env(format!("CC_{env_triple}"), clang.as_str())
                .env(format!("AR_{env_triple}"), ndk.ar()?.as_str());
            }
            Os::Linux | Os::Windows | Os::Wasm => {}
        }
        let output = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .into_diagnostic()
            .wrap_err("failed to run cargo")?;
        if !output.status.success() {
            bail!(
                "cargo failed to build `{}` for {} ({triple}); see its errors above",
                self.krate.name,
                platform.id()
            );
        }
        let produced = self.krate.lib_artifacts(&output.stdout);

        let dir = self.output_dir(platform);
        if dir.exists() {
            std::fs::remove_dir_all(dir.as_std_path())
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to clear {dir}"))?;
        }
        std::fs::create_dir_all(dir.as_std_path())
            .into_diagnostic()
            .wrap_err_with(|| format!("failed to create {dir}"))?;
        let place = |suffix: &str, name: String| -> Result<Option<Utf8PathBuf>> {
            let Some(source) = produced.iter().find(|p| p.as_str().ends_with(suffix)) else {
                return Ok(None);
            };
            let dest = dir.join(name);
            std::fs::copy(source.as_std_path(), dest.as_std_path())
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to copy {source} to {dest}"))?;
            Ok(Some(dest))
        };
        let primary = platform.lib_filename(library);
        let (suffix, name) = match platform.os() {
            Os::MacOs => (".dylib", primary),
            Os::Linux | Os::Android => (".so", primary),
            Os::Windows => (".dll", primary),
            Os::Wasm => (".wasm", primary),
            Os::Ios => (".a", primary),
        };
        let Some(lib) = place(suffix, name)? else {
            bail!(
                "cargo built `{}` for {triple} but produced no {suffix} library",
                self.krate.name
            );
        };
        let mut binary = NativeBinary::new(platform, lib);
        match platform.os() {
            Os::Ios => binary.staticlib = Some(binary.library.clone()),
            Os::MacOs => binary.staticlib = place(".a", format!("lib{library}.a"))?,
            Os::Windows => binary.import_library = place(".dll.lib", format!("{library}.dll.lib"))?,
            _ => {}
        }
        Ok(binary)
    }
}
