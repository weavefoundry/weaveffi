//! Compile-time metadata: type spellings, contract signatures, and the
//! metadata frames a producer embeds.
//!
//! The producer macros only see the tokens of the item they annotate, so a
//! parameter's IDL spelling (`[Entry?]`, `{string:u64}`) comes from its
//! type's [`FfiType::NAME`](crate::rt::FfiType::NAME), a [`Name`] each type
//! builds at compile time from its parts. A declaration's metadata frame and
//! contract signature are then sequences of [`Part`]s (literal text from the
//! macro, names from the types) that [`frame`] and [`hash`] assemble in
//! `const` context. The CLI computes the same signature strings from the IR
//! (`weaveffi_model::contract`), which is what makes a binding's contract
//! check agree with the library it loads.

/// The capacity of a [`Name`] in bytes.
pub const NAME_CAP: usize = 256;

/// A type's IDL spelling, built at compile time (`i32`, `[Entry?]`).
#[derive(Clone, Copy)]
pub struct Name {
    bytes: [u8; NAME_CAP],
    len: usize,
}

impl Name {
    /// The empty name (a callable that throws nothing has an empty
    /// [`FfiReturn::DOMAIN`](crate::rt::FfiReturn::DOMAIN)).
    pub const EMPTY: Name = Name {
        bytes: [0; NAME_CAP],
        len: 0,
    };

    /// The name `s`.
    #[must_use]
    pub const fn new(s: &str) -> Name {
        Name::EMPTY.push(s.as_bytes())
    }

    /// This name followed by `more`.
    ///
    /// # Panics
    ///
    /// Panics (at compile time) when the result exceeds [`NAME_CAP`] bytes.
    #[must_use]
    pub const fn push(mut self, more: &[u8]) -> Name {
        assert!(
            self.len + more.len() <= NAME_CAP,
            "a WeaveFFI type spelling is longer than 256 bytes"
        );
        let mut i = 0;
        while i < more.len() {
            self.bytes[self.len + i] = more[i];
            i += 1;
        }
        self.len += more.len();
        self
    }

    /// `inner?`.
    #[must_use]
    pub const fn optional(inner: &Name) -> Name {
        Name::EMPTY.push(inner.as_bytes()).push(b"?")
    }

    /// `[inner]`.
    #[must_use]
    pub const fn list(inner: &Name) -> Name {
        Name::new("[").push(inner.as_bytes()).push(b"]")
    }

    /// `{key:value}`.
    #[must_use]
    pub const fn map(key: &Name, value: &Name) -> Name {
        Name::new("{")
            .push(key.as_bytes())
            .push(b":")
            .push(value.as_bytes())
            .push(b"}")
    }

    /// `iter<inner>`.
    #[must_use]
    pub const fn iter(inner: &Name) -> Name {
        Name::new("iter<").push(inner.as_bytes()).push(b">")
    }

    /// A contract signature's ` throws` suffix for the domain `domain`
    /// (`KvError`, or `any` for an untyped error): ` throws {domain}`, or
    /// nothing for the empty domain of an infallible callable.
    #[must_use]
    pub const fn suffix(domain: &Name) -> Name {
        if domain.len == 0 {
            Name::EMPTY
        } else {
            Name::new(" throws ").push(domain.as_bytes())
        }
    }

    /// The spelling's bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        self.bytes.split_at(self.len).0
    }

    /// Whether the spelling is `s`.
    #[must_use]
    pub const fn is(&self, s: &str) -> bool {
        let s = s.as_bytes();
        if s.len() != self.len {
            return false;
        }
        let mut i = 0;
        while i < s.len() {
            if s[i] != self.bytes[i] {
                return false;
            }
            i += 1;
        }
        true
    }
}

impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(self.as_bytes()))
    }
}

/// One part of a frame or a signature.
#[derive(Clone, Copy)]
pub enum Part<'a> {
    /// Literal bytes.
    Lit(&'a str),
    /// A type's spelling (or a ` throws` suffix).
    Name(&'a Name),
    /// A source path, with `\` written as `/` (it lands inside a JSON
    /// string, and paths never contain `"`).
    Path(&'a str),
    /// A decimal number.
    Num(u32),
}

const fn digits(mut n: u32) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

const fn part_len(part: &Part<'_>) -> usize {
    match part {
        Part::Lit(s) | Part::Path(s) => s.len(),
        Part::Name(n) => n.len,
        Part::Num(n) => digits(*n),
    }
}

/// The total length of `parts`.
#[must_use]
pub const fn len(parts: &[Part<'_>]) -> usize {
    let mut total = 0;
    let mut i = 0;
    while i < parts.len() {
        total += part_len(&parts[i]);
        i += 1;
    }
    total
}

/// Call `f` on each byte of `parts`, in order (as a const-friendly loop the
/// callers inline).
const fn byte_at(parts: &[Part<'_>], mut index: usize) -> u8 {
    let mut i = 0;
    while i < parts.len() {
        let n = part_len(&parts[i]);
        if index < n {
            return match &parts[i] {
                Part::Lit(s) => s.as_bytes()[index],
                Part::Path(s) => match s.as_bytes()[index] {
                    b'\\' => b'/',
                    b => b,
                },
                Part::Name(name) => name.bytes[index],
                Part::Num(num) => {
                    let mut v = *num;
                    let mut k = n - 1 - index;
                    while k > 0 {
                        v /= 10;
                        k -= 1;
                    }
                    b'0' + (v % 10) as u8
                }
            };
        }
        index -= n;
        i += 1;
    }
    0
}

/// The 64-bit FNV-1a hash of the concatenation of `parts`: a contract
/// signature's hash.
#[must_use]
pub const fn hash(parts: &[Part<'_>]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let total = len(parts);
    let mut i = 0;
    while i < total {
        hash ^= byte_at(parts, i) as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// The byte length of the frame [`frame`] builds from `parts`: a `u32`
/// length prefix and the parts.
#[must_use]
pub const fn frame_len(parts: &[Part<'_>]) -> usize {
    4 + len(parts)
}

/// A metadata frame: a little-endian `u32` length, then the concatenation
/// of `parts` (compact JSON). `N` must be [`frame_len`] of `parts`.
///
/// # Panics
///
/// Panics (at compile time) when `N` is wrong.
#[must_use]
pub const fn frame<const N: usize>(parts: &[Part<'_>]) -> [u8; N] {
    let body = len(parts);
    assert!(N == body + 4, "frame length mismatch");
    let mut out = [0u8; N];
    let prefix = (body as u32).to_le_bytes();
    out[0] = prefix[0];
    out[1] = prefix[1];
    out[2] = prefix[2];
    out[3] = prefix[3];
    let mut i = 0;
    while i < body {
        out[4 + i] = byte_at(parts, i);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY: Name = Name::new("Entry");
    const SPELLING: Name = Name::map(&Name::new("string"), &Name::list(&Name::optional(&ENTRY)));

    #[test]
    fn names_compose() {
        assert_eq!(SPELLING.as_bytes(), b"{string:[Entry?]}");
        assert!(Name::iter(&ENTRY).is("iter<Entry>"));
        assert!(Name::suffix(&Name::new("any")).is(" throws any"));
        assert!(Name::suffix(&Name::EMPTY).is(""));
        assert!(Name::EMPTY.is(""));
    }

    #[test]
    fn frames_and_hashes_concatenate_parts() {
        const PARTS: &[Part<'static>] = &[
            Part::Lit("method get("),
            Part::Name(&Name::new("string")),
            Part::Lit(") -> "),
            Part::Name(&ENTRY),
            Part::Name(&Name::suffix(&Name::new("KvError"))),
        ];
        let text = b"method get(string) -> Entry throws KvError";
        let expected = {
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for b in text {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
            h
        };
        assert_eq!(hash(PARTS), expected);
        const LEN: usize = frame_len(PARTS);
        const FRAME: [u8; LEN] = frame::<LEN>(PARTS);
        assert_eq!(&FRAME[..4], &(text.len() as u32).to_le_bytes());
        assert_eq!(&FRAME[4..], text);

        const LOC: &[Part<'static>] = &[
            Part::Path("src\\lib.rs"),
            Part::Lit(":"),
            Part::Num(1207),
            Part::Lit(":"),
            Part::Num(5),
            Part::Num(0),
        ];
        const L: usize = len(LOC);
        const BYTES: [u8; L + 4] = frame::<{ L + 4 }>(LOC);
        assert_eq!(&BYTES[4..], b"src/lib.rs:1207:50");
    }
}
