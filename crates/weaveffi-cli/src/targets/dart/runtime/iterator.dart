// ── Iterators ──

/// Anchors one live native iterator for its GC-finalizer backstop. A
/// suspended `sync*` body keeps its anchor reachable; abandoning the
/// iteration drops the body, and the finalizer destroys the native iterator.
/// An iteration that runs to completion (or fails) detaches and destroys it
/// eagerly instead, so the iterator is destroyed exactly once either way.
final class _IteratorAnchor implements Finalizable {}

