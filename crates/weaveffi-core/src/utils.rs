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
