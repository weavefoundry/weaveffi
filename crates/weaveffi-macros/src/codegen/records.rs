//! Thunk emission for records: the create/destroy/getter surface and the
//! optional fluent builder.

use std::collections::HashMap;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;
use weaveffi_core::abi::CType;
use weaveffi_core::model::{BuilderBinding, FieldBinding, StructBinding};
use weaveffi_ir::ir::TypeRef;

use super::helpers::{ident, is_copy, ret_arrow, rust_type_ident, sentinel, slot_tokens};
use super::marshal::{field_as_param, lift_param, lower_value, seq_lift_expr};
use super::unsupported;

/// Generate the create/destroy/getter surface for one record (plus a fluent
/// builder when the record opted in with `#[weaveffi::builder]`).
pub(crate) fn gen_record(
    s: &StructBinding,
    item: Option<&syn::ItemStruct>,
) -> syn::Result<TokenStream> {
    let rust_ty = rust_type_ident(&s.name);

    // create: lift each field, build the struct, box it. The return slot's
    // CType is a `Named` tag (`{path}_{Name}`), so the Rust return type is
    // spelled directly from the producer's real struct type instead.
    let create_sym = ident(&s.create.symbol);
    let create_params: Vec<TokenStream> = s.create.params.iter().map(slot_tokens).collect();
    let create_sentinel = quote!(::std::ptr::null_mut());

    let mut create_pre = TokenStream::new();
    let mut field_inits: Vec<TokenStream> = Vec::new();
    for field in &s.fields {
        let pb = field_as_param(field);
        let (pre, _) = lift_param(&pb, false, &create_sentinel)?;
        create_pre.extend(pre);
        let fname = ident(&field.name);
        field_inits.push(quote!(#fname: #fname));
    }

    let create = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #create_sym(#(#create_params),*) -> *mut #rust_ty {
            #create_pre
            let __wv_obj = #rust_ty { #(#field_inits),* };
            ::weaveffi::abi::error_set_ok(out_err);
            ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_obj))
        }
    };

    // destroy: the single release call the consumer owes for every owned
    // record pointer (`ReturnFree::OwnedObject`/`ElemFree::Object` in
    // `weaveffi_core::plan`).
    let destroy_sym = ident(&s.destroy_symbol);
    let destroy = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #destroy_sym(ptr: *mut #rust_ty) {
            if ptr.is_null() {
                return;
            }
            unsafe { drop(::std::boxed::Box::from_raw(ptr)) };
        }
    };

    // getters.
    let mut getters = TokenStream::new();
    for field in &s.fields {
        getters.extend(gen_getter(&rust_ty, field)?);
    }

    let builder = match &s.builder {
        Some(b) => {
            let item = item.ok_or_else(|| {
                syn::Error::new(
                    Span::call_site(),
                    format!("internal error: no source struct for builder `{}`", s.name),
                )
            })?;
            gen_builder(s, b, &rust_ty, item)?
        }
        None => TokenStream::new(),
    };

    Ok(quote! {
        #create
        #destroy
        #getters
        #builder
    })
}

/// True when a record field's IR type is optional, so its builder slot may be
/// left unset (defaulting to `None`) rather than erroring at `build`.
fn is_optional(ty: &TypeRef) -> bool {
    matches!(ty, TypeRef::Optional(_))
}

/// Map a field name to its source Rust type, read from the producer's real
/// struct so the builder stores each field with its exact type (including the
/// concrete map flavor `HashMap`/`BTreeMap`, which the IR does not record).
fn struct_field_types(item: &syn::ItemStruct) -> HashMap<String, syn::Type> {
    let mut out = HashMap::new();
    if let syn::Fields::Named(named) = &item.fields {
        for f in &named.named {
            if let Some(id) = &f.ident {
                out.insert(id.to_string(), f.ty.clone());
            }
        }
    }
    out
}

/// Generate the fluent builder surface for one record: an internal builder
/// object holding each field as `Option<FieldType>`, a `_new`/`_destroy` pair,
/// one best-effort setter per field, and a `_build` that materializes the
/// record (erroring through `out_err` when a required field was never set).
fn gen_builder(
    s: &StructBinding,
    b: &BuilderBinding,
    rust_ty: &Ident,
    item: &syn::ItemStruct,
) -> syn::Result<TokenStream> {
    let builder_ty = ident(&format!("__Weaveffi{}Builder", s.name));
    let field_types = struct_field_types(item);

    // The builder object: one `Option<FieldType>` slot per field.
    let mut builder_fields: Vec<TokenStream> = Vec::new();
    let mut new_inits: Vec<TokenStream> = Vec::new();
    for field in &s.fields {
        let fname = ident(&field.name);
        let fty = field_types.get(&field.name).ok_or_else(|| {
            syn::Error::new(
                Span::call_site(),
                format!(
                    "internal error: builder field `{}` not found in struct",
                    field.name
                ),
            )
        })?;
        builder_fields.push(quote!(#fname: ::std::option::Option<#fty>));
        new_inits.push(quote!(#fname: ::std::option::Option::None));
    }

    let new_sym = ident(&b.new_symbol);
    let build_sym = ident(&b.build_symbol);
    let destroy_sym = ident(&b.destroy_symbol);

    // Setters: one per field, keyed to the model's `(field, symbol)` pairs.
    let mut setters = TokenStream::new();
    for (field_name, setter_symbol) in &b.setters {
        let field = s
            .fields
            .iter()
            .find(|f| &f.name == field_name)
            .expect("builder setter references a known field");
        setters.extend(gen_setter(&builder_ty, field, setter_symbol)?);
    }

    // build: take each slot, erroring when a required field is missing.
    let mut takes: Vec<TokenStream> = Vec::new();
    let mut field_inits: Vec<TokenStream> = Vec::new();
    for field in &s.fields {
        let fname = ident(&field.name);
        let missing = if is_optional(&field.ty) {
            quote!(::std::option::Option::None)
        } else {
            let msg = format!("{}.{} is required", s.name, field.name);
            quote! {{
                ::weaveffi::abi::error_set(out_err, -1, #msg);
                return ::std::ptr::null_mut();
            }}
        };
        takes.push(quote! {
            let #fname = match b.#fname.take() {
                ::std::option::Option::Some(__v) => __v,
                ::std::option::Option::None => #missing,
            };
        });
        field_inits.push(quote!(#fname: #fname));
    }

    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub struct #builder_ty {
            #(#builder_fields),*
        }

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #new_sym() -> *mut #builder_ty {
            ::std::boxed::Box::into_raw(::std::boxed::Box::new(#builder_ty {
                #(#new_inits),*
            }))
        }

        #setters

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #build_sym(
            builder: *mut #builder_ty,
            out_err: *mut ::weaveffi::abi::weaveffi_error,
        ) -> *mut #rust_ty {
            if builder.is_null() {
                ::weaveffi::abi::error_set(out_err, -1, "builder is null");
                return ::std::ptr::null_mut();
            }
            let b = unsafe { &mut *builder };
            #(#takes)*
            ::weaveffi::abi::error_set_ok(out_err);
            ::std::boxed::Box::into_raw(::std::boxed::Box::new(#rust_ty { #(#field_inits),* }))
        }

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #destroy_sym(builder: *mut #builder_ty) {
            if !builder.is_null() {
                unsafe { drop(::std::boxed::Box::from_raw(builder)) };
            }
        }
    })
}

/// Generate one builder setter. Setters carry no `out_err`, so the lift is
/// best-effort: a malformed input leaves the slot unset (defaulting at `build`)
/// rather than reporting an error mid-chain, mirroring hand-written builders.
fn gen_setter(
    builder_ty: &Ident,
    field: &FieldBinding,
    setter_symbol: &str,
) -> syn::Result<TokenStream> {
    let sym = ident(setter_symbol);
    let slots: Vec<TokenStream> = field.value_params.iter().map(slot_tokens).collect();
    let assign = setter_assign(field)?;
    Ok(quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #sym(builder: *mut #builder_ty, #(#slots),*) {
            if builder.is_null() {
                return;
            }
            let b = unsafe { &mut *builder };
            #assign
        }
    })
}

/// The body of a builder setter: store the lifted field value into `b.#field`.
/// The slot names match the field's `value_params` (the same names the record's
/// `create` uses), so the marshalling mirrors `lift_param` without `out_err`.
fn setter_assign(field: &FieldBinding) -> syn::Result<TokenStream> {
    let fname = ident(&field.name);
    let slot = fname.clone();
    Ok(match &field.ty {
        ty if is_copy(ty) && !matches!(ty, TypeRef::Enum(_)) => {
            quote!(b.#fname = ::std::option::Option::Some(#slot);)
        }
        TypeRef::Enum(en) => {
            let et = rust_type_ident(en);
            quote! {
                if let ::std::option::Option::Some(__v) = #et::__weaveffi_from_i32(#slot) {
                    b.#fname = ::std::option::Option::Some(__v);
                }
            }
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => quote! {
            b.#fname = ::std::option::Option::Some(
                ::weaveffi::abi::c_ptr_to_string(#slot).unwrap_or_default(),
            );
        },
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            quote!(b.#fname = ::std::option::Option::Some(::weaveffi::abi::lift_opt_string(#slot));)
        }
        TypeRef::Optional(inner) if is_copy(inner) => {
            quote!(b.#fname = ::std::option::Option::Some(unsafe { ::weaveffi::abi::lift_opt_scalar(#slot) });)
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::Record(_) | TypeRef::RichEnum(_)) =>
        {
            quote! {
                b.#fname = ::std::option::Option::Some(if #slot.is_null() {
                    ::std::option::Option::None
                } else {
                    ::std::option::Option::Some(unsafe { &*#slot }.clone())
                });
            }
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            let ptr = ident(&format!("{}_ptr", field.name));
            let len = ident(&format!("{}_len", field.name));
            quote!(b.#fname = ::std::option::Option::Some(unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) });)
        }
        TypeRef::List(inner) => {
            let len = ident(&format!("{}_len", field.name));
            let lift = match &**inner {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    quote!(unsafe { ::weaveffi::abi::lift_string_vec(#slot, #len) })
                }
                t if is_copy(t) => {
                    quote!(unsafe { ::weaveffi::abi::lift_scalar_vec(#slot, #len) })
                }
                TypeRef::Record(st) | TypeRef::RichEnum(st) => {
                    let elem_ty = rust_type_ident(st);
                    quote!(unsafe { ::weaveffi::abi::lift_ptr_vec(#slot as *const *const #elem_ty, #len) })
                }
                _ => return Err(unsupported(&field.name, "builder list element type")),
            };
            quote!(b.#fname = ::std::option::Option::Some(#lift);)
        }
        TypeRef::Map(k, v) => {
            let keys = ident(&format!("{}_keys", field.name));
            let values = ident(&format!("{}_values", field.name));
            let len = ident(&format!("{}_len", field.name));
            let kl = seq_lift_expr(k, quote!(#keys), quote!(#len))
                .ok_or_else(|| unsupported(&field.name, "builder map key type"))?;
            let vl = seq_lift_expr(v, quote!(#values), quote!(#len))
                .ok_or_else(|| unsupported(&field.name, "builder map value type"))?;
            quote! {
                b.#fname = ::std::option::Option::Some({
                    let __wv_keys = #kl;
                    let __wv_vals = #vl;
                    __wv_keys.into_iter().zip(__wv_vals).collect()
                });
            }
        }
        TypeRef::Record(_) | TypeRef::RichEnum(_) => quote! {
            if !#slot.is_null() {
                b.#fname = ::std::option::Option::Some(unsafe { &*#slot }.clone());
            }
        },
        TypeRef::TypedHandle(_) => quote!(b.#fname = ::std::option::Option::Some(#slot);),
        _ => return Err(unsupported(&field.name, "builder field type")),
    })
}

/// Generate one field getter: `{tag}_get_{field}(ptr, [out_len]) -> ...`.
pub(crate) fn gen_getter(rust_ty: &Ident, field: &FieldBinding) -> syn::Result<TokenStream> {
    let sym = ident(&field.getter_symbol);
    let fname = ident(&field.name);
    let mut params: Vec<TokenStream> = vec![quote!(ptr: *const #rust_ty)];
    params.extend(field.getter_out_params.iter().map(slot_tokens));
    let arrow = ret_arrow(&field.getter_ret);
    let sent = sentinel(&field.getter_ret);

    // Read the field by copy (scalars/enums) or clone (owned data).
    let read = if is_copy(&field.ty) {
        quote!(__wv_obj.#fname)
    } else {
        quote!(__wv_obj.#fname.clone())
    };
    let lowered = lower_value(&field.ty, read)?;

    let guard = if matches!(field.getter_ret, CType::Void) {
        quote!(return;)
    } else {
        quote!(return #sent;)
    };

    Ok(quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #sym(#(#params),*) #arrow {
            if ptr.is_null() {
                #guard
            }
            let __wv_obj = unsafe { &*ptr };
            #lowered
        }
    })
}
