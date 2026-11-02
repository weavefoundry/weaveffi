//! Callback interfaces: the Go `interface` a consumer implements, the
//! exported trampolines behind each vtable entry, and the cgo preamble
//! declaring them and the one static vtable per interface.
//!
//! An implementation crosses as the interface's `{p}_{Cb}` argument struct:
//! a handle-table context (see `wvNewCallback`) and the address of the
//! interface's static vtable, whose header records the vtable's `size` and
//! the `free` trampoline that deletes the handle once the native library is
//! done with it.

use crate::cabi::c_param_name;
use crate::codegen::CodeWriter;
use weaveffi_model::abi::{ctx_param, AbiParam, CType};
use weaveffi_model::model::{CallbackBinding, CallbackMethodBinding, ErrorStrategy, Model};

use crate::targets::go::calls::{completion_trampoline, vtable_accessor};
use crate::targets::go::codec::func;
use crate::targets::go::docs::GoDoc;
use crate::targets::go::names::{self, pascal};
use crate::targets::go::types::{cgo_type, go_type};
use crate::targets::go::values::{borrow, hand_over};
use crate::targets::go::Ctx;

/// The C name of the exported trampoline behind one vtable entry;
/// `free` names the header's release entry.
fn trampoline(c_tag: &str, method: &str) -> String {
    format!("goWv_{c_tag}_{method}")
}

/// The C identifier of the process-wide static vtable of `vtable_tag`.
fn vtable_var(vtable_tag: &str) -> String {
    format!("wvVtable_{vtable_tag}")
}

/// The preamble `extern` declaration of one exported trampoline, matching
/// the prototype cgo writes into `_cgo_export.h` from the Go signature
/// (every slot of a trampoline is const-free).
fn extern_decl(name: &str, ret: &CType, params: &[AbiParam], prefix: &str) -> String {
    let args: Vec<String> = params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), c_param_name(&p.name)))
        .collect();
    format!(
        "extern {} {name}({});",
        ret.render_c(prefix),
        args.join(", ")
    )
}

/// Write every declaration the cgo preamble needs beyond the header: for
/// each callback interface, the `extern` prototypes of its trampolines,
/// its static vtable, and the accessor returning the vtable's address; then
/// one `extern` per async completion trampoline.
///
/// A file that uses `//export` may only declare in its preamble (the
/// preamble is compiled into two C translation units), so the vtable is a
/// `static const`: each unit gets a private copy and no symbol is
/// duplicated. Go reaches the copy in its own unit through the accessor,
/// so the native library always sees one vtable that lives for the
/// process.
pub(crate) fn emit_preamble_decls(w: &mut CodeWriter, model: &Model) {
    let prefix = model.prefix();
    for cb in &model.callbacks {
        for m in &cb.methods {
            w.line(extern_decl(
                &trampoline(&cb.c_tag, &m.name),
                &m.abi.ret,
                &m.abi.params,
                prefix,
            ));
        }
        let free = trampoline(&cb.c_tag, "free");
        w.line(extern_decl(&free, &CType::Void, &[ctx_param()], prefix));
        let tag = &cb.vtable_tag;
        w.block(
            format!("static const {tag} {} = {{", vtable_var(tag)),
            "};",
            |w| {
                w.line(format!(".size = sizeof({tag}),"));
                // Go may run a callback on any thread, so the vtable isn't
                // thread-affine.
                w.line(".flags = 0,");
                w.line(format!(".free = {free},"));
                for m in &cb.methods {
                    w.line(format!(
                        ".{} = {},",
                        c_param_name(&m.name),
                        trampoline(&cb.c_tag, &m.name)
                    ));
                }
            },
        );
        w.line(format!(
            "__attribute__((unused)) static const {tag}* {}(void) {{ return &{}; }}",
            vtable_accessor(tag),
            vtable_var(tag)
        ));
    }
    for f in model.callables() {
        if let Some(ab) = f.async_binding() {
            w.line(extern_decl(
                &completion_trampoline(&ab.callback_type),
                &CType::Void,
                &ab.callback.params,
                prefix,
            ));
        }
    }
}

/// The Go method signature of one callback method: its parameters, and its
/// result, as `(T, error)` or `error` when it declares errors.
fn method_sig(m: &CallbackMethodBinding) -> String {
    let params: Vec<String> = m
        .params
        .iter()
        .map(|p| format!("{} {}", names::method_param(&p.name), go_type(&p.ty)))
        .collect();
    let ret = match (&m.ret, m.error.throws()) {
        (Some(ty), true) => format!(" ({}, error)", go_type(ty)),
        (Some(ty), false) => format!(" {}", go_type(ty)),
        (None, true) => " error".into(),
        (None, false) => String::new(),
    };
    format!("{}({}){ret}", pascal(&m.name), params.join(", "))
}

/// Render one callback interface: the Go `interface` the consumer
/// implements, the trampoline behind each vtable entry, and the `free`
/// trampoline.
pub(crate) fn render_callback_interface(w: &mut CodeWriter, ctx: &Ctx, cb: &CallbackBinding) {
    let name = pascal(&cb.name);
    let (doc, deprecated) = ctx.docs.of(&cb.doc, &cb.deprecated);
    GoDoc::decl(&name, doc)
        .para(&format!(
            "Implement {name} in Go and pass the value to the functions that take it. \
             The native library may call its methods from any thread until it \
             releases the implementation. A method that panics fails the native call \
             in progress as a callback failure (code -4)."
        ))
        .deprecated(deprecated)
        .emit(w);
    w.block(format!("type {name} interface {{"), "}", |w| {
        for m in &cb.methods {
            let (doc, deprecated) = ctx.docs.of(&m.doc, &m.deprecated);
            let params = m.params.iter().filter_map(|p| {
                let (doc, _) = ctx.docs.of(&p.doc, &None);
                doc.map(|d| (names::method_param(&p.name), d))
            });
            let mut doc = GoDoc::plain(doc).params(params);
            match &m.error {
                ErrorStrategy::Domain(d) => {
                    doc = doc.para(&format!(
                        "Return a {} to report one of its codes, with its fields, to \
                         the native caller; any other error reports code -1 with its \
                         text.",
                        ctx.names.domain(d).iface
                    ));
                }
                ErrorStrategy::Untyped => {
                    doc = doc.para(
                        "A returned error reaches the native caller as code -1 with its text.",
                    );
                }
                ErrorStrategy::Trap => {}
            }
            doc.deprecated(deprecated).emit(w);
            w.line(method_sig(m));
        }
    });
    w.blank();

    for m in &cb.methods {
        render_trampoline(w, ctx, cb, m, &name);
    }

    let free = trampoline(&cb.c_tag, "free");
    w.line(format!("//export {free}"));
    func(
        w,
        &format!("func {free}(ctx unsafe.Pointer)"),
        "wvFreeCallback(ctx)",
    );
}

/// Render the exported trampoline behind one vtable entry. It recovers the
/// implementation from the context, receives the arguments (runs are lent
/// for the call and copied or decoded; an object moves one strong
/// reference into a new wrapper), calls the Go method, and hands the result
/// back as the C return (a run from the library's allocator, or a fresh
/// object reference, which the library adopts). A returned error is
/// reported through `out_err`: a code of the method's domain with its
/// payload, or code -1 with its text; a panic is recovered into code -4.
/// Nothing unwinds through the C frame.
fn render_trampoline(
    w: &mut CodeWriter,
    ctx: &Ctx,
    cb: &CallbackBinding,
    m: &CallbackMethodBinding,
    iface: &str,
) {
    let tramp = trampoline(&cb.c_tag, &m.name);
    let params = &m.abi.params;
    let formals: Vec<String> = params
        .iter()
        .map(|s| format!("{} {}", names::slot(&s.name), cgo_type(&s.ty, ctx.prefix)))
        .collect();
    let first = names::slot(&params[0].name);
    let out_err = names::slot(&params[params.len() - 1].name);
    let ret_sig = match &m.ret {
        Some(_) => format!(" (cRet {})", cgo_type(&m.abi.ret, ctx.prefix)),
        None => String::new(),
    };
    // The method's own parameters sit between `ctx` and `out_err`.
    let args: Vec<String> = m
        .params
        .iter()
        .zip(&params[1..])
        .map(|(p, slot)| borrow(&p.ty, &names::slot(&slot.name)))
        .collect();
    let call = format!(
        "wvCallback[{iface}]({first}).{}({})",
        pascal(&m.name),
        args.join(", ")
    );
    let failed = match &m.error {
        ErrorStrategy::Domain(d) => format!(
            "wvCallbackFailed[{}]({out_err}, err)",
            ctx.names.domain(d).iface
        ),
        ErrorStrategy::Untyped | ErrorStrategy::Trap => {
            format!("wvCallbackError({out_err}, err)")
        }
    };

    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}){ret_sig} {{", formals.join(", ")),
        "}",
        |w| {
            w.line(format!("defer wvRecoverCallback({out_err})"));
            let Some(ty) = &m.ret else {
                if m.error.throws() {
                    w.block(format!("if err := {call}; err != nil {{"), "}", |w| {
                        w.line(failed);
                    });
                } else {
                    w.line(call);
                }
                return;
            };
            if m.error.throws() {
                w.line(format!("ret, err := {call}"));
                w.block("if err != nil {", "}", |w| {
                    w.line(failed);
                    w.line("return");
                });
            } else {
                w.line(format!("ret := {call}"));
            }
            w.line(format!("return {}", hand_over(ctx, ty, "ret")));
        },
    );
    w.blank();
}
