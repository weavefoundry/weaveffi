//! Extract a WeaveFFI [`Api`] from annotated Rust source.
//!
//! This crate is the single Rust-to-IR bridge shared by two callers:
//!
//! * the `#[weaveffi::module]` proc-macro, which lowers a module to its IR to
//!   build the C ABI scaffolding at compile time; and
//! * the `weaveffi` CLI, which reads the same annotated source to drive
//!   `generate`/`extract`.
//!
//! Because both paths call into the *same* extraction, the IDL the CLI emits
//! and the symbols the macro produces cannot drift: they are two views of one
//! parse. Annotated Rust is therefore the single source of truth.
//!
//! # The annotation scheme
//!
//! A `#[weaveffi::module]` on an inline `mod` marks an exported namespace.
//! Inside it:
//!
//! * `#[weaveffi::export]` on a `fn` exports a function. An `async fn` lowers to
//!   an asynchronous symbol; a `fn -> Result<T, E>` is fallible (`throws` in
//!   the IDL; the return type is `T` and errors are reported through the ABI's
//!   `out_err`).
//! * `#[weaveffi::interface]` on a `struct` declares an interface (opaque,
//!   reference-counted object type). Its `impl` block's `pub fn`s become the
//!   interface's members: an associated function returning `Self` (or
//!   `Arc<Self>`) is a constructor, a `&self` or `self: Arc<Self>` function is
//!   a method, and any other associated function is a static.
//! * `#[weaveffi::record]` on a `struct` declares a by-value record.
//! * `#[weaveffi::enumeration]` on a `#[repr(i32)]` `enum` declares a C-style
//!   enum; an enum with data-carrying variants declares a rich enum.
//! * `#[weaveffi::error]` on an enum declares the module's error domain.
//! * `#[weaveffi::callback_interface]` on a `trait` declares a callback
//!   interface: a method set the consumer implements. Producers accept one as
//!   `Arc<dyn Trait>`.
//!
//! The type mapping mirrors the IDL: `String` (or `&str`) is a string, `Vec<u8>`
//! (or `&[u8]`) is a byte buffer, every integer primitive (including `u64`) is
//! the matching scalar, `Vec<T>`, `Option<T>`, and `HashMap`/`BTreeMap` are the
//! list, optional, and map types, `weaveffi::Iter<T>` is an `iter<T>` return,
//! `Arc<T>` names the interface `T` (an object reference), `Arc<dyn Trait>`
//! names the callback interface `Trait`, and any other named path is a record,
//! enum, or interface resolved later.

#![deny(missing_docs)]

use syn::spanned::Spanned;
use weaveffi_ir::ir::{
    Api, CallbackInterfaceDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function,
    InterfaceDef, Module, Param, StructDef, StructField, TypeRef, CURRENT_SCHEMA_VERSION,
};

/// Match a WeaveFFI marker attribute by its final path segment.
///
/// Accepts both the namespaced form (`#[weaveffi::record]`) and the bare form
/// (`#[record]`), so the macro's stripped re-emit and a hand-written attribute
/// resolve identically.
fn is_marker(attr: &syn::Attribute, name: &str) -> bool {
    attr.path().segments.last().is_some_and(|s| s.ident == name)
}

/// Whether any attribute in `attrs` is the WeaveFFI marker `name`.
pub fn has_marker(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|a| is_marker(a, name))
}

fn has_repr_i32(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("repr") && a.parse_args::<syn::Ident>().is_ok_and(|id| id == "i32")
    })
}

/// Parse `#[deprecated(since = "...", note = "...")]` into `(since, note)`.
fn parse_deprecated(attrs: &[syn::Attribute]) -> (Option<String>, Option<String>) {
    let Some(attr) = attrs.iter().find(|a| a.path().is_ident("deprecated")) else {
        return (None, None);
    };
    let mut since = None;
    let mut note = None;
    if matches!(attr.meta, syn::Meta::Path(_)) {
        return (None, Some("deprecated".to_string()));
    }
    let _ = attr.parse_nested_meta(|meta| {
        let Some(ident) = meta.path.get_ident() else {
            return Ok(());
        };
        let value = meta.value()?;
        let lit: syn::LitStr = value.parse()?;
        match ident.to_string().as_str() {
            "since" => since = Some(lit.value()),
            "note" => note = Some(lit.value()),
            _ => {}
        }
        Ok(())
    });
    if note.is_none() && since.is_none() {
        note = Some("deprecated".to_string());
    }
    (since, note)
}

fn extract_doc(attrs: &[syn::Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter_map(|attr| {
            let syn::Meta::NameValue(nv) = &attr.meta else {
                return None;
            };
            if !nv.path.is_ident("doc") {
                return None;
            }
            let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            else {
                return None;
            };
            let val = s.value();
            Some(match val.strip_prefix(' ') {
                Some(stripped) => stripped.to_string(),
                None => val,
            })
        })
        .collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn is_ident(ty: &syn::Type, name: &str) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.is_ident(name))
}

fn single_generic_arg(seg: &syn::PathSegment) -> syn::Result<&syn::Type> {
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return Err(syn::Error::new(seg.span(), "expected generic arguments"));
    };
    if args.args.len() != 1 {
        return Err(syn::Error::new(
            seg.span(),
            "expected exactly 1 generic argument",
        ));
    }
    let syn::GenericArgument::Type(ty) = &args.args[0] else {
        return Err(syn::Error::new(seg.span(), "expected a type argument"));
    };
    Ok(ty)
}

fn two_generic_args(seg: &syn::PathSegment) -> syn::Result<(&syn::Type, &syn::Type)> {
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return Err(syn::Error::new(seg.span(), "expected generic arguments"));
    };
    if args.args.len() != 2 {
        return Err(syn::Error::new(
            seg.span(),
            "expected exactly 2 generic arguments",
        ));
    }
    let syn::GenericArgument::Type(k) = &args.args[0] else {
        return Err(syn::Error::new(seg.span(), "expected a key type argument"));
    };
    let syn::GenericArgument::Type(v) = &args.args[1] else {
        return Err(syn::Error::new(
            seg.span(),
            "expected a value type argument",
        ));
    };
    Ok((k, v))
}

fn type_path_ident(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else { return None };
    p.path.segments.last().map(|s| s.ident.to_string())
}

/// The bare name of the single trait a `dyn Trait` object names, ignoring
/// auto-trait and lifetime bounds (`dyn Listener + Send + Sync + 'static`).
fn trait_object_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::TraitObject(obj) = ty else {
        return None;
    };
    let mut names = obj.bounds.iter().filter_map(|b| match b {
        syn::TypeParamBound::Trait(t) => {
            let name = t.path.segments.last()?.ident.to_string();
            (!matches!(name.as_str(), "Send" | "Sync" | "Unpin")).then_some(name)
        }
        _ => None,
    });
    let first = names.next()?;
    names.next().is_none().then_some(first)
}

/// Peel `Arc<T>` to its `T`, returning any other type unchanged.
///
/// `Arc` is how a producer spells an interface object it holds or returns
/// (`Arc<Store>`, `Arc<Self>`) and a callback interface it accepts
/// (`Arc<dyn Listener>`); the IR names the pointee type in both cases.
pub fn peel_arc(ty: &syn::Type) -> &syn::Type {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            if seg.ident == "Arc" {
                if let Ok(inner) = single_generic_arg(seg) {
                    return inner;
                }
            }
        }
    }
    ty
}

/// Whether a parameter's type is the reserved `weaveffi::CancelToken`.
///
/// A `#[weaveffi::cancellable]` `async fn` accepts a `CancelToken` as its final
/// parameter, but the token is part of the async *calling convention* (the
/// launcher's `cancel_token` slot), not the function's logical signature, so it
/// is filtered out of the extracted parameter list. Matching on the final path
/// segment accepts both the bare and `weaveffi::`-qualified spellings.
fn is_cancel_token(ty: &syn::Type) -> bool {
    type_path_ident(ty).as_deref() == Some("CancelToken")
}

/// Map a Rust [`syn::Type`] onto the WeaveFFI [`TypeRef`] it represents.
///
/// This is the canonical mapping every caller shares. Notable conventions:
///
/// * `String` and `&str` are both the `string` type; `Vec<u8>` and `&[u8]`
///   are both `bytes`. A reference is a producer-side calling convention (the
///   thunk lifts an owned value and lends it), not an IDL distinction.
/// * every integer primitive maps to its matching scalar (`u64` included).
/// * `Vec<T>` and `&[T]` are lists, `Option<T>` is an optional, and
///   `HashMap`/`BTreeMap` are maps.
/// * `weaveffi::Iter<T>` is an `iter<T>` return.
/// * `Arc<T>` and `&T` name the interface `T`; `Arc<dyn Trait>` names the
///   callback interface `Trait`. Any other named path is a record, enum,
///   interface, or callback interface reference resolved by the validator.
///
/// # Errors
///
/// Returns a spanned error for type syntax WeaveFFI cannot express across the
/// FFI boundary (raw pointers, `Box<dyn Trait>`, tuples, a generic with the
/// wrong arity, and so on).
pub fn type_ref_from_syn(ty: &syn::Type) -> syn::Result<TypeRef> {
    match ty {
        syn::Type::Reference(r) => {
            if let syn::Type::Path(p) = r.elem.as_ref() {
                if p.path.is_ident("str") {
                    return Ok(TypeRef::StringUtf8);
                }
            }
            if let syn::Type::Slice(slice) = r.elem.as_ref() {
                if is_ident(&slice.elem, "u8") {
                    return Ok(TypeRef::Bytes);
                }
                return Ok(TypeRef::List(Box::new(type_ref_from_syn(&slice.elem)?)));
            }
            type_ref_from_syn(&r.elem)
        }
        syn::Type::Ptr(_) => Err(syn::Error::new(
            ty.span(),
            "weaveffi: raw pointers cannot cross the FFI boundary; declare the pointee as a \
             #[weaveffi::interface] and pass it as `&T` or `Arc<T>`",
        )),
        syn::Type::TraitObject(_) => {
            let name = trait_object_name(ty).ok_or_else(|| {
                syn::Error::new(
                    ty.span(),
                    "weaveffi: a trait object must name exactly one #[weaveffi::callback_interface] \
                     trait (plus optional `Send`/`Sync` bounds)",
                )
            })?;
            Ok(TypeRef::Named(name))
        }
        syn::Type::Path(type_path) => {
            let seg = type_path
                .path
                .segments
                .last()
                .ok_or_else(|| syn::Error::new(ty.span(), "empty type path"))?;
            let ident = seg.ident.to_string();
            match ident.as_str() {
                "i8" => Ok(TypeRef::I8),
                "i16" => Ok(TypeRef::I16),
                "i32" => Ok(TypeRef::I32),
                "i64" => Ok(TypeRef::I64),
                "u8" => Ok(TypeRef::U8),
                "u16" => Ok(TypeRef::U16),
                "u32" => Ok(TypeRef::U32),
                "f32" => Ok(TypeRef::F32),
                "f64" => Ok(TypeRef::F64),
                "bool" => Ok(TypeRef::Bool),
                "String" => Ok(TypeRef::StringUtf8),
                "u64" => Ok(TypeRef::U64),
                // `Arc<T>` is a reference to the interface object `T`, and
                // `Arc<dyn Trait>` a consumer-implemented callback interface;
                // the IR names the pointee in both cases.
                "Arc" => type_ref_from_syn(single_generic_arg(seg)?),
                "Box" | "Rc" => Err(syn::Error::new(
                    ty.span(),
                    format!(
                        "weaveffi: `{ident}` cannot cross the FFI boundary; objects and callback \
                         interfaces are shared, so spell them as `Arc<T>` / `Arc<dyn Trait>`"
                    ),
                )),
                "Vec" => {
                    let inner = single_generic_arg(seg)?;
                    if is_ident(inner, "u8") {
                        return Ok(TypeRef::Bytes);
                    }
                    Ok(TypeRef::List(Box::new(type_ref_from_syn(inner)?)))
                }
                "Option" => {
                    let inner = single_generic_arg(seg)?;
                    Ok(TypeRef::Optional(Box::new(type_ref_from_syn(inner)?)))
                }
                // `weaveffi::Iter<T>` is the producer spelling of an `iter<T>`
                // return: a lazily-pulled stream rather than a materialized list.
                "Iter" => {
                    let inner = single_generic_arg(seg)?;
                    Ok(TypeRef::Iterator(Box::new(type_ref_from_syn(inner)?)))
                }
                "HashMap" | "BTreeMap" => {
                    let (k, v) = two_generic_args(seg)?;
                    Ok(TypeRef::Map(
                        Box::new(type_ref_from_syn(k)?),
                        Box::new(type_ref_from_syn(v)?),
                    ))
                }
                other => Ok(TypeRef::Named(other.to_string())),
            }
        }
        _ => Err(syn::Error::new(ty.span(), "unsupported type syntax")),
    }
}

/// Peel `Result<T, E>` to its `T`, returning any other type unchanged. A
/// `Result` return marks the function `throws` in the IDL; the error type
/// itself carries no extra IR shape (it reports through the ABI's `out_err`).
pub fn peel_result(ty: &syn::Type) -> &syn::Type {
    if let syn::Type::Path(p) = ty {
        if let Some(seg) = p.path.segments.last() {
            if seg.ident == "Result" {
                if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
                    if let Some(syn::GenericArgument::Type(ok)) = args.args.first() {
                        return ok;
                    }
                }
            }
        }
    }
    ty
}

/// Whether a function's return type is a `Result<_, _>`, which marks the
/// function as `throws` in the IDL.
pub fn output_is_result(output: &syn::ReturnType) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    matches!(ty.as_ref(), syn::Type::Path(p)
        if p.path.segments.last().is_some_and(|s| s.ident == "Result"))
}

fn is_unit(ty: &syn::Type) -> bool {
    matches!(ty, syn::Type::Tuple(t) if t.elems.is_empty())
}

/// Map a function's return type to its IDL return [`TypeRef`], peeling
/// `Result<T, E>` and treating `()` (and `Result<(), E>`) as no return.
///
/// # Errors
///
/// Propagates any error from [`type_ref_from_syn`] on the (peeled) return type.
pub fn return_type_from_syn(output: &syn::ReturnType) -> syn::Result<Option<TypeRef>> {
    match output {
        syn::ReturnType::Default => Ok(None),
        syn::ReturnType::Type(_, ty) => {
            let inner = peel_result(ty);
            if is_unit(inner) {
                Ok(None)
            } else {
                Ok(Some(type_ref_from_syn(inner)?))
            }
        }
    }
}

fn parse_discriminant(expr: &syn::Expr) -> syn::Result<i32> {
    match expr {
        syn::Expr::Lit(lit) => {
            let syn::Lit::Int(int_lit) = &lit.lit else {
                return Err(syn::Error::new(
                    expr.span(),
                    "expected an integer literal discriminant",
                ));
            };
            int_lit.base10_parse::<i32>()
        }
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => {
            Ok(-parse_discriminant(&unary.expr)?)
        }
        _ => Err(syn::Error::new(
            expr.span(),
            "unsupported discriminant expression",
        )),
    }
}

fn extract_params(
    inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>,
) -> syn::Result<Vec<Param>> {
    inputs
        .iter()
        .filter_map(|arg| match arg {
            syn::FnArg::Typed(pt) => Some(pt),
            syn::FnArg::Receiver(_) => None,
        })
        // The cancellation token is part of the async calling convention, not a
        // logical parameter, so it never appears in the IDL.
        .filter(|pt| !is_cancel_token(&pt.ty))
        .map(|pt| {
            let param_name = match pt.pat.as_ref() {
                syn::Pat::Ident(id) => id.ident.to_string(),
                _ => return Err(syn::Error::new(pt.span(), "unsupported parameter pattern")),
            };
            if matches!(pt.ty.as_ref(), syn::Type::Reference(r) if r.mutability.is_some()) {
                return Err(syn::Error::new(
                    pt.ty.span(),
                    "weaveffi: `&mut` parameters cannot cross the FFI boundary; take the value \
                     by `&T` or by value and return the updated result",
                ));
            }
            let ty = type_ref_from_syn(&pt.ty)?;
            // The proc macro path skips `validate_api` (see the macro crate),
            // so the one type shape lowering can't represent as a parameter
            // must be caught here with a span instead of panicking later.
            let mut has_iterator = false;
            ty.walk(&mut |t| has_iterator |= matches!(t, TypeRef::Iterator(_)));
            if has_iterator {
                return Err(syn::Error::new(
                    pt.ty.span(),
                    "weaveffi: `Iter<T>` is a pull handle the consumer drives and is only \
                     valid as a function's outermost return type, not as a parameter",
                ));
            }
            Ok(Param {
                name: param_name,
                ty,
                doc: extract_doc(&pt.attrs),
            })
        })
        .collect()
}

/// Map one `fn` signature (free function, interface member, or callback
/// method) to the IR [`Function`]. `returns` is supplied by the caller because
/// constructors and `Self` returns need interface-aware handling.
fn function_from_sig(
    sig: &syn::Signature,
    attrs: &[syn::Attribute],
    returns: Option<TypeRef>,
) -> syn::Result<Function> {
    if let Some(ret) = &returns {
        let nested_iterator = |t: &TypeRef| {
            let mut found = false;
            t.walk(&mut |inner| found |= matches!(inner, TypeRef::Iterator(_)));
            found
        };
        let invalid = match ret {
            TypeRef::Iterator(elem) => nested_iterator(elem),
            other => nested_iterator(other),
        };
        if invalid {
            let span = match &sig.output {
                syn::ReturnType::Type(_, ty) => ty.span(),
                other => other.span(),
            };
            return Err(syn::Error::new(
                span,
                "weaveffi: `Iter<T>` is only valid as the outermost return type; it can't be \
                 nested in an `Option`, `Vec`, map, or another `Iter`",
            ));
        }
    }
    Ok(Function {
        name: sig.ident.to_string(),
        params: extract_params(&sig.inputs)?,
        returns,
        doc: extract_doc(attrs),
        throws: output_is_result(&sig.output),
        r#async: sig.asyncness.is_some(),
        cancellable: has_marker(attrs, "cancellable"),
        deprecated: parse_deprecated(attrs).1,
    })
}

fn extract_function(item: &syn::ItemFn) -> syn::Result<Function> {
    let returns = return_type_from_syn(&item.sig.output)?;
    function_from_sig(&item.sig, &item.attrs, returns)
}

/// Whether the (peeled) return type names the interface itself (`Self`,
/// `Arc<Self>`, or the interface's own name), which classifies an associated
/// function as a constructor.
fn returns_self(output: &syn::ReturnType, iface: &str) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    match type_path_ident(peel_arc(peel_result(ty))) {
        Some(name) => name == "Self" || name == iface,
        None => false,
    }
}

/// Extract one interface member from an `impl` block function.
///
/// The receiver decides the member kind at the call site (see
/// [`members_from_impl`]); this helper maps the signature. A `Self` (or
/// interface-named) return on a constructor is dropped: the IR leaves a
/// constructor's `return` empty because the instance is implicit.
fn extract_member(item: &syn::ImplItemFn, iface: &str, is_ctor: bool) -> syn::Result<Function> {
    let returns = if is_ctor {
        None
    } else {
        match return_type_from_syn(&item.sig.output)? {
            // `fn hand(&self) -> Arc<Self>` style returns name the interface.
            Some(TypeRef::Named(name)) if name == "Self" => Some(TypeRef::Named(iface.to_string())),
            other => other,
        }
    };
    function_from_sig(&item.sig, &item.attrs, returns)
}

/// Whether a method receiver is one WeaveFFI can lift: `&self` (a borrow for
/// the call) or `self: Arc<Self>` (a retained reference).
///
/// `&mut self` and by-value `self` are rejected: the object is shared across
/// the FFI boundary (and may be in use on other threads), so mutable state
/// needs interior mutability.
pub fn receiver_is_supported(recv: &syn::Receiver) -> bool {
    if recv.mutability.is_some() {
        return false;
    }
    if recv.reference.is_some() {
        return true;
    }
    recv.colon_token.is_some() && type_path_ident(&recv.ty).as_deref() == Some("Arc")
}

/// Classify and extract every `pub fn` of an interface's `impl` block into
/// the interface's constructors, methods, and statics.
///
/// * a function with a `&self` or `self: Arc<Self>` receiver is a **method**;
/// * an associated function returning `Self` or `Arc<Self>` (or the interface
///   type, optionally inside `Result`) is a **constructor**;
/// * any other associated function is a **static**.
///
/// Non-`pub` items are private helpers and stay unexported.
fn members_from_impl(item_impl: &syn::ItemImpl, iface: &mut InterfaceDef) -> syn::Result<()> {
    for impl_item in &item_impl.items {
        let syn::ImplItem::Fn(f) = impl_item else {
            continue;
        };
        if !matches!(f.vis, syn::Visibility::Public(_)) {
            continue;
        }
        match f.sig.receiver() {
            Some(recv) => {
                if !receiver_is_supported(recv) {
                    return Err(syn::Error::new(
                        recv.span(),
                        "weaveffi: interface methods must take `&self` or `self: Arc<Self>`; \
                         use interior mutability (Mutex, RwLock, atomics) for mutable state, \
                         because the object is shared across the FFI boundary",
                    ));
                }
                iface.methods.push(extract_member(f, &iface.name, false)?);
            }
            None if returns_self(&f.sig.output, &iface.name) => {
                iface
                    .constructors
                    .push(extract_member(f, &iface.name, true)?);
            }
            None => {
                iface.statics.push(extract_member(f, &iface.name, false)?);
            }
        }
    }
    Ok(())
}

/// Extract a `#[weaveffi::callback_interface]` trait into a
/// [`CallbackInterfaceDef`].
///
/// Every trait method is a callback method the consumer implements. Methods
/// must take `&self`; the validator additionally rejects `async`, `Result`
/// returns, and non-direct return types, so those are mapped faithfully here
/// and reported with the IDL-level diagnostics.
fn extract_callback_interface(item: &syn::ItemTrait) -> syn::Result<CallbackInterfaceDef> {
    let mut methods = Vec::new();
    for trait_item in &item.items {
        let syn::TraitItem::Fn(f) = trait_item else {
            continue;
        };
        match f.sig.receiver() {
            Some(recv) if recv.reference.is_some() && recv.mutability.is_none() => {}
            Some(recv) => {
                return Err(syn::Error::new(
                    recv.span(),
                    "weaveffi: callback interface methods must take `&self`",
                ));
            }
            None => {
                return Err(syn::Error::new(
                    f.sig.span(),
                    "weaveffi: callback interface methods must take `&self` (associated \
                     functions can't be implemented by the consumer)",
                ));
            }
        }
        let returns = return_type_from_syn(&f.sig.output)?;
        methods.push(function_from_sig(&f.sig, &f.attrs, returns)?);
    }
    Ok(CallbackInterfaceDef {
        name: item.ident.to_string(),
        doc: extract_doc(&item.attrs),
        deprecated: parse_deprecated(&item.attrs).1,
        methods,
    })
}

fn extract_struct(item: &syn::ItemStruct) -> syn::Result<StructDef> {
    let name = item.ident.to_string();
    let fields = match &item.fields {
        syn::Fields::Named(named) => named
            .named
            .iter()
            .map(|f| {
                let field_name = f
                    .ident
                    .as_ref()
                    .ok_or_else(|| syn::Error::new(f.span(), "unnamed field in record"))?
                    .to_string();
                Ok(StructField {
                    name: field_name,
                    ty: type_ref_from_syn(&f.ty)?,
                    doc: extract_doc(&f.attrs),
                })
            })
            .collect::<syn::Result<_>>()?,
        _ => {
            return Err(syn::Error::new(
                item.span(),
                "only named fields are supported for #[weaveffi::record]",
            ))
        }
    };
    Ok(StructDef {
        name,
        doc: extract_doc(&item.attrs),
        deprecated: parse_deprecated(&item.attrs).1,
        fields,
    })
}

/// Extract the named fields of one rich (algebraic) enum variant into the IR's
/// [`StructField`] list, the same shape a record's fields take.
fn extract_variant_fields(fields: &syn::FieldsNamed) -> syn::Result<Vec<StructField>> {
    fields
        .named
        .iter()
        .map(|f| {
            let field_name = f
                .ident
                .as_ref()
                .ok_or_else(|| syn::Error::new(f.span(), "unnamed field in enum variant"))?
                .to_string();
            Ok(StructField {
                name: field_name,
                ty: type_ref_from_syn(&f.ty)?,
                doc: extract_doc(&f.attrs),
            })
        })
        .collect()
}

fn extract_enum(item: &syn::ItemEnum) -> syn::Result<EnumDef> {
    let name = item.ident.to_string();

    // A *rich* (algebraic) enum has at least one variant carrying data. Rust
    // forbids explicit discriminants on such an enum, so its tags are the
    // declaration-order positions (0, 1, 2, …) - exactly what the IDL records.
    // A *C-style* enum (every variant fieldless) keeps the stricter contract:
    // it must be `#[repr(i32)]` with an explicit discriminant on each variant.
    let is_rich = item
        .variants
        .iter()
        .any(|v| !matches!(v.fields, syn::Fields::Unit));

    if !is_rich && !has_repr_i32(&item.attrs) {
        return Err(syn::Error::new(
            item.span(),
            format!(
                "enum `{}` must have #[repr(i32)] to be a #[weaveffi::enumeration]",
                item.ident
            ),
        ));
    }

    let mut next_value: i32 = 0;
    let variants = item
        .variants
        .iter()
        .map(|v| {
            let value = match v.discriminant.as_ref() {
                Some((_, expr)) => parse_discriminant(expr)?,
                None if is_rich => next_value,
                None => {
                    return Err(syn::Error::new(
                        v.span(),
                        format!(
                            "enum `{name}` variant `{}` must have an explicit discriminant",
                            v.ident
                        ),
                    ))
                }
            };
            next_value = value.wrapping_add(1);

            let fields = match &v.fields {
                syn::Fields::Unit => vec![],
                syn::Fields::Named(named) => extract_variant_fields(named)?,
                syn::Fields::Unnamed(_) => {
                    return Err(syn::Error::new(
                        v.span(),
                        format!(
                            "enum `{name}` variant `{}`: tuple-style variants are not supported; \
                             use named fields",
                            v.ident
                        ),
                    ))
                }
            };

            Ok(EnumVariant {
                name: v.ident.to_string(),
                value,
                doc: extract_doc(&v.attrs),
                fields,
            })
        })
        .collect::<syn::Result<_>>()?;
    Ok(EnumDef {
        name,
        doc: extract_doc(&item.attrs),
        deprecated: parse_deprecated(&item.attrs).1,
        variants,
    })
}

/// Extract a `#[weaveffi::error]` enum into the module's [`ErrorDomain`].
///
/// Every variant needs an explicit integer discriminant: the code's stable
/// ABI value. A variant may be a unit variant or carry named fields, which
/// become the code's structured payload (serialized in the value-buffer
/// format alongside the `(code, message)` pair). Note that Rust requires a
/// primitive representation such as `#[repr(i32)]` on an enum that mixes
/// explicit discriminants with data-carrying variants. A variant's doc
/// comment becomes the code's default message (falling back to the variant
/// name). The enum's name is the domain name, and the macro generates a
/// matching `ErrorReport` implementation so `Err(KvError::...)` reports its
/// declared code and payload.
fn extract_error_domain(item: &syn::ItemEnum) -> syn::Result<ErrorDomain> {
    let codes = item
        .variants
        .iter()
        .map(|v| {
            let fields = match &v.fields {
                syn::Fields::Unit => vec![],
                syn::Fields::Named(named) => extract_variant_fields(named)?,
                syn::Fields::Unnamed(_) => {
                    return Err(syn::Error::new(
                        v.span(),
                        "weaveffi: #[weaveffi::error] payload variants must use named \
                         fields (tuple-style variants are not supported)",
                    ));
                }
            };
            let Some((_, expr)) = v.discriminant.as_ref() else {
                return Err(syn::Error::new(
                    v.span(),
                    format!(
                        "weaveffi: error variant `{}` must have an explicit discriminant \
                         (its stable ABI error code)",
                        v.ident
                    ),
                ));
            };
            let doc = extract_doc(&v.attrs);
            let message = doc
                .as_deref()
                .and_then(|d| d.lines().next())
                .unwrap_or(&v.ident.to_string())
                .to_string();
            Ok(ErrorCode {
                name: v.ident.to_string(),
                code: parse_discriminant(expr)?,
                message,
                doc,
                fields,
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;
    Ok(ErrorDomain {
        name: item.ident.to_string(),
        codes,
    })
}

/// Extract a single [`Module`] from a `#[weaveffi::module]`-annotated `mod`.
///
/// Only items carrying a WeaveFFI marker are exported; everything else (private
/// helpers, `use` items, free functions without `#[weaveffi::export]`) is
/// ignored, so a module can freely mix exported surface and implementation.
///
/// # Errors
///
/// Returns a spanned error when an annotated item cannot be mapped to the IR
/// (an unsupported type, an enum without `#[repr(i32)]`, a callback interface
/// method without `&self`, and so on).
pub fn module_from_item_mod(item_mod: &syn::ItemMod) -> syn::Result<Module> {
    let name = item_mod.ident.to_string();
    let mut functions = Vec::new();
    let mut interfaces: Vec<InterfaceDef> = Vec::new();
    let mut structs = Vec::new();
    let mut enums = Vec::new();
    let mut callback_interfaces = Vec::new();
    let mut modules = Vec::new();
    let mut errors: Option<ErrorDomain> = None;

    if let Some((_, items)) = &item_mod.content {
        for item in items {
            match item {
                syn::Item::Trait(t) if has_marker(&t.attrs, "callback_interface") => {
                    callback_interfaces.push(extract_callback_interface(t)?);
                }
                syn::Item::Fn(f) if has_marker(&f.attrs, "export") => {
                    functions.push(extract_function(f)?);
                }
                syn::Item::Struct(s) if has_marker(&s.attrs, "interface") => {
                    interfaces.push(InterfaceDef {
                        name: s.ident.to_string(),
                        doc: extract_doc(&s.attrs),
                        deprecated: parse_deprecated(&s.attrs).1,
                        constructors: vec![],
                        methods: vec![],
                        statics: vec![],
                    });
                }
                syn::Item::Struct(s) if has_marker(&s.attrs, "record") => {
                    structs.push(extract_struct(s)?);
                }
                syn::Item::Enum(e) if has_marker(&e.attrs, "error") => {
                    if errors.is_some() {
                        return Err(syn::Error::new(
                            e.span(),
                            "weaveffi: a module may declare at most one #[weaveffi::error] \
                             domain",
                        ));
                    }
                    errors = Some(extract_error_domain(e)?);
                }
                syn::Item::Enum(e) if has_marker(&e.attrs, "enumeration") => {
                    enums.push(extract_enum(e)?);
                }
                syn::Item::Mod(m) if has_marker(&m.attrs, "module") && m.content.is_some() => {
                    modules.push(module_from_item_mod(m)?);
                }
                _ => {}
            }
        }

        // Second pass: attach `impl` block members to their interfaces. The
        // interface struct may be declared after its impl block, so members
        // are collected only once every interface name is known.
        for item in items {
            let syn::Item::Impl(item_impl) = item else {
                continue;
            };
            if item_impl.trait_.is_some() {
                continue;
            }
            let Some(self_name) = type_path_ident(&item_impl.self_ty) else {
                continue;
            };
            if let Some(iface) = interfaces.iter_mut().find(|i| i.name == self_name) {
                members_from_impl(item_impl, iface)?;
            }
        }
    }

    Ok(Module {
        name,
        doc: extract_doc(&item_mod.attrs),
        functions,
        interfaces,
        structs,
        enums,
        callback_interfaces,
        errors,
        modules,
    })
}

/// Extract a full [`Api`] from a parsed Rust [`syn::File`], collecting every
/// top-level `#[weaveffi::module]`.
///
/// # Errors
///
/// Propagates any error from [`module_from_item_mod`].
pub fn api_from_file(file: &syn::File) -> syn::Result<Api> {
    let mut modules = Vec::new();
    for item in &file.items {
        if let syn::Item::Mod(item_mod) = item {
            if has_marker(&item_mod.attrs, "module") && item_mod.content.is_some() {
                modules.push(module_from_item_mod(item_mod)?);
            }
        }
    }
    Ok(Api {
        version: CURRENT_SCHEMA_VERSION.to_string(),
        modules,
    })
}

/// Parse Rust source text and extract its [`Api`].
///
/// # Errors
///
/// Returns a (line/column aware) error when the source does not parse as Rust,
/// or when an annotated item cannot be mapped to the IR.
pub fn api_from_src(src: &str) -> syn::Result<Api> {
    let file = syn::parse_file(src)?;
    api_from_file(&file)
}

/// A convenience wrapper around [`api_from_src`] that renders any error to a
/// plain string, for non-macro callers (e.g. the CLI).
///
/// # Errors
///
/// Returns the formatted error message when extraction fails.
pub fn api_from_src_stringly(src: &str) -> Result<Api, String> {
    api_from_src(src).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_module(src: &str) -> Module {
        let api = api_from_src(src).unwrap();
        assert_eq!(api.modules.len(), 1, "expected exactly one module");
        api.modules.into_iter().next().unwrap()
    }

    #[test]
    fn extracts_exported_function() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod math {
                #[weaveffi::export]
                pub fn add(a: i32, b: i32) -> i32 { a + b }
            }
        "#,
        );
        assert_eq!(m.name, "math");
        assert_eq!(m.functions.len(), 1);
        let f = &m.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.params[0].ty, TypeRef::I32);
        assert_eq!(f.returns, Some(TypeRef::I32));
        assert!(!f.r#async);
    }

    #[test]
    fn unmarked_module_is_ignored() {
        let api = api_from_src(
            r#"
            mod plain {
                #[weaveffi::export]
                pub fn add(a: i32) -> i32 { a }
            }
        "#,
        )
        .unwrap();
        assert!(api.modules.is_empty());
    }

    #[test]
    fn result_return_peels_to_ok_type() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                pub fn get(id: u64) -> Result<Contact, MyError> { todo!() }
            }
        "#,
        );
        assert_eq!(
            m.functions[0].returns,
            Some(TypeRef::Named("Contact".into()))
        );
        assert_eq!(m.functions[0].params[0].ty, TypeRef::U64);
    }

    #[test]
    fn result_unit_return_is_none() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                pub fn run() -> Result<(), MyError> { Ok(()) }
            }
        "#,
        );
        assert_eq!(m.functions[0].returns, None);
    }

    #[test]
    fn async_fn_is_async() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                pub async fn fetch(url: String) -> String { url }
            }
        "#,
        );
        assert!(m.functions[0].r#async);
    }

    #[test]
    fn record_and_enumeration() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod contacts {
                #[weaveffi::enumeration]
                #[repr(i32)]
                pub enum ContactType { Personal = 0, Work = 1 }

                #[weaveffi::record]
                pub struct Contact {
                    pub id: i64,
                    pub email: Option<String>,
                    pub kind: ContactType,
                }
            }
        "#,
        );
        assert_eq!(m.enums.len(), 1);
        assert_eq!(m.enums[0].variants.len(), 2);
        assert_eq!(m.structs.len(), 1);
        let s = &m.structs[0];
        assert_eq!(
            s.fields[1].ty,
            TypeRef::Optional(Box::new(TypeRef::StringUtf8))
        );
        assert_eq!(s.fields[2].ty, TypeRef::Named("ContactType".into()));
    }

    #[test]
    fn enumeration_requires_repr_i32() {
        let err = api_from_src(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::enumeration]
                pub enum Bad { A = 0 }
            }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("repr(i32)"));
    }

    #[test]
    fn references_and_arcs_name_the_pointee() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                pub fn open(name: &str, data: &[u8], tags: &[String]) -> Arc<Store> { todo!() }
                #[weaveffi::export]
                pub fn peek(store: &Store, other: Option<Arc<Store>>) -> Option<Arc<Store>> { None }
            }
        "#,
        );
        let open = &m.functions[0];
        assert_eq!(open.params[0].ty, TypeRef::StringUtf8);
        assert_eq!(open.params[1].ty, TypeRef::Bytes);
        assert_eq!(
            open.params[2].ty,
            TypeRef::List(Box::new(TypeRef::StringUtf8))
        );
        assert_eq!(open.returns, Some(TypeRef::Named("Store".into())));
        let peek = &m.functions[1];
        assert_eq!(peek.params[0].ty, TypeRef::Named("Store".into()));
        let opt_store = TypeRef::Optional(Box::new(TypeRef::Named("Store".into())));
        assert_eq!(peek.params[1].ty, opt_store);
        assert_eq!(peek.returns, Some(opt_store));
    }

    #[test]
    fn raw_pointers_and_boxes_are_rejected() {
        let err = api_from_src(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                pub fn close(token: *mut Token) {}
            }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("raw pointers"));
        let err = api_from_src(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                pub fn listen(l: Box<dyn Listener>) {}
            }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Arc<dyn Trait>"));
        let err = api_from_src(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                pub fn bump(counter: &mut i32) {}
            }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("&mut"));
    }

    #[test]
    fn cancel_token_param_is_skipped() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::export]
                #[weaveffi::cancellable]
                pub async fn compact(store: Arc<Store>, cancel: weaveffi::CancelToken) -> i64 { 0 }
            }
        "#,
        );
        let f = &m.functions[0];
        assert!(f.r#async);
        assert!(f.cancellable);
        // Only `store` survives; the `CancelToken` is dropped from the IDL.
        assert_eq!(f.params.len(), 1);
        assert_eq!(f.params[0].name, "store");
        assert_eq!(f.params[0].ty, TypeRef::Named("Store".into()));
    }

    #[test]
    fn interface_members_and_receivers() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::interface]
                pub struct Store;
                impl Store {
                    pub fn open(path: String) -> Result<Arc<Self>, StoreError> { todo!() }
                    pub fn new() -> Self { Store }
                    pub fn len(&self) -> u64 { 0 }
                    pub fn share(self: Arc<Self>) -> Arc<Self> { self }
                    pub fn default_path() -> String { String::new() }
                    fn private(&self) {}
                }
            }
        "#,
        );
        let s = &m.interfaces[0];
        assert_eq!(s.constructors.len(), 2);
        assert!(s.constructors[0].throws);
        assert_eq!(s.constructors[0].returns, None);
        assert_eq!(s.methods.len(), 2);
        assert_eq!(s.methods[1].returns, Some(TypeRef::Named("Store".into())));
        assert_eq!(s.statics.len(), 1);
    }

    #[test]
    fn mut_self_receiver_is_rejected() {
        let err = api_from_src(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::interface]
                pub struct Store;
                impl Store {
                    pub fn bump(&mut self) {}
                }
            }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("&self"));
    }

    #[test]
    fn callback_interface_trait() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                /// Receives messages.
                #[weaveffi::callback_interface]
                pub trait Listener: Send + Sync {
                    fn on_message(&self, text: String, meta: Option<Meta>);
                    fn level(&self) -> i32;
                }
                #[weaveffi::export]
                pub fn subscribe(listener: Arc<dyn Listener>) {}
                #[weaveffi::export]
                pub fn subscribe_bounded(listener: Arc<dyn Listener + Send + Sync>) {}
            }
        "#,
        );
        assert_eq!(m.callback_interfaces.len(), 1);
        let cb = &m.callback_interfaces[0];
        assert_eq!(cb.name, "Listener");
        assert_eq!(cb.doc.as_deref(), Some("Receives messages."));
        assert_eq!(cb.methods.len(), 2);
        assert_eq!(cb.methods[0].params.len(), 2);
        assert_eq!(cb.methods[1].returns, Some(TypeRef::I32));
        assert_eq!(
            m.functions[0].params[0].ty,
            TypeRef::Named("Listener".into())
        );
        assert_eq!(
            m.functions[1].params[0].ty,
            TypeRef::Named("Listener".into())
        );
    }

    #[test]
    fn callback_interface_methods_need_self() {
        let err = api_from_src(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::callback_interface]
                pub trait Listener {
                    fn make() -> i32;
                }
            }
        "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("&self"));
    }

    #[test]
    fn nested_modules() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod outer {
                #[weaveffi::export]
                pub fn top() -> i32 { 0 }
                #[weaveffi::module]
                mod inner {
                    #[weaveffi::export]
                    pub fn deep(x: bool) -> bool { x }
                }
            }
        "#,
        );
        assert_eq!(m.modules.len(), 1);
        assert_eq!(m.modules[0].name, "inner");
        assert_eq!(m.modules[0].functions[0].name, "deep");
    }
}
