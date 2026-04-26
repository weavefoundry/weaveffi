use anyhow::{bail, Context, Result};
use camino::Utf8Path;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};
use weaveffi_ir::ir::{Api, CURRENT_SCHEMA_VERSION};

use crate::cache;
use crate::config::GeneratorConfig;
use crate::templates::TemplateEngine;

/// Filename of the lockfile written to the output dir root.
pub const LOCKFILE: &str = "weaveffi.lock";

/// Returns the header message used to stamp every generated source file.
///
/// The returned string is the bare message; each generator wraps it in the
/// appropriate comment syntax for its target language (`//`, `#`, or `/* */`).
/// It embeds the IR schema version, the generator name, and the WeaveFFI tool
/// version (from `CARGO_PKG_VERSION`) so stamped files are traceable back to
/// the exact toolchain that produced them.
pub fn stamp_header(generator_name: &str) -> String {
    format!(
        "WeaveFFI {ir} {gen} {tool} - DO NOT EDIT - regenerate with 'weaveffi generate'",
        ir = CURRENT_SCHEMA_VERSION,
        gen = generator_name,
        tool = env!("CARGO_PKG_VERSION"),
    )
}

fn run_hook(label: &str, cmd: &str) -> Result<()> {
    let status = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/C", cmd])
            .status()?
    } else {
        std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .status()?
    };
    if !status.success() {
        bail!("{label} hook failed with {status}");
    }
    Ok(())
}

/// IR features a generator may or may not support.
///
/// Generators declare their supported capabilities via [`Generator::capabilities`].
/// The default implementation returns [`Capability::ALL`], meaning generators are
/// assumed feature-complete unless they override it to advertise a narrower set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    Callbacks,
    Listeners,
    Iterators,
    Builders,
    AsyncFunctions,
    CancellableAsync,
    TypedHandles,
    BorrowedTypes,
    MapTypes,
    NestedModules,
    CrossModuleTypes,
    ErrorDomains,
    DeprecatedAnnotations,
}

impl Capability {
    /// Every capability defined by the IR.
    pub const ALL: &'static [Capability] = &[
        Capability::Callbacks,
        Capability::Listeners,
        Capability::Iterators,
        Capability::Builders,
        Capability::AsyncFunctions,
        Capability::CancellableAsync,
        Capability::TypedHandles,
        Capability::BorrowedTypes,
        Capability::MapTypes,
        Capability::NestedModules,
        Capability::CrossModuleTypes,
        Capability::ErrorDomains,
        Capability::DeprecatedAnnotations,
    ];
}

pub trait Generator {
    fn name(&self) -> &'static str;
    fn generate(&self, api: &Api, out_dir: &Utf8Path) -> Result<()>;

    fn generate_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        _config: &GeneratorConfig,
    ) -> Result<()> {
        self.generate(api, out_dir)
    }

    fn generate_with_templates(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
        _templates: Option<&TemplateEngine>,
    ) -> Result<()> {
        self.generate_with_config(api, out_dir, config)
    }

    fn output_files(&self, _api: &Api, _out_dir: &Utf8Path) -> Vec<String> {
        vec![]
    }

    fn output_files_with_config(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        _config: &GeneratorConfig,
    ) -> Vec<String> {
        self.output_files(api, out_dir)
    }

    /// IR features this generator fully supports. Defaults to [`Capability::ALL`].
    fn capabilities(&self) -> &'static [Capability] {
        Capability::ALL
    }
}

pub struct Orchestrator<'a> {
    generators: Vec<&'a dyn Generator>,
    quiet: bool,
    lockfile: bool,
}

impl Default for Orchestrator<'_> {
    fn default() -> Self {
        Self {
            generators: Vec::new(),
            quiet: false,
            lockfile: true,
        }
    }
}

impl<'a> Orchestrator<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_generator(mut self, gen: &'a dyn Generator) -> Self {
        self.generators.push(gen);
        self
    }

    /// Suppress informational stdout output (e.g. the cache-skip notice).
    pub fn quiet(mut self, quiet: bool) -> Self {
        self.quiet = quiet;
        self
    }

    /// Enable or disable writing `weaveffi.lock` after generation. Defaults to on.
    pub fn lockfile(mut self, enabled: bool) -> Self {
        self.lockfile = enabled;
        self
    }

    pub fn run(
        &self,
        api: &Api,
        out_dir: &Utf8Path,
        config: &GeneratorConfig,
        force: bool,
        templates: Option<&TemplateEngine>,
    ) -> Result<()> {
        let hash = cache::hash_api(api);

        if !force {
            if let Some(cached) = cache::read_cache(out_dir) {
                if cached == hash {
                    if !self.quiet {
                        println!("No changes detected, skipping code generation.");
                    }
                    return Ok(());
                }
            }
        }

        if let Some(cmd) = &config.pre_generate {
            run_hook("pre_generate", cmd)?;
        }

        for g in &self.generators {
            g.generate_with_templates(api, out_dir, config, templates)?;
        }

        if let Some(cmd) = &config.post_generate {
            run_hook("post_generate", cmd)?;
        }

        cache::write_cache(out_dir, &hash)?;

        if self.lockfile {
            write_lockfile(out_dir, &hash).context("failed to write weaveffi.lock")?;
        }

        Ok(())
    }
}

#[derive(Serialize)]
struct LockfileDoc {
    meta: LockMeta,
    hash: LockHash,
    files: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct LockMeta {
    ir_version: String,
    tool_version: String,
    generated_at: String,
}

#[derive(Serialize)]
struct LockHash {
    api: String,
}

/// Walk `out_dir`, SHA-256 every emitted file (excluding metadata), and write
/// a deterministic `weaveffi.lock` TOML file to the directory root.
fn write_lockfile(out_dir: &Utf8Path, api_hash: &str) -> Result<()> {
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    collect_file_hashes(out_dir, out_dir, &mut files)?;

    let doc = LockfileDoc {
        meta: LockMeta {
            ir_version: CURRENT_SCHEMA_VERSION.to_string(),
            tool_version: env!("CARGO_PKG_VERSION").to_string(),
            generated_at: format_rfc3339_utc(SystemTime::now()),
        },
        hash: LockHash {
            api: api_hash.to_string(),
        },
        files,
    };

    let serialized = toml::to_string(&doc).context("failed to serialize weaveffi.lock")?;
    let path = out_dir.join(LOCKFILE);
    std::fs::write(path.as_std_path(), serialized)
        .with_context(|| format!("failed to write {path}"))?;
    Ok(())
}

fn collect_file_hashes(
    base: &Utf8Path,
    dir: &Utf8Path,
    out: &mut BTreeMap<String, String>,
) -> Result<()> {
    let entries = std::fs::read_dir(dir.as_std_path())
        .with_context(|| format!("failed to read directory: {dir}"))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let utf8 = Utf8Path::from_path(&path)
            .ok_or_else(|| anyhow::anyhow!("non-UTF-8 path: {path:?}"))?
            .to_owned();
        if utf8.is_dir() {
            collect_file_hashes(base, &utf8, out)?;
        } else {
            let rel = utf8
                .strip_prefix(base)
                .context("failed to strip output-dir prefix")?
                .to_string();
            if is_metadata_file(&rel) {
                continue;
            }
            let bytes = std::fs::read(utf8.as_std_path())
                .with_context(|| format!("failed to read {utf8}"))?;
            out.insert(rel, format!("{:x}", Sha256::digest(&bytes)));
        }
    }
    Ok(())
}

/// Files that are orchestrator metadata, not generator output, and therefore
/// must not be recorded in the lockfile.
fn is_metadata_file(rel: &str) -> bool {
    rel == LOCKFILE || rel == ".weaveffi-cache" || rel.starts_with(".weaveffi-cache.tmp.")
}

/// Format a `SystemTime` as an RFC-3339 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ`.
fn format_rfc3339_utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    let hours = rem / 3600;
    let minutes = (rem % 3600) / 60;
    let seconds = rem % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Howard Hinnant's civil-from-days algorithm: convert days since
/// 1970-01-01 to `(year, month, day)` with month in `[1, 12]` and day in
/// `[1, 31]`. Public-domain reference implementation.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use weaveffi_ir::ir::{Function, Module, Param, TypeRef};

    struct CountingGenerator {
        calls: Arc<AtomicUsize>,
    }

    impl Generator for CountingGenerator {
        fn name(&self) -> &'static str {
            "counting"
        }

        fn generate(&self, _api: &Api, out_dir: &Utf8Path) -> Result<()> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            std::fs::write(out_dir.join("output.txt").as_std_path(), "generated")?;
            Ok(())
        }
    }

    fn test_api() -> Api {
        Api {
            version: "0.1.0".to_string(),
            modules: vec![Module {
                name: "math".to_string(),
                functions: vec![Function {
                    name: "add".to_string(),
                    params: vec![
                        Param {
                            name: "a".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                            mutable: false,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                    cancellable: false,
                    deprecated: None,
                    since: None,
                }],
                structs: vec![],
                enums: vec![],
                callbacks: vec![],
                listeners: vec![],
                errors: None,
                modules: vec![],
            }],
            generators: None,
        }
    }

    #[test]
    fn incremental_skips_when_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let content_after_first = std::fs::read_to_string(out_dir.join("output.txt")).unwrap();

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "generator should not run again"
        );
        let content_after_second = std::fs::read_to_string(out_dir.join("output.txt")).unwrap();

        assert_eq!(content_after_first, content_after_second);
    }

    #[test]
    fn force_bypasses_cache() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);

        orch.run(&api, out_dir, &config, false, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "force should bypass cache");
    }

    #[test]
    fn generate_with_custom_templates_dir() {
        use crate::templates::TemplateEngine;

        let tpl_dir = tempfile::tempdir().unwrap();
        let tpl_path = Utf8Path::from_path(tpl_dir.path()).unwrap();
        std::fs::write(tpl_path.join("greeting.tera"), "Hello from {{ name }}!").unwrap();

        let mut engine = TemplateEngine::new();
        engine.load_dir(tpl_path).unwrap();

        let mut ctx = tera::Context::new();
        ctx.insert("name", "user-templates");
        let rendered = engine.render("greeting.tera", &ctx).unwrap();
        assert_eq!(rendered, "Hello from user-templates!");

        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, Some(&engine))
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pre_hook_runs_before_generate() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            pre_generate: Some("echo ok".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn pre_hook_failure_aborts() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            pre_generate: Some("exit 1".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        let result = orch.run(&api, out_dir, &config, true, None);
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "generator should not run");
    }

    #[test]
    fn post_hook_runs_after_generate() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            post_generate: Some("echo ok".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, None).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn capability_all_contains_every_variant() {
        use Capability::*;
        let expected = [
            Callbacks,
            Listeners,
            Iterators,
            Builders,
            AsyncFunctions,
            CancellableAsync,
            TypedHandles,
            BorrowedTypes,
            MapTypes,
            NestedModules,
            CrossModuleTypes,
            ErrorDomains,
            DeprecatedAnnotations,
        ];
        assert_eq!(Capability::ALL, &expected);
    }

    #[test]
    fn default_generator_capabilities_is_feature_complete() {
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };
        assert_eq!(gen.capabilities(), Capability::ALL);
    }

    #[test]
    fn generator_can_override_capabilities() {
        struct LimitedGenerator;
        impl Generator for LimitedGenerator {
            fn name(&self) -> &'static str {
                "limited"
            }
            fn generate(&self, _api: &Api, _out_dir: &Utf8Path) -> Result<()> {
                Ok(())
            }
            fn capabilities(&self) -> &'static [Capability] {
                &[Capability::AsyncFunctions, Capability::ErrorDomains]
            }
        }

        let gen = LimitedGenerator;
        assert_eq!(
            gen.capabilities(),
            &[Capability::AsyncFunctions, Capability::ErrorDomains]
        );
        assert!(!gen.capabilities().contains(&Capability::Callbacks));
    }

    #[test]
    fn stamp_header_contains_versions_and_generator() {
        let s = stamp_header("c");
        assert!(s.starts_with("WeaveFFI "), "unexpected prefix: {s}");
        assert!(
            s.contains(weaveffi_ir::ir::CURRENT_SCHEMA_VERSION),
            "missing IR version in {s}"
        );
        assert!(s.contains(" c "), "missing generator name in {s}");
        assert!(
            s.contains(env!("CARGO_PKG_VERSION")),
            "missing tool version in {s}"
        );
        assert!(
            s.contains("DO NOT EDIT"),
            "missing DO NOT EDIT notice in {s}"
        );
        assert!(
            s.contains("regenerate with 'weaveffi generate'"),
            "missing regeneration hint in {s}"
        );
    }

    #[test]
    fn post_hook_failure_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig {
            post_generate: Some("exit 42".into()),
            ..Default::default()
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        let result = orch.run(&api, out_dir, &config, true, None);
        assert!(result.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "generator should have run");
    }

    #[test]
    fn lockfile_written_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen);
        orch.run(&api, out_dir, &config, true, None).unwrap();

        let lock_path = out_dir.join(LOCKFILE);
        assert!(lock_path.exists(), "lockfile should be written by default");

        let contents = std::fs::read_to_string(lock_path.as_std_path()).unwrap();
        let parsed: toml::Value = toml::from_str(&contents).expect("lockfile should parse as TOML");

        let meta = parsed
            .get("meta")
            .and_then(|v| v.as_table())
            .expect("[meta] section");
        assert_eq!(
            meta.get("ir_version").and_then(|v| v.as_str()),
            Some(weaveffi_ir::ir::CURRENT_SCHEMA_VERSION)
        );
        assert_eq!(
            meta.get("tool_version").and_then(|v| v.as_str()),
            Some(env!("CARGO_PKG_VERSION"))
        );
        let generated_at = meta
            .get("generated_at")
            .and_then(|v| v.as_str())
            .expect("generated_at");
        assert!(
            generated_at.ends_with('Z') && generated_at.len() == 20,
            "generated_at should be RFC-3339 UTC: {generated_at}"
        );

        let hash_section = parsed
            .get("hash")
            .and_then(|v| v.as_table())
            .expect("[hash] section");
        let api_hash = hash_section
            .get("api")
            .and_then(|v| v.as_str())
            .expect("api hash");
        assert_eq!(api_hash.len(), 64, "SHA-256 hex digest is 64 chars");
        assert_eq!(api_hash, cache::hash_api(&api));

        let files = parsed
            .get("files")
            .and_then(|v| v.as_table())
            .expect("[files] section");
        let recorded_hash = files
            .get("output.txt")
            .and_then(|v| v.as_str())
            .expect("output.txt entry");
        let actual_bytes = std::fs::read(out_dir.join("output.txt").as_std_path()).unwrap();
        assert_eq!(
            recorded_hash,
            format!("{:x}", Sha256::digest(&actual_bytes))
        );

        assert!(
            !files.contains_key(LOCKFILE),
            "lockfile must not reference itself"
        );
        assert!(
            !files.contains_key(".weaveffi-cache"),
            "cache file must not appear in lockfile"
        );
    }

    #[test]
    fn lockfile_disabled_via_builder() {
        let dir = tempfile::tempdir().unwrap();
        let out_dir = Utf8Path::from_path(dir.path()).unwrap();
        let api = test_api();
        let config = GeneratorConfig::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let gen = CountingGenerator {
            calls: Arc::clone(&calls),
        };

        let orch = Orchestrator::new().with_generator(&gen).lockfile(false);
        orch.run(&api, out_dir, &config, true, None).unwrap();

        assert!(
            !out_dir.join(LOCKFILE).exists(),
            "lockfile must not be written when disabled"
        );
    }

    #[test]
    fn format_rfc3339_utc_known_values() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(format_rfc3339_utc(epoch), "1970-01-01T00:00:00Z");

        let y2k = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(946_684_800);
        assert_eq!(format_rfc3339_utc(y2k), "2000-01-01T00:00:00Z");

        let leap_day = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_709_164_800);
        assert_eq!(format_rfc3339_utc(leap_day), "2024-02-29T00:00:00Z");
    }
}
