//! Debug leak counters behind the `leak-check` cargo feature.
//!
//! With the feature on, the runtime counts every resource it hands to a
//! consumer and every release, so a test harness can assert that a consumer
//! released everything it was given (after forcing its garbage collector or
//! finalizers to run). `export_runtime!` exports the counters as
//! `uint64_t {prefix}_debug_live(int32_t kind)`.
//!
//! The symbol is exported whether or not the feature is on, so a consumer
//! never has to know how the producer was built: without the feature every
//! counter reads `0`. Check [`ENABLED`] to tell "nothing is live" from
//! "nothing is counted".

/// Whether this build counts live resources (the `leak-check` feature).
pub const ENABLED: bool = cfg!(feature = "leak-check");

/// Interface object references held by the consumer: one per pointer a
/// thunk returns, per `_clone`, and per object token written into a value
/// buffer; released by `_destroy` and by adopting a token.
pub const OBJECTS: i32 = 0;

/// Consumer callback-interface implementations the producer still holds
/// (each is released through the vtable's `free` entry).
pub const CALLBACKS: i32 = 1;

/// Iterator handles a launcher returned that haven't been destroyed.
pub const ITERATORS: i32 = 2;

/// Cancel-token references, both the consumer's (`create` until `destroy`)
/// and the producer's (one per in-flight cancellable call).
pub const TOKENS: i32 = 3;

/// Returned string, bytes, and value-buffer allocations (including error
/// payloads) that haven't been passed to `{prefix}_free_bytes`.
pub const ALLOCATIONS: i32 = 4;

#[cfg(feature = "leak-check")]
mod counters {
    use std::sync::atomic::{AtomicI64, Ordering};

    static LIVE: [AtomicI64; 5] = [
        AtomicI64::new(0),
        AtomicI64::new(0),
        AtomicI64::new(0),
        AtomicI64::new(0),
        AtomicI64::new(0),
    ];

    pub(super) fn track(kind: i32, delta: i64) {
        if let Some(c) = usize::try_from(kind).ok().and_then(|k| LIVE.get(k)) {
            c.fetch_add(delta, Ordering::Relaxed);
        }
    }

    pub(super) fn live(kind: i32) -> u64 {
        usize::try_from(kind)
            .ok()
            .and_then(|k| LIVE.get(k))
            .map_or(0, |c| u64::try_from(c.load(Ordering::Relaxed)).unwrap_or(0))
    }
}

/// Adjust the live count of `kind` by `delta`. Compiles to nothing without
/// the `leak-check` feature.
#[inline]
pub(crate) fn track(kind: i32, delta: i64) {
    #[cfg(feature = "leak-check")]
    counters::track(kind, delta);
    #[cfg(not(feature = "leak-check"))]
    let _ = (kind, delta);
}

/// The body of `{prefix}_debug_live`: how many resources of `kind` (one of
/// [`OBJECTS`], [`CALLBACKS`], [`ITERATORS`], [`TOKENS`], or
/// [`ALLOCATIONS`]) are live right now.
///
/// Returns `0` for an unknown kind, and for every kind when the
/// `leak-check` feature is off.
#[must_use]
pub fn debug_live(kind: i32) -> u64 {
    #[cfg(feature = "leak-check")]
    {
        counters::live(kind)
    }
    #[cfg(not(feature = "leak-check"))]
    {
        let _ = kind;
        0
    }
}
