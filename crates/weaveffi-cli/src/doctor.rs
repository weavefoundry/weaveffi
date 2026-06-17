//! `weaveffi doctor`: probe the host for the toolchains each language target
//! needs (compilers, SDKs, Rust cross-targets, …) and report what is present.
//!
//! Each probe yields a [`DoctorCheck`]; `--format json` serializes the raw
//! list, otherwise the checks are grouped into human-readable sections. With a
//! `--target` filter the command exits non-zero if any *relevant* check fails,
//! so it can gate a language's build in CI.

use miette::{IntoDiagnostic, Result, WrapErr};
use serde::Serialize;
use std::collections::BTreeSet;
use std::env;
use std::process::Command;

/// A single toolchain check produced by `cmd_doctor`. Serialized as JSON when
/// the user passes `--format json` and grouped into sections for the
/// human-readable output otherwise. `applies_to` is the set of target names
/// (`swift`, `android`, `cpp`, …) for which this check is relevant; the
/// special value `"*"` means the check is always run regardless of any
/// `--target` filter (e.g. the required Rust toolchain).
#[derive(Debug, Serialize)]
struct DoctorCheck {
    id: String,
    name: String,
    ok: bool,
    version: Option<String>,
    hint: Option<String>,
    applies_to: Vec<&'static str>,
}

impl DoctorCheck {
    fn applies_to_target(&self, target: Option<&str>) -> bool {
        match target {
            None => true,
            Some(t) => self.applies_to.contains(&"*") || self.applies_to.contains(&t),
        }
    }
}

pub(crate) fn cmd_doctor(target: Option<&str>, format: Option<&str>) -> Result<()> {
    let checks: Vec<DoctorCheck> = collect_doctor_checks()
        .into_iter()
        .filter(|c| c.applies_to_target(target))
        .collect();

    if format == Some("json") {
        let json = serde_json::to_string_pretty(&checks)
            .into_diagnostic()
            .wrap_err("failed to serialize doctor checks")?;
        println!("{json}");
    } else {
        print_doctor_human(&checks);
    }

    if target.is_some() && checks.iter().any(|c| !c.ok) {
        std::process::exit(1);
    }

    Ok(())
}

fn collect_doctor_checks() -> Vec<DoctorCheck> {
    let mut checks = Vec::new();

    checks.push(tool_check(
        "rustc",
        "rustc",
        &["--version"],
        "Rust compiler",
        Some("Install via https://rustup.rs"),
        vec!["*"],
    ));
    checks.push(tool_check(
        "cargo",
        "cargo",
        &["--version"],
        "Cargo (Rust package manager)",
        Some("Install via https://rustup.rs"),
        vec!["*"],
    ));

    if cfg!(target_os = "macos") {
        checks.push(tool_check(
            "xcodebuild",
            "xcodebuild",
            &["-version"],
            "Xcode command-line tools",
            Some("Install Xcode from the App Store, then run `xcode-select --install`"),
            vec!["swift"],
        ));
    }

    checks.push(ndk_check());

    checks.push(tool_check(
        "node",
        "node",
        &["-v"],
        "Node.js",
        Some("Install from https://nodejs.org or with your package manager"),
        vec!["node"],
    ));
    checks.push(tool_check(
        "npm",
        "npm",
        &["-v"],
        "npm",
        Some("Install Node.js which includes npm, or use pnpm/yarn"),
        vec!["node"],
    ));

    checks.extend(cross_target_checks());

    checks.push(tool_check(
        "wasm-pack",
        "wasm-pack",
        &["--version"],
        "wasm-pack",
        Some("install with `cargo install wasm-pack`"),
        vec!["wasm"],
    ));
    checks.push(tool_check(
        "wasm-bindgen",
        "wasm-bindgen",
        &["--version"],
        "wasm-bindgen-cli",
        Some("install with `cargo install wasm-bindgen-cli`"),
        vec!["wasm"],
    ));

    checks.push(tool_check(
        "cmake",
        "cmake",
        &["--version"],
        "CMake",
        Some("Install via https://cmake.org or your package manager"),
        vec!["cpp"],
    ));
    checks.push(cxx_compiler_check());

    checks.push(tool_check(
        "dart",
        "dart",
        &["--version"],
        "Dart SDK",
        Some("Install from https://dart.dev/get-dart"),
        vec!["dart"],
    ));
    checks.push(tool_check(
        "flutter",
        "flutter",
        &["--version"],
        "Flutter (optional)",
        Some("Install from https://flutter.dev/docs/get-started/install"),
        vec!["dart"],
    ));

    checks.push(tool_check(
        "go",
        "go",
        &["version"],
        "Go",
        Some("Install from https://go.dev/dl"),
        vec!["go"],
    ));

    checks.push(tool_check(
        "ruby",
        "ruby",
        &["--version"],
        "Ruby",
        Some("Install from https://www.ruby-lang.org or use rbenv/rvm"),
        vec!["ruby"],
    ));
    checks.push(tool_check(
        "gem",
        "gem",
        &["--version"],
        "RubyGems",
        Some("Install Ruby which includes gem"),
        vec!["ruby"],
    ));
    checks.push(tool_check(
        "bundler",
        "bundler",
        &["--version"],
        "Bundler",
        Some("Install with `gem install bundler`"),
        vec!["ruby"],
    ));

    checks.push(tool_check(
        "dotnet",
        "dotnet",
        &["--version"],
        ".NET SDK",
        Some("Install from https://dotnet.microsoft.com/download"),
        vec!["dotnet"],
    ));

    checks.push(tool_check(
        "python3",
        "python3",
        &["--version"],
        "Python 3",
        Some("Install Python 3 from https://www.python.org or your package manager"),
        vec!["python"],
    ));
    checks.push(python_ctypes_check());

    checks
}

fn tool_check(
    id: &str,
    cmd: &str,
    args: &[&str],
    name: &str,
    hint: Option<&str>,
    applies_to: Vec<&'static str>,
) -> DoctorCheck {
    let (ok, version) = match Command::new(cmd).args(args).output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let ver = stdout
                .lines()
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    stderr
                        .lines()
                        .next()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                })
                .map(|s| s.to_string());
            (true, ver)
        }
        _ => (false, None),
    };
    DoctorCheck {
        id: id.to_string(),
        name: name.to_string(),
        ok,
        version,
        hint: hint.map(str::to_string),
        applies_to,
    }
}

fn ndk_check() -> DoctorCheck {
    let result = Command::new("ndk-build").arg("-v").output();
    let ok = matches!(&result, Ok(o) if o.status.success());
    let version = if ok {
        result.ok().and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
        })
    } else {
        None
    };
    let hint = if ok {
        None
    } else {
        let env_ok = env::var_os("ANDROID_NDK_HOME")
            .map(|p| std::path::Path::new(&p).exists())
            .unwrap_or(false)
            || env::var_os("ANDROID_NDK_ROOT")
                .map(|p| std::path::Path::new(&p).exists())
                .unwrap_or(false);
        if env_ok {
            Some("ANDROID_NDK_HOME/ROOT is set; ensure `ndk-build` is in PATH".to_string())
        } else if cfg!(target_os = "macos") {
            Some(
                "Install via Android Studio SDK Manager or `brew install android-ndk`. Set ANDROID_NDK_HOME."
                    .to_string(),
            )
        } else {
            Some("Install via Android Studio SDK Manager. Set ANDROID_NDK_HOME.".to_string())
        }
    };
    DoctorCheck {
        id: "ndk-build".to_string(),
        name: "Android NDK (ndk-build)".to_string(),
        ok,
        version,
        hint,
        applies_to: vec!["android"],
    }
}

fn cross_target_checks() -> Vec<DoctorCheck> {
    let installed = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .and_then(|out| {
            if out.status.success() {
                Some(String::from_utf8_lossy(&out.stdout).to_string())
            } else {
                None
            }
        });

    let targets: [(&str, &str, &str, Vec<&'static str>); 3] = [
        (
            "target_ios",
            "aarch64-apple-ios",
            "iOS target (aarch64-apple-ios)",
            vec!["swift"],
        ),
        (
            "target_android",
            "aarch64-linux-android",
            "Android target (aarch64-linux-android)",
            vec!["android"],
        ),
        (
            "target_wasm",
            "wasm32-unknown-unknown",
            "WebAssembly target (wasm32-unknown-unknown)",
            vec!["wasm"],
        ),
    ];

    targets
        .into_iter()
        .map(|(id, target, name, applies_to)| {
            let ok = installed
                .as_ref()
                .map(|s| s.lines().any(|line| line.trim() == target))
                .unwrap_or(false);
            let hint = if ok {
                None
            } else {
                Some(format!("install with `rustup target add {target}`"))
            };
            DoctorCheck {
                id: id.to_string(),
                name: name.to_string(),
                ok,
                version: None,
                hint,
                applies_to,
            }
        })
        .collect()
}

fn cxx_compiler_check() -> DoctorCheck {
    for cmd in ["g++", "clang++"] {
        if let Ok(out) = Command::new(cmd).arg("--version").output() {
            if out.status.success() {
                let ver = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .next()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(|s| s.to_string());
                return DoctorCheck {
                    id: "cxx_compiler".to_string(),
                    name: format!("C++ compiler ({cmd})"),
                    ok: true,
                    version: ver,
                    hint: None,
                    applies_to: vec!["cpp"],
                };
            }
        }
    }
    DoctorCheck {
        id: "cxx_compiler".to_string(),
        name: "C++ compiler (g++ or clang++)".to_string(),
        ok: false,
        version: None,
        hint: Some("Install g++ or clang++ via your package manager".to_string()),
        applies_to: vec!["cpp"],
    }
}

fn python_ctypes_check() -> DoctorCheck {
    let ok = Command::new("python3")
        .args(["-c", "import ctypes"])
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    DoctorCheck {
        id: "python_ctypes".to_string(),
        name: "Python ctypes module".to_string(),
        ok,
        version: None,
        hint: if ok {
            None
        } else {
            Some(
                "ctypes ships with the Python 3 standard library; reinstall Python 3 (with libffi) and re-run `python3 -c 'import ctypes'`"
                    .to_string(),
            )
        },
        applies_to: vec!["python"],
    }
}

fn print_doctor_human(checks: &[DoctorCheck]) {
    println!("WeaveFFI Doctor: checking toolchain prerequisites\n");

    let sections: &[(&str, &[&str])] = &[
        ("Required toolchain", &["rustc", "cargo"]),
        ("Swift / iOS", &["xcodebuild"]),
        ("Android", &["ndk-build"]),
        ("Node.js", &["node", "npm"]),
        (
            "Cross-compilation targets",
            &["target_ios", "target_android", "target_wasm"],
        ),
        ("WebAssembly tools", &["wasm-pack", "wasm-bindgen"]),
        ("C++", &["cmake", "cxx_compiler"]),
        ("Dart", &["dart", "flutter"]),
        ("Go", &["go"]),
        ("Ruby", &["ruby", "gem", "bundler"]),
        (".NET", &["dotnet"]),
        ("Python", &["python3", "python_ctypes"]),
    ];

    let mut printed: BTreeSet<&str> = BTreeSet::new();
    for (heading, ids) in sections {
        let in_section: Vec<&DoctorCheck> = checks
            .iter()
            .filter(|c| ids.contains(&c.id.as_str()))
            .collect();
        if in_section.is_empty() {
            continue;
        }
        println!("{heading}:");
        for c in in_section {
            print_check_line(c);
            printed.insert(c.id.as_str());
        }
        println!();
    }

    let leftovers: Vec<&DoctorCheck> = checks
        .iter()
        .filter(|c| !printed.contains(c.id.as_str()))
        .collect();
    if !leftovers.is_empty() {
        println!("Other:");
        for c in leftovers {
            print_check_line(c);
        }
        println!();
    }

    println!("Doctor completed. Address any missing items above.");
}

fn print_check_line(c: &DoctorCheck) {
    let status = if c.ok { "OK" } else { "MISSING" };
    match &c.version {
        Some(ver) => println!("- {}: {} ({})", c.name, status, ver),
        None => println!("- {}: {}", c.name, status),
    }
    if !c.ok {
        if let Some(h) = &c.hint {
            println!("  hint: {h}");
        }
    }
}
