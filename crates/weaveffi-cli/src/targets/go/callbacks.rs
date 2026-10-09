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
use weaveffi_model::model::{CallShape, CallbackInterfaceBinding, CallbackMethodBinding, Model};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, RetPass};

use crate::targets::go::calls::{completion_trampoline, vtable_accessor};
use crate::targets::go::codec::{func, read_fn, write_fn};
use crate::targets::go::docs::{GoDoc, Kind};
use crate::targets::go::names::{self, domain_type, pascal};
use crate::targets::go::types::{cgo_type, from_c_direct, go_type, strip_const, to_c_direct};
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
                &m.abi_ret,
                &m.abi_params,
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
                w.line(".flags = 0,");
                w.line(format!(".free = {free},"));
                for m in &cb.methods {
                    let tramp = trampoline(&cb.c_tag, &m.name);
                    // An entry whose C signature has `const` pointers is
                    // cast to the field's exact type, since the exported
                    // Go function is declared const-free.
                    if m.abi_params.iter().any(|p| strip_const(&p.ty) != p.ty) {
                        let types: Vec<String> =
                            m.abi_params.iter().map(|p| p.ty.render_c(prefix)).collect();
                        w.line(format!(
                            ".{} = ({} (*)({})){tramp},",
                            c_param_name(&m.name),
                            m.abi_ret.render_c(prefix),
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
/// result, as `(T, error)` or `error` when it throws.
fn method_sig(m: &CallbackMethodBinding) -> String {
    let params: Vec<String> = m
        .params
        .iter()
        .map(|p| format!("{} {}", names::method_param(&p.name), go_type(&p.ty)))
        .collect();
    let ret = match (&m.ret, m.throws) {
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
    let domain = ctx.domain.map(domain_type);
    GoDoc::new(&name, cb.doc.as_deref(), Kind::Value, None)
        .para(&format!(
            "Implement {name} in Go and pass the value to the functions that take it. \
             The native library may call its methods from any thread until it \
             releases the implementation. A method that panics, or a throwing method \
             that returns an error outside its domain, fails the native call in \
             progress as a callback failure (code -4)."
        ))
        .deprecated(cb.deprecated.as_deref())
        .emit(w);
    w.block(format!("type {name} interface {{"), "}", |w| {
        for m in &cb.methods {
            let kind = Kind::callable(m.ret.is_some());
            let mut doc = GoDoc::new(&pascal(&m.name), m.doc.as_deref(), kind, None)
                .params(&m.params, names::method_param);
            if m.throws {
                let line = match &domain {
                    Some(d) => format!(
                        "Return a {d} to report that code, with its fields, to the native caller."
                    ),
                    None => "A returned error reaches the native caller as code -4.".into(),
                };
                doc = doc.para(&line);
            }
            doc.deprecated(m.deprecated.as_deref()).emit(w);
            w.line(method_sig(m));
        }
    });
    w.blank();

    for m in &cb.methods {
        render_trampoline(w, ctx, cb, m, &name, domain.as_deref());
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
/// Go value the implementation receives: strings, bytes, and buffers are
/// borrowed for the call, so they're copied or decoded; an object transfers
/// one strong reference, adopted into a new wrapper.
fn argument(p: &weaveffi_model::model::ParamBinding) -> String {
    match p.arg_pass() {
        ArgPass::Buffer { ptr, len } => format!(
            "wvDecodeBorrowed({}, {}, {})",
            names::slot(&ptr.name),
            names::slot(&len.name),
            read_fn(&p.ty)
        ),
        ArgPass::String { ptr, len } => format!(
            "wvBorrowString({}, {})",
            names::slot(&ptr.name),
            names::slot(&len.name)
        ),
        ArgPass::Bytes { ptr, len } => format!(
            "wvBorrowBytes({}, {})",
            names::slot(&ptr.name),
            names::slot(&len.name)
        ),
        ArgPass::Object { slot, .. } => {
            let n = p.ty.interface_name().expect("an object names an interface");
            format!("wvAdopt{}({})", pascal(n), names::slot(&slot.name))
        }
        ArgPass::Direct { slot } => from_c_direct(&names::slot(&slot.name), &p.ty),
        ArgPass::Callback { .. } => {
            unreachable!("validation rejects callback interfaces as callback-method parameters")
        }
    }
}

/// Render the exported trampoline behind one vtable entry. It recovers the
/// implementation from the context, converts the arguments, calls the Go
/// method, and hands the result back: a direct value as the C return, an
/// object as a fresh strong reference the native library adopts (nil stays
/// null), and a string, bytes, or buffer as a run allocated with the
/// library's allocator in the out slots. A throwing method's error is
/// reported through `out_err` with its domain code and payload (code -4
/// for any other error), and a panic is recovered into code -4; nothing
/// unwinds through the C frame.
fn render_trampoline(
    w: &mut CodeWriter,
    ctx: &Ctx,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    iface: &str,
    domain: Option<&str>,
) {
    let tramp = trampoline(&cb.c_tag, &m.name);
    let formals: Vec<String> = m
        .abi_params
        .iter()
        .map(|s| format!("{} {}", names::slot(&s.name), cgo_type(&s.ty, ctx.prefix)))
        .collect();
    let slot_named = |n: &str| {
        m.abi_params
            .iter()
            .find(|p| p.name == n)
            .map(|p| names::slot(&p.name))
    };
    let first = names::slot(&m.abi_params[0].name);
    let out_err = names::slot(&m.abi_params[m.abi_params.len() - 1].name);
    let direct_ret = m.abi_ret != CType::Void;
    let ret_sig = if direct_ret {
        format!(" (cRet {})", cgo_type(&m.abi_ret, ctx.prefix))
    } else {
        String::new()
    };
    let pass = RetPass::of(m.ret.as_ref());
    let throws = m.error_strategy() == ErrorStrategy::Throws;
    let args: Vec<String> = m.params.iter().map(argument).collect();
    let call = format!(
        "wvCallback[{iface}]({first}).{}({})",
        pascal(&m.name),
        args.join(", ")
    );

    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}){ret_sig} {{", formals.join(", ")),
        "}",
        |w| {
            w.line(format!("defer wvRecoverCallback({out_err})"));
            let failed = format!(
                "wvCallbackFailed[{}]({out_err}, err)",
                domain.unwrap_or("error")
            );
            match (&m.ret, throws) {
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
            let ty = m.ret.as_ref().expect("a returning method");
            match pass {
                RetPass::Direct => {
                    w.line(format!(
                        "return {}",
                        to_c_direct("ret", &m.abi_ret, ctx.prefix)
                    ));
                }
                RetPass::Object { .. } => {
                    w.line("return ret.share()");
                }
                RetPass::String | RetPass::Bytes | RetPass::Buffer => {
                    let out_ptr = slot_named("out_ptr").expect("a run return has out_ptr");
                    let out_len = slot_named("out_len").expect("a run return has out_len");
                    let hand = match pass {
                        RetPass::String => format!("wvHandOverString(ret, {out_ptr}, {out_len})"),
                        RetPass::Bytes => format!("wvHandOverBytes(ret, {out_ptr}, {out_len})"),
                        _ => format!(
                            "wvHandOverBytes(wvEncode(ret, {}), {out_ptr}, {out_len})",
                            write_fn(ty)
                        ),
                    };
                    w.line(hand);
                    if direct_ret {
                        w.line("return");
                    }
                }
                RetPass::Void => unreachable!("a returning method"),
            }
        },
    );
    w.blank();
}
