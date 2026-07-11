//! Type-reference resolution: rewrites parsed [`TypeRef::Named`] references
//! into their resolved kinds ([`TypeRef::Record`], [`TypeRef::RichEnum`],
//! [`TypeRef::Enum`], or [`TypeRef::Interface`]) and qualifies cross-module
//! references with the owning module's dot-joined path. Runs after the rule
//! checks pass, so every name is known to resolve.

use std::collections::BTreeMap;
use weaveffi_ir::ir::{Api, Function, Module, TypeRef};

/// How a bare type name resolves: a record, a C-style enum, an algebraic
/// (rich) enum, or an interface. The kinds differ in their C ABI lowering: a
/// C-style enum is a by-value integer ([`TypeRef::Enum`]), a record and a rich
/// enum are opaque object pointers ([`TypeRef::Record`] /
/// [`TypeRef::RichEnum`]), and an interface is an opaque object reference with
/// its own ownership convention ([`TypeRef::Interface`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypeKind {
    Record,
    Enum,
    RichEnum,
    Interface,
}

impl TypeKind {
    /// Build the resolved [`TypeRef`] for this kind with the given (possibly
    /// module-qualified) name.
    fn type_ref(self, name: String) -> TypeRef {
        match self {
            TypeKind::Record => TypeRef::Record(name),
            TypeKind::Enum => TypeRef::Enum(name),
            TypeKind::RichEnum => TypeRef::RichEnum(name),
            TypeKind::Interface => TypeRef::Interface(name),
        }
    }
}

/// Resolve every type reference in `api` in place.
///
/// Every [`TypeRef::Named`] is rewritten to the variant matching its
/// declaration: [`TypeRef::Record`] for structs, [`TypeRef::RichEnum`] for
/// algebraic enums, [`TypeRef::Enum`] for C-style enums, and
/// [`TypeRef::Interface`] for interfaces. Cross-module references are
/// qualified with the owning module's dot-joined path. Runs after the rule
/// checks pass, so no unresolved `Named` reference remains afterwards.
pub fn resolve_type_refs(api: &mut Api) {
    let mut global_types: BTreeMap<String, (String, TypeKind)> = BTreeMap::new();
    for module in &api.modules {
        index_module_types(module, "", &mut global_types);
    }

    for module in &mut api.modules {
        resolve_module_type_refs(module, "", &global_types);
    }
}

/// Recursively index every struct/enum/interface in the module tree by its
/// bare name, recording the owner's *dot-joined* module path (e.g.
/// `graphics.shapes`) so cross-module and nested references can be
/// auto-qualified. Bare names are globally unique (validation enforces it),
/// so the map has one entry per name.
fn index_module_types(
    module: &Module,
    parent_path: &str,
    out: &mut BTreeMap<String, (String, TypeKind)>,
) {
    let path = join_module_path(parent_path, &module.name);
    for s in &module.structs {
        out.entry(s.name.clone())
            .or_insert((path.clone(), TypeKind::Record));
    }
    for e in &module.enums {
        let kind = if e.is_rich() {
            TypeKind::RichEnum
        } else {
            TypeKind::Enum
        };
        out.entry(e.name.clone()).or_insert((path.clone(), kind));
    }
    for i in &module.interfaces {
        out.entry(i.name.clone())
            .or_insert((path.clone(), TypeKind::Interface));
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
    let ctx = ResolveCtx {
        current_module: &module_path,
        global_types,
    };
    fn resolve_callable(f: &mut Function, ctx: &ResolveCtx<'_>) {
        for p in &mut f.params {
            resolve_single_type_ref(&mut p.ty, ctx);
        }
        if let Some(ret) = &mut f.returns {
            resolve_single_type_ref(ret, ctx);
        }
    }
    for f in &mut module.functions {
        resolve_callable(f, &ctx);
    }
    for i in &mut module.interfaces {
        for c in &mut i.constructors {
            resolve_callable(c, &ctx);
        }
        for m in &mut i.methods {
            resolve_callable(m, &ctx);
        }
        for s in &mut i.statics {
            resolve_callable(s, &ctx);
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
    for cb in &mut module.callbacks {
        for p in &mut cb.params {
            resolve_single_type_ref(&mut p.ty, &ctx);
        }
    }
    for child in &mut module.modules {
        resolve_module_type_refs(child, &module_path, global_types);
    }
}

/// Bundled lookup tables for resolving a single type reference within one
/// module, so the recursive resolver does not thread several parameters.
struct ResolveCtx<'a> {
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
        // A parsed bare name resolves against the global type index; a
        // reference declared outside the current module is additionally
        // qualified with the owner's dot-joined path so the C ABI lowering
        // and every generator emit the owner's symbol prefix.
        TypeRef::Named(name) => {
            if let Some((mod_name, kind)) = ctx.global_types.get(name.as_str()) {
                let resolved_name = if mod_name == ctx.current_module {
                    std::mem::take(name)
                } else {
                    format!("{mod_name}.{name}")
                };
                *ty = kind.type_ref(resolved_name);
            }
        }
        // A typed handle's target may live in a different module (e.g.
        // `handle<Store>` in a `kv.stats` submodule referring to `kv.Store`);
        // qualify it to the owner's path so the C ABI lowering emits the
        // owner's symbol prefix rather than the referrer's.
        TypeRef::TypedHandle(name) => {
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

/// Locate a struct, enum, or interface by its bare `name` anywhere in `api`.
///
/// Returns the owning module's dot-joined path and a boolean that is `true`
/// when the match is an enum and `false` when it is a struct or interface, or
/// `None` if no type with that name exists. Bare names are globally unique
/// (validation enforces it), so at most one declaration can match.
pub fn find_type_in_api(api: &Api, name: &str) -> Option<(String, bool)> {
    fn search(module: &Module, parent_path: &str, name: &str) -> Option<(String, bool)> {
        let path = join_module_path(parent_path, &module.name);
        if module.structs.iter().any(|s| s.name == name)
            || module.interfaces.iter().any(|i| i.name == name)
        {
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
