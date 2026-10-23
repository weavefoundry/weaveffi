//! The ABI revision is declared twice on purpose: once in the `weaveffi`
//! runtime (which producers link) and once in the model the generators read.
//! The runtime must not depend on the generator stack, so this test is what
//! keeps the two numbers equal.

#[test]
fn runtime_and_generator_abi_revisions_agree() {
    assert_eq!(
        weaveffi::abi::ABI_VERSION,
        weaveffi_model::model::ABI_VERSION
    );
}
