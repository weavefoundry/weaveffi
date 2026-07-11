//! Thunk emission for enums: the private `i32 -> enum` conversion for C-style
//! enums, and the opaque-object surface (tag reader, per-variant constructors
//! and getters, destructor) for rich (algebraic) enums.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;
use weaveffi_core::abi::CType;
use weaveffi_core::model::{EnumBinding, FieldBinding, RichEnumBinding, RichVariantBinding};

use super::helpers::{ident, is_copy, ret_arrow, rust_type_ident, sentinel, slot_tokens};
use super::marshal::{field_as_param, lift_param, lower_value};

/// Generate the surface for one enum. A C-style enum needs only the private
/// `i32 -> enum` conversion the marshalling uses (the header emits its variants
/// as constants); a rich (algebraic) enum delegates to [`gen_rich_enum`] for
/// its opaque-object surface.
///
/// The split mirrors the declaration side of the `TypeRef::Enum` /
/// `TypeRef::RichEnum` reference split: references to a rich enum reuse the
/// record (opaque-pointer) marshalling, so only this declaration surface is
/// enum-specific.
pub(crate) fn gen_enum(e: &EnumBinding, item: Option<&syn::ItemEnum>) -> syn::Result<TokenStream> {
    if let Some(rich) = &e.rich {
        let item = item.ok_or_else(|| {
            syn::Error::new(
                Span::call_site(),
                format!("internal error: no source enum for rich enum `{}`", e.name),
            )
        })?;
        return gen_rich_enum(e, rich, item);
    }
    let ty = rust_type_ident(&e.name);
    let arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        quote!(#value => ::std::option::Option::Some(Self::#vident),)
    });
    Ok(quote! {
        #[allow(dead_code)]
        impl #ty {
            #[doc(hidden)]
            pub fn __weaveffi_from_i32(__v: i32) -> ::std::option::Option<Self> {
                match __v {
                    #(#arms)*
                    _ => ::std::option::Option::None,
                }
            }
        }
    })
}

/// True when a rich-enum variant carries associated data (a struct variant), so
/// its match pattern binds fields (`Enum::V { .. }`) rather than naming a unit
/// variant (`Enum::V`).
fn variant_has_fields(v: &RichVariantBinding) -> bool {
    !v.fields.is_empty()
}

/// Generate the opaque-object surface of a rich (algebraic) enum: a tag reader,
/// a destructor, and per-variant constructors and field getters. The enum value
/// itself crosses the ABI exactly like a record (an owning `*mut Enum`), so the
/// surrounding marshalling shares the `Record` arms; only this declaration set
/// is enum-specific.
fn gen_rich_enum(
    e: &EnumBinding,
    rich: &RichEnumBinding,
    item: &syn::ItemEnum,
) -> syn::Result<TokenStream> {
    let ty = rust_type_ident(&e.name);

    // Reject the variant shapes the per-variant codegen can't construct: only
    // unit and named-field (struct) variants are supported.
    for v in &item.variants {
        if matches!(v.fields, syn::Fields::Unnamed(_)) {
            return Err(syn::Error::new_spanned(
                v,
                "weaveffi: tuple-style rich-enum variants are not supported; use named fields",
            ));
        }
    }

    // tag: read the active variant's discriminant.
    let tag_sym = ident(&rich.tag_symbol);
    let tag_arms = rich.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        if variant_has_fields(v) {
            quote!(#ty::#vident { .. } => #value,)
        } else {
            quote!(#ty::#vident => #value,)
        }
    });
    let tag = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #tag_sym(ptr: *const #ty) -> i32 {
            assert!(!ptr.is_null());
            match unsafe { &*ptr } {
                #(#tag_arms)*
            }
        }
    };

    // destroy: the single release call the consumer owes for every owned
    // rich-enum pointer (`ReturnFree::OwnedObject`/`ElemFree::Object` in
    // `weaveffi_core::plan`), shared with the record convention.
    let destroy_sym = ident(&rich.destroy_symbol);
    let destroy = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #destroy_sym(ptr: *mut #ty) {
            if ptr.is_null() {
                return;
            }
            unsafe { drop(::std::boxed::Box::from_raw(ptr)) };
        }
    };

    // Per-variant constructors and field getters.
    let mut variants = TokenStream::new();
    for v in &rich.variants {
        let vident = ident(&v.name);
        let create_sym = ident(&v.create.symbol);
        let create_params: Vec<TokenStream> = v.create.params.iter().map(slot_tokens).collect();
        let create_sentinel = quote!(::std::ptr::null_mut());

        let mut create_pre = TokenStream::new();
        let mut field_inits: Vec<TokenStream> = Vec::new();
        for field in &v.fields {
            let pb = field_as_param(field);
            let (pre, _) = lift_param(&pb, false, &create_sentinel)?;
            create_pre.extend(pre);
            let fname = ident(&field.name);
            field_inits.push(quote!(#fname: #fname));
        }
        let ctor_value = if variant_has_fields(v) {
            quote!(#ty::#vident { #(#field_inits),* })
        } else {
            quote!(#ty::#vident)
        };
        variants.extend(quote! {
            #[no_mangle]
            #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
            pub extern "C" fn #create_sym(#(#create_params),*) -> *mut #ty {
                #create_pre
                ::weaveffi::abi::error_set_ok(out_err);
                ::std::boxed::Box::into_raw(::std::boxed::Box::new(#ctor_value))
            }
        });

        for field in &v.fields {
            variants.extend(gen_variant_getter(&ty, &vident, field)?);
        }
    }

    Ok(quote! {
        #tag
        #destroy
        #variants
    })
}

/// Generate one rich-enum variant field getter: it matches the active variant,
/// projects the requested field, and lowers it; any other variant yields the
/// type's zero/null sentinel.
fn gen_variant_getter(
    ty: &Ident,
    vident: &Ident,
    field: &FieldBinding,
) -> syn::Result<TokenStream> {
    let sym = ident(&field.getter_symbol);
    let fname = ident(&field.name);
    let mut params: Vec<TokenStream> = vec![quote!(ptr: *const #ty)];
    params.extend(field.getter_out_params.iter().map(slot_tokens));
    let arrow = ret_arrow(&field.getter_ret);
    let sent = sentinel(&field.getter_ret);

    // The matched binding is a reference; copy scalars, clone owned data.
    let read = if is_copy(&field.ty) {
        quote!(*#fname)
    } else {
        quote!(#fname.clone())
    };
    let lowered = lower_value(&field.ty, read)?;

    let guard = if matches!(field.getter_ret, CType::Void) {
        quote!(return;)
    } else {
        quote!(return #sent;)
    };
    let miss = if matches!(field.getter_ret, CType::Void) {
        quote!(_ => {})
    } else {
        quote!(_ => #sent,)
    };

    Ok(quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #sym(#(#params),*) #arrow {
            if ptr.is_null() {
                #guard
            }
            match unsafe { &*ptr } {
                #ty::#vident { #fname, .. } => #lowered,
                #miss
            }
        }
    })
}
