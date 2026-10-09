//! Call rendering: sync, async, and iterator wrappers and the argument and
//! result marshalling they share.
//!
//! Marshalling follows the shared plan: each parameter's passing contract
//! comes from [`ParamBinding::arg_pass`] and each result's receiving
//! contract from [`RetPass::of`], so this module only spells those
//! contracts in Go.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::CType;
use weaveffi_model::model::{AsyncBinding, CallShape, FnBinding, IteratorBinding, ParamBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, RetPass};
use weaveffi_model::ty::Ty;

use crate::targets::go::codec::{read_fn, write_fn};
use crate::targets::go::docs::{GoDoc, Kind};
use crate::targets::go::entities::domain_mapper;
use crate::targets::go::names::{self, pascal};
use crate::targets::go::types::{
    cgo_type, from_c_direct, go_type, go_zero, strip_const, to_c_direct,
};
use crate::targets::go::Ctx;

/// The wrapper type a method is declared on.
#[derive(Clone, Copy)]
pub(crate) struct Receiver<'a> {
    /// The Go type name of the interface wrapper (`Store`).
    pub(crate) ty: &'a str,
}

// ── Errors ──

/// How a wrapper reports a non-zero error slot: a throwing callable returns
/// `(T, error)` with the failure mapped onto its domain's code types; any
/// other callable panics with an `*Error`, since its failure is a bug.
struct ErrCtx {
    throws: bool,
    /// The domain's mapping helper (`wvKvError`), or `None` for the generic
    /// `*Error` when no domain is in scope.
    mapper: Option<String>,
}

impl ErrCtx {
    fn of(ctx: &Ctx, f: &FnBinding) -> Self {
        Self {
            throws: f.error_strategy() == ErrorStrategy::Throws,
            mapper: ctx.domain.map(domain_mapper),
        }
    }

    /// The Go expression converting the `wvFailure` value `failure` into an
    /// `error`.
    fn map(&self, failure: &str) -> String {
        match &self.mapper {
            Some(m) => format!("{m}({failure})"),
            None => format!("{failure}.err()"),
        }
    }

    /// The return type (with its leading space) of a sync wrapper.
    fn ret_sig(&self, ret: Option<&Ty>) -> String {
        match (ret, self.throws) {
            (Some(r), true) => format!(" ({}, error)", go_type(r)),
            (Some(r), false) => format!(" {}", go_type(r)),
            (None, true) => " error".into(),
            (None, false) => String::new(),
        }
    }
}

// ── Shared pieces ──

/// The wrapper's signature line.
fn header(recv: Option<Receiver>, go_name: &str, params: &[String], ret_sig: &str) -> String {
    let params = params.join(", ");
    match recv {
        Some(r) => format!("func (s *{}) {go_name}({params}){ret_sig} {{", r.ty),
        None => format!("func {go_name}({params}){ret_sig} {{"),
    }
}

/// The wrapper's Go parameter list.
fn go_params(f: &FnBinding) -> Vec<String> {
    let is_async = f.is_async();
    let mut params = Vec::new();
    if is_async {
        params.push("ctx context.Context".to_string());
    }
    params.extend(
        f.params
            .iter()
            .map(|p| format!("{} {}", names::param(&p.name, f), go_type(&p.ty))),
    );
    params
}

/// The receiver borrow every method opens with: `s.native()` panics when
/// the wrapper was already closed, and the deferred release keeps the
/// object alive (even across a concurrent `Close`) until the call returns.
fn emit_receiver(w: &mut CodeWriter, args: &mut Vec<String>) {
    w.line("cSelf := s.native()");
    w.line("defer s.ref.release()");
    args.push("cSelf".into());
}

/// Emit the staging statements for one parameter and push its C argument
/// expressions, per its [`ArgPass`].
///
/// Strings and bytes pass a borrowed view of the Go memory (cgo pins it
/// for the call); a buffered value is encoded first. An object parameter
/// borrows the wrapper's pointer for the call, like a receiver, and a nil
/// `Interface?` passes null. A callback implementation is kept in a handle
/// table, passed as the context, beside the interface's static vtable; a
/// nil `Cb?` passes a null vtable.
fn emit_param(
    w: &mut CodeWriter,
    ctx: &Ctx,
    args: &mut Vec<String>,
    p: &ParamBinding,
    f: &FnBinding,
) {
    let name = names::param(&p.name, f);
    let c = format!("c{}", pascal(&p.name));
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            w.line(format!(
                "{c}Ptr, {c}Len := wvBytes(wvEncode({name}, {}))",
                write_fn(&p.ty)
            ));
            args.extend([format!("{c}Ptr"), format!("{c}Len")]);
        }
        ArgPass::String { .. } => {
            w.line(format!("{c}Ptr, {c}Len := wvStr({name})"));
            args.extend([format!("{c}Ptr"), format!("{c}Len")]);
        }
        ArgPass::Bytes { .. } => {
            w.line(format!("{c}Ptr, {c}Len := wvBytes({name})"));
            args.extend([format!("{c}Ptr"), format!("{c}Len")]);
        }
        ArgPass::Object { slot, nullable } => {
            if nullable {
                w.line(format!(
                    "var {c} {}",
                    cgo_type(&strip_const(&slot.ty), ctx.prefix)
                ));
                w.block(format!("if {name} != nil {{"), "}", |w| {
                    w.line(format!("{c} = {name}.native()"));
                    w.line(format!("defer {name}.ref.release()"));
                });
            } else {
                w.line(format!("{c} := {name}.native()"));
                w.line(format!("defer {name}.ref.release()"));
            }
            args.push(c);
        }
        ArgPass::Callback {
            vtable, nullable, ..
        } => {
            let CType::Ptr { pointee, .. } = &vtable.ty else {
                unreachable!("a callback vtable slot is a pointer")
            };
            let accessor = format!("C.{}()", vtable_accessor(&pointee.render_c(ctx.prefix)));
            if nullable {
                w.line(format!("var {c}Ctx unsafe.Pointer"));
                w.line(format!(
                    "var {c}Vtable {}",
                    cgo_type(&strip_const(&vtable.ty), ctx.prefix)
                ));
                w.block(format!("if {name} != nil {{"), "}", |w| {
                    w.line(format!(
                        "{c}Ctx, {c}Vtable = wvNewCallback({name}), {accessor}"
                    ));
                });
                args.extend([format!("{c}Ctx"), format!("{c}Vtable")]);
            } else {
                args.extend([format!("wvNewCallback({name})"), accessor]);
            }
        }
        ArgPass::Direct { slot } => args.push(to_c_direct(&name, &slot.ty, ctx.prefix)),
    }
}

/// The Go expression receiving one value the native library hands over,
/// per its [`RetPass`]: a direct value converted, a string or bytes copied
/// and released, a buffer decoded and released, an object adopted (a null
/// `Interface?` adopts to nil). `len` names the length slot of a run.
fn receive(ty: &Ty, pass: &RetPass, ptr: &str, len: &str) -> String {
    match pass {
        RetPass::Void => unreachable!("a present value is never void"),
        RetPass::Direct => from_c_direct(ptr, ty),
        RetPass::String => format!("wvTakeString({ptr}, {len})"),
        RetPass::Bytes => format!("wvTakeBytes({ptr}, {len})"),
        RetPass::Buffer => format!("wvDecode({ptr}, {len}, {})", read_fn(ty)),
        RetPass::Object { .. } => {
            let n = ty
                .interface_name()
                .expect("an object result names an interface");
            format!("wvAdopt{}({ptr})", pascal(n))
        }
    }
}

/// `true` when a value received per `pass` arrives as a `(ptr, len)` run.
fn has_len(pass: &RetPass) -> bool {
    matches!(pass, RetPass::String | RetPass::Bytes | RetPass::Buffer)
}

/// The doc comment of a wrapper: its IDL doc and documented parameters,
/// then `extra` paragraphs, then `Deprecated:`.
fn wrapper_doc(f: &FnBinding, go_name: &str, extra: &[&str]) -> GoDoc {
    let kind = Kind::callable(f.ret.is_some());
    let mut doc =
        GoDoc::new(go_name, f.doc.as_deref(), kind, None).params(&f.params, |n| names::param(n, f));
    for p in extra {
        doc = doc.para(p);
    }
    doc.deprecated(f.deprecated.as_deref())
}

// ── Sync ──

/// Render a sync or iterator callable: the Go wrapper marshalling the
/// parameters, calling the C symbol, checking the error slot (a typed
/// `(T, error)` when throwing, a panic otherwise), and receiving the
/// result. With `recv` set, it's a method passing the wrapper's pointer
/// first.
pub(crate) fn render_sync(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    if let CallShape::Iterator(ib) = &f.shape {
        render_iterator(w, ctx, f, ib, go_name, recv);
        return;
    }
    let err = ErrCtx::of(ctx, f);
    let pass = RetPass::of(f.ret.as_ref());
    wrapper_doc(f, go_name, &[]).emit(w);
    w.block(
        header(recv, go_name, &go_params(f), &err.ret_sig(f.ret.as_ref())),
        "}",
        |w| {
            let mut args = Vec::new();
            if recv.is_some() {
                emit_receiver(w, &mut args);
            }
            for p in &f.params {
                emit_param(w, ctx, &mut args, p, f);
            }
            if has_len(&pass) {
                w.line("var cRetLen C.size_t");
                args.push("&cRetLen".into());
            }
            w.line(format!("var cErr C.{}_error", ctx.prefix));
            args.push("&cErr".into());
            let call = format!("C.{}({})", f.c_base, args.join(", "));
            match &f.ret {
                Some(_) => w.line(format!("cRet := {call}")),
                None => w.line(call),
            };
            let ok = if err.throws { ", nil" } else { "" };
            if err.throws {
                let mapped = err.map("wvTakeError(&cErr)");
                w.block("if cErr.code != 0 {", "}", |w| {
                    match &f.ret {
                        Some(ty) => w.line(format!("return {}, {mapped}", go_zero(ty))),
                        None => w.line(format!("return {mapped}")),
                    };
                });
            } else {
                w.line("wvTrap(&cErr)");
            }
            match &f.ret {
                Some(ty) => {
                    w.line(format!(
                        "return {}{ok}",
                        receive(ty, &pass, "cRet", "cRetLen")
                    ));
                }
                None if err.throws => {
                    w.line("return nil");
                }
                None => {}
            }
        },
    );
    w.blank();
}

/// Render an `iter<T>` callable as Go's lazy sequence idiom: `iter.Seq[T]`,
/// or `iter.Seq2[T, error]` when it throws, where a launch or `next`
/// failure is yielded as a final `(zero, err)` pair. The native iterator is
/// launched on each range, `next` runs once per element, and `destroy` runs
/// exactly once when the range ends, early or not.
fn render_iterator(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    ib: &IteratorBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    let err = ErrCtx::of(ctx, f);
    let proto = ib.protocol(f);
    let elem = &ib.elem;
    let elem_go = go_type(elem);
    let CType::Ptr { pointee, .. } = &ib.next.params[1].ty else {
        unreachable!("an iterator's item slot is a pointer")
    };
    let item_ty = cgo_type(&strip_const(pointee), ctx.prefix);
    let with_len = has_len(&proto.elem);
    let (seq, yield_ty) = if err.throws {
        (
            format!("iter.Seq2[{elem_go}, error]"),
            format!("func({elem_go}, error) bool"),
        )
    } else {
        (
            format!("iter.Seq[{elem_go}]"),
            format!("func({elem_go}) bool"),
        )
    };

    let notes = if err.throws {
        "The sequence launches the native iterator each time it's ranged over and \
         pulls one element per step, yielding a failure as a final (zero value, \
         error) pair; the iterator is released when the range ends, early or not."
    } else {
        "The sequence launches the native iterator each time it's ranged over and \
         pulls one element per step; the iterator is released when the range ends, \
         early or not."
    };
    wrapper_doc(f, go_name, &[notes]).emit(w);

    // Surfaces a non-zero error slot: a throwing sequence yields the mapped
    // error and stops; any other traps.
    let check = |w: &mut CodeWriter, slot: &str| {
        if err.throws {
            let mapped = err.map(&format!("wvTakeError(&{slot})"));
            w.block(format!("if {slot}.code != 0 {{"), "}", |w| {
                w.line(format!("yield({}, {mapped})", go_zero(elem)));
                w.line("return");
            });
        } else {
            w.line(format!("wvTrap(&{slot})"));
        }
    };

    w.block(
        header(recv, go_name, &go_params(f), &format!(" {seq}")),
        "}",
        |w| {
            w.block(format!("return func(yield {yield_ty}) {{"), "}", |w| {
                let mut args = Vec::new();
                if recv.is_some() {
                    emit_receiver(w, &mut args);
                }
                for p in &f.params {
                    emit_param(w, ctx, &mut args, p, f);
                }
                w.line(format!("var cErr C.{}_error", ctx.prefix));
                args.push("&cErr".into());
                w.line(format!(
                    "cIter := C.{}({})",
                    ib.launch.symbol,
                    args.join(", ")
                ));
                check(w, "cErr");
                w.line(format!("defer C.{}(cIter)", ib.destroy_symbol));
                w.block("for {", "}", |w| {
                    w.line(format!("var cItem {item_ty}"));
                    let next_args = if with_len {
                        w.line("var cItemLen C.size_t");
                        "cIter, &cItem, &cItemLen, &cIterErr"
                    } else {
                        "cIter, &cItem, &cIterErr"
                    };
                    w.line(format!("var cIterErr C.{}_error", ctx.prefix));
                    w.line(format!("more := C.{}({next_args}) != 0", ib.next.symbol));
                    check(w, "cIterErr");
                    w.block("if !more {", "}", |w| {
                        w.line("return");
                    });
                    let item = receive(elem, &proto.elem, "cItem", "cItemLen");
                    let call = if err.throws {
                        format!("if !yield({item}, nil) {{")
                    } else {
                        format!("if !yield({item}) {{")
                    };
                    w.block(call, "}", |w| {
                        w.line("return");
                    });
                });
            });
        },
    );
    w.blank();
}

// ── Async ──

/// The C name of the exported Go trampoline behind an async completion
/// typedef.
pub(crate) fn completion_trampoline(callback_type: &str) -> String {
    format!("goWv_{callback_type}")
}

/// The C identifier of the static preamble function returning the address
/// of a callback interface's one vtable (Go can't take the address of a
/// `static` C variable through cgo, so wrappers call this instead).
pub(crate) fn vtable_accessor(vtable_tag: &str) -> String {
    format!("wvVtablePtr_{vtable_tag}")
}

/// Render an async callable: the exported completion trampoline, which
/// converts the result and delivers it on the call's channel, and a
/// blocking wrapper taking a leading `context.Context`.
///
/// Every async wrapper returns an `error`. A cancellable one creates a
/// native cancel token, cancels it when `ctx` is done, and returns
/// `ctx.Err()` for the cancelled completion; any other returns `ctx.Err()`
/// as soon as `ctx` is done and abandons the result. A failure maps onto
/// the domain when the function throws and panics with an `*Error`
/// otherwise.
pub(crate) fn render_async(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    ab: &AsyncBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    let err = ErrCtx::of(ctx, f);
    let proto = ab.protocol(f);
    let tramp = completion_trampoline(&ab.callback_type);
    let val_ty = f
        .ret
        .as_ref()
        .map_or_else(|| "struct{}".to_string(), go_type);

    let formals: Vec<String> = ab
        .callback_params
        .iter()
        .map(|s| format!("{} {}", names::slot(&s.name), cgo_type(&s.ty, ctx.prefix)))
        .collect();
    let context = names::slot(&ab.callback_params[0].name);
    let err_slot = names::slot(&ab.callback_params[1].name);
    let results: Vec<String> = ab.callback_params[2..]
        .iter()
        .map(|s| names::slot(&s.name))
        .collect();
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
            let value = match &f.ret {
                None => "struct{}{}".to_string(),
                Some(ty) => {
                    let len = results.get(1).map_or("", String::as_str);
                    receive(ty, &proto.result, &results[0], len)
                }
            };
            w.block(
                format!("wvComplete({context}, {err_slot}, func() {val_ty} {{"),
                "})",
                |w| {
                    w.line(format!("return {value}"));
                },
            );
        },
    );
    w.blank();

    let notes = if f.cancellable {
        "It blocks until the call completes. Cancelling ctx cancels the native \
         call, which then returns ctx.Err()."
    } else {
        "It blocks until the call completes or ctx is done; in the latter case it \
         returns ctx.Err() at once and the native result is discarded."
    };
    let kind = Kind::callable(f.ret.is_some());
    let mut doc =
        GoDoc::new(go_name, f.doc.as_deref(), kind, None).params(&f.params, |n| names::param(n, f));
    if doc.is_empty() {
        doc = doc.para("Calls the native async function.");
    }
    doc = doc.para(notes);
    doc.deprecated(f.deprecated.as_deref()).emit(w);

    let (ret_sig, zero) = match &f.ret {
        Some(r) => (
            format!(" ({}, error)", go_type(r)),
            format!("{}, ", go_zero(r)),
        ),
        None => (" error".to_string(), String::new()),
    };
    w.block(header(recv, go_name, &go_params(f), &ret_sig), "}", |w| {
        w.block("if err := ctx.Err(); err != nil {", "}", |w| {
            w.line(format!("return {zero}err"));
        });
        let mut args = Vec::new();
        if recv.is_some() {
            emit_receiver(w, &mut args);
        }
        for p in &f.params {
            emit_param(w, ctx, &mut args, p, f);
        }
        let token = if proto.cancellable {
            w.line(format!("token := C.{}_cancel_token_create()", ctx.prefix));
            w.line(format!(
                "defer C.{}_cancel_token_destroy(token)",
                ctx.prefix
            ));
            args.push("token".into());
            "token"
        } else {
            "nil"
        };
        w.line(format!("call := wvNewAsyncCall[{val_ty}]()"));
        args.push(format!("C.{}(unsafe.Pointer(C.{tramp}))", ab.callback_type));
        args.push("call.context()".into());
        w.line(format!("C.{}({})", ab.launch.symbol, args.join(", ")));
        let res = if f.ret.is_some() { "res" } else { "_" };
        w.line(format!("{res}, fail, err := call.wait(ctx, {token})"));
        w.block("if err != nil {", "}", |w| {
            w.line(format!("return {zero}err"));
        });
        w.block("if fail != nil {", "}", |w| {
            if err.throws {
                w.line(format!("return {zero}{}", err.map("*fail")));
            } else {
                w.line("panic(fail.err())");
            }
        });
        match &f.ret {
            Some(_) => w.line("return res, nil"),
            None => w.line("return nil"),
        };
    });
    w.blank();
}
