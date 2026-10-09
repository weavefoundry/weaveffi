//! Locating the Android NDK and its per-API-level compilers.

use camino::{Utf8Path, Utf8PathBuf};
use miette::{bail, IntoDiagnostic, Result, WrapErr};

use crate::platform::Platform;

/// An Android NDK installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ndk {
    /// The NDK's root directory (holding `toolchains/` and
    /// `source.properties`).
    pub root: Utf8PathBuf,
}

impl Ndk {
    /// Find the NDK: `ANDROID_NDK_HOME`, then `ANDROID_NDK_ROOT`, then the
    /// newest `ndk/<version>` (or `ndk-bundle`) of the Android SDK named by
    /// `ANDROID_HOME` or `ANDROID_SDK_ROOT`, or installed at the Android
    /// Studio default location.
    ///
    /// # Errors
    ///
    /// Returns an error explaining how to install or point at an NDK when
    /// none is found.
    pub fn locate() -> Result<Self> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        for name in ["ANDROID_NDK_HOME", "ANDROID_NDK_ROOT"] {
            if let Some(dir) = var(name) {
                let root = Utf8PathBuf::from(dir);
                if !root.join("toolchains").is_dir() {
                    bail!("{name} is set to {root}, which isn't an Android NDK (no toolchains/)");
                }
                return Ok(Self { root });
            }
        }
        let home = var("HOME")
            .or_else(|| var("USERPROFILE"))
            .map(Utf8PathBuf::from);
        let mut sdks: Vec<Utf8PathBuf> = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
            .into_iter()
            .filter_map(|n| var(n).map(Utf8PathBuf::from))
            .collect();
        if let Some(home) = &home {
            sdks.push(home.join("Library/Android/sdk"));
            sdks.push(home.join("Android/Sdk"));
        }
        if let Some(local) = var("LOCALAPPDATA") {
            sdks.push(Utf8PathBuf::from(local).join("Android/Sdk"));
        }
        for sdk in &sdks {
            if let Some(root) = newest_version(&sdk.join("ndk")) {
                return Ok(Self { root });
            }
            let bundle = sdk.join("ndk-bundle");
            if bundle.join("toolchains").is_dir() {
                return Ok(Self { root: bundle });
            }
        }
        Err(miette::miette!(
            "the Android NDK wasn't found: set ANDROID_NDK_HOME to an NDK directory, or install \
             one with the Android SDK manager (`sdkmanager --install \"ndk;27.2.12479018\"`) so \
             it lands in $ANDROID_HOME/ndk/<version>"
        ))
    }

    /// The directory of the NDK's LLVM toolchain binaries for this host.
    ///
    /// # Errors
    ///
    /// Returns an error when the NDK has no prebuilt LLVM toolchain.
    pub fn bin_dir(&self) -> Result<Utf8PathBuf> {
        let prebuilt = self.root.join("toolchains/llvm/prebuilt");
        let host = prebuilt
            .read_dir_utf8()
            .into_diagnostic()
            .wrap_err_with(|| format!("the NDK at {} has no {prebuilt}", self.root))?
            .filter_map(Result::ok)
            .map(|e| e.path().to_path_buf())
            .find(|p| p.join("bin").is_dir());
        match host {
            Some(host) => Ok(host.join("bin")),
            None => Err(miette::miette!(
                "the NDK at {} has no LLVM toolchain under {prebuilt}",
                self.root
            )),
        }
    }

    /// The C compiler driver targeting `platform` at Android `api_level`
    /// (`aarch64-linux-android21-clang`).
    ///
    /// # Errors
    ///
    /// Returns an error when the NDK has no compiler for that platform and
    /// API level.
    pub fn clang(&self, platform: Platform, api_level: u32) -> Result<Utf8PathBuf> {
        let suffix = if cfg!(windows) { ".cmd" } else { "" };
        let clang = self.bin_dir()?.join(format!(
            "{}{api_level}-clang{suffix}",
            platform.rust_target()
        ));
        if !clang.is_file() {
            bail!(
                "the NDK at {} has no compiler for {} at API level {api_level} ({clang}); \
                 check `[build] android_api`",
                self.root,
                platform.id()
            );
        }
        Ok(clang)
    }

    /// The NDK's `llvm-ar`.
    ///
    /// # Errors
    ///
    /// Returns an error when the NDK has no prebuilt LLVM toolchain.
    pub fn ar(&self) -> Result<Utf8PathBuf> {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        Ok(self.bin_dir()?.join(format!("llvm-ar{suffix}")))
    }
}

/// The newest NDK under an SDK's `ndk/` directory, comparing the numeric
/// components of each version directory's name.
fn newest_version(dir: &Utf8Path) -> Option<Utf8PathBuf> {
    let key = |name: &str| -> Vec<u64> {
        name.split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    dir.read_dir_utf8()
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().to_path_buf())
        .filter(|p| p.join("toolchains").is_dir())
        .max_by_key(|p| key(p.file_name().unwrap_or("")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_newest_ndk_version_wins() {
        let dir = tempfile::tempdir().unwrap();
        let ndk = Utf8Path::from_path(dir.path()).unwrap().join("ndk");
        for v in ["25.2.9519653", "27.1.12297006", "9.0.1", "notes"] {
            let toolchains = ndk.join(v).join("toolchains");
            if v != "notes" {
                std::fs::create_dir_all(toolchains).unwrap();
            } else {
                std::fs::create_dir_all(ndk.join(v)).unwrap();
            }
        }
        assert_eq!(newest_version(&ndk), Some(ndk.join("27.1.12297006")));
        assert_eq!(newest_version(&ndk.join("missing")), None);
    }

    #[test]
    fn compilers_are_named_by_triple_and_api_level() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8Path::from_path(dir.path()).unwrap().to_path_buf();
        let bin = root.join("toolchains/llvm/prebuilt/test-host/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let suffix = if cfg!(windows) { ".cmd" } else { "" };
        std::fs::write(
            bin.join(format!("aarch64-linux-android24-clang{suffix}")),
            "",
        )
        .unwrap();
        let ndk = Ndk { root };
        assert_eq!(
            ndk.clang(Platform::AndroidArm64, 24).unwrap(),
            bin.join(format!("aarch64-linux-android24-clang{suffix}"))
        );
        let err = ndk.clang(Platform::AndroidArm64, 19).unwrap_err();
        assert!(err.to_string().contains("android_api"), "{err}");
    }
}
