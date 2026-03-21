/// Build the C symbol name for a function: `weaveffi_<module>_<func>`.
pub fn c_symbol_name(module: &str, func: &str) -> String {
    format!("weaveffi_{}_{}", module, func)
}
