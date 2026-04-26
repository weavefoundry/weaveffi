use color_eyre::eyre::{bail, eyre, Result};
use weaveffi_ir::ir::{
    Api, CallbackDef, EnumDef, EnumVariant, Function, Module, Param, StructDef, StructField,
    TypeRef,
};

fn has_attr(attrs: &[syn::Attribute], name: &str) -> bool {
    attrs.iter().any(|a| a.path().is_ident(name))
}

fn has_repr_i32(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        a.path().is_ident("repr") && a.parse_args::<syn::Ident>().is_ok_and(|id| id == "i32")
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

fn simple_type_name(ty: &syn::Type) -> Option<String> {
    let syn::Type::Path(p) = ty else {
        return None;
    };
    let seg = p.path.segments.last()?;
    if matches!(seg.arguments, syn::PathArguments::None) {
        Some(seg.ident.to_string())
    } else {
        None
    }
}

fn extract_typed_handle_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let syn::Meta::NameValue(nv) = &attr.meta else {
            return None;
        };
        if !nv.path.is_ident("weaveffi_typed_handle") {
            return None;
        }
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = &nv.value
        else {
            return None;
        };
        Some(s.value())
    })
}

fn extract_default_attr(attrs: &[syn::Attribute]) -> Result<Option<serde_yaml::Value>> {
    for attr in attrs {
        let syn::Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        if !nv.path.is_ident("weaveffi_default") {
            continue;
        }
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = &nv.value
        else {
            bail!("weaveffi_default: expected a string literal YAML value");
        };
        let value: serde_yaml::Value = serde_yaml::from_str(&s.value())
            .map_err(|e| eyre!("weaveffi_default: invalid YAML literal: {e}"))?;
        return Ok(Some(value));
    }
    Ok(None)
}

fn extract_callback_name_attr(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        let syn::Meta::NameValue(nv) = &attr.meta else {
            return None;
        };
        if !nv.path.is_ident("weaveffi_callback") {
            return None;
        }
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = &nv.value
        else {
            return None;
        };
        Some(s.value())
    })
}

fn is_box_dyn_fn(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    let Some(seg) = tp.path.segments.last() else {
        return false;
    };
    if seg.ident != "Box" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
        return false;
    };
    let Some(syn::GenericArgument::Type(syn::Type::TraitObject(to))) = args.args.first() else {
        return false;
    };
    to.bounds.iter().any(|b| {
        matches!(b, syn::TypeParamBound::Trait(tb)
            if tb.path.segments.last().is_some_and(
                |s| s.ident == "Fn" || s.ident == "FnMut" || s.ident == "FnOnce"))
    })
}

#[derive(Default)]
struct ExportArgs {
    r#async: bool,
    cancellable: bool,
    since: Option<String>,
}

fn parse_export_args(attrs: &[syn::Attribute]) -> Result<ExportArgs> {
    let mut out = ExportArgs::default();
    for attr in attrs {
        if !attr.path().is_ident("weaveffi_export") {
            continue;
        }
        if !matches!(attr.meta, syn::Meta::List(_)) {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("async") {
                out.r#async = true;
            } else if meta.path.is_ident("cancellable") {
                out.cancellable = true;
            } else if meta.path.is_ident("since") {
                let lit: syn::LitStr = meta.value()?.parse()?;
                out.since = Some(lit.value());
            } else {
                return Err(meta.error("unknown weaveffi_export argument"));
            }
            Ok(())
        })
        .map_err(|e| eyre!("weaveffi_export: {e}"))?;
    }
    Ok(out)
}

fn extract_deprecated(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("deprecated") {
            return None;
        }
        match &attr.meta {
            syn::Meta::Path(_) => Some(String::new()),
            syn::Meta::NameValue(nv) => {
                let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                else {
                    return None;
                };
                Some(s.value())
            }
            syn::Meta::List(_) => {
                let mut note = None;
                let _ = attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("note") {
                        let lit: syn::LitStr = meta.value()?.parse()?;
                        note = Some(lit.value());
                    }
                    Ok(())
                });
                note
            }
        }
    })
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

fn map_type(ty: &syn::Type) -> Result<TypeRef> {
    if let syn::Type::Reference(r) = ty {
        return match r.elem.as_ref() {
            inner if is_ident(inner, "str") => Ok(TypeRef::BorrowedStr),
            syn::Type::Slice(slice) if is_ident(&slice.elem, "u8") => Ok(TypeRef::BorrowedBytes),
            _ => bail!("unsupported reference type; only &str and &[u8] are supported"),
        };
    }
    if let syn::Type::ImplTrait(impl_trait) = ty {
        for bound in &impl_trait.bounds {
            let syn::TypeParamBound::Trait(tb) = bound else {
                continue;
            };
            let Some(seg) = tb.path.segments.last() else {
                continue;
            };
            if seg.ident != "Iterator" {
                continue;
            }
            let syn::PathArguments::AngleBracketed(args) = &seg.arguments else {
                continue;
            };
            for arg in &args.args {
                if let syn::GenericArgument::AssocType(at) = arg {
                    if at.ident == "Item" {
                        return Ok(TypeRef::Iterator(Box::new(map_type(&at.ty)?)));
                    }
                }
            }
        }
        bail!("unsupported impl Trait; only impl Iterator<Item = T> is supported");
    }
    if is_box_dyn_fn(ty) {
        bail!("Box<dyn Fn(...)> params must carry a #[weaveffi_callback = \"Name\"] attribute");
    }
    let syn::Type::Path(type_path) = ty else {
        bail!("unsupported type syntax");
    };
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
        "Handle" if matches!(seg.arguments, syn::PathArguments::AngleBracketed(_)) => {
            let inner = single_generic_arg(seg)?;
            let name = simple_type_name(inner)
                .ok_or_else(|| eyre!("Handle: expected a named struct type argument"))?;
            Ok(TypeRef::TypedHandle(name))
        }
        other => Ok(TypeRef::Struct(other.to_string())),
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

fn extract_function(item: &syn::ItemFn) -> Result<Function> {
    let name = item.sig.ident.to_string();
    let params = item
        .sig
        .inputs
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
            let ty = if let Some(name) = extract_callback_name_attr(&pt.attrs) {
                TypeRef::Callback(name)
            } else if let Some(name) = extract_typed_handle_attr(&pt.attrs) {
                TypeRef::TypedHandle(name)
            } else {
                map_type(&pt.ty)?
            };
            Ok(Param {
                name: param_name,
                ty,
                mutable: false,
            })
        })
        .collect::<Result<_>>()?;

    let returns = match &item.sig.output {
        syn::ReturnType::Default => None,
        syn::ReturnType::Type(_, ty) => Some(map_type(ty)?),
    };

    let export_args = parse_export_args(&item.attrs)?;

    Ok(Function {
        name,
        params,
        returns,
        doc: extract_doc(&item.attrs),
        r#async: export_args.r#async,
        cancellable: export_args.cancellable,
        deprecated: extract_deprecated(&item.attrs),
        since: export_args.since,
    })
}

fn extract_callback(item: &syn::ItemFn) -> Result<CallbackDef> {
    let name = item.sig.ident.to_string();
    let params = item
        .sig
        .inputs
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
            Ok(Param {
                name: param_name,
                ty: map_type(&pt.ty)?,
                mutable: false,
            })
        })
        .collect::<Result<_>>()?;

    let returns = match &item.sig.output {
        syn::ReturnType::Default => None,
        syn::ReturnType::Type(_, ty) => Some(map_type(ty)?),
    };

    Ok(CallbackDef {
        name,
        params,
        returns,
        doc: extract_doc(&item.attrs),
    })
}

fn parse_struct_args(attrs: &[syn::Attribute]) -> Result<bool> {
    let mut builder = false;
    for attr in attrs {
        if !attr.path().is_ident("weaveffi_struct") {
            continue;
        }
        if !matches!(attr.meta, syn::Meta::List(_)) {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("builder") {
                builder = true;
            } else {
                return Err(meta.error("unknown weaveffi_struct argument"));
            }
            Ok(())
        })
        .map_err(|e| eyre!("weaveffi_struct: {e}"))?;
    }
    Ok(builder)
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
                    default: extract_default_attr(&f.attrs)?,
                })
            })
            .collect::<Result<_>>()?,
        _ => bail!("only named fields are supported for #[weaveffi_struct]"),
    };

    Ok(StructDef {
        name,
        doc: extract_doc(&item.attrs),
        fields,
        builder: parse_struct_args(&item.attrs)?,
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

    if let Some((_, items)) = &item_mod.content {
        for item in items {
            match item {
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
        listeners: vec![],
        errors: None,
        modules: vec![],
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
    fn extract_struct_with_builder_attribute() {
        let src = r#"
            mod m {
                #[weaveffi_struct(builder)]
                struct Config {
                    host: String,
                    port: i32,
                }

                #[weaveffi_struct]
                struct Point {
                    x: f64,
                    y: f64,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let structs = &api.modules[0].structs;
        assert_eq!(structs.len(), 2);
        assert_eq!(structs[0].name, "Config");
        assert!(structs[0].builder);
        assert_eq!(structs[1].name, "Point");
        assert!(!structs[1].builder);
    }

    #[test]
    fn extract_struct_with_unknown_arg_errors() {
        let src = r#"
            mod m {
                #[weaveffi_struct(wat)]
                struct Bad { x: i32 }
            }
        "#;
        let err = extract_api_from_rust(src).unwrap_err();
        assert!(
            format!("{err}").contains("weaveffi_struct"),
            "unexpected error: {err}"
        );
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
    fn extract_struct_field_default() {
        let src = r#"
            mod m {
                #[weaveffi_struct]
                struct Contact {
                    name: String,
                    #[weaveffi_default = "0"]
                    age: i32,
                    #[weaveffi_default = "\"unknown\""]
                    nickname: String,
                    #[weaveffi_default = "true"]
                    active: bool,
                }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let fields = &api.modules[0].structs[0].fields;
        assert_eq!(fields[0].default, None);
        assert_eq!(
            fields[1].default,
            Some(serde_yaml::Value::Number(serde_yaml::Number::from(0)))
        );
        assert_eq!(
            fields[2].default,
            Some(serde_yaml::Value::String("unknown".to_string()))
        );
        assert_eq!(fields[3].default, Some(serde_yaml::Value::Bool(true)));
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
    fn extract_typed_handle_param() {
        let src = r#"
            mod sessions {
                #[weaveffi_export]
                fn open(h: weaveffi_handle::Handle<Session>) {}

                #[weaveffi_export]
                fn close(#[weaveffi_typed_handle = "Session"] h: u64) {}

                #[weaveffi_export]
                fn use_bare(h: Handle<Session>) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let fns = &api.modules[0].functions;
        assert_eq!(
            fns[0].params[0].ty,
            TypeRef::TypedHandle("Session".to_string())
        );
        assert_eq!(
            fns[1].params[0].ty,
            TypeRef::TypedHandle("Session".to_string())
        );
        assert_eq!(
            fns[2].params[0].ty,
            TypeRef::TypedHandle("Session".to_string())
        );
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
                fn greet(name: &str) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::BorrowedStr);
    }

    #[test]
    fn extract_borrowed_bytes_param() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn hash(data: &[u8]) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.params[0].ty, TypeRef::BorrowedBytes);
    }

    #[test]
    fn extract_async_function() {
        let src = r#"
            mod m {
                #[weaveffi_export(async)]
                fn fetch(url: String) -> String { String::new() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.r#async);
        assert!(!f.cancellable);
        assert_eq!(f.deprecated, None);
        assert_eq!(f.since, None);
    }

    #[test]
    fn extract_cancellable_function() {
        let src = r#"
            mod m {
                #[weaveffi_export(cancellable)]
                fn download(url: String) -> Vec<u8> { vec![] }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert!(f.cancellable);
        assert!(!f.r#async);
    }

    #[test]
    fn extract_deprecated_function() {
        let src = r#"
            mod m {
                #[deprecated(note = "use add_v2 instead")]
                #[weaveffi_export]
                fn add(a: i32, b: i32) -> i32 { a + b }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.deprecated.as_deref(), Some("use add_v2 instead"));
    }

    #[test]
    fn extract_since_attribute() {
        let src = r#"
            mod m {
                #[weaveffi_export(since = "0.5.0")]
                fn new_api(x: i32) -> i32 { x }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let f = &api.modules[0].functions[0];
        assert_eq!(f.since.as_deref(), Some("0.5.0"));
    }

    #[test]
    fn extract_iterator_return() {
        let src = r#"
            mod m {
                #[weaveffi_export]
                fn ids() -> impl Iterator<Item = i32> { std::iter::empty() }

                #[weaveffi_export]
                fn names() -> impl Iterator<Item = String> { std::iter::empty() }
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let fns = &api.modules[0].functions;
        assert_eq!(
            fns[0].returns,
            Some(TypeRef::Iterator(Box::new(TypeRef::I32)))
        );
        assert_eq!(
            fns[1].returns,
            Some(TypeRef::Iterator(Box::new(TypeRef::StringUtf8)))
        );
    }

    #[test]
    fn extract_callback_param_with_attribute() {
        let src = r#"
            mod events {
                #[weaveffi_callback]
                fn OnData(payload: String) -> bool { unreachable!() }

                #[weaveffi_export]
                fn subscribe(
                    #[weaveffi_callback = "OnData"]
                    handler: Box<dyn Fn(String) -> bool>,
                ) {}
            }
        "#;
        let api = extract_api_from_rust(src).unwrap();
        let m = &api.modules[0];

        assert_eq!(m.callbacks.len(), 1);
        let cb = &m.callbacks[0];
        assert_eq!(cb.name, "OnData");
        assert_eq!(cb.params.len(), 1);
        assert_eq!(cb.params[0].name, "payload");
        assert_eq!(cb.params[0].ty, TypeRef::StringUtf8);
        assert_eq!(cb.returns, Some(TypeRef::Bool));

        assert_eq!(m.functions.len(), 1);
        let f = &m.functions[0];
        assert_eq!(f.name, "subscribe");
        assert_eq!(f.params[0].name, "handler");
        assert_eq!(f.params[0].ty, TypeRef::Callback("OnData".to_string()));
    }

    #[test]
    fn box_dyn_fn_without_attribute_errors() {
        let src = r#"
            mod events {
                #[weaveffi_export]
                fn subscribe(handler: Box<dyn Fn(String) -> bool>) {}
            }
        "#;
        let err = extract_api_from_rust(src).unwrap_err();
        assert!(
            format!("{err}").contains("weaveffi_callback"),
            "unexpected error: {err}"
        );
    }
}
