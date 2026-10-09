//! Calculator sample cdylib: the smallest useful WeaveFFI producer.
//!
//! Three plain, safe Rust functions: `add`, `divide` (which fails with the
//! typed `CalcError` domain on a zero divisor), and `greet` (a string in and
//! a string out). The `#[weaveffi::module]` attribute generates the
//! `#[no_mangle] extern "C"` thunks that the stable C ABI (and every
//! generated binding) calls, so this file contains no `unsafe` glue.
//!
//! This is the example the README and the getting-started guide walk
//! through; `kvstore` is the feature-complete sample.

#[weaveffi::module]
pub mod calculator {
    /// The calculator's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum CalcError {
        /// division by zero
        DivisionByZero = 1,
    }

    impl std::fmt::Display for CalcError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("division by zero")
        }
    }

    /// Add two integers, wrapping on overflow.
    #[weaveffi::export]
    pub fn add(a: i32, b: i32) -> i32 {
        a.wrapping_add(b)
    }

    /// Divide `a` by `b`, rounding toward zero. Fails with
    /// `DivisionByZero` when `b` is zero.
    #[weaveffi::export]
    pub fn divide(a: i32, b: i32) -> Result<i32, CalcError> {
        if b == 0 {
            return Err(CalcError::DivisionByZero);
        }
        Ok(a.wrapping_div(b))
    }

    /// Greet `name`: `"Hello, {name}!"`.
    #[weaveffi::export]
    pub fn greet(name: String) -> String {
        format!("Hello, {name}!")
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use super::calculator::*;
    use weaveffi::abi::{self, FfiError};

    fn message(err: &FfiError) -> &str {
        unsafe { err.message_str() }.unwrap_or_default()
    }

    #[test]
    fn add_wraps() {
        let mut err = FfiError::default();
        assert_eq!(unsafe { calculator_calculator_add(2, 40, &mut err) }, 42);
        assert_eq!(err.code, 0);
        assert_eq!(
            unsafe { calculator_calculator_add(i32::MAX, 1, &mut err) },
            i32::MIN
        );
    }

    #[test]
    fn divide_ok_path() {
        let mut err = FfiError::default();
        assert_eq!(unsafe { calculator_calculator_divide(-7, 2, &mut err) }, -3);
        assert_eq!(err.code, 0);
        assert_eq!(
            unsafe { calculator_calculator_divide(i32::MIN, -1, &mut err) },
            i32::MIN
        );
    }

    #[test]
    fn divide_by_zero_reports_domain_code() {
        let mut err = FfiError::default();
        let r = unsafe { calculator_calculator_divide(1, 0, &mut err) };
        assert_eq!(r, 0, "error path returns the zero sentinel");
        assert_eq!(err.code, 1, "CalcError::DivisionByZero's declared code");
        assert_eq!(message(&err), "division by zero");
    }

    #[test]
    fn greet_round_trips_utf8() {
        let name = "Wörld \u{1F980}";
        let mut err = FfiError::default();
        let mut len = 0usize;
        let ptr =
            unsafe { calculator_calculator_greet(name.as_ptr(), name.len(), &mut len, &mut err) };
        assert_eq!(err.code, 0);
        assert_eq!(
            unsafe { abi::lift_str(ptr, len) },
            Some("Hello, Wörld \u{1F980}!")
        );
        unsafe { abi::free_bytes(ptr.cast_mut(), len) };
    }

    #[test]
    fn contract_table_is_exported() {
        let mut len = 0usize;
        let table = unsafe { calculator_calculator_contract(&mut len) };
        assert!(!table.is_null());
        assert_eq!(len, 4, "three functions and the error domain");
    }
}
