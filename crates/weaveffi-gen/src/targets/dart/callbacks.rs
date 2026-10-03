//! Callback-interface rendering: the abstract class a consumer implements,
//! the `Struct` mirroring the C vtable, one entry per method, the dispatcher
//! that delivers forwarded void methods, and the one process-wide vtable.
//!
//! A method that returns a value is a `NativeCallable.isolateLocal`
//! trampoline: it runs synchronously on the isolate's thread, receives its
//! borrowed arguments in place, adopts object arguments, and reports a thrown
//! exception through `{prefix}_error_set` with the foreign code instead of
//! unwinding. A void method is a `NativeCallable.isolateGroupBound`
//! forwarder: it may run on any thread, copies its arguments into a message
//! for the isolate, and returns at once; the dispatcher then runs the
//! implementation on the event loop.

use crate::codegen::CodeWriter;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding, Ty};
use weaveffi_model::plan::ArgPass;

use crate::targets::dart::calls::{adopt_expr, decoder};
use crate::targets::dart::docs::{write_deprecated, write_doc};
use crate::targets::dart::types::{
    dart_class, dart_ident, dart_type, default_literal, ffi_type, ffi_typedef, object_class,
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

/// Render one callback interface.
pub(crate) fn render_callback_interface(out: &mut String, cb: &CallbackInterfaceBinding) {
    let class = dart_class(&cb.name);
    let mut w = CodeWriter::two_space();
    render_abstract_class(&mut w, cb, &class);
    render_vtable_struct(&mut w, cb, &class);
    for m in &cb.methods {
        if m.ret.is_some() {
            render_trampoline(&mut w, cb, m, &class);
        } else {
            render_forwarder(&mut w, cb, m, &class);
        }
    }
    render_dispatch(&mut w, cb, &class);
    render_vtable(&mut w, cb, &class);
    out.push_str(&w.finish());
}

/// The class a consumer extends or implements.
fn render_abstract_class(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, class: &str) {
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
    w.line("/// thrown exception fails that call with [NativeException.foreignCode].");
    w.line("/// Void methods may be called from any thread: they're delivered");
    w.line("/// asynchronously on this isolate's event loop, in the zone that passed");
    w.line("/// the instance, where a thrown exception is an uncaught error of that");
    w.line("/// zone. Object arguments are owned by the implementation.");
    write_deprecated(w, &cb.deprecated);
    w.block(format!("abstract class {class} {{"), "}", |w| {
        for (i, m) in cb.methods.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            write_doc(w, &m.doc);
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

/// The `Struct` whose layout is the C vtable: one function pointer per
/// method in declaration order, then `free`.
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
            for m in &cb.methods {
                w.line(format!(
                    "external Pointer<NativeFunction<{}>> {};",
                    entry_typedef(cb, m),
                    dart_ident(&m.name)
                ));
            }
            w.line("external Pointer<NativeFunction<Void Function(Pointer<Void>)>> free;");
        },
    );
}

/// The `(dart type, dart name)` pairs of one vtable entry's parameters.
fn entry_params(m: &CallbackMethodBinding) -> Vec<String> {
    m.abi_params
        .iter()
        .map(|p| format!("{} {}", ffi_type(&p.ty).1, dart_ident(&p.name)))
        .collect()
}

/// A value-returning method's `isolateLocal` trampoline: adopt object
/// arguments first (so a later failure still leaves each reference owned by
/// a finalizable wrapper), receive the borrowed ones in place, call the
/// implementation, and return its value. A thrown exception is reported
/// through `_foreignError` and the zero value is returned.
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    class: &str,
) {
    let ret = m.ret.as_ref().expect("value-returning method");
    let (_, dart_ret) = ffi_type(&m.abi_ret);
    w.blank();
    w.line(format!("// Vtable entry `{}.{}`.", cb.vtable_tag, m.name));
    w.block(
        format!(
            "{dart_ret} {}({}) {{",
            entry_fn(class, &m.name),
            entry_params(m).join(", ")
        ),
        "}",
        |w| {
            w.line("try {");
            w.scope(|w| {
                let mut args = Vec::new();
                for p in &m.params {
                    let base = dart_ident(&p.name);
                    let slot = dart_ident(&p.abi[0].name);
                    args.push(match p.arg_pass() {
                        ArgPass::Direct { .. } => match &p.ty {
                            Ty::Enum(n) => format!("{}.fromValue({slot})", dart_class(n)),
                            _ => slot,
                        },
                        ArgPass::String { len, .. } => {
                            format!("_readString({slot}, {})", dart_ident(&len.name))
                        }
                        ArgPass::Bytes { len, .. } => {
                            format!("_copyBytes({slot}, {})", dart_ident(&len.name))
                        }
                        ArgPass::Buffer { len, .. } => format!(
                            "_decode(_copyBytes({slot}, {}), {})",
                            dart_ident(&len.name),
                            decoder(&p.ty)
                        ),
                        ArgPass::Object { nullable, .. } => {
                            w.line(format!(
                                "final _{base} = {};",
                                adopt_expr(&slot, &p.ty, nullable)
                            ));
                            format!("_{base}")
                        }
                        ArgPass::Callback { .. } => {
                            unreachable!("callback methods never take callback interfaces")
                        }
                    });
                }
                let call = format!(
                    "(_callbackTarget(ctx) as {class}).{}({})",
                    dart_ident(&m.name),
                    args.join(", ")
                );
                match ret {
                    Ty::Enum(_) => w.line(format!("return {call}.value;")),
                    _ => w.line(format!("return {call};")),
                };
            });
            w.line("} catch (e) {");
            w.scope(|w| {
                w.line("_foreignError(outErr, e);");
                w.line(format!("return {};", default_literal(ret)));
            });
            w.line("}");
        },
    );
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
        format!(
            "void {}({}) {{",
            entry_fn(class, &m.name),
            entry_params(m).join(", ")
        ),
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
                                Ty::Bool => format!("..boolean({slot})"),
                                Ty::F32 | Ty::F64 => format!("..float64({slot})"),
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
                        let args: Vec<String> = (0..m.params.len())
                            .map(|i| format!("_a{}", i + 2))
                            .collect();
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

/// Decode forwarded argument `i` of `message` into the local `_a{i}`.
fn message_arg(w: &mut CodeWriter, p: &ParamBinding, i: usize) {
    let field = format!("message[{i}]!");
    let local = format!("_a{i}");
    let value = match p.arg_pass() {
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Enum(n) => format!("{}.fromValue({field} as int)", dart_class(n)),
            ty => format!("{field} as {}", dart_type(ty)),
        },
        ArgPass::String { .. } => format!("utf8.decode({field} as Uint8List)"),
        ArgPass::Bytes { .. } => format!("{field} as Uint8List"),
        ArgPass::Buffer { .. } => {
            format!("_decode({field} as Uint8List, {})", decoder(&p.ty))
        }
        ArgPass::Object { nullable, .. } => {
            let address = format!("{local}Address");
            w.line(format!("final {address} = {field} as int;"));
            if nullable {
                format!(
                    "{address} == 0 ? null : {}._(Pointer<Void>.fromAddress({address}))",
                    object_class(&p.ty)
                )
            } else {
                format!(
                    "{}._(Pointer<Void>.fromAddress({address}))",
                    object_class(&p.ty)
                )
            }
        }
        ArgPass::Callback { .. } => {
            unreachable!("callback methods never take callback interfaces")
        }
    };
    w.line(format!("final {local} = {value};"));
}

/// The one process-wide vtable every instance passed to the producer
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
                    for m in &cb.methods {
                        let td = entry_typedef(cb, m);
                        let entry = entry_fn(class, &m.name);
                        let callable = match &m.ret {
                            Some(ret) => format!(
                                "NativeCallable<{td}>.isolateLocal({entry}, exceptionalReturn: {})",
                                default_literal(ret)
                            ),
                            None => format!("NativeCallable<{td}>.isolateGroupBound({entry})"),
                        };
                        w.line(format!("..{} = _pin({callable})", dart_ident(&m.name)));
                    }
                    w.line("..free = _callbackFree;");
                });
            });
            w.line("return vtable;");
        },
    );
}
