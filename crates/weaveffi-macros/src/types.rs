//! The value-type attributes: `#[weaveffi::record]`,
//! `#[weaveffi::enumeration]`, `#[weaveffi::error]`,
//! `#[weaveffi::interface]`, and `#[weaveffi::custom]`.
//!
//! Each implements the runtime's value traits for the type under the crate's
//! tag (a record's or rich enum's `Codec`, a C-style enum's `CEnum`, an
//! error domain's `ErrorType`, an interface's object traits) and exports its
//! metadata frame and contract function.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::spanned::Spanned;
use weaveffi_model::contract::Sig;
use weaveffi_model::ir::{
    EnumDef, EnumVariant, ErrorCode, ErrorDomain, Field, InterfaceDef, RecordDef,
};
use weaveffi_model::meta::Item;

use crate::common::{
    cfgs, contract, deprecated, docs, is_helper, location, name_lit, no_member_cfg, prefix, scoped,
    tag, FrameBuilder,
};

fn no_args(attr: &TokenStream, what: &str) -> syn::Result<()> {
    if attr.is_empty() {
        Ok(())
    } else {
        Err(syn::Error::new(
            attr.span(),
            format!("weaveffi: `#[weaveffi::{what}]` takes no arguments"),
        ))
    }
}

fn no_generics(generics: &syn::Generics) -> syn::Result<()> {
    if generics.params.is_empty() {
        Ok(())
    } else {
        Err(syn::Error::new(
            generics.span(),
            "weaveffi: an exported type can't be generic",
        ))
    }
}

/// The local each of `n` fields binds to in a generated `match` arm:
/// `Self::V { key: __wv_f0 }`, never the shorthand `Self::V { key }`, which
/// a constant named `key` in scope would turn into a constant pattern.
fn field_locals(n: usize) -> Vec<syn::Ident> {
    (0..n).map(|i| format_ident!("__wv_f{i}")).collect()
}

/// The `Name` expression of a field's type.
fn field_name(ty: &syn::Type) -> TokenStream {
    let tag = tag();
    quote!(<#ty as ::weaveffi::__rt::FfiType<#tag>>::NAME)
}

/// A named field: its identifier, type, and docs.
struct NamedField<'a> {
    ident: &'a syn::Ident,
    ty: &'a syn::Type,
    attrs: &'a [syn::Attribute],
}

fn named_fields<'a>(fields: &'a syn::Fields, what: &str) -> syn::Result<Vec<NamedField<'a>>> {
    match fields {
        syn::Fields::Named(named) => named
            .named
            .iter()
            .map(|f| {
                no_member_cfg(&f.attrs, &format!("{what} field"))?;
                Ok(NamedField {
                    ident: f.ident.as_ref().expect("a named field"),
                    ty: &f.ty,
                    attrs: &f.attrs,
                })
            })
            .collect(),
        syn::Fields::Unit => Ok(Vec::new()),
        syn::Fields::Unnamed(u) => Err(syn::Error::new(
            u.span(),
            format!("weaveffi: {what} fields must be named"),
        )),
    }
}

fn field_ir(fb: &mut FrameBuilder, fields: &[NamedField<'_>]) -> Vec<Field> {
    fields
        .iter()
        .map(|f| Field {
            name: f.ident.to_string().trim_start_matches("r#").to_string(),
            ty: fb.ty(field_name(f.ty)),
            doc: docs(f.attrs),
        })
        .collect()
}

/// Whether any of `tys` can hold an object token, as a `const` expression.
fn carries_objects<'a>(tys: impl Iterator<Item = &'a syn::Type>) -> TokenStream {
    let tag = tag();
    let tys = tys.map(|t| quote!(<#t as ::weaveffi::__rt::FfiType<#tag>>::CARRIES_OBJECTS));
    quote!(false #(|| #tys)*)
}

/// `#[weaveffi::record]`.
pub(crate) fn record(attr: TokenStream, item: syn::ItemStruct) -> syn::Result<TokenStream> {
    no_args(&attr, "record")?;
    no_generics(&item.generics)?;
    let prefix = prefix()?;
    let tag = tag();
    let name = item.ident.to_string();
    let ty = &item.ident;
    let fields = named_fields(&item.fields, "a record's")?;
    if fields.is_empty() {
        return Err(syn::Error::new(
            item.ident.span(),
            "weaveffi: a record needs at least one field",
        ));
    }
    let writes = fields.iter().map(|f| {
        let (id, fty) = (f.ident, f.ty);
        quote!(<#fty as ::weaveffi::__rt::FfiType<#tag>>::write(&self.#id, w);)
    });
    let reads = fields.iter().map(|f| {
        let (id, fty) = (f.ident, f.ty);
        quote!(#id: unsafe { <#fty as ::weaveffi::__rt::FfiType<#tag>>::read(r) }?)
    });
    let lit = name_lit(&name);
    let carries = carries_objects(fields.iter().map(|f| f.ty));
    let codec = quote! {
        #[allow(unsafe_code, unused_unsafe)]
        impl ::weaveffi::__rt::Codec<#tag> for #ty {
            const NAME: ::weaveffi::__rt::Name = #lit;
            const CARRIES_OBJECTS: bool = #carries;
            fn encode(&self, w: &mut ::weaveffi::__rt::BufferWriter) {
                #(#writes)*
            }
            unsafe fn decode(
                r: &mut ::weaveffi::__rt::BufferReader<'_>,
            ) -> ::core::result::Result<Self, ::weaveffi::__rt::BufferDecodeError> {
                ::core::result::Result::Ok(Self { #(#reads),* })
            }
        }
        ::weaveffi::__weaveffi_buffered!(#tag, #ty);
    };
    let mut fb = FrameBuilder::new();
    let def = RecordDef {
        name: name.clone(),
        doc: docs(&item.attrs),
        deprecated: deprecated(&item.attrs),
        fields: field_ir(&mut fb, &fields),
    };
    let frame = fb.finish(&prefix, location(item.ident.span()), Item::Record(def));
    let sig = Sig::record(&name, fields.iter().map(|f| field_name(f.ty)).collect());
    let contract = contract(&prefix, &name, sig);
    let cfg = cfgs(&item.attrs);
    let generated = scoped(&cfg, quote!(#codec #frame #contract));
    Ok(quote!(#item #generated))
}

/// The discriminant of each variant: its written `= value`, or one more
/// than the previous variant's (`0` for the first).
fn discriminants(item: &syn::ItemEnum) -> syn::Result<Vec<i32>> {
    let mut next = 0i32;
    item.variants
        .iter()
        .map(|v| {
            let value = match &v.discriminant {
                Some((_, expr)) => int_literal(expr)?,
                None => next,
            };
            next = value.wrapping_add(1);
            Ok(value)
        })
        .collect()
}

fn int_literal(expr: &syn::Expr) -> syn::Result<i32> {
    let err = || {
        syn::Error::new(
            expr.span(),
            "weaveffi: a discriminant must be an integer literal that fits in an i32",
        )
    };
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(i),
            ..
        }) => i.base10_parse::<i32>().map_err(|_| err()),
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => int_literal(expr).map(|v| -v),
        syn::Expr::Group(g) => int_literal(&g.expr),
        _ => Err(err()),
    }
}

/// `#[weaveffi::enumeration]`: a C-style enum, or a rich enum whose
/// variants carry named fields.
pub(crate) fn enumeration(attr: TokenStream, item: syn::ItemEnum) -> syn::Result<TokenStream> {
    no_args(&attr, "enumeration")?;
    no_generics(&item.generics)?;
    let prefix = prefix()?;
    let tag = tag();
    let name = item.ident.to_string();
    let ty = &item.ident;
    let values = discriminants(&item)?;
    let variants: Vec<(&syn::Variant, Vec<NamedField<'_>>)> = item
        .variants
        .iter()
        .map(|v| {
            no_member_cfg(&v.attrs, "an enum variant")?;
            Ok((v, named_fields(&v.fields, "an enum variant's")?))
        })
        .collect::<syn::Result<_>>()?;
    if variants.is_empty() {
        return Err(syn::Error::new(
            item.ident.span(),
            "weaveffi: an enum needs at least one variant",
        ));
    }
    let rich = variants.iter().any(|(_, f)| !f.is_empty());
    let lit = name_lit(&name);
    let impls = if rich {
        let writes = variants.iter().zip(&values).map(|((v, fields), value)| {
            let vid = &v.ident;
            let ids: Vec<_> = fields.iter().map(|f| f.ident).collect();
            let locals = field_locals(ids.len());
            let tys = fields.iter().map(|f| f.ty);
            let pat = if matches!(v.fields, syn::Fields::Unit) {
                quote!(Self::#vid)
            } else {
                quote!(Self::#vid { #(#ids: #locals),* })
            };
            quote! {
                #pat => {
                    w.write_i32(#value);
                    #(<#tys as ::weaveffi::__rt::FfiType<#tag>>::write(#locals, w);)*
                }
            }
        });
        let reads = variants.iter().zip(&values).map(|((v, fields), value)| {
            let vid = &v.ident;
            if matches!(v.fields, syn::Fields::Unit) {
                return quote!(#value => Self::#vid,);
            }
            let reads = fields.iter().map(|f| {
                let (id, fty) = (f.ident, f.ty);
                quote!(#id: unsafe { <#fty as ::weaveffi::__rt::FfiType<#tag>>::read(r) }?)
            });
            quote!(#value => Self::#vid { #(#reads),* },)
        });
        let carries = carries_objects(variants.iter().flat_map(|(_, f)| f.iter().map(|f| f.ty)));
        quote! {
            #[allow(unsafe_code, unused_unsafe)]
            impl ::weaveffi::__rt::Codec<#tag> for #ty {
                const NAME: ::weaveffi::__rt::Name = #lit;
                const CARRIES_OBJECTS: bool = #carries;
                fn encode(&self, w: &mut ::weaveffi::__rt::BufferWriter) {
                    match self {
                        #(#writes)*
                    }
                }
                unsafe fn decode(
                    r: &mut ::weaveffi::__rt::BufferReader<'_>,
                ) -> ::core::result::Result<Self, ::weaveffi::__rt::BufferDecodeError> {
                    ::core::result::Result::Ok(match r.read_i32()? {
                        #(#reads)*
                        _ => {
                            return ::core::result::Result::Err(::weaveffi::__rt::BufferDecodeError {
                                context: "unknown enum tag",
                            })
                        }
                    })
                }
            }
            ::weaveffi::__weaveffi_buffered!(#tag, #ty);
        }
    } else {
        let to = variants.iter().zip(&values).map(|((v, _), value)| {
            let vid = &v.ident;
            quote!(Self::#vid => #value,)
        });
        let from = variants.iter().zip(&values).map(|((v, _), value)| {
            let vid = &v.ident;
            quote!(#value => Self::#vid,)
        });
        quote! {
            impl ::weaveffi::__rt::CEnum<#tag> for #ty {
                const NAME: ::weaveffi::__rt::Name = #lit;
                fn to_i32(&self) -> i32 {
                    match self {
                        #(#to)*
                    }
                }
                fn from_i32(value: i32) -> ::core::option::Option<Self> {
                    ::core::option::Option::Some(match value {
                        #(#from)*
                        _ => return ::core::option::Option::None,
                    })
                }
            }
            ::weaveffi::__weaveffi_c_enum!(#tag, #ty);
        }
    };
    let mut fb = FrameBuilder::new();
    let def = EnumDef {
        name: name.clone(),
        doc: docs(&item.attrs),
        deprecated: deprecated(&item.attrs),
        variants: variants
            .iter()
            .zip(&values)
            .map(|((v, fields), value)| EnumVariant {
                name: v.ident.to_string(),
                value: Some(*value),
                doc: docs(&v.attrs),
                fields: field_ir(&mut fb, fields),
            })
            .collect(),
    };
    let frame = fb.finish(&prefix, location(item.ident.span()), Item::Enum(def));
    let sig = Sig::enumeration(
        &name,
        variants
            .iter()
            .zip(&values)
            .map(|((v, fields), value)| {
                (
                    v.ident.to_string(),
                    *value,
                    fields.iter().map(|f| field_name(f.ty)).collect(),
                )
            })
            .collect(),
    );
    let contract = contract(&prefix, &name, sig);
    let cfg = cfgs(&item.attrs);
    let generated = scoped(&cfg, quote!(#impls #frame #contract));
    Ok(quote!(#item #generated))
}

/// The `#[weaveffi(message = "...")]` template of an error variant.
fn message_template(attrs: &[syn::Attribute]) -> syn::Result<Option<syn::LitStr>> {
    let mut found = None;
    for attr in attrs.iter().filter(|a| is_helper(a)) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("message") {
                found = Some(meta.value()?.parse::<syn::LitStr>()?);
                Ok(())
            } else {
                Err(meta.error("weaveffi: an error variant takes `#[weaveffi(message = \"...\")]`"))
            }
        })?;
    }
    Ok(found)
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

/// `#[weaveffi::error]` (or `#[weaveffi::error(no_display)]`): an error
/// domain.
pub(crate) fn error(attr: TokenStream, mut item: syn::ItemEnum) -> syn::Result<TokenStream> {
    let no_display = match attr.to_string().as_str() {
        "" => false,
        "no_display" => true,
        _ => {
            return Err(syn::Error::new(
                attr.span(),
                "weaveffi: `#[weaveffi::error]` takes nothing or `no_display`",
            ))
        }
    };
    no_generics(&item.generics)?;
    let prefix = prefix()?;
    let tag = tag();
    let name = item.ident.to_string();
    let ty = item.ident.clone();
    let mut codes = Vec::new();
    for v in &item.variants {
        no_member_cfg(&v.attrs, "an error code")?;
        let Some((_, expr)) = &v.discriminant else {
            return Err(syn::Error::new(
                v.ident.span(),
                "weaveffi: every error code needs an explicit positive value (`NotFound = 1`)",
            ));
        };
        let value = int_literal(expr)?;
        if value <= 0 {
            return Err(syn::Error::new(
                expr.span(),
                "weaveffi: an error code must be positive; 0 means success and negative codes \
                 belong to the runtime",
            ));
        }
        let fields: Vec<(syn::Ident, syn::Type, Vec<syn::Attribute>)> =
            named_fields(&v.fields, "an error code's")?
                .into_iter()
                .map(|f| (f.ident.clone(), f.ty.clone(), f.attrs.to_vec()))
                .collect();
        let template = message_template(&v.attrs)?;
        let default_message = docs(&v.attrs)
            .and_then(|d| d.lines().next().map(str::to_string))
            .unwrap_or_else(|| v.ident.to_string());
        codes.push((v.clone(), value, fields, template, default_message));
    }
    // The helper attributes are this macro's; the enum is re-emitted
    // without them.
    for v in &mut item.variants {
        v.attrs.retain(|a| !is_helper(a));
    }

    let code_arms = codes.iter().map(|(v, value, fields, ..)| {
        let vid = &v.ident;
        if fields.is_empty() && matches!(v.fields, syn::Fields::Unit) {
            quote!(Self::#vid => #value,)
        } else {
            quote!(Self::#vid { .. } => #value,)
        }
    });
    let payload_arms = codes.iter().filter(|(_, _, f, ..)| !f.is_empty()).map(|(v, _, fields, ..)| {
        let vid = &v.ident;
        let ids: Vec<_> = fields.iter().map(|f| &f.0).collect();
        let locals = field_locals(ids.len());
        let tys = fields.iter().map(|f| &f.1);
        quote! {
            Self::#vid { #(#ids: #locals),* } => __wv_err.set_payload(&::weaveffi::__rt::encode(|w| {
                #(<#tys as ::weaveffi::__rt::FfiType<#tag>>::write(#locals, w);)*
            })),
        }
    });
    let read_arms = codes.iter().map(|(v, value, fields, ..)| {
        let vid = &v.ident;
        if matches!(v.fields, syn::Fields::Unit) {
            return quote!(#value => Self::#vid,);
        }
        let reads = fields.iter().map(|(id, fty, _)| {
            quote!(#id: unsafe { <#fty as ::weaveffi::__rt::FfiType<#tag>>::read(&mut __wv_r) }.ok()?)
        });
        quote!(#value => Self::#vid { #(#reads),* },)
    });
    let lit = name_lit(&name);
    // The message is the domain's `Display`: spanned at the type, a missing
    // one (with `no_display`) is reported there.
    let to_string = quote_spanned!(ty.span()=> <#ty as ::std::string::ToString>::to_string);
    let no_objects = codes.iter().flat_map(|(v, _, fields, ..)| {
        let (name, tag) = (&name, &tag);
        fields.iter().map(move |(id, fty, _)| {
            let message = format!(
                "weaveffi: field `{id}` of error code `{name}::{}` can carry an object, which \
                 an error payload can't: a consumer that doesn't know the code discards the \
                 payload undecoded, so the object would never be released",
                v.ident
            );
            quote_spanned! {fty.span()=>
                const _: () = ::core::assert!(
                    !<#fty as ::weaveffi::__rt::FfiType<#tag>>::CARRIES_OBJECTS,
                    #message
                );
            }
        })
    });
    let domain_impl = quote_spanned! {ty.span()=>
        #[allow(unsafe_code, unused_unsafe, unreachable_patterns)]
        impl ::weaveffi::__rt::ErrorType<#tag> for #ty
    };
    let mut impls = quote! {
        #(#no_objects)*
        #domain_impl {
            const DOMAIN: ::weaveffi::__rt::Name = #lit;
            fn into_ffi(self) -> ::weaveffi::__rt::FfiError {
                let __wv_code = match &self {
                    #(#code_arms)*
                };
                let mut __wv_err = ::weaveffi::__rt::FfiError::new(
                    __wv_code,
                    &#to_string(&self),
                );
                match &self {
                    #(#payload_arms)*
                    _ => {}
                }
                __wv_err
            }
            fn from_code(__wv_code: i32, __wv_payload: &[u8]) -> ::core::option::Option<Self> {
                let mut __wv_r = ::weaveffi::__rt::payload_reader(__wv_payload);
                let __wv_value = match __wv_code {
                    #(#read_arms)*
                    _ => return ::core::option::Option::None,
                };
                __wv_r.expect_end().ok()?;
                ::core::option::Option::Some(__wv_value)
            }
        }
    };
    if !no_display {
        let arms = codes.iter().map(|(v, _, fields, template, default)| {
            let vid = &v.ident;
            let ids: Vec<_> = fields.iter().map(|f| &f.0).collect();
            let locals = field_locals(ids.len());
            let pat = if matches!(v.fields, syn::Fields::Unit) {
                quote!(Self::#vid)
            } else {
                quote!(Self::#vid { #(#ids: #locals),* })
            };
            let body = match template {
                Some(t) => {
                    let named = placeholders(&t.value());
                    let args = ids
                        .iter()
                        .zip(&locals)
                        .filter(|(id, _)| named.contains(&id.to_string()))
                        .map(|(id, local)| quote!(, #id = #local));
                    quote!(::std::write!(__wv_f, #t #(#args)*))
                }
                None => quote!(__wv_f.write_str(#default)),
            };
            quote! {
                #[allow(unused_variables)]
                #pat => #body,
            }
        });
        let error_impl = quote_spanned! {ty.span()=>
            impl ::std::error::Error for #ty {}
        };
        impls.extend(quote! {
            impl ::std::fmt::Display for #ty {
                fn fmt(&self, __wv_f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    match self {
                        #(#arms)*
                    }
                }
            }
            #error_impl
        });
    }

    let mut fb = FrameBuilder::new();
    let def = ErrorDomain {
        name: name.clone(),
        doc: docs(&item.attrs),
        codes: codes
            .iter()
            .map(|(v, value, fields, template, default)| ErrorCode {
                name: v.ident.to_string(),
                code: *value,
                message: template
                    .as_ref()
                    .map_or_else(|| default.clone(), syn::LitStr::value),
                doc: docs(&v.attrs),
                fields: fields
                    .iter()
                    .map(|(id, fty, attrs)| Field {
                        name: id.to_string(),
                        ty: fb.ty(field_name(fty)),
                        doc: docs(attrs),
                    })
                    .collect(),
            })
            .collect(),
    };
    let frame = fb.finish(&prefix, location(item.ident.span()), Item::Error(def));
    let mut contracts = contract(&prefix, &name, Sig::text(format!("errors {name}")));
    for (v, value, fields, ..) in &codes {
        let code = v.ident.to_string();
        let sig = Sig::code(
            &code,
            *value,
            fields.iter().map(|f| field_name(&f.1)).collect(),
        );
        let one = contract(&prefix, &format!("{name}.{code}"), sig);
        // Each contract function lives in its own block.
        contracts.extend(scoped(&[], one));
    }
    let cfg = cfgs(&item.attrs);
    let generated = scoped(&cfg, quote!(#impls #frame #contracts));
    Ok(quote!(#item #generated))
}

/// `#[weaveffi::interface]`: a reference-counted object type.
pub(crate) fn interface(attr: TokenStream, item: syn::ItemStruct) -> syn::Result<TokenStream> {
    no_args(&attr, "interface")?;
    no_generics(&item.generics)?;
    let prefix = prefix()?;
    let tag = tag();
    let name = item.ident.to_string();
    let ty = &item.ident;
    let clone = format!("{prefix}_{name}_clone");
    let destroy = format!("{prefix}_{name}_destroy");
    let assert = quote_spanned! {ty.span()=>
        const _: fn() = || {
            fn shared<T: ::core::marker::Send + ::core::marker::Sync + 'static>() {}
            shared::<#ty>();
        };
    };
    let impls = quote! {
        #assert
        ::weaveffi::__weaveffi_interface!(#tag, #ty, #name);

        #[doc(hidden)]
        #[allow(unsafe_code, clippy::missing_safety_doc)]
        #[unsafe(export_name = #clone)]
        pub unsafe extern "C" fn __weaveffi_clone(ptr: *const #ty) -> *const #ty {
            unsafe { ::weaveffi::__rt::object_clone(ptr) }.cast_const()
        }

        #[doc(hidden)]
        #[allow(unsafe_code, clippy::missing_safety_doc)]
        #[unsafe(export_name = #destroy)]
        pub unsafe extern "C" fn __weaveffi_destroy(ptr: *mut #ty) {
            unsafe { ::weaveffi::__rt::object_destroy(ptr) }
        }
    };
    let fb = FrameBuilder::new();
    let def = InterfaceDef {
        name: name.clone(),
        doc: docs(&item.attrs),
        deprecated: deprecated(&item.attrs),
        constructors: Vec::new(),
        methods: Vec::new(),
        statics: Vec::new(),
    };
    let frame = fb.finish(&prefix, location(item.ident.span()), Item::Interface(def));
    let contract = contract(&prefix, &name, Sig::text(format!("interface {name}")));
    let cfg = cfgs(&item.attrs);
    let generated = scoped(&cfg, quote!(#impls #frame #contract));
    Ok(quote!(#item #generated))
}

/// The arguments of `#[weaveffi::custom(repr = R, lift = f, lower = g)]`.
struct CustomArgs {
    repr: syn::Type,
    lift: syn::Expr,
    lower: syn::Expr,
}

impl syn::parse::Parse for CustomArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let (mut repr, mut lift, mut lower) = (None, None, None);
        let args =
            syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated(
                input,
            )?;
        for arg in args {
            let key = arg
                .path
                .get_ident()
                .map(ToString::to_string)
                .unwrap_or_default();
            match key.as_str() {
                "repr" => {
                    let tokens = quote!(#arg).to_string();
                    let value = arg.value;
                    repr = Some(syn::parse2(quote!(#value)).map_err(|_| {
                        syn::Error::new(
                            arg.path.span(),
                            format!("weaveffi: `{tokens}` is not a type"),
                        )
                    })?);
                }
                "lift" => lift = Some(arg.value),
                "lower" => lower = Some(arg.value),
                _ => {
                    return Err(syn::Error::new(
                        arg.path.span(),
                        "weaveffi: `#[weaveffi::custom]` takes `repr`, `lift`, and `lower`",
                    ))
                }
            }
        }
        let missing = |what| {
            syn::Error::new(
                input.span(),
                format!(
                    "weaveffi: `#[weaveffi::custom]` needs `{what}`: \
                     `#[weaveffi::custom(repr = R, lift = f, lower = g)]`"
                ),
            )
        };
        Ok(Self {
            repr: repr.ok_or_else(|| missing("repr"))?,
            lift: lift.ok_or_else(|| missing("lift"))?,
            lower: lower.ok_or_else(|| missing("lower"))?,
        })
    }
}

/// Reject a custom alias of a type that already crosses the ABI (a
/// scalar, `String`, a container, a borrow, ...): its trait impls would
/// conflict with the built-in ones.
fn builtin_alias(t: &syn::ItemType) -> syn::Result<()> {
    const BUILTIN: &[&str] = &[
        "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64", "f32", "f64", "bool", "char",
        "usize", "isize", "String", "Vec", "Option", "HashMap", "BTreeMap", "Arc", "Box",
    ];
    let target = crate::shape::ungroup(&t.ty);
    let builtin = match target {
        syn::Type::Path(p) if p.qself.is_none() => p
            .path
            .segments
            .last()
            .is_some_and(|s| BUILTIN.contains(&s.ident.to_string().as_str())),
        syn::Type::Reference(_)
        | syn::Type::Slice(_)
        | syn::Type::Array(_)
        | syn::Type::Tuple(_)
        | syn::Type::Ptr(_) => true,
        _ => false,
    };
    if !builtin {
        return Ok(());
    }
    let (name, target) = (&t.ident, quote!(#target).to_string());
    Err(syn::Error::new(
        t.ty.span(),
        format!(
            "weaveffi: `{target}` already crosses the ABI, so an alias of it can't be a custom \
             type; wrap it in a newtype (`pub struct {name}(pub {target});`) and put \
             `#[weaveffi::custom]` on that"
        ),
    ))
}

/// `#[weaveffi::custom(repr = R, lift = f, lower = g)]` on a type alias or
/// a struct: the type crosses the ABI as `R`.
pub(crate) fn custom(attr: TokenStream, item: syn::Item) -> syn::Result<TokenStream> {
    let args: CustomArgs = syn::parse2(attr)?;
    let (ty, attrs) = match &item {
        syn::Item::Type(t) => {
            builtin_alias(t)?;
            (&t.ident, &t.attrs)
        }
        syn::Item::Struct(s) => (&s.ident, &s.attrs),
        other => {
            return Err(syn::Error::new(
                other.span(),
                "weaveffi: `#[weaveffi::custom]` goes on a type alias or a struct",
            ))
        }
    };
    let tag = tag();
    let (repr, lift, lower) = (&args.repr, &args.lift, &args.lower);
    let cfg = cfgs(attrs);
    let generated = scoped(
        &cfg,
        quote!(::weaveffi::__weaveffi_custom!(#tag, #ty, #repr, #lift, #lower);),
    );
    Ok(quote!(#item #generated))
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
