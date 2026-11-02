//! Calculator sample cdylib: the smallest useful WeaveFFI producer.
//!
//! Plain, safe Rust functions: `add`, `divide` (which fails with the typed
//! `CalcError` domain on a zero divisor), `greet` (a string in and a string
//! out), `parse` (a second domain, `ParseError`, whose message names the
//! input), `sqrt` (an untyped `throws any` error), `mean` (a numeric list in,
//! an optional number out), and `running_total` (a numeric list in and
//! out). Each `#[weaveffi::...]` attribute generates the `extern "C"`
//! functions the C ABI (and every generated binding) calls, so this file
//! contains no `unsafe` glue.
//!
//! This is the example the README and the getting-started guide walk
//! through; `kvstore` is the feature-complete sample.

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

/// Divide `a` by `b`, rounding toward zero. Fails with `DivisionByZero`
/// when `b` is zero.
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

/// The calculator's parsing errors: a second domain.
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

/// The square root of `x`. Fails with an untyped error (`throws any`) whose
/// message is `"cannot take the square root of {x}"` for a negative `x`.
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

/// The running totals of `values` (wrapping on overflow): `[1, 2, 3]`
/// gives `[1, 3, 6]`.
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

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use weaveffi::__rt::{FfiError, Opt, Run};

    // The library's own C functions, as a consumer calls them.
    unsafe extern "C" {
        fn calculator_add(a: i32, b: i32, err: *mut FfiError) -> i32;
        fn calculator_divide(a: i32, b: i32, err: *mut FfiError) -> i32;
        fn calculator_greet(name: Run, err: *mut FfiError) -> Run;
        fn calculator_parse(text: Run, err: *mut FfiError) -> i32;
        fn calculator_sqrt(x: f64, err: *mut FfiError) -> f64;
        fn calculator_mean(values: Run, err: *mut FfiError) -> Opt<f64>;
        fn calculator_running_total(values: Run, err: *mut FfiError) -> Run;
        fn calculator_contract_e70bc3190530e654() -> u64;
    }

    fn message(err: &FfiError) -> &str {
        unsafe { err.message_str() }.unwrap_or_default()
    }

    fn bytes<T: Copy>(values: &[T]) -> Run {
        Run::borrowing(unsafe {
            std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values))
        })
    }

    #[test]
    fn scalars_and_typed_errors() {
        let mut err = FfiError::default();
        assert_eq!(unsafe { calculator_add(2, 40, &mut err) }, 42);
        assert_eq!(err.code, 0);
        assert_eq!(unsafe { calculator_add(i32::MAX, 1, &mut err) }, i32::MIN);
        assert_eq!(unsafe { calculator_divide(-7, 2, &mut err) }, -3);
        assert_eq!(unsafe { calculator_divide(1, 0, &mut err) }, 0);
        assert_eq!(err.code, 1);
        assert_eq!(message(&err), "division by zero");
        unsafe { weaveffi::__rt::error_clear(&mut err) };
    }

    #[test]
    fn strings_round_trip() {
        let mut err = FfiError::default();
        let out = unsafe { calculator_greet(Run::borrowing("Wörld 🦀".as_bytes()), &mut err) };
        assert_eq!(err.code, 0);
        assert_eq!(unsafe { out.text() }.unwrap(), "Hello, Wörld 🦀!");
        unsafe { out.free() };
    }

    #[test]
    fn domains_carry_payloads_and_untyped_errors_messages() {
        let mut err = FfiError::default();
        assert_eq!(
            unsafe { calculator_parse(Run::borrowing(b" 42 "), &mut err) },
            42
        );
        assert_eq!(
            unsafe { calculator_parse(Run::borrowing(b"4x"), &mut err) },
            0
        );
        assert_eq!((err.code, message(&err)), (1, "not a number: 4x"));
        assert_eq!(err.payload(), &[2, 0, 0, 0, b'4', b'x']);
        unsafe { weaveffi::__rt::error_clear(&mut err) };
        assert_eq!(unsafe { calculator_sqrt(9.0, &mut err) }, 3.0);
        assert_eq!(unsafe { calculator_sqrt(-4.0, &mut err) }, 0.0);
        assert_eq!(err.code, weaveffi::__rt::GENERIC_ERROR_CODE);
        assert_eq!(message(&err), "cannot take the square root of -4");
        unsafe { weaveffi::__rt::error_clear(&mut err) };
    }

    #[test]
    fn lists_cross_as_typed_arrays() {
        let mut err = FfiError::default();
        let mean = unsafe { calculator_mean(bytes(&[1.0f64, 2.0, 6.0]), &mut err) };
        assert_eq!((mean.has, mean.value), (1, 3.0));
        let none = unsafe { calculator_mean(Run::EMPTY, &mut err) };
        assert_eq!(none.has, 0);
        let out = unsafe { calculator_running_total(bytes(&[1i32, 2, 3, i32::MAX]), &mut err) };
        assert_eq!(
            unsafe { out.slice::<i32>() }.unwrap(),
            [1, 3, 6, i32::MIN + 5]
        );
        unsafe { out.free() };
    }

    #[test]
    fn every_declaration_exports_its_contract() {
        // `add`'s contract function, by its id: fnv1a64("add").
        let id = weaveffi_model_free_fnv(b"add");
        assert_eq!(format!("{id:016x}"), "e70bc3190530e654");
        assert_eq!(
            unsafe { calculator_contract_e70bc3190530e654() },
            weaveffi_model_free_fnv(b"function add(i32, i32) -> i32")
        );
    }

    fn weaveffi_model_free_fnv(data: &[u8]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for b in data {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}
