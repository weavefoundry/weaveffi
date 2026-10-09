//! Generation records: what each target's last generation wrote.
//!
//! After generating a target, the orchestrator writes
//! `{out_dir}/.weaveffi-cache/{target}.json`: the path of every file the
//! target produced. Files recorded by the previous run that the current run
//! no longer produces are removed, so a renamed type never leaves a stale
//! file behind, while files the user added (a `node_modules/`, a build
//! directory) are never touched.

use std::collections::BTreeSet;

use camino::{Utf8Path, Utf8PathBuf};
use miette::{IntoDiagnostic, Result, WrapErr};
use serde::{Deserialize, Serialize};

const RECORD_DIR: &str = ".weaveffi-cache";

/// What one generation of one target produced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Record {
    /// Every file the target produced, relative to the output directory
    /// (with `/` separators).
    pub(crate) files: BTreeSet<String>,
}

fn record_path(out_dir: &Utf8Path, target: &str) -> Utf8PathBuf {
    out_dir.join(RECORD_DIR).join(format!("{target}.json"))
}

/// Read the record of the last generation of `target` under `out_dir`, if
/// any (a missing or unreadable record means "never generated").
pub(crate) fn read_record(out_dir: &Utf8Path, target: &str) -> Option<Record> {
    let text = std::fs::read_to_string(record_path(out_dir, target)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist the record of a generation of `target`.
pub(crate) fn write_record(out_dir: &Utf8Path, target: &str, record: &Record) -> Result<()> {
    let path = record_path(out_dir, target);
    let dir = out_dir.join(RECORD_DIR);
    std::fs::create_dir_all(dir.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to create the record directory {dir}"))?;
    let json = serde_json::to_string_pretty(record).into_diagnostic()?;
    std::fs::write(path.as_std_path(), json)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to write the generation record {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let out = Utf8Path::from_path(dir.path()).unwrap();
        assert_eq!(read_record(out, "swift"), None);
        let record = Record {
            files: ["swift/a.swift".to_string()].into(),
        };
        write_record(out, "swift", &record).unwrap();
        assert_eq!(read_record(out, "swift"), Some(record));
    }
}
