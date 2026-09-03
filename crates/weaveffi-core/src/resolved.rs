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

use weaveffi_ir::ir::{Api, Module, TypeRef};

use crate::model::Ty;
use crate::pkg::Package;

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
/// It cannot be constructed from an unchecked document except through
/// [`assume_valid`](Self::assume_valid), which exists for the two construction
/// sites that cannot route through validation: the `#[weaveffi::module]`
/// proc-macro (which expands one module in isolation) and tests that build
/// well-formed trees by hand.
///
/// `ResolvedApi` dereferences to [`Api`] for read access; there is no mutable
/// access, so the index can never go stale.
///
/// The CLI also attaches the project's [`Package`] identity (from the
/// `[package]` table of `weaveffi.toml`) with [`with_package`](Self::with_package),
/// so every backend resolves manifest metadata from one place.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedApi {
    api: Api,
    types: BTreeMap<String, TypeDecl>,
    package: Option<Package>,
}

impl ResolvedApi {
    /// Attach the project's package identity. Generators read it through
    /// [`pkg::resolve`](crate::pkg::resolve).
    #[must_use]
    pub fn with_package(mut self, package: Package) -> Self {
        self.package = Some(package);
        self
    }

    /// The project's package identity, when the CLI attached one.
    pub fn package(&self) -> Option<&Package> {
        self.package.as_ref()
    }

    /// Wrap an [`Api`] the caller asserts satisfies every validation rule,
    /// indexing its declarations for [`resolve`](Self::resolve).
    ///
    /// A reference to a name the document does not declare resolves as a
    /// [`Ty::Record`]. That fallback exists for the proc-macro, which expands
    /// a single `#[weaveffi::module]` and may legitimately reference a value
    /// type declared in a sibling module it cannot see; a record and a rich
    /// enum share the same buffered ABI shape, so the emitted thunk is correct
    /// for both, and `rustc` rejects a genuinely undefined type. Documents
    /// that went through [`validate_api`](crate::validate::validate_api)
    /// never hit the fallback.
    pub fn assume_valid(api: Api) -> Self {
        let mut types = BTreeMap::new();
        for module in &api.modules {
            index_module(module, "", &mut types);
        }
        Self {
            api,
            types,
            package: None,
        }
    }

    /// Borrow the underlying [`Api`] document.
    pub fn api(&self) -> &Api {
        &self.api
    }

    /// Look up where a bare type name is declared.
    pub fn declaration(&self, name: &str) -> Option<&TypeDecl> {
        self.types.get(name)
    }

    /// Resolve a written type reference against the declarations in scope
    /// from the module at `module_path` (dot-joined, e.g. `kv.stats`).
    ///
    /// User references become their declared kind. A reference to a type
    /// declared in a *different* module is qualified with the owner's
    /// dot-joined path (`shared.Status`), so the ABI lowering emits the
    /// owner's symbol prefix; a reference to a type in the same module stays
    /// bare. Typed handles are qualified the same way.
    pub fn resolve(&self, ty: &TypeRef, module_path: &str) -> Ty {
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
            TypeRef::BorrowedStr => Ty::BorrowedStr,
            TypeRef::BorrowedBytes => Ty::BorrowedBytes,
            TypeRef::Handle => Ty::Handle,
            TypeRef::TypedHandle(name) => Ty::TypedHandle(self.qualify(name, module_path)),
            TypeRef::Named(name) => {
                let qualified = self.qualify(name, module_path);
                match self.types.get(bare(name)).map(|d| d.kind) {
                    Some(TypeKind::Enum) => Ty::Enum(qualified),
                    Some(TypeKind::RichEnum) => Ty::RichEnum(qualified),
                    Some(TypeKind::Interface) => Ty::Interface(qualified),
                    Some(TypeKind::Record) | None => Ty::Record(qualified),
                }
            }
            TypeRef::Optional(inner) => Ty::Optional(Box::new(self.resolve(inner, module_path))),
            TypeRef::List(inner) => Ty::List(Box::new(self.resolve(inner, module_path))),
            TypeRef::Map(k, v) => Ty::Map(
                Box::new(self.resolve(k, module_path)),
                Box::new(self.resolve(v, module_path)),
            ),
            TypeRef::Iterator(inner) => Ty::Iterator(Box::new(self.resolve(inner, module_path))),
        }
    }

    /// Qualify `name` with its owner's module path when the owner is not the
    /// module at `module_path`. An already-qualified name is normalized to the
    /// owner's canonical path.
    fn qualify(&self, name: &str, module_path: &str) -> String {
        let bare_name = bare(name);
        match self.types.get(bare_name) {
            Some(decl) if decl.module_path == module_path => bare_name.to_string(),
            Some(decl) => format!("{}.{bare_name}", decl.module_path),
            None => name.to_string(),
        }
    }
}

/// The final segment of a possibly dot-qualified name.
fn bare(name: &str) -> &str {
    name.rsplit('.').next().unwrap_or(name)
}

/// Recursively index every struct/enum/interface in the module tree by bare
/// name. Bare names are globally unique (validation enforces it), so the map
/// has one entry per name; the first declaration wins otherwise.
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
    use weaveffi_ir::ir::{
        EnumDef, EnumVariant, InterfaceDef, StructDef, StructField, CURRENT_SCHEMA_VERSION,
    };

    fn module(name: &str) -> Module {
        Module {
            name: name.into(),
            doc: None,
            functions: vec![],
            interfaces: vec![],
            structs: vec![],
            enums: vec![],
            callbacks: vec![],
            listeners: vec![],
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
            api.resolve(&TypeRef::Named("Status".into()), "shared"),
            Ty::Enum("Status".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Status".into()), "orders"),
            Ty::Enum("shared.Status".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Shape".into()), "orders"),
            Ty::RichEnum("shared.Shape".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Store".into()), "shared"),
            Ty::Interface("shared.inner.Store".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::Named("Store".into()), "shared.inner"),
            Ty::Interface("Store".into())
        );
        // Already-qualified spellings normalize to the owner's path.
        assert_eq!(
            api.resolve(&TypeRef::Named("shared.Point".into()), "orders"),
            Ty::Record("shared.Point".into())
        );
        assert_eq!(
            api.resolve(&TypeRef::TypedHandle("Store".into()), "orders"),
            Ty::TypedHandle("shared.inner.Store".into())
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
            api.resolve(&ty, "orders"),
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
            api.resolve(&TypeRef::Named("Elsewhere".into()), "orders"),
            Ty::Record("Elsewhere".into())
        );
        assert!(api.declaration("Elsewhere").is_none());
        assert_eq!(api.declaration("Point").unwrap().kind, TypeKind::Record);
    }
}
