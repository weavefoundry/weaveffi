//! Callback-interface rendering: the abstract class a consumer implements,
//! the `Struct` mirroring the C vtable (the `size`/`flags`/`free` header,
//! then one entry per method), the vtable entries, the dispatcher that
//! delivers forwarded void methods, and the vtable itself.
//!
//! A method that returns a value is a `NativeCallable.isolateLocal`
//! trampoline: it runs synchronously on the isolate's thread, receives its
//! borrowed arguments in place, adopts object arguments, and hands its return
//! to the producer (by value, as a fresh object reference, or as a
//! `{prefix}_alloc` run in the out slots). A thrown exception never unwinds:
//! a `throws` method reports its domain error's code and payload, and
//! anything else is reported as `-4`. A void method is a
//! `NativeCallable.isolateGroupBound` forwarder: it may run on any thread,
//! copies its arguments into a message for the isolate, and returns at once;
//! the dispatcher then runs the implementation on the event loop.

use crate::codegen::CodeWriter;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding};
use weaveffi_model::plan::{ArgPass, RetPass};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::dart::calls::adopt_expr;
use crate::targets::dart::codec::{pack_fn, unpack_fn};
use crate::targets::dart::docs::{write_deprecated, write_doc};
use crate::targets::dart::entities::payload_fn;
use crate::targets::dart::types::{
    dart_class, dart_ident, dart_type, ffi_type, ffi_typedef, object_class, zero_literal,
};

/// The private global holding a callback interface's vtable.
pub(crate) fn vtable_var(name: &str) -> String {
    format!("_{}Vtable", lower_first(&dart_class(name)))
}

/// The private dispatcher delivering a callback interface's void methods.
pub(crate) fn dispatch_fn(name: &str) -> String {
    format!("_{}Dispatch", lower_first(&dart_class(name)))
}

/// The `Struct` mirroring a callback interface's C vtable.
fn vtable_struct(class: &str) -> String {
    format!("_{class}Vtable")
}

/// The vtable entry function of one method.
fn entry_fn(class: &str, method: &str) -> String {
    format!("_{}{}", lower_first(class), method.to_upper_camel_case())
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |c| {
        c.to_lowercase().collect::<String>() + chars.as_str()
    })
}

/// Render one callback interface; `exception` is the domain exception
/// class in scope for its module, which its `throws` methods report.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    exception: Option<&str>,
) {
    let class = dart_class(&cb.name);
    render_abstract_class(w, cb, &class, exception);
    render_vtable_struct(w, cb, &class);
    for m in &cb.methods {
        if m.ret.is_some() {
            render_trampoline(w, cb, m, &class, exception.filter(|_| m.throws));
        } else {
            render_forwarder(w, cb, m, &class);
        }
    }
    render_dispatch(w, cb, &class);
    render_vtable(w, cb, &class);
}

/// The class a consumer extends or implements.
fn render_abstract_class(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    class: &str,
    exception: Option<&str>,
) {
    w.blank();
    write_doc(w, &cb.doc);
    if cb.doc.is_some() {
        w.line("///");
    }
    w.line("/// A callback interface: implement it and pass an instance to any");
    w.line(format!(
        "/// function taking a [{class}]. The library keeps the instance alive"
    ));
    w.line("/// until the producer releases it.");
    w.line("///");
    w.line("/// Methods that return a value run synchronously and must be called by");
    w.line("/// the producer on this isolate's thread during a call from Dart; a");
    w.line("/// thrown exception fails that call with [NativeException.foreignCode]");
    w.line("/// (or, from a method that throws, the domain exception's code and");
    w.line("/// fields). Void methods may be called from any thread: they're");
    w.line("/// delivered asynchronously on this isolate's event loop, in the zone");
    w.line("/// that passed the instance, where a thrown exception is an uncaught");
    w.line("/// error of that zone. Object arguments are owned by the");
    w.line("/// implementation.");
    write_deprecated(w, &cb.deprecated);
    w.block(format!("abstract class {class} {{"), "}", |w| {
        for (i, m) in cb.methods.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            write_doc(w, &m.doc);
            if let (true, Some(exc), Some(_)) = (m.throws, exception, &m.ret) {
                if m.doc.is_some() {
                    w.line("///");
                }
                w.line(format!(
                    "/// Throw a [{exc}] to fail the producer's call with it."
                ));
            }
            write_deprecated(w, &m.deprecated);
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{} {}", dart_type(&p.ty), dart_ident(&p.name)))
                .collect();
            let ret = m.ret.as_ref().map_or("void".to_string(), dart_type);
            w.line(format!(
                "{ret} {}({});",
                dart_ident(&m.name),
                params.join(", ")
            ));
        }
    });
}

/// The native typedef of one vtable entry.
fn entry_typedef(cb: &CallbackInterfaceBinding, m: &CallbackMethodBinding) -> String {
    ffi_typedef(&format!("{}_{}", cb.c_tag, m.name))
}

/// The `Struct` whose layout is the C vtable: the `size`, `flags`, and
/// `free` header, then one function pointer per method in declaration order.
fn render_vtable_struct(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, class: &str) {
    w.blank();
    for m in &cb.methods {
        let natives: Vec<String> = m.abi_params.iter().map(|p| ffi_type(&p.ty).0).collect();
        w.line(format!(
            "typedef {} = {} Function({});",
            entry_typedef(cb, m),
            ffi_type(&m.abi_ret).0,
            natives.join(", ")
        ));
    }
    w.blank();
    w.line(format!("// The C `{}` layout.", cb.vtable_tag));
    w.block(
        format!("final class {} extends Struct {{", vtable_struct(class)),
        "}",
        |w| {
            w.line("@Uint32()");
            w.line("external int size;");
            w.line("@Uint32()");
            w.line("external int flags;");
            w.line("external Pointer<NativeFunction<Void Function(Pointer<Void>)>> free;");
            for m in &cb.methods {
                w.line(format!(
                    "external Pointer<NativeFunction<{}>> {};",
                    entry_typedef(cb, m),
                    dart_ident(&m.name)
                ));
            }
        },
    );
}

/// The Dart parameter list of one vtable entry.
fn entry_params(m: &CallbackMethodBinding) -> String {
    m.abi_params
        .iter()
        .map(|p| format!("{} {}", ffi_type(&p.ty).1, dart_ident(&p.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The Dart name of one of an entry's trailing slots (`out_ptr`, `out_len`,
/// `out_err`), counted from the end.
fn trailing_slot(m: &CallbackMethodBinding, from_end: usize) -> String {
    dart_ident(&m.abi_params[m.abi_params.len() - from_end].name)
}

/// A value-returning method's `isolateLocal` trampoline: adopt object
/// arguments first (so a later failure still leaves each reference owned by
/// a finalizable wrapper), receive the borrowed ones in place, call the
/// implementation, and hand its value to the producer. A thrown exception is
/// reported through `out_err` and the zero value is returned; `exception` is
/// the domain the method reports when it's declared `throws`.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    class: &str,
    exception: Option<&str>,
) {
    let ret = m.ret.as_ref().expect("value-returning method");
    let pass = RetPass::of(Some(ret));
    let (_, dart_ret) = ffi_type(&m.abi_ret);
    let out_err = trailing_slot(m, 1);
    let zero = match &pass {
        RetPass::Direct => Some(zero_literal(ret)),
        RetPass::Object { .. } => Some("nullptr"),
        _ => None,
    };
    w.blank();
    w.line(format!("// Vtable entry `{}.{}`.", cb.vtable_tag, m.name));
    w.block(
        format!(
            "{dart_ret} {}({}) {{",
            entry_fn(class, &m.name),
            entry_params(m)
        ),
        "}",
        |w| {
            w.line("try {");
            w.scope(|w| {
                // Objects first: nothing before them can throw.
                let is_object = |p: &ParamBinding| matches!(p.arg_pass(), ArgPass::Object { .. });
                let mut args = vec![String::new(); m.params.len()];
                for (i, p) in m.params.iter().enumerate().filter(|(_, p)| is_object(p)) {
                    args[i] = receive_arg(w, p);
                }
                for (i, p) in m.params.iter().enumerate().filter(|(_, p)| !is_object(p)) {
                    args[i] = receive_arg(w, p);
                }
                let call = format!(
                    "(_callbackTarget(ctx) as {class}).{}({})",
                    dart_ident(&m.name),
                    args.join(", ")
                );
                if matches!(pass, RetPass::Direct) {
                    let value = if matches!(ret, Ty::Enum(_)) {
                        ".value"
                    } else {
                        ""
                    };
                    w.line(format!("return {call}{value};"));
                    return;
                }
                w.line(format!("final result = {call};"));
                // The out slots of a string, bytes, or buffer return.
                let out = || format!("{}, {}", trailing_slot(m, 3), trailing_slot(m, 2));
                w.line(match &pass {
                    // A fresh strong reference the producer adopts.
                    RetPass::Object { nullable: false } => "return result._cloneRef();".into(),
                    RetPass::Object { nullable: true } => {
                        "return result?._cloneRef() ?? nullptr;".into()
                    }
                    RetPass::String => {
                        format!("_handOver(utf8.encode(result), {});", out())
                    }
                    RetPass::Bytes => format!("_handOver(result, {});", out()),
                    RetPass::Buffer => {
                        format!("_handOver(_encode(result, {}), {});", pack_fn(ret), out())
                    }
                    RetPass::Void | RetPass::Direct => unreachable!("handled above"),
                });
            });
            if let Some(exc) = exception {
                w.line(format!("}} on {exc} catch (e) {{"));
                w.scope(|w| {
                    w.line(format!(
                        "_failCallback({out_err}, e.code, e.message, {}(e));",
                        payload_fn(exc)
                    ));
                });
            }
            w.line("} catch (e) {");
            w.scope(|w| {
                w.line(format!("_foreignError({out_err}, e);"));
            });
            w.line("}");
            if let Some(zero) = zero {
                w.line(format!("return {zero};"));
            }
        },
    );
}

/// The expression receiving one parameter inside a trampoline: borrowed runs
/// are copied, buffers decoded, and objects adopted. Objects and buffers
/// (which may carry object tokens) are received into locals declared first,
/// so each reference is owned even if a later step throws.
fn receive_arg(w: &mut CodeWriter, p: &ParamBinding) -> String {
    let slot = dart_ident(&p.abi[0].name);
    match p.arg_pass() {
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Enum(n) => format!("{}.fromValue({slot})", dart_class(n)),
            _ => slot,
        },
        ArgPass::String { len, .. } => format!("_readString({slot}, {})", dart_ident(&len.name)),
        ArgPass::Bytes { len, .. } => format!("_copyBytes({slot}, {})", dart_ident(&len.name)),
        ArgPass::Buffer { len, .. } => {
            let decode = format!(
                "_decode(_copyBytes({slot}, {}), {})",
                dart_ident(&len.name),
                unpack_fn(&p.ty)
            );
            // Any object tokens inside are adopted references.
            let local = format!("_{}", dart_ident(&p.name));
            w.line(format!("final {local} = {decode};"));
            local
        }
        ArgPass::Object { nullable, .. } => {
            let local = format!("_{}", dart_ident(&p.name));
            w.line(format!(
                "final {local} = {};",
                adopt_expr(&slot, &p.ty, nullable)
            ));
            local
        }
        ArgPass::Callback { .. } => {
            unreachable!("callback methods never take callback interfaces")
        }
    }
}

/// A void method's `isolateGroupBound` forwarder: it runs outside the
/// isolate, so it only copies its slots into a message (borrowed bytes are
/// copied by the post) and touches no Dart globals.
fn render_forwarder(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    class: &str,
) {
    let index = cb
        .methods
        .iter()
        .position(|x| x.name == m.name)
        .expect("method of its interface");
    w.blank();
    w.line(format!(
        "// Vtable entry `{}.{}`: forwarded to the isolate.",
        cb.vtable_tag, m.name
    ));
    w.block(
        format!("void {}({}) {{", entry_fn(class, &m.name), entry_params(m)),
        "}",
        |w| {
            w.line(format!(
                "_CallbackMessage(ctx, {index}, {})",
                m.params.len()
            ));
            w.scope(|w| {
                w.scope(|w| {
                    for p in &m.params {
                        let slot = dart_ident(&p.abi[0].name);
                        w.line(match p.arg_pass() {
                            ArgPass::Direct { .. } => match &p.ty {
                                Ty::Prim(Prim::Bool) => format!("..boolean({slot})"),
                                Ty::Prim(Prim::F32 | Prim::F64) => {
                                    format!("..float64({slot})")
                                }
                                _ => format!("..int64({slot})"),
                            },
                            ArgPass::String { len, .. }
                            | ArgPass::Bytes { len, .. }
                            | ArgPass::Buffer { len, .. } => {
                                format!("..bytes({slot}, {})", dart_ident(&len.name))
                            }
                            ArgPass::Object { .. } => format!("..pointer({slot})"),
                            ArgPass::Callback { .. } => {
                                unreachable!("callback methods never take callback interfaces")
                            }
                        });
                    }
                    w.line("..send();");
                });
            });
        },
    );
}

/// The dispatcher running forwarded void methods on the isolate:
/// `message` is `[key, method, arguments...]`.
fn render_dispatch(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, class: &str) {
    w.blank();
    w.block(
        format!(
            "void {}(Object impl, int method, List<Object?> message) {{",
            dispatch_fn(&cb.name)
        ),
        "}",
        |w| {
            if cb.methods.iter().all(|m| m.ret.is_some()) {
                w.line(format!(
                    "// Every {class} method returns a value: nothing is forwarded."
                ));
                return;
            }
            w.line(format!("final target = impl as {class};"));
            w.block("switch (method) {", "}", |w| {
                for (index, m) in cb.methods.iter().enumerate() {
                    if m.ret.is_some() {
                        continue;
                    }
                    // Object arguments are adopted first, so a failure while
                    // decoding the rest still leaves each transferred
                    // reference owned by a finalizable wrapper.
                    w.block(format!("case {index}: {{"), "}", |w| {
                        let params: Vec<(usize, &ParamBinding)> =
                            m.params.iter().enumerate().collect();
                        let is_object =
                            |p: &ParamBinding| matches!(p.arg_pass(), ArgPass::Object { .. });
                        for (i, p) in params.iter().filter(|(_, p)| is_object(p)) {
                            message_arg(w, p, i + 2);
                        }
                        for (i, p) in params.iter().filter(|(_, p)| !is_object(p)) {
                            message_arg(w, p, i + 2);
                        }
                        let args: Vec<String> =
                            (0..m.params.len()).map(|i| format!("a{}", i + 2)).collect();
                        w.line(format!(
                            "target.{}({});",
                            dart_ident(&m.name),
                            args.join(", ")
                        ));
                    });
                }
            });
        },
    );
}

/// Decode forwarded argument `i` of `message` into the local `a{i}`.
fn message_arg(w: &mut CodeWriter, p: &ParamBinding, i: usize) {
    let field = format!("message[{i}]!");
    let local = format!("a{i}");
    let value = match p.arg_pass() {
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Enum(n) => format!("{}.fromValue({field} as int)", dart_class(n)),
            ty => format!("{field} as {}", dart_type(ty)),
        },
        ArgPass::String { .. } => format!("utf8.decode({field} as Uint8List)"),
        ArgPass::Bytes { .. } => format!("{field} as Uint8List"),
        ArgPass::Buffer { .. } => {
            format!("_decode({field} as Uint8List, {})", unpack_fn(&p.ty))
        }
        ArgPass::Object { nullable, .. } => {
            let address = format!("{local}Address");
            w.line(format!("final {address} = {field} as int;"));
            let adopt = format!(
                "{}._(Pointer<Void>.fromAddress({address}))",
                object_class(&p.ty)
            );
            if nullable {
                format!("{address} == 0 ? null : {adopt}")
            } else {
                adopt
            }
        }
        ArgPass::Callback { .. } => {
            unreachable!("callback methods never take callback interfaces")
        }
    };
    w.line(format!("final {local} = {value};"));
}

/// The vtable every instance passed to the producer from this isolate
/// shares; the per-instance state travels in `ctx`.
fn render_vtable(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, class: &str) {
    let strukt = vtable_struct(class);
    w.blank();
    w.block(
        format!("final Pointer<{strukt}> {} = () {{", vtable_var(&cb.name)),
        "}();",
        |w| {
            w.line(format!("final vtable = calloc<{strukt}>();"));
            w.line("vtable.ref");
            w.scope(|w| {
                w.scope(|w| {
                    w.line(format!("..size = sizeOf<{strukt}>()"));
                    w.line("..flags = 0");
                    w.line("..free = _callbackFree");
                    for (i, m) in cb.methods.iter().enumerate() {
                        let td = entry_typedef(cb, m);
                        let entry = entry_fn(class, &m.name);
                        let callable = match RetPass::of(m.ret.as_ref()) {
                            RetPass::Void => {
                                format!("NativeCallable<{td}>.isolateGroupBound({entry})")
                            }
                            RetPass::Direct => format!(
                                "NativeCallable<{td}>.isolateLocal({entry}, exceptionalReturn: {})",
                                zero_literal(m.ret.as_ref().expect("direct return"))
                            ),
                            _ => format!("NativeCallable<{td}>.isolateLocal({entry})"),
                        };
                        let end = if i + 1 == cb.methods.len() { ";" } else { "" };
                        w.line(format!("..{} = _pin({callable}){end}", dart_ident(&m.name)));
                    }
                });
            });
            w.line("return vtable;");
        },
    );
}
