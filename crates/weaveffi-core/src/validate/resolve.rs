//! Type-reference resolution: rewrites bare struct names into enum
//! references and qualifies cross-module references with the owning
//! module's dot-joined path. Runs after the rule checks pass.

use std::collections::{BTreeMap, BTreeSet};
use weaveffi_ir::ir::{Api, Module, TypeRef};

/// How a bare type name resolves: a struct, a C-style enum, or an algebraic
/// (rich) enum. The two enum kinds differ in their C ABI lowering — a C-style
/// enum is a by-value integer ([`TypeRef::Enum`]) while a rich enum is an
/// opaque object pointer ([`TypeRef::RichEnum`]) — so the resolver must know
/// which to emit for every reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeKind {
    Struct,
    Enum,
    RichEnum,
}

pub fn resolve_type_refs(api: &mut Api) {
    let mut global_types: BTreeMap<String, (String, TypeKind)> = BTreeMap::new();
    for module in &api.modules {
        index_module_types(module, "", &mut global_types);
    }

    for module in &mut api.modules {
        resolve_module_type_refs(module, "", &global_types);
    }
}

/// Recursively index every struct/enum in the module tree by its bare name,
/// recording the owner's *dot-joined* module path (e.g. `graphics.shapes`) so
/// cross-module and nested references can be auto-qualified. First definition
/// wins on a name clash (pre-existing behavior).
fn index_module_types(
    module: &Module,
    parent_path: &str,
    out: &mut BTreeMap<String, (String, TypeKind)>,
) {
    let path = join_module_path(parent_path, &module.name);
    for s in &module.structs {
        out.entry(s.name.clone())
            .or_insert((path.clone(), TypeKind::Struct));
    }
    for e in &module.enums {
        let kind = if e.is_rich() {
            TypeKind::RichEnum
        } else {
            TypeKind::Enum
        };
        out.entry(e.name.clone()).or_insert((path.clone(), kind));
    }
    for child in &module.modules {
        index_module_types(child, &path, out);
    }
}

/// Resolve every type reference inside `module` (and recursively its
/// submodules), qualifying cross-module references against `global_types`.
/// `current_module` is tracked as a dot-joined path so multi-level nesting
/// resolves correctly.
fn resolve_module_type_refs(
    module: &mut Module,
    parent_path: &str,
    global_types: &BTreeMap<String, (String, TypeKind)>,
) {
    let module_path = join_module_path(parent_path, &module.name);
    // Only *C-style* enums are rewritten to `TypeRef::Enum` (by-value lowering).
    // A rich (algebraic) enum is left as `TypeRef::Struct` because it crosses
    // the ABI as an opaque object pointer, exactly like a struct — so it does
    // not belong in `local_enum_names`.
    let local_enum_names: BTreeSet<String> = module
        .enums
        .iter()
        .filter(|e| !e.is_rich())
        .map(|e| e.name.clone())
        .collect();
    let local_struct_names: BTreeSet<String> =
        module.structs.iter().map(|s| s.name.clone()).collect();
    let ctx = ResolveCtx {
        local_enum_names: &local_enum_names,
        local_struct_names: &local_struct_names,
        current_module: &module_path,
        global_types,
    };
    for f in &mut module.functions {
        for p in &mut f.params {
            resolve_single_type_ref(&mut p.ty, &ctx);
        }
        if let Some(ret) = &mut f.returns {
            resolve_single_type_ref(ret, &ctx);
        }
    }
    for s in &mut module.structs {
        for field in &mut s.fields {
            resolve_single_type_ref(&mut field.ty, &ctx);
        }
    }
    // A rich enum's variant fields are themselves type references that must be
    // resolved (e.g. a variant field of struct or sibling-enum type).
    for e in &mut module.enums {
        for v in &mut e.variants {
            for field in &mut v.fields {
                resolve_single_type_ref(&mut field.ty, &ctx);
            }
        }
    }
    for child in &mut module.modules {
        resolve_module_type_refs(child, &module_path, global_types);
    }
}

/// Bundled lookup tables for resolving a single type reference within one
/// module, so the recursive resolver does not thread several parameters.
struct ResolveCtx<'a> {
    local_enum_names: &'a BTreeSet<String>,
    local_struct_names: &'a BTreeSet<String>,
    current_module: &'a str,
    global_types: &'a BTreeMap<String, (String, TypeKind)>,
}

/// Join a parent module path with a child segment using `.`. A top-level
/// module (empty parent) is just its own name, preserving the single-segment
/// behavior that existed before nested resolution.
fn join_module_path(parent_path: &str, name: &str) -> String {
    if parent_path.is_empty() {
        name.to_string()
    } else {
        format!("{parent_path}.{name}")
    }
}

fn resolve_single_type_ref(ty: &mut TypeRef, ctx: &ResolveCtx<'_>) {
    match ty {
        // A bare reference to a local C-style enum becomes a by-value `Enum`.
        // A local rich enum or struct is left as `Struct` (opaque pointer).
        TypeRef::Struct(name) if ctx.local_enum_names.contains(name.as_str()) => {
            let name = std::mem::take(name);
            *ty = TypeRef::Enum(name);
        }
        TypeRef::Struct(name) if !ctx.local_struct_names.contains(name.as_str()) => {
            if let Some((mod_name, kind)) = ctx.global_types.get(name.as_str()) {
                if mod_name != ctx.current_module {
                    let qualified = format!("{mod_name}.{name}");
                    match kind {
                        // C-style enum: by-value reference.
                        TypeKind::Enum => *ty = TypeRef::Enum(qualified),
                        // Struct or rich enum: opaque-pointer reference, kept
                        // as `Struct` with the owner's qualified path.
                        TypeKind::Struct | TypeKind::RichEnum => *name = qualified,
                    }
                }
            }
        }
        // A typed handle's target is always a struct. When that struct lives in
        // a different module (e.g. `handle<Store>` in a `kv.stats` submodule
        // referring to `kv.Store`), qualify it to the owner's path so the C ABI
        // lowering and every generator emit the owner's symbol prefix rather
        // than the referrer's. Mirrors the `Struct` arm above.
        TypeRef::TypedHandle(name) if !ctx.local_struct_names.contains(name.as_str()) => {
            if let Some((mod_name, _kind)) = ctx.global_types.get(name.as_str()) {
                if mod_name != ctx.current_module {
                    *name = format!("{mod_name}.{name}");
                }
            }
        }
        TypeRef::Optional(inner) | TypeRef::List(inner) | TypeRef::Iterator(inner) => {
            resolve_single_type_ref(inner, ctx);
        }
        TypeRef::Map(k, v) => {
            resolve_single_type_ref(k, ctx);
            resolve_single_type_ref(v, ctx);
        }
        _ => {}
    }
}

pub fn find_type_in_api(api: &Api, name: &str) -> Option<(String, bool)> {
    fn search(module: &Module, parent_path: &str, name: &str) -> Option<(String, bool)> {
        let path = join_module_path(parent_path, &module.name);
        if module.structs.iter().any(|s| s.name == name) {
            return Some((path, false));
        }
        if module.enums.iter().any(|e| e.name == name) {
            return Some((path, true));
        }
        module
            .modules
            .iter()
            .find_map(|child| search(child, &path, name))
    }
    api.modules
        .iter()
        .find_map(|module| search(module, "", name))
}
