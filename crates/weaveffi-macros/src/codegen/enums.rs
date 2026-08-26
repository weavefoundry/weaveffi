//! Codegen for enums: the private `i32 -> enum` conversion plus the
//! [`weaveffi_abi::BufferValue`] implementation.
//!
//! A C-style enum crosses the ABI by value as an `i32`, and inside a value
//! buffer as the same four discriminant bytes. A rich (algebraic) enum is a
//! value type exactly like a record: it crosses the ABI serialized as an
//! `i32` tag followed by the active variant's fields in declaration order,
//! so its whole generated surface is one `BufferValue` impl.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use weaveffi_core::model::EnumBinding;

use super::helpers::{ident, rust_type_ident};
use super::marshal::{field_read_expr, field_write_stmt_ref};

/// Generate the surface for one enum: `__weaveffi_from_i32` plus a
/// `BufferValue` impl for a C-style enum, or the tag-and-fields
/// `BufferValue` impl for a rich (algebraic) enum.
pub(crate) fn gen_enum(e: &EnumBinding, item: Option<&syn::ItemEnum>) -> syn::Result<TokenStream> {
    if e.is_rich() {
        let item = item.ok_or_else(|| {
            syn::Error::new(
                Span::call_site(),
                format!("internal error: no source enum for rich enum `{}`", e.name),
            )
        })?;
        return gen_rich_enum(e, item);
    }
    let ty = rust_type_ident(&e.name);
    let arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        quote!(#value => ::std::option::Option::Some(Self::#vident),)
    });
    let write_arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        quote!(Self::#vident => #value,)
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

        impl ::weaveffi::abi::BufferValue for #ty {
            fn write_value(&self, __wv_w: &mut ::weaveffi::abi::BufferWriter) {
                __wv_w.write_i32(match self {
                    #(#write_arms)*
                });
            }
            fn read_value(
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<Self, ::weaveffi::abi::BufferDecodeError> {
                let __wv_v = __wv_r.read_i32()?;
                Self::__weaveffi_from_i32(__wv_v).ok_or(::weaveffi::abi::BufferDecodeError {
                    context: "enum discriminant out of range",
                })
            }
        }
    })
}

/// Generate the `BufferValue` impl for a rich (algebraic) enum: the write
/// side emits the active variant's tag then its fields in declaration order;
/// the read side dispatches on the tag and reconstructs the variant.
fn gen_rich_enum(e: &EnumBinding, item: &syn::ItemEnum) -> syn::Result<TokenStream> {
    let ty = rust_type_ident(&e.name);

    // Reject the variant shapes the codegen can't construct: only unit and
    // named-field (struct) variants are supported.
    for v in &item.variants {
        if matches!(v.fields, syn::Fields::Unnamed(_)) {
            return Err(syn::Error::new_spanned(
                v,
                "weaveffi: tuple-style rich-enum variants are not supported; use named fields",
            ));
        }
    }

    let write_arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        if v.fields.is_empty() {
            quote!(Self::#vident => { __wv_w.write_i32(#value); })
        } else {
            let bindings: Vec<syn::Ident> = v.fields.iter().map(|f| ident(&f.name)).collect();
            let writes: Vec<TokenStream> = v
                .fields
                .iter()
                .zip(&bindings)
                .map(|(f, b)| field_write_stmt_ref(f, b))
                .collect();
            quote! {
                Self::#vident { #(#bindings),* } => {
                    __wv_w.write_i32(#value);
                    #(#writes)*
                }
            }
        }
    });

    let read_arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        if v.fields.is_empty() {
            quote!(#value => Self::#vident,)
        } else {
            let inits: Vec<TokenStream> = v
                .fields
                .iter()
                .map(|f| {
                    let fname = ident(&f.name);
                    let read = field_read_expr(f);
                    quote!(#fname: #read?)
                })
                .collect();
            quote!(#value => Self::#vident { #(#inits),* },)
        }
    });

    Ok(quote! {
        #[allow(unsafe_code)]
        impl ::weaveffi::abi::BufferValue for #ty {
            fn write_value(&self, __wv_w: &mut ::weaveffi::abi::BufferWriter) {
                match self {
                    #(#write_arms)*
                }
            }
            fn read_value(
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<Self, ::weaveffi::abi::BufferDecodeError> {
                let __wv_tag = __wv_r.read_i32()?;
                ::std::result::Result::Ok(match __wv_tag {
                    #(#read_arms)*
                    _ => {
                        return ::std::result::Result::Err(::weaveffi::abi::BufferDecodeError {
                            context: "rich enum tag out of range",
                        })
                    }
                })
            }
        }
    })
}
