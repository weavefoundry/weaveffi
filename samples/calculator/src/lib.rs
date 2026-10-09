//! Calculator sample cdylib: the smallest useful WeaveFFI producer.
//!
//! Plain, safe Rust functions: `add`, `divide` (which fails with the typed
//! `CalcError` domain on a zero divisor), `greet` (a string in and a string
//! out), `parse` (a second domain, `ParseError`, whose message names the
//! input), `sqrt` (an untyped `throws any` error), `mean` (a numeric list in,
//! an optional number out), and `running_total` (a numeric list in and
//! out). The `#[weaveffi::module]` attribute generates the
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

    /// The calculator's parsing errors: a second domain in the module.
    #[weaveffi::error]
    #[derive(Debug)]
    #[repr(i32)]
    pub enum ParseError {
        /// not a number
        #[weaveffi(message = "not a number: {text}")]
        NotANumber {
            /// The text that didn't parse.
            text: String,
        } = 1,
    }

    /// Parse a decimal integer (surrounding whitespace allowed). Fails with
    /// `NotANumber` carrying the input.
    #[weaveffi::export]
    pub fn parse(text: &str) -> Result<i32, ParseError> {
        text.trim().parse().map_err(|_| ParseError::NotANumber {
            text: text.to_string(),
        })
    }

    /// The square root of `x`. Fails with an untyped error (`throws any`)
    /// whose message is `"cannot take the square root of {x}"` for a
    /// negative `x`.
    #[weaveffi::export]
    pub fn sqrt(x: f64) -> Result<f64, String> {
        if x < 0.0 {
            return Err(format!("cannot take the square root of {x}"));
        }
        Ok(x.sqrt())
    }

    /// The arithmetic mean of `values`, or none for an empty list.
    #[weaveffi::export]
    pub fn mean(values: &[f64]) -> Option<f64> {
        if values.is_empty() {
            return None;
        }
        Some(values.iter().sum::<f64>() / values.len() as f64)
    }

    /// The running totals of `values` (wrapping on overflow):
    /// `[1, 2, 3]` gives `[1, 3, 6]`.
    #[weaveffi::export]
    pub fn running_total(values: Vec<i32>) -> Vec<i32> {
        values
            .iter()
            .scan(0i32, |total, v| {
                *total = total.wrapping_add(*v);
                Some(*total)
            })
            .collect()
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
        assert_eq!(
            len, 11,
            "seven functions, and two error domains with one code each"
        );
    }

    #[test]
    fn parse_reports_its_own_domain() {
        let mut err = FfiError::default();
        let ok = " 42 ";
        assert_eq!(
            unsafe { calculator_calculator_parse(ok.as_ptr(), ok.len(), &mut err) },
            42
        );
        let bad = "4x";
        assert_eq!(
            unsafe { calculator_calculator_parse(bad.as_ptr(), bad.len(), &mut err) },
            0
        );
        assert_eq!(err.code, 1);
        assert_eq!(message(&err), "not a number: 4x");
        assert_eq!(
            unsafe { abi::decode_value::<String>(err.payload()) }.unwrap(),
            "4x"
        );
        unsafe { abi::error_clear(&mut err) };
    }

    #[test]
    fn sqrt_fails_untyped() {
        let mut err = FfiError::default();
        assert_eq!(unsafe { calculator_calculator_sqrt(9.0, &mut err) }, 3.0);
        assert_eq!(unsafe { calculator_calculator_sqrt(-4.0, &mut err) }, 0.0);
        assert_eq!(err.code, abi::GENERIC_ERROR_CODE);
        assert_eq!(message(&err), "cannot take the square root of -4");
        unsafe { abi::error_clear(&mut err) };
    }

    #[test]
    fn lists_cross_as_typed_arrays() {
        let mut err = FfiError::default();
        let mut mean = 0.0f64;
        let xs = [1.0, 2.0, 6.0];
        assert!(unsafe { calculator_calculator_mean(xs.as_ptr(), 3, &mut mean, &mut err) });
        assert_eq!(mean, 3.0);
        assert!(!unsafe { calculator_calculator_mean(std::ptr::null(), 0, &mut mean, &mut err) });
        let ints = [1, 2, 3, i32::MAX];
        let mut len = 0usize;
        let ptr =
            unsafe { calculator_calculator_running_total(ints.as_ptr(), 4, &mut len, &mut err) };
        assert_eq!(
            unsafe { std::slice::from_raw_parts(ptr, len) },
            [1, 3, 6, i32::MIN + 5]
        );
        unsafe { abi::free_bytes(ptr.cast(), len * 4) };
    }
}
