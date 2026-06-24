//! Lower a `#[weaveffi::module]` to its IR and emit the C ABI thunks.
//!
//! The flow mirrors the rest of WeaveFFI: parse the annotated module to the IR
//! via [`weaveffi_bridge`], validate/resolve it, build the canonical
//! [`BindingModel`], then render each lowered symbol. Signatures come straight
//! from the model (so they match the generated header by construction); only
//! the body marshalling - which lifts each ABI slot into a Rust value, calls
//! the user's safe function, and lowers the result - is new here, and every
//! `unsafe` operation in it bottoms out in a `weaveffi-abi` helper.

use std::collections::HashMap;

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::Ident;
use weaveffi_core::abi::{AbiParam, CType, ConstPos};
use weaveffi_core::model::{
    AbiFn, AsyncBinding, BindingModel, BuilderBinding, CallShape, CallbackBinding, EnumBinding,
    FieldBinding, FnBinding, IteratorBinding, ListenerBinding, ModuleBinding, ParamBinding,
    RichEnumBinding, RichVariantBinding, StructBinding,
};
use weaveffi_ir::ir::{Api, TypeRef, CURRENT_SCHEMA_VERSION};

/// The single global ABI prefix; matches the CLI's default so the macro and
/// `weaveffi generate` emit identical symbols.
const PREFIX: &str = "weaveffi";

/// Make a call-site identifier from a string.
fn ident(name: &str) -> Ident {
    Ident::new(name, Span::call_site())
}

/// Expand a `#[weaveffi::module]` into the original module plus generated
/// thunks.
pub fn expand_module(item_mod: &syn::ItemMod) -> syn::Result<TokenStream> {
    if item_mod.content.is_none() {
        return Err(syn::Error::new_spanned(
            item_mod,
            "#[weaveffi::module] requires an inline module body (`mod foo { ... }`)",
        ));
    }

    // 1. Lower to IR through the shared bridge, then resolve type references so
    //    C-style enum references are distinguished from struct references and
    //    cross-module references are qualified. The bridge recurses into nested
    //    `#[weaveffi::module]` submodules, so this single lowering covers the
    //    whole module tree.
    //
    //    We run `resolve_type_refs` rather than the full `validate_api`: a
    //    `#[weaveffi::module]` is expanded in isolation, so a reference to a type
    //    declared in a *sibling* top-level module (e.g. `orders` using a
    //    `products::Product`) must not be rejected as an unknown type here. A
    //    cross-module struct still crosses the ABI as an opaque pointer, and the
    //    emitted thunk names the producer's real Rust type, so the symbol and
    //    calling convention match the header regardless. Whole-API rule checks
    //    remain enforced by the CLI's `validate`/`extract`/`generate`, and the
    //    Rust compiler rejects any genuinely undefined type in the thunks.
    let module_ir = weaveffi_bridge::module_from_item_mod(item_mod)?;
    let mut api = Api {
        version: CURRENT_SCHEMA_VERSION.to_string(),
        package: None,
        modules: vec![module_ir],
        generators: None,
    };
    weaveffi_core::validate::resolve_type_refs(&mut api);

    // 2. Build the canonical lowered model. Nested modules are flattened into
    //    one binding each, keyed here by their path segments (e.g. `["kv",
    //    "stats"]`) so the recursive rebuild can match each `mod` to its
    //    binding and emit symbols under the right `weaveffi_<a>_<b>_` prefix.
    let model = BindingModel::build(&api, PREFIX);
    let by_path: HashMap<Vec<String>, &ModuleBinding> = model
        .modules
        .iter()
        .map(|m| (m.segments.clone(), m))
        .collect();

    // 3. Rebuild the module tree, injecting each module's thunks into its own
    //    body and stripping inner `#[weaveffi::module]` markers so nested
    //    modules expand here (with the correct prefix) instead of re-expanding
    //    standalone under the wrong one.
    rebuild_module(item_mod, &[], &by_path)
}

/// Re-emit `item_mod` with its generated thunks appended, recursing into nested
/// `#[weaveffi::module]` submodules.
fn rebuild_module(
    item_mod: &syn::ItemMod,
    parent_segments: &[String],
    by_path: &HashMap<Vec<String>, &ModuleBinding>,
) -> syn::Result<TokenStream> {
    let Some((_, items)) = &item_mod.content else {
        return Err(syn::Error::new_spanned(
            item_mod,
            "#[weaveffi::module] requires an inline module body (`mod foo { ... }`)",
        ));
    };

    let mut segments = parent_segments.to_vec();
    segments.push(item_mod.ident.to_string());
    let mb = by_path.get(&segments).ok_or_else(|| {
        syn::Error::new_spanned(
            &item_mod.ident,
            "internal error: module has no lowered binding",
        )
    })?;

    let generated = render_symbols(mb, items, &item_mod.ident)?;

    // Pass items through verbatim, except nested `#[weaveffi::module]`s, which
    // we expand inline (recursively) with their marker stripped.
    let mut body = TokenStream::new();
    for item in items {
        if let syn::Item::Mod(child) = item {
            if weaveffi_bridge::has_marker(&child.attrs, "module") && child.content.is_some() {
                body.extend(rebuild_module(child, &segments, by_path)?);
                continue;
            }
        }
        body.extend(quote!(#item));
    }

    let attrs = item_mod
        .attrs
        .iter()
        .filter(|a| !is_module_marker(a))
        .collect::<Vec<_>>();
    let vis = &item_mod.vis;
    let mod_token = &item_mod.mod_token;
    let name = &item_mod.ident;
    Ok(quote! {
        #(#attrs)*
        #vis #mod_token #name {
            #body

            #generated
        }
    })
}

/// Whether `attr` is the `#[weaveffi::module]` marker (matched by final path
/// segment, mirroring the bridge so detection and stripping stay symmetric).
fn is_module_marker(attr: &syn::Attribute) -> bool {
    attr.path()
        .segments
        .last()
        .is_some_and(|s| s.ident == "module")
}

/// Render every C ABI symbol for one lowered module binding.
///
/// `items` are the syn items directly in that module's body; they are indexed
/// so body marshalling can read reference-ness and `Result` returns from the
/// user's signatures, and record / enum codegen can read the producer's real
/// field and variant types. `mod_ident` is used only for error spans.
fn render_symbols(
    mb: &ModuleBinding,
    items: &[syn::Item],
    mod_ident: &syn::Ident,
) -> syn::Result<TokenStream> {
    let mut fns: HashMap<String, &syn::ItemFn> = HashMap::new();
    let mut structs: HashMap<String, &syn::ItemStruct> = HashMap::new();
    let mut enums: HashMap<String, &syn::ItemEnum> = HashMap::new();
    for item in items {
        match item {
            syn::Item::Fn(f) => {
                fns.insert(f.sig.ident.to_string(), f);
            }
            syn::Item::Struct(s) => {
                structs.insert(s.ident.to_string(), s);
            }
            syn::Item::Enum(e) => {
                enums.insert(e.ident.to_string(), e);
            }
            _ => {}
        }
    }

    let mut generated = TokenStream::new();
    for e in &mb.enums {
        generated.extend(gen_enum(e, enums.get(&e.name).copied())?);
    }
    for s in &mb.structs {
        generated.extend(gen_record(s, structs.get(&s.name).copied())?);
    }
    for c in &mb.callbacks {
        generated.extend(gen_callback_type(c)?);
    }
    for l in &mb.listeners {
        generated.extend(gen_listener(mb, l)?);
    }
    for f in &mb.functions {
        let sfn = fns.get(&f.name).ok_or_else(|| {
            syn::Error::new_spanned(
                mod_ident,
                format!(
                    "internal error: no source for exported function `{}`",
                    f.name
                ),
            )
        })?;
        generated.extend(gen_function(f, sfn)?);
    }
    Ok(generated)
}

// ── C type -> Rust FFI type ──────────────────────────────────────────────

/// Render a [`CType`] as the Rust spelling a producer thunk uses.
///
/// This mirrors [`CType::render_rust`] for every slot except opaque object
/// pointers: a struct tag resolves to the producer's *real* Rust type (the
/// `Box`ed object), which is ABI-identical to the header's incomplete tag.
fn ctype_to_rust(ct: &CType) -> TokenStream {
    match ct {
        CType::Int8 => quote!(i8),
        CType::Int16 => quote!(i16),
        CType::Int32 => quote!(i32),
        CType::Int64 => quote!(i64),
        CType::Uint8 => quote!(u8),
        CType::Uint16 => quote!(u16),
        CType::Uint32 => quote!(u32),
        CType::Uint64 => quote!(u64),
        CType::Float => quote!(f32),
        CType::Double => quote!(f64),
        CType::Bool => quote!(bool),
        CType::Size => quote!(usize),
        CType::Char => quote!(::std::os::raw::c_char),
        CType::Void => quote!(::std::ffi::c_void),
        CType::Handle => quote!(u64),
        CType::CancelToken => quote!(::weaveffi::abi::weaveffi_cancel_token),
        CType::Error => quote!(::weaveffi::abi::weaveffi_error),
        CType::Enum { .. } => quote!(i32),
        CType::StructTag { name, .. } => {
            let ty = ident(name);
            quote!(#ty)
        }
        // A generator-named typedef (e.g. an async completion callback). It
        // renders `{prefix}_{core}`, matching `CType::render_rust` so the slot
        // type lines up with the alias the macro emits for it.
        CType::Named(core) => {
            let ty = ident(&format!("{PREFIX}_{core}"));
            quote!(#ty)
        }
        CType::Ptr { konst, pointee } => {
            let inner = ctype_to_rust(pointee);
            match konst {
                ConstPos::None => quote!(*mut #inner),
                ConstPos::West | ConstPos::East => quote!(*const #inner),
            }
        }
    }
}

/// Render one ABI slot as `name: ty`.
fn slot_tokens(p: &AbiParam) -> TokenStream {
    let n = ident(&p.name);
    let t = ctype_to_rust(&p.ty);
    quote!(#n: #t)
}

/// Render the `-> T` return clause for a lowered symbol (empty for `void`).
fn ret_arrow(ret: &CType) -> TokenStream {
    if matches!(ret, CType::Void) {
        TokenStream::new()
    } else {
        let t = ctype_to_rust(ret);
        quote!(-> #t)
    }
}

/// The zero/null value a fallible symbol returns on the error path.
fn sentinel(ret: &CType) -> TokenStream {
    match ret {
        CType::Void => quote!(()),
        CType::Ptr {
            konst: ConstPos::None,
            ..
        } => quote!(::std::ptr::null_mut()),
        CType::Ptr { .. } => quote!(::std::ptr::null()),
        CType::Bool => quote!(false),
        CType::Float | CType::Double => quote!(0.0),
        _ => quote!(0),
    }
}

// ── Functions ────────────────────────────────────────────────────────────

/// True when this enum/struct field/param type crosses the ABI without owning
/// heap data (so a getter can read it by copy rather than clone).
fn is_copy(ty: &TypeRef) -> bool {
    matches!(
        ty,
        TypeRef::I8
            | TypeRef::I16
            | TypeRef::I32
            | TypeRef::I64
            | TypeRef::U8
            | TypeRef::U16
            | TypeRef::U32
            | TypeRef::U64
            | TypeRef::F32
            | TypeRef::F64
            | TypeRef::Bool
            | TypeRef::Handle
            | TypeRef::Enum(_)
    )
}

/// The bare Rust type name of a struct/enum reference (dropping any qualifying
/// module path the resolver added).
fn rust_type_ident(name: &str) -> Ident {
    ident(name.rsplit('.').next().unwrap_or(name))
}

fn returns_result(output: &syn::ReturnType) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    matches!(ty.as_ref(), syn::Type::Path(p)
        if p.path.segments.last().is_some_and(|s| s.ident == "Result"))
}

/// The producer's source type for the parameter named `name`, if present.
fn user_param_type<'a>(sfn: &'a syn::ItemFn, name: &str) -> Option<&'a syn::Type> {
    sfn.sig.inputs.iter().find_map(|arg| {
        let syn::FnArg::Typed(pt) = arg else {
            return None;
        };
        let syn::Pat::Ident(id) = pt.pat.as_ref() else {
            return None;
        };
        (id.ident == name).then(|| pt.ty.as_ref())
    })
}

/// Render one ABI slot, preferring the producer's own type for a typed handle.
///
/// A `handle<T>` (`*mut T`/`*const T`) crosses the thunk untouched, so the slot
/// is spelled exactly as the producer wrote it. That keeps the pointee path the
/// producer chose (importantly `super::T` for a handle owned by a parent
/// module) in scope where the thunk is emitted, which a synthesized bare `T`
/// would not be when the thunk lands inside a nested submodule.
fn slot_tokens_for(p: &AbiParam, f: &FnBinding, sfn: &syn::ItemFn) -> TokenStream {
    if let Some(ty) = typed_handle_user_type(p, f, sfn) {
        let n = ident(&p.name);
        quote!(#n: #ty)
    } else {
        slot_tokens(p)
    }
}

/// Render the slot list for a function signature, honoring producer-written
/// typed-handle types (see [`slot_tokens_for`]).
fn fn_slots(params: &[AbiParam], f: &FnBinding, sfn: &syn::ItemFn) -> Vec<TokenStream> {
    params.iter().map(|p| slot_tokens_for(p, f, sfn)).collect()
}

/// The producer's source type for an ABI slot that lowers a `handle<T>`
/// parameter, or `None` when the slot is not a typed handle.
fn typed_handle_user_type<'a>(
    p: &AbiParam,
    f: &FnBinding,
    sfn: &'a syn::ItemFn,
) -> Option<&'a syn::Type> {
    let pb = f.params.iter().find(|pb| pb.name == p.name)?;
    if !matches!(pb.ty, TypeRef::TypedHandle(_)) {
        return None;
    }
    user_param_type(sfn, &p.name)
}

/// Render the `-> T` return clause, preferring the producer's own type for a
/// typed-handle return (so a parent-module `super::T` stays in scope).
fn ret_arrow_for(ret: &CType, f: &FnBinding, sfn: &syn::ItemFn) -> TokenStream {
    if matches!(f.ret, Some(TypeRef::TypedHandle(_))) {
        if let syn::ReturnType::Type(_, ty) = &sfn.sig.output {
            let ty = weaveffi_bridge::peel_result(ty);
            return quote!(-> #ty);
        }
    }
    ret_arrow(ret)
}

/// Whether a parameter's source type is a shared reference (`&T`), so the call
/// passes a borrow rather than a clone. `&str`/`&[u8]`/`&mut T` are handled by
/// the type-specific lift, not here.
fn param_is_ref(sfn: &syn::ItemFn, name: &str) -> bool {
    sfn.sig.inputs.iter().any(|arg| {
        let syn::FnArg::Typed(pt) = arg else {
            return false;
        };
        let syn::Pat::Ident(id) = pt.pat.as_ref() else {
            return false;
        };
        id.ident == name
            && matches!(pt.ty.as_ref(), syn::Type::Reference(r) if r.mutability.is_none())
    })
}

/// Lift a contiguous foreign array (`ptr` + `len`) of `elem` into an owned
/// `Vec`, reusing the runtime's sequence helpers. Used for the parallel key and
/// value arrays of a map parameter. Returns `None` for an unsupported element.
fn seq_lift_expr(elem: &TypeRef, ptr: TokenStream, len: TokenStream) -> Option<TokenStream> {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            Some(quote!(unsafe { ::weaveffi::abi::lift_string_vec(#ptr, #len) }))
        }
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => {
            Some(quote!(unsafe { ::weaveffi::abi::lift_scalar_vec(#ptr, #len) }))
        }
        _ => None,
    }
}

/// Lower an owned `Vec` of `elem` into a heap array base pointer (no length
/// slot; the map writer carries the shared length). Returns `None` for an
/// unsupported element.
fn seq_lower_expr(elem: &TypeRef, vec: TokenStream) -> Option<TokenStream> {
    match elem {
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            Some(quote!(unsafe { ::weaveffi::abi::lower_string_vec(#vec, ::std::ptr::null_mut()) }))
        }
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => {
            Some(quote!(unsafe { ::weaveffi::abi::lower_scalar_vec(#vec, ::std::ptr::null_mut()) }))
        }
        _ => None,
    }
}

/// Generate the lift preamble and the call-argument expression for one param.
fn lift_param(
    pb: &ParamBinding,
    is_ref: bool,
    sentinel: &TokenStream,
) -> syn::Result<(TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let owned = || quote!(#name);
    let by_ref = || quote!(&#name);
    let msg = format!("{} is null or invalid", pb.name);

    Ok(match &pb.ty {
        ty if is_copy(ty) && !matches!(ty, TypeRef::Enum(_)) => (TokenStream::new(), owned()),
        TypeRef::Enum(enum_name) => {
            let et = rust_type_ident(enum_name);
            let pre = quote! {
                let #name = match #et::__weaveffi_from_i32(#name) {
                    ::std::option::Option::Some(__v) => __v,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(out_err, 1, #msg);
                        return #sentinel;
                    }
                };
            };
            (pre, owned())
        }
        TypeRef::StringUtf8 => {
            let pre = quote! {
                let #name = match ::weaveffi::abi::c_ptr_to_string(#name) {
                    ::std::option::Option::Some(__s) => __s,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(out_err, 1, #msg);
                        return #sentinel;
                    }
                };
            };
            (pre, owned())
        }
        TypeRef::BorrowedStr => {
            let pre = quote! {
                let #name = match ::weaveffi::abi::c_ptr_to_string(#name) {
                    ::std::option::Option::Some(__s) => __s,
                    ::std::option::Option::None => {
                        ::weaveffi::abi::error_set(out_err, 1, #msg);
                        return #sentinel;
                    }
                };
            };
            (pre, by_ref())
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            (
                quote!(let #name = ::weaveffi::abi::lift_opt_string(#name);),
                owned(),
            )
        }
        TypeRef::Optional(inner) if is_copy(inner) => {
            let pre = quote!(let #name = unsafe { ::weaveffi::abi::lift_opt_scalar(#name) };);
            (pre, owned())
        }
        TypeRef::Optional(inner) if matches!(**inner, TypeRef::Struct(_)) => {
            let pre = quote! {
                let #name = if #name.is_null() {
                    ::std::option::Option::None
                } else {
                    ::std::option::Option::Some(unsafe { &*#name }.clone())
                };
            };
            (pre, owned())
        }
        TypeRef::Bytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                owned(),
            )
        }
        TypeRef::BorrowedBytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_byte_slice(#ptr, #len) };),
                owned(),
            )
        }
        TypeRef::List(inner) => {
            let len = ident(&format!("{}_len", pb.name));
            let pre = match &**inner {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_string_vec(#name, #len) };)
                }
                t if is_copy(t) => {
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_scalar_vec(#name, #len) };)
                }
                // A `[Struct]` arrives as an east-const array of object pointers
                // (`const T* const*`), which the model renders to a Rust
                // `*const *mut T`. Cast the inner pointee to `*const` so it
                // matches `lift_ptr_vec`, which clones each element into the
                // owned `Vec<T>` the user's function expects.
                TypeRef::Struct(s) => {
                    let elem_ty = rust_type_ident(s);
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_ptr_vec(#name as *const *const #elem_ty, #len) };)
                }
                _ => {
                    return Err(unsupported(&pb.name, "list element type"));
                }
            };
            (pre, owned())
        }
        TypeRef::Struct(_) => {
            let bind = if pb.mutable {
                quote!(let #name = unsafe { &mut *#name };)
            } else if is_ref {
                quote!(let #name = unsafe { &*#name };)
            } else {
                quote!(let #name = unsafe { &*#name }.clone();)
            };
            let pre = quote! {
                if #name.is_null() {
                    ::weaveffi::abi::error_set(out_err, 1, #msg);
                    return #sentinel;
                }
                #bind
            };
            (pre, owned())
        }
        TypeRef::TypedHandle(_) => (TokenStream::new(), owned()),
        // A map arrives as two parallel arrays (`{name}_keys`, `{name}_values`)
        // and a shared `{name}_len`. Lift each array, then zip into the user's
        // declared map type (the `collect` target is inferred from the call).
        TypeRef::Map(k, v) => {
            let keys = ident(&format!("{}_keys", pb.name));
            let values = ident(&format!("{}_values", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            let kl = seq_lift_expr(k, quote!(#keys), quote!(#len))
                .ok_or_else(|| unsupported(&pb.name, "map key type"))?;
            let vl = seq_lift_expr(v, quote!(#values), quote!(#len))
                .ok_or_else(|| unsupported(&pb.name, "map value type"))?;
            let pre = quote! {
                let #name = {
                    let __wv_keys = #kl;
                    let __wv_vals = #vl;
                    __wv_keys.into_iter().zip(__wv_vals).collect()
                };
            };
            (pre, owned())
        }
        TypeRef::Iterator(_) => return Err(unsupported(&pb.name, "iterator parameter")),
        _ => return Err(unsupported(&pb.name, "parameter type")),
    })
}

/// Lower an owned Rust `value` of IR type `ty` into its C return expression.
/// `out_len` names the trailing length slot for the buffer-returning shapes.
fn lower_value(ty: &TypeRef, value: TokenStream) -> syn::Result<TokenStream> {
    Ok(match ty {
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => value,
        TypeRef::Enum(_) => quote!((#value) as i32),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            quote!(::weaveffi::abi::string_to_c_ptr(&(#value)))
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            quote!(::weaveffi::abi::lower_opt_string(#value))
        }
        TypeRef::Optional(inner) if is_copy(inner) => {
            quote!(::weaveffi::abi::lower_opt_scalar(#value))
        }
        TypeRef::Optional(inner) if matches!(**inner, TypeRef::Struct(_)) => quote! {
            match #value {
                ::std::option::Option::Some(__v) => ::std::boxed::Box::into_raw(::std::boxed::Box::new(__v)),
                ::std::option::Option::None => ::std::ptr::null_mut(),
            }
        },
        TypeRef::Bytes | TypeRef::BorrowedBytes => {
            quote!(unsafe { ::weaveffi::abi::lower_bytes(#value, out_len) })
        }
        TypeRef::Struct(_) => {
            quote!(::std::boxed::Box::into_raw(::std::boxed::Box::new(#value)))
        }
        TypeRef::TypedHandle(_) => value,
        TypeRef::List(inner) => match &**inner {
            TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                quote!(unsafe { ::weaveffi::abi::lower_string_vec(#value, out_len) })
            }
            t if is_copy(t) => {
                quote!(unsafe { ::weaveffi::abi::lower_scalar_vec(#value, out_len) })
            }
            TypeRef::Struct(_) => quote! {
                unsafe {
                    ::weaveffi::abi::lower_ptr_vec(
                        (#value).into_iter()
                            .map(|__e| ::std::boxed::Box::into_raw(::std::boxed::Box::new(__e)))
                            .collect::<::std::vec::Vec<_>>(),
                        out_len,
                    )
                }
            },
            _ => return Err(unsupported("return", "list element type")),
        },
        // A returned map is delivered through the `out_keys`/`out_values`/
        // `out_len` triple: split into parallel owned arrays, lower each, and
        // publish the bases. The C return type is `void`, so this arm evaluates
        // to `()`.
        TypeRef::Map(k, v) => {
            let kl = seq_lower_expr(k, quote!(__wv_ks))
                .ok_or_else(|| unsupported("return", "map key type"))?;
            let vl = seq_lower_expr(v, quote!(__wv_vs))
                .ok_or_else(|| unsupported("return", "map value type"))?;
            quote! {{
                let __wv_map = #value;
                let mut __wv_ks = ::std::vec::Vec::with_capacity(__wv_map.len());
                let mut __wv_vs = ::std::vec::Vec::with_capacity(__wv_map.len());
                for (__wv_k, __wv_v) in __wv_map {
                    __wv_ks.push(__wv_k);
                    __wv_vs.push(__wv_v);
                }
                let __wv_len = __wv_ks.len();
                let __wv_kp = #kl;
                let __wv_vp = #vl;
                unsafe {
                    ::weaveffi::abi::write_map_out(
                        __wv_kp, __wv_vp, __wv_len, out_keys, out_values, out_len,
                    )
                };
            }}
        }
        TypeRef::Iterator(_) => return Err(unsupported("return", "iterator return")),
        _ => return Err(unsupported("return", "return type")),
    })
}

fn unsupported(what: &str, kind: &str) -> syn::Error {
    syn::Error::new(
        Span::call_site(),
        format!("weaveffi: unsupported {kind} for `{what}` (not yet implemented by #[weaveffi::module])"),
    )
}

/// Spell an IR type as the Rust type a producer uses, for the cases the macro
/// needs to name a generic argument (currently the element of `iter<T>`). The
/// map flavor is not recorded by the IR, so it defaults to `HashMap`.
fn typeref_to_rust(ty: &TypeRef) -> syn::Result<TokenStream> {
    Ok(match ty {
        TypeRef::I8 => quote!(i8),
        TypeRef::I16 => quote!(i16),
        TypeRef::I32 => quote!(i32),
        TypeRef::I64 => quote!(i64),
        TypeRef::U8 => quote!(u8),
        TypeRef::U16 => quote!(u16),
        TypeRef::U32 => quote!(u32),
        TypeRef::U64 => quote!(u64),
        TypeRef::F32 => quote!(f32),
        TypeRef::F64 => quote!(f64),
        TypeRef::Bool => quote!(bool),
        TypeRef::Handle => quote!(u64),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => quote!(::std::string::String),
        TypeRef::Bytes | TypeRef::BorrowedBytes => quote!(::std::vec::Vec<u8>),
        TypeRef::Struct(s) | TypeRef::Enum(s) => {
            let ty = rust_type_ident(s);
            quote!(#ty)
        }
        TypeRef::TypedHandle(n) => {
            let ty = rust_type_ident(n);
            quote!(*mut #ty)
        }
        TypeRef::Optional(inner) => {
            let inner = typeref_to_rust(inner)?;
            quote!(::std::option::Option<#inner>)
        }
        TypeRef::List(inner) => {
            let inner = typeref_to_rust(inner)?;
            quote!(::std::vec::Vec<#inner>)
        }
        TypeRef::Map(k, v) => {
            let k = typeref_to_rust(k)?;
            let v = typeref_to_rust(v)?;
            quote!(::std::collections::HashMap<#k, #v>)
        }
        TypeRef::Iterator(_) => return Err(unsupported("type", "nested iterator")),
    })
}

/// Dispatch one exported function to the codegen for its call shape.
fn gen_function(f: &FnBinding, sfn: &syn::ItemFn) -> syn::Result<TokenStream> {
    match &f.shape {
        CallShape::Sync(abi) => gen_sync_function(f, abi, sfn),
        CallShape::Iterator(it) => gen_iterator_function(f, it, sfn),
        CallShape::Async(a) => gen_async_function(f, a, sfn),
    }
}

/// Generate the `extern "C"` thunk for one synchronous exported function.
fn gen_sync_function(f: &FnBinding, abi: &AbiFn, sfn: &syn::ItemFn) -> syn::Result<TokenStream> {
    let sym = ident(&abi.symbol);
    let params: Vec<TokenStream> = fn_slots(&abi.params, f, sfn);
    let arrow = ret_arrow_for(&abi.ret, f, sfn);
    let sentinel = sentinel(&abi.ret);

    // Lift each parameter, collecting the preambles and the call arguments.
    let mut preamble = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let is_ref = param_is_ref(sfn, &pb.name);
        let (pre, arg) = lift_param(pb, is_ref, &sentinel)?;
        preamble.extend(pre);
        call_args.push(arg);
    }

    let fn_ident = ident(&f.name);
    let call = quote!(#fn_ident(#(#call_args),*));

    let body = build_call_body(&f.ret, &abi.ret, returns_result(&sfn.sig.output), call)?;

    Ok(quote! {
        #[no_mangle]
        #[allow(unsafe_code, deprecated, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #sym(#(#params),*) #arrow {
            #preamble
            #body
        }
    })
}

/// Generate the launcher / `_next` / `_destroy` trio for a function returning
/// `iter<T>`. The producer returns a `weaveffi::Iter<T>` (optionally wrapped in
/// `Result`); the launcher boxes it behind the opaque iterator handle, `_next`
/// pulls one element and lowers it through `out_item`, and `_destroy` drops the
/// box.
fn gen_iterator_function(
    f: &FnBinding,
    it: &IteratorBinding,
    sfn: &syn::ItemFn,
) -> syn::Result<TokenStream> {
    let elem_rust = typeref_to_rust(&it.elem)?;
    let iter_rust = quote!(::weaveffi::Iter<#elem_rust>);

    // ── launcher: lift inputs, run the user fn, box the iterator ──
    let launch_sym = ident(&it.launch.symbol);
    let launch_params: Vec<TokenStream> = fn_slots(&it.launch.params, f, sfn);
    let launch_sentinel = quote!(::std::ptr::null_mut());

    let mut preamble = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let is_ref = param_is_ref(sfn, &pb.name);
        let (pre, arg) = lift_param(pb, is_ref, &launch_sentinel)?;
        preamble.extend(pre);
        call_args.push(arg);
    }
    let fn_ident = ident(&f.name);
    let call = quote!(#fn_ident(#(#call_args),*));
    let bind_iter = if returns_result(&sfn.sig.output) {
        quote! {
            let __wv_iter = match #call {
                ::std::result::Result::Ok(__v) => __v,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_set(
                        out_err,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
                    );
                    return ::std::ptr::null_mut();
                }
            };
        }
    } else {
        quote!(let __wv_iter = #call;)
    };
    let launch = quote! {
        #[no_mangle]
        #[allow(unsafe_code, deprecated, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #launch_sym(#(#launch_params),*) -> *mut #iter_rust {
            #preamble
            #bind_iter
            ::weaveffi::abi::error_set_ok(out_err);
            ::std::boxed::Box::into_raw(::std::boxed::Box::new(__wv_iter))
        }
    };

    // ── next: pull one element, lower it into `out_item`, return 1/0 ──
    let next_sym = ident(&it.next.symbol);
    // The first slot is the opaque `iter` handle (spelled with the real Rust
    // type); the rest (`out_item`, any item out-params, `out_err`) lower
    // straight from the model.
    let rest_params: Vec<TokenStream> = it.next.params[1..].iter().map(slot_tokens).collect();
    let item_lowered = lower_value(&it.elem, quote!(__wv_item))?;
    if matches!(it.elem, TypeRef::Map(_, _)) {
        return Err(unsupported("iterator", "map element type"));
    }
    let next = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #next_sym(iter: *mut #iter_rust, #(#rest_params),*) -> i32 {
            if iter.is_null() || out_item.is_null() {
                ::weaveffi::abi::error_set(out_err, -1, "iterator or out_item is null");
                return 0;
            }
            let __wv_it = unsafe { &mut *iter };
            match ::std::iter::Iterator::next(__wv_it) {
                ::std::option::Option::Some(__wv_item) => {
                    ::weaveffi::abi::error_set_ok(out_err);
                    let __wv_slot = #item_lowered;
                    unsafe { *out_item = __wv_slot };
                    1
                }
                ::std::option::Option::None => {
                    ::weaveffi::abi::error_set_ok(out_err);
                    0
                }
            }
        }
    };

    // ── destroy ──
    let destroy_sym = ident(&it.destroy_symbol);
    let destroy = quote! {
        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #destroy_sym(iter: *mut #iter_rust) {
            if !iter.is_null() {
                unsafe { drop(::std::boxed::Box::from_raw(iter)) };
            }
        }
    };

    Ok(quote! {
        #launch
        #next
        #destroy
    })
}

// ── Async functions ──────────────────────────────────────────────────────

/// Lift one async-launcher input slot into an *owned* Rust value and give the
/// call argument the user's `async fn` receives.
///
/// Async inputs must own their data before the worker thread runs: the foreign
/// caller may free or reuse the argument buffers as soon as the launcher
/// returns. So borrowed slots (`&str`, `&[u8]`, `[&str]`) are copied here, just
/// like the hand-written async sample does. There is no `out_err` slot on a
/// launcher, so an invalid input is lifted leniently (an unreadable string
/// becomes empty) rather than reported - the completion callback is the only
/// error channel, and it fires with the result the future produces.
fn lift_async_input(
    pb: &ParamBinding,
    sfn: &syn::ItemFn,
) -> syn::Result<(TokenStream, TokenStream, TokenStream)> {
    let name = ident(&pb.name);
    let none = TokenStream::new();
    Ok(match &pb.ty {
        ty if is_copy(ty) && !matches!(ty, TypeRef::Enum(_)) => (none.clone(), none, quote!(#name)),
        // A typed handle is an opaque pointer, which is not `Send`. Carry it
        // across the worker-thread boundary as a `usize` and rebuild it inside
        // the closure with the producer's own pointer type (so a `super::T`
        // path stays in scope), mirroring the hand-written async sample.
        TypeRef::TypedHandle(_) => {
            let addr = ident(&format!("__wv_addr_{}", pb.name));
            let uty = user_param_type(sfn, &pb.name)
                .ok_or_else(|| unsupported(&pb.name, "async typed-handle parameter"))?;
            (
                quote!(let #addr = #name as usize;),
                quote!(let #name = #addr as #uty;),
                quote!(#name),
            )
        }
        TypeRef::StringUtf8 => (
            quote!(let #name = ::weaveffi::abi::c_ptr_to_string(#name).unwrap_or_default();),
            none,
            quote!(#name),
        ),
        TypeRef::BorrowedStr => (
            quote!(let #name = ::weaveffi::abi::c_ptr_to_string(#name).unwrap_or_default();),
            none,
            quote!(&#name),
        ),
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            (
                quote!(let #name = ::weaveffi::abi::lift_opt_string(#name);),
                none,
                quote!(#name),
            )
        }
        TypeRef::Optional(inner) if is_copy(inner) => (
            quote!(let #name = unsafe { ::weaveffi::abi::lift_opt_scalar(#name) };),
            none,
            quote!(#name),
        ),
        TypeRef::Bytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                none,
                quote!(#name),
            )
        }
        TypeRef::BorrowedBytes => {
            let ptr = ident(&format!("{}_ptr", pb.name));
            let len = ident(&format!("{}_len", pb.name));
            (
                quote!(let #name = unsafe { ::weaveffi::abi::lift_bytes(#ptr, #len) };),
                none,
                quote!(&#name),
            )
        }
        TypeRef::List(inner) => {
            let len = ident(&format!("{}_len", pb.name));
            let pre = match &**inner {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_string_vec(#name, #len) };)
                }
                t if is_copy(t) => {
                    quote!(let #name = unsafe { ::weaveffi::abi::lift_scalar_vec(#name, #len) };)
                }
                _ => return Err(unsupported(&pb.name, "async list element type")),
            };
            (pre, none, quote!(#name))
        }
        _ => return Err(unsupported(&pb.name, "async parameter type")),
    })
}

/// Lower the future's output into the completion callback's *result* arguments
/// (the slots after `context` and `err`), returning `(preamble, args)`.
///
/// Single-slot results (scalars, strings, structs, options) need no preamble
/// and produce one argument. A `[T]` list result lowers to the two-slot
/// `(result, result_len)` pair exactly like a synchronous list return (so the
/// foreign side frees it the same way), emitting a preamble that builds the
/// base pointer and length. `bytes` and `map<K,V>` results are not yet
/// supported.
fn async_result_args(
    ty: &TypeRef,
    value: TokenStream,
) -> syn::Result<(TokenStream, Vec<TokenStream>)> {
    let none = TokenStream::new();
    Ok(match ty {
        t if is_copy(t) && !matches!(t, TypeRef::Enum(_)) => (none, vec![value]),
        TypeRef::Enum(_) => (none, vec![quote!((#value) as i32)]),
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => (
            none,
            vec![quote!(::weaveffi::abi::string_to_c_ptr(&(#value)))],
        ),
        TypeRef::Struct(_) => (
            none,
            vec![quote!(::std::boxed::Box::into_raw(::std::boxed::Box::new(#value)))],
        ),
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            (
                none,
                vec![quote!(::weaveffi::abi::lower_opt_string(#value))],
            )
        }
        TypeRef::Optional(inner) if is_copy(inner) => (
            none,
            vec![quote!(::weaveffi::abi::lower_opt_scalar(#value))],
        ),
        TypeRef::Optional(inner) if matches!(**inner, TypeRef::Struct(_)) => (
            none,
            vec![quote! {
                match #value {
                    ::std::option::Option::Some(__v) =>
                        ::std::boxed::Box::into_raw(::std::boxed::Box::new(__v)),
                    ::std::option::Option::None => ::std::ptr::null_mut(),
                }
            }],
        ),
        TypeRef::List(inner) => {
            let base = match &**inner {
                TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
                    quote!(unsafe { ::weaveffi::abi::lower_string_vec(__wv_list, &mut __wv_len) })
                }
                t if is_copy(t) => {
                    quote!(unsafe { ::weaveffi::abi::lower_scalar_vec(__wv_list, &mut __wv_len) })
                }
                TypeRef::Struct(_) => quote! {
                    unsafe {
                        ::weaveffi::abi::lower_ptr_vec(
                            __wv_list.into_iter()
                                .map(|__e| ::std::boxed::Box::into_raw(::std::boxed::Box::new(__e)))
                                .collect::<::std::vec::Vec<_>>(),
                            &mut __wv_len,
                        )
                    }
                },
                _ => return Err(unsupported("async return", "list element type")),
            };
            let pre = quote! {
                let __wv_list = #value;
                let mut __wv_len: usize = 0;
                let __wv_base = #base;
            };
            (pre, vec![quote!(__wv_base), quote!(__wv_len)])
        }
        _ => return Err(unsupported("async return", "result type")),
    })
}

/// Generate the completion-callback typedef and the `_async` launcher for an
/// `async fn`.
///
/// The launcher lifts inputs into owned values, spawns a worker thread, drives
/// the future to completion with `weaveffi::abi::block_on`, then invokes the
/// host's callback with `(context, err, result…)`. A `Result`
/// return routes its `Err` through a transient `weaveffi_error`; success passes
/// a null `err`. The foreign caller's `context` is moved across the thread
/// boundary as a `usize` (so the closure is `Send`); the callback pointer is
/// `Send` on its own.
fn gen_async_function(
    f: &FnBinding,
    a: &AsyncBinding,
    sfn: &syn::ItemFn,
) -> syn::Result<TokenStream> {
    let cb_ty = ident(&a.callback_type);
    let cb_slots: Vec<TokenStream> = a
        .callback_params
        .iter()
        .map(|p| ctype_to_rust(&p.ty))
        .collect();
    let callback_typedef = quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub type #cb_ty = extern "C" fn(#(#cb_slots),*);
    };

    let launch_sym = ident(&a.launch.symbol);
    let launch_params: Vec<TokenStream> = fn_slots(&a.launch.params, f, sfn);

    // Lift each logical input into three parts: a pre-spawn statement that runs
    // on the caller's thread (owning borrowed data, bouncing a non-`Send`
    // handle through a `usize`), an in-closure statement that reconstitutes the
    // value on the worker thread, and the argument forwarded to the producer.
    let mut pre_spawn = TokenStream::new();
    let mut in_closure = TokenStream::new();
    let mut call_args: Vec<TokenStream> = Vec::new();
    for pb in &f.params {
        let (pre, inc, arg) = lift_async_input(pb, sfn)?;
        pre_spawn.extend(pre);
        in_closure.extend(inc);
        call_args.push(arg);
    }

    // A `#[weaveffi::cancellable]` function receives the launcher's
    // `cancel_token` slot as a `Send` [`CancelToken`] appended after its
    // logical inputs (the producer declares it as the final parameter).
    // Building it before the spawn keeps the raw pointer off the capture list.
    if f.cancellable {
        pre_spawn.extend(quote! {
            let __wv_cancel = ::weaveffi::CancelToken::from_raw(cancel_token);
        });
        call_args.push(quote!(__wv_cancel));
    }

    let fn_ident = ident(&f.name);

    // The error path replays the result slots as their zero/null sentinels.
    let sentinels: Vec<TokenStream> = a
        .callback_params
        .iter()
        .skip(2)
        .map(|p| sentinel(&p.ty))
        .collect();

    let is_result = returns_result(&sfn.sig.output);
    let (result_pre, success_args) = match &f.ret {
        Some(ty) => async_result_args(ty, quote!(__wv_val))?,
        None => (TokenStream::new(), Vec::new()),
    };
    let success_call = quote! {
        #result_pre
        callback(__wv_ctx as *mut ::std::ffi::c_void, ::std::ptr::null_mut() #(, #success_args)*);
    };

    let dispatch = if is_result {
        let bind = if f.ret.is_some() {
            quote!(__wv_val)
        } else {
            quote!(_)
        };
        quote! {
            match __wv_out {
                ::std::result::Result::Ok(#bind) => { #success_call }
                ::std::result::Result::Err(__wv_err) => {
                    let mut __wv_e = ::weaveffi::abi::weaveffi_error::default();
                    ::weaveffi::abi::error_set(
                        &mut __wv_e,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
                    );
                    callback(__wv_ctx as *mut ::std::ffi::c_void, &mut __wv_e #(, #sentinels)*);
                    ::weaveffi::abi::error_clear(&mut __wv_e);
                }
            }
        }
    } else if f.ret.is_some() {
        quote! {
            let __wv_val = __wv_out;
            #success_call
        }
    } else {
        quote! {
            let () = __wv_out;
            #success_call
        }
    };

    Ok(quote! {
        #callback_typedef

        #[no_mangle]
        #[allow(unsafe_code, deprecated, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #launch_sym(#(#launch_params),*) {
            #pre_spawn
            let __wv_ctx = context as usize;
            // Drive the future to completion, then fire the host callback. On a
            // threaded target this happens on a worker thread so the launcher
            // returns immediately; `wasm32` has no threads, so run inline (the
            // sample futures are ready without awaiting real I/O).
            let __wv_body = move || {
                #in_closure
                let __wv_out = ::weaveffi::abi::block_on(#fn_ident(#(#call_args),*));
                #dispatch
            };
            #[cfg(target_arch = "wasm32")]
            {
                __wv_body();
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                ::std::thread::spawn(__wv_body);
            }
        }
    })
}

// ── Callbacks and listeners ──────────────────────────────────────────────

/// Emit the `extern "C"` function-pointer typedef for a module-level callback.
///
/// The alias names every ABI slot (the callback's parameters plus the trailing
/// `void* context`) so a registered host function is called with exactly the
/// signature the generated header declares. It is `#[doc(hidden)]` because the
/// `register_*` symbol, not the bare typedef, is the producer-facing surface.
fn gen_callback_type(c: &CallbackBinding) -> syn::Result<TokenStream> {
    let ty = ident(&c.c_fn_type);
    let slots: Vec<TokenStream> = c.abi_params.iter().map(|p| ctype_to_rust(&p.ty)).collect();
    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_camel_case_types)]
        pub type #ty = extern "C" fn(#(#slots),*);
    })
}

/// Build the four pieces the emit helper needs for one callback parameter: the
/// Rust parameter it accepts, the lowering preamble, the C call argument(s) it
/// forwards, and any post-call cleanup. Borrowed inputs (`&str`, `&[u8]`,
/// `&Struct`) are lowered into temporaries that live across every dispatch and
/// are freed once afterward, mirroring the hand-written events sample.
fn emit_callback_param(
    pb: &ParamBinding,
) -> syn::Result<(TokenStream, TokenStream, Vec<TokenStream>, TokenStream)> {
    let n = ident(&pb.name);
    let none = TokenStream::new();
    Ok(match &pb.ty {
        TypeRef::Enum(name) => {
            let et = rust_type_ident(name);
            (
                quote!(#n: #et),
                none.clone(),
                vec![quote!((#n) as i32)],
                none,
            )
        }
        ty if is_copy(ty) => {
            let rt = typeref_to_rust(ty)?;
            (quote!(#n: #rt), none.clone(), vec![quote!(#n)], none)
        }
        TypeRef::StringUtf8 | TypeRef::BorrowedStr => {
            let tmp = ident(&format!("__wv_cb_{}", pb.name));
            (
                quote!(#n: &str),
                quote!(let #tmp = ::weaveffi::abi::string_to_c_ptr(#n);),
                vec![quote!(#tmp)],
                quote!(::weaveffi::abi::free_string(#tmp);),
            )
        }
        TypeRef::Optional(inner)
            if matches!(**inner, TypeRef::StringUtf8 | TypeRef::BorrowedStr) =>
        {
            let tmp = ident(&format!("__wv_cb_{}", pb.name));
            (
                quote!(#n: ::std::option::Option<&str>),
                quote!(let #tmp = ::weaveffi::abi::lower_opt_string(#n);),
                vec![quote!(#tmp)],
                quote!(if !#tmp.is_null() { ::weaveffi::abi::free_string(#tmp); }),
            )
        }
        TypeRef::Bytes | TypeRef::BorrowedBytes => (
            quote!(#n: &[u8]),
            none.clone(),
            vec![quote!(#n.as_ptr()), quote!(#n.len())],
            none,
        ),
        TypeRef::Struct(s) => {
            let st = rust_type_ident(s);
            (
                quote!(#n: &#st),
                none.clone(),
                vec![quote!(#n as *const #st)],
                none,
            )
        }
        _ => return Err(unsupported(&pb.name, "callback parameter type")),
    })
}

/// Emit a listener's registry, its `register_*`/`unregister_*` symbols, and a
/// safe `emit_<listener>` helper the producer calls to fire every subscriber.
///
/// The registry is a module-level `Mutex<Vec<(callback, context, id)>>`;
/// `emit_*` snapshots the `(callback, context)` pairs under the lock, releases
/// it, lowers the event payload once, dispatches to each subscriber, and frees
/// the payload, so a re-entrant `register`/`unregister` from inside a callback
/// cannot deadlock.
fn gen_listener(mb: &ModuleBinding, l: &ListenerBinding) -> syn::Result<TokenStream> {
    let cb = mb.callback(&l.event_callback).ok_or_else(|| {
        syn::Error::new(
            Span::call_site(),
            format!(
                "weaveffi: listener `{}` references unknown callback `{}`",
                l.name, l.event_callback
            ),
        )
    })?;

    let cb_ty = ident(&l.callback_c_fn_type);
    let reg = ident(&format!("__WEAVEFFI_REG_{}", l.name));
    let next_id = ident(&format!("__WEAVEFFI_NEXTID_{}", l.name));
    let register_sym = ident(&l.register_symbol);
    let unregister_sym = ident(&l.unregister_symbol);
    let emit_fn = ident(&format!("emit_{}", l.name));

    let mut decls: Vec<TokenStream> = Vec::new();
    let mut pre = TokenStream::new();
    let mut c_args: Vec<TokenStream> = Vec::new();
    let mut cleanup = TokenStream::new();
    for pb in &cb.params {
        let (decl, p, args, free) = emit_callback_param(pb)?;
        decls.push(decl);
        pre.extend(p);
        c_args.extend(args);
        cleanup.extend(free);
    }

    let emit_doc = format!(
        "Fire the `{}` listener, invoking every registered `{}` callback.",
        l.name, l.event_callback
    );

    Ok(quote! {
        #[allow(non_upper_case_globals)]
        static #reg: ::std::sync::Mutex<::std::vec::Vec<(#cb_ty, usize, u64)>> =
            ::std::sync::Mutex::new(::std::vec::Vec::new());
        #[allow(non_upper_case_globals)]
        static #next_id: ::std::sync::atomic::AtomicU64 =
            ::std::sync::atomic::AtomicU64::new(1);

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref, clippy::missing_safety_doc)]
        pub extern "C" fn #register_sym(
            callback: #cb_ty,
            context: *mut ::std::ffi::c_void,
        ) -> u64 {
            let __id = #next_id.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed);
            #reg.lock()
                .unwrap_or_else(::std::sync::PoisonError::into_inner)
                .push((callback, context as usize, __id));
            __id
        }

        #[no_mangle]
        #[allow(unsafe_code, clippy::not_unsafe_ptr_arg_deref)]
        pub extern "C" fn #unregister_sym(id: u64) {
            #reg.lock()
                .unwrap_or_else(::std::sync::PoisonError::into_inner)
                .retain(|&(_, _, __i)| __i != id);
        }

        #[doc = #emit_doc]
        #[allow(unsafe_code)]
        pub fn #emit_fn(#(#decls),*) {
            let __targets: ::std::vec::Vec<(#cb_ty, usize)> = #reg
                .lock()
                .unwrap_or_else(::std::sync::PoisonError::into_inner)
                .iter()
                .map(|&(__c, __ctx, _)| (__c, __ctx))
                .collect();
            if __targets.is_empty() {
                return;
            }
            #pre
            for (__cb, __ctx) in __targets {
                __cb(#(#c_args,)* __ctx as *mut ::std::ffi::c_void);
            }
            #cleanup
        }
    })
}

/// Assemble the call + error handling + return lowering for a function or
/// constructor whose `call` expression invokes the user's code.
fn build_call_body(
    ret_ty: &Option<TypeRef>,
    ret_ctype: &CType,
    is_result: bool,
    call: TokenStream,
) -> syn::Result<TokenStream> {
    let sentinel = sentinel(ret_ctype);
    let lowered = match ret_ty {
        Some(ty) => lower_value(ty, quote!(__wv_ret))?,
        None => quote!(()),
    };
    let ok_arm = if ret_ty.is_some() {
        quote! {{ ::weaveffi::abi::error_set_ok(out_err); #lowered }}
    } else {
        quote! {{ ::weaveffi::abi::error_set_ok(out_err); }}
    };

    Ok(if is_result {
        // A `Result<(), E>` thunk returns void, so its `Err` arm must stop at the
        // `error_set` statement; emitting the void sentinel `()` would leave a bare
        // trailing unit that trips clippy's `unused_unit`.
        let (bind, err_sentinel) = if ret_ty.is_some() {
            (quote!(__wv_ret), quote!(#sentinel))
        } else {
            (quote!(_), TokenStream::new())
        };
        quote! {
            match #call {
                ::std::result::Result::Ok(#bind) => #ok_arm,
                ::std::result::Result::Err(__wv_err) => {
                    ::weaveffi::abi::error_set(
                        out_err,
                        ::weaveffi::abi::ErrorReport::code(&__wv_err),
                        &::weaveffi::abi::ErrorReport::message(&__wv_err),
                    );
                    #err_sentinel
                }
            }
        }
    } else if ret_ty.is_some() {
        quote! {
            let __wv_ret = #call;
            ::weaveffi::abi::error_set_ok(out_err);
            #lowered
        }
    } else {
        quote! {
            #call;
            ::weaveffi::abi::error_set_ok(out_err);
        }
    })
}

// ── Records ──────────────────────────────────────────────────────────────

/// Generate the create/destroy/getter surface for one record (plus a fluent
/// builder when the record opted in with `#[weaveffi::builder]`).
fn gen_record(s: &StructBinding, item: Option<&syn::ItemStruct>) -> syn::Result<TokenStream> {
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

    // destroy.
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
        TypeRef::Optional(inner) if matches!(**inner, TypeRef::Struct(_)) => quote! {
            b.#fname = ::std::option::Option::Some(if #slot.is_null() {
                ::std::option::Option::None
            } else {
                ::std::option::Option::Some(unsafe { &*#slot }.clone())
            });
        },
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
                TypeRef::Struct(st) => {
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
        TypeRef::Struct(_) => quote! {
            if !#slot.is_null() {
                b.#fname = ::std::option::Option::Some(unsafe { &*#slot }.clone());
            }
        },
        TypeRef::TypedHandle(_) => quote!(b.#fname = ::std::option::Option::Some(#slot);),
        _ => return Err(unsupported(&field.name, "builder field type")),
    })
}

/// Adapt a [`FieldBinding`] to a [`ParamBinding`] so the field lift reuses the
/// parameter machinery (records lower their create fields exactly like inputs).
fn field_as_param(field: &FieldBinding) -> ParamBinding {
    ParamBinding {
        name: field.name.clone(),
        ty: field.ty.clone(),
        mutable: false,
        doc: None,
        abi: field.value_params.clone(),
    }
}

/// Generate one field getter: `{tag}_get_{field}(ptr, [out_len]) -> ...`.
fn gen_getter(rust_ty: &Ident, field: &FieldBinding) -> syn::Result<TokenStream> {
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

// ── Enums ────────────────────────────────────────────────────────────────

/// Generate the surface for one enum. A C-style enum needs only the private
/// `i32 -> enum` conversion the marshalling uses (the header emits its variants
/// as constants); a rich (algebraic) enum delegates to [`gen_rich_enum`] for
/// its opaque-object surface.
fn gen_enum(e: &EnumBinding, item: Option<&syn::ItemEnum>) -> syn::Result<TokenStream> {
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
/// itself crosses the ABI exactly like a struct (an owning `*mut Enum`), so the
/// surrounding marshalling reuses the `Struct` arms; only this declaration set
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

    // destroy.
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
