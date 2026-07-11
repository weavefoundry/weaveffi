//! Thunk emission for interfaces: the `_destroy` symbol.
//!
//! Interface members (constructors, methods, statics) share the callable
//! dispatch in [`super::sync`]; only the destructor is interface-specific.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::model::InterfaceBinding;

use super::helpers::ident;

/// Generate `{c_tag}_destroy`: release the boxed interface object. A panicking
/// user `Drop` is swallowed (there is no `out_err` slot to report through, and
/// a destructor must not take down the process). This is the release call an
/// interface return's `ReturnFree::OwnedObject` plan (see
/// `weaveffi_core::plan`) obliges the consumer to make exactly once.
pub(crate) fn gen_interface_destroy(i: &InterfaceBinding) -> TokenStream {
    let sym = ident(&i.destroy_symbol);
    let ty = ident(&i.name);
    quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #sym(ptr: *mut #ty) {
            if !ptr.is_null() {
                let _ = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
                    unsafe { drop(::std::boxed::Box::from_raw(ptr)) }
                }));
            }
        }
    }
}
