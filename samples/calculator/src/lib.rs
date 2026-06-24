//! Calculator sample cdylib: the canonical minimal WeaveFFI producer.
//!
//! Every exported function is plain, safe Rust. The `#[weaveffi::module]`
//! attribute reads the annotated items and generates the `#[no_mangle]
//! extern "C"` thunks that the stable C ABI (and every generated language
//! binding) calls, marshalling arguments and results through the runtime so
//! this file contains no `unsafe` glue.

#[weaveffi::module]
pub mod calculator {
    /// Add two integers.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a + b
    }

    /// Multiply two integers.
    #[weaveffi::export]
    pub fn mul(a: i32, b: i32) -> i32 {
        a * b
    }

    /// Divide two integers, reporting division by zero through the error channel.
    #[weaveffi::export]
    pub fn div(a: i32, b: i32) -> Result<i32, String> {
        if b == 0 {
            return Err("division by zero".to_string());
        }
        Ok(a / b)
    }

    /// Echo a string back to the caller.
    #[weaveffi::export]
    pub fn echo(s: String) -> String {
        s
    }
}

weaveffi::export_runtime!();
