//! Per-target **feature capability declarations** and the loud-failure
//! contract that replaces silent feature skipping.
//!
//! Historically a backend that did not implement an IDL feature simply
//! omitted it from its output: Go and Ruby dropped `async` functions, nine
//! of eleven wrappers skipped callbacks and listeners, and nothing told the
//! user. That class of silent degradation is banned: every generator now
//! declares a [`TargetCapabilities`] and the orchestrator refuses to run a
//! generator against an API that uses a feature the target does not support,
//! listing each offending declaration by path.
//!
//! A backend that gains a feature flips the corresponding flag and the gate
//! opens; a backend that loses one (or a new feature lands in the IR before
//! every backend implements it) fails generation with an actionable error
//! instead of producing incomplete bindings.

use std::collections::BTreeMap;
use std::fmt;

use weaveffi_ir::ir::{Api, Module, TypeRef};

/// An IDL feature whose support varies (or could vary) per target.
///
/// Core types (scalars, strings, bytes, structs, enums, optionals, lists,
/// maps, handles) are mandatory for every backend and are not gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    /// `async: true` functions (callback-completed launchers).
    AsyncFunctions,
    /// Module-level callback typedefs (`callbacks:`).
    Callbacks,
    /// Listener register/unregister pairs (`listeners:`).
    Listeners,
    /// `iter<T>` returns (opaque iterator handle + `next`/`destroy`).
    Iterators,
}

impl Feature {
    /// Every gated feature, for exhaustive iteration in checks and tests.
    pub const ALL: [Feature; 4] = [
        Feature::AsyncFunctions,
        Feature::Callbacks,
        Feature::Listeners,
        Feature::Iterators,
    ];

    /// The IDL-facing name used in error messages.
    pub fn idl_name(&self) -> &'static str {
        match self {
            Feature::AsyncFunctions => "async functions",
            Feature::Callbacks => "callbacks",
            Feature::Listeners => "listeners",
            Feature::Iterators => "iterator returns (iter<T>)",
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.idl_name())
    }
}

/// The feature set a generator implements. Declared by every backend via
/// [`LanguageBackend::capabilities`](crate::backend::LanguageBackend::capabilities)
/// / [`Generator::capabilities`](crate::codegen::Generator::capabilities).
///
/// There is intentionally no `Default` impl: a backend must state what it
/// supports explicitly so a new gated feature cannot be claimed by omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetCapabilities {
    /// Whether the target generates `async: true` functions.
    pub async_functions: bool,
    /// Whether the target generates module-level callback typedefs.
    pub callbacks: bool,
    /// Whether the target generates listener register/unregister pairs.
    pub listeners: bool,
    /// Whether the target generates `iter<T>` returns.
    pub iterators: bool,
}

impl TargetCapabilities {
    /// Full support for every gated feature. Every shipped WeaveFFI backend
    /// declares this; partial sets exist for backends under development.
    pub const fn full() -> Self {
        Self {
            async_functions: true,
            callbacks: true,
            listeners: true,
            iterators: true,
        }
    }

    /// Whether this set includes `feature`.
    pub const fn supports(&self, feature: Feature) -> bool {
        match feature {
            Feature::AsyncFunctions => self.async_functions,
            Feature::Callbacks => self.callbacks,
            Feature::Listeners => self.listeners,
            Feature::Iterators => self.iterators,
        }
    }
}

/// Every gated feature `api` uses, mapped to the locations (dotted IDL paths)
/// that use it. Deterministic ordering for stable error output.
pub fn used_features(api: &Api) -> BTreeMap<Feature, Vec<String>> {
    let mut used: BTreeMap<Feature, Vec<String>> = BTreeMap::new();
    for module in &api.modules {
        collect_module(module, "", &mut used);
    }
    used
}

fn collect_module(module: &Module, parent: &str, used: &mut BTreeMap<Feature, Vec<String>>) {
    let path = if parent.is_empty() {
        module.name.clone()
    } else {
        format!("{parent}.{}", module.name)
    };
    for cb in &module.callbacks {
        used.entry(Feature::Callbacks)
            .or_default()
            .push(format!("{path}.{}", cb.name));
    }
    for l in &module.listeners {
        used.entry(Feature::Listeners)
            .or_default()
            .push(format!("{path}.{}", l.name));
    }
    for f in &module.functions {
        let loc = format!("{path}.{}", f.name);
        if f.r#async {
            used.entry(Feature::AsyncFunctions)
                .or_default()
                .push(loc.clone());
        }
        if matches!(f.returns, Some(TypeRef::Iterator(_))) {
            used.entry(Feature::Iterators).or_default().push(loc);
        }
    }
    for child in &module.modules {
        collect_module(child, &path, used);
    }
}

/// A target was asked to generate bindings for an API that uses features it
/// does not support. Carries every violation so the user sees the complete
/// picture in one failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct UnsupportedFeatures {
    /// The `--target` token of the failing generator.
    pub target: String,
    /// Each unsupported feature with the IDL paths that use it.
    pub violations: Vec<(Feature, Vec<String>)>,
}

impl fmt::Display for UnsupportedFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "target '{}' does not support every feature this IDL uses:",
            self.target
        )?;
        for (feature, locations) in &self.violations {
            writeln!(f, "  - {feature} (used by: {})", locations.join(", "))?;
        }
        write!(
            f,
            "remove the unsupported declarations, drop '{}' from --target, or set \
             `generators.{}.allow_unsupported: true` in the IDL to generate the supported \
             surface anyway (unsupported entry points become explicit throwing stubs)",
            self.target, self.target
        )
    }
}

/// Check `api` against one target's declared capabilities. `Ok(())` when the
/// target supports every feature the API uses.
///
/// # Errors
///
/// Returns [`UnsupportedFeatures`] when `api` uses one or more gated features
/// that `caps` does not declare support for. The error carries every offending
/// feature paired with the IDL paths that use it, so the caller can report all
/// violations at once.
pub fn check(
    api: &Api,
    target: &str,
    caps: &TargetCapabilities,
) -> Result<(), UnsupportedFeatures> {
    let violations: Vec<(Feature, Vec<String>)> = used_features(api)
        .into_iter()
        .filter(|(feature, _)| !caps.supports(*feature))
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(UnsupportedFeatures {
            target: target.to_string(),
            violations,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_ir::ir::{CallbackDef, Function, ListenerDef, Param};

    fn func(name: &str, is_async: bool, returns: Option<TypeRef>) -> Function {
        Function {
            name: name.into(),
            params: vec![Param {
                name: "x".into(),
                ty: TypeRef::I32,
                mutable: false,
                doc: None,
            }],
            returns,
            doc: None,
            r#async: is_async,
            cancellable: false,
            deprecated: None,
            since: None,
        }
    }

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            functions: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api(modules: Vec<Module>) -> Api {
        Api {
            version: "0.4.0".into(),
            modules,
            generators: None,
            package: None,
        }
    }

    fn events_api() -> Api {
        api(vec![Module {
            callbacks: vec![CallbackDef {
                name: "OnMessage".into(),
                params: vec![],
                doc: None,
            }],
            listeners: vec![ListenerDef {
                name: "message_listener".into(),
                event_callback: "OnMessage".into(),
                doc: None,
            }],
            functions: vec![
                func("send", false, None),
                func("fetch", true, Some(TypeRef::StringUtf8)),
                func(
                    "all",
                    false,
                    Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8))),
                ),
            ],
            ..module("events")
        }])
    }

    #[test]
    fn full_capabilities_pass_everything() {
        assert!(check(&events_api(), "c", &TargetCapabilities::full()).is_ok());
    }

    #[test]
    fn plain_api_uses_no_gated_features() {
        let plain = api(vec![Module {
            functions: vec![func("add", false, Some(TypeRef::I32))],
            ..module("math")
        }]);
        assert!(used_features(&plain).is_empty());
    }

    #[test]
    fn used_features_collects_locations() {
        let used = used_features(&events_api());
        assert_eq!(
            used[&Feature::Callbacks],
            vec!["events.OnMessage".to_string()]
        );
        assert_eq!(
            used[&Feature::Listeners],
            vec!["events.message_listener".to_string()]
        );
        assert_eq!(
            used[&Feature::AsyncFunctions],
            vec!["events.fetch".to_string()]
        );
        assert_eq!(used[&Feature::Iterators], vec!["events.all".to_string()]);
    }

    #[test]
    fn nested_modules_use_dotted_paths() {
        let nested = api(vec![Module {
            modules: vec![Module {
                functions: vec![func("fetch", true, None)],
                ..module("inner")
            }],
            ..module("outer")
        }]);
        let used = used_features(&nested);
        assert_eq!(
            used[&Feature::AsyncFunctions],
            vec!["outer.inner.fetch".to_string()]
        );
    }

    #[test]
    fn missing_capability_is_reported_with_locations() {
        let caps = TargetCapabilities {
            async_functions: false,
            listeners: false,
            ..TargetCapabilities::full()
        };
        let err = check(&events_api(), "go", &caps).unwrap_err();
        assert_eq!(err.target, "go");
        assert_eq!(err.violations.len(), 2);
        let msg = err.to_string();
        assert!(msg.contains("target 'go' does not support"), "{msg}");
        assert!(
            msg.contains("async functions (used by: events.fetch)"),
            "{msg}"
        );
        assert!(
            msg.contains("listeners (used by: events.message_listener)"),
            "{msg}"
        );
    }
}
