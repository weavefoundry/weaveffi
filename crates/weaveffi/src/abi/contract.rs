//! Contract tables: the per-declaration fingerprints a consumer checks when
//! it loads the library.
//!
//! The `#[weaveffi::module]` expansion exports, for each top-level module
//! `m`, `const {prefix}_contract_entry* {prefix}_{m}_contract(size_t*
//! out_len)`, returning a static table of [`ContractEntry`] values sorted by
//! id: one per declaration (function, interface and each of its members,
//! record, enum, callback interface, and error domain) in `m` and its
//! submodules. The expansion computes every entry at compile time and keeps
//! only those whose `#[cfg]` holds, using [`contract_len`] and
//! [`contract_compact`] in constant context, so the table reflects exactly
//! what the build exports.

/// One declaration's fingerprint (`{prefix}_contract_entry` in C).
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractEntry {
    /// FNV-1a 64 of the declaration's dotted path (`kv.Store.put`).
    pub id: u64,
    /// FNV-1a 64 of the declaration's canonical signature.
    pub hash: u64,
}

impl ContractEntry {
    /// An entry with `id` and `hash`.
    #[must_use]
    pub const fn new(id: u64, hash: u64) -> Self {
        Self { id, hash }
    }
}

/// How many of `entries` are enabled (their flag is `true`).
#[must_use]
pub const fn contract_len<const N: usize>(entries: &[(bool, ContractEntry); N]) -> usize {
    let mut count = 0;
    let mut i = 0;
    while i < N {
        if entries[i].0 {
            count += 1;
        }
        i += 1;
    }
    count
}

/// The enabled entries of `entries`, in order. `M` must be
/// [`contract_len`] of `entries`.
///
/// # Panics
///
/// Panics (at compile time, in a constant) when `M` isn't the number of
/// enabled entries.
#[must_use]
pub const fn contract_compact<const N: usize, const M: usize>(
    entries: [(bool, ContractEntry); N],
) -> [ContractEntry; M] {
    let mut out = [ContractEntry::new(0, 0); M];
    let mut i = 0;
    let mut j = 0;
    while i < N {
        if entries[i].0 {
            out[j] = entries[i].1;
            j += 1;
        }
        i += 1;
    }
    assert!(j == M, "contract table length mismatch");
    out
}

/// The body of `{prefix}_{module}_contract`: write the table's length to
/// `out_len` and return its first entry (null for an empty table).
///
/// # Safety
///
/// `out_len` must be null or point to a writable `usize`.
pub unsafe fn contract_table(
    table: &'static [ContractEntry],
    out_len: *mut usize,
) -> *const ContractEntry {
    if !out_len.is_null() {
        // SAFETY: the caller guarantees `out_len` is writable when non-null.
        unsafe { *out_len = table.len() };
    }
    if table.is_empty() {
        std::ptr::null()
    } else {
        table.as_ptr()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [(bool, ContractEntry); 3] = [
        (true, ContractEntry::new(1, 10)),
        (false, ContractEntry::new(2, 20)),
        (true, ContractEntry::new(3, 30)),
    ];
    const LEN: usize = contract_len(&ALL);
    static TABLE: [ContractEntry; LEN] = contract_compact(ALL);

    #[test]
    fn disabled_entries_are_compacted_out() {
        assert_eq!(LEN, 2);
        assert_eq!(
            TABLE,
            [ContractEntry::new(1, 10), ContractEntry::new(3, 30)]
        );
        let mut len = 0;
        let ptr = unsafe { contract_table(&TABLE, &mut len) };
        assert_eq!(len, 2);
        assert_eq!(unsafe { *ptr.add(1) }.hash, 30);
        assert!(unsafe { contract_table(&[], &mut len) }.is_null());
        assert_eq!(len, 0);
    }
}
