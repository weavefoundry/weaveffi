//! Prebuilding the per-language C glue, so consumers never compile.
//!
//! The Node.js and Kotlin targets reach the C ABI through a small C library
//! of their own: an N-API addon and a JNI shim. Both are generated as C
//! source (so a producer written in any language works), and `weaveffi
//! build` compiles them per platform next to the producer library, linked
//! against it with a relative run path, so packages ship them prebuilt.

use std::process::Command;

use anyhow::{bail, Context, Result};
use camino::{Utf8Path, Utf8PathBuf};

use crate::build::ndk::Ndk;
use crate::build::BuildSettings;
use crate::platform::{jni_shim_name, node_addon_name, Os, Platform};

/// One glue library to compile: its C source and the directory holding the
/// header it includes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlueSource {
    /// The C source file.
    pub source: Utf8PathBuf,
    /// The directory holding the producer's C header.
    pub include_dir: Utf8PathBuf,
}

/// Check that this host can compile desktop glue for `platform`: macOS
/// compiles both macOS architectures, Linux compiles its own architecture.
///
/// # Errors
///
/// Returns an error explaining which host can compile it instead.
pub fn check_desktop_host(platform: Platform) -> Result<()> {
    let ok = match platform.os() {
        Os::MacOs => cfg!(target_os = "macos"),
        Os::Linux => cfg!(target_os = "linux") && Platform::host() == Some(platform),
        _ => false,
    };
    if !ok {
        bail!(
            "this host can't compile glue for {}; build it on a {} machine or CI runner",
            platform.id(),
            platform.display_name()
        );
    }
    Ok(())
}

/// The C compiler for desktop glue: `$CC`, else `cc`.
fn desktop_cc() -> String {
    std::env::var("CC")
        .ok()
        .filter(|cc| !cc.is_empty())
        .unwrap_or_else(|| "cc".to_string())
}

/// Architecture and deployment-target flags for a macOS compile.
fn apple_flags(platform: Platform, settings: &BuildSettings) -> Vec<String> {
    let arch = match platform {
        Platform::MacosArm64 => "arm64",
        _ => "x86_64",
    };
    vec![
        "-arch".into(),
        arch.into(),
        format!("-mmacosx-version-min={}", settings.macos_deployment_target),
    ]
}

/// The directory holding Node.js's C headers (`node_api.h`, `uv.h`):
/// `$npm_config_nodedir/include/node`, the running Node.js installation's
/// `include/node`, or node-gyp's header cache for its version.
///
/// # Errors
///
/// Returns an error explaining how to make the headers available when none
/// are found.
pub fn node_headers() -> Result<Utf8PathBuf> {
    let has_headers =
        |dir: &Utf8Path| dir.join("node_api.h").is_file() && dir.join("uv.h").is_file();
    if let Ok(nodedir) = std::env::var("npm_config_nodedir") {
        let dir = Utf8PathBuf::from(nodedir).join("include/node");
        if has_headers(&dir) {
            return Ok(dir);
        }
    }
    let node = |expr: &str| -> Option<String> {
        let out = Command::new("node").args(["-p", expr]).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let Some(exec) = node("process.execPath") else {
        bail!("Node.js isn't installed (no `node` on PATH), so its addon headers can't be found");
    };
    let mut candidates = Vec::new();
    if let Some(prefix) = Utf8Path::new(&exec).parent().and_then(Utf8Path::parent) {
        candidates.push(prefix.join("include/node"));
    }
    if let Some(version) = node("process.versions.node") {
        let home = std::env::var("HOME").unwrap_or_default();
        for cache in ["Library/Caches/node-gyp", ".cache/node-gyp"] {
            candidates.push(
                Utf8PathBuf::from(&home)
                    .join(cache)
                    .join(&version)
                    .join("include/node"),
            );
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            candidates.push(
                Utf8PathBuf::from(local)
                    .join("node-gyp/Cache")
                    .join(&version)
                    .join("include/node"),
            );
        }
    }
    candidates
        .into_iter()
        .find(|d| has_headers(d))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Node.js's C headers (node_api.h) weren't found next to {exec}; install them with \
             `npx node-gyp install` or point npm_config_nodedir at a Node.js source or headers \
             directory"
            )
        })
}

/// Compile the Node.js addon `glue` for desktop `platform`, linked against
/// the producer library in `dir`, into `dir/{library}_node.node`.
///
/// # Errors
///
/// Returns an error when this host can't compile for `platform`, the
/// headers are missing, or the compiler fails.
pub fn compile_node_addon(
    glue: &GlueSource,
    platform: Platform,
    dir: &Utf8Path,
    library: &str,
    node_include: &Utf8Path,
    settings: &BuildSettings,
) -> Result<Utf8PathBuf> {
    if platform.os() == Os::Windows {
        bail!(
            "prebuilding the Node.js addon for windows-x64 isn't supported (it links node.lib); \
             the package falls back to node-gyp at install time"
        );
    }
    check_desktop_host(platform)?;
    let output = dir.join(format!("{}.node", node_addon_name(library)));
    let mut cmd = Command::new(desktop_cc());
    cmd.args(["-shared", "-fPIC", "-O2"])
        .arg(format!("-I{node_include}"))
        .arg(format!("-I{}", glue.include_dir))
        .arg(glue.source.as_str())
        .arg(format!("-L{dir}"))
        .arg(format!("-l{library}"))
        .arg("-o")
        .arg(output.as_str());
    if platform.os() == Os::MacOs {
        cmd.args(apple_flags(platform, settings))
            .args(["-undefined", "dynamic_lookup", "-Wl,-rpath,@loader_path"])
            .arg(format!(
                "-Wl,-install_name,@rpath/{}",
                output.file_name().unwrap_or_default()
            ));
    } else {
        cmd.arg("-Wl,-rpath,$ORIGIN");
    }
    run(cmd, "the Node.js addon", platform)?;
    Ok(output)
}

/// The JDK include directories JNI code compiles against:
/// `$JAVA_HOME/include` and its OS subdirectory, finding the JDK through
/// `/usr/libexec/java_home` or `javac` on PATH when `JAVA_HOME` is unset.
///
/// # Errors
///
/// Returns an error when no JDK with `jni.h` is found.
pub fn jdk_includes() -> Result<Vec<Utf8PathBuf>> {
    let mut homes = Vec::new();
    if let Some(home) = std::env::var("JAVA_HOME").ok().filter(|h| !h.is_empty()) {
        homes.push(Utf8PathBuf::from(home));
    }
    if cfg!(target_os = "macos") {
        if let Ok(out) = Command::new("/usr/libexec/java_home").output() {
            if out.status.success() {
                homes.push(Utf8PathBuf::from(
                    String::from_utf8_lossy(&out.stdout).trim(),
                ));
            }
        }
    }
    if let Ok(out) = Command::new("which").arg("javac").output() {
        let javac = Utf8PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if let Ok(real) = javac.canonicalize_utf8() {
            if let Some(home) = real.parent().and_then(Utf8Path::parent) {
                homes.push(home.to_path_buf());
            }
        }
    }
    let os_dir = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        "linux"
    };
    for home in homes {
        let include = home.join("include");
        if include.join("jni.h").is_file() {
            return Ok(vec![include.clone(), include.join(os_dir)]);
        }
    }
    bail!("no JDK with jni.h was found; install a JDK and set JAVA_HOME")
}

/// Compile the JNI shim `glue` for `platform` (a desktop JVM platform, or
/// an Android ABI through `ndk`), linked against the producer library in
/// `dir`, into `dir/lib{library}_jni.{so,dylib}`.
///
/// # Errors
///
/// Returns an error when this host can't compile for `platform`, the JDK or
/// NDK is missing, or the compiler fails.
pub fn compile_jni_shim(
    glue: &GlueSource,
    platform: Platform,
    dir: &Utf8Path,
    library: &str,
    ndk: Option<&Ndk>,
    settings: &BuildSettings,
) -> Result<Utf8PathBuf> {
    let shim = jni_shim_name(library);
    let file = platform.lib_filename(&shim);
    let output = dir.join(&file);
    let mut cmd = match platform.os() {
        Os::Android => {
            let Some(ndk) = ndk else {
                bail!(
                    "compiling the JNI shim for {} needs the Android NDK",
                    platform.id()
                );
            };
            let mut cmd = Command::new(ndk.clang(platform, settings.android_api)?.as_str());
            cmd.arg(format!("-Wl,-soname,{file}"))
                .arg("-Wl,-z,max-page-size=16384");
            cmd
        }
        Os::Windows => bail!(
            "prebuilding the JNI shim for windows-x64 isn't supported; build it with the \
             generated CMake project on Windows"
        ),
        Os::MacOs | Os::Linux => {
            check_desktop_host(platform)?;
            let mut cmd = Command::new(desktop_cc());
            for include in jdk_includes()? {
                cmd.arg(format!("-I{include}"));
            }
            if platform.os() == Os::MacOs {
                cmd.args(apple_flags(platform, settings))
                    .arg("-Wl,-rpath,@loader_path")
                    .arg(format!("-Wl,-install_name,@rpath/{file}"));
            } else {
                cmd.arg("-Wl,-rpath,$ORIGIN")
                    .arg(format!("-Wl,-soname,{file}"))
                    .arg("-lpthread");
            }
            cmd
        }
        Os::Ios | Os::Wasm => bail!("{} has no JVM", platform.id()),
    };
    cmd.args(["-shared", "-fPIC", "-O2"])
        .arg(format!("-I{}", glue.include_dir))
        .arg(glue.source.as_str())
        .arg(format!("-L{dir}"))
        .arg(format!("-l{library}"))
        .arg("-o")
        .arg(output.as_str());
    run(cmd, "the JNI shim", platform)?;
    Ok(output)
}

/// Run a compiler command, capturing its output for the error message.
fn run(mut cmd: Command, what: &str, platform: Platform) -> Result<()> {
    let program = cmd.get_program().to_string_lossy().into_owned();
    let out = cmd
        .output()
        .with_context(|| format!("failed to run `{program}` to compile {what}"))?;
    if !out.status.success() {
        bail!(
            "compiling {what} for {} failed:\n{}",
            platform.id(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        );
    }
    Ok(())
}
