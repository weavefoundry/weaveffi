//! Codegen for records: the generated [`weaveffi::abi::BufferValue`]
//! implementation that serializes the producer's struct field by field in
//! declaration (wire) order, and the field codec records, rich enums, and
//! error payloads share.
//!
//! A record is a value type: it declares no C symbols of its own and crosses
//! the ABI serialized inside a value buffer, so all a record needs from the
//! expansion is the `BufferValue` impl the surrounding marshalling (parameter
//! decode, return encode, nesting inside other composites) calls into.

use proc_macro2::TokenStream;
use quote::quote;
use weaveffi_model::model::StructBinding;

use super::custom::{CustomScope, LiftSite};
use super::helpers::ident;

/// How one field encodes: its `encoded_len` expression, its `write_value`
/// statement (writing to `__wv_w`), and its `read_value` expression
/// (reading from `__wv_r`, propagating with `?`).
pub(crate) struct FieldCodec {
    pub(crate) len: TokenStream,
    pub(crate) write: TokenStream,
    pub(crate) read: TokenStream,
}

/// The codec of a field reached through `access` (an expression of type
/// `&T`), declared with the written type `ty`. A field whose type mentions
/// a custom type encodes as its repr.
pub(crate) fn field_codec(
    access: &TokenStream,
    ty: Option<&syn::Type>,
    customs: CustomScope<'_>,
) -> FieldCodec {
    match ty.and_then(|t| customs.shape(t)) {
        Some(shape) => {
            let repr = shape.repr_ty();
            let lowered = shape.lower(access.clone());
            let lifted = shape.lift(quote!(__wv_repr), LiftSite::Buffer);
            FieldCodec {
                len: quote!(::weaveffi::abi::BufferValue::encoded_len(&#lowered)),
                write: quote!(::weaveffi::abi::BufferValue::write_value(&#lowered, __wv_w);),
                read: quote!({
                    let __wv_repr: #repr = ::weaveffi::abi::BufferValue::read_value(__wv_r)?;
                    #lifted?
                }),
            }
        }
        None => FieldCodec {
            len: quote!(::weaveffi::abi::BufferValue::encoded_len(#access)),
            write: quote!(::weaveffi::abi::BufferValue::write_value(#access, __wv_w);),
            read: quote!(::weaveffi::abi::BufferValue::read_value(__wv_r)?),
        },
    }
}

/// The written type of the named field `name` in `fields`.
pub(crate) fn field_type<'a>(fields: Option<&'a syn::Fields>, name: &str) -> Option<&'a syn::Type> {
    fields?
        .iter()
        .find(|f| f.ident.as_ref().is_some_and(|i| i == name))
        .map(|f| &f.ty)
}

/// Generate the `BufferValue` and `ByValue` implementations for one record.
pub(crate) fn gen_record(
    s: &StructBinding,
    item: Option<&syn::ItemStruct>,
    customs: CustomScope<'_>,
) -> TokenStream {
    let rust_ty = ident(&s.name);
    let names: Vec<syn::Ident> = s.fields.iter().map(|f| ident(&f.name)).collect();
    let codecs: Vec<FieldCodec> = s
        .fields
        .iter()
        .zip(&names)
        .map(|(f, n)| {
            field_codec(
                &quote!(&self.#n),
                field_type(item.map(|i| &i.fields), &f.name),
                customs,
            )
        })
        .collect();
    let lens = codecs.iter().map(|c| &c.len);
    let writes = codecs.iter().map(|c| &c.write);
    let reads = codecs.iter().map(|c| &c.read);
    quote! {
        #[allow(unsafe_code, unused_unsafe)]
        impl ::weaveffi::abi::BufferValue for #rust_ty {
            fn encoded_len(&self) -> usize {
                0 #(+ #lens)*
            }
            fn write_value(&self, __wv_w: &mut ::weaveffi::abi::BufferWriter) {
                #(#writes)*
            }
            unsafe fn read_value(
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<Self, ::weaveffi::abi::BufferDecodeError> {
                // SAFETY: forwarded from the caller.
                ::std::result::Result::Ok(unsafe {
                    Self { #(#names: #reads),* }
                })
            }
        }

        impl ::weaveffi::abi::ByValue for #rust_ty {}
    }
}
