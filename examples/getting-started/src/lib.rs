//! The producer the getting-started walkthrough builds: integer arithmetic
//! with a typed error, and a greeting. The book includes this file, and the
//! README's quickstart repeats the region between the `lib` anchors (a test
//! below keeps the two identical).

// ANCHOR: lib
/// Errors the math functions report.
#[weaveffi::error]
#[derive(Debug)]
pub enum MathError {
    /// division by zero
    DivisionByZero = 1,
}

/// Add two integers, wrapping on overflow.
#[weaveffi::export]
pub fn add(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

/// Divide `a` by `b`, failing when `b` is zero.
#[weaveffi::export]
pub fn div(a: i32, b: i32) -> Result<i32, MathError> {
    a.checked_div(b).ok_or(MathError::DivisionByZero)
}

/// Greet someone by name.
#[weaveffi::export]
pub fn greet(name: &str) -> String {
    format!("Hello, {name}!")
}

// The runtime symbols (errors, memory, cancel tokens), once per library.
weaveffi::export_runtime!();
// ANCHOR_END: lib

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use weaveffi::__rt::{error_clear, FfiError, Run};

    // The library's C functions, declared the way a C consumer sees them.
    unsafe extern "C" {
        fn mathlib_abi_version() -> u32;
        fn mathlib_add(a: i32, b: i32, err: *mut FfiError) -> i32;
        fn mathlib_div(a: i32, b: i32, err: *mut FfiError) -> i32;
        fn mathlib_greet(name: Run, err: *mut FfiError) -> Run;
    }

    #[test]
    fn exports_revision_6() {
        assert_eq!(unsafe { mathlib_abi_version() }, 6);
    }

    #[test]
    fn add_and_div() {
        let mut err = FfiError::default();
        assert_eq!(unsafe { mathlib_add(2, 3, &mut err) }, 5);
        assert_eq!(err.code, 0);
        assert_eq!(unsafe { mathlib_div(7, 2, &mut err) }, 3);
        assert_eq!(err.code, 0);
        assert_eq!(unsafe { mathlib_div(1, 0, &mut err) }, 0);
        assert_eq!(err.code, 1);
        assert_eq!(unsafe { err.message_str() }.unwrap(), "division by zero");
        unsafe { error_clear(&mut err) };
        assert_eq!(err.code, 0);
    }

    #[test]
    fn greet_returns_a_run_the_caller_frees() {
        let mut err = FfiError::default();
        let out = unsafe { mathlib_greet(Run::borrowing(b"C"), &mut err) };
        assert_eq!(err.code, 0);
        assert_eq!(unsafe { out.text() }.unwrap(), "Hello, C!");
        unsafe { out.free() };
    }

    #[test]
    fn invalid_utf8_is_a_marshalling_failure() {
        let mut err = FfiError::default();
        let out = unsafe { mathlib_greet(Run::borrowing(&[0xC3, 0x28]), &mut err) };
        assert_eq!(err.code, -3);
        assert!(out.ptr.is_null() && out.len == 0);
        unsafe { error_clear(&mut err) };
    }

    /// The text between the `lib` anchors of this file.
    fn anchored(source: &str) -> String {
        let start = source.find("// ANCHOR: lib\n").unwrap() + "// ANCHOR: lib\n".len();
        let end = source.find("// ANCHOR_END: lib").unwrap();
        source[start..end].to_string()
    }

    #[test]
    fn the_readme_quickstart_matches_this_file() {
        let readme = include_str!("../../../README.md");
        let lib = anchored(include_str!("lib.rs"));
        assert!(
            readme.contains(&format!("```rust\n{lib}```")),
            "README.md's quickstart must repeat examples/getting-started/src/lib.rs"
        );
    }
}
