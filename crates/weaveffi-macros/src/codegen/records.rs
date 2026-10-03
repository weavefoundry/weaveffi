//! Codegen for records: the generated [`weaveffi_abi::BufferValue`]
//! implementation that serializes the producer's struct field by field in
//! declaration (wire) order.
//!
//! A record is a value type: it declares no C symbols of its own and crosses
//! the ABI serialized inside a value buffer, so all a record needs from the
//! expansion is the `BufferValue` impl the surrounding marshalling (parameter
//! decode, return encode, nesting inside other composites) calls into.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::StructBinding;

use super::helpers::{ident, rust_type_ident};
use super::marshal::{field_read_expr, field_write_stmt};

/// Generate the `BufferValue` and `ByValue` implementations for one record.
pub(crate) fn gen_record(s: &StructBinding) -> TokenStream {
    let rust_ty = rust_type_ident(&s.name);
    let names: Vec<syn::Ident> = s.fields.iter().map(|f| ident(&f.name)).collect();
    let writes = s
        .fields
        .iter()
        .zip(&names)
        .map(|(f, n)| field_write_stmt(f, quote!(self.#n)));
    let reads = s.fields.iter().zip(&names).map(|(f, n)| {
        let read = field_read_expr(f);
        quote!(#n: #read?)
    });

    quote! {
        impl ::weaveffi::abi::BufferValue for #rust_ty {
            fn encoded_len(&self) -> usize {
                0 #(+ ::weaveffi::abi::BufferValue::encoded_len(&self.#names))*
            }
            fn write_value(&self, __wv_w: &mut ::weaveffi::abi::BufferWriter) {
                #(#writes)*
            }
            fn read_value(
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<Self, ::weaveffi::abi::BufferDecodeError> {
                ::std::result::Result::Ok(Self { #(#reads),* })
            }
        }

        impl ::weaveffi::abi::ByValue for #rust_ty {}
    }
}
