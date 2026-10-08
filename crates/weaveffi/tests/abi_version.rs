//! `export_runtime!` exports the runtime's ABI revision under the crate's
//! prefix (this test crate is `abi_version`).

#![allow(unsafe_code)]

weaveffi::export_runtime!();

#[test]
fn exported_thunk_reports_the_runtime_revision() {
    assert_eq!(abi_version_abi_version(), weaveffi::abi::ABI_VERSION);
}
