//! Library metadata: the API a Rust producer embeds in its compiled library.
//!
//! Each `#[weaveffi::...]` attribute describes the declaration it exports in
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
//! One frame describes one declaration: a free function, an interface
//! (without its members), one interface member, a record, an enum, an error
//! domain, or a callback interface. Each frame records the source location
//! of its attribute, so [`assemble`] orders declarations by where they're
//! written (file, then line, then column), whatever order the linker laid
//! the statics out in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::contract::fnv1a64;
use crate::ir::{
    Api, CallbackDef, EnumDef, ErrorDomain, Function, InterfaceDef, RecordDef, Throws, TypeRef,
    CURRENT_SCHEMA_VERSION,
};
use crate::model::ABI_VERSION;

/// The `wasm32` custom section the frames are placed in.
pub const SECTION: &str = "weaveffi_meta";

/// The infix of every metadata symbol: `{PREFIX}_META_{HASH16}`.
const SYMBOL_INFIX: &str = "_META_";

/// One declaration of a producer's API, as embedded in its library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    /// The schema version the producer was built with.
    pub schema: String,
    /// The C ABI revision the producer was built with.
    pub abi: u32,
    /// The C symbol prefix of the crate that declared the item (its library
    /// name), which tells a crate's own frames from a dependency's.
    pub prefix: String,
    /// Where the declaration's attribute is written.
    pub at: Location,
    /// The declaration.
    pub item: Item,
}

/// A source location: the file as the compiler names it, and a 1-based
/// line and column.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    /// The source file, with `/` separators.
    pub file: String,
    /// The line.
    pub line: u32,
    /// The column.
    pub column: u32,
}

/// The declaration a [`Frame`] describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "def", rename_all = "snake_case")]
pub enum Item {
    /// An error domain.
    Error(ErrorDomain),
    /// A free function.
    Function(Function),
    /// An interface, with no members: each has a [`Member`](Self::Member)
    /// frame.
    Interface(InterfaceDef),
    /// One constructor, method, or static of an interface.
    Member(Member),
    /// A record.
    Record(RecordDef),
    /// An enum.
    Enum(EnumDef),
    /// A callback interface, with its methods.
    Callback(CallbackDef),
}

/// One member of an interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// The name of the interface the member belongs to.
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
    /// A frame was written by another WeaveFFI version.
    #[error(
        "`{path}` was built with WeaveFFI schema {found} (C ABI {found_abi}), but this CLI reads \
         schema {expected} (C ABI {expected_abi}); build the producer with the same WeaveFFI \
         version as the CLI"
    )]
    Version {
        /// The declaration's dotted path.
        path: String,
        /// The frame's schema version.
        found: String,
        /// The frame's ABI revision.
        found_abi: u32,
        /// The schema version this crate reads.
        expected: String,
        /// The ABI revision this crate lowers to.
        expected_abi: u32,
    },
    /// Two different frames describe the same declaration.
    #[error("the library describes `{path}` twice, differently")]
    Conflict {
        /// The declaration's dotted path.
        path: String,
    },
    /// A member's interface has no frame.
    #[error(
        "`{path}` is a member of `{interface}`, which the library doesn't export; mark the \
         struct #[weaveffi::interface]"
    )]
    Orphan {
        /// The member's dotted path.
        path: String,
        /// The missing interface.
        interface: String,
    },
}

impl Frame {
    /// A frame of the current schema and ABI revision.
    #[must_use]
    pub fn new(prefix: &str, at: Location, item: Item) -> Self {
        Self {
            schema: CURRENT_SCHEMA_VERSION.to_string(),
            abi: ABI_VERSION,
            prefix: prefix.to_string(),
            at,
            item,
        }
    }

    /// The declaration's own name.
    #[must_use]
    pub fn name(&self) -> &str {
        match &self.item {
            Item::Error(e) => &e.name,
            Item::Function(f) => &f.name,
            Item::Interface(i) => &i.name,
            Item::Member(m) => &m.function.name,
            Item::Record(r) => &r.name,
            Item::Enum(e) => &e.name,
            Item::Callback(c) => &c.name,
        }
    }

    /// The declaration's dotted path: its name, or `Interface.member`.
    #[must_use]
    pub fn path(&self) -> String {
        match &self.item {
            Item::Member(m) => format!("{}.{}", m.interface, m.function.name),
            _ => self.name().to_string(),
        }
    }

    /// The name of the exported static that holds the frame:
    /// `{PREFIX}_META_{HASH16}`, where `HASH16` is 16 upper-case hex digits
    /// of [`fnv1a64`] over the [dotted path](Self::path).
    #[must_use]
    pub fn symbol(&self) -> String {
        symbol(&self.prefix, &self.path())
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

/// The metadata symbol of the declaration `path` in the library with
/// `prefix` (see [`Frame::symbol`]).
#[must_use]
pub fn symbol(prefix: &str, path: &str) -> String {
    format!(
        "{}{SYMBOL_INFIX}{:016X}",
        prefix.to_ascii_uppercase(),
        fnv1a64(path.as_bytes())
    )
}

/// Whether `symbol` names a metadata static: it ends in `_META_` and 16
/// upper-case hex digits. A leading underscore (Mach-O's symbol prefix) is
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

/// Every frame of `api`, as though one crate with `prefix` declared it in
/// one file, in document order (for tests and tools that synthesize
/// metadata).
#[must_use]
pub fn frames(api: &Api, prefix: &str) -> Vec<Frame> {
    let mut line = 0u32;
    let mut frame = |item| {
        line += 1;
        Frame::new(
            prefix,
            Location {
                file: "src/lib.rs".to_string(),
                line,
                column: 1,
            },
            item,
        )
    };
    let mut out = Vec::new();
    for r in &api.records {
        out.push(frame(Item::Record(r.clone())));
    }
    for e in &api.enums {
        out.push(frame(Item::Enum(e.clone())));
    }
    for d in &api.errors {
        out.push(frame(Item::Error(d.clone())));
    }
    for i in &api.interfaces {
        out.push(frame(Item::Interface(InterfaceDef {
            constructors: Vec::new(),
            methods: Vec::new(),
            statics: Vec::new(),
            ..i.clone()
        })));
        for (role, list) in [
            (Role::Constructor, &i.constructors),
            (Role::Method, &i.methods),
            (Role::Static, &i.statics),
        ] {
            for f in list {
                out.push(frame(Item::Member(Member {
                    interface: i.name.clone(),
                    role,
                    function: f.clone(),
                })));
            }
        }
    }
    for c in &api.callbacks {
        out.push(frame(Item::Callback(c.clone())));
    }
    for f in &api.functions {
        out.push(frame(Item::Function(f.clone())));
    }
    out
}

/// Assemble the [`Api`] that the frames with `prefix` describe, ignoring
/// every other frame (a dependency's). Declarations of each kind are in
/// source order. A frame repeated verbatim (a library that links an object
/// twice) counts once.
///
/// # Errors
///
/// Returns an error when a frame was built by another WeaveFFI version, two
/// different frames describe the same declaration, or a member's interface
/// has no frame.
pub fn assemble(frames: &[Frame], prefix: &str) -> Result<Api, MetaError> {
    let mut seen: BTreeMap<(String, bool), &Frame> = BTreeMap::new();
    let mut ours: Vec<&Frame> = Vec::new();
    for frame in frames.iter().filter(|f| f.prefix == prefix) {
        let path = frame.path();
        if frame.schema != CURRENT_SCHEMA_VERSION || frame.abi != ABI_VERSION {
            return Err(MetaError::Version {
                path,
                found: frame.schema.clone(),
                found_abi: frame.abi,
                expected: CURRENT_SCHEMA_VERSION.to_string(),
                expected_abi: ABI_VERSION,
            });
        }
        let key = (path.clone(), matches!(frame.item, Item::Member(_)));
        match seen.get(&key) {
            Some(prev) if *prev == frame => continue,
            Some(_) => return Err(MetaError::Conflict { path }),
            None => {
                seen.insert(key, frame);
            }
        }
        ours.push(frame);
    }
    ours.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.path().cmp(&b.path())));

    let mut api = Api::new();
    let mut members = Vec::new();
    for frame in ours {
        match &frame.item {
            Item::Error(e) => api.errors.push(e.clone()),
            Item::Function(f) => api.functions.push(f.clone()),
            Item::Interface(i) => api.interfaces.push(i.clone()),
            Item::Member(m) => members.push(m.clone()),
            Item::Record(r) => api.records.push(r.clone()),
            Item::Enum(e) => api.enums.push(e.clone()),
            Item::Callback(c) => api.callbacks.push(c.clone()),
        }
    }
    for f in api.functions.iter_mut() {
        normalize(f);
    }
    for f in api.callbacks.iter_mut().flat_map(|c| c.methods.iter_mut()) {
        normalize(f);
    }
    for mut member in members {
        normalize(&mut member.function);
        let Some(iface) = api
            .interfaces
            .iter_mut()
            .find(|i| i.name == member.interface)
        else {
            return Err(MetaError::Orphan {
                path: format!("{}.{}", member.interface, member.function.name),
                interface: member.interface,
            });
        };
        let list = match member.role {
            Role::Constructor => &mut iface.constructors,
            Role::Method => &mut iface.methods,
            Role::Static => &mut iface.statics,
        };
        list.push(member.function);
    }
    Ok(api)
}

/// Undo the spellings a producer's frame uses for "nothing": a callable
/// that returns nothing names its return `void`, and one that throws
/// nothing names an empty domain.
fn normalize(f: &mut Function) {
    if matches!(&f.returns, Some(TypeRef::Named(n)) if n == crate::contract::VOID) {
        f.returns = None;
    }
    if matches!(&f.throws, Some(Throws::Domain(d)) if d.is_empty()) {
        f.throws = None;
    }
}

#[cfg(all(test, feature = "idl"))]
mod tests {
    use super::*;
    use crate::parse::parse_api_str;

    const API: &str = r#"
version: "0.13.0"
records:
  - { name: Entry, fields: [{ name: key, type: string }, { name: tags, type: "[string]" }] }
enums:
  - { name: Kind, variants: [{ name: A }, { name: B }] }
errors:
  - name: KvError
    codes:
      - { name: Missing, code: 1, message: missing, fields: [{ name: key, type: string }] }
interfaces:
  - name: Store
    constructors:
      - { name: new }
      - { name: open, params: [{ name: path, type: string }], throws: KvError }
    methods:
      - { name: put, params: [{ name: key, type: string }] }
      - { name: count, return: u32 }
    statics:
      - { name: default_capacity, return: u32 }
callbacks:
  - name: Listener
    methods:
      - { name: on_change, params: [{ name: key, type: string }] }
functions:
  - { name: open_store, params: [{ name: path, type: string }], return: Store, throws: KvError }
  - { name: sync, params: [{ name: ids, type: "[u64]" }], return: "i32?", throws: any }
"#;

    fn api() -> Api {
        parse_api_str(API, "yaml").unwrap()
    }

    fn section(frames: &[Frame]) -> Vec<u8> {
        frames.iter().flat_map(Frame::encode).collect()
    }

    #[test]
    fn frames_round_trip_to_the_same_api_in_any_link_order() {
        let api = api();
        let mut frames = frames(&api, "kvstore");
        frames.reverse();
        let decoded = decode(&section(&frames)).unwrap();
        assert_eq!(decoded, frames);
        assert_eq!(assemble(&decoded, "kvstore").unwrap(), api);
    }

    #[test]
    fn a_producers_void_return_and_empty_throws_mean_none() {
        let mut f = Function::named("f");
        f.returns = Some(TypeRef::Named("void".into()));
        f.throws = Some(Throws::Domain(String::new()));
        let frame = Frame::new("kv", Location::default(), Item::Function(f));
        let api = assemble(&[frame], "kv").unwrap();
        assert_eq!(api.functions[0], Function::named("f"));
    }

    #[test]
    fn foreign_and_repeated_frames() {
        let api = api();
        let mut all = frames(&api, "kvstore");
        all.extend(frames(&api, "kvstore"));
        all.extend(frames(&api, "other"));
        assert_eq!(assemble(&all, "kvstore").unwrap(), api);
        assert_eq!(assemble(&all, "nobody").unwrap(), Api::new());
    }

    #[test]
    fn symbols_and_paths() {
        let all = frames(&api(), "kvstore");
        let put = all
            .iter()
            .find(|f| matches!(&f.item, Item::Member(m) if m.function.name == "put"))
            .unwrap();
        assert_eq!(put.path(), "Store.put");
        let symbol = put.symbol();
        assert_eq!(
            symbol,
            format!("KVSTORE_META_{:016X}", fnv1a64(b"Store.put"))
        );
        assert!(is_symbol(&symbol));
        assert!(is_symbol(&format!("_{symbol}")));
        assert!(!is_symbol("KVSTORE_META_0123"));
        assert!(!is_symbol("KVSTORE_META_0123456789abcdef"));
        assert!(!is_symbol("_META_0123456789ABCDEF"));
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
        stale[3].abi = 5;
        let err = assemble(&stale, "kvstore").unwrap_err();
        assert!(err.to_string().contains("C ABI 5"), "{err}");

        let mut conflict = all.clone();
        let mut changed = all[2].clone();
        changed.at.line += 70;
        conflict.push(changed);
        assert!(matches!(
            assemble(&conflict, "kvstore"),
            Err(MetaError::Conflict { .. })
        ));

        let memberless: Vec<Frame> = all
            .iter()
            .filter(|f| !matches!(&f.item, Item::Interface(_)))
            .cloned()
            .collect();
        let err = assemble(&memberless, "kvstore").unwrap_err();
        assert_eq!(
            err,
            MetaError::Orphan {
                path: "Store.new".into(),
                interface: "Store".into()
            }
        );
    }
}
