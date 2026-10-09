//! Callback-interface rendering: the abstract class a consumer implements,
//! the `Struct` mirroring the C vtable (the `size`/`flags`/`free` header,
//! then one entry per method), the vtable entries, the dispatcher that
//! delivers forwarded void methods, and the vtable itself.
//!
//! A method that returns a value (its [`CallbackRetPass`] isn't `Void`) is a
//! `NativeCallable.isolateLocal` trampoline: it runs synchronously on the
//! isolate's thread, receives its borrowed arguments in place, adopts object
//! arguments, and hands its return to the producer (by value, as a presence
//! flag and an out value, as a fresh object reference, or as a
//! `{prefix}_alloc` run in the out slots). The vtable is flagged
//! thread-affine, so the producer refuses to call such a method from any
//! other thread (failing it with -4) instead of letting the VM abort. A
//! thrown exception never unwinds: a method that throws a domain reports
//! its code and payload, and anything else is reported with a runtime code.
//! A void method is a `NativeCallable.isolateGroupBound` forwarder: it may
//! run on any thread, copies its arguments into a message for the isolate,
//! and returns at once; the dispatcher then runs the implementation on the
//! event loop.

use crate::codegen::CodeWriter;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{
    CallbackInterfaceBinding, CallbackMethodBinding, CallbackParamBinding, Model,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::dart::calls::{receive, Arrival, Throws};
use crate::targets::dart::codec::{pack_fn, unpack_fn};
use crate::targets::dart::docs::Docs;
use crate::targets::dart::entities::payload_fn;
use crate::targets::dart::types::{
    dart_class, dart_ident, dart_in_type, dart_type, ffi_type, ffi_typedef, object_class, slice_fn,
    typed_data_kind, zero_literal,
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

/// Whether a method returns a value to the producer (a non-void C return or
/// a return out slot), which makes it a synchronous, thread-affine
/// trampoline.
pub(crate) fn returns_value(m: &CallbackMethodBinding) -> bool {
    !matches!(m.ret_pass, CallbackRetPass::Void)
}

/// Render one callback interface.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    cb: &CallbackInterfaceBinding,
) {
    let class = dart_class(&cb.name);
    render_abstract_class(w, model, docs, cb, &class);
    render_vtable_struct(w, cb, &class);
    for m in &cb.methods {
        if returns_value(m) {
            render_trampoline(w, model, cb, m, &class);
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
    model: &Model,
    docs: &Docs,
    cb: &CallbackInterfaceBinding,
    class: &str,
) {
    w.blank();
    docs.write(w, &cb.doc);
    if cb.doc.is_some() {
        w.line("///");
    }
    for line in [
        "A callback interface: implement it and pass an instance to any",
        &format!("function taking a [{class}]. The library keeps the instance alive"),
        "until the producer releases it.",
        "",
        "Methods that return a value run synchronously on this isolate's",
        "thread, during the call from Dart that led the producer to call them.",
        "The producer refuses to call one from any other thread: the method",
        "doesn't run, and the producer sees it fail with",
        "[NativeException.foreignCode]. An exception a method throws fails the",
        "producer's call (see each method). Void methods may be called from any",
        "thread: they're delivered asynchronously on this isolate's event loop,",
        "in the zone that passed the instance, where a thrown exception is an",
        "uncaught error of that zone. Object arguments are owned by the",
        "implementation.",
    ] {
        w.line(if line.is_empty() {
            "///".to_string()
        } else {
            format!("/// {line}")
        });
    }
    docs.write_deprecated(w, &cb.deprecated);
    w.block(format!("abstract class {class} {{"), "}", |w| {
        for (i, m) in cb.methods.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            docs.write(w, &m.doc);
            if returns_value(m) {
                let note = match Throws::of(model, &m.error) {
                    Throws::Domain(exc) => format!(
                        "Throw a [{exc}] to fail the producer's call with its code and fields."
                    ),
                    Throws::Untyped => {
                        "Throw any exception to fail the producer's call with its message.".into()
                    }
                    Throws::Trap => {
                        "An exception fails the producer's call with [NativeException.foreignCode]."
                            .into()
                    }
                };
                if m.doc.is_some() {
                    w.line("///");
                }
                w.line(format!("/// {note}"));
            }
            docs.write_deprecated(w, &m.deprecated);
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{} {}", dart_type(&p.ty), dart_ident(&p.name)))
                .collect();
            let ret = m.ret.as_ref().map_or("void".to_string(), dart_in_type);
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
        let natives: Vec<String> = m.abi.params.iter().map(|p| ffi_type(&p.ty).0).collect();
        w.line(format!(
            "typedef {} = {} Function({});",
            entry_typedef(cb, m),
            ffi_type(&m.abi.ret).0,
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

/// The Dart parameter list of one vtable entry, named after its C slots.
fn entry_params(m: &CallbackMethodBinding) -> String {
    m.abi
        .params
        .iter()
        .map(|p| format!("{} {}", ffi_type(&p.ty).1, dart_ident(&p.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The Dart name of the entry's `out_err` slot (always last).
fn out_err(m: &CallbackMethodBinding) -> String {
    dart_ident(&m.abi.params.last().expect("out_err").name)
}

/// How a borrowed callback argument arrives, per its [`ArgPass`].
fn arrival(p: &CallbackParamBinding) -> Arrival {
    let n = |s: &weaveffi_model::abi::AbiParam| dart_ident(&s.name);
    match &p.pass {
        ArgPass::Direct { slot } => Arrival::Direct(n(slot)),
        ArgPass::OptDirect { has, value, .. } => Arrival::OptDirect {
            has: n(has),
            value: n(value),
        },
        ArgPass::Slice { ptr, len, elem } => Arrival::Slice {
            ptr: n(ptr),
            len: n(len),
            elem: *elem,
        },
        ArgPass::String { ptr, len } => Arrival::String {
            ptr: n(ptr),
            len: n(len),
        },
        ArgPass::Bytes { ptr, len } => Arrival::Bytes {
            ptr: n(ptr),
            len: n(len),
        },
        ArgPass::Buffer { ptr, len } => Arrival::Buffer {
            ptr: n(ptr),
            len: n(len),
        },
        ArgPass::Object { slot, nullable, .. } => Arrival::Object {
            ptr: n(slot),
            nullable: *nullable,
        },
        ArgPass::Callback { .. } => {
            unreachable!("callback methods never take callback interfaces")
        }
    }
}

/// Whether an argument is received into a local first: objects (adopted
/// references) and buffers (which may carry object tokens), so each
/// reference is owned by a finalizable wrapper even if a later step throws.
fn owns_references(p: &CallbackParamBinding) -> bool {
    matches!(p.pass, ArgPass::Object { .. } | ArgPass::Buffer { .. })
}

/// A value-returning method's `isolateLocal` trampoline: adopt object
/// arguments first (so a later failure still leaves each reference owned by
/// a finalizable wrapper), receive the borrowed ones in place, call the
/// implementation, and hand its value to the producer. A thrown exception is
/// reported through `out_err` and the zero value is returned.
fn render_trampoline(
    w: &mut CodeWriter,
    model: &Model,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    class: &str,
) {
    let throws = &Throws::of(model, &m.error);
    let (_, dart_ret) = ffi_type(&m.abi.ret);
    let out_err = out_err(m);
    let n = |s: &weaveffi_model::abi::AbiParam| dart_ident(&s.name);
    let zero = match (&m.ret_pass, &m.ret) {
        (CallbackRetPass::Direct, Some(ret)) => Some(zero_literal(ret)),
        (CallbackRetPass::OptDirect { .. }, _) => Some("false"),
        (CallbackRetPass::Object { .. }, _) => Some("nullptr"),
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
                let mut args = vec![String::new(); m.params.len()];
                let first = m
                    .params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| owns_references(p));
                for (i, p) in first {
                    let local = format!("_{}", dart_ident(&p.name));
                    w.line(format!(
                        "final {local} = {};",
                        receive(&p.ty, arrival(p), false)
                    ));
                    args[i] = local;
                }
                for (i, p) in m.params.iter().enumerate() {
                    if !owns_references(p) {
                        args[i] = receive(&p.ty, arrival(p), false);
                    }
                }
                let call = format!(
                    "(_callbackTarget(ctx) as {class}).{}({})",
                    dart_ident(&m.name),
                    args.join(", ")
                );
                let enum_value = |ty: Option<&Ty>| match ty {
                    Some(Ty::Enum(_)) => ".value",
                    _ => "",
                };
                match &m.ret_pass {
                    CallbackRetPass::Void => unreachable!("void methods are forwarded"),
                    CallbackRetPass::Direct => {
                        w.line(format!("return {call}{};", enum_value(m.ret.as_ref())));
                    }
                    CallbackRetPass::OptDirect { out_value } => {
                        let inner = match &m.ret {
                            Some(Ty::Optional(inner)) => Some(inner.as_ref()),
                            _ => None,
                        };
                        w.line(format!("final _result = {call};"));
                        w.line("if (_result == null) return false;");
                        w.line(format!(
                            "{}.value = _result{};",
                            n(out_value),
                            enum_value(inner)
                        ));
                        w.line("return true;");
                    }
                    CallbackRetPass::Slice {
                        out_ptr,
                        out_len,
                        elem,
                    } => {
                        w.line(format!("final _result = {call};"));
                        w.line(format!(
                            "{}(_result, {}, {});",
                            slice_fn("handOver", *elem),
                            n(out_ptr),
                            n(out_len)
                        ));
                    }
                    CallbackRetPass::String { out_ptr, out_len } => {
                        w.line(format!("final _result = {call};"));
                        w.line(format!(
                            "_handOver(utf8.encode(_result), {}, {});",
                            n(out_ptr),
                            n(out_len)
                        ));
                    }
                    CallbackRetPass::Bytes { out_ptr, out_len } => {
                        w.line(format!("final _result = {call};"));
                        w.line(format!(
                            "_handOver(_result, {}, {});",
                            n(out_ptr),
                            n(out_len)
                        ));
                    }
                    CallbackRetPass::Buffer { out_ptr, out_len } => {
                        let ret = m.ret.as_ref().expect("a buffer return");
                        w.line(format!("final _result = {call};"));
                        w.line(format!(
                            "_handOver(_encode(_result, {}), {}, {});",
                            pack_fn(ret),
                            n(out_ptr),
                            n(out_len)
                        ));
                    }
                    // A fresh strong reference the producer adopts.
                    CallbackRetPass::Object { nullable, .. } => {
                        w.line(format!("final _result = {call};"));
                        w.line(if *nullable {
                            "return _result?._cloneRef() ?? nullptr;"
                        } else {
                            "return _result._cloneRef();"
                        });
                    }
                }
            });
            match throws {
                Throws::Domain(exc) => {
                    let payload = if domain_has_fields(model, &m.error) {
                        format!("{}(e)", payload_fn(exc))
                    } else {
                        "null".into()
                    };
                    w.line(format!("}} on {exc} catch (e) {{"));
                    w.scope(|w| {
                        w.line(format!(
                            "_failCallback({out_err}, e.code, e.message, {payload});"
                        ));
                    });
                    w.line("} catch (e) {");
                    w.scope(|w| {
                        w.line(format!(
                            "_reportCallbackError({out_err}, NativeException.genericCode, e);"
                        ));
                    });
                }
                Throws::Untyped => {
                    w.line("} catch (e) {");
                    w.scope(|w| {
                        w.line(format!(
                            "_reportCallbackError({out_err}, NativeException.genericCode, e);"
                        ));
                    });
                }
                Throws::Trap => {
                    w.line("} catch (e) {");
                    w.scope(|w| {
                        w.line(format!(
                            "_reportCallbackError({out_err}, NativeException.foreignCode, e);"
                        ));
                    });
                }
            }
            w.line("}");
            if let Some(zero) = zero {
                w.line(format!("return {zero};"));
            }
        },
    );
}

/// The `_CallbackMessage` cascade step posting one argument of a forwarded
/// void method.
fn post_arg(p: &CallbackParamBinding) -> String {
    let n = |s: &weaveffi_model::abi::AbiParam| dart_ident(&s.name);
    let scalar = |ty: &Ty| match ty {
        Ty::Prim(Prim::Bool) => "Bool",
        Ty::Prim(Prim::F32 | Prim::F64) => "Float64",
        _ => "Int64",
    };
    match &p.pass {
        ArgPass::Direct { slot } => match scalar(&p.ty) {
            "Bool" => format!("..boolean({})", n(slot)),
            "Float64" => format!("..float64({})", n(slot)),
            _ => format!("..int64({})", n(slot)),
        },
        ArgPass::OptDirect { has, value, inner } => {
            format!("..maybe{}({}, {})", scalar(inner), n(has), n(value))
        }
        ArgPass::Slice { ptr, len, elem } => format!(
            "..typedData({}, {}, _CallbackMessage.{})",
            n(ptr),
            n(len),
            typed_data_kind(*elem)
        ),
        ArgPass::String { ptr, len }
        | ArgPass::Bytes { ptr, len }
        | ArgPass::Buffer { ptr, len } => {
            format!("..bytes({}, {})", n(ptr), n(len))
        }
        ArgPass::Object { slot, .. } => format!("..pointer({})", n(slot)),
        ArgPass::Callback { .. } => {
            unreachable!("callback methods never take callback interfaces")
        }
    }
}

/// A void method's `isolateGroupBound` forwarder: it runs outside the
/// isolate, so it only copies its slots into a message (borrowed runs and
/// arrays are copied by the post) and touches no Dart globals.
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
                        w.line(post_arg(p));
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
            if cb.methods.iter().all(returns_value) {
                w.line(format!(
                    "// Every {class} method returns a value: nothing is forwarded."
                ));
                return;
            }
            w.line(format!("final target = impl as {class};"));
            w.block("switch (method) {", "}", |w| {
                for (index, m) in cb.methods.iter().enumerate() {
                    if returns_value(m) {
                        continue;
                    }
                    // Object arguments are adopted first, so a failure while
                    // decoding the rest still leaves each transferred
                    // reference owned by a finalizable wrapper.
                    w.block(format!("case {index}: {{"), "}", |w| {
                        let is_object =
                            |p: &CallbackParamBinding| matches!(p.pass, ArgPass::Object { .. });
                        for (i, p) in m.params.iter().enumerate().filter(|(_, p)| is_object(p)) {
                            message_arg(w, p, i + 2);
                        }
                        for (i, p) in m.params.iter().enumerate().filter(|(_, p)| !is_object(p)) {
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
fn message_arg(w: &mut CodeWriter, p: &CallbackParamBinding, i: usize) {
    let field = format!("message[{i}]");
    let local = format!("a{i}");
    let value = match &p.pass {
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Enum(n) => format!("{}.fromValue({field}! as int)", dart_class(n)),
            ty => format!("{field}! as {}", dart_type(ty)),
        },
        ArgPass::OptDirect { inner, .. } => match inner {
            Ty::Enum(n) => {
                let raw = format!("{local}Value");
                w.line(format!("final {raw} = {field} as int?;"));
                format!("{raw} == null ? null : {}.fromValue({raw})", dart_class(n))
            }
            other => format!("{field} as {}?", dart_type(other)),
        },
        ArgPass::Slice { .. } => format!("{field}! as {}", dart_type(&p.ty)),
        ArgPass::String { .. } => format!("utf8.decode({field}! as Uint8List)"),
        ArgPass::Bytes { .. } => format!("{field}! as Uint8List"),
        ArgPass::Buffer { .. } => {
            format!("_decode({field}! as Uint8List, {})", unpack_fn(&p.ty))
        }
        ArgPass::Object {
            nullable,
            interface,
            ..
        } => {
            let address = format!("{local}Address");
            w.line(format!("final {address} = {field}! as int;"));
            let adopt = format!(
                "{}._(Pointer<Void>.fromAddress({address}))",
                object_class(interface)
            );
            if *nullable {
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
/// shares; the per-instance state travels in `ctx`. It's flagged
/// thread-affine (DESIGN 1.7): only this isolate's thread may call a
/// value-returning method.
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
                    w.line("..flags = _vtableThreadAffine");
                    w.line(if cb.methods.is_empty() {
                        "..free = _callbackFree;"
                    } else {
                        "..free = _callbackFree"
                    });
                    for (i, m) in cb.methods.iter().enumerate() {
                        let td = entry_typedef(cb, m);
                        let entry = entry_fn(class, &m.name);
                        let callable = match (&m.ret_pass, &m.ret) {
                            (CallbackRetPass::Void, _) => {
                                format!("NativeCallable<{td}>.isolateGroupBound({entry})")
                            }
                            (CallbackRetPass::Direct, Some(ret)) => format!(
                                "NativeCallable<{td}>.isolateLocal({entry}, exceptionalReturn: {})",
                                zero_literal(ret)
                            ),
                            (CallbackRetPass::OptDirect { .. }, _) => format!(
                                "NativeCallable<{td}>.isolateLocal({entry}, exceptionalReturn: false)"
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

/// Whether `error` names a domain with a code that carries fields.
fn domain_has_fields(model: &Model, error: &ErrorStrategy) -> bool {
    error.domain().is_some_and(|name| {
        model
            .error_domain(name)
            .codes
            .iter()
            .any(|c| !c.fields.is_empty())
    })
}

/// The error domains a value-returning callback method throws whose codes
/// carry fields, by domain name: each needs an encoder for those fields.
pub(crate) fn reported_domains(model: &Model) -> std::collections::BTreeSet<String> {
    model
        .callback_interfaces()
        .flat_map(|(_, cb)| &cb.methods)
        .filter(|m| returns_value(m) && domain_has_fields(model, &m.error))
        .filter_map(|m| m.error.domain().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_class() {
        assert_eq!(vtable_var("ready_listener"), "_readyListenerVtable");
        assert_eq!(dispatch_fn("Sampler"), "_samplerDispatch");
        assert_eq!(entry_fn("Sampler", "on_item"), "_samplerOnItem");
    }
}
