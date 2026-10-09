//! Call rendering: sync, async, and iterator wrappers and the argument and
//! result marshalling they share.
//!
//! Marshalling reads the model's stored contracts: each parameter's
//! [`ArgPass`], and the result's [`RetPass`] (sync), [`ResultPass`] (async
//! completion), or [`ItemPass`] (iterator element), so this module only
//! spells those contracts in Go. How a failure surfaces comes from the
//! callable's
//! [`ErrorStrategy`](weaveffi_model::plan::ErrorStrategy).

use crate::codegen::CodeWriter;
use weaveffi_model::abi::CType;
use weaveffi_model::model::{AsyncBinding, CallShape, FnBinding, IteratorBinding, ParamBinding};
use weaveffi_model::plan::{ArgPass, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{Prim, RetTy, Ty};

use crate::targets::go::codec::{read_fn, write_fn};
use crate::targets::go::docs::GoDoc;
use crate::targets::go::names::{self, pascal};
use crate::targets::go::types::{
    cgo_pointee, cgo_type, go_type, go_zero, param_type, prim_type, ret_type, strip_const,
};
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
    match (&f.shape, f.iterator()) {
        (CallShape::Async(ab), _) => render_async(w, ctx, f, ab, go_name, recv),
        (CallShape::Sync, Some(it)) => render_iterator(w, ctx, f, it, go_name, recv),
        (CallShape::Sync, None) => render_sync(w, ctx, f, go_name, recv),
    }
}

// ── Shared pieces ──

/// How a value the native library hands over arrives, whatever the
/// position: the shape of its slots, independent of the slot names.
enum Received<'a> {
    /// A scalar or C-style enum by value.
    Direct,
    /// A presence flag and a scalar.
    OptDirect,
    /// A typed array of the primitive (pointer and element count).
    Slice(Prim),
    /// A UTF-8 run.
    String,
    /// A byte run.
    Bytes,
    /// A value buffer.
    Buffer,
    /// An owned object reference of the interface.
    Object(&'a str),
}

/// The Go expression receiving one value of type `ty` the native library
/// hands over in the slots `a` (and `b`, the length or value slot), taking
/// ownership: a direct value converted, an optional scalar joined, a typed
/// array, string, or bytes copied and released, a buffer decoded and
/// released, an object adopted (a null `Interface?` adopts to nil).
fn receive(ty: &Ty, how: &Received, a: &str, b: &str) -> String {
    match how {
        Received::Direct => format!("{}({a})", go_type(ty)),
        Received::OptDirect => {
            let Ty::Optional(inner) = ty else {
                unreachable!("an optional scalar has an optional type")
            };
            format!("wvOptional(bool({a}), {}({b}))", go_type(inner))
        }
        Received::Slice(p) => format!("wvTakeSlice[{}]({a}, {b})", prim_type(*p)),
        Received::String => format!("wvTakeString({a}, {b})"),
        Received::Bytes => format!("wvTakeBytes({a}, {b})"),
        Received::Buffer => format!("wvDecode({a}, {b}, {})", read_fn(ty)),
        Received::Object(i) => format!("wvAdopt{}({a})", pascal(i)),
    }
}

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

/// The C type's cgo spelling with `const` dropped.
fn cgo(ct: &CType, ctx: &Ctx) -> String {
    cgo_type(&strip_const(ct), ctx.prefix)
}

/// Emit the staging statements for one parameter and push its C argument
/// expressions, per its [`ArgPass`].
///
/// Strings, bytes, and typed arrays pass a borrowed view of the Go memory
/// (cgo keeps it in place for the call); a buffered value is encoded first,
/// and an optional scalar splits into its flag and value. An object
/// parameter borrows the wrapper's pointer for the call, like a receiver,
/// and a nil `Interface?` passes null. A callback implementation is kept in
/// a handle table, passed as the context, beside the interface's static
/// vtable; a nil `Cb?` (or a typed nil) passes no callback.
fn emit_param(
    w: &mut CodeWriter,
    ctx: &Ctx,
    args: &mut Vec<String>,
    p: &ParamBinding,
    f: &FnBinding,
) {
    let name = names::param(&p.name, f);
    let c = format!("c{}", pascal(&p.name));
    match &p.pass {
        ArgPass::Direct { slot } => args.push(format!("{}({name})", cgo(&slot.ty, ctx))),
        ArgPass::OptDirect { has, value, .. } => {
            w.line(format!("{c}Has, {c} := wvPresent({name})"));
            args.extend([
                format!("{}({c}Has)", cgo(&has.ty, ctx)),
                format!("{}({c})", cgo(&value.ty, ctx)),
            ]);
        }
        ArgPass::Slice { ptr, .. } => {
            w.line(format!(
                "{c}Ptr, {c}Len := wvSliceIn[{}]({name})",
                cgo_pointee(&ptr.ty, ctx.prefix)
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
        ArgPass::Buffer { .. } => {
            let ty = p.ty.value().expect("a buffered parameter is a value");
            w.line(format!(
                "{c}Ptr, {c}Len := wvBytes(wvEncode({name}, {}))",
                write_fn(ty)
            ));
            args.extend([format!("{c}Ptr"), format!("{c}Len")]);
        }
        ArgPass::Object { slot, nullable, .. } => {
            if *nullable {
                w.line(format!("var {c} {}", cgo(&slot.ty, ctx)));
                w.block(format!("if {name} != nil {{"), "}", |w| {
                    w.line(format!("{c} = {name}.native()"));
                    w.line(format!("defer {name}.release()"));
                });
            } else {
                w.line(format!("{c} := {name}.native()"));
                w.line(format!("defer {name}.release()"));
            }
            args.push(c);
        }
        ArgPass::Callback {
            vtable,
            nullable,
            interface,
            ..
        } => {
            let CType::Ptr { pointee, .. } = &vtable.ty else {
                unreachable!("a callback vtable slot is a pointer")
            };
            let accessor = format!("C.{}()", vtable_accessor(&pointee.render_c(ctx.prefix)));
            if *nullable {
                w.line(format!(
                    "{c}Ctx, {c}Vtable := wvOptionalCallback({name}, {accessor})"
                ));
                args.extend([format!("{c}Ctx"), format!("{c}Vtable")]);
            } else {
                args.extend([
                    format!("wvNewCallback({name}, \"{}\")", pascal(interface)),
                    accessor,
                ]);
            }
        }
    }
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

/// The value type a callable returns, if any (the element type for an
/// iterator).
fn ret_value(f: &FnBinding) -> Option<&Ty> {
    f.ret.as_ref().map(RetTy::elem)
}

// ── Sync ──

/// Render a sync callable: the Go wrapper marshalling the parameters,
/// calling the C symbol, checking the error slot, and receiving the
/// result. A callable that declares errors returns `(T, error)` (its
/// domain's code types, or an `*Error` when it fails with any error); any
/// other panics with an `*Error`.
fn render_sync(
    w: &mut CodeWriter,
    ctx: &Ctx,
    f: &FnBinding,
    go_name: &str,
    recv: Option<Receiver>,
) {
    let throws = f.error.throws();
    let ret = ret_value(f);
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
        for p in &f.params {
            emit_param(w, ctx, &mut args, p, f);
        }
        let how = match &f.ret_pass {
            RetPass::Void => None,
            RetPass::Direct => Some((Received::Direct, "")),
            RetPass::OptDirect { out_value } => {
                w.line(format!(
                    "var cOut {}",
                    cgo_pointee(&out_value.ty, ctx.prefix)
                ));
                args.push("&cOut".into());
                Some((Received::OptDirect, "cOut"))
            }
            RetPass::Slice { elem, .. } => Some((Received::Slice(*elem), "cRetLen")),
            RetPass::String { .. } => Some((Received::String, "cRetLen")),
            RetPass::Bytes { .. } => Some((Received::Bytes, "cRetLen")),
            RetPass::Buffer { .. } => Some((Received::Buffer, "cRetLen")),
            RetPass::Object { interface, .. } => Some((Received::Object(interface), "")),
            RetPass::Iterator(_) => unreachable!("iterators render separately"),
        };
        if let Some((_, "cRetLen")) = &how {
            w.line("var cRetLen C.size_t");
            args.push("&cRetLen".into());
        }
        w.line(format!("var cErr C.{}_error", ctx.prefix));
        args.push("&cErr".into());
        let call = format!("C.{}({})", f.abi.symbol, args.join(", "));
        match &how {
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
        match (&how, ret) {
            (Some((how, b)), Some(ty)) => {
                w.line(format!("return {}{ok}", receive(ty, how, "cRet", b)));
            }
            _ if throws => {
                w.line("return nil");
            }
            _ => {}
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
                for param in &f.params {
                    emit_param(w, ctx, &mut args, param, f);
                }
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

/// The body of an iterator cursor's `next`: declare the item slots, call
/// `_next`, and receive the element.
fn emit_next(w: &mut CodeWriter, ctx: &Ctx, it: &IteratorBinding, elem: &Ty) {
    let pointee = |slot: &weaveffi_model::abi::AbiParam| cgo_pointee(&slot.ty, ctx.prefix);
    let (how, slots): (Received, Vec<(&str, String)>) = match &it.item {
        ItemPass::Direct { out_item } => (Received::Direct, vec![("cItem", pointee(out_item))]),
        ItemPass::OptDirect { out_has, out_item } => (
            Received::OptDirect,
            vec![("cHas", pointee(out_has)), ("cItem", pointee(out_item))],
        ),
        ItemPass::Slice {
            out_item,
            out_len,
            elem,
        } => (
            Received::Slice(*elem),
            vec![("cItem", pointee(out_item)), ("cItemLen", pointee(out_len))],
        ),
        ItemPass::String { out_item, out_len } => (
            Received::String,
            vec![("cItem", pointee(out_item)), ("cItemLen", pointee(out_len))],
        ),
        ItemPass::Bytes { out_item, out_len } => (
            Received::Bytes,
            vec![("cItem", pointee(out_item)), ("cItemLen", pointee(out_len))],
        ),
        ItemPass::Buffer { out_item, out_len } => (
            Received::Buffer,
            vec![("cItem", pointee(out_item)), ("cItemLen", pointee(out_len))],
        ),
        ItemPass::Object {
            out_item,
            interface,
            ..
        } => (
            Received::Object(interface),
            vec![("cItem", pointee(out_item))],
        ),
    };
    let mut args = vec!["it".to_string()];
    for (name, ty) in &slots {
        w.line(format!("var {name} {ty}"));
        args.push(format!("&{name}"));
    }
    args.push("cErr".into());
    w.block(
        format!("if C.{}({}) == 0 {{", it.next.symbol, args.join(", ")),
        "}",
        |w| {
            w.line(format!("return {}, false", go_zero(elem)));
        },
    );
    let (a, b) = match slots.as_slice() {
        [(a, _)] => (*a, ""),
        [(a, _), (b, _)] => (*a, *b),
        _ => unreachable!("an item has one or two slots"),
    };
    // An optional scalar's slots are (flag, value); `receive` wants them in
    // that order too.
    w.line(format!("return {}, true", receive(elem, &how, a, b)));
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
    let ret = ret_value(f);
    let val_ty = ret.map_or_else(|| "struct{}".to_string(), go_type);

    let formals: Vec<String> = ab
        .callback_params
        .iter()
        .map(|s| format!("{} {}", names::slot(&s.name), cgo_type(&s.ty, ctx.prefix)))
        .collect();
    let context = names::slot(&ab.callback_params[0].name);
    let err_slot = names::slot(&ab.callback_params[1].name);
    let slot = |s: &weaveffi_model::abi::AbiParam| names::slot(&s.name);
    let value = match (&ab.result, ret) {
        (ResultPass::Void, _) | (_, None) => "struct{}{}".to_string(),
        (ResultPass::Direct { result }, Some(ty)) => {
            receive(ty, &Received::Direct, &slot(result), "")
        }
        (ResultPass::OptDirect { has, value }, Some(ty)) => {
            receive(ty, &Received::OptDirect, &slot(has), &slot(value))
        }
        (ResultPass::Slice { ptr, len, elem }, Some(ty)) => {
            receive(ty, &Received::Slice(*elem), &slot(ptr), &slot(len))
        }
        (ResultPass::String { ptr, len }, Some(ty)) => {
            receive(ty, &Received::String, &slot(ptr), &slot(len))
        }
        (ResultPass::Bytes { ptr, len }, Some(ty)) => {
            receive(ty, &Received::Bytes, &slot(ptr), &slot(len))
        }
        (ResultPass::Buffer { ptr, len }, Some(ty)) => {
            receive(ty, &Received::Buffer, &slot(ptr), &slot(len))
        }
        (
            ResultPass::Object {
                result, interface, ..
            },
            Some(ty),
        ) => receive(ty, &Received::Object(interface), &slot(result), ""),
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
        for p in &f.params {
            emit_param(w, ctx, &mut args, p, f);
        }
        let token = if ab.cancellable() {
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
