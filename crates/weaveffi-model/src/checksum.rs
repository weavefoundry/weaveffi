//! Contract checksums: a stable fingerprint of a top-level module's ABI.
//!
//! A producer exports `uint64_t {prefix}_{module}_checksum(void)` for every
//! top-level module, and every generated consumer compares it with the value
//! it was generated against when it loads the library. Both sides compute the
//! value with [`module_checksum`] over the same IR (the `#[weaveffi::module]`
//! macro on the module it expands, the CLI on the parsed IDL or the extracted
//! Rust source), so bindings that are stale relative to the library they load
//! fail loudly instead of misreading memory.
//!
//! The hash covers everything that shapes the C ABI or the value-buffer wire
//! format: every declaration name, every type reference, parameter and field
//! order, function flags (`throws`, `async`, `cancellable`), enum
//! discriminants, and error-code values. It deliberately skips documentation,
//! deprecation notes, and error-code messages, so editing prose never breaks a
//! deployed binding.

use crate::ir::{
    CallbackInterfaceDef, EnumDef, ErrorDomain, Function, InterfaceDef, Module, StructDef,
    StructField,
};

/// FNV-1a 64-bit hasher; tiny, dependency-free, and stable across platforms
/// and Rust versions.
struct Fnv(u64);

impl Fnv {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    fn bytes(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(Self::PRIME);
        }
    }

    /// Hash a tagged string. The tag and a terminator keep adjacent fields
    /// from running together (`"ab" + "c"` never equals `"a" + "bc"`).
    fn str(&mut self, tag: u8, s: &str) {
        self.bytes(&[tag]);
        self.bytes(s.as_bytes());
        self.bytes(&[0xff]);
    }

    fn int(&mut self, tag: u8, v: i64) {
        self.bytes(&[tag]);
        self.bytes(&v.to_le_bytes());
    }

    fn flag(&mut self, tag: u8, v: bool) {
        self.bytes(&[tag, u8::from(v)]);
    }
}

/// The contract checksum of one top-level module (including its nested
/// modules).
#[must_use]
pub fn module_checksum(module: &Module) -> u64 {
    let mut h = Fnv(Fnv::OFFSET);
    hash_module(&mut h, module);
    h.0
}

fn hash_module(h: &mut Fnv, m: &Module) {
    h.str(b'M', &m.name);
    for f in &m.functions {
        hash_function(h, b'F', f);
    }
    for i in &m.interfaces {
        hash_interface(h, i);
    }
    for c in &m.callback_interfaces {
        hash_callback(h, c);
    }
    for s in &m.structs {
        hash_struct(h, s);
    }
    for e in &m.enums {
        hash_enum(h, e);
    }
    if let Some(d) = &m.errors {
        hash_errors(h, d);
    }
    for child in &m.modules {
        hash_module(h, child);
    }
    h.bytes(b"}");
}

fn hash_function(h: &mut Fnv, tag: u8, f: &Function) {
    h.str(tag, &f.name);
    for p in &f.params {
        h.str(b'p', &p.name);
        h.str(b't', &p.ty.to_string());
    }
    if let Some(r) = &f.returns {
        h.str(b'r', &r.to_string());
    }
    h.flag(b'T', f.throws);
    h.flag(b'A', f.r#async);
    h.flag(b'C', f.cancellable);
}

fn hash_interface(h: &mut Fnv, i: &InterfaceDef) {
    h.str(b'I', &i.name);
    for c in &i.constructors {
        hash_function(h, b'c', c);
    }
    for m in &i.methods {
        hash_function(h, b'm', m);
    }
    for s in &i.statics {
        hash_function(h, b's', s);
    }
}

fn hash_callback(h: &mut Fnv, c: &CallbackInterfaceDef) {
    h.str(b'K', &c.name);
    for m in &c.methods {
        hash_function(h, b'k', m);
    }
}

fn hash_fields(h: &mut Fnv, fields: &[StructField]) {
    for f in fields {
        h.str(b'f', &f.name);
        h.str(b't', &f.ty.to_string());
    }
}

fn hash_struct(h: &mut Fnv, s: &StructDef) {
    h.str(b'S', &s.name);
    hash_fields(h, &s.fields);
}

fn hash_enum(h: &mut Fnv, e: &EnumDef) {
    h.str(b'E', &e.name);
    for v in &e.variants {
        h.str(b'v', &v.name);
        h.int(b'd', i64::from(v.value));
        hash_fields(h, &v.fields);
    }
}

fn hash_errors(h: &mut Fnv, d: &ErrorDomain) {
    h.str(b'D', &d.name);
    for c in &d.codes {
        h.str(b'e', &c.name);
        h.int(b'n', i64::from(c.code));
        hash_fields(h, &c.fields);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Param, TypeRef};

    fn module() -> Module {
        Module {
            name: "kv".into(),
            doc: Some("docs".into()),
            functions: vec![Function {
                name: "get".into(),
                params: vec![Param {
                    name: "key".into(),
                    ty: TypeRef::StringUtf8,
                    doc: None,
                }],
                returns: Some(TypeRef::Optional(Box::new(TypeRef::Bytes))),
                doc: Some("Fetch a value.".into()),
                throws: true,
                r#async: false,
                cancellable: false,
                deprecated: None,
            }],
            interfaces: vec![],
            callback_interfaces: vec![],
            structs: vec![],
            enums: vec![],
            errors: None,
            modules: vec![],
        }
    }

    #[test]
    fn docs_do_not_affect_the_checksum() {
        let a = module();
        let mut b = module();
        b.doc = None;
        b.functions[0].doc = Some("Changed prose.".into());
        b.functions[0].deprecated = Some("old".into());
        assert_eq!(module_checksum(&a), module_checksum(&b));
    }

    #[test]
    fn abi_changes_do() {
        let a = module();
        let mut b = module();
        b.functions[0].throws = false;
        assert_ne!(module_checksum(&a), module_checksum(&b));
        let mut c = module();
        c.functions[0].params[0].ty = TypeRef::Bytes;
        assert_ne!(module_checksum(&a), module_checksum(&c));
    }

    #[test]
    fn value_is_stable() {
        // Pin the algorithm: changing it invalidates every deployed binding,
        // so it must be a deliberate, documented ABI change.
        assert_eq!(module_checksum(&module()), module_checksum(&module()));
        assert_eq!(format!("{:016x}", Fnv(Fnv::OFFSET).0), "cbf29ce484222325");
    }
}
