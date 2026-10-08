//! Extract the IR of a `#[weaveffi::module]` tree from its Rust syntax.
//!
//! This is the `#[weaveffi::module]` proc-macro's reader: it lowers a module
//! tree to its IR ([`extract_module`]), which the macro validates, builds
//! the C ABI scaffolding from, and embeds in the library as metadata (see
//! [`meta`](crate::meta)). The CLI never reads Rust source; it reads that
//! metadata back out of the built library, so the API it generates bindings
//! for is, by construction, the one the library was compiled with.
//!
//! The extraction reports only what syntax alone rules out (a raw pointer,
//! a tuple variant, a `#[cfg]` on a member); every other rule is the
//! validator's. Alongside the IR, [`extract_module`] returns a [`SourceMap`]
//! recording where each declaration came from and the `#[cfg]` attributes on
//! it, so the macro can point a validation error at the offending item and
//! apply each item's `#[cfg]` to its generated code.
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
//!   `Arc<dyn Trait>` (or `Option<Arc<dyn Trait>>`). Every method returns
//!   `Result<T, weaveffi::ForeignError>`, which maps to a `T` return; a
//!   method marked `#[weaveffi::throws]` throws.
//! * `pub type Name = T;` in the tree is an alias: every use of `Name` is
//!   extracted as `T`.
//!
//! The type mapping mirrors the IDL: `String` (or `&str`) is a string, `Vec<u8>`
//! (or `&[u8]`) is a byte buffer, every integer primitive (including `u64`) is
//! the matching scalar, `Vec<T>`, `Option<T>`, and `HashMap`/`BTreeMap` are the
//! list, optional, and map types, `weaveffi::Iter<T>` is an `iter<T>` return,
//! `Arc<T>` names the interface `T` (an object reference), `Arc<dyn Trait>`
//! names the callback interface `Trait`, and any other named path is a record,
//! enum, or interface resolved later.

use std::collections::{BTreeMap, HashMap};

use crate::ir::{
    CallbackInterfaceDef, EnumDef, EnumVariant, ErrorCode, ErrorDomain, Function, InterfaceDef,
    Module, Param, StructDef, StructField, TypeRef,
};
use crate::ty::Prim;
use proc_macro2::Span;
use syn::spanned::Spanned;

/// Where each extracted declaration came from, keyed by its declaration
/// path: module segments from the tree's root, then the declaration name,
/// then a member name (an interface's constructor, method, or static; a
/// callback interface's method; an enum's variant; an error domain's code),
/// then a parameter or field name. These are the paths validation reports
/// its errors under.
///
/// A parameter's or field's path extended with [`TYPE`](Self::TYPE) locates
/// its written type, and a callable's path extended with
/// [`RETURN`](Self::RETURN) its written return type.
#[derive(Clone, Default)]
pub struct SourceMap {
    spans: BTreeMap<Vec<String>, Vec<Span>>,
    cfgs: BTreeMap<Vec<String>, Vec<syn::Attribute>>,
}

impl std::fmt::Debug for SourceMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceMap")
            .field("paths", &self.spans.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl SourceMap {
    /// The key segment that, appended to a parameter's or field's path,
    /// names its written type.
    pub const TYPE: &'static str = "#type";
    /// The key segment that, appended to a callable's path, names its
    /// written return type.
    pub const RETURN: &'static str = "#return";

    fn record(&mut self, path: Vec<String>, span: Span) {
        self.spans.entry(path).or_default().push(span);
    }

    fn record_cfg(&mut self, path: Vec<String>, attrs: &[syn::Attribute]) {
        let cfg: Vec<syn::Attribute> = attrs.iter().filter(|a| is_cfg(a)).cloned().collect();
        if !cfg.is_empty() {
            self.cfgs.entry(path).or_default().extend(cfg);
        }
    }

    /// Every span recorded for `path`, in source order (a duplicated name
    /// has several).
    #[must_use]
    pub fn spans(&self, path: &[String]) -> &[Span] {
        self.spans.get(path).map_or(&[], Vec::as_slice)
    }

    /// The `#[cfg]` attributes written on the declaration at `path` itself
    /// (for an interface member, on its `impl` block), not including its
    /// enclosing modules' or interface's.
    #[must_use]
    pub fn cfg(&self, path: &[String]) -> &[syn::Attribute] {
        self.cfgs.get(path).map_or(&[], Vec::as_slice)
    }
}

/// Whether `attr` is a `#[cfg(...)]`.
fn is_cfg(attr: &syn::Attribute) -> bool {
    attr.path().is_ident("cfg")
}

/// Reject a `#[cfg]` on a member of an exported item.
fn no_member_cfg(attrs: &[syn::Attribute], what: &str, instead: &str) -> syn::Result<()> {
    match attrs.iter().find(|a| is_cfg(a)) {
        Some(attr) => Err(syn::Error::new(
            attr.span(),
            format!(
                "weaveffi: `#[cfg]` on {what} isn't supported, because the generated bindings \
                 can't follow it; {instead}"
            ),
        )),
        None => Ok(()),
    }
}

/// The type aliases declared in a module tree (`pub type Id = u64;`), by
/// name. Generic aliases are left alone.
type Aliases = HashMap<String, syn::Type>;

fn collect_aliases(item_mod: &syn::ItemMod, out: &mut Aliases) {
    let Some((_, items)) = &item_mod.content else {
        return;
    };
    for item in items {
        match item {
            syn::Item::Type(t) if t.generics.params.is_empty() => {
                out.insert(t.ident.to_string(), (*t.ty).clone());
            }
            syn::Item::Mod(m) if has_marker(&m.attrs, "module") => collect_aliases(m, out),
            _ => {}
        }
    }
}

/// The extraction context: the tree's aliases and the source map being
/// built.
struct Extractor<'a> {
    aliases: &'a Aliases,
    map: SourceMap,
}

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

/// The doc comment of an item, one line per `///` line with the leading
/// space removed, and with rustdoc intra-doc links unwrapped to the inline
/// code they display (see [`unwrap_doc_links`]), since no binding resolves
/// Rust paths.
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
    if lines.is_empty() {
        return None;
    }
    let mut fenced = false;
    let lines: Vec<String> = lines
        .into_iter()
        .map(|line| {
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                line
            } else if fenced {
                line
            } else {
                unwrap_doc_links(&line)
            }
        })
        .collect();
    Some(lines.join("\n"))
}

/// Unwrap every rustdoc intra-doc link written as inline code in brackets
/// (`` [`Store`] ``, `` [`KvError::KeyNotFound`] ``) to the inline code
/// alone. A bracketed span followed by `(`, `[`, or `:` is an ordinary
/// Markdown link or link definition, and stays as written.
fn unwrap_doc_links(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("[`") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('`') else {
            break;
        };
        let code = &after[..end];
        let tail = &after[end + 1..];
        let link = tail.starts_with(']')
            && !code.is_empty()
            && !matches!(tail[1..].chars().next(), Some('(' | '[' | ':'));
        if link {
            out.push_str(&rest[..start]);
            out.push('`');
            out.push_str(code);
            out.push('`');
            rest = &tail[1..];
        } else {
            out.push_str(&rest[..start + 2]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
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

/// Map a Rust [`syn::Type`] onto the WeaveFFI [`TypeRef`] it represents,
/// substituting the tree's `aliases`.
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
fn type_ref_from_syn(ty: &syn::Type, aliases: &Aliases) -> syn::Result<TypeRef> {
    type_ref_at(ty, aliases, 0)
}

fn type_ref_at(ty: &syn::Type, aliases: &Aliases, depth: usize) -> syn::Result<TypeRef> {
    let recurse = |t: &syn::Type| type_ref_at(t, aliases, depth);
    match ty {
        syn::Type::Reference(r) => {
            if let syn::Type::Path(p) = r.elem.as_ref() {
                if p.path.is_ident("str") {
                    return Ok(TypeRef::Prim(Prim::String));
                }
            }
            if let syn::Type::Slice(slice) = r.elem.as_ref() {
                if is_ident(&slice.elem, "u8") {
                    return Ok(TypeRef::Prim(Prim::Bytes));
                }
                return Ok(TypeRef::List(Box::new(recurse(&slice.elem)?)));
            }
            recurse(&r.elem)
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
            // A Rust scalar (`i32`, `bool`, ...) is the primitive of the same
            // name; `string` and `bytes` are IDL spellings, not Rust types.
            if let Some(p) =
                Prim::from_name(&ident).filter(|p| !matches!(p, Prim::String | Prim::Bytes))
            {
                return Ok(TypeRef::Prim(p));
            }
            match ident.as_str() {
                "String" => Ok(TypeRef::Prim(Prim::String)),
                // `Arc<T>` is a reference to the interface object `T`, and
                // `Arc<dyn Trait>` a consumer-implemented callback interface;
                // the IR names the pointee in both cases.
                "Arc" => recurse(single_generic_arg(seg)?),
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
                        return Ok(TypeRef::Prim(Prim::Bytes));
                    }
                    Ok(TypeRef::List(Box::new(recurse(inner)?)))
                }
                "Option" => {
                    let inner = single_generic_arg(seg)?;
                    Ok(TypeRef::Optional(Box::new(recurse(inner)?)))
                }
                // `weaveffi::Iter<T>` is the producer spelling of an `iter<T>`
                // return: a lazily-pulled stream rather than a materialized list.
                "Iter" => {
                    let inner = single_generic_arg(seg)?;
                    Ok(TypeRef::Iterator(Box::new(recurse(inner)?)))
                }
                "HashMap" | "BTreeMap" => {
                    let (k, v) = two_generic_args(seg)?;
                    Ok(TypeRef::Map(Box::new(recurse(k)?), Box::new(recurse(v)?)))
                }
                other => match aliases.get(other) {
                    Some(target) if matches!(seg.arguments, syn::PathArguments::None) => {
                        if depth > 16 {
                            return Err(syn::Error::new(
                                ty.span(),
                                format!("weaveffi: type alias `{other}` refers to itself"),
                            ));
                        }
                        type_ref_at(target, aliases, depth + 1)
                    }
                    _ => Ok(TypeRef::Named(other.to_string())),
                },
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

/// `path` extended with `names`.
fn at(path: &[String], names: &[&str]) -> Vec<String> {
    let mut out = path.to_vec();
    out.extend(names.iter().map(|n| (*n).to_string()));
    out
}

/// Whether a return type is `Result<T, ForeignError>`, the return type every
/// callback-interface method must have.
pub fn returns_foreign_result(output: &syn::ReturnType) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    let syn::Type::Path(p) = ty.as_ref() else {
        return false;
    };
    let Some(seg) = p.path.segments.last() else {
        return false;
    };
    if seg.ident != "Result" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return false;
    };
    match args.args.iter().nth(1) {
        Some(syn::GenericArgument::Type(err)) => {
            type_path_ident(err).as_deref() == Some("ForeignError")
        }
        _ => false,
    }
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

/// Whether the (peeled) return type names the interface itself (`Self`,
/// `Arc<Self>`, or the interface's own name), which classifies a
/// synchronous associated function as a constructor.
fn returns_self(output: &syn::ReturnType, iface: &str) -> bool {
    let syn::ReturnType::Type(_, ty) = output else {
        return false;
    };
    match type_path_ident(peel_arc(peel_result(ty))) {
        Some(name) => name == "Self" || name == iface,
        None => false,
    }
}

impl Extractor<'_> {
    fn ty(&self, ty: &syn::Type) -> syn::Result<TypeRef> {
        type_ref_from_syn(ty, self.aliases)
    }

    /// Map a function's return type to its IDL return [`TypeRef`], peeling
    /// `Result<T, E>` and treating `()` (and `Result<(), E>`) as no return.
    fn return_type(&self, output: &syn::ReturnType) -> syn::Result<Option<TypeRef>> {
        match output {
            syn::ReturnType::Default => Ok(None),
            syn::ReturnType::Type(_, ty) => {
                let inner = peel_result(ty);
                if is_unit(inner) {
                    Ok(None)
                } else {
                    Ok(Some(self.ty(inner)?))
                }
            }
        }
    }

    fn params(
        &mut self,
        path: &[String],
        inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>,
    ) -> syn::Result<Vec<Param>> {
        let mut out = Vec::new();
        for pt in inputs.iter().filter_map(|arg| match arg {
            syn::FnArg::Typed(pt) => Some(pt),
            syn::FnArg::Receiver(_) => None,
        }) {
            // The cancellation token is part of the async calling
            // convention, not a logical parameter, so it never appears in
            // the IDL.
            if is_cancel_token(&pt.ty) {
                continue;
            }
            let name = match pt.pat.as_ref() {
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
            self.map.record(at(path, &[&name]), pt.pat.span());
            self.map
                .record(at(path, &[&name, SourceMap::TYPE]), pt.ty.span());
            out.push(Param {
                ty: self.ty(&pt.ty)?,
                name,
                doc: extract_doc(&pt.attrs),
            });
        }
        Ok(out)
    }

    /// Map one `fn` signature (free function, interface member, or callback
    /// method) declared at `path` to the IR [`Function`]. `returns` is
    /// supplied by the caller because constructors, `Self` returns, and
    /// callback methods need their own handling.
    fn function(
        &mut self,
        path: &[String],
        sig: &syn::Signature,
        attrs: &[syn::Attribute],
        returns: Option<TypeRef>,
    ) -> syn::Result<Function> {
        let ret_span = match &sig.output {
            syn::ReturnType::Type(_, ty) => ty.span(),
            syn::ReturnType::Default => sig.ident.span(),
        };
        self.map.record(at(path, &[SourceMap::RETURN]), ret_span);
        Ok(Function {
            name: sig.ident.to_string(),
            params: self.params(path, &sig.inputs)?,
            returns,
            doc: extract_doc(attrs),
            throws: output_is_result(&sig.output),
            r#async: sig.asyncness.is_some(),
            cancellable: has_marker(attrs, "cancellable"),
            deprecated: parse_deprecated(attrs).1,
        })
    }

    /// Extract one interface member from an `impl` block function. A `Self`
    /// (or interface-named) return on a constructor is dropped: the IR
    /// leaves a constructor's `return` empty because the instance is
    /// implicit.
    fn member(
        &mut self,
        path: &[String],
        item: &syn::ImplItemFn,
        iface: &str,
        is_ctor: bool,
    ) -> syn::Result<Function> {
        let returns = if is_ctor {
            None
        } else {
            match self.return_type(&item.sig.output)? {
                // `fn hand(&self) -> Arc<Self>` style returns name the
                // interface.
                Some(TypeRef::Named(name)) if name == "Self" => {
                    Some(TypeRef::Named(iface.to_string()))
                }
                other => other,
            }
        };
        let member = at(path, &[&item.sig.ident.to_string()]);
        self.map.record(member.clone(), item.sig.ident.span());
        self.function(&member, &item.sig, &item.attrs, returns)
    }

    /// Classify and extract every `pub fn` of an interface's `impl` block
    /// into the interface's constructors, methods, and statics:
    ///
    /// * a function with a `&self` or `self: Arc<Self>` receiver is a
    ///   **method**;
    /// * a synchronous associated function returning `Self` or `Arc<Self>`
    ///   (or the interface type, optionally inside `Result`) is a
    ///   **constructor**;
    /// * any other associated function is a **static**, including an
    ///   `async fn` returning the interface (constructors are synchronous,
    ///   so that's an async factory).
    ///
    /// Non-`pub` items are private helpers and stay unexported. The block's
    /// `#[cfg]` is recorded on each member it declares.
    fn members(
        &mut self,
        path: &[String],
        item_impl: &syn::ItemImpl,
        iface: &mut InterfaceDef,
    ) -> syn::Result<()> {
        for impl_item in &item_impl.items {
            let syn::ImplItem::Fn(f) = impl_item else {
                continue;
            };
            if !matches!(f.vis, syn::Visibility::Public(_)) {
                continue;
            }
            no_member_cfg(
                &f.attrs,
                "an interface member",
                "move the member into its own `impl` block and put the `#[cfg]` on that block",
            )?;
            let member = at(path, &[&f.sig.ident.to_string()]);
            self.map.record_cfg(member, &item_impl.attrs);
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
                    let m = self.member(path, f, &iface.name, false)?;
                    iface.methods.push(m);
                }
                None if f.sig.asyncness.is_none() && returns_self(&f.sig.output, &iface.name) => {
                    let m = self.member(path, f, &iface.name, true)?;
                    iface.constructors.push(m);
                }
                None => {
                    let m = self.member(path, f, &iface.name, false)?;
                    iface.statics.push(m);
                }
            }
        }
        Ok(())
    }

    /// Extract a `#[weaveffi::callback_interface]` trait into a
    /// [`CallbackInterfaceDef`].
    ///
    /// Every trait method is a callback method the consumer implements. A
    /// method takes `&self` and returns `Result<T, weaveffi::ForeignError>`,
    /// which maps to a `T` return; `#[weaveffi::throws]` marks it throwing.
    fn callback_interface(
        &mut self,
        path: &[String],
        item: &syn::ItemTrait,
    ) -> syn::Result<CallbackInterfaceDef> {
        let mut methods = Vec::new();
        for trait_item in &item.items {
            let syn::TraitItem::Fn(f) = trait_item else {
                continue;
            };
            no_member_cfg(
                &f.attrs,
                "a callback interface method",
                "put the `#[cfg]` on the whole trait",
            )?;
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
            if !returns_foreign_result(&f.sig.output) {
                let span = match &f.sig.output {
                    syn::ReturnType::Type(_, ty) => ty.span(),
                    syn::ReturnType::Default => f.sig.ident.span(),
                };
                return Err(syn::Error::new(
                    span,
                    "weaveffi: a callback interface method must return \
                     `Result<T, weaveffi::ForeignError>` (or `Result<(), weaveffi::ForeignError>` \
                     for no value), because the consumer's implementation can fail and the \
                     `Err` is how its failure reaches you",
                ));
            }
            let returns = self.return_type(&f.sig.output)?;
            let method_path = at(path, &[&f.sig.ident.to_string()]);
            self.map.record(method_path.clone(), f.sig.ident.span());
            let mut method = self.function(&method_path, &f.sig, &f.attrs, returns)?;
            method.throws = has_marker(&f.attrs, "throws");
            methods.push(method);
        }
        Ok(CallbackInterfaceDef {
            name: item.ident.to_string(),
            doc: extract_doc(&item.attrs),
            deprecated: parse_deprecated(&item.attrs).1,
            methods,
        })
    }

    /// The named fields of a record, a rich enum variant, or an error code,
    /// declared under `path`.
    fn fields(
        &mut self,
        path: &[String],
        fields: &syn::FieldsNamed,
        what: &str,
    ) -> syn::Result<Vec<StructField>> {
        let mut out = Vec::new();
        for f in &fields.named {
            no_member_cfg(&f.attrs, what, "put the `#[cfg]` on the whole type")?;
            let name = f
                .ident
                .as_ref()
                .ok_or_else(|| syn::Error::new(f.span(), "unnamed field"))?
                .to_string();
            self.map.record(at(path, &[&name]), f.span());
            self.map
                .record(at(path, &[&name, SourceMap::TYPE]), f.ty.span());
            out.push(StructField {
                ty: self.ty(&f.ty)?,
                name,
                doc: extract_doc(&f.attrs),
            });
        }
        Ok(out)
    }

    fn record(&mut self, path: &[String], item: &syn::ItemStruct) -> syn::Result<StructDef> {
        let syn::Fields::Named(named) = &item.fields else {
            return Err(syn::Error::new(
                item.span(),
                "only named fields are supported for #[weaveffi::record]",
            ));
        };
        Ok(StructDef {
            name: item.ident.to_string(),
            doc: extract_doc(&item.attrs),
            deprecated: parse_deprecated(&item.attrs).1,
            fields: self.fields(path, named, "a record field")?,
        })
    }

    fn enumeration(&mut self, path: &[String], item: &syn::ItemEnum) -> syn::Result<EnumDef> {
        let name = item.ident.to_string();

        // A *rich* (algebraic) enum has at least one variant carrying data.
        // Rust forbids explicit discriminants on such an enum, so its tags
        // are the declaration-order positions (0, 1, 2, ...), exactly what
        // the IDL records. A *C-style* enum (every variant fieldless) keeps
        // the stricter contract: it must be `#[repr(i32)]` with an explicit
        // discriminant on each variant.
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
        let mut variants = Vec::new();
        for v in &item.variants {
            no_member_cfg(
                &v.attrs,
                "an enum variant",
                "put the `#[cfg]` on the whole enum",
            )?;
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
            let variant = v.ident.to_string();
            let vpath = at(path, &[&variant]);
            self.map.record(vpath.clone(), v.ident.span());
            let fields = match &v.fields {
                syn::Fields::Unit => vec![],
                syn::Fields::Named(named) => self.fields(&vpath, named, "an enum variant field")?,
                syn::Fields::Unnamed(_) => {
                    return Err(syn::Error::new(
                        v.span(),
                        format!(
                            "enum `{name}` variant `{variant}`: tuple-style variants are not \
                             supported; use named fields"
                        ),
                    ))
                }
            };
            variants.push(EnumVariant {
                name: variant,
                value,
                doc: extract_doc(&v.attrs),
                fields,
            });
        }
        Ok(EnumDef {
            name,
            doc: extract_doc(&item.attrs),
            deprecated: parse_deprecated(&item.attrs).1,
            variants,
        })
    }

    /// Extract a `#[weaveffi::error]` enum into the module's
    /// [`ErrorDomain`].
    ///
    /// Every variant needs an explicit integer discriminant: the code's
    /// stable ABI value. A variant may be a unit variant or carry named
    /// fields, which become the code's structured payload (serialized in the
    /// value-buffer format alongside the `(code, message)` pair). Note that
    /// Rust requires a primitive representation such as `#[repr(i32)]` on an
    /// enum that mixes explicit discriminants with data-carrying variants. A
    /// variant's doc comment becomes the code's default message (falling
    /// back to the variant name). The enum's name is the domain name, and
    /// the macro generates matching `ErrorReport` and `ErrorDomain`
    /// implementations.
    fn error_domain(&mut self, path: &[String], item: &syn::ItemEnum) -> syn::Result<ErrorDomain> {
        let mut codes = Vec::new();
        for v in &item.variants {
            no_member_cfg(
                &v.attrs,
                "an error code",
                "put the `#[cfg]` on the whole error enum",
            )?;
            let code = v.ident.to_string();
            let cpath = at(path, &[&code]);
            self.map.record(cpath.clone(), v.ident.span());
            let fields = match &v.fields {
                syn::Fields::Unit => vec![],
                syn::Fields::Named(named) => self.fields(&cpath, named, "an error code field")?,
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
                        "weaveffi: error variant `{code}` must have an explicit discriminant \
                         (its stable ABI error code)"
                    ),
                ));
            };
            let doc = extract_doc(&v.attrs);
            let message = doc
                .as_deref()
                .and_then(|d| d.lines().next())
                .unwrap_or(&code)
                .to_string();
            codes.push(ErrorCode {
                code: parse_discriminant(expr)?,
                name: code,
                message,
                doc,
                fields,
            });
        }
        Ok(ErrorDomain {
            name: item.ident.to_string(),
            codes,
        })
    }

    /// Extract one `#[weaveffi::module]`, whose parent path is `parent`.
    fn module(&mut self, parent: &[String], item_mod: &syn::ItemMod) -> syn::Result<Module> {
        let name = item_mod.ident.to_string();
        let path = at(parent, &[&name]);
        self.map.record(path.clone(), item_mod.ident.span());
        self.map.record_cfg(path.clone(), &item_mod.attrs);
        let mut module = Module {
            name,
            doc: extract_doc(&item_mod.attrs),
            functions: Vec::new(),
            interfaces: Vec::new(),
            callback_interfaces: Vec::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            errors: None,
            modules: Vec::new(),
        };
        let Some((_, items)) = &item_mod.content else {
            return Err(syn::Error::new(
                item_mod.span(),
                "weaveffi: a #[weaveffi::module] must have an inline body (`mod name { ... }`); \
                 the macro can't read a module from another file",
            ));
        };
        // Records a declaration's span and `#[cfg]`.
        let declare = |map: &mut SourceMap, decl: &str, span: Span, attrs: &[syn::Attribute]| {
            let decl = at(&path, &[decl]);
            map.record(decl.clone(), span);
            map.record_cfg(decl, attrs);
        };
        for item in items {
            match item {
                syn::Item::Trait(t) if has_marker(&t.attrs, "callback_interface") => {
                    let decl = t.ident.to_string();
                    declare(&mut self.map, &decl, t.ident.span(), &t.attrs);
                    let cb = self.callback_interface(&at(&path, &[&decl]), t)?;
                    module.callback_interfaces.push(cb);
                }
                syn::Item::Fn(f) if has_marker(&f.attrs, "export") => {
                    let decl = f.sig.ident.to_string();
                    declare(&mut self.map, &decl, f.sig.ident.span(), &f.attrs);
                    let returns = self.return_type(&f.sig.output)?;
                    let func = self.function(&at(&path, &[&decl]), &f.sig, &f.attrs, returns)?;
                    module.functions.push(func);
                }
                syn::Item::Struct(s) if has_marker(&s.attrs, "interface") => {
                    let decl = s.ident.to_string();
                    declare(&mut self.map, &decl, s.ident.span(), &s.attrs);
                    module.interfaces.push(InterfaceDef {
                        name: decl,
                        doc: extract_doc(&s.attrs),
                        deprecated: parse_deprecated(&s.attrs).1,
                        constructors: vec![],
                        methods: vec![],
                        statics: vec![],
                    });
                }
                syn::Item::Struct(s) if has_marker(&s.attrs, "record") => {
                    let decl = s.ident.to_string();
                    declare(&mut self.map, &decl, s.ident.span(), &s.attrs);
                    let record = self.record(&at(&path, &[&decl]), s)?;
                    module.structs.push(record);
                }
                syn::Item::Enum(e) if has_marker(&e.attrs, "error") => {
                    if module.errors.is_some() {
                        return Err(syn::Error::new(
                            e.span(),
                            "weaveffi: a module may declare at most one #[weaveffi::error] \
                             domain",
                        ));
                    }
                    let decl = e.ident.to_string();
                    declare(&mut self.map, &decl, e.ident.span(), &e.attrs);
                    module.errors = Some(self.error_domain(&at(&path, &[&decl]), e)?);
                }
                syn::Item::Enum(e) if has_marker(&e.attrs, "enumeration") => {
                    let decl = e.ident.to_string();
                    declare(&mut self.map, &decl, e.ident.span(), &e.attrs);
                    let def = self.enumeration(&at(&path, &[&decl]), e)?;
                    module.enums.push(def);
                }
                syn::Item::Mod(m) if has_marker(&m.attrs, "module") => {
                    let child = self.module(&path, m)?;
                    module.modules.push(child);
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
            if let Some(i) = module.interfaces.iter().position(|i| i.name == self_name) {
                let mut iface = std::mem::replace(
                    &mut module.interfaces[i],
                    InterfaceDef {
                        name: String::new(),
                        doc: None,
                        deprecated: None,
                        constructors: vec![],
                        methods: vec![],
                        statics: vec![],
                    },
                );
                let result = self.members(&at(&path, &[&self_name]), item_impl, &mut iface);
                module.interfaces[i] = iface;
                result?;
            }
        }
        Ok(module)
    }
}

/// Extract the module tree rooted at a `#[weaveffi::module]`-annotated
/// `mod`, together with its [`SourceMap`].
///
/// Only items carrying a WeaveFFI marker are exported; everything else
/// (private helpers, `use` items, free functions without
/// `#[weaveffi::export]`) is ignored, so a module can freely mix exported
/// surface and implementation. Type aliases declared anywhere in the tree are
/// substituted.
///
/// # Errors
///
/// Returns a spanned error when an annotated item cannot be mapped to the IR
/// (an unsupported type, an enum without `#[repr(i32)]`, a callback interface
/// method that doesn't return `Result<T, weaveffi::ForeignError>`, a
/// `#[cfg]` on a member, an out-of-line submodule, and so on).
pub fn extract_module(item_mod: &syn::ItemMod) -> syn::Result<(Module, SourceMap)> {
    let mut aliases = Aliases::new();
    collect_aliases(item_mod, &mut aliases);
    let mut extractor = Extractor {
        aliases: &aliases,
        map: SourceMap::default(),
    };
    let module = extractor.module(&[], item_mod)?;
    Ok((module, extractor.map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Api, CURRENT_SCHEMA_VERSION};

    /// Extract every top-level `#[weaveffi::module]` in `src`.
    fn api_from_src(src: &str) -> syn::Result<Api> {
        let file = syn::parse_file(src)?;
        let mut modules = Vec::new();
        for item in &file.items {
            if let syn::Item::Mod(item_mod) = item {
                if has_marker(&item_mod.attrs, "module") {
                    modules.push(extract_module(item_mod)?.0);
                }
            }
        }
        Ok(Api {
            version: CURRENT_SCHEMA_VERSION.to_string(),
            modules,
        })
    }

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
        assert_eq!(f.params[0].ty, TypeRef::Prim(Prim::I32));
        assert_eq!(f.returns, Some(TypeRef::Prim(Prim::I32)));
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
        assert_eq!(m.functions[0].params[0].ty, TypeRef::Prim(Prim::U64));
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
            TypeRef::Optional(Box::new(TypeRef::Prim(Prim::String)))
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
        assert_eq!(open.params[0].ty, TypeRef::Prim(Prim::String));
        assert_eq!(open.params[1].ty, TypeRef::Prim(Prim::Bytes));
        assert_eq!(
            open.params[2].ty,
            TypeRef::List(Box::new(TypeRef::Prim(Prim::String)))
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
    fn doc_links_unwrap_to_inline_code() {
        assert_eq!(
            unwrap_doc_links("Fails with [`KvError::KeyNotFound`] for [`Store`]."),
            "Fails with `KvError::KeyNotFound` for `Store`."
        );
        assert_eq!(
            unwrap_doc_links("See [`docs`](https://x.dev) and [`a`][b]; [`c`]: d"),
            "See [`docs`](https://x.dev) and [`a`][b]; [`c`]: d"
        );
        assert_eq!(
            unwrap_doc_links("A `[string]`, [`]`, [x]."),
            "A `[string]`, [`]`, [x]."
        );
        assert_eq!(unwrap_doc_links("open [`"), "open [`");

        let m = one_module(
            r#"
            /// Uses [`Store`].
            #[weaveffi::module]
            mod m {
                /// Returns [`Store::size`] items.
                ///
                /// ```
                /// let v = [`x`];
                /// ```
                #[weaveffi::export]
                pub fn count() -> i32 { 0 }
            }
        "#,
        );
        assert_eq!(
            m.functions[0].doc.as_deref(),
            Some("Returns `Store::size` items.\n\n```\nlet v = [`x`];\n```")
        );
    }

    #[test]
    fn async_associated_functions_returning_the_interface_are_statics() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                #[weaveffi::interface]
                pub struct Store;
                impl Store {
                    pub async fn open_async(path: String) -> Result<Arc<Self>, StoreError> { todo!() }
                    pub async fn fresh() -> Self { Store }
                    pub async fn named() -> Store { Store }
                }
            }
        "#,
        );
        let s = &m.interfaces[0];
        assert!(s.constructors.is_empty());
        assert_eq!(s.statics.len(), 3);
        for f in &s.statics {
            assert!(f.r#async, "{} is async", f.name);
            assert_eq!(
                f.returns,
                Some(TypeRef::Named("Store".into())),
                "{}",
                f.name
            );
        }
        assert!(s.statics[0].throws);
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
                    fn on_message(&self, text: String, meta: Option<Meta>) -> Result<(), ForeignError>;
                    fn level(&self) -> Result<i32, weaveffi::ForeignError>;
                    #[weaveffi::throws]
                    fn label(&self) -> Result<String, ForeignError>;
                }
                #[weaveffi::export]
                pub fn subscribe(listener: Arc<dyn Listener>) {}
                #[weaveffi::export]
                pub fn subscribe_bounded(listener: Option<Arc<dyn Listener + Send + Sync>>) {}
            }
        "#,
        );
        assert_eq!(m.callback_interfaces.len(), 1);
        let cb = &m.callback_interfaces[0];
        assert_eq!(cb.name, "Listener");
        assert_eq!(cb.doc.as_deref(), Some("Receives messages."));
        let methods = &cb.methods;
        assert_eq!(methods[0].params.len(), 2);
        assert_eq!(methods[0].returns, None);
        assert!(!methods[0].throws);
        assert_eq!(methods[1].returns, Some(TypeRef::Prim(Prim::I32)));
        assert!(!methods[1].throws);
        assert_eq!(methods[2].returns, Some(TypeRef::Prim(Prim::String)));
        assert!(methods[2].throws);
        assert_eq!(
            m.functions[0].params[0].ty,
            TypeRef::Named("Listener".into())
        );
        assert_eq!(
            m.functions[1].params[0].ty,
            TypeRef::Optional(Box::new(TypeRef::Named("Listener".into())))
        );
    }

    #[test]
    fn callback_methods_must_return_foreign_results() {
        for ret in ["", "-> i32", "-> Result<i32, String>"] {
            let src = format!(
                "#[weaveffi::module] mod m {{ #[weaveffi::callback_interface] \
                 pub trait L {{ fn f(&self) {ret}; }} }}"
            );
            let err = api_from_src(&src).unwrap_err();
            assert!(err.to_string().contains("ForeignError"), "{ret}: {err}");
        }
    }

    #[test]
    fn aliases_are_substituted() {
        let m = one_module(
            r#"
            #[weaveffi::module]
            mod m {
                pub type Id = u64;
                pub type Ids = Vec<Id>;
                #[weaveffi::export]
                pub fn get(id: Id, all: Ids) -> Option<Id> { None }
                #[weaveffi::module]
                mod inner {
                    #[weaveffi::export]
                    pub fn deep(id: super::Id) {}
                }
            }
        "#,
        );
        let f = &m.functions[0];
        assert_eq!(f.params[0].ty, TypeRef::Prim(Prim::U64));
        assert_eq!(
            f.params[1].ty,
            TypeRef::List(Box::new(TypeRef::Prim(Prim::U64)))
        );
        assert_eq!(
            f.returns,
            Some(TypeRef::Optional(Box::new(TypeRef::Prim(Prim::U64))))
        );
        assert_eq!(
            m.modules[0].functions[0].params[0].ty,
            TypeRef::Prim(Prim::U64)
        );
    }

    #[test]
    fn member_cfgs_and_out_of_line_modules_are_rejected() {
        for (src, needle) in [
            (
                "#[weaveffi::module] mod m { #[weaveffi::record] pub struct R { #[cfg(unix)] pub x: i32 } }",
                "record field",
            ),
            (
                "#[weaveffi::module] mod m { #[weaveffi::interface] pub struct S; impl S { #[cfg(unix)] pub fn f(&self) {} } }",
                "interface member",
            ),
            (
                "#[weaveffi::module] mod m { #[weaveffi::enumeration] #[repr(i32)] pub enum E { #[cfg(unix)] A = 0 } }",
                "enum variant",
            ),
            (
                "#[weaveffi::module] mod m { #[weaveffi::module] mod inner; }",
                "inline body",
            ),
        ] {
            let err = api_from_src(src).unwrap_err().to_string();
            assert!(err.contains(needle), "{src}: {err}");
        }
    }

    #[test]
    fn the_source_map_records_spans_and_item_cfgs() {
        let file: syn::File = syn::parse_str(
            r#"
            #[weaveffi::module]
            mod m {
                #[cfg(unix)]
                #[weaveffi::export]
                pub fn f(x: usize) {}
                #[weaveffi::interface]
                pub struct S;
                #[cfg(feature = "s")]
                impl S {
                    pub fn get(&self) -> i32 { 0 }
                }
            }
        "#,
        )
        .unwrap();
        let syn::Item::Mod(item) = &file.items[0] else {
            panic!("a module")
        };
        let (_, map) = extract_module(item).unwrap();
        let path = |p: &[&str]| p.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(map.spans(&path(&["m", "f"])).len(), 1);
        assert_eq!(map.spans(&path(&["m", "f", "x", SourceMap::TYPE])).len(), 1);
        assert_eq!(map.cfg(&path(&["m", "f"])).len(), 1);
        assert_eq!(map.cfg(&path(&["m", "S", "get"])).len(), 1);
        assert!(map.cfg(&path(&["m", "S"])).is_empty());
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
