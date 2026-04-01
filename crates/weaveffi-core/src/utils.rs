/// Build the C symbol name for a function: `weaveffi_<module>_<func>`.
pub fn c_symbol_name(module: &str, func: &str) -> String {
    format!("weaveffi_{}_{}", module, func)
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
}
