/// Build the C symbol name for a function: `weaveffi_<module>_<func>`.
pub fn c_symbol_name(module: &str, func: &str) -> String {
    format!("weaveffi_{}_{}", module, func)
}

/// Symbols (functions and types) exported by the `weaveffi-abi` runtime crate.
///
/// Generators that emit C/C++ headers use this list to produce
/// `#define {prefix}_{name} weaveffi_{name}` aliases at the top of the header
/// when a non-default `c_prefix` is configured, so consumer code can refer to
/// runtime helpers by the prefixed name while still linking against the
/// canonical `weaveffi_*` symbols supplied by `weaveffi-abi`.
pub const ABI_RUNTIME_SYMBOLS: &[&str] = &[
    "error",
    "handle_t",
    "error_set",
    "error_clear",
    "free_string",
    "free_bytes",
    "arena_create",
    "arena_destroy",
    "arena_register",
    "cancel_token",
    "cancel_token_create",
    "cancel_token_cancel",
    "cancel_token_is_cancelled",
    "cancel_token_destroy",
];

/// Render a `#define {prefix}_{name} weaveffi_{name}` block for runtime ABI
/// symbols. Returns an empty string when `prefix == "weaveffi"`.
pub fn render_abi_prefix_aliases(prefix: &str) -> String {
    if prefix == "weaveffi" {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("/* Aliases for weaveffi-abi runtime symbols */\n");
    for sym in ABI_RUNTIME_SYMBOLS {
        out.push_str(&format!("#define {prefix}_{sym} weaveffi_{sym}\n"));
    }
    out.push('\n');
    out
}

/// Build the wrapper function name exposed to the foreign language.
///
/// When `strip_module_prefix` is `true`, returns just `func`.
/// When `false`, returns `{module}_{func}`.
pub fn wrapper_name(module: &str, func: &str, strip_module_prefix: bool) -> String {
    if strip_module_prefix {
        func.to_string()
    } else {
        format!("{module}_{func}")
    }
}

/// Extract the local type name from a potentially qualified `module.TypeName`.
///
/// `"other.Contact"` → `"Contact"`, `"Contact"` → `"Contact"`.
pub fn local_type_name(name: &str) -> &str {
    name.split_once('.').map_or(name, |(_, local)| local)
}

/// Build the C ABI struct name, resolving cross-module qualified references.
///
/// `"other.Contact"` with any current module → `"{prefix}_other_Contact"`.
/// `"Contact"` with current module `"math"` → `"{prefix}_math_Contact"`.
pub fn c_abi_struct_name(name: &str, current_module: &str, prefix: &str) -> String {
    if let Some((mod_name, type_name)) = name.split_once('.') {
        format!("{prefix}_{mod_name}_{type_name}")
    } else {
        format!("{prefix}_{current_module}_{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_type_name_unqualified() {
        assert_eq!(local_type_name("Contact"), "Contact");
    }

    #[test]
    fn local_type_name_qualified() {
        assert_eq!(local_type_name("other.Contact"), "Contact");
    }

    #[test]
    fn c_abi_struct_name_unqualified() {
        assert_eq!(
            c_abi_struct_name("Contact", "math", "weaveffi"),
            "weaveffi_math_Contact"
        );
    }

    #[test]
    fn c_abi_struct_name_qualified() {
        assert_eq!(
            c_abi_struct_name("types.Name", "ops", "weaveffi"),
            "weaveffi_types_Name"
        );
    }

    #[test]
    fn abi_prefix_aliases_default_is_empty() {
        assert!(render_abi_prefix_aliases("weaveffi").is_empty());
    }

    #[test]
    fn abi_prefix_aliases_custom_lists_every_symbol() {
        let out = render_abi_prefix_aliases("myffi");
        for sym in ABI_RUNTIME_SYMBOLS {
            let line = format!("#define myffi_{sym} weaveffi_{sym}");
            assert!(out.contains(&line), "missing alias `{line}` in:\n{out}");
        }
    }
}
