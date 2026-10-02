//! Doc-comment emission: `///` comments carried from the IDL, plus the
//! generated notes (streaming contract, ownership, thrown exception,
//! cancellation, deprecation) a wrapper's declaration carries.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use weaveffi_model::model::{CallShape, FnBinding, Ty};

use crate::targets::dart::calls::ErrCtx;
use crate::targets::dart::types::dart_str_literal;

/// Emit `doc` at the writer's current depth.
pub(crate) fn write_doc(w: &mut CodeWriter, doc: &Option<String>) {
    w.doc(doc, DocCommentStyle::TripleSlash);
}

/// Emit `@Deprecated(...)` when `deprecated` carries a message.
pub(crate) fn write_deprecated(w: &mut CodeWriter, deprecated: &Option<String>) {
    if let Some(msg) = deprecated {
        w.line(format!("@Deprecated('{}')", dart_str_literal(msg)));
    }
}

/// Emit a wrapper's doc comment followed by the generated notes that apply
/// to it, then its `@Deprecated` annotation.
pub(crate) fn emit_wrapper_doc(w: &mut CodeWriter, f: &FnBinding, err: ErrCtx) {
    write_doc(w, &f.doc);
    let mut notes: Vec<&str> = Vec::new();
    let mut owned: Vec<String> = Vec::new();
    if let CallShape::Iterator(ib) = &f.shape {
        notes.extend([
            "Returns a lazy [Iterable]: each step pulls one element from a native",
            "iterator, and iterating again starts a new one. The native iterator",
            "is released when the iteration ends, fails, or is abandoned and",
            "collected.",
        ]);
        if ib.elem.interface_name().is_some() {
            notes.extend([
                "",
                "Each element is owned by the caller: call its `dispose()` when",
                "you're done with it.",
            ]);
        }
    } else if let Some(ret) = f.ret.as_ref().filter(|r| r.interface_name().is_some()) {
        if matches!(ret, Ty::Optional(_)) {
            notes.extend([
                "Returns null when the producer reports no object. A returned object",
                "is owned by the caller: call its `dispose()` when you're done with it.",
            ]);
        } else {
            notes.extend([
                "The returned object is owned by the caller: call its `dispose()`",
                "when you're done with it.",
            ]);
        }
    }
    if f.cancellable {
        if !notes.is_empty() {
            notes.push("");
        }
        notes.extend([
            "Cancelling [cancelToken] completes the returned future with a",
            "[CancelledException].",
        ]);
    }
    if let Some(exc) = err.thrown_exception() {
        if !notes.is_empty() {
            notes.push("");
        }
        owned.push(format!("Throws [{exc}] on domain errors."));
    }
    if !notes.is_empty() || !owned.is_empty() {
        if f.doc.is_some() {
            w.line("///");
        }
        for n in notes {
            w.line(if n.is_empty() {
                "///".to_string()
            } else {
                format!("/// {n}")
            });
        }
        for n in owned {
            w.line(format!("/// {n}"));
        }
    }
    write_deprecated(w, &f.deprecated);
}
