//! The structural lowering: how each resolved type maps onto C ABI slots in
//! every position. This is the single source of truth the model stores on
//! every binding, so no generator re-derives it.
//!
//! The lowering classifies a value [`Ty`] into its
//! [`Family`](crate::ty::Family) and, per
//! position, produces the passing contract from [`crate::plan`] that names
//! its slots:
//!
//! | Family | Parameter | Return | Async result | Iterator item | Callback return |
//! |---|---|---|---|---|---|
//! | Direct | `T n` | C return `T` | `T result` | `T* out_item` | C return `T` |
//! | OptDirect | `bool has_n, T n` | C return `bool`, `T* out_value` | `bool has_result, T result` | `bool* out_has_item, T* out_item` | C return `bool`, `T* out_value` |
//! | Slice | `const T* n_ptr, size_t n_len` | C return `T*`, `size_t* out_len` | `const T* result_ptr, size_t result_len` | `T** out_item, size_t* out_len` | `T** out_ptr, size_t* out_len` |
//! | String, Bytes, Buffer | `const uint8_t* n_ptr, size_t n_len` | C return `const uint8_t*`, `size_t* out_len` | `const uint8_t* result_ptr, size_t result_len` | `const uint8_t** out_item, size_t* out_len` | `uint8_t** out_ptr, size_t* out_len` |
//! | Object | `const {tag}* n` | C return `{tag}*` | `{tag}* result` | `{tag}** out_item` | C return `{tag}*` |
//!
//! A callback method's parameter is lowered like a callable's, except that
//! an object parameter is a mutable `{tag}* n` (the consumer adopts one
//! strong reference). A callback interface parameter is
//! `void* n_ctx, const {vtable}* n_vtable`.

use crate::model::{c_tag, member_symbol};
use crate::plan::{ArgPass, CallbackRetPass, ItemPass, ResultPass, RetPass};
use crate::ty::{ParamTy, Prim, Ty, TypeIndex};

use super::ctype::{CType, ConstPos};

/// A named C parameter slot.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AbiParam {
    /// The C parameter name (for example `out_err`, `data_ptr`, `has_limit`).
    pub name: String,
    /// The C type of the slot.
    pub ty: CType,
}

impl AbiParam {
    /// Build a parameter slot from a name and its C type.
    pub fn new(name: impl Into<String>, ty: CType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

/// A user type name the lowering needed a declaration for (an enum,
/// interface, or callback interface owner) but the index doesn't have.
///
/// Validation rejects every such name before the model is built, so this is
/// only reachable when a rule is missing; the validator reports it as an
/// unknown type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Undeclared(pub(crate) String);

/// Where a parameter appears: a callable's input, or a callback method's
/// input (the consumer's trampoline receives it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamSite {
    /// A function, interface member, or launcher parameter.
    Call,
    /// A callback-interface method parameter.
    CallbackMethod,
}

/// A value type's lowering class: its family plus what the slots need.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Class<'a> {
    Direct(CType),
    OptDirect(CType),
    Slice(Prim, CType),
    String,
    Bytes,
    Buffer,
    Object { interface: &'a str, nullable: bool },
}

/// The lowering context: the type index (for C type owners) and the symbol
/// prefix (for the release symbols the contracts carry).
pub(crate) struct Lower<'a> {
    pub(crate) types: &'a TypeIndex,
    pub(crate) prefix: &'a str,
}

impl<'a> Lower<'a> {
    /// The underscore-joined C path of the module declaring `name`.
    fn owner(&self, name: &str) -> Result<String, Undeclared> {
        self.types
            .module_path(name)
            .map(str::to_string)
            .ok_or_else(|| Undeclared(name.to_string()))
    }

    fn enum_ctype(&self, name: &str) -> Result<CType, Undeclared> {
        Ok(CType::Enum {
            module: self.owner(name)?,
            name: name.to_string(),
        })
    }

    /// The opaque C tag type of the interface `name`.
    pub(crate) fn struct_tag(&self, name: &str) -> Result<CType, Undeclared> {
        Ok(CType::StructTag {
            module: self.owner(name)?,
            name: name.to_string(),
        })
    }

    /// The `{prefix}_{path}_{name}` C tag of the interface `name`.
    fn tag_of(&self, name: &str) -> Result<String, Undeclared> {
        Ok(c_tag(self.prefix, &self.owner(name)?, name))
    }

    fn destroy_symbol(&self, interface: &str) -> Result<String, Undeclared> {
        Ok(member_symbol(&self.tag_of(interface)?, "destroy"))
    }

    fn clone_symbol(&self, interface: &str) -> Result<String, Undeclared> {
        Ok(member_symbol(&self.tag_of(interface)?, "clone"))
    }

    /// Classify a value type. Agrees with [`Ty::family`] by construction
    /// (a unit test pins the agreement).
    fn class<'t>(&self, ty: &'t Ty) -> Result<Class<'t>, Undeclared> {
        Ok(match ty {
            Ty::Prim(Prim::String) => Class::String,
            Ty::Prim(Prim::Bytes) => Class::Bytes,
            Ty::Prim(p) => Class::Direct(CType::of_prim(*p)),
            Ty::Enum(e) => Class::Direct(self.enum_ctype(e)?),
            Ty::Interface(i) => Class::Object {
                interface: i,
                nullable: false,
            },
            Ty::Optional(inner) => match inner.as_ref() {
                Ty::Prim(p) if p.is_scalar() => Class::OptDirect(CType::of_prim(*p)),
                Ty::Enum(e) => Class::OptDirect(self.enum_ctype(e)?),
                Ty::Interface(i) => Class::Object {
                    interface: i,
                    nullable: true,
                },
                _ => Class::Buffer,
            },
            Ty::List(inner) => match inner.as_ref() {
                Ty::Prim(p) if p.is_slice_elem() => Class::Slice(*p, CType::of_prim(*p)),
                _ => Class::Buffer,
            },
            Ty::Record(_) | Ty::RichEnum(_) | Ty::Map(_, _) => Class::Buffer,
        })
    }

    /// Lower one parameter at `site`.
    pub(crate) fn param(
        &self,
        name: &str,
        ty: &ParamTy,
        site: ParamSite,
    ) -> Result<ArgPass, Undeclared> {
        match ty {
            ParamTy::Value(ty) => self.value_param(name, ty, site),
            // A callback interface is an opaque consumer context plus the
            // consumer's static vtable for the interface. A nullable one
            // passes a null vtable for none.
            ParamTy::Callback { name: cb, nullable } => Ok(ArgPass::Callback {
                ctx: AbiParam::new(format!("{name}_ctx"), CType::ptr(CType::Void)),
                vtable: AbiParam::new(
                    format!("{name}_vtable"),
                    CType::const_ptr(CType::VtableTag {
                        module: self.owner(cb)?,
                        name: cb.clone(),
                    }),
                ),
                nullable: *nullable,
                interface: cb.clone(),
            }),
        }
    }

    /// Lower one value parameter at `site`.
    pub(crate) fn value_param(
        &self,
        name: &str,
        ty: &Ty,
        site: ParamSite,
    ) -> Result<ArgPass, Undeclared> {
        let ptr = |ty| AbiParam::new(format!("{name}_ptr"), ty);
        let len = || AbiParam::new(format!("{name}_len"), CType::Size);
        let bytes = || ptr(CType::const_ptr(CType::Uint8));
        Ok(match self.class(ty)? {
            Class::Direct(c) => ArgPass::Direct {
                slot: AbiParam::new(name, c),
            },
            Class::OptDirect(c) => ArgPass::OptDirect {
                has: AbiParam::new(format!("has_{name}"), CType::Bool),
                value: AbiParam::new(name, c),
                inner: match ty {
                    Ty::Optional(inner) => (**inner).clone(),
                    other => other.clone(),
                },
            },
            Class::Slice(elem, c) => ArgPass::Slice {
                ptr: ptr(CType::const_ptr(c)),
                len: len(),
                elem,
            },
            Class::String => ArgPass::String {
                ptr: bytes(),
                len: len(),
            },
            Class::Bytes => ArgPass::Bytes {
                ptr: bytes(),
                len: len(),
            },
            Class::Buffer => ArgPass::Buffer {
                ptr: bytes(),
                len: len(),
            },
            // A callable borrows the object for the call; a callback method
            // receives one strong reference the consumer adopts, so its
            // slot is mutable.
            Class::Object {
                interface,
                nullable,
            } => {
                let konst = match site {
                    ParamSite::Call => ConstPos::West,
                    ParamSite::CallbackMethod => ConstPos::None,
                };
                ArgPass::Object {
                    slot: AbiParam::new(
                        name,
                        CType::Ptr {
                            konst,
                            pointee: Box::new(self.struct_tag(interface)?),
                        },
                    ),
                    nullable,
                    interface: interface.to_string(),
                }
            }
        })
    }

    /// Lower a synchronous value return (or `void`) to its C return type and
    /// passing contract.
    pub(crate) fn ret(&self, ty: Option<&Ty>) -> Result<(CType, RetPass), Undeclared> {
        let Some(ty) = ty else {
            return Ok((CType::Void, RetPass::Void));
        };
        let out_len = || AbiParam::new("out_len", CType::ptr(CType::Size));
        let run = || CType::const_ptr(CType::Uint8);
        Ok(match self.class(ty)? {
            Class::Direct(c) => (c, RetPass::Direct),
            Class::OptDirect(c) => (
                CType::Bool,
                RetPass::OptDirect {
                    out_value: AbiParam::new("out_value", CType::ptr(c)),
                },
            ),
            Class::Slice(elem, c) => (
                CType::ptr(c),
                RetPass::Slice {
                    out_len: out_len(),
                    elem,
                },
            ),
            Class::String => (run(), RetPass::String { out_len: out_len() }),
            Class::Bytes => (run(), RetPass::Bytes { out_len: out_len() }),
            Class::Buffer => (run(), RetPass::Buffer { out_len: out_len() }),
            Class::Object {
                interface,
                nullable,
            } => (
                CType::ptr(self.struct_tag(interface)?),
                RetPass::Object {
                    nullable,
                    interface: interface.to_string(),
                    destroy_symbol: self.destroy_symbol(interface)?,
                },
            ),
        })
    }

    /// Lower an async result (or `void`) to its completion-callback slots.
    pub(crate) fn result(&self, ty: Option<&Ty>) -> Result<ResultPass, Undeclared> {
        let Some(ty) = ty else {
            return Ok(ResultPass::Void);
        };
        let ptr = |c| AbiParam::new("result_ptr", c);
        let len = || AbiParam::new("result_len", CType::Size);
        let bytes = || ptr(CType::const_ptr(CType::Uint8));
        Ok(match self.class(ty)? {
            Class::Direct(c) => ResultPass::Direct {
                result: AbiParam::new("result", c),
            },
            Class::OptDirect(c) => ResultPass::OptDirect {
                has: AbiParam::new("has_result", CType::Bool),
                value: AbiParam::new("result", c),
            },
            Class::Slice(elem, c) => ResultPass::Slice {
                ptr: ptr(CType::const_ptr(c)),
                len: len(),
                elem,
            },
            Class::String => ResultPass::String {
                ptr: bytes(),
                len: len(),
            },
            Class::Bytes => ResultPass::Bytes {
                ptr: bytes(),
                len: len(),
            },
            Class::Buffer => ResultPass::Buffer {
                ptr: bytes(),
                len: len(),
            },
            Class::Object {
                interface,
                nullable,
            } => ResultPass::Object {
                result: AbiParam::new("result", CType::ptr(self.struct_tag(interface)?)),
                nullable,
                interface: interface.to_string(),
                destroy_symbol: self.destroy_symbol(interface)?,
            },
        })
    }

    /// Lower an iterator element to `next`'s out slots.
    pub(crate) fn item(&self, ty: &Ty) -> Result<ItemPass, Undeclared> {
        let out_item = |c| AbiParam::new("out_item", CType::ptr(c));
        let out_len = || AbiParam::new("out_len", CType::ptr(CType::Size));
        let run = || out_item(CType::const_ptr(CType::Uint8));
        Ok(match self.class(ty)? {
            Class::Direct(c) => ItemPass::Direct {
                out_item: out_item(c),
            },
            Class::OptDirect(c) => ItemPass::OptDirect {
                out_has: AbiParam::new("out_has_item", CType::ptr(CType::Bool)),
                out_item: out_item(c),
            },
            Class::Slice(elem, c) => ItemPass::Slice {
                out_item: out_item(CType::ptr(c)),
                out_len: out_len(),
                elem,
            },
            Class::String => ItemPass::String {
                out_item: run(),
                out_len: out_len(),
            },
            Class::Bytes => ItemPass::Bytes {
                out_item: run(),
                out_len: out_len(),
            },
            Class::Buffer => ItemPass::Buffer {
                out_item: run(),
                out_len: out_len(),
            },
            Class::Object {
                interface,
                nullable,
            } => ItemPass::Object {
                out_item: out_item(CType::ptr(self.struct_tag(interface)?)),
                nullable,
                interface: interface.to_string(),
                destroy_symbol: self.destroy_symbol(interface)?,
            },
        })
    }

    /// Lower a callback method's return (or `void`) to its C return type and
    /// passing contract. The consumer produces the value, so a run is one
    /// it allocates with `{prefix}_alloc` and hands back through out slots.
    pub(crate) fn callback_ret(
        &self,
        ty: Option<&Ty>,
    ) -> Result<(CType, CallbackRetPass), Undeclared> {
        let Some(ty) = ty else {
            return Ok((CType::Void, CallbackRetPass::Void));
        };
        let out_ptr = |c| AbiParam::new("out_ptr", CType::ptr(CType::ptr(c)));
        let out_len = || AbiParam::new("out_len", CType::ptr(CType::Size));
        let run = || out_ptr(CType::Uint8);
        Ok(match self.class(ty)? {
            Class::Direct(c) => (c, CallbackRetPass::Direct),
            Class::OptDirect(c) => (
                CType::Bool,
                CallbackRetPass::OptDirect {
                    out_value: AbiParam::new("out_value", CType::ptr(c)),
                },
            ),
            Class::Slice(elem, c) => (
                CType::Void,
                CallbackRetPass::Slice {
                    out_ptr: out_ptr(c),
                    out_len: out_len(),
                    elem,
                },
            ),
            Class::String => (
                CType::Void,
                CallbackRetPass::String {
                    out_ptr: run(),
                    out_len: out_len(),
                },
            ),
            Class::Bytes => (
                CType::Void,
                CallbackRetPass::Bytes {
                    out_ptr: run(),
                    out_len: out_len(),
                },
            ),
            Class::Buffer => (
                CType::Void,
                CallbackRetPass::Buffer {
                    out_ptr: run(),
                    out_len: out_len(),
                },
            ),
            Class::Object {
                interface,
                nullable,
            } => (
                CType::ptr(self.struct_tag(interface)?),
                CallbackRetPass::Object {
                    nullable,
                    interface: interface.to_string(),
                    clone_symbol: self.clone_symbol(interface)?,
                },
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::{Family, TypeDecl, TypeKind};

    fn render(params: &[&AbiParam]) -> Vec<String> {
        params
            .iter()
            .map(|p| format!("{} {}", p.ty.render_c("weaveffi"), p.name))
            .collect()
    }

    /// `Store` in `kv`, `Status` in `shared`, `Listener` in `events`.
    fn types() -> TypeIndex {
        let mut types = TypeIndex::default();
        for (module, name, kind) in [
            ("kv", "Store", TypeKind::Interface),
            ("shared", "Status", TypeKind::Enum),
            ("events", "Listener", TypeKind::CallbackInterface),
        ] {
            let module = types.push_module(module.into());
            types.declare(
                name,
                TypeDecl {
                    kind,
                    module,
                    index: 0,
                },
            );
        }
        types
    }

    fn with<T>(f: impl FnOnce(&Lower<'_>) -> T) -> T {
        let types = types();
        f(&Lower {
            types: &types,
            prefix: "weaveffi",
        })
    }

    fn ty(s: &str) -> Ty {
        fn resolve(t: &crate::ir::TypeRef) -> Ty {
            use crate::ir::TypeRef;
            match t {
                TypeRef::Prim(p) => Ty::Prim(*p),
                TypeRef::Named(n) if n == "Store" => Ty::Interface(n.clone()),
                TypeRef::Named(n) if n == "Status" => Ty::Enum(n.clone()),
                TypeRef::Named(n) => Ty::Record(n.clone()),
                TypeRef::Optional(i) => Ty::Optional(Box::new(resolve(i))),
                TypeRef::List(i) => Ty::List(Box::new(resolve(i))),
                TypeRef::Map(k, v) => Ty::Map(Box::new(resolve(k)), Box::new(resolve(v))),
                TypeRef::Iterator(_) => unreachable!("not a value"),
            }
        }
        resolve(&crate::ir::parse_type_ref(s).unwrap())
    }

    fn param(name: &str, t: &str, site: ParamSite) -> Vec<String> {
        with(|l| render(&l.value_param(name, &ty(t), site).unwrap().slots()))
    }

    fn ret(t: &str) -> (String, Vec<String>) {
        with(|l| {
            let (c, pass) = l.ret(Some(&ty(t))).unwrap();
            (c.render_c("weaveffi"), render(&pass.out_slots()))
        })
    }

    fn result(t: &str) -> Vec<String> {
        with(|l| render(&l.result(Some(&ty(t))).unwrap().slots()))
    }

    fn item(t: &str) -> Vec<String> {
        with(|l| render(&l.item(&ty(t)).unwrap().slots()))
    }

    fn cb_ret(t: &str) -> (String, Vec<String>) {
        with(|l| {
            let (c, pass) = l.callback_ret(Some(&ty(t))).unwrap();
            (c.render_c("weaveffi"), render(&pass.out_slots()))
        })
    }

    #[test]
    fn classes_agree_with_families() {
        let cases = [
            "i32",
            "bool",
            "Status",
            "string",
            "bytes",
            "Contact",
            "Store",
            "Store?",
            "i32?",
            "bool?",
            "Status?",
            "u8?",
            "[i32]",
            "[f64]",
            "[u64]",
            "[i8]",
            "[bool]",
            "[string]",
            "[Status]",
            "[[i32]]",
            "i32??",
            "string?",
            "[i32]?",
            "{string:i32}",
            "[Store]",
        ];
        with(|l| {
            for s in cases {
                let t = ty(s);
                let class = l.class(&t).unwrap();
                let agrees = matches!(
                    (t.family(), &class),
                    (Family::Direct, Class::Direct(_))
                        | (Family::OptDirect, Class::OptDirect(_))
                        | (Family::String, Class::String)
                        | (Family::Bytes, Class::Bytes)
                        | (Family::Buffer, Class::Buffer)
                ) || matches!(
                    (t.family(), &class),
                    (Family::Slice(a), Class::Slice(b, _)) if a == *b
                ) || matches!(
                    (t.family(), &class),
                    (Family::Object { nullable: a }, Class::Object { nullable: b, .. }) if a == *b
                );
                assert!(agrees, "{s}: {:?} vs {class:?}", t.family());
            }
        });
    }

    #[test]
    fn params_lower_by_family() {
        let call = ParamSite::Call;
        assert_eq!(param("x", "i32", call), ["int32_t x"]);
        assert_eq!(
            param("s", "string", call),
            ["const uint8_t* s_ptr", "size_t s_len"]
        );
        assert_eq!(
            param("data", "bytes", call),
            ["const uint8_t* data_ptr", "size_t data_len"]
        );
        assert_eq!(
            param("xs", "[string]", call),
            ["const uint8_t* xs_ptr", "size_t xs_len"]
        );
        assert_eq!(
            param("c", "Contact", call),
            ["const uint8_t* c_ptr", "size_t c_len"]
        );
        assert_eq!(param("s", "Store?", call), ["const weaveffi_kv_Store* s"]);
        assert_eq!(param("s", "Status", call), ["weaveffi_shared_Status s"]);
        // An optional that isn't OptDirect stays a buffer.
        assert_eq!(
            param("o", "string?", call),
            ["const uint8_t* o_ptr", "size_t o_len"]
        );
        assert_eq!(
            param("o", "[bool]", call),
            ["const uint8_t* o_ptr", "size_t o_len"]
        );
        with(|l| {
            let cb = ParamTy::Callback {
                name: "Listener".into(),
                nullable: true,
            };
            let pass = l.param("l", &cb, call).unwrap();
            assert_eq!(
                render(&pass.slots()),
                [
                    "void* l_ctx",
                    "const weaveffi_events_Listener_vtable* l_vtable"
                ]
            );
            assert!(
                matches!(pass, ArgPass::Callback { nullable: true, ref interface, .. } if interface == "Listener")
            );
        });
    }

    #[test]
    fn opt_direct_params() {
        for site in [ParamSite::Call, ParamSite::CallbackMethod] {
            assert_eq!(
                param("limit", "i32?", site),
                ["bool has_limit", "int32_t limit"]
            );
            assert_eq!(param("flag", "bool?", site), ["bool has_flag", "bool flag"]);
            assert_eq!(param("r", "f64?", site), ["bool has_r", "double r"]);
            assert_eq!(
                param("s", "Status?", site),
                ["bool has_s", "weaveffi_shared_Status s"]
            );
        }
        with(|l| {
            let pass = l.value_param("s", &ty("Status?"), ParamSite::Call).unwrap();
            let ArgPass::OptDirect { has, value, inner } = pass else {
                panic!("expected OptDirect, got {pass:?}");
            };
            assert_eq!(has.name, "has_s");
            assert_eq!(value.name, "s");
            assert_eq!(inner, Ty::Enum("Status".into()));
        });
    }

    #[test]
    fn slice_params() {
        for site in [ParamSite::Call, ParamSite::CallbackMethod] {
            assert_eq!(
                param("xs", "[f64]", site),
                ["const double* xs_ptr", "size_t xs_len"]
            );
            assert_eq!(
                param("xs", "[i32]", site),
                ["const int32_t* xs_ptr", "size_t xs_len"]
            );
            assert_eq!(
                param("xs", "[u64]", site),
                ["const uint64_t* xs_ptr", "size_t xs_len"]
            );
            assert_eq!(
                param("xs", "[i8]", site),
                ["const int8_t* xs_ptr", "size_t xs_len"]
            );
            assert_eq!(
                param("xs", "[f32]", site),
                ["const float* xs_ptr", "size_t xs_len"]
            );
        }
        with(|l| {
            let pass = l.value_param("xs", &ty("[u16]"), ParamSite::Call).unwrap();
            assert!(matches!(
                pass,
                ArgPass::Slice {
                    elem: Prim::U16,
                    ..
                }
            ));
        });
    }

    #[test]
    fn callback_method_object_params_transfer_a_reference() {
        assert_eq!(
            param("s", "Store", ParamSite::Call),
            ["const weaveffi_kv_Store* s"]
        );
        assert_eq!(
            param("s", "Store", ParamSite::CallbackMethod),
            ["weaveffi_kv_Store* s"]
        );
    }

    #[test]
    fn returns_lower_by_family() {
        assert_eq!(ret("i32"), ("int32_t".into(), vec![]));
        assert_eq!(ret("Status"), ("weaveffi_shared_Status".into(), vec![]));
        for t in [
            "string",
            "bytes",
            "Contact",
            "[string]",
            "[Store]",
            "{string:i32}",
            "string?",
        ] {
            assert_eq!(
                ret(t),
                ("const uint8_t*".into(), vec!["size_t* out_len".into()]),
                "{t}"
            );
        }
        assert_eq!(ret("Store?"), ("weaveffi_kv_Store*".into(), vec![]));
        with(|l| {
            let (_, pass) = l.ret(Some(&ty("Store"))).unwrap();
            assert_eq!(
                pass,
                RetPass::Object {
                    nullable: false,
                    interface: "Store".into(),
                    destroy_symbol: "weaveffi_kv_Store_destroy".into(),
                }
            );
            assert_eq!(l.ret(None).unwrap(), (CType::Void, RetPass::Void));
        });
    }

    #[test]
    fn opt_direct_and_slice_returns() {
        assert_eq!(
            ret("i64?"),
            ("bool".into(), vec!["int64_t* out_value".into()])
        );
        assert_eq!(
            ret("bool?"),
            ("bool".into(), vec!["bool* out_value".into()])
        );
        assert_eq!(
            ret("Status?"),
            (
                "bool".into(),
                vec!["weaveffi_shared_Status* out_value".into()]
            )
        );
        assert_eq!(
            ret("[f64]"),
            ("double*".into(), vec!["size_t* out_len".into()])
        );
        assert_eq!(
            ret("[u64]"),
            ("uint64_t*".into(), vec!["size_t* out_len".into()])
        );
        with(|l| {
            let (_, pass) = l.ret(Some(&ty("[i32]"))).unwrap();
            assert!(
                matches!(pass, RetPass::Slice { elem: Prim::I32, ref out_len } if out_len.name == "out_len")
            );
        });
    }

    #[test]
    fn async_results() {
        assert_eq!(result("i32"), ["int32_t result"]);
        assert_eq!(result("i64?"), ["bool has_result", "int64_t result"]);
        assert_eq!(
            result("Status?"),
            ["bool has_result", "weaveffi_shared_Status result"]
        );
        assert_eq!(
            result("[f32]"),
            ["const float* result_ptr", "size_t result_len"]
        );
        assert_eq!(
            result("string"),
            ["const uint8_t* result_ptr", "size_t result_len"]
        );
        assert_eq!(
            result("[string]"),
            ["const uint8_t* result_ptr", "size_t result_len"]
        );
        assert_eq!(result("Store?"), ["weaveffi_kv_Store* result"]);
        with(|l| {
            assert_eq!(l.result(None).unwrap(), ResultPass::Void);
            let pass = l.result(Some(&ty("[f32]"))).unwrap();
            assert!(matches!(
                pass,
                ResultPass::Slice {
                    elem: Prim::F32,
                    ..
                }
            ));
            let pass = l.result(Some(&ty("Store"))).unwrap();
            assert!(
                matches!(pass, ResultPass::Object { ref destroy_symbol, .. } if destroy_symbol == "weaveffi_kv_Store_destroy")
            );
        });
    }

    #[test]
    fn iterator_items() {
        assert_eq!(item("i32"), ["int32_t* out_item"]);
        assert_eq!(item("i32?"), ["bool* out_has_item", "int32_t* out_item"]);
        assert_eq!(item("[i32]"), ["int32_t** out_item", "size_t* out_len"]);
        assert_eq!(
            item("string"),
            ["const uint8_t** out_item", "size_t* out_len"]
        );
        assert_eq!(
            item("Contact"),
            ["const uint8_t** out_item", "size_t* out_len"]
        );
        assert_eq!(item("Store"), ["weaveffi_kv_Store** out_item"]);
        with(|l| {
            assert!(matches!(
                l.item(&ty("[u32]")).unwrap(),
                ItemPass::Slice {
                    elem: Prim::U32,
                    ..
                }
            ));
        });
    }

    #[test]
    fn callback_method_returns() {
        assert_eq!(cb_ret("i32"), ("int32_t".into(), vec![]));
        assert_eq!(
            cb_ret("f64?"),
            ("bool".into(), vec!["double* out_value".into()])
        );
        assert_eq!(
            cb_ret("[i16]"),
            (
                "void".into(),
                vec!["int16_t** out_ptr".into(), "size_t* out_len".into()]
            )
        );
        for t in ["string", "bytes", "Contact?", "[string]"] {
            assert_eq!(
                cb_ret(t),
                (
                    "void".into(),
                    vec!["uint8_t** out_ptr".into(), "size_t* out_len".into()]
                ),
                "{t}"
            );
        }
        assert_eq!(cb_ret("Store"), ("weaveffi_kv_Store*".into(), vec![]));
        with(|l| {
            let (_, pass) = l.callback_ret(Some(&ty("Store?"))).unwrap();
            assert!(
                matches!(pass, CallbackRetPass::Object { nullable: true, ref clone_symbol, .. } if clone_symbol == "weaveffi_kv_Store_clone")
            );
            assert_eq!(
                l.callback_ret(None).unwrap(),
                (CType::Void, CallbackRetPass::Void)
            );
        });
    }

    #[test]
    fn undeclared_owners_are_errors_not_panics() {
        with(|l| {
            assert_eq!(
                l.value_param("x", &Ty::Enum("Nope".into()), ParamSite::Call),
                Err(Undeclared("Nope".into()))
            );
            assert_eq!(
                l.ret(Some(&Ty::Interface("Nope".into()))),
                Err(Undeclared("Nope".into()))
            );
            // Records never name a C type, so a foreign one lowers fine.
            assert!(l
                .value_param("c", &Ty::Record("Elsewhere".into()), ParamSite::Call)
                .is_ok());
        });
    }
}
