use color_eyre::eyre::{bail, eyre, Result};
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, EnumVariant, Function, ListenerDef, Module, Param, StructDef,
    StructField, TypeRef,
};

fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

fn find_attr<'a>(attrs: &'a [syn::Attribute], name: &str) -> Option<&'a syn::Attribute> {
    attrs.iter().find(|a| a.path().is_ident(name))
}

fn has_repr_i32(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("repr") && a.parse_args::<syn::Ident>().is_ok_and(|id| id == "i32")
    })
}

/// Parse `#[deprecated(since = "...", note = "...")]` into `(since, note)`.
/// Bare `#[deprecated]` returns `(None, Some("deprecated"))` so callers know
/// the function is flagged even when no message is provided.
fn parse_deprecated(attrs: &[syn::Attribute]) -> (Option<String>, Option<String>) {
    let Some(attr) = find_attr(attrs, "deprecated") else {
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

/// Parse `#[weaveffi_listener(event_callback = "OnReady")]` and return the
/// referenced callback name.
fn parse_listener_callback(attr: &syn::Attribute) -> Result<String> {
    if matches!(attr.meta, syn::Meta::Path(_)) {
        bail!("#[weaveffi_listener] requires `event_callback = \"<callback name>\"`");
    }
    let mut callback = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("event_callback") {
            let value = meta.value()?;
            let lit: syn::LitStr = value.parse()?;
            callback = Some(lit.value());
        }
        Ok(())
    })
    .map_err(|e| eyre!("failed to parse #[weaveffi_listener] attribute: {e}"))?;
    callback.ok_or_else(|| {
        eyre!("#[weaveffi_listener] requires `event_callback = \"<callback name>\"`")
    })
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
    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

fn is_ident(ty: &syn::Type, name: &str) -> bool {
    matches!(ty, syn::Type::Path(p) if p.path.is_ident(name))
}

fn single_generic_arg(seg: &syn::PathSegment) -> Result<&syn::Type> {
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        bail!("{}: expected generic arguments", seg.ident);
    };
    if args.args.len() != 1 {
        bail!("{}: expected exactly 1 generic argument", seg.ident);
    }
    let syn::GenericArgument::Type(ty) = &args.args[0] else {
        bail!("{}: expected type argument", seg.ident);
    };
    Ok(ty)
}

fn two_generic_args(seg: &syn::PathSegment) -> Result<(&syn::Type, &syn::Type)> {
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        bail!("{}: expected generic arguments", seg.ident);
    };
    if args.args.len() != 2 {
        bail!("{}: expected exactly 2 generic arguments", seg.ident);
    }
    let syn::GenericArgument::Type(k) = &args.args[0] else {
        bail!("{}: expected type argument for key", seg.ident);
    };
    let syn::GenericArgument::Type(v) = &args.args[1] else {
        bail!("{}: expected type argument for value", seg.ident);
    };
    Ok((k, v))
}

/// Returns the bare ident name (last path segment) for a `Type::Path`, or
/// `None` if the type isn't a simple path. Used to emit
/// [`TypeRef::TypedHandle`] for `*mut Foo` patterns.
fn type_path_ident(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else { return None };
    p.path.segments.last().map(|s| s.ident.to_string())
}

fn map_type(ty: &syn::Type) -> Result<TypeRef> {
    match ty {
        syn::Type::Reference(r) => {
            // `&str`, `&[u8]`, `&mut T`, `&T` — `&str` and `&[u8]` map to
            // borrowed IR variants; everything else delegates to the inner
            // type so `&mut Foo` becomes `Struct(Foo)` (the `mutable` flag is
            // captured separately by the caller).
            if let syn::Type::Path(p) = r.elem.as_ref() {
                if p.path.is_ident("str") {
                    return Ok(TypeRef::BorrowedStr);
                }
            }
            if let syn::Type::Slice(slice) = r.elem.as_ref() {
                if is_ident(&slice.elem, "u8") {
                    return Ok(TypeRef::BorrowedBytes);
                }
            }
            map_type(&r.elem)
        }
        syn::Type::Ptr(p) => {
            // `*mut Foo` and `*const Foo` are treated as opaque typed handles
            // (`handle<Foo>`). The mutability of the pointer itself is not
            // expressible in the IR.
            let name = type_path_ident(&p.elem)
                .ok_or_else(|| eyre!("unsupported pointer target; expected a named type"))?;
            Ok(TypeRef::TypedHandle(name))
        }
        syn::Type::Path(type_path) => {
            let seg = type_path
                .path
                .segments
                .last()
                .ok_or_else(|| eyre!("empty type path"))?;
            let ident = seg.ident.to_string();
            match ident.as_str() {
                "i32" => Ok(TypeRef::I32),
                "u32" => Ok(TypeRef::U32),
                "i64" => Ok(TypeRef::I64),
                "f64" => Ok(TypeRef::F64),
                "bool" => Ok(TypeRef::Bool),
                "String" => Ok(TypeRef::StringUtf8),
                "u64" => Ok(TypeRef::Handle),
                "Vec" => {
                    let inner = single_generic_arg(seg)?;
                    if is_ident(inner, "u8") {
                        return Ok(TypeRef::Bytes);
                    }
                    Ok(TypeRef::List(Box::new(map_type(inner)?)))
                }
                "Option" => {
                    let inner = single_generic_arg(seg)?;
                    Ok(TypeRef::Optional(Box::new(map_type(inner)?)))
                }
                "HashMap" | "BTreeMap" => {
                    let (k, v) = two_generic_args(seg)?;
                    Ok(TypeRef::Map(Box::new(map_type(k)?), Box::new(map_type(v)?)))
                }
                other => Ok(TypeRef::Struct(other.to_string())),
            }
        }
        _ => bail!("unsupported type syntax"),
    }
}

fn parse_discriminant(expr: &syn::Expr) -> Result<i32> {
    match expr {
        syn::Expr::Lit(lit) => {
            let syn::Lit::Int(int_lit) = &lit.lit else {
                bail!("expected integer literal for discriminant");
            };
            Ok(int_lit.base10_parse::<i32>()?)
        }
        syn::Expr::Unary(unary) if matches!(unary.op, syn::UnOp::Neg(_)) => {
            Ok(-parse_discriminant(&unary.expr)?)
        }
        _ => bail!("unsupported discriminant expression"),
    }
}

fn extract_params(
    inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>,
) -> Result<Vec<Param>> {
    inputs
        .iter()
        .filter_map(|arg| match arg {
            syn::FnArg::Typed(pt) => Some(pt),
            syn::FnArg::Receiver(_) => None,
        })
        .map(|pt| {
            let param_name = match pt.pat.as_ref() {
                syn::Pat::Ident(id) => id.ident.to_string(),
                _ => bail!("unsupported parameter pattern"),
            };
            // `&mut T` (excluding `&mut str` / `&mut [u8]`, which become the
            // borrowed IR variants) is the syntax for a mutable parameter.
            let mutable =
                matches!(pt.ty.as_ref(), syn::Type::Reference(r) if r.mutability.is_some());
            Ok(Param {
                name: param_name,
                ty: map_type(&pt.ty)?,
                mutable,
                doc: extract_doc(&pt.attrs),
            })
        })
        .collect()
}

fn extract_function(item: &syn::ItemFn) -> Result<Function> {
    let name = item.sig.ident.to_string();
    let params = extract_params(&item.sig.inputs)?;

    let returns = match &item.sig.output {
        syn::ReturnType::Default => None,
        syn::ReturnType::Type(_, ty) => Some(map_type(ty)?),
    };

    let (since, deprecated) = parse_deprecated(&item.attrs);

    Ok(Function {
        name,
        params,
        returns,
        doc: extract_doc(&item.attrs),
        r#async: item.sig.asyncness.is_some() || has_attr(&item.attrs, "weaveffi_async"),
        cancellable: has_attr(&item.attrs, "weaveffi_cancellable"),
        deprecated,
        since,
    })
}

fn extract_callback(item: &syn::ItemFn) -> Result<CallbackDef> {
    Ok(CallbackDef {
        name: item.sig.ident.to_string(),
        params: extract_params(&item.sig.inputs)?,
        doc: extract_doc(&item.attrs),
    })
}

fn extract_listener(item: &syn::ItemFn) -> Result<ListenerDef> {
    let attr = find_attr(&item.attrs, "weaveffi_listener")
        .ok_or_else(|| eyre!("missing #[weaveffi_listener] attribute"))?;
    Ok(ListenerDef {
        name: item.sig.ident.to_string(),
        event_callback: parse_listener_callback(attr)?,
        doc: extract_doc(&item.attrs),
    })
}

fn extract_struct(item: &syn::ItemStruct) -> Result<StructDef> {
    let name = item.ident.to_string();
    let fields = match &item.fields {
        syn::Fields::Named(named) => named
            .named
            .iter()
            .map(|f| {
                let field_name = f
                    .ident
                    .as_ref()
                    .ok_or_else(|| eyre!("unnamed field in struct {name}"))?
                    .to_string();
                Ok(StructField {
                    name: field_name,
                    ty: map_type(&f.ty)?,
                    doc: extract_doc(&f.attrs),
                    default: None,
                })
            })
            .collect::<Result<_>>()?,
        _ => bail!("only named fields are supported for #[weaveffi_struct]"),
    };

    Ok(StructDef {
        name,
        doc: extract_doc(&item.attrs),
        fields,
        builder: has_attr(&item.attrs, "weaveffi_builder"),
    })
}

fn extract_enum(item: &syn::ItemEnum) -> Result<EnumDef> {
    if !has_repr_i32(&item.attrs) {
        bail!(
            "enum `{}` must have #[repr(i32)] to be a weaveffi_enum",
            item.ident
        );
    }
    let name = item.ident.to_string();
    let variants = item
        .variants
        .iter()
        .map(|v| {
            let (_, expr) = v.discriminant.as_ref().ok_or_else(|| {
                eyre!(
                    "enum `{name}` variant `{}` must have an explicit discriminant",
                    v.ident
                )
            })?;
            Ok(EnumVariant {
                name: v.ident.to_string(),
                value: parse_discriminant(expr)?,
                doc: extract_doc(&v.attrs),
            })
        })
        .collect::<Result<_>>()?;

    Ok(EnumDef {
        name,
        doc: extract_doc(&item.attrs),
        variants,
    })
}

fn extract_module(item_mod: &syn::ItemMod) -> Result<Module> {
    let name = item_mod.ident.to_string();
    let mut functions = Vec::new();
    let mut structs = Vec::new();
    let mut enums = Vec::new();
    let mut callbacks = Vec::new();
    let mut listeners = Vec::new();
    let mut modules = Vec::new();

    if let Some((_, items)) = &item_mod.content {
        for item in items {
            match item {
                syn::Item::Fn(f) if has_attr(&f.attrs, "weaveffi_listener") => {
                    listeners.push(extract_listener(f)?);
                }
                syn::Item::Fn(f) if has_attr(&f.attrs, "weaveffi_callback") => {
                    callbacks.push(extract_callback(f)?);
                }
                syn::Item::Fn(f) if has_attr(&f.attrs, "weaveffi_export") => {
                    functions.push(extract_function(f)?);
                }
                syn::Item::Struct(s) if has_attr(&s.attrs, "weaveffi_struct") => {
                    structs.push(extract_struct(s)?);
                }
                syn::Item::Enum(e) if has_attr(&e.attrs, "weaveffi_enum") => {
                    enums.push(extract_enum(e)?);
                }
                syn::Item::Mod(m) if m.content.is_some() => {
                    modules.push(extract_module(m)?);
                }
                _ => {}
            }
        }
    }

    Ok(Module {
        name,
        functions,
        structs,
        enums,
        callbacks,
        listeners,
        errors: None,
        modules,
    })
}

pub fn extract_api_from_rust(source: &str) -> Result<Api> {
    let file = syn::parse_file(source)?;
    let mut modules = Vec::new();

    for item in &file.items {
        if let syn::Item::Mod(item_mod) = item {
            if item_mod.content.is_some() {
                modules.push(extract_module(item_mod)?);
            }
        }
    }

    Ok(Api {
        version: "0.1.0".to_string(),
        modules,
        generators: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_produces_no_modules() {
        let api = extract_api_from_rust("").unwrap();
        assert_eq!(api.version, "0.1.0");
        assert!(api.modules.is_empty());
    }

    #[test]
    fn extracts_exported_function() {
        let src = r#"
            mod math {
                #[weaveffi_export]
                fn add(a: i32, b: i32) -> i32 {
                    a + b
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(api.modules.len(), 1);
        let m = &api.modules[0];
        assert_eq!(m.name, "math");
        assert_eq!(m.functions.len(), 1);
        let f = &m.functions[0];
        assert_eq!(f.name, "add");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.params[0].name, "a");
        assert_eq!(f.params[0].ty, TypeRef::I32);
        assert_eq!(f.params[1].name, "b");
        assert_eq!(f.params[1].ty, TypeRef::I32);
        assert_eq!(f.returns, Some(TypeRef::I32));
        assert!(!f.r#async);
    }

    #[test]
    fn ignores_unannotated_functions() {
        let src = r#"
            mod math {
                fn helper() -> i32 { 42 }
                #[weaveffi_export]
                fn public_fn(x: i32) -> i32 { x }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(api.modules[0].functions.len(), 1);
        assert_eq!(api.modules[0].functions[0].name, "public_fn");
    }

    #[test]
    fn maps_all_primitive_types() {
        let src = r#"
            mod types {
                #[weaveffi_export]
                fn all(a: i32, b: u32, c: i64, d: f64, e: bool, f: String, g: Vec<u8>, h: u64) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let params = &api.modules[0].functions[0].params;
        assert_eq!(params[0].ty, TypeRef::I32);
        assert_eq!(params[1].ty, TypeRef::U32);
        assert_eq!(params[2].ty, TypeRef::I64);
        assert_eq!(params[3].ty, TypeRef::F64);
        assert_eq!(params[4].ty, TypeRef::Bool);
        assert_eq!(params[5].ty, TypeRef::StringUtf8);
        assert_eq!(params[6].ty, TypeRef::Bytes);
        assert_eq!(params[7].ty, TypeRef::Handle);
    }

    #[test]
    fn maps_option_type() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn opt(x: Option<i32>) -> Option<String> { None }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::Optional(Box::new(TypeRef::I32)));
        assert_eq!(
            f.returns,
            Some(TypeRef::Optional(Box::new(TypeRef::StringUtf8)))
        );
    }

    #[test]
    fn maps_vec_type() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn list(items: Vec<i32>) -> Vec<String> { vec![] }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::List(Box::new(TypeRef::I32)));
        assert_eq!(
            f.returns,
            Some(TypeRef::List(Box::new(TypeRef::StringUtf8)))
        );
    }

    #[test]
    fn maps_hashmap_type() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn hmap(x: HashMap<String, i32>) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(
            api.modules[0].functions[0].params[0].ty,
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32))
        );
    }

    #[test]
    fn maps_btreemap_type() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn bmap(x: BTreeMap<String, f64>) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(
            api.modules[0].functions[0].params[0].ty,
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::F64))
        );
    }

    #[test]
    fn maps_custom_struct_type() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn create(p: Point) -> Rect { todo!() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::Struct("Point".to_string()));
        assert_eq!(f.returns, Some(TypeRef::Struct("Rect".to_string())));
    }

    #[test]
    fn extracts_struct() {
        let src = r#"
            mod geo {
                #[weaveffi_struct]
                struct Point {
                    x: f64,
                    y: f64,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let s = &api.modules[0].structs[0];
        assert_eq!(s.name, "Point");
        assert_eq!(s.fields.len(), 2);
        assert_eq!(s.fields[0].name, "x");
        assert_eq!(s.fields[0].ty, TypeRef::F64);
        assert_eq!(s.fields[1].name, "y");
        assert_eq!(s.fields[1].ty, TypeRef::F64);
    }

    #[test]
    fn extracts_enum_with_discriminants() {
        let src = r#"
            mod status {
                #[weaveffi_enum]
                #[repr(i32)]
                enum Color {
                    Red = 0,
                    Green = 1,
                    Blue = 2,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let e = &api.modules[0].enums[0];
        assert_eq!(e.name, "Color");
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Red");
        assert_eq!(e.variants[0].value, 0);
        assert_eq!(e.variants[1].name, "Green");
        assert_eq!(e.variants[1].value, 1);
        assert_eq!(e.variants[2].name, "Blue");
        assert_eq!(e.variants[2].value, 2);
    }

    #[test]
    fn enum_negative_discriminant() {
        let src = r#"
            mod m {
                #[weaveffi_enum]
                #[repr(i32)]
                enum Signed {
                    Neg = -1,
                    Zero = 0,
                    Pos = 1,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let e = &api.modules[0].enums[0];
        assert_eq!(e.variants[0].value, -1);
        assert_eq!(e.variants[1].value, 0);
        assert_eq!(e.variants[2].value, 1);
    }

    #[test]
    fn enum_requires_repr_i32() {
        let src = r#"
            mod m {
                #[weaveffi_enum]
                enum Bad {
                    A = 0,
                }
            }
        "#;
        let err = extract_api_from_rust(src).unwrap_err();
        assert!(
            format!("{err}").contains("repr(i32)"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn enum_requires_explicit_discriminants() {
        let src = r#"
            mod m {
                #[weaveffi_enum]
                #[repr(i32)]
                enum Bad {
                    A,
                    B,
                }
            }
        "#;
        assert!(extract_api_from_rust(src).is_err());
    }

    #[test]
    fn multiple_modules() {
        let src = r#"
            mod math {
                #[weaveffi_export]
                fn add(a: i32, b: i32) -> i32 { a + b }
            }
            mod strings {
                #[weaveffi_export]
                fn len(s: String) -> i32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(api.modules.len(), 2);
        assert_eq!(api.modules[0].name, "math");
        assert_eq!(api.modules[1].name, "strings");
    }

    #[test]
    fn function_with_no_params_or_return() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn noop() {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.returns.is_none());
        assert!(f.params.is_empty());
    }

    #[test]
    fn mixed_items_in_module() {
        let src = r#"
            mod geo {
                #[weaveffi_struct]
                struct Point { x: f64, y: f64 }

                #[weaveffi_enum]
                #[repr(i32)]
                enum Axis { X = 0, Y = 1 }

                #[weaveffi_export]
                fn distance(a: Point, b: Point) -> f64 { 0.0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.structs.len(), 1);
        assert_eq!(m.enums.len(), 1);
        assert_eq!(m.functions.len(), 1);
    }

    #[test]
    fn ignores_unannotated_structs_and_enums() {
        let src = r#"
            mod m {
                struct Internal { x: i32 }

                enum Private { A = 0 }

                #[weaveffi_export]
                fn go() {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let m = &api.modules[0];
        assert!(m.structs.is_empty());
        assert!(m.enums.is_empty());
        assert_eq!(m.functions.len(), 1);
    }

    #[test]
    fn nested_generic_types() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn nested(x: Option<Vec<i32>>) -> Vec<Option<String>> { vec![] }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.params[0].ty,
            TypeRef::Optional(Box::new(TypeRef::List(Box::new(TypeRef::I32))))
        );
        assert_eq!(
            f.returns,
            Some(TypeRef::List(Box::new(TypeRef::Optional(Box::new(
                TypeRef::StringUtf8
            )))))
        );
    }

    #[test]
    fn extract_simple_function() {
        let src = r#"
            mod calc {
                #[weaveffi_export]
                fn multiply(x: f64, y: f64) -> f64 { x * y }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.name, "multiply");
        assert_eq!(f.params.len(), 2);
        assert_eq!(f.params[0].name, "x");
        assert_eq!(f.params[0].ty, TypeRef::F64);
        assert_eq!(f.params[1].name, "y");
        assert_eq!(f.params[1].ty, TypeRef::F64);
        assert_eq!(f.returns, Some(TypeRef::F64));
    }

    #[test]
    fn extract_struct_with_typed_fields() {
        let src = r#"
            mod shapes {
                #[weaveffi_struct]
                struct Rect {
                    width: i32,
                    height: i32,
                    label: String,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let s = &api.modules[0].structs[0];
        assert_eq!(s.name, "Rect");
        assert_eq!(s.fields.len(), 3);
        assert_eq!(s.fields[0].name, "width");
        assert_eq!(s.fields[0].ty, TypeRef::I32);
        assert_eq!(s.fields[1].name, "height");
        assert_eq!(s.fields[1].ty, TypeRef::I32);
        assert_eq!(s.fields[2].name, "label");
        assert_eq!(s.fields[2].ty, TypeRef::StringUtf8);
    }

    #[test]
    fn extract_enum_with_explicit_discriminants() {
        let src = r#"
            mod status {
                #[weaveffi_enum]
                #[repr(i32)]
                enum Level {
                    Low = 0,
                    Medium = 5,
                    High = 10,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let e = &api.modules[0].enums[0];
        assert_eq!(e.name, "Level");
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].name, "Low");
        assert_eq!(e.variants[0].value, 0);
        assert_eq!(e.variants[1].name, "Medium");
        assert_eq!(e.variants[1].value, 5);
        assert_eq!(e.variants[2].name, "High");
        assert_eq!(e.variants[2].value, 10);
    }

    #[test]
    fn extract_optional_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn greet(name: Option<String>) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.params[0].ty,
            TypeRef::Optional(Box::new(TypeRef::StringUtf8))
        );
    }

    #[test]
    fn extract_vec_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn sum(values: Vec<i32>) -> i32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::List(Box::new(TypeRef::I32)));
    }

    #[test]
    fn extract_hashmap_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn lookup(table: HashMap<String, i32>) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(
            f.params[0].ty,
            TypeRef::Map(Box::new(TypeRef::StringUtf8), Box::new(TypeRef::I32))
        );
    }

    #[test]
    fn extract_nested_module() {
        let src = r#"
            mod outer {
                #[weaveffi_export]
                fn action(x: i32) -> bool { true }

                #[weaveffi_struct]
                struct Config { limit: i32 }

                #[weaveffi_enum]
                #[repr(i32)]
                enum Mode { Fast = 0, Safe = 1 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(api.modules.len(), 1);
        let m = &api.modules[0];
        assert_eq!(m.name, "outer");
        assert_eq!(m.functions.len(), 1);
        assert_eq!(m.functions[0].name, "action");
        assert_eq!(m.structs.len(), 1);
        assert_eq!(m.structs[0].name, "Config");
        assert_eq!(m.enums.len(), 1);
        assert_eq!(m.enums[0].name, "Mode");
    }

    #[test]
    fn extract_unannotated_items_skipped() {
        let src = r#"
            mod m {
                fn private_fn(x: i32) -> i32 { x }
                struct InternalData { v: i32 }
                enum InternalState { A, B }

                #[weaveffi_export]
                fn public_fn() -> bool { true }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let m = &api.modules[0];
        assert_eq!(m.functions.len(), 1);
        assert_eq!(m.functions[0].name, "public_fn");
        assert!(m.structs.is_empty());
        assert!(m.enums.is_empty());
    }

    #[test]
    fn extract_doc_comments() {
        let src = r#"
            mod m {
                /// Adds two numbers.
                #[weaveffi_export]
                fn add(a: i32, b: i32) -> i32 { a + b }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.doc.as_deref(), Some("Adds two numbers."));
    }

    #[test]
    fn extract_borrowed_str_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn echo(s: &str) -> String { String::new() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::BorrowedStr);
        assert_eq!(f.returns, Some(TypeRef::StringUtf8));
    }

    #[test]
    fn extract_borrowed_bytes_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn hash(b: &[u8]) -> u32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::BorrowedBytes);
    }

    #[test]
    fn extract_typed_handle_via_pointer() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn open() -> *mut Token { std::ptr::null_mut() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.returns, Some(TypeRef::TypedHandle("Token".into())));
    }

    #[test]
    fn extract_typed_handle_param_via_pointer() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn close(token: *mut Token) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::TypedHandle("Token".into()));
        assert!(!f.params[0].mutable);
    }

    #[test]
    fn extract_const_pointer_is_typed_handle() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn peek(token: *const Token) -> i32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::TypedHandle("Token".into()));
    }

    #[test]
    fn extract_mutable_reference_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn fill(buf: &mut Buffer) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let p = &api.modules[0].functions[0].params[0];
        assert!(p.mutable);
        assert_eq!(p.ty, TypeRef::Struct("Buffer".into()));
    }

    #[test]
    fn extract_immutable_reference_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn read(buf: &Buffer) -> i32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let p = &api.modules[0].functions[0].params[0];
        assert!(!p.mutable);
        assert_eq!(p.ty, TypeRef::Struct("Buffer".into()));
    }

    #[test]
    fn extract_async_marker_attribute() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                #[weaveffi_async]
                fn fetch(url: String) -> String { String::new() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.r#async);
    }

    #[test]
    fn extract_async_keyword() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                async fn fetch(url: String) -> String { String::new() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.r#async);
    }

    #[test]
    fn extract_cancellable_marker() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                #[weaveffi_async]
                #[weaveffi_cancellable]
                fn fetch(url: String) -> String { String::new() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.r#async);
        assert!(f.cancellable);
    }

    #[test]
    fn extract_deprecated_with_since_and_note() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                #[deprecated(since = "0.1.0", note = "Use new_op instead")]
                fn legacy() -> i32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.since.as_deref(), Some("0.1.0"));
        assert_eq!(f.deprecated.as_deref(), Some("Use new_op instead"));
    }

    #[test]
    fn extract_deprecated_bare_attribute() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                #[deprecated]
                fn legacy() -> i32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.since.is_none());
        assert_eq!(f.deprecated.as_deref(), Some("deprecated"));
    }

    #[test]
    fn extract_no_deprecated_means_none() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn ok() -> i32 { 0 }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.since.is_none());
        assert!(f.deprecated.is_none());
    }

    #[test]
    fn extract_builder_struct() {
        let src = r#"
            mod m {
                #[weaveffi_struct]
                #[weaveffi_builder]
                struct Item {
                    name: String,
                    count: i32,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let s = &api.modules[0].structs[0];
        assert!(s.builder);
    }

    #[test]
    fn extract_callback_definition() {
        let src = r#"
            mod m {
                /// Fires when an item is ready.
                #[weaveffi_callback]
                fn OnReady(code: i32, msg: String) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(api.modules[0].callbacks.len(), 1);
        let cb = &api.modules[0].callbacks[0];
        assert_eq!(cb.name, "OnReady");
        assert_eq!(cb.params.len(), 2);
        assert_eq!(cb.params[0].name, "code");
        assert_eq!(cb.params[0].ty, TypeRef::I32);
        assert_eq!(cb.params[1].name, "msg");
        assert_eq!(cb.params[1].ty, TypeRef::StringUtf8);
        assert_eq!(cb.doc.as_deref(), Some("Fires when an item is ready."));
    }

    #[test]
    fn extract_listener_definition() {
        let src = r#"
            mod m {
                /// Subscribe to OnReady events.
                #[weaveffi_listener(event_callback = "OnReady")]
                fn ready_listener() {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        assert_eq!(api.modules[0].listeners.len(), 1);
        let l = &api.modules[0].listeners[0];
        assert_eq!(l.name, "ready_listener");
        assert_eq!(l.event_callback, "OnReady");
        assert_eq!(l.doc.as_deref(), Some("Subscribe to OnReady events."));
    }

    #[test]
    fn extract_listener_without_event_callback_is_error() {
        let src = r#"
            mod m {
                #[weaveffi_listener]
                fn bad() {}
            }
        "#;
        let err = extract_api_from_rust(src).unwrap_err();
        assert!(
            format!("{err}").contains("event_callback"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn extract_nested_modules_recursive() {
        let src = r#"
            mod outer {
                #[weaveffi_export]
                fn top() -> i32 { 0 }

                mod inner {
                    #[weaveffi_export]
                    fn deep(x: bool) -> bool { x }

                    mod deeper {
                        #[weaveffi_export]
                        fn very() {}
                    }
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let outer = &api.modules[0];
        assert_eq!(outer.name, "outer");
        assert_eq!(outer.functions.len(), 1);
        assert_eq!(outer.modules.len(), 1);
        let inner = &outer.modules[0];
        assert_eq!(inner.name, "inner");
        assert_eq!(inner.functions.len(), 1);
        assert_eq!(inner.functions[0].name, "deep");
        assert_eq!(inner.modules.len(), 1);
        let deeper = &inner.modules[0];
        assert_eq!(deeper.name, "deeper");
        assert_eq!(deeper.functions.len(), 1);
        assert_eq!(deeper.functions[0].name, "very");
    }

    #[test]
    fn extract_preserves_param_doc() {
        // Doc comments on parameters round-trip when present (rare in
        // practice, since rustfmt rejects them in many configurations).
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn add(
                    /// First addend.
                    a: i32,
                    b: i32,
                ) -> i32 { a + b }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].doc.as_deref(), Some("First addend."));
        assert!(f.params[1].doc.is_none());
    }
}
