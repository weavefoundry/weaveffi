//! Codegen for enums: the [`weaveffi::abi::Scalar`] implementation of a
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

use super::custom::CustomScope;
use super::helpers::ident;
use super::records::{field_codec, field_type, FieldCodec};

/// Generate the surface for one enum: `Scalar` plus a `BufferValue` impl for
/// a C-style enum, or the tag-and-fields `BufferValue` impl for a rich
/// (algebraic) enum.
pub(crate) fn gen_enum(
    e: &EnumBinding,
    item: Option<&syn::ItemEnum>,
    customs: CustomScope<'_>,
) -> TokenStream {
    if e.is_rich() {
        return gen_rich_enum(e, item, customs);
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
        impl ::weaveffi::abi::Scalar for #ty {
            type Abi = i32;
            fn from_abi(__wv_value: i32) -> ::std::option::Option<Self> {
                match __wv_value {
                    #(#from_arms)*
                    _ => ::std::option::Option::None,
                }
            }
            fn to_abi(&self) -> i32 {
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

/// The codecs of a variant's fields, reached through the bindings its
/// pattern introduces (each a `&T`).
fn variant_codecs(
    fields: &[weaveffi_model::model::FieldBinding],
    written: Option<&syn::Fields>,
    customs: CustomScope<'_>,
) -> Vec<FieldCodec> {
    fields
        .iter()
        .map(|f| {
            let binding = field_local(&f.name);
            field_codec(&quote!(#binding), field_type(written, &f.name), customs)
        })
        .collect()
}

/// The local a variant's field `name` binds to in a generated pattern:
/// `__wv_f_{name}`, so a constant the producer declared with the field's
/// name can't turn the binding into a constant pattern.
pub(crate) fn field_local(name: &str) -> syn::Ident {
    quote::format_ident!("__wv_f_{}", name)
}

/// The written fields of the variant `name` of `item`.
pub(crate) fn variant_fields<'a>(
    item: Option<&'a syn::ItemEnum>,
    name: &str,
) -> Option<&'a syn::Fields> {
    item?
        .variants
        .iter()
        .find(|v| v.ident == name)
        .map(|v| &v.fields)
}

/// Generate the `BufferValue` impl for a rich (algebraic) enum: the write
/// side emits the active variant's tag then its fields in declaration order;
/// the read side dispatches on the tag and reconstructs the variant.
fn gen_rich_enum(
    e: &EnumBinding,
    item: Option<&syn::ItemEnum>,
    customs: CustomScope<'_>,
) -> TokenStream {
    let ty = ident(&e.name);
    let mut len_arms = Vec::new();
    let mut write_arms = Vec::new();
    let mut read_arms = Vec::new();
    for v in &e.variants {
        let value = v.value;
        let vident = ident(&v.name);
        let bindings: Vec<syn::Ident> = v.fields.iter().map(|f| ident(&f.name)).collect();
        let locals: Vec<syn::Ident> = v.fields.iter().map(|f| field_local(&f.name)).collect();
        let codecs = variant_codecs(&v.fields, variant_fields(item, &v.name), customs);
        let pattern = if bindings.is_empty() {
            quote!(Self::#vident)
        } else {
            quote!(Self::#vident { #(#bindings: #locals),* })
        };
        let lens = codecs.iter().map(|c| &c.len);
        let writes = codecs.iter().map(|c| &c.write);
        let reads = codecs.iter().map(|c| &c.read);
        len_arms.push(quote!(#pattern => 4 #(+ #lens)*,));
        write_arms.push(quote! {
            #pattern => {
                __wv_w.write_i32(#value);
                #(#writes)*
            }
        });
        read_arms.push(if bindings.is_empty() {
            quote!(#value => Self::#vident,)
        } else {
            quote!(#value => Self::#vident { #(#bindings: #reads),* },)
        });
    }

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
