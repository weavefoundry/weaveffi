//! The typed C symbol table: every identifier the ABI declares, paired with
//! the declaration that owns it.

use std::fmt;

use super::{
    contract_check_symbol, contract_symbol, FnBinding, Model, RUNTIME_SYMBOLS, VALUE_CODECS,
};
use crate::plan::RetPass;

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
/// Every `path` is a dotted declaration path: the module path, then the
/// declaration name, then a member, variant, or code name (`kv.open`,
/// `kv.Store.get`, `kv.KvError.NotFound`). `Display` renders the
/// human-readable description diagnostics quote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymbolOwner {
    /// The runtime surface every producer exports.
    Runtime,
    /// A top-level module's contract table function (`module` is its name).
    ContractTable {
        /// The top-level module's name.
        module: String,
    },
    /// The C header's checker for a top-level module's contract.
    ContractCheck {
        /// The top-level module's name.
        module: String,
    },
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
    /// A callback interface's vtable type.
    Vtable {
        /// The callback interface's path.
        path: String,
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
            Self::ContractTable { module } => {
                write!(f, "the contract table of module '{module}'")
            }
            Self::ContractCheck { module } => {
                write!(f, "the contract check of module '{module}'")
            }
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
            Self::Vtable { path } => write!(f, "the vtable of callback interface '{path}'"),
            Self::Codec { codec, of } => write!(f, "the value-buffer codec '{codec}' of {of}"),
        }
    }
}

/// Build the symbol table of `model` (see [`Model::c_symbols`]).
pub(super) fn collect(model: &Model) -> Vec<Symbol> {
    let p = model.prefix();
    let mut out: Vec<Symbol> = RUNTIME_SYMBOLS
        .iter()
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
    for m in model.roots() {
        push(
            &mut out,
            &contract_symbol(p, &m.name),
            SymbolOwner::ContractTable {
                module: m.name.clone(),
            },
        );
        push(
            &mut out,
            &contract_check_symbol(p, &m.name),
            SymbolOwner::ContractCheck {
                module: m.name.clone(),
            },
        );
    }
    let callable = |out: &mut Vec<Symbol>, f: &FnBinding, owner: &str| {
        let path = format!("{owner}.{}", f.name);
        out.push(Symbol {
            name: f.abi.symbol.clone(),
            owner: SymbolOwner::Callable { path: path.clone() },
        });
        if let Some(a) = f.async_binding() {
            out.push(Symbol {
                name: a.callback_type.clone(),
                owner: SymbolOwner::AsyncCallback { path: path.clone() },
            });
        }
        if let RetPass::Iterator(it) = &f.ret_pass {
            out.push(Symbol {
                name: it.iter_tag.clone(),
                owner: SymbolOwner::IteratorType { path: path.clone() },
            });
            out.push(Symbol {
                name: it.next.symbol.clone(),
                owner: SymbolOwner::IteratorNext { path: path.clone() },
            });
            out.push(Symbol {
                name: it.destroy_symbol.clone(),
                owner: SymbolOwner::IteratorDestroy { path },
            });
        }
    };
    let codecs = |out: &mut Vec<Symbol>, tag: &str, of: &SymbolOwner| {
        for codec in VALUE_CODECS {
            out.push(Symbol {
                name: format!("{tag}_{codec}"),
                owner: SymbolOwner::Codec {
                    codec,
                    of: Box::new(of.clone()),
                },
            });
        }
    };
    for m in &model.modules {
        let dot = &m.dot_path;
        for e in &m.errors {
            let path = format!("{dot}.{}", e.name);
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
        for e in &m.enums {
            let path = format!("{dot}.{}", e.name);
            let owner = SymbolOwner::Enum { path: path.clone() };
            push(&mut out, &e.c_tag, owner.clone());
            if e.rich {
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
        for s in &m.structs {
            let owner = SymbolOwner::Record {
                path: format!("{dot}.{}", s.name),
            };
            codecs(&mut out, &s.c_tag, &owner);
            push(&mut out, &s.c_tag, owner);
        }
        for c in &m.callback_interfaces {
            push(
                &mut out,
                &c.vtable_tag,
                SymbolOwner::Vtable {
                    path: format!("{dot}.{}", c.name),
                },
            );
        }
        for i in &m.interfaces {
            let path = format!("{dot}.{}", i.name);
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
                SymbolOwner::InterfaceDestroy { path: path.clone() },
            );
            for f in i.members() {
                callable(&mut out, f, &path);
            }
        }
        for f in &m.functions {
            callable(&mut out, f, dot);
        }
    }
    out
}
