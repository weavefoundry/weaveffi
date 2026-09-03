//! Compile-time contract of the `#[weaveffi::module]` family of macros.
//!
//! `ui/pass_*.rs` must expand and compile; `ui/fail_*.rs` must be rejected
//! with the diagnostic pinned in the matching `.stderr` file. Regenerate the
//! expectations with `TRYBUILD=overwrite cargo test -p weaveffi-macros --test ui`.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.pass("tests/ui/pass_*.rs");
    t.compile_fail("tests/ui/fail_*.rs");
}
