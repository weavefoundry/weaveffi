//! Codegen for enums: the [`weaveffi::abi::CEnum`] implementation of a
//! C-style enum, and the [`weaveffi::abi::BufferValue`] implementation of
//! every enum.
//!
//! A C-style enum crosses the ABI by value as an `i32`, and inside a value
//! buffer as the same four discriminant bytes. A rich (algebraic) enum is a
//! value type exactly like a record: it crosses the ABI serialized as an
//! `i32` tag followed by the active variant's fields in declaration order,
//! so its whole generated surface is one `BufferValue` impl.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::EnumBinding;

use super::helpers::ident;

/// Generate the surface for one enum: `CEnum` plus a `BufferValue` impl for
/// a C-style enum, or the tag-and-fields `BufferValue` impl for a rich
/// (algebraic) enum.
pub(crate) fn gen_enum(e: &EnumBinding) -> TokenStream {
    if e.is_rich() {
        return gen_rich_enum(e);
    }
    let ty = ident(&e.name);
    let from_arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        quote!(#value => ::std::option::Option::Some(Self::#vident),)
    });
    let to_arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        quote!(Self::#vident => #value,)
    });
    quote! {
        impl ::weaveffi::abi::CEnum for #ty {
            fn from_i32(value: i32) -> ::std::option::Option<Self> {
                match value {
                    #(#from_arms)*
                    _ => ::std::option::Option::None,
                }
            }
            fn to_i32(&self) -> i32 {
                match self {
                    #(#to_arms)*
                }
            }
        }

        #[allow(unsafe_code, unused_unsafe)]
        impl ::weaveffi::abi::BufferValue for #ty {
            fn encoded_len(&self) -> usize {
                4
            }
            fn write_value(&self, __wv_w: &mut ::weaveffi::abi::BufferWriter) {
                ::weaveffi::abi::write_enum(self, __wv_w);
            }
            unsafe fn read_value(
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<Self, ::weaveffi::abi::BufferDecodeError> {
                ::weaveffi::abi::read_enum(__wv_r)
            }
        }
    }
}

/// Generate the `BufferValue` impl for a rich (algebraic) enum: the write
/// side emits the active variant's tag then its fields in declaration order;
/// the read side dispatches on the tag and reconstructs the variant.
fn gen_rich_enum(e: &EnumBinding) -> TokenStream {
    let ty = ident(&e.name);
    let len_arms = e.variants.iter().map(|v| {
        let vident = ident(&v.name);
        if v.fields.is_empty() {
            quote!(Self::#vident => 4,)
        } else {
            let bindings: Vec<syn::Ident> = v.fields.iter().map(|f| ident(&f.name)).collect();
            quote! {
                Self::#vident { #(#bindings),* } => {
                    4 #(+ ::weaveffi::abi::BufferValue::encoded_len(#bindings))*
                }
            }
        }
    });
    let write_arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        let bindings: Vec<syn::Ident> = v.fields.iter().map(|f| ident(&f.name)).collect();
        let pattern = if bindings.is_empty() {
            quote!(Self::#vident)
        } else {
            quote!(Self::#vident { #(#bindings),* })
        };
        quote! {
            #pattern => {
                __wv_w.write_i32(#value);
                #(::weaveffi::abi::BufferValue::write_value(#bindings, __wv_w);)*
            }
        }
    });
    let read_arms = e.variants.iter().map(|v| {
        let value = v.value;
        let vident = ident(&v.name);
        if v.fields.is_empty() {
            quote!(#value => Self::#vident,)
        } else {
            let names: Vec<syn::Ident> = v.fields.iter().map(|f| ident(&f.name)).collect();
            quote! {
                #value => Self::#vident {
                    #(#names: ::weaveffi::abi::BufferValue::read_value(__wv_r)?),*
                },
            }
        }
    });

    quote! {
        #[allow(unsafe_code, unused_unsafe)]
        impl ::weaveffi::abi::BufferValue for #ty {
            fn encoded_len(&self) -> usize {
                match self {
                    #(#len_arms)*
                }
            }
            fn write_value(&self, __wv_w: &mut ::weaveffi::abi::BufferWriter) {
                match self {
                    #(#write_arms)*
                }
            }
            unsafe fn read_value(
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<Self, ::weaveffi::abi::BufferDecodeError> {
                let __wv_tag = __wv_r.read_i32()?;
                // SAFETY: forwarded from the caller.
                ::std::result::Result::Ok(unsafe {
                    match __wv_tag {
                        #(#read_arms)*
                        _ => {
                            return ::std::result::Result::Err(
                                ::weaveffi::abi::BufferDecodeError {
                                    context: "rich enum tag out of range",
                                },
                            )
                        }
                    }
                })
            }
        }

        impl ::weaveffi::abi::ByValue for #ty {}
    }
}
