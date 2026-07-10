//! Calculator sample cdylib: the canonical minimal WeaveFFI producer.
//!
//! Every exported function is plain, safe Rust. The `#[weaveffi::module]`
//! attribute reads the annotated items and generates the `#[no_mangle]
//! extern "C"` thunks that the stable C ABI (and every generated language
//! binding) calls, marshalling arguments and results through the runtime so
//! this file contains no `unsafe` glue. The `CalcError` domain shows the
//! smallest possible typed error surface: one code, one throwing function.

#[weaveffi::module]
pub mod calculator {
    /// The calculator's error domain: the codes its throwing functions report.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CalcError {
        /// division by zero
        DivisionByZero = 1,
    }

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

    /// Divide two integers, failing on a zero divisor.
    #[weaveffi::export]
    pub fn div(a: i32, b: i32) -> Result<i32, CalcError> {
        if b == 0 {
            return Err(CalcError::DivisionByZero);
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

#[cfg(test)]
mod tests {
    use super::calculator::*;
    use weaveffi::abi::{self, c_ptr_to_string, weaveffi_error};

    #[test]
    fn add_and_mul() {
        let mut err = weaveffi_error::default();
        assert_eq!(weaveffi_calculator_add(2, 40, &mut err), 42);
        assert_eq!(err.code, 0);
        assert_eq!(weaveffi_calculator_mul(6, 7, &mut err), 42);
        assert_eq!(err.code, 0);
    }

    #[test]
    fn div_ok_path() {
        let mut err = weaveffi_error::default();
        assert_eq!(weaveffi_calculator_div(10, 2, &mut err), 5);
        assert_eq!(err.code, 0);
    }

    #[test]
    fn div_by_zero_reports_domain_code() {
        let mut err = weaveffi_error::default();
        let r = weaveffi_calculator_div(1, 0, &mut err);
        assert_eq!(r, 0, "error path returns the zero sentinel");
        assert_eq!(err.code, 1, "CalcError::DivisionByZero's declared code");
        assert_eq!(c_ptr_to_string(err.message).unwrap(), "division by zero");
        abi::error_clear(&mut err);
    }
}
