//! Thunk emission for callbacks and listeners: the `extern "C"` function
//! pointer typedefs, the register/unregister symbols, and the safe `emit_*`
//! helpers.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use weaveffi_core::model::{CallbackBinding, ListenerBinding, ModuleBinding, ParamBinding};
use weaveffi_ir::ir::TypeRef;

use super::helpers::{ctype_to_rust, ident, is_copy, rust_type_ident, typeref_to_rust};
use super::unsupported;

/// Emit the `extern "C"` function-pointer typedef for a module-level callback.
///
/// The alias names every ABI slot (the callback's parameters plus the trailing
/// `void* context`) so a registered host function is called with exactly the
/// signature the generated header declares. It is `#[doc(hidden)]` because the
/// `register_*` symbol, not the bare typedef, is the producer-facing surface.
pub(crate) fn gen_callback_type(c: &CallbackBinding) -> syn::Result<TokenStream> {
    let ty = ident(&c.c_fn_type);
    let slots: Vec<TokenStream> = c.abi_params.iter().map(|p| ctype_to_rust(&p.ty)).collect();
    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub type #ty = extern "C" fn(#(#slots),*);
    })
}

/// Build the four pieces the emit helper needs for one callback parameter: the
/// Rust parameter it accepts, the lowering preamble, the C call argument(s) it
/// forwards, and any post-call cleanup. Borrowed inputs (`&str`, `&[u8]`,
/// `&Record`) are lowered into temporaries that live across every dispatch and
/// are freed once afterward, mirroring the hand-written events sample.
fn emit_callback_param(
    pb: &ParamBinding,
) -> syn::Result<(TokenStream, TokenStream, Vec<TokenStream>, TokenStream)> {
    let n = ident(&pb.name);
    let none = TokenStream::new();
    Ok(match &pb.ty {
        TypeRef::Enum(name) => {
            let et = rust_type_ident(name);
            (
                quote!(#n: #et),
                none.clone(),
                vec![quote!((#n) as i32)],
                none,
            )
        }
        ty if is_copy(ty) => {
            let rt = typeref_to_rust(ty)?;
            (quote!(#n: #rt), none.clone(), vec![quote!(#n)], none)
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let tmp = ident(&format!("__wv_cb_{}", pb.name));
            (
                quote!(#n: &str),
                quote!(let #tmp = ::weaveffi::abi::string_to_c_ptr(#n);),
                vec![quote!(#tmp)],
                quote!(::weaveffi::abi::free_string(#tmp);),
            )
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            let tmp = ident(&format!("__wv_cb_{}", pb.name));
            (
                quote!(#n: ::std::option::Option<&str>),
                quote!(let #tmp = ::weaveffi::abi::lower_opt_string(#n);),
                vec![quote!(#tmp)],
                quote!(if !#tmp.is_null() { ::weaveffi::abi::free_string(#tmp); }),
            )
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => (
            quote!(#n: &[u8]),
            none.clone(),
            vec![quote!(#n.as_ptr()), quote!(#n.len())],
            none,
        ),
        // A record or rich-enum payload is borrowed for the dispatch: the
        // producer keeps ownership and the host copies what it needs.
        TypeRef::Record(s) | TypeRef::RichEnum(s) => {
            let st = rust_type_ident(s);
            (
                quote!(#n: &#st),
                none.clone(),
                vec![quote!(#n as *const #st)],
                none,
            )
        }
        _ => return Err(unsupported(&pb.name, "callback parameter type")),
    })
}

/// Emit a listener's registry, its `register_*`/`unregister_*` symbols, and a
/// safe `emit_<listener>` helper the producer calls to fire every subscriber.
///
/// The registry is a module-level `Mutex<Vec<(callback, context, id)>>`;
/// `emit_*` snapshots the `(callback, context)` pairs under the lock, releases
/// it, lowers the event payload once, dispatches to each subscriber, and frees
/// the payload, so a re-entrant `register`/`unregister` from inside a callback
/// cannot deadlock.
pub(crate) fn gen_listener(mb: &ModuleBinding, l: &ListenerBinding) -> syn::Result<TokenStream> {
    let cb = mb.callback(&l.event_callback).ok_or_else(|| {
        syn::Error::new(
            Span::call_site(),
            format!(
                "weaveffi: listener `{}` references unknown callback `{}`",
                l.name, l.event_callback
            ),
        )
    })?;

    let cb_ty = ident(&l.callback_c_fn_type);
    let reg = ident(&format!("__WEAVEFFI_REG_{}", l.name));
    let next_id = ident(&format!("__WEAVEFFI_NEXTID_{}", l.name));
    let register_sym = ident(&l.register_symbol);
    let unregister_sym = ident(&l.unregister_symbol);
    let emit_fn = ident(&format!("emit_{}", l.name));

    let mut decls: Vec<TokenStream> = Vec::new();
    let mut pre = TokenStream::new();
    let mut c_args: Vec<TokenStream> = Vec::new();
    let mut cleanup = TokenStream::new();
    for pb in &cb.params {
        let (decl, p, args, free) = emit_callback_param(pb)?;
        decls.push(decl);
        pre.extend(p);
        c_args.extend(args);
        cleanup.extend(free);
    }

    let emit_doc = format!(
        "Fire the `{}` listener, invoking every registered `{}` callback.",
        l.name, l.event_callback
    );

    Ok(quote! {
        #[allow(non_upper_case_globals)]
        static #reg: ::std::sync::Mutex<::std::vec::Vec<(#cb_ty, usize, u64)>> =
            ::std::sync::Mutex::new(::std::vec::Vec::new());
        #[allow(non_upper_case_globals)]
        static #next_id: ::std::sync::atomic::AtomicU64 =
            ::std::sync::atomic::AtomicU64::new(1);

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #register_sym(
            callback: #cb_ty,
            context: *mut ::std::ffi::c_void,
        ) -> u64 {
            let __id = #next_id.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
            #reg.lock()
                .unwrap_or_else(::std::sync::PoisonError::into_inner)
                .push((callback, context as usize, __id));
            __id
        }

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #unregister_sym(id: u64) {
            #reg.lock()
                .unwrap_or_else(::std::sync::PoisonError::into_inner)
                .retain(|&(_, _, __i)| __i != id);
        }

        #[doc = #emit_doc]
        #[allow(unsafe_code)]
        pub fn #emit_fn(#(#decls),*) {
            let __targets: ::std::vec::Vec<(#cb_ty, usize)> = #reg
                .lock()
                .unwrap_or_else(::std::sync::PoisonError::into_inner)
                .iter()
                .map(|&(__c, __ctx, _)| (__c, __ctx))
                .collect();
            if __targets.is_empty() {
                return;
            }
            #pre
            for (__cb, __ctx) in __targets {
                __cb(#(#c_args,)* __ctx as *mut ::std::ffi::c_void);
            }
            #cleanup
        }
    })
}
