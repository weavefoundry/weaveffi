//! Content-hashing and caching for skip-if-unchanged builds.

use anyhow::Result;
use camino::Utf8Path;
use sha2::{Digest, Sha256};
use weaveffi_ir::ir::Api;

const CACHE_FILE: &str = ".weaveffi-cache";

/// Serialize the API to canonical JSON and return its SHA-256 hex digest.
pub fn hash_api(api: &Api) -> String {
    let json = serde_json::to_string(api).expect("Api serialization should not fail");
    let hash = Sha256::digest(json.as_bytes());
    format!("{hash:x}")
}

/// Read a previously written cache hash from the output directory.
pub fn read_cache(out_dir: &Utf8Path) -> Option<String> {
    std::fs::read_to_string(out_dir.join(CACHE_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Write the hash to the cache file in the output directory.
pub fn write_cache(out_dir: &Utf8Path, hash: &str) -> Result<()> {
    std::fs::write(out_dir.join(CACHE_FILE), hash)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{Function, Module, Param, TypeRef};

    fn minimal_api() -> Api {
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
                        },
                        Param {
                            name: "b".to_string(),
                            ty: TypeRef::I32,
                        },
                    ],
                    returns: Some(TypeRef::I32),
                    doc: None,
                    r#async: false,
                }],
                structs: vec![],
                enums: vec![],
                errors: None,
            }],
        }
    }

    #[test]
    fn hash_deterministic() {
        let api = minimal_api();
        let h1 = hash_api(&api);
        let h2 = hash_api(&api);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex digest
    }

    #[test]
    fn hash_changes_on_modification() {
        let mut api = minimal_api();
        let h1 = hash_api(&api);

        api.modules[0].functions.push(Function {
            name: "subtract".to_string(),
            params: vec![
                Param {
                    name: "a".to_string(),
                    ty: TypeRef::I32,
                },
                Param {
                    name: "b".to_string(),
                    ty: TypeRef::I32,
                },
            ],
            returns: Some(TypeRef::I32),
            doc: None,
            r#async: false,
        });
        let h2 = hash_api(&api);

        assert_ne!(h1, h2);
    }

    #[test]
    fn cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();

        let hash = hash_api(&minimal_api());
        write_cache(dir_path, &hash).unwrap();

        let read_back = read_cache(dir_path);
        assert_eq!(read_back, Some(hash));
    }

    #[test]
    fn read_cache_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let dir_path = Utf8Path::from_path(dir.path()).unwrap();
        assert_eq!(read_cache(dir_path), None);
    }
}
