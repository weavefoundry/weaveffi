//! Callback interfaces: the Go `interface` a consumer implements, the
//! exported trampolines behind each vtable entry, and the cgo preamble
//! declaring them and the one static vtable per interface.
//!
//! An implementation crosses as a handle-table context (see
//! `wvNewCallback`) plus the address of its interface's static vtable,
//! whose header records the vtable's `size` and the `free` trampoline that
//! deletes the handle once the native library is done with it.

use crate::cabi::c_param_name;
use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    CallShape, CallbackInterfaceBinding, CallbackMethodBinding, CallbackParamBinding, Model,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};
use weaveffi_model::ty::Ty;

use crate::targets::go::calls::{completion_trampoline, vtable_accessor};
use crate::targets::go::codec::{func, read_fn, write_fn};
use crate::targets::go::docs::GoDoc;
use crate::targets::go::names::{self, pascal};
use crate::targets::go::types::{cgo_pointee, cgo_type, go_type, prim_type, strip_const};
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

/// The preamble `extern` declaration of one exported trampoline. Pointer
/// types are const-free to match the prototypes cgo writes into
/// `_cgo_export.h` from the Go signature.
fn extern_decl(name: &str, ret: &CType, params: &[AbiParam], prefix: &str) -> String {
    let args: Vec<String> = params
        .iter()
        .map(|p| {
            format!(
                "{} {}",
                strip_const(&p.ty).render_c(prefix),
                c_param_name(&p.name)
            )
        })
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
    for (_, cb) in model.callback_interfaces() {
        for m in &cb.methods {
            w.line(extern_decl(
                &trampoline(&cb.c_tag, &m.name),
                &m.abi.ret,
                &m.abi.params,
                prefix,
            ));
        }
        let free = trampoline(&cb.c_tag, "free");
        w.line(extern_decl(
            &free,
            &CType::Void,
            &[AbiParam::new("ctx", CType::ptr(CType::Void))],
            prefix,
        ));
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
                    let tramp = trampoline(&cb.c_tag, &m.name);
                    // An entry whose C signature has `const` pointers is
                    // cast to the field's exact type, since the exported
                    // Go function is declared const-free.
                    if m.abi.params.iter().any(|p| strip_const(&p.ty) != p.ty) {
                        let types: Vec<String> =
                            m.abi.params.iter().map(|p| p.ty.render_c(prefix)).collect();
                        w.line(format!(
                            ".{} = ({} (*)({})){tramp},",
                            c_param_name(&m.name),
                            m.abi.ret.render_c(prefix),
                            types.join(", ")
                        ));
                    } else {
                        w.line(format!(".{} = {tramp},", c_param_name(&m.name)));
                    }
                }
            },
        );
        w.line(format!(
            "__attribute__((unused)) static const {tag}* {}(void) {{ return &{}; }}",
            vtable_accessor(tag),
            vtable_var(tag)
        ));
    }
    for (_, f) in model.callables() {
        if let CallShape::Async(ab) = &f.shape {
            w.line(extern_decl(
                &completion_trampoline(&ab.callback_type),
                &CType::Void,
                &ab.callback_params,
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
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    ctx: &Ctx,
    cb: &CallbackInterfaceBinding,
) {
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

/// The Go expression converting one callback argument's C slots into the
/// Go value the implementation receives: strings, bytes, typed arrays, and
/// buffers are borrowed for the call, so they're copied or decoded; an
/// optional scalar is joined from its flag and value; an object transfers
/// one strong reference, adopted into a new wrapper.
fn argument(p: &CallbackParamBinding) -> String {
    let s = |slot: &weaveffi_model::abi::AbiParam| names::slot(&slot.name);
    match &p.pass {
        ArgPass::Direct { slot } => format!("{}({})", go_type(&p.ty), s(slot)),
        ArgPass::OptDirect { has, value, inner } => {
            format!(
                "wvOptional(bool({}), {}({}))",
                s(has),
                go_type(inner),
                s(value)
            )
        }
        ArgPass::Slice { ptr, len, elem } => {
            format!(
                "wvBorrowSlice[{}]({}, {})",
                prim_type(*elem),
                s(ptr),
                s(len)
            )
        }
        ArgPass::String { ptr, len } => format!("wvBorrowString({}, {})", s(ptr), s(len)),
        ArgPass::Bytes { ptr, len } => format!("wvBorrowBytes({}, {})", s(ptr), s(len)),
        ArgPass::Buffer { ptr, len } => format!(
            "wvDecodeBorrowed({}, {}, {})",
            s(ptr),
            s(len),
            read_fn(&p.ty)
        ),
        ArgPass::Object {
            slot, interface, ..
        } => format!("wvAdopt{}({})", pascal(interface), s(slot)),
        ArgPass::Callback { .. } => {
            unreachable!("a callback method parameter is never a callback")
        }
    }
}

/// Render the exported trampoline behind one vtable entry. It recovers the
/// implementation from the context, converts the arguments, calls the Go
/// method, and hands the result back: a direct value as the C return, an
/// optional scalar as the C return (present) plus its out slot, an object
/// as a fresh strong reference the native library adopts (nil stays null),
/// and a string, bytes, typed array, or buffer as a run allocated with the
/// library's allocator in the out slots. A returned error is reported
/// through `out_err`: a code of the method's domain with its payload, or
/// code -1 with its text; a panic is recovered into code -4. Nothing
/// unwinds through the C frame.
fn render_trampoline(
    w: &mut CodeWriter,
    ctx: &Ctx,
    cb: &CallbackInterfaceBinding,
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
    let direct_ret = m.abi.ret != CType::Void;
    let ret_sig = if direct_ret {
        format!(" (cRet {})", cgo_type(&m.abi.ret, ctx.prefix))
    } else {
        String::new()
    };
    let args: Vec<String> = m.params.iter().map(argument).collect();
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
            match (&m.ret, m.error.throws()) {
                (None, false) => {
                    w.line(call);
                    return;
                }
                (None, true) => {
                    w.block(format!("if err := {call}; err != nil {{"), "}", |w| {
                        w.line(failed);
                    });
                    return;
                }
                (Some(_), false) => {
                    w.line(format!("ret := {call}"));
                }
                (Some(_), true) => {
                    w.line(format!("ret, err := {call}"));
                    w.block("if err != nil {", "}", |w| {
                        w.line(failed);
                        w.line("return");
                    });
                }
            }
            let ty: &Ty = m.ret.as_ref().expect("a returning method");
            let s = |slot: &weaveffi_model::abi::AbiParam| names::slot(&slot.name);
            match &m.ret_pass {
                CallbackRetPass::Direct => {
                    w.line(format!("return {}(ret)", cgo_type(&m.abi.ret, ctx.prefix)));
                }
                CallbackRetPass::OptDirect { out_value } => {
                    w.line("cPresent, cValue := wvPresent(ret)");
                    w.line(format!(
                        "*{} = {}(cValue)",
                        s(out_value),
                        cgo_pointee(&out_value.ty, ctx.prefix)
                    ));
                    w.line(format!(
                        "return {}(cPresent)",
                        cgo_type(&m.abi.ret, ctx.prefix)
                    ));
                }
                CallbackRetPass::Object { .. } => {
                    w.line("return ret.share()");
                }
                CallbackRetPass::Slice {
                    out_ptr, out_len, ..
                } => {
                    w.line(format!(
                        "wvHandOverSlice(ret, {}, {})",
                        s(out_ptr),
                        s(out_len)
                    ));
                }
                CallbackRetPass::String { out_ptr, out_len } => {
                    w.line(format!(
                        "wvHandOverString(ret, {}, {})",
                        s(out_ptr),
                        s(out_len)
                    ));
                }
                CallbackRetPass::Bytes { out_ptr, out_len } => {
                    w.line(format!(
                        "wvHandOverBytes(ret, {}, {})",
                        s(out_ptr),
                        s(out_len)
                    ));
                }
                CallbackRetPass::Buffer { out_ptr, out_len } => {
                    w.line(format!(
                        "wvHandOverBytes(wvEncode(ret, {}), {}, {})",
                        write_fn(ty),
                        s(out_ptr),
                        s(out_len)
                    ));
                }
                CallbackRetPass::Void => unreachable!("a returning method"),
            }
        },
    );
    w.blank();
}
