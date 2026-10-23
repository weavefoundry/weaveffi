//! Call rendering: sync, async, and iterator wrappers and the parameter
//! marshalling they share.
//!
//! Each value crosses as one C value (see [`values`](super::values)), so a
//! wrapper lends each parameter, passes `&cErr` (sync calls and iterators)
//! or the completion and its context (async calls), and receives the one
//! result. How a failure surfaces comes from the callable's
//! [`ErrorStrategy`](weaveffi_model::model::ErrorStrategy).

use crate::codegen::CodeWriter;
use weaveffi_model::abi::Position;
use weaveffi_model::model::{AsyncBinding, FnBinding, IteratorBinding, Shape};
use weaveffi_model::ty::{ParamTy, Ty};

use crate::targets::go::docs::GoDoc;
use crate::targets::go::names::{self, pascal};
use crate::targets::go::types::{cgo_type, go_type, go_zero, param_type, ret_type};
use crate::targets::go::values::{cgo_value, lend, take};
use crate::targets::go::Ctx;

/// The wrapper type a method is declared on.
#[derive(Clone, Copy)]
pub(crate) struct Receiver<'a> {
    /// The Go type name of the interface wrapper (`Store`).
    pub(crate) ty: &'a str,
}

/// Render the Go wrapper of one callable: a package function (a free
/// function, constructor, or static) or, with `recv`, a method.
pub(crate) fn render_function(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    match &f.shape {
        Shape::Async(ab) => render_async(w, ctx, f, ab, go_name, recv),
        Shape::Iterator(it) => render_iterator(w, ctx, f, it, go_name, recv),
        Shape::Sync => render_sync(w, ctx, f, go_name, recv),
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
    let mut params = Vec::new();
    if f.is_async() {
        params.push("ctx context.Context".to_string());
    }
    params.extend(
        f.params
            .iter()
            .map(|p| format!("{} {}", names::param(&p.name, f), param_type(&p.ty))),
    );
    params
}

/// The opening of every wrapper body: a package function runs the
/// load-time checks (see `Check`), and a method borrows its receiver.
/// `s.native()` panics when the wrapper was already closed, and the
/// deferred release keeps the object alive (even across a concurrent
/// `Close`) until the call returns.
fn emit_prologue(w: &mut CodeWriter, recv: Option<Receiver>, args: &mut Vec<String>) {
    if recv.is_some() {
        w.line("cSelf := s.native()");
        w.line("defer s.release()");
        args.push("cSelf".into());
    } else {
        w.line("wvLoaded()");
    }
}

/// The local a parameter's C value is staged in: `c` and the Go parameter
/// name, capitalized (`other` is `cOther`, `err_` is `cErr_`).
fn staging(go_param: &str) -> String {
    let mut chars = go_param.chars();
    let first: String = chars
        .next()
        .map(|c| c.to_ascii_uppercase())
        .into_iter()
        .collect();
    format!("c{first}{}", chars.as_str())
}

/// Emit the staging for every parameter of `f` and push one C argument per
/// parameter, in order.
///
/// Value parameters are lent (see [`lend`]). A callback implementation is
/// kept in a handle table, passed as the argument struct's context beside
/// its interface's static vtable; a nil `Cb?` (or a typed nil) passes a
/// null vtable. Beside a callback, every value is staged first, so a value
/// that panics while it's lent (a nil required object, a rich-enum value
/// that's no variant) can't strand a callback's handle.
fn emit_params(w: &mut CodeWriter, ctx: &Ctx, f: &FnBinding, args: &mut Vec<String>) {
    let has_callbacks = f.params.iter().any(|p| p.ty.callback().is_some());
    let mut slots: Vec<Option<String>> = vec![None; f.params.len()];
    for (i, p) in f.params.iter().enumerate() {
        if let ParamTy::Value(ty) = &p.ty {
            let name = names::param(&p.name, f);
            let c = staging(&name);
            let mut arg = lend(w, ctx, ty, &name, &c);
            if has_callbacks && arg != c {
                w.line(format!("{c} := {arg}"));
                arg = c;
            }
            slots[i] = Some(arg);
        }
    }
    for (i, p) in f.params.iter().enumerate() {
        let ParamTy::Callback { name: cb, nullable } = &p.ty else {
            continue;
        };
        let name = names::param(&p.name, f);
        let tag = &ctx.model.callback(cb).c_tag;
        let vtable = format!(
            "C.{}()",
            vtable_accessor(&ctx.model.callback(cb).vtable_tag)
        );
        slots[i] = Some(if *nullable {
            let c = staging(&name);
            w.line(format!(
                "{c}Ctx, {c}Vtable := wvOptionalCallback({name}, {vtable})"
            ));
            format!("C.{tag}{{ctx: {c}Ctx, vtable: {c}Vtable}}")
        } else {
            format!(
                "C.{tag}{{ctx: wvNewCallback({name}, \"{}\"), vtable: {vtable}}}",
                pascal(cb)
            )
        });
    }
    args.extend(
        slots
            .into_iter()
            .map(|s| s.expect("every parameter has a slot")),
    );
}

/// The doc comment of a wrapper: its IDL doc and documented parameters,
/// then `extra` paragraphs, then `Deprecated:`.
fn wrapper_doc(ctx: &Ctx, f: &FnBinding, go_name: &str, extra: &[String]) -> GoDoc {
    let (doc, deprecated) = ctx.docs.of(&f.doc, &f.deprecated);
    let params = f.params.iter().filter_map(|p| {
        let (doc, _) = ctx.docs.of(&p.doc, &None);
        doc.map(|d| (names::param(&p.name, f), d))
    });
    let mut doc = GoDoc::decl(go_name, doc).params(params);
    for p in extra {
        doc = doc.para(p);
    }
    doc.deprecated(deprecated)
}

// ── Sync ──

/// Render a sync callable: the Go wrapper lending the parameters, calling
/// the C symbol, checking the error slot, and taking the result. A
/// callable that declares errors returns `(T, error)` (its domain's code
/// types, or an `*Error` when it fails with any error); any other panics
/// with an `*Error`.
fn render_sync(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    let throws = f.error.throws();
    let ret = f.ret_value();
    let ret_sig = match (ret, throws) {
        (Some(r), true) => format!(" ({}, error)", go_type(r)),
        (Some(r), false) => format!(" {}", go_type(r)),
        (None, true) => " error".into(),
        (None, false) => String::new(),
    };
    wrapper_doc(ctx, f, go_name, &[]).emit(w);
    w.block(header(recv, go_name, &go_params(f), &ret_sig), "}", |w| {
        let mut args = Vec::new();
        emit_prologue(w, recv, &mut args);
        emit_params(w, ctx, f, &mut args);
        w.line(format!("var cErr C.{}_error", ctx.prefix));
        args.push("&cErr".into());
        let call = format!("C.{}({})", f.abi.symbol, args.join(", "));
        match ret {
            Some(_) => w.line(format!("cRet := {call}")),
            None => w.line(call),
        };
        if throws {
            let mapped = ctx.map_failure(&f.error, "wvTakeError(&cErr)");
            w.block("if cErr.code != 0 {", "}", |w| {
                match ret {
                    Some(ty) => w.line(format!("return {}, {mapped}", go_zero(ty))),
                    None => w.line(format!("return {mapped}")),
                };
            });
        } else {
            w.line("wvTrap(&cErr)");
        }
        let ok = if throws { ", nil" } else { "" };
        match ret {
            Some(ty) => {
                w.line(format!("return {}{ok}", take(ty, "cRet")));
            }
            None if throws => {
                w.line("return nil");
            }
            None => {}
        }
    });
    w.blank();
}

// ── Iterators ──

/// Render an `iter<T>` callable as Go's lazy sequence idiom over the
/// runtime's `wvSeq` (`iter.Seq[T]`) or, when it declares errors, `wvSeq2`
/// (`iter.Seq2[T, error]`, a failure yielded as a final `(zero, err)`
/// pair). The wrapper supplies the launch, whose cursor pulls one element
/// per step through `_next` and destroys the iterator.
fn render_iterator(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    it: &IteratorBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    let throws = f.error.throws();
    let elem = &it.elem;
    let elem_go = go_type(elem);
    let seq = ret_type(f.ret.as_ref().expect("an iterator returns"), throws);
    let mut note = format!(
        "{go_name} returns a lazy sequence: each range over it launches the native \
         iterator, pulls one element per step, and releases the iterator when the \
         loop ends, early or not."
    );
    if throws {
        note.push_str(" A failure is yielded as a final (zero value, error) pair.");
    }
    wrapper_doc(ctx, f, go_name, &[note]).emit(w);

    let p = ctx.prefix;
    w.block(
        header(recv, go_name, &go_params(f), &format!(" {seq}")),
        "}",
        |w| {
            if recv.is_none() {
                w.line("wvLoaded()");
            }
            let (seq_fn, tail) = if throws {
                ("wvSeq2", format!(", {})", ctx.mapper(&f.error)))
            } else {
                ("wvSeq", ")".to_string())
            };
            w.line(format!(
                "return {seq_fn}(func(cErr *C.{p}_error) wvCursor[{elem_go}] {{"
            ));
            w.scope(|w| {
                let mut args = Vec::new();
                if recv.is_some() {
                    emit_prologue(w, recv, &mut args);
                }
                emit_params(w, ctx, f, &mut args);
                args.push("cErr".into());
                w.line(format!("it := C.{}({})", f.abi.symbol, args.join(", ")));
                w.block(format!("return wvCursor[{elem_go}]{{"), "}", |w| {
                    w.block(
                        format!("next: func(cErr *C.{p}_error) ({elem_go}, bool) {{"),
                        "},",
                        |w| emit_next(w, ctx, it, elem),
                    );
                    w.line(format!(
                        "destroy: func() {{ C.{}(it) }},",
                        it.destroy_symbol
                    ));
                });
            });
            w.line(format!("}}{tail}"));
        },
    );
    w.blank();
}

/// The body of an iterator cursor's `next`: declare the item, call
/// `_next`, and take the element.
fn emit_next(w: &mut CodeWriter, ctx: &Ctx, it: &IteratorBinding, elem: &Ty) {
    w.line(format!(
        "var cItem {}",
        cgo_value(ctx, elem, Position::Transfer)
    ));
    w.block(
        format!("if C.{}(it, &cItem, cErr) == 0 {{", it.next.symbol),
        "}",
        |w| {
            w.line(format!("return {}, false", go_zero(elem)));
        },
    );
    w.line(format!("return {}, true", take(elem, "cItem")));
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
/// takes the result and delivers it on the call's channel, and a blocking
/// wrapper taking a leading `context.Context`.
///
/// Every async wrapper returns an `error` and never panics on a failure:
/// a function that declares a domain returns its code types, any other an
/// `*Error`. A cancellable one creates a native cancel token, cancels it
/// when `ctx` is done, and returns `ctx.Err()` for the cancelled
/// completion; any other returns `ctx.Err()` as soon as `ctx` is done and
/// abandons the result (see the runtime's `wvAwait`).
fn render_async(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    ab: &AsyncBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    let tramp = completion_trampoline(&ab.callback_type);
    let ret = f.ret_value();
    let val_ty = ret.map_or_else(|| "struct{}".to_string(), go_type);

    let params = &ab.callback.params;
    let formals: Vec<String> = params
        .iter()
        .map(|s| format!("{} {}", names::slot(&s.name), cgo_type(&s.ty, ctx.prefix)))
        .collect();
    let context = names::slot(&params[0].name);
    let err_slot = names::slot(&params[1].name);
    let value = match (params.get(2), ret) {
        (Some(result), Some(ty)) => take(ty, &names::slot(&result.name)),
        _ => "struct{}{}".to_string(),
    };
    w.line(format!("//export {tramp}"));
    w.block(
        format!("func {tramp}({}) {{", formals.join(", ")),
        "}",
        |w| {
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

    let note = if f.cancellable() {
        format!(
            "{go_name} blocks until the call completes. Cancelling ctx cancels the \
             native call, which then returns ctx.Err()."
        )
    } else {
        format!(
            "{go_name} blocks until the call completes or ctx is done; in the latter \
             case it returns ctx.Err() at once and the native result is discarded."
        )
    };
    wrapper_doc(ctx, f, go_name, &[note]).emit(w);

    let (ret_sig, zero) = match ret {
        Some(r) => (
            format!(" ({}, error)", go_type(r)),
            format!("{}, ", go_zero(r)),
        ),
        None => (" error".to_string(), String::new()),
    };
    w.block(header(recv, go_name, &go_params(f), &ret_sig), "}", |w| {
        if recv.is_none() {
            w.line("wvLoaded()");
        }
        w.block("if err := ctx.Err(); err != nil {", "}", |w| {
            w.line(format!("return {zero}err"));
        });
        let mut args = Vec::new();
        if recv.is_some() {
            emit_prologue(w, recv, &mut args);
        }
        emit_params(w, ctx, f, &mut args);
        let token = if ab.cancellable {
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
        w.line(format!("C.{}({})", f.abi.symbol, args.join(", ")));
        let await_call = format!("wvAwait(ctx, call, {token}, {})", ctx.mapper(&f.error));
        match ret {
            Some(_) => w.line(format!("return {await_call}")),
            None => {
                w.line(format!("_, err := {await_call}"));
                w.line("return err")
            }
        };
    });
    w.blank();
}
