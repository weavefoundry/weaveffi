//! Per-target generator configuration.

use serde::{Deserialize, Serialize};

/// Configuration knobs that generators consult at code-generation time.
///
/// Every field falls back to a sensible default when `None`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeneratorConfig {
    pub swift_module_name: Option<String>,
    pub android_package: Option<String>,
    pub node_package_name: Option<String>,
    pub wasm_module_name: Option<String>,
    pub c_prefix: Option<String>,
    pub python_package_name: Option<String>,
    pub dotnet_namespace: Option<String>,
    #[serde(default)]
    pub strip_module_prefix: bool,
}

impl GeneratorConfig {
    pub fn swift_module_name(&self) -> &str {
        self.swift_module_name.as_deref().unwrap_or("WeaveFFI")
    }

    pub fn android_package(&self) -> &str {
        self.android_package.as_deref().unwrap_or("com.weaveffi")
    }

    pub fn node_package_name(&self) -> &str {
        self.node_package_name.as_deref().unwrap_or("weaveffi")
    }

    pub fn wasm_module_name(&self) -> &str {
        self.wasm_module_name.as_deref().unwrap_or("weaveffi_wasm")
    }

    pub fn c_prefix(&self) -> &str {
        self.c_prefix.as_deref().unwrap_or("weaveffi")
    }

    pub fn python_package_name(&self) -> &str {
        self.python_package_name.as_deref().unwrap_or("weaveffi")
    }

    pub fn dotnet_namespace(&self) -> &str {
        self.dotnet_namespace.as_deref().unwrap_or("WeaveFFI")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_applied() {
        let cfg = GeneratorConfig::default();

        assert_eq!(cfg.swift_module_name(), "WeaveFFI");
        assert_eq!(cfg.android_package(), "com.weaveffi");
        assert_eq!(cfg.node_package_name(), "weaveffi");
        assert_eq!(cfg.wasm_module_name(), "weaveffi_wasm");
        assert_eq!(cfg.c_prefix(), "weaveffi");
        assert_eq!(cfg.python_package_name(), "weaveffi");
        assert_eq!(cfg.dotnet_namespace(), "WeaveFFI");
        assert!(!cfg.strip_module_prefix);
    }

    #[test]
    fn custom_values_override_defaults() {
        let cfg = GeneratorConfig {
            swift_module_name: Some("MySwift".into()),
            android_package: Some("org.example".into()),
            node_package_name: Some("my-node-pkg".into()),
            wasm_module_name: Some("my_wasm".into()),
            c_prefix: Some("myffi".into()),
            python_package_name: Some("my_python_pkg".into()),
            dotnet_namespace: Some("MyCompany.Bindings".into()),
            strip_module_prefix: true,
        };

        assert_eq!(cfg.swift_module_name(), "MySwift");
        assert_eq!(cfg.android_package(), "org.example");
        assert_eq!(cfg.node_package_name(), "my-node-pkg");
        assert_eq!(cfg.wasm_module_name(), "my_wasm");
        assert_eq!(cfg.c_prefix(), "myffi");
        assert_eq!(cfg.python_package_name(), "my_python_pkg");
        assert_eq!(cfg.dotnet_namespace(), "MyCompany.Bindings");
        assert!(cfg.strip_module_prefix);
    }

    #[test]
    fn roundtrip_json() {
        let cfg = GeneratorConfig {
            swift_module_name: Some("S".into()),
            android_package: None,
            node_package_name: None,
            wasm_module_name: None,
            c_prefix: None,
            python_package_name: Some("mypkg".into()),
            dotnet_namespace: None,
            strip_module_prefix: true,
        };

        let json = serde_json::to_string(&cfg).unwrap();
        let back: GeneratorConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(back.swift_module_name(), "S");
        assert_eq!(back.android_package(), "com.weaveffi");
        assert_eq!(back.python_package_name(), "mypkg");
        assert_eq!(back.dotnet_namespace(), "WeaveFFI");
        assert!(back.strip_module_prefix);
    }

    #[test]
    fn deserialize_empty_object_gives_defaults() {
        let cfg: GeneratorConfig = serde_json::from_str("{}").unwrap();

        assert_eq!(cfg.swift_module_name(), "WeaveFFI");
        assert!(!cfg.strip_module_prefix);
    }
}
