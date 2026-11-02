//! `cargo xtask skew`: bindings against a library of a different version.
//!
//! The contract (see the C ABI reference) lets a library grow: bindings
//! generated from an older API load a newer library, a declaration the
//! library changed is reported by its path, and one the library lacks
//! fails to link (or, for a binding that loads the library at run time, to
//! resolve). Each scenario here edits the calculator sample's extracted
//! API, generates the C target from the edited copy, and builds
//! `xtask/skew/check.c` against the real calculator library.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

use crate::{run, Result, Workspace};

/// One version-skew scenario: how the bindings' API differs from the
/// library's, and what loading must do.
struct Scenario {
    name: &'static str,
    edit: fn(&mut Value),
    expect: Expect,
}

enum Expect {
    /// The contract check passes and calls work.
    Loads,
    /// The contract check reports this declaration path.
    Mismatch(&'static str),
    /// Linking fails, naming the contract function of this declaration
    /// path, which the library doesn't export.
    NoLink(&'static str),
}

fn functions(api: &mut Value) -> &mut Vec<Value> {
    api["functions"].as_array_mut().expect("functions")
}

fn function<'a>(api: &'a mut Value, name: &str) -> &'a mut Value {
    functions(api)
        .iter_mut()
        .find(|f| f["name"] == name)
        .expect("function")
}

const SCENARIOS: [Scenario; 5] = [
    Scenario {
        name: "the same API",
        edit: |_| {},
        expect: Expect::Loads,
    },
    Scenario {
        // An older binding: the library has since added `mean`,
        // `running_total`, `parse`, and the `ParseError` domain.
        name: "bindings older than the library",
        edit: |api| {
            functions(api).retain(|f| {
                !matches!(f["name"].as_str(), Some("mean" | "running_total" | "parse"))
            });
            api["errors"]
                .as_array_mut()
                .expect("errors")
                .retain(|e| e["name"] != "ParseError");
        },
        expect: Expect::Loads,
    },
    Scenario {
        name: "a parameter type changed",
        edit: |api| function(api, "add")["params"][1]["type"] = "i64".into(),
        expect: Expect::Mismatch("add"),
    },
    Scenario {
        name: "an error code's value changed",
        edit: |api| {
            let domain = api["errors"]
                .as_array_mut()
                .expect("errors")
                .iter_mut()
                .find(|e| e["name"] == "CalcError")
                .expect("CalcError");
            domain["codes"][0]["code"] = 7.into();
        },
        expect: Expect::Mismatch("CalcError.DivisionByZero"),
    },
    Scenario {
        // A newer binding: it declares `subtract`, which the library lacks.
        name: "bindings newer than the library",
        edit: |api| {
            let mut sub = function(api, "add").clone();
            sub["name"] = "subtract".into();
            functions(api).push(sub);
        },
        expect: Expect::NoLink("subtract"),
    },
];

/// Run every scenario.
pub(crate) fn run_all(ws: &Workspace) -> Result<bool> {
    if cfg!(windows) {
        println!("skew: skipped on Windows (the check links with cc)");
        return Ok(true);
    }
    ws.build_cli()?;
    run(Command::new(crate::cargo())
        .args(["build", "-q", "-p", "calculator"])
        .current_dir(&ws.root))?;
    let library = ws.sample_lib("calculator");
    let lib_dir = library.parent().expect("library directory").to_path_buf();
    let output = Command::new(ws.weaveffi())
        .args(["-q", "extract", "-f", "json", "--library"])
        .arg(&library)
        .arg(ws.root.join("samples/calculator"))
        .output()
        .map_err(|e| format!("failed to run weaveffi extract: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "weaveffi extract failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let api: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("extract printed invalid JSON: {e}"))?;

    let scratch = ws.target_dir.join("skew");
    let _ = std::fs::remove_dir_all(&scratch);
    let mut ok = true;
    for (i, scenario) in SCENARIOS.iter().enumerate() {
        let dir = scratch.join(i.to_string());
        let mut edited = api.clone();
        (scenario.edit)(&mut edited);
        let outcome = check(ws, &dir, &edited, &lib_dir)?;
        let pass = match (&scenario.expect, &outcome) {
            (Expect::Loads, Outcome::Loaded) => true,
            (Expect::Mismatch(path), Outcome::Mismatch(got)) => got == path,
            (Expect::NoLink(path), Outcome::NoLink(log)) => {
                log.contains(&format!("calculator_contract_{:016x}", fnv1a64(path)))
            }
            _ => false,
        };
        println!(
            "[{}] skew: {} ({outcome})",
            if pass { "OK" } else { "FAIL" },
            scenario.name
        );
        ok &= pass;
    }
    Ok(ok)
}

/// A declaration path's contract id (see the C ABI reference).
fn fnv1a64(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// What building and running the check against the library did.
enum Outcome {
    Loaded,
    Mismatch(String),
    NoLink(String),
    Failed(String),
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Loaded => f.write_str("loaded"),
            Outcome::Mismatch(path) => write!(f, "contract mismatch at {path}"),
            Outcome::NoLink(_) => f.write_str("didn't link"),
            Outcome::Failed(why) => write!(f, "failed: {why}"),
        }
    }
}

/// Generate the C target from `api` into `dir`, then build and run the
/// check program against the library in `lib_dir`.
fn check(ws: &Workspace, dir: &Path, api: &Value, lib_dir: &Path) -> Result<Outcome> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let idl = dir.join("calculator.json");
    std::fs::write(&idl, api.to_string()).map_err(|e| format!("{}: {e}", idl.display()))?;
    std::fs::write(
        dir.join("weaveffi.toml"),
        "[package]\nname = \"calculator\"\nversion = \"1.0.0\"\n",
    )
    .map_err(|e| format!("weaveffi.toml: {e}"))?;
    run(Command::new(ws.weaveffi())
        .args(["-q", "generate"])
        .arg(&idl)
        .args(["--target", "c", "-o"])
        .arg(dir.join("out")))?;

    let exe = dir.join("check");
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let build = Command::new(cc)
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
        .arg(dir.join("out/c"))
        .arg(ws.root.join("xtask/skew/check.c"))
        .arg("-L")
        .arg(lib_dir)
        .args(["-lcalculator", "-o"])
        .arg(&exe)
        .output()
        .map_err(|e| format!("failed to run cc: {e}"))?;
    if !build.status.success() {
        return Ok(Outcome::NoLink(
            String::from_utf8_lossy(&build.stderr).into_owned(),
        ));
    }
    let loader_var = if cfg!(target_os = "macos") {
        "DYLD_LIBRARY_PATH"
    } else {
        "LD_LIBRARY_PATH"
    };
    let ran = Command::new(&exe)
        .env(loader_var, lib_dir)
        .output()
        .map_err(|e| format!("failed to run the check: {e}"))?;
    let stdout = String::from_utf8_lossy(&ran.stdout).trim().to_string();
    Ok(match ran.status.code() {
        Some(0) => Outcome::Loaded,
        Some(3) => Outcome::Mismatch(stdout),
        _ => Outcome::Failed(format!(
            "{}{}",
            stdout,
            String::from_utf8_lossy(&ran.stderr)
        )),
    })
}
