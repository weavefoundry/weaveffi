//! What every attribute's expansion shares: the crate's prefix and tag, doc
//! and deprecation attributes, source locations, and the metadata frame and
//! contract function each declaration exports.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use weaveffi_model::contract::{Part, Sig};
use weaveffi_model::ir::{Throws, TypeRef};
use weaveffi_model::meta::{Frame, Item, Location, SECTION};

/// The C symbol prefix of the crate being expanded: its crate name, which is
/// also the prefix the CLI gives the crate's identity.
pub(crate) fn prefix() -> syn::Result<String> {
    std::env::var("CARGO_CRATE_NAME").map_err(|_| {
        syn::Error::new(
            Span::call_site(),
            "weaveffi: CARGO_CRATE_NAME is not set; WeaveFFI names every C symbol after the \
             crate, so build the crate with cargo",
        )
    })
}

/// The crate's tag type, which `weaveffi::export_runtime!()` defines at the
/// crate root.
pub(crate) fn tag() -> TokenStream {
    quote!(crate::__weaveffi::Tag)
}

/// An identifier from a string.
pub(crate) fn ident(name: &str) -> syn::Ident {
    format_ident!("{}", name)
}

/// The documentation of an item: its `///` lines joined, each with its one
/// leading space removed and, outside code fences, with rustdoc intra-doc
/// links unwrapped to the inline code they display (no binding resolves Rust
/// paths), or `None` when there are none.
pub(crate) fn docs(attrs: &[syn::Attribute]) -> Option<String> {
    let lines: Vec<String> = attrs
        .iter()
        .filter(|a| a.path().is_ident("doc"))
        .filter_map(|a| match &a.meta {
            syn::Meta::NameValue(nv) => match &nv.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) => Some(s.value()),
                _ => None,
            },
            _ => None,
        })
        .map(|line| {
            line.strip_prefix(' ')
                .unwrap_or(&line)
                .trim_end()
                .to_string()
        })
        .collect();
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
    let text = lines.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
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

/// The deprecation note of a `#[deprecated]` item (an empty note becomes
/// `deprecated`).
pub(crate) fn deprecated(attrs: &[syn::Attribute]) -> Option<String> {
    let attr = attrs.iter().find(|a| a.path().is_ident("deprecated"))?;
    let note = match &attr.meta {
        syn::Meta::NameValue(nv) => lit_str(&nv.value),
        syn::Meta::List(list) => {
            let mut note = None;
            let _ = list.parse_nested_meta(|meta| {
                if meta.path.is_ident("note") {
                    note = meta.value()?.parse::<syn::LitStr>().ok().map(|s| s.value());
                }
                Ok(())
            });
            note
        }
        syn::Meta::Path(_) => None,
    };
    Some(note.unwrap_or_else(|| "deprecated".to_string()))
}

fn lit_str(expr: &syn::Expr) -> Option<String> {
    match expr {
        syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    }
}

/// The `#[cfg]` and `#[cfg_attr]` attributes of an item, which every item
/// generated for it repeats.
pub(crate) fn cfgs(attrs: &[syn::Attribute]) -> Vec<&syn::Attribute> {
    attrs
        .iter()
        .filter(|a| a.path().is_ident("cfg") || a.path().is_ident("cfg_attr"))
        .collect()
}

/// Reject a `#[cfg]` on a member of a declaration (`what`: "a record
/// field", "an enum variant", ...): the generated bindings describe every
/// member, so they can't follow it.
pub(crate) fn no_member_cfg(attrs: &[syn::Attribute], what: &str) -> syn::Result<()> {
    match attrs.iter().find(|a| a.path().is_ident("cfg")) {
        Some(attr) => Err(syn::Error::new_spanned(
            attr.path(),
            format!(
                "weaveffi: `#[cfg]` on {what} isn't supported, because the generated bindings \
                 can't follow it; put the `#[cfg]` on the whole item"
            ),
        )),
        None => Ok(()),
    }
}

/// Whether `attr` is a `#[weaveffi(...)]` helper attribute.
pub(crate) fn is_helper(attr: &syn::Attribute) -> bool {
    attr.path().is_ident("weaveffi")
}

/// Whether the item's `#[weaveffi(...)]` helpers include the bare word
/// `word` (`#[weaveffi(skip)]`).
pub(crate) fn has_helper_word(attrs: &[syn::Attribute], word: &str) -> syn::Result<bool> {
    let mut found = false;
    for attr in attrs.iter().filter(|a| is_helper(a)) {
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident(word) {
                found = true;
                Ok(())
            } else if meta.input.peek(syn::Token![=]) {
                // Another helper's `key = value`; leave it to its owner.
                let _: syn::Expr = meta.value()?.parse()?;
                Ok(())
            } else {
                Ok(())
            }
        })?;
    }
    Ok(found)
}

/// Where `span` is: the file as the compiler names it and its line and
/// column, which order the declarations of the assembled API.
pub(crate) fn location(span: Span) -> Location {
    let span = span.unwrap();
    let start = span.start();
    Location {
        file: span.file().replace('\\', "/"),
        line: u32::try_from(start.line()).unwrap_or(u32::MAX),
        column: u32::try_from(start.column()).unwrap_or(u32::MAX),
    }
}

/// The metadata frame of one declaration, built from IR whose type and
/// `throws` spellings are placeholders the expansion fills from each type's
/// compile-time name.
pub(crate) struct FrameBuilder {
    /// The name expression of each placeholder, in order.
    names: Vec<TokenStream>,
}

/// The marker around a placeholder's index; JSON escapes it as `\u0001`.
const MARK: char = '\u{1}';

impl FrameBuilder {
    pub(crate) fn new() -> Self {
        Self { names: Vec::new() }
    }

    fn placeholder(&mut self, name: TokenStream) -> String {
        let n = self.names.len();
        self.names.push(name);
        format!("{MARK}{n}{MARK}")
    }

    /// A type whose spelling is the `Name` expression `name`.
    pub(crate) fn ty(&mut self, name: TokenStream) -> TypeRef {
        TypeRef::Named(self.placeholder(name))
    }

    /// A `throws` whose domain is the `Name` expression `name` (empty for an
    /// infallible callable, which the CLI reads as no `throws`).
    pub(crate) fn throws(&mut self, name: TokenStream) -> Throws {
        Throws::Domain(self.placeholder(name))
    }

    /// The exported static holding the frame of `item`, located at `at`.
    pub(crate) fn finish(self, prefix: &str, at: Location, item: Item) -> TokenStream {
        let frame = Frame::new(prefix, at, item);
        let symbol = frame.symbol();
        let json = serde_json::to_string(&frame).expect("a frame always serializes");
        let parts = split_placeholders(&json, &self.names);
        let static_name = format_ident!("__WEAVEFFI_META");
        quote! {
            #[doc(hidden)]
            #[allow(unsafe_code)]
            #[unsafe(export_name = #symbol)]
            #[cfg_attr(target_family = "wasm", unsafe(link_section = #SECTION))]
            #[used]
            pub static #static_name: [u8; {
                const PARTS: &[::weaveffi::__rt::Part<'static>] = &[#(#parts),*];
                ::weaveffi::__rt::meta::frame_len(PARTS)
            }] = {
                const PARTS: &[::weaveffi::__rt::Part<'static>] = &[#(#parts),*];
                ::weaveffi::__rt::meta::frame::<{ ::weaveffi::__rt::meta::frame_len(PARTS) }>(PARTS)
            };
        }
    }
}

/// The parts of `json`, with each placeholder replaced by its name.
fn split_placeholders(json: &str, names: &[TokenStream]) -> Vec<TokenStream> {
    let mut parts = Vec::new();
    let mut rest = json;
    let mark = "\\u0001";
    while let Some(start) = rest.find(mark) {
        let after = &rest[start + mark.len()..];
        let Some(end) = after.find(mark) else {
            break;
        };
        let index: usize = after[..end].parse().expect("a placeholder holds its index");
        let lit = &rest[..start];
        if !lit.is_empty() {
            parts.push(quote!(::weaveffi::__rt::Part::Lit(#lit)));
        }
        let name = &names[index];
        parts.push(quote!(::weaveffi::__rt::Part::Name(&#name)));
        rest = &after[end + mark.len()..];
    }
    if !rest.is_empty() {
        parts.push(quote!(::weaveffi::__rt::Part::Lit(#rest)));
    }
    parts
}

/// The exported contract function of the declaration `path`: it returns
/// the hash of the canonical signature `sig`, whose slots are `Name`
/// expressions.
pub(crate) fn contract(prefix: &str, path: &str, sig: Sig<TokenStream>) -> TokenStream {
    let symbol = weaveffi_model::contract::symbol(prefix, path);
    let parts = sig.0.into_iter().map(|p| match p {
        Part::Lit(s) => quote!(::weaveffi::__rt::Part::Lit(#s)),
        Part::Slot(name) => quote!(::weaveffi::__rt::Part::Name(&#name)),
    });
    quote! {
        #[doc(hidden)]
        #[allow(unsafe_code)]
        #[unsafe(export_name = #symbol)]
        pub extern "C" fn __weaveffi_contract() -> u64 {
            const PARTS: &[::weaveffi::__rt::Part<'static>] = &[#(#parts),*];
            const HASH: u64 = ::weaveffi::__rt::meta::hash(PARTS);
            HASH
        }
    }
}

/// A literal `Name`.
pub(crate) fn name_lit(s: &str) -> TokenStream {
    quote!(::weaveffi::__rt::Name::new(#s))
}

/// Wrap generated items in an anonymous `const` block carrying `cfgs`, so
/// their helper names never clash with the producer's.
pub(crate) fn scoped(cfgs: &[&syn::Attribute], body: TokenStream) -> TokenStream {
    quote! {
        #(#cfgs)*
        #[allow(non_snake_case, non_upper_case_globals, deprecated, clippy::all)]
        const _: () = {
            #body
        };
    }
}

/// `ty` with every `Self` replaced by `with` (for the members of an `impl`
/// block, whose thunks live outside it).
pub(crate) fn replace_self(ty: &syn::Type, with: &syn::Type) -> syn::Type {
    struct Replace<'a>(&'a syn::Type);
    impl syn::visit_mut::VisitMut for Replace<'_> {
        fn visit_type_mut(&mut self, ty: &mut syn::Type) {
            if let syn::Type::Path(p) = ty {
                if p.qself.is_none() && p.path.is_ident("Self") {
                    *ty = self.0.clone();
                    return;
                }
            }
            syn::visit_mut::visit_type_mut(self, ty);
        }
    }
    let mut ty = ty.clone();
    syn::visit_mut::VisitMut::visit_type_mut(&mut Replace(with), &mut ty);
    ty
}
