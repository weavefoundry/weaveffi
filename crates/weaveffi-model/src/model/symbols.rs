//! The typed C symbol table: every identifier the ABI declares, paired with
//! the declaration that owns it.

use std::fmt;

use super::{FnBinding, Model, RUNTIME_SYMBOLS};

/// The codec functions the C value-buffer helper header declares for each
/// record, rich enum, and error payload struct `T`: `T_write`, `T_read`,
/// `T_decode`, and `T_free`.
pub const VALUE_CODECS: &[&str] = &["write", "read", "decode", "free"];

/// One C identifier the library's ABI declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// The full, prefixed C identifier.
    pub name: String,
    /// The declaration that owns it.
    pub owner: SymbolOwner,
}

/// The declaration that owns a C [`Symbol`].
///
/// Every `path` is a dotted declaration path: the declaration name, then a
/// member, variant, or code name (`open`, `Store.get`,
/// `KvError.NotFound`). `Display` renders the human-readable description
/// diagnostics quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolOwner {
    /// The runtime surface every producer exports.
    Runtime,
    /// A callable's entry point: the sync symbol, async launcher, or
    /// iterator launcher.
    Callable {
        /// The callable's path.
        path: String,
    },
    /// An async callable's completion-callback typedef.
    AsyncCallback {
        /// The callable's path.
        path: String,
    },
    /// The opaque iterator type an iterator-returning callable launches.
    IteratorType {
        /// The callable's path.
        path: String,
    },
    /// That iterator's `_next`.
    IteratorNext {
        /// The callable's path.
        path: String,
    },
    /// That iterator's `_destroy`.
    IteratorDestroy {
        /// The callable's path.
        path: String,
    },
    /// An interface's opaque type.
    Interface {
        /// The interface's path.
        path: String,
    },
    /// An interface's `_clone`.
    InterfaceClone {
        /// The interface's path.
        path: String,
    },
    /// An interface's `_destroy`.
    InterfaceDestroy {
        /// The interface's path.
        path: String,
    },
    /// An enum's type (a C-style enum's `int32_t` type, or a rich enum's
    /// buffer-header struct).
    Enum {
        /// The enum's path.
        path: String,
    },
    /// A rich enum's `_Tag` type.
    EnumTag {
        /// The enum's path.
        path: String,
    },
    /// An enum variant's constant.
    EnumVariant {
        /// The variant's path.
        path: String,
    },
    /// An error domain's code type.
    ErrorDomain {
        /// The domain's path.
        path: String,
    },
    /// An error code's constant.
    ErrorCode {
        /// The code's path.
        path: String,
    },
    /// The buffer-header struct of an error code's payload.
    ErrorPayload {
        /// The code's path.
        path: String,
    },
    /// A record's buffer-header struct.
    Record {
        /// The record's path.
        path: String,
    },
    /// A callback interface's argument struct.
    Callback {
        /// The callback interface's path.
        path: String,
    },
    /// A callback interface's vtable type.
    Vtable {
        /// The callback interface's path.
        path: String,
    },
    /// The C value-buffer header's struct of a composite (an optional,
    /// list, or map shape), and its codecs.
    Composite {
        /// The composite's stem (`list_i32`).
        stem: String,
    },
    /// One of the [`VALUE_CODECS`] the buffer header declares for a struct.
    Codec {
        /// The codec (`write`, `read`, `decode`, or `free`).
        codec: &'static str,
        /// The struct's owner: a [`Record`](Self::Record),
        /// [`Enum`](Self::Enum), or [`ErrorPayload`](Self::ErrorPayload).
        of: Box<SymbolOwner>,
    },
}

impl fmt::Display for SymbolOwner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Runtime => f.write_str("the runtime surface"),
            Self::Callable { path } => write!(f, "function '{path}'"),
            Self::AsyncCallback { path } => write!(f, "the completion type of '{path}'"),
            Self::IteratorType { path } => write!(f, "the iterator type of '{path}'"),
            Self::IteratorNext { path } => write!(f, "the iterator step of '{path}'"),
            Self::IteratorDestroy { path } => write!(f, "the iterator destructor of '{path}'"),
            Self::Interface { path } => write!(f, "interface '{path}'"),
            Self::InterfaceClone { path } => write!(f, "the clone of '{path}'"),
            Self::InterfaceDestroy { path } => write!(f, "the destructor of '{path}'"),
            Self::Enum { path } => write!(f, "enum '{path}'"),
            Self::EnumTag { path } => write!(f, "the tag type of enum '{path}'"),
            Self::EnumVariant { path } => write!(f, "enum variant '{path}'"),
            Self::ErrorDomain { path } => write!(f, "error domain '{path}'"),
            Self::ErrorCode { path } => write!(f, "error code '{path}'"),
            Self::ErrorPayload { path } => {
                write!(f, "the payload struct of error code '{path}'")
            }
            Self::Record { path } => write!(f, "record '{path}'"),
            Self::Composite { stem } => write!(f, "the value-buffer codec of '{stem}'"),
            Self::Callback { path } => write!(f, "callback interface '{path}'"),
            Self::Vtable { path } => write!(f, "the vtable of callback interface '{path}'"),
            Self::Codec { codec, of } => write!(f, "the value-buffer codec '{codec}' of {of}"),
        }
    }
}

/// Build the symbol table of `model` (see [`Model::c_symbols`]). The
/// per-declaration contract functions are left out: their names fall in a
/// family no declaration may use.
pub(super) fn collect(model: &Model) -> Vec<Symbol> {
    let p = model.prefix();
    let mut out: Vec<Symbol> = RUNTIME_SYMBOLS
        .iter()
        .map(|s| s.to_string())
        .chain(
            crate::ty::Prim::ALL
                .into_iter()
                .filter(|p| p.is_scalar())
                .map(|prim| format!("opt_{}", prim.snake())),
        )
        .chain(std::iter::once("contract_check".to_string()))
        .map(|s| Symbol {
            name: format!("{p}_{s}"),
            owner: SymbolOwner::Runtime,
        })
        .collect();
    let push = |out: &mut Vec<Symbol>, name: &str, owner: SymbolOwner| {
        out.push(Symbol {
            name: name.to_string(),
            owner,
        });
    };
    let callable = |out: &mut Vec<Symbol>, f: &FnBinding| {
        let path = f.path.clone();
        push(
            out,
            &f.abi.symbol,
            SymbolOwner::Callable { path: path.clone() },
        );
        if let Some(a) = f.async_binding() {
            push(
                out,
                &a.callback_type,
                SymbolOwner::AsyncCallback { path: path.clone() },
            );
        }
        if let Some(it) = f.iterator() {
            push(
                out,
                &it.iter_tag,
                SymbolOwner::IteratorType { path: path.clone() },
            );
            push(
                out,
                &it.next.symbol,
                SymbolOwner::IteratorNext { path: path.clone() },
            );
            push(
                out,
                &it.destroy_symbol,
                SymbolOwner::IteratorDestroy { path },
            );
        }
    };
    let codecs = |out: &mut Vec<Symbol>, tag: &str, of: &SymbolOwner| {
        for codec in VALUE_CODECS {
            push(
                out,
                &format!("{tag}_{codec}"),
                SymbolOwner::Codec {
                    codec,
                    of: Box::new(of.clone()),
                },
            );
        }
    };
    for e in &model.errors {
        let path = e.name.clone();
        push(
            &mut out,
            &e.c_tag,
            SymbolOwner::ErrorDomain { path: path.clone() },
        );
        for c in &e.codes {
            let code = format!("{path}.{}", c.name);
            push(
                &mut out,
                &c.c_const,
                SymbolOwner::ErrorCode { path: code.clone() },
            );
            if !c.fields.is_empty() {
                let owner = SymbolOwner::ErrorPayload { path: code };
                let tag = c.payload_tag();
                codecs(&mut out, &tag, &owner);
                push(&mut out, &tag, owner);
            }
        }
    }
    for e in &model.enums {
        let path = e.name.clone();
        let owner = SymbolOwner::Enum { path: path.clone() };
        push(&mut out, &e.c_tag, owner.clone());
        if e.is_rich() {
            push(
                &mut out,
                &format!("{}_Tag", e.c_tag),
                SymbolOwner::EnumTag { path: path.clone() },
            );
            codecs(&mut out, &e.c_tag, &owner);
        }
        for v in &e.variants {
            push(
                &mut out,
                &v.c_const,
                SymbolOwner::EnumVariant {
                    path: format!("{path}.{}", v.name),
                },
            );
        }
    }
    for r in &model.records {
        let owner = SymbolOwner::Record {
            path: r.name.clone(),
        };
        codecs(&mut out, &r.c_tag, &owner);
        push(&mut out, &r.c_tag, owner);
    }
    for c in &model.callbacks {
        let path = c.name.clone();
        push(
            &mut out,
            &c.c_tag,
            SymbolOwner::Callback { path: path.clone() },
        );
        push(&mut out, &c.vtable_tag, SymbolOwner::Vtable { path });
    }
    for i in &model.interfaces {
        let path = i.name.clone();
        push(
            &mut out,
            &i.c_tag,
            SymbolOwner::Interface { path: path.clone() },
        );
        push(
            &mut out,
            &i.clone_symbol,
            SymbolOwner::InterfaceClone { path: path.clone() },
        );
        push(
            &mut out,
            &i.destroy_symbol,
            SymbolOwner::InterfaceDestroy { path },
        );
        for f in i.members() {
            callable(&mut out, f);
        }
    }
    for f in &model.functions {
        callable(&mut out, f);
    }
    if model.has_buffers() {
        for ty in model.composites() {
            let stem = ty.stem();
            let owner = SymbolOwner::Composite { stem: stem.clone() };
            let tag = format!("{p}_buf_{stem}");
            codecs(&mut out, &tag, &owner);
            push(&mut out, &tag, owner);
        }
    }
    out
}
