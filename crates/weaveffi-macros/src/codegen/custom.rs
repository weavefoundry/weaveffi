//! Custom types: the hidden `weaveffi::abi::Custom` impl each
//! `#[weaveffi::custom]` alias gets, and the converters that turn a value
//! whose type mentions custom types into its repr and back.
//!
//! A custom type crosses as its repr, so the thunks lift and lower the repr
//! with the ordinary per-family machinery and convert at the boundary. A
//! converter follows the written type's shape (`Option`, `Vec` or a slice,
//! `HashMap` or `BTreeMap`) down to the custom types inside it, so
//! `Vec<Option<Id>>` converts element by element.

use proc_macro2::TokenStream;
use quote::{format_ident, quote, ToTokens};

use crate::extract::{CustomDef, Customs};

use super::helpers::ident;

/// The name of the hidden marker type implementing `Custom` for the custom
/// type `name`.
fn marker_name(name: &str) -> syn::Ident {
    ident(&format!("__WvCustom_{name}"))
}

/// The hidden `Custom` impl for one custom type, emitted in the module that
/// declares it.
pub(crate) fn gen_custom(def: &CustomDef) -> TokenStream {
    let marker = marker_name(&def.name);
    let name = ident(&def.name);
    let (repr, lift, lower) = (&def.repr, &def.lift, &def.lower);
    quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub struct #marker;

        impl ::weaveffi::abi::Custom for #marker {
            type Repr = #repr;
            type Value = #name;
            fn lift(
                __wv_repr: Self::Repr,
            ) -> ::std::result::Result<Self::Value, ::std::string::String> {
                (#lift)(__wv_repr).map_err(|__wv_e| ::std::string::ToString::to_string(&__wv_e))
            }
            fn lower(__wv_value: &Self::Value) -> Self::Repr {
                (#lower)(__wv_value)
            }
        }
    }
}

/// The tree's custom types as seen from one module, which knows the
/// relative path to each one's marker type.
#[derive(Clone, Copy)]
pub(crate) struct CustomScope<'a> {
    customs: &'a Customs,
    here: &'a [String],
}

impl<'a> CustomScope<'a> {
    pub(crate) fn new(customs: &'a Customs, here: &'a [String]) -> Self {
        Self { customs, here }
    }

    /// The path from this module to the marker type of `def`.
    fn marker_path(&self, def: &CustomDef) -> TokenStream {
        let common = self
            .here
            .iter()
            .zip(&def.module)
            .take_while(|(a, b)| a == b)
            .count();
        let ups = std::iter::repeat_n(quote!(super::), self.here.len() - common);
        let downs = def.module[common..].iter().map(|m| {
            let m = ident(m);
            quote!(#m::)
        });
        let marker = marker_name(&def.name);
        quote!(#(#ups)* #(#downs)* #marker)
    }

    /// The shape of `ty` when it mentions a custom type, else `None`.
    pub(crate) fn shape(&self, ty: &syn::Type) -> Option<Shape> {
        if self.customs.is_empty() {
            return None;
        }
        let shape = self.shape_of(ty);
        shape.has_custom().then_some(shape)
    }

    fn shape_of(&self, ty: &syn::Type) -> Shape {
        let plain = || Shape::Plain(ty.to_token_stream());
        match ty {
            syn::Type::Reference(r) => match r.elem.as_ref() {
                syn::Type::Slice(s) => Shape::Vec(Box::new(self.shape_of(&s.elem))),
                other => self.shape_of(other),
            },
            syn::Type::Paren(p) => self.shape_of(&p.elem),
            syn::Type::Path(p) if p.qself.is_none() => {
                let Some(seg) = p.path.segments.last() else {
                    return plain();
                };
                let name = seg.ident.to_string();
                if let (Some(def), syn::PathArguments::None) =
                    (self.customs.get(&name), &seg.arguments)
                {
                    return Shape::Custom(self.marker_path(def));
                }
                let args: Vec<&syn::Type> = match &seg.arguments {
                    syn::PathArguments::AngleBracketed(a) => a
                        .args
                        .iter()
                        .filter_map(|g| match g {
                            syn::GenericArgument::Type(t) => Some(t),
                            _ => None,
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                match (name.as_str(), args.as_slice()) {
                    ("Option", [inner]) => Shape::Option(Box::new(self.shape_of(inner))),
                    ("Vec", [inner]) => Shape::Vec(Box::new(self.shape_of(inner))),
                    ("HashMap" | "BTreeMap", [k, v]) => {
                        let mut path = p.path.clone();
                        if let Some(last) = path.segments.last_mut() {
                            last.arguments = syn::PathArguments::None;
                        }
                        Shape::Map(
                            path.to_token_stream(),
                            Box::new(self.shape_of(k)),
                            Box::new(self.shape_of(v)),
                        )
                    }
                    _ => plain(),
                }
            }
            _ => plain(),
        }
    }
}

/// How a written type maps onto its repr: identity, a custom type, or a
/// container of shapes.
#[derive(Clone)]
pub(crate) enum Shape {
    /// No custom type inside: the type as written.
    Plain(TokenStream),
    /// A custom type, by the path to its marker type.
    Custom(TokenStream),
    /// `Option<T>`.
    Option(Box<Shape>),
    /// `Vec<T>` or `&[T]`.
    Vec(Box<Shape>),
    /// A map, by its path without generics.
    Map(TokenStream, Box<Shape>, Box<Shape>),
}

/// How a custom type's `lift` failure is reported where a converter runs.
#[derive(Clone, Copy)]
pub(crate) enum LiftSite<'a> {
    /// A parameter (or async parameter): a marshalling `FfiError` naming
    /// it.
    Param(&'a syn::LitStr),
    /// A field read out of a value buffer: a `BufferDecodeError`.
    Buffer,
    /// A callback method's return: a marshalling `ForeignError`.
    Returned,
}

impl Shape {
    fn has_custom(&self) -> bool {
        match self {
            Shape::Plain(_) => false,
            Shape::Custom(_) => true,
            Shape::Option(s) | Shape::Vec(s) => s.has_custom(),
            Shape::Map(_, k, v) => k.has_custom() || v.has_custom(),
        }
    }

    /// The owned repr type (every custom type replaced by its repr, every
    /// slice by a `Vec`).
    pub(crate) fn repr_ty(&self) -> TokenStream {
        match self {
            Shape::Plain(t) => t.clone(),
            Shape::Custom(m) => quote!(<#m as ::weaveffi::abi::Custom>::Repr),
            Shape::Option(s) => {
                let inner = s.repr_ty();
                quote!(::std::option::Option<#inner>)
            }
            Shape::Vec(s) => {
                let inner = s.repr_ty();
                quote!(::std::vec::Vec<#inner>)
            }
            Shape::Map(path, k, v) => {
                let (k, v) = (k.repr_ty(), v.repr_ty());
                quote!(#path<#k, #v>)
            }
        }
    }

    /// An expression converting the owned repr `value` into the producer's
    /// type, as a `Result` whose error `site` decides.
    pub(crate) fn lift(&self, value: TokenStream, site: LiftSite<'_>) -> TokenStream {
        self.lift_at(value, site, 0)
    }

    fn lift_at(&self, value: TokenStream, site: LiftSite<'_>, depth: usize) -> TokenStream {
        let x = format_ident!("__wv_x{}", depth);
        match self {
            Shape::Plain(_) => quote!(::std::result::Result::Ok(#value)),
            Shape::Custom(m) => match site {
                LiftSite::Param(lit) => {
                    quote!(::weaveffi::abi::lift_custom_param::<#m>(#value, #lit))
                }
                LiftSite::Buffer => quote!(::weaveffi::abi::lift_custom_buffered::<#m>(#value)),
                LiftSite::Returned => {
                    quote!(::weaveffi::abi::lift_custom_returned::<#m>(#value))
                }
            },
            Shape::Option(s) => {
                let inner = s.lift_at(quote!(#x), site, depth + 1);
                quote! {
                    match #value {
                        ::std::option::Option::Some(#x) => {
                            (#inner).map(::std::option::Option::Some)
                        }
                        ::std::option::Option::None => {
                            ::std::result::Result::Ok(::std::option::Option::None)
                        }
                    }
                }
            }
            Shape::Vec(s) => {
                let inner = s.lift_at(quote!(#x), site, depth + 1);
                quote! {
                    ::std::iter::IntoIterator::into_iter(#value)
                        .map(|#x| #inner)
                        .collect::<::std::result::Result<::std::vec::Vec<_>, _>>()
                }
            }
            Shape::Map(path, k, v) => {
                let (kx, vx) = (
                    format_ident!("__wv_k{}", depth),
                    format_ident!("__wv_v{}", depth),
                );
                let (kl, vl) = (
                    format_ident!("__wv_kl{}", depth),
                    format_ident!("__wv_vl{}", depth),
                );
                let key = k.lift_at(quote!(#kx), site, depth + 1);
                let val = v.lift_at(quote!(#vx), site, depth + 1);
                quote! {
                    ::std::iter::IntoIterator::into_iter(#value)
                        .map(|(#kx, #vx)| {
                            (#key).and_then(|#kl| (#val).map(|#vl| (#kl, #vl)))
                        })
                        .collect::<::std::result::Result<#path<_, _>, _>>()
                }
            }
        }
    }

    /// An expression converting `value`, a reference to the producer's
    /// type, into the owned repr.
    pub(crate) fn lower(&self, value: TokenStream) -> TokenStream {
        self.lower_at(value, 0)
    }

    fn lower_at(&self, value: TokenStream, depth: usize) -> TokenStream {
        let x = format_ident!("__wv_x{}", depth);
        match self {
            Shape::Plain(_) => quote!(::std::clone::Clone::clone(#value)),
            Shape::Custom(m) => quote!(<#m as ::weaveffi::abi::Custom>::lower(#value)),
            Shape::Option(s) => {
                let inner = s.lower_at(quote!(#x), depth + 1);
                quote!(::std::option::Option::as_ref(#value).map(|#x| #inner))
            }
            Shape::Vec(s) => {
                let inner = s.lower_at(quote!(#x), depth + 1);
                quote! {
                    (#value).iter().map(|#x| #inner).collect::<::std::vec::Vec<_>>()
                }
            }
            Shape::Map(path, k, v) => {
                let (kx, vx) = (
                    format_ident!("__wv_k{}", depth),
                    format_ident!("__wv_v{}", depth),
                );
                let key = k.lower_at(quote!(#kx), depth + 1);
                let val = v.lower_at(quote!(#vx), depth + 1);
                quote! {
                    (#value).iter().map(|(#kx, #vx)| (#key, #val)).collect::<#path<_, _>>()
                }
            }
        }
    }
}
