//! Codegen for error domains: the [`weaveffi::abi::ErrorDomain`]
//! implementation of each `#[weaveffi::error]` enum, and (unless the enum
//! says `no_display`) its `Display` and `std::error::Error`.
//!
//! The domain maps each variant to its declared code, serializes a
//! variant's fields into the error's value-buffer payload, and decodes a
//! code (and payload) back into the variant, which is how a callback method
//! that throws the domain receives the consumer's typed error. Its message
//! is its `Display` output: the variant's `#[weaveffi(message = "...")]`
//! template, which may name the variant's fields in braces (`"key not
//! found: {key}"`), else its documented default message.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use weaveffi_model::model::ErrorBinding;

use super::custom::CustomScope;
use super::enums::{field_local, variant_fields};
use super::helpers::ident;
use super::records::{field_codec, field_type};
use crate::extract::{default_message, error_no_display, variant_message};

/// Generate the error-domain surface of one `#[weaveffi::error]` enum.
///
/// # Errors
///
/// Returns an error for a malformed `#[weaveffi(...)]` or
/// `#[weaveffi::error(...)]` attribute.
pub(crate) fn gen_error_domain(
    eb: &ErrorBinding,
    item: &syn::ItemEnum,
    customs: CustomScope<'_>,
) -> syn::Result<TokenStream> {
    let ty = ident(&eb.name);
    let user_ty = &item.ident;
    let pattern = |c: &weaveffi_model::model::ErrorCodeBinding| {
        let v = ident(&c.name);
        if c.fields.is_empty() {
            quote!(Self::#v)
        } else {
            quote!(Self::#v { .. })
        }
    };
    let code_arms = eb.codes.iter().map(|c| {
        let pat = pattern(c);
        let value = c.value;
        quote!(#pat => #value,)
    });
    let mut payload_arms = Vec::new();
    let mut read_arms = Vec::new();
    for c in &eb.codes {
        let v = ident(&c.name);
        let value = c.value;
        let written = variant_fields(Some(item), &c.name);
        if c.fields.is_empty() {
            read_arms.push(quote!(#value => Self::#v,));
            continue;
        }
        let bindings: Vec<syn::Ident> = c.fields.iter().map(|f| ident(&f.name)).collect();
        let locals: Vec<syn::Ident> = c.fields.iter().map(|f| field_local(&f.name)).collect();
        let codecs: Vec<_> = c
            .fields
            .iter()
            .zip(&locals)
            .map(|(f, b)| field_codec(&quote!(#b), field_type(written, &f.name), customs))
            .collect();
        let lens = codecs.iter().map(|c| &c.len);
        let writes = codecs.iter().map(|c| &c.write);
        let reads = codecs.iter().map(|c| &c.read);
        payload_arms.push(quote! {
            Self::#v { #(#bindings: #locals),* } => {
                let mut __wv_buf =
                    ::weaveffi::abi::BufferWriter::with_capacity(0 #(+ #lens)*);
                let __wv_w = &mut __wv_buf;
                #(#writes)*
                __wv_buf.finish()
            }
        });
        read_arms.push(quote!(#value => Self::#v { #(#bindings: #reads),* },));
    }
    let payload_fn = if payload_arms.is_empty() {
        TokenStream::new()
    } else {
        quote! {
            fn payload(&self) -> ::std::vec::Vec<u8> {
                #[allow(unreachable_patterns)]
                match self {
                    #(#payload_arms)*
                    _ => ::std::vec::Vec::new(),
                }
            }
        }
    };

    // Spanned at the enum so a `no_display` enum without a `Display` impl
    // gets one error that points at it.
    let domain_impl = quote_spanned! {user_ty.span()=>
        #[allow(unsafe_code, unused_unsafe)]
        impl ::weaveffi::abi::ErrorDomain for #user_ty
    };
    let mut out = quote! {
        #domain_impl {
            fn code(&self) -> i32 {
                match self {
                    #(#code_arms)*
                }
            }
            #payload_fn
            unsafe fn read_code(
                __wv_code: i32,
                __wv_r: &mut ::weaveffi::abi::BufferReader<'_>,
            ) -> ::std::result::Result<
                ::std::option::Option<Self>,
                ::weaveffi::abi::BufferDecodeError,
            > {
                // SAFETY: forwarded from the caller.
                ::std::result::Result::Ok(::std::option::Option::Some(unsafe {
                    match __wv_code {
                        #(#read_arms)*
                        _ => return ::std::result::Result::Ok(::std::option::Option::None),
                    }
                }))
            }
        }
    };

    if !error_no_display(&item.attrs)? {
        let mut display_arms = Vec::new();
        for c in &eb.codes {
            let v = ident(&c.name);
            let variant = item.variants.iter().find(|x| x.ident == c.name);
            let attrs = variant.map_or(&[][..], |x| &x.attrs[..]);
            let bindings: Vec<syn::Ident> = c.fields.iter().map(|f| ident(&f.name)).collect();
            let locals: Vec<syn::Ident> = c.fields.iter().map(|f| field_local(&f.name)).collect();
            let pat = if bindings.is_empty() {
                quote!(Self::#v)
            } else {
                quote!(Self::#v { #(#bindings: #locals),* })
            };
            let body = match variant_message(attrs)? {
                Some(template) => {
                    // Each field the template names is passed as a named
                    // argument, so it's interpolated from the binding.
                    let named = placeholders(&template.value());
                    let args = c
                        .fields
                        .iter()
                        .filter(|f| named.contains(&f.name))
                        .map(|f| {
                            let name = ident(&f.name);
                            let local = field_local(&f.name);
                            quote!(, #name = #local)
                        });
                    quote!(::std::write!(__wv_f, #template #(#args)*))
                }
                None => {
                    let message = default_message(attrs, &c.name)?;
                    quote!(__wv_f.write_str(#message))
                }
            };
            display_arms.push(quote! {
                #[allow(unused_variables)]
                #pat => #body,
            });
        }
        let error_impl = quote_spanned! {user_ty.span()=>
            impl ::std::error::Error for #user_ty {}
        };
        out.extend(quote! {
            impl ::std::fmt::Display for #ty {
                fn fmt(&self, __wv_f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    match self {
                        #(#display_arms)*
                    }
                }
            }

            #error_impl
        });
    }
    Ok(out)
}

/// The names a format template's `{name}` and `{name:spec}` placeholders
/// refer to (`{{` escapes skipped).
fn placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' {
            continue;
        }
        if chars.peek() == Some(&'{') {
            chars.next();
            continue;
        }
        let name: String =
            std::iter::from_fn(|| chars.next_if(|c| *c == '_' || c.is_alphanumeric())).collect();
        if !name.is_empty() && !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::placeholders;

    #[test]
    fn placeholders_name_the_interpolated_fields() {
        assert_eq!(
            placeholders("{key} at {at:?}, {{literal}} {key} {0}"),
            ["key", "at", "0"]
        );
        assert!(placeholders("no fields").is_empty());
    }
}
