//! Doc-comment emission: `///` comments carried from the IDL, plus the
//! generated notes (streaming contract, thrown exception, deprecation) a
//! wrapper's declaration carries.

use weaveffi_core::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{CallShape, FnBinding};

use crate::calls::ErrCtx;

/// Append `doc` as a `///` comment block at `indent`, or nothing when absent.
pub(crate) fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::TripleSlash);
}

/// Emit a wrapper's doc comment, the streaming/disposal note for an iterator
/// callable, the typed-exception note for a throwing callable, and its
/// `@Deprecated` annotation when present.
pub(crate) fn emit_wrapper_doc(w: &mut CodeWriter, f: &FnBinding, err: ErrCtx) {
    {
        let mut d = String::new();
        emit_doc(&mut d, &f.doc, "");
        w.raw(d);
    }
    let mut has_content = f.doc.is_some();
    let separator = |w: &mut CodeWriter, has_content: &mut bool| {
        if *has_content {
            w.line("///");
        }
        *has_content = true;
    };
    if let CallShape::Iterator(ib) = &f.shape {
        separator(w, &mut has_content);
        w.line("/// Returns a lazy [Iterable]: elements are pulled from the native");
        w.line("/// iterator one at a time (one native `next` call per element), and");
        w.line("/// iterating the result again launches a fresh native iterator.");
        w.line("///");
        w.line("/// The native iterator handle is destroyed exactly once: eagerly when");
        w.line("/// the iteration completes or fails, or by a GC finalizer if the");
        w.line("/// iteration is abandoned before it is exhausted.");
        if ib.elem.interface_name().is_some() {
            w.line("///");
            w.line("/// Each yielded element is owned by the caller: call its `dispose()`");
            w.line("/// when you are done with it.");
        }
    } else if let Some(ret) = f.ret.as_ref().filter(|r| r.interface_name().is_some()) {
        separator(w, &mut has_content);
        if matches!(ret, Ty::Optional(_)) {
            w.line("/// Returns `null` when the producer reports no object. A non-null result");
            w.line("/// is owned by the caller: call its `dispose()` when you are done with it.");
        } else {
            w.line("/// The returned object is owned by the caller: call its `dispose()` when");
            w.line("/// you are done with it.");
        }
    }
    if let Some(exc) = err.thrown_exception() {
        separator(w, &mut has_content);
        w.line(format!("/// Throws [{exc}] on domain errors."));
    }
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('\'', "\\'");
        w.line(format!("@Deprecated('{escaped}')"));
    }
}
