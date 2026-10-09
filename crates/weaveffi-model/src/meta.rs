//! Library metadata: the API a Rust producer embeds in its compiled library.
//!
//! The `#[weaveffi::module]` macro describes every declaration it exports in
//! a [`Frame`] and embeds the frame's [encoding](Frame::encode) in the
//! producer's library as an exported static named [`Frame::symbol`] (on
//! `wasm32`, in the [`SECTION`] custom section instead). The CLI reads the
//! frames back out of the built library, [decodes](decode) them, keeps those
//! with the crate's own prefix, and [assembles](assemble) the [`Api`] it
//! generates from. Because the frames are compiled with the code they
//! describe, a declaration removed by `#[cfg]` is absent from both, and the
//! generated bindings describe exactly what the library exports.
//!
//! # Encoding
//!
//! A frame is a little-endian `u32` byte length followed by that many bytes
//! of compact JSON (the [`Frame`] serialized with serde). Frames are
//! self-delimiting, so a section holding several frames back to back
//! [decodes](decode) into all of them.
//!
//! One frame describes one declaration: a module (its name and doc), one of
//! its error domains, a free function, an interface (without its members), one
//! interface member, a record, an enum, or a callback interface. Each frame
//! records the path of the module it belongs to and its position among its
//! siblings of the same kind, so [`assemble`] restores declaration order.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::fnv1a64;
use crate::ir::{
    Api, CallbackInterfaceDef, EnumDef, ErrorDomain, Function, InterfaceDef, Module, StructDef,
    CURRENT_SCHEMA_VERSION,
};

/// The `wasm32` custom section the frames are placed in.
pub const SECTION: &str = "weaveffi_meta";

/// The infix of every metadata symbol: `{PREFIX}_META_{HASH16}`.
const SYMBOL_INFIX: &str = "_META_";

/// One declaration of a producer's API, as embedded in its library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    /// The schema version the producer was built with
    /// ([`CURRENT_SCHEMA_VERSION`] when built by this version).
    pub schema: String,
    /// The C symbol prefix of the crate that declared the item (its library
    /// name), which tells a crate's own frames from a dependency's.
    pub prefix: String,
    /// The path of the module the item belongs to; for a module frame, its
    /// parent's path (empty for a top-level module).
    pub module: Vec<String>,
    /// The item's position among its siblings of the same kind (functions
    /// among functions, submodules among submodules, and so on), in
    /// declaration order.
    pub index: u32,
    /// The declaration.
    pub item: Item,
}

/// The declaration a [`Frame`] describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "def", rename_all = "snake_case")]
pub enum Item {
    /// A module's own name and doc; its declarations have frames of their
    /// own.
    Module(ModuleHeader),
    /// One of a module's error domains.
    Error(ErrorDomain),
    /// A free function.
    Function(Function),
    /// An interface, with no members: each has a [`Member`](Self::Member)
    /// frame.
    Interface(InterfaceDef),
    /// One constructor, method, or static of an interface.
    Member(Member),
    /// A record.
    Record(StructDef),
    /// An enum.
    Enum(EnumDef),
    /// A callback interface, with its methods.
    Callback(CallbackInterfaceDef),
}

/// The part of a [`Module`] that isn't a declaration of its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleHeader {
    /// The module name.
    pub name: String,
    /// The module's documentation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
}

/// One member of an interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// The name of the interface the member belongs to, declared in the same
    /// module.
    pub interface: String,
    /// Which list of the interface the member is in.
    pub role: Role,
    /// The member's signature.
    pub function: Function,
}

/// Which list of an [`InterfaceDef`] a [`Member`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// [`InterfaceDef::constructors`].
    Constructor,
    /// [`InterfaceDef::methods`].
    Method,
    /// [`InterfaceDef::statics`].
    Static,
}

/// A failure to decode or assemble metadata frames.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MetaError {
    /// The bytes end inside a frame.
    #[error("metadata frame at byte {offset} is truncated")]
    Truncated {
        /// Where the frame starts.
        offset: usize,
    },
    /// A frame's JSON doesn't describe a frame.
    #[error("metadata frame at byte {offset} is malformed: {message}")]
    Malformed {
        /// Where the frame starts.
        offset: usize,
        /// The JSON error.
        message: String,
    },
    /// A frame was written for another schema version.
    #[error(
        "`{path}` was built with WeaveFFI schema {found}, but this CLI reads schema {expected}; \
         build the producer with the same WeaveFFI version as the CLI"
    )]
    Schema {
        /// The declaration's dotted path.
        path: String,
        /// The frame's schema version.
        found: String,
        /// The schema version this crate reads.
        expected: String,
    },
    /// Two different frames describe the same declaration.
    #[error("the library describes `{path}` twice, differently")]
    Conflict {
        /// The declaration's dotted path.
        path: String,
    },
    /// A frame belongs to a module or interface no frame describes.
    #[error("`{path}` belongs to `{parent}`, which the library doesn't describe")]
    Orphan {
        /// The declaration's dotted path.
        path: String,
        /// The missing module or interface.
        parent: String,
    },
}

impl Frame {
    /// The declaration's own name.
    #[must_use]
    pub fn name(&self) -> &str {
        match &self.item {
            Item::Module(m) => &m.name,
            Item::Error(e) => &e.name,
            Item::Function(f) => &f.name,
            Item::Interface(i) => &i.name,
            Item::Member(m) => &m.function.name,
            Item::Record(r) => &r.name,
            Item::Enum(e) => &e.name,
            Item::Callback(c) => &c.name,
        }
    }

    /// The names that locate the declaration inside its module: `[name]`,
    /// `[interface, name]` for a member, and nothing for a module frame.
    #[must_use]
    pub fn local_path(&self) -> Vec<&str> {
        match &self.item {
            Item::Module(_) => Vec::new(),
            Item::Member(m) => vec![&m.interface, &m.function.name],
            _ => vec![self.name()],
        }
    }

    /// The declaration's dotted path (`kv.Store.put`, `kv.stats`).
    #[must_use]
    pub fn path(&self) -> String {
        let mut parts: Vec<&str> = self.module.iter().map(String::as_str).collect();
        match &self.item {
            Item::Module(m) => parts.push(&m.name),
            _ => parts.extend(self.local_path()),
        }
        parts.join(".")
    }

    /// The name of the exported static that holds the frame:
    /// `{PREFIX}_META_{HASH16}`, where `HASH16` is 16 uppercase hex digits
    /// of [`fnv1a64`] over the [dotted path](Self::path) (followed by a `.`
    /// for a module, so a module and a same-named declaration beside it
    /// differ).
    #[must_use]
    pub fn symbol(&self) -> String {
        let mut key = self.path();
        if matches!(self.item, Item::Module(_)) {
            key.push('.');
        }
        format!(
            "{}{SYMBOL_INFIX}{:016X}",
            self.prefix.to_ascii_uppercase(),
            fnv1a64(key.as_bytes())
        )
    }

    /// The frame's bytes: a little-endian `u32` length, then compact JSON.
    ///
    /// # Panics
    ///
    /// Panics if the JSON is longer than `u32::MAX` bytes.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let json = serde_json::to_vec(self).expect("a frame always serializes");
        let len = u32::try_from(json.len()).expect("a frame fits in u32::MAX bytes");
        let mut out = Vec::with_capacity(4 + json.len());
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&json);
        out
    }
}

/// Whether `symbol` names a metadata static: it ends in `_META_` and 16
/// uppercase hex digits. A leading underscore (Mach-O's symbol prefix) is
/// fine; the frame's own prefix decides whose it is.
#[must_use]
pub fn is_symbol(symbol: &str) -> bool {
    let Some((head, hash)) = symbol.rsplit_once(SYMBOL_INFIX) else {
        return false;
    };
    !head.is_empty()
        && hash.len() == 16
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}

/// The byte length of the frame starting at `bytes`, length prefix
/// included, read from its prefix alone.
///
/// # Errors
///
/// Returns [`MetaError::Truncated`] when `bytes` is shorter than the length
/// prefix.
pub fn frame_len(bytes: &[u8]) -> Result<usize, MetaError> {
    let prefix: [u8; 4] = bytes
        .get(..4)
        .and_then(|b| b.try_into().ok())
        .ok_or(MetaError::Truncated { offset: 0 })?;
    Ok(4 + u32::from_le_bytes(prefix) as usize)
}

/// Decode every frame in `bytes`, which holds zero or more frames back to
/// back (one static's contents, or a whole `wasm32` custom section).
///
/// # Errors
///
/// Returns an error when a frame is truncated or its JSON isn't a frame.
pub fn decode(bytes: &[u8]) -> Result<Vec<Frame>, MetaError> {
    let mut frames = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let rest = &bytes[offset..];
        let len = frame_len(rest).map_err(|_| MetaError::Truncated { offset })?;
        let json = rest.get(4..len).ok_or(MetaError::Truncated { offset })?;
        let frame = serde_json::from_slice(json).map_err(|e| MetaError::Malformed {
            offset,
            message: e.to_string(),
        })?;
        frames.push(frame);
        offset += len;
    }
    Ok(frames)
}

/// The frames describing `module` itself and its own declarations (not its
/// submodules'): what the macro embeds for one module of a tree. `parent`
/// is the path of the module's parent and `index` its position among its
/// parent's submodules.
#[must_use]
pub fn module_frames(module: &Module, parent: &[String], index: u32, prefix: &str) -> Vec<Frame> {
    let mut path = parent.to_vec();
    path.push(module.name.clone());
    let frame = |module: &[String], index: usize, item: Item| Frame {
        schema: CURRENT_SCHEMA_VERSION.to_string(),
        prefix: prefix.to_string(),
        module: module.to_vec(),
        index: u32::try_from(index).unwrap_or(u32::MAX),
        item,
    };
    let mut out = vec![frame(
        parent,
        index as usize,
        Item::Module(ModuleHeader {
            name: module.name.clone(),
            doc: module.doc.clone(),
        }),
    )];
    for (i, e) in module.errors.iter().enumerate() {
        out.push(frame(&path, i, Item::Error(e.clone())));
    }
    for (i, f) in module.functions.iter().enumerate() {
        out.push(frame(&path, i, Item::Function(f.clone())));
    }
    for (i, iface) in module.interfaces.iter().enumerate() {
        out.push(frame(
            &path,
            i,
            Item::Interface(InterfaceDef {
                constructors: Vec::new(),
                methods: Vec::new(),
                statics: Vec::new(),
                ..iface.clone()
            }),
        ));
        for (role, members) in [
            (Role::Constructor, &iface.constructors),
            (Role::Method, &iface.methods),
            (Role::Static, &iface.statics),
        ] {
            for (j, function) in members.iter().enumerate() {
                out.push(frame(
                    &path,
                    j,
                    Item::Member(Member {
                        interface: iface.name.clone(),
                        role,
                        function: function.clone(),
                    }),
                ));
            }
        }
    }
    for (i, r) in module.structs.iter().enumerate() {
        out.push(frame(&path, i, Item::Record(r.clone())));
    }
    for (i, e) in module.enums.iter().enumerate() {
        out.push(frame(&path, i, Item::Enum(e.clone())));
    }
    for (i, c) in module.callback_interfaces.iter().enumerate() {
        out.push(frame(&path, i, Item::Callback(c.clone())));
    }
    out
}

/// Every frame of `api` (each module's [`module_frames`], recursively), as
/// though one crate with `prefix` declared it.
#[must_use]
pub fn frames(api: &Api, prefix: &str) -> Vec<Frame> {
    fn walk(modules: &[Module], parent: &[String], prefix: &str, out: &mut Vec<Frame>) {
        for (i, m) in modules.iter().enumerate() {
            out.extend(module_frames(
                m,
                parent,
                u32::try_from(i).unwrap_or(u32::MAX),
                prefix,
            ));
            let mut path = parent.to_vec();
            path.push(m.name.clone());
            walk(&m.modules, &path, prefix, out);
        }
    }
    let mut out = Vec::new();
    walk(&api.modules, &[], prefix, &mut out);
    out
}

/// One module while assembling: its header and its declarations, each with
/// its sibling index.
#[derive(Default)]
struct Node {
    header: Option<(u32, ModuleHeader)>,
    errors: Vec<(u32, ErrorDomain)>,
    functions: Vec<(u32, Function)>,
    interfaces: Vec<(u32, InterfaceDef)>,
    members: Vec<(u32, Member)>,
    structs: Vec<(u32, StructDef)>,
    enums: Vec<(u32, EnumDef)>,
    callbacks: Vec<(u32, CallbackInterfaceDef)>,
}

/// Assemble the [`Api`] that the frames with `prefix` describe, ignoring
/// every other frame (a dependency's). Declarations keep the order the
/// frames record; top-level modules, which separate macro invocations
/// declare, are sorted by name. A frame repeated verbatim (a library that
/// links an object twice) counts once.
///
/// # Errors
///
/// Returns an error when a frame was built for another schema version, two
/// different frames describe the same declaration, or a frame belongs to a
/// module or interface no frame describes.
pub fn assemble(frames: &[Frame], prefix: &str) -> Result<Api, MetaError> {
    let mut seen: BTreeMap<(String, &'static str), &Frame> = BTreeMap::new();
    let mut nodes: BTreeMap<Vec<String>, Node> = BTreeMap::new();
    for frame in frames.iter().filter(|f| f.prefix == prefix) {
        let path = frame.path();
        if frame.schema != CURRENT_SCHEMA_VERSION {
            return Err(MetaError::Schema {
                path,
                found: frame.schema.clone(),
                expected: CURRENT_SCHEMA_VERSION.to_string(),
            });
        }
        let kind = match &frame.item {
            Item::Module(_) => "module",
            Item::Member(_) => "member",
            _ => "declaration",
        };
        match seen.get(&(path.clone(), kind)) {
            Some(prev) if *prev == frame => continue,
            Some(_) => return Err(MetaError::Conflict { path }),
            None => {
                seen.insert((path, kind), frame);
            }
        }
        let i = frame.index;
        match &frame.item {
            Item::Module(h) => {
                let mut full = frame.module.clone();
                full.push(h.name.clone());
                nodes.entry(full).or_default().header = Some((i, h.clone()));
            }
            Item::Error(e) => nodes
                .entry(frame.module.clone())
                .or_default()
                .errors
                .push((i, e.clone())),
            Item::Function(f) => nodes
                .entry(frame.module.clone())
                .or_default()
                .functions
                .push((i, f.clone())),
            Item::Interface(d) => nodes
                .entry(frame.module.clone())
                .or_default()
                .interfaces
                .push((i, d.clone())),
            Item::Member(m) => nodes
                .entry(frame.module.clone())
                .or_default()
                .members
                .push((i, m.clone())),
            Item::Record(r) => nodes
                .entry(frame.module.clone())
                .or_default()
                .structs
                .push((i, r.clone())),
            Item::Enum(e) => nodes
                .entry(frame.module.clone())
                .or_default()
                .enums
                .push((i, e.clone())),
            Item::Callback(c) => nodes
                .entry(frame.module.clone())
                .or_default()
                .callbacks
                .push((i, c.clone())),
        }
    }

    // Every declaration's module must have a header, and every module's
    // parent too.
    for (path, node) in &nodes {
        if node.header.is_none() {
            let (child, parent) = orphan_of(path, node);
            return Err(MetaError::Orphan {
                path: child,
                parent,
            });
        }
        if path.len() > 1 && !nodes.contains_key(&path[..path.len() - 1]) {
            return Err(MetaError::Orphan {
                path: path.join("."),
                parent: path[..path.len() - 1].join("."),
            });
        }
    }

    let mut modules = build(&mut nodes, &[])?;
    // Top-level modules come from separate macro invocations, whose
    // relative order the frames can't record.
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Api {
        version: CURRENT_SCHEMA_VERSION.to_string(),
        modules,
    })
}

/// The first declaration of a header-less module, for the orphan error.
fn orphan_of(path: &[String], node: &Node) -> (String, String) {
    let module = path.join(".");
    let name = node
        .functions
        .first()
        .map(|(_, f)| f.name.clone())
        .or_else(|| node.interfaces.first().map(|(_, d)| d.name.clone()))
        .or_else(|| node.members.first().map(|(_, m)| m.interface.clone()))
        .or_else(|| node.structs.first().map(|(_, d)| d.name.clone()))
        .or_else(|| node.enums.first().map(|(_, d)| d.name.clone()))
        .or_else(|| node.callbacks.first().map(|(_, d)| d.name.clone()))
        .or_else(|| node.errors.first().map(|(_, e)| e.name.clone()))
        .unwrap_or_default();
    (format!("{module}.{name}"), module)
}

/// Sort `(index, item)` pairs by index (then name, for a stable order when
/// a producer's frames disagree) and drop the indices.
fn ordered<T>(mut items: Vec<(u32, T)>, name: impl Fn(&T) -> &str) -> Vec<T> {
    items.sort_by(|(i, a), (j, b)| i.cmp(j).then_with(|| name(a).cmp(name(b))));
    items.into_iter().map(|(_, t)| t).collect()
}

/// Build the children of the module at `parent`, recursively.
fn build(
    nodes: &mut BTreeMap<Vec<String>, Node>,
    parent: &[String],
) -> Result<Vec<Module>, MetaError> {
    let children: Vec<Vec<String>> = nodes
        .keys()
        .filter(|p| p.len() == parent.len() + 1 && p.starts_with(parent))
        .cloned()
        .collect();
    let mut modules = Vec::new();
    for path in children {
        let node = nodes.remove(&path).unwrap_or_default();
        let Some((index, header)) = node.header else {
            continue;
        };
        let mut interfaces = ordered(node.interfaces, |d| &d.name);
        for member in ordered(node.members, |m| &m.function.name) {
            let Some(iface) = interfaces.iter_mut().find(|d| d.name == member.interface) else {
                return Err(MetaError::Orphan {
                    path: format!(
                        "{}.{}.{}",
                        path.join("."),
                        member.interface,
                        member.function.name
                    ),
                    parent: format!("{}.{}", path.join("."), member.interface),
                });
            };
            let list = match member.role {
                Role::Constructor => &mut iface.constructors,
                Role::Method => &mut iface.methods,
                Role::Static => &mut iface.statics,
            };
            list.push(member.function);
        }
        let mut module = Module {
            name: header.name,
            doc: header.doc,
            functions: ordered(node.functions, |f| &f.name),
            interfaces,
            callback_interfaces: ordered(node.callbacks, |d| &d.name),
            structs: ordered(node.structs, |d| &d.name),
            enums: ordered(node.enums, |d| &d.name),
            errors: ordered(node.errors, |d| &d.name),
            modules: Vec::new(),
        };
        module.modules = build(nodes, &path)?;
        modules.push((index, module));
    }
    Ok(ordered(modules, |m| &m.name))
}

#[cfg(all(test, feature = "idl"))]
mod tests {
    use super::*;
    use crate::parse::parse_api_str;

    const API: &str = r#"
version: "0.12.0"
modules:
  - name: kv
    doc: The store.
    errors:
      - name: KvError
        codes:
          - { name: Missing, code: 1, message: missing, fields: [{ name: key, type: string }] }
      - name: IoError
        codes:
          - { name: Disk, code: 1, message: disk }
    functions:
      - { name: open_store, params: [{ name: path, type: string }], return: Store, throws: KvError }
      - { name: sync, params: [{ name: ids, type: "[u64]" }], return: "i32?", throws: any }
      - { name: version, return: u32 }
    interfaces:
      - name: Store
        constructors:
          - { name: new }
          - { name: open, params: [{ name: path, type: string }], throws: IoError }
        methods:
          - { name: put, params: [{ name: key, type: string }] }
          - { name: count, return: u32 }
        statics:
          - { name: default_capacity, return: u32 }
    callback_interfaces:
      - name: Listener
        methods:
          - { name: on_change, params: [{ name: key, type: string }] }
    structs:
      - { name: Entry, fields: [{ name: key, type: string }, { name: tags, type: "[string]" }] }
    enums:
      - { name: Kind, variants: [{ name: A, value: 0 }, { name: B, value: 1 }] }
    modules:
      - name: stats
        functions:
          - { name: summarize, params: [{ name: store, type: Store }], return: u32 }
      - name: admin
        functions:
          - { name: wipe, params: [{ name: store, type: Store }] }
  - name: report
    functions:
      - { name: render, params: [{ name: entries, type: "[Entry]" }], return: "[string]" }
"#;

    fn api() -> Api {
        parse_api_str(API, "yaml").unwrap()
    }

    fn section(frames: &[Frame]) -> Vec<u8> {
        frames.iter().flat_map(Frame::encode).collect()
    }

    #[test]
    fn frames_round_trip_to_the_same_api() {
        let api = api();
        let mut frames = frames(&api, "kvstore");
        // Link order is arbitrary; declaration order comes from the indices.
        frames.reverse();
        let decoded = decode(&section(&frames)).unwrap();
        assert_eq!(decoded, frames);
        assert_eq!(assemble(&decoded, "kvstore").unwrap(), api);
    }

    #[test]
    fn foreign_and_repeated_frames() {
        let api = api();
        let mut all = frames(&api, "kvstore");
        all.extend(frames(&api, "kvstore"));
        all.extend(frames(&api, "other"));
        assert_eq!(assemble(&all, "kvstore").unwrap(), api);
        assert!(assemble(&all, "nobody").unwrap().modules.is_empty());
    }

    #[test]
    fn symbols_paths_and_names() {
        let all = frames(&api(), "kvstore");
        let put = all
            .iter()
            .find(|f| matches!(&f.item, Item::Member(m) if m.function.name == "put"))
            .unwrap();
        assert_eq!(put.path(), "kv.Store.put");
        assert_eq!(put.local_path(), ["Store", "put"]);
        let symbol = put.symbol();
        assert_eq!(
            symbol,
            format!("KVSTORE_META_{:016X}", fnv1a64(b"kv.Store.put"))
        );
        assert!(is_symbol(&symbol));
        assert!(is_symbol(&format!("_{symbol}")));
        assert!(!is_symbol("KVSTORE_META_0123"));
        assert!(!is_symbol("KVSTORE_META_0123456789abcdef"));
        assert!(!is_symbol("_META_0123456789ABCDEF"));
        let stats = all
            .iter()
            .find(|f| matches!(&f.item, Item::Module(m) if m.name == "stats"))
            .unwrap();
        assert_eq!(stats.path(), "kv.stats");
        assert_ne!(
            stats.symbol(),
            format!("KVSTORE_META_{:016X}", fnv1a64(b"kv.stats"))
        );
        let mut symbols: Vec<String> = all.iter().map(Frame::symbol).collect();
        symbols.sort();
        symbols.dedup();
        assert_eq!(symbols.len(), all.len(), "every symbol is unique");
    }

    #[test]
    fn malformed_and_inconsistent_frames_are_errors() {
        let all = frames(&api(), "kvstore");
        let bytes = section(&all[..2]);
        assert!(matches!(
            decode(&bytes[..bytes.len() - 1]),
            Err(MetaError::Truncated { .. })
        ));
        assert!(matches!(
            decode(&[2, 0, 0, 0, b'{', b'}']),
            Err(MetaError::Malformed { offset: 0, .. })
        ));

        let mut stale = all.clone();
        stale[3].schema = "0.10.0".into();
        let err = assemble(&stale, "kvstore").unwrap_err();
        assert!(err.to_string().contains("schema 0.10.0"), "{err}");

        let mut conflict = all.clone();
        let mut changed = all[2].clone();
        changed.index += 7;
        conflict.push(changed);
        assert!(matches!(
            assemble(&conflict, "kvstore"),
            Err(MetaError::Conflict { .. })
        ));

        let headless: Vec<Frame> = all
            .iter()
            .filter(|f| !matches!(&f.item, Item::Module(m) if m.name == "stats"))
            .cloned()
            .collect();
        let err = assemble(&headless, "kvstore").unwrap_err();
        assert_eq!(
            err,
            MetaError::Orphan {
                path: "kv.stats.summarize".into(),
                parent: "kv.stats".into()
            }
        );

        let memberless: Vec<Frame> = all
            .iter()
            .filter(|f| !matches!(&f.item, Item::Interface(_)))
            .cloned()
            .collect();
        assert!(matches!(
            assemble(&memberless, "kvstore"),
            Err(MetaError::Orphan { .. })
        ));
    }
}
