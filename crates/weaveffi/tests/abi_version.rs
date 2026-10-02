//! The ABI revision is declared twice on purpose: once in `weaveffi-abi`
//! (which producers link) and once in `weaveffi-gen` (which generators
//! read). The runtime crate must not depend on the generator stack, so
//! this test is what keeps the two numbers equal.

#![allow(unsafe_code)]

weaveffi::export_runtime!();

#[test]
fn runtime_and_generator_abi_revisions_agree() {
    assert_eq!(weaveffi::abi::ABI_VERSION, weaveffi_gen::cabi::ABI_VERSION);
}

#[test]
fn exported_thunk_reports_the_runtime_revision() {
    assert_eq!(abi_version_abi_version(), weaveffi::abi::ABI_VERSION);
}
