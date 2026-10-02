//! The resolved IR: a validated [`Api`] paired with the type index that turns
//! its written [`TypeRef`]s into resolved [`Ty`]s.
//!
//! WeaveFFI keeps two distinct representations of an API:
//!
//! * the **IDL document**: the [`Api`] tree exactly as parsed from
//!   YAML/JSON/TOML (or extracted from annotated Rust), in which every
//!   user-type reference is a [`TypeRef::Named`] string; and
//! * the **binding model** ([`crate::model::BindingModel`]): the lowered view
//!   generators consume, in which every type is a [`Ty`] whose kind (record,
//!   enum, interface) and owning module are known.
//!
//! [`ResolvedApi`] is the bridge. The only checked way to obtain one is
//! [`validate_api`](crate::validate::validate_api), which proves every rule
//! holds; [`ResolvedApi::resolve`] then maps a written reference to its
//! resolved type against the index built from the document's declarations.
//! The document itself is never mutated, so an IDL always round-trips.

use std::collections::BTreeMap;

use crate::ir::{Api, Module, TypeRef};

use crate::model::Ty;
use crate::pkg::Identity;

/// What kind of declaration a bare type name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeKind {
    /// A `structs:` entry.
    Record,
    /// An `enums:` entry with no payload-carrying variant.
    Enum,
    /// An `enums:` entry with at least one payload-carrying variant.
    RichEnum,
    /// An `interfaces:` entry.
    Interface,
    /// A `callback_interfaces:` entry.
    CallbackInterface,
}

/// Where a type is declared: the owning module's dot-joined path and its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    /// Dot-joined path of the declaring module (e.g. `graphics.shapes`).
    pub module_path: String,
    /// The declaration kind.
    pub kind: TypeKind,
}

/// A validated API whose type references can be resolved.
///
/// This is the input to [`BindingModel::build`](crate::model::BindingModel::build).
/// The checked way to obtain one is
/// [`validate_api`](crate::validate::validate_api). [`assume_valid`](Self::assume_valid)
/// exists for the `#[weaveffi::module]` proc-macro, which expands one module
/// tree in isolation, and for tests that build well-formed trees by hand.
///
/// `ResolvedApi` dereferences to [`Api`] for read access; there is no mutable
/// access, so the index can never go stale. It also carries the library's
/// [`Identity`], which the CLI attaches with [`with_identity`](Self::with_identity)
/// and every generator reads names from.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedApi {
    api: Api,
    types: BTreeMap<String, TypeDecl>,
    identity: Identity,
}

impl ResolvedApi {
    /// Attach the library's resolved identity.
    #[must_use]
    pub fn with_identity(mut self, identity: Identity) -> Self {
        self.identity = identity;
        self
    }

    /// The library's identity: its name, C symbol prefix, native library
    /// name, and package metadata.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Wrap a document without running validation.
    ///
    /// Type names that resolve to no declaration become [`Ty::Record`]
    /// references named exactly as written; [`unresolved`](Self::unresolved)
    /// lists them. The proc-macro relies on this to expand one module tree
    /// while a sibling tree declares some of the types it mentions, and
    /// asserts at compile time that each such type really crosses as a value
    /// buffer.
    #[doc(hidden)]
    pub fn assume_valid(api: Api) -> Self {
        let mut types = BTreeMap::new();
        for module in &api.modules {
            index_module(module, "", &mut types);
        }
        Self {
            api,
            types,
            identity: Identity::default(),
        }
    }

    /// The underlying document.
    pub fn api(&self) -> &Api {
        &self.api
    }

    /// Look up the declaration a bare type name refers to.
    pub fn declaration(&self, name: &str) -> Option<&TypeDecl> {
        self.types.get(bare(name))
    }

    /// Every type name the document references that no declaration in it
    /// provides, deduplicated and sorted. Always empty for an API that
    /// passed validation.
    pub fn unresolved(&self) -> Vec<String> {
        let mut out = std::collections::BTreeSet::new();
        let mut visit = |ty: &TypeRef| {
            ty.walk(&mut |t| {
                if let TypeRef::Named(n) = t {
                    if self.declaration(n).is_none() {
                        out.insert(n.clone());
                    }
                }
            });
        };
        for_each_type_ref(&self.api.modules, &mut visit);
        out.into_iter().collect()
    }

    /// The absolute, dot-joined name of the type a reference denotes
    /// (`kv.Store`, `shapes.geo.Point`). Unknown names come back as written.
    pub fn qualified_name(&self, name: &str) -> String {
        match self.declaration(name) {
            Some(decl) => format!("{}.{}", decl.module_path, bare(name)),
            None => name.to_string(),
        }
    }

    /// Map a written type reference to its resolved type. User types carry
    /// their kind and absolute dot-joined name, so two references to one
    /// declaration compare equal wherever they are written.
    pub fn resolve(&self, ty: &TypeRef) -> Ty {
        match ty {
            TypeRef::I8 => Ty::I8,
            TypeRef::I16 => Ty::I16,
            TypeRef::I32 => Ty::I32,
            TypeRef::I64 => Ty::I64,
            TypeRef::U8 => Ty::U8,
            TypeRef::U16 => Ty::U16,
            TypeRef::U32 => Ty::U32,
            TypeRef::U64 => Ty::U64,
            TypeRef::F32 => Ty::F32,
            TypeRef::F64 => Ty::F64,
            TypeRef::Bool => Ty::Bool,
            TypeRef::StringUtf8 => Ty::StringUtf8,
            TypeRef::Bytes => Ty::Bytes,
            TypeRef::Named(name) => {
                let qualified = self.qualified_name(name);
                match self.declaration(name).map(|d| d.kind) {
                    Some(TypeKind::Enum) => Ty::Enum(qualified),
                    Some(TypeKind::RichEnum) => Ty::RichEnum(qualified),
                    Some(TypeKind::Interface) => Ty::Interface(qualified),
                    Some(TypeKind::CallbackInterface) => Ty::CallbackInterface(qualified),
                    Some(TypeKind::Record) | None => Ty::Record(qualified),
                }
            }
            TypeRef::Optional(inner) => Ty::Optional(Box::new(self.resolve(inner))),
            TypeRef::List(inner) => Ty::List(Box::new(self.resolve(inner))),
            TypeRef::Map(k, v) => Ty::Map(Box::new(self.resolve(k)), Box::new(self.resolve(v))),
            TypeRef::Iterator(inner) => Ty::Iterator(Box::new(self.resolve(inner))),
        }
    }
}

/// Call `f` on every type reference written anywhere in `modules`.
pub(crate) fn for_each_type_ref(modules: &[Module], f: &mut dyn FnMut(&TypeRef)) {
    fn fns(fs: &[crate::ir::Function], f: &mut dyn FnMut(&TypeRef)) {
        for func in fs {
            for p in &func.params {
                f(&p.ty);
            }
            if let Some(r) = &func.returns {
                f(r);
            }
        }
    }
    for m in modules {
        fns(&m.functions, f);
        for i in &m.interfaces {
            fns(&i.constructors, f);
            fns(&i.methods, f);
            fns(&i.statics, f);
        }
        for c in &m.callback_interfaces {
            fns(&c.methods, f);
        }
        for s in &m.structs {
            for field in &s.fields {
                f(&field.ty);
            }
        }
        for e in &m.enums {
            for v in &e.variants {
                for field in &v.fields {
                    f(&field.ty);
                }
            }
        }
        if let Some(d) = &m.errors {
            for c in &d.codes {
                for field in &c.fields {
                    f(&field.ty);
                }
            }
        }
        for_each_type_ref(&m.modules, f);
    }
}

fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

fn index_module(module: &Module, parent: &str, out: &mut BTreeMap<String, TypeDecl>) {
    let path = if parent.is_empty() {
        module.name.clone()
    } else {
        format!("{parent}.{}", module.name)
    };
    let mut add = |name: &str, kind: TypeKind| {
        out.entry(name.to_string()).or_insert(TypeDecl {
            module_path: path.clone(),
            kind,
        });
    };
    for s in &module.structs {
        add(&s.name, TypeKind::Record);
    }
    for e in &module.enums {
        let kind = if e.is_rich() {
            TypeKind::RichEnum
        } else {
            TypeKind::Enum
        };
        add(&e.name, kind);
    }
    for i in &module.interfaces {
        add(&i.name, TypeKind::Interface);
    }
    for c in &module.callback_interfaces {
        add(&c.name, TypeKind::CallbackInterface);
    }
    for child in &module.modules {
        index_module(child, &path, out);
    }
}

impl std::ops::Deref for ResolvedApi {
    type Target = Api;

    fn deref(&self) -> &Api {
        &self.api
    }
}

impl AsRef<Api> for ResolvedApi {
    fn as_ref(&self) -> &Api {
        &self.api
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        CallbackInterfaceDef, EnumDef, EnumVariant, Function, InterfaceDef, StructDef, StructField,
        CURRENT_SCHEMA_VERSION,
    };

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            doc: None,
            functions: vec![],
            interfaces: vec![],
            callback_interfaces: vec![],
            structs: vec![],
            enums: vec![],
            errors: None,
            modules: vec![],
        }
    }

    fn api() -> ResolvedApi {
        let shared = Module {
            enums: vec![
                EnumDef {
                    name: "Status".into(),
                    doc: None,
                    deprecated: None,
                    variants: vec![EnumVariant {
                        name: "Ok".into(),
                        value: 0,
                        doc: None,
                        fields: vec![],
                    }],
                },
                EnumDef {
                    name: "Shape".into(),
                    doc: None,
                    deprecated: None,
                    variants: vec![EnumVariant {
                        name: "Circle".into(),
                        value: 0,
                        doc: None,
                        fields: vec![StructField {
                            name: "r".into(),
                            ty: TypeRef::F64,
                            doc: None,
                        }],
                    }],
                },
            ],
            structs: vec![StructDef {
                name: "Point".into(),
                doc: None,
                deprecated: None,
                fields: vec![],
            }],
            modules: vec![Module {
                interfaces: vec![InterfaceDef {
                    name: "Store".into(),
                    doc: None,
                    deprecated: None,
                    constructors: vec![],
                    methods: vec![],
                    statics: vec![],
                }],
                callback_interfaces: vec![CallbackInterfaceDef {
                    name: "Watcher".into(),
                    doc: None,
                    deprecated: None,
                    methods: vec![Function {
                        name: "on_change".into(),
                        params: vec![],
                        returns: None,
                        doc: None,
                        throws: false,
                        r#async: false,
                        cancellable: false,
                        deprecated: None,
                    }],
                }],
                ..module("inner")
            }],
            ..module("shared")
        };
        ResolvedApi::assume_valid(Api {
            version: CURRENT_SCHEMA_VERSION.into(),
            modules: vec![shared, module("orders")],
        })
    }

    #[test]
    fn kinds_and_qualification() {
        let api = api();
        assert_eq!(
            api.resolve(&TypeRef::Named("Status".into())),
            Ty::Enum("shared.Status".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Status".into())),
            Ty::Enum("shared.Status".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Shape".into())),
            Ty::RichEnum("shared.Shape".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Store".into())),
            Ty::Interface("shared.inner.Store".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Store".into())),
            Ty::Interface("shared.inner.Store".into())
        );
        // Already-qualified spellings normalize to the owner's path.
        assert_eq!(
            api.resolve(&TypeRef::Named("shared.Point".into())),
            Ty::Record("shared.Point".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Watcher".into())),
            Ty::CallbackInterface("shared.inner.Watcher".into())
        );
        assert_eq!(
            api.declaration("Watcher").unwrap().kind,
            TypeKind::CallbackInterface
        );
    }

    #[test]
    fn composites_resolve_recursively() {
        let api = api();
        let ty = TypeRef::List(Box::new(TypeRef::Map(
            Box::new(TypeRef::StringUtf8),
            Box::new(TypeRef::Optional(Box::new(TypeRef::Named("Point".into())))),
        )));
        assert_eq!(
            api.resolve(&ty),
            Ty::List(Box::new(Ty::Map(
                Box::new(Ty::StringUtf8),
                Box::new(Ty::Optional(Box::new(Ty::Record("shared.Point".into()))))
            )))
        );
    }

    #[test]
    fn unknown_names_fall_back_to_records() {
        let api = api();
        assert_eq!(
            api.resolve(&TypeRef::Named("Elsewhere".into())),
            Ty::Record("Elsewhere".into())
        );
        assert!(api.unresolved().is_empty());
        assert!(api.declaration("Elsewhere").is_none());
        assert_eq!(api.declaration("Point").unwrap().kind, TypeKind::Record);
    }
}
