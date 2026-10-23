//! `cargo xtask`: the repository's task runner.
//!
//! ```text
//! cargo xtask conformance [--only LANES] [--skip LANES] [--no-gen] [--lane-timeout SECS]
//! cargo xtask fixtures [TARGET ...]
//! cargo xtask skew
//! ```
//!
//! `conformance` builds the sample producers with leak counters, generates
//! their bindings from those builds (library mode), and runs each language's
//! lanes (`conformance/<lang>/run.sh`), then summarizes. `fixtures` generates
//! every snapshot fixture for each target and compile-checks it with the
//! target's own toolchain (`scripts/fixtures/<target>.sh`). `skew` checks
//! the contract between bindings and a library of a different version: older
//! bindings load a newer library, and a changed declaration is reported by
//! its path. The language scripts are Bash; everything around them is here,
//! so the driver behaves the same on every OS.
//!
//! `ONLY`, `SKIP`, `SKIP_GEN`, and `LANE_TIMEOUT` in the environment work as
//! the flags do, for `conformance/run.sh` and CI.

use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

mod skew;

/// The sample producers every language's lanes consume.
const SAMPLES: [&str; 3] = ["calculator", "codec", "kvstore"];

/// Every language with conformance lanes, in run order.
const LANGS: [&str; 11] = [
    "c", "cpp", "python", "ruby", "dart", "go", "swift", "dotnet", "node", "kotlin", "wasm",
];

/// Every target with a fixture check, in registry order.
const TARGETS: [&str; 11] = [
    "c", "cpp", "swift", "kotlin", "node", "wasm", "python", "dotnet", "dart", "go", "ruby",
];

type Result<T> = std::result::Result<T, String>;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("conformance") => conformance(&args[1..]),
        Some("fixtures") => fixtures(&args[1..]),
        Some("skew") => Workspace::locate().and_then(|ws| skew::run_all(&ws)),
        _ => Err(
            "usage: cargo xtask <conformance [--only LANES] [--skip LANES] [--no-gen] \
             [--lane-timeout SECS] | fixtures [TARGET ...] | skew>"
                .to_string(),
        ),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The workspace's root and Cargo target directory.
pub(crate) struct Workspace {
    pub(crate) root: PathBuf,
    pub(crate) target_dir: PathBuf,
}

impl Workspace {
    fn locate() -> Result<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("xtask has no parent directory")?
            .to_path_buf();
        let output = Command::new(cargo())
            .args(["metadata", "--format-version", "1", "--no-deps"])
            .current_dir(&root)
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("failed to run cargo metadata: {e}"))?;
        let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("cargo metadata printed invalid JSON: {e}"))?;
        let target_dir = metadata["target_directory"]
            .as_str()
            .map_or_else(|| root.join("target"), PathBuf::from);
        Ok(Self { root, target_dir })
    }

    /// The `weaveffi` binary `build_cli` built.
    pub(crate) fn weaveffi(&self) -> PathBuf {
        self.target_dir.join("debug").join(exe("weaveffi"))
    }

    /// Build the CLI.
    pub(crate) fn build_cli(&self) -> Result<()> {
        run(Command::new(cargo())
            .args(["build", "-q", "-p", "weaveffi-cli"])
            .current_dir(&self.root))
    }

    /// The path of sample `name`'s shared library in the debug profile.
    pub(crate) fn sample_lib(&self, name: &str) -> PathBuf {
        let base = name.replace('-', "_");
        let file = match env::consts::OS {
            "macos" => format!("lib{base}.dylib"),
            "windows" => format!("{base}.dll"),
            _ => format!("lib{base}.so"),
        };
        self.target_dir.join("debug").join(file)
    }
}

fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

fn exe(name: &str) -> String {
    format!("{name}{}", env::consts::EXE_SUFFIX)
}

/// Run `cmd`, failing unless it exits 0.
pub(crate) fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .status()
        .map_err(|e| format!("failed to run {cmd:?}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{cmd:?} failed with {status}"))
    }
}

/// A comma-separated list (empty entries dropped).
fn list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The selection flags of `conformance`, with the environment as defaults.
struct Selection {
    only: Vec<String>,
    skip: Vec<String>,
    no_gen: bool,
    lane_timeout: String,
}

impl Selection {
    fn parse(args: &[String]) -> Result<Self> {
        let var = |name: &str| env::var(name).unwrap_or_default();
        let mut sel = Self {
            only: list(&var("ONLY")),
            skip: list(&var("SKIP")),
            no_gen: !var("SKIP_GEN").is_empty(),
            lane_timeout: env::var("LANE_TIMEOUT").unwrap_or_else(|_| "300".to_string()),
        };
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            let mut value = || it.next().cloned().ok_or(format!("{arg} needs a value"));
            match arg.as_str() {
                "--only" => sel.only = list(&value()?),
                "--skip" => sel.skip = list(&value()?),
                "--no-gen" => sel.no_gen = true,
                "--lane-timeout" => sel.lane_timeout = value()?,
                other => return Err(format!("unknown flag {other}")),
            }
        }
        Ok(sel)
    }

    /// The languages to run: those `--only` names (by language or lane),
    /// else every one, minus those `--skip` names as a whole language.
    fn langs(&self) -> Vec<&'static str> {
        LANGS
            .into_iter()
            .filter(|lang| {
                self.only.is_empty()
                    || self
                        .only
                        .iter()
                        .any(|s| s == lang || s.starts_with(&format!("{lang}-")))
            })
            .filter(|lang| !self.skip.iter().any(|s| s == lang))
            .collect()
    }
}

/// `cargo xtask conformance`.
fn conformance(args: &[String]) -> Result<bool> {
    let sel = Selection::parse(args)?;
    let ws = Workspace::locate()?;
    let langs = sel.langs();
    let lib_dir = ws.target_dir.join("debug");
    let gen_root = ws.target_dir.join("conformance-gen");
    let out = ws.target_dir.join("conformance-build");
    let results = out.join("results.txt");
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    let _ = std::fs::remove_file(&results);

    println!("--- building producers");
    // The samples depend on `weaveffi` with `leak-check`; the feature is
    // named here too so the build the consumers load is unmistakably a
    // counting one.
    let mut build = Command::new(cargo());
    build
        .args(["build", "-q", "--features", "weaveffi/leak-check"])
        .current_dir(&ws.root);
    for s in SAMPLES {
        build.args(["-p", s]);
    }
    run(&mut build)?;
    ws.build_cli()?;

    if !sel.no_gen {
        // Generate the selected languages plus the C header every other
        // target builds on (every target when nothing is selected).
        let targets: BTreeSet<&str> = std::iter::once("c").chain(langs.iter().copied()).collect();
        let targets = targets.into_iter().collect::<Vec<_>>().join(",");
        for s in SAMPLES {
            println!("--- generating bindings: {s}");
            let dir = gen_root.join(s);
            let _ = std::fs::remove_dir_all(&dir);
            run(Command::new(ws.weaveffi())
                .arg("generate")
                .arg(ws.root.join("samples").join(s))
                .arg("--library")
                .arg(ws.sample_lib(s))
                .arg("-o")
                .arg(&dir)
                .args(["--target", &targets])
                .current_dir(&ws.root))?;
        }
    }

    let mut path = env::split_paths(&env::var_os("PATH").unwrap_or_default()).collect::<Vec<_>>();
    let ext = match env::consts::OS {
        "macos" => "dylib",
        "windows" => {
            path.insert(0, lib_dir.clone());
            "dll"
        }
        _ => "so",
    };
    let loader_var = match env::consts::OS {
        "macos" => Some("DYLD_LIBRARY_PATH"),
        "windows" => None,
        _ => Some("LD_LIBRARY_PATH"),
    };
    for lang in &langs {
        let mut cmd = Command::new("bash");
        cmd.arg(ws.root.join("conformance").join(lang).join("run.sh"))
            .current_dir(&ws.root)
            .env("ROOT", &ws.root)
            .env("TARGET_DIR", &ws.target_dir)
            .env("LIBDIR", &lib_dir)
            .env("GENROOT", &gen_root)
            .env("OUT", &out)
            .env("RESULTS", &results)
            .env("LANE_TIMEOUT", &sel.lane_timeout)
            .env("EXT", ext)
            .env("ONLY", sel.only.join(","))
            .env("SKIP", sel.skip.join(","))
            .env(
                "PATH",
                env::join_paths(&path).map_err(|e| format!("PATH: {e}"))?,
            );
        if let Some(var) = loader_var {
            let mut dirs = vec![lib_dir.clone()];
            dirs.extend(env::split_paths(&env::var_os(var).unwrap_or_default()));
            cmd.env(
                var,
                env::join_paths(dirs).map_err(|e| format!("{var}: {e}"))?,
            );
        }
        // A language's failed lanes are recorded in the results file; the
        // summary below decides the exit code.
        let _ = cmd.status();
    }

    let text = std::fs::read_to_string(&results).unwrap_or_default();
    let with = |prefix: &str| -> Vec<&str> {
        text.lines()
            .filter_map(|l| l.strip_prefix(prefix))
            .collect()
    };
    let (passed, failed, skipped) = (with("OK "), with("FAIL "), with("SKIP "));
    println!();
    println!(
        "conformance: {} passed, {} failed",
        passed.len(),
        failed.len()
    );
    if !skipped.is_empty() {
        println!("skipped (missing tools): {}", skipped.join(" "));
    }
    if !failed.is_empty() {
        eprintln!("failed: {}", failed.join(" "));
        return Ok(false);
    }
    if passed.is_empty() && skipped.is_empty() {
        eprintln!("no lanes ran");
        return Ok(false);
    }
    Ok(true)
}

/// `cargo xtask fixtures`.
fn fixtures(args: &[String]) -> Result<bool> {
    let ws = Workspace::locate()?;
    let targets: Vec<String> = if args.is_empty() {
        TARGETS.iter().map(|t| (*t).to_string()).collect()
    } else {
        args.to_vec()
    };
    ws.build_cli()?;
    let scratch = ws.target_dir.join("fixture-check");
    let _ = std::fs::remove_dir_all(&scratch);
    let fixtures_dir = ws.root.join("crates/weaveffi-cli/tests/fixtures");
    let mut fixtures: Vec<PathBuf> = std::fs::read_dir(&fixtures_dir)
        .map_err(|e| format!("{}: {e}", fixtures_dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "yml"))
        .collect();
    fixtures.sort();
    let (mut failed, mut skipped) = (Vec::new(), Vec::new());
    for fixture in &fixtures {
        let name = fixture
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        for t in &targets {
            let out = scratch.join(t).join(&name);
            let generated = Command::new(ws.weaveffi())
                .args(["-q", "generate"])
                .arg(fixture)
                .arg("-o")
                .arg(&out)
                .args(["--target", &format!("c,{t}")])
                .status()
                .is_ok_and(|s| s.success());
            if !generated {
                failed.push(format!("{t}:{name}(generate)"));
                continue;
            }
            println!("==> {t}: {name}");
            let status = Command::new("bash")
                .arg(ws.root.join("scripts/fixtures").join(format!("{t}.sh")))
                .arg(&out)
                .current_dir(&ws.root)
                .status()
                .map_err(|e| format!("failed to run bash: {e}"))?;
            match status.code() {
                Some(0) => {}
                Some(77) => skipped.push(format!("{t}:{name}")),
                _ => failed.push(format!("{t}:{name}")),
            }
        }
    }
    if !skipped.is_empty() {
        eprintln!(
            "fixture check skipped (missing tools): {}",
            skipped.join(" ")
        );
    }
    if !failed.is_empty() {
        eprintln!("fixture check failed: {}", failed.join(" "));
        return Ok(false);
    }
    println!(
        "fixture check passed{}",
        if skipped.is_empty() {
            ""
        } else {
            " (with skips)"
        }
    );
    Ok(true)
}
