//! Thunk emission for interfaces: the `_clone` / `_destroy` reference-count
//! symbols and the thread-safety assertion.
//!
//! Interface members (constructors, methods, statics) share the callable
//! dispatch in [`super::sync`]; only the object lifecycle is
//! interface-specific. An interface object is an `Arc<T>` handed out as a raw
//! pointer (see `weaveffi_abi::object`), so `_clone` bumps the count and
//! `_destroy` releases one reference.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_core::model::InterfaceBinding;

use super::helpers::ident;

/// Generate the lifecycle surface for one interface:
///
/// * a compile-time `Send + Sync` assertion, because the consumer may call
///   methods and release references from any thread, and async methods hold
///   an `Arc<T>` across a spawn;
/// * `{c_tag}_clone`, which returns a new strong reference to the same object
///   (the plan's `RetPass::Object::clone_symbol`);
/// * `{c_tag}_destroy`, which releases one strong reference; the object drops
///   with the last one. A panicking user `Drop` is swallowed (there is no
///   `out_err` slot to report through, and a destructor must not take down the
///   process).
pub(crate) fn gen_interface_lifecycle(i: &InterfaceBinding) -> TokenStream {
    let clone_sym = ident(&i.clone_symbol);
    let destroy_sym = ident(&i.destroy_symbol);
    let ty = ident(&i.name);
    let assert_msg = format!(
        "weaveffi: interface `{}` must be Send + Sync because consumers may use it from any \
         thread; wrap interior state in Mutex/RwLock/atomics",
        i.name
    );
    quote! {
        const _: () = {
            #[doc = #assert_msg]
            const fn __wv_assert_send_sync<T: ::std::marker::Send + ::std::marker::Sync>() {}
            __wv_assert_send_sync::<#ty>();
        };

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #clone_sym(ptr: *const #ty) -> *mut #ty {
            unsafe { ::weaveffi::abi::object_clone(ptr) }
        }

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #destroy_sym(ptr: *mut #ty) {
            unsafe { ::weaveffi::abi::object_destroy(ptr) }
        }
    }
}
