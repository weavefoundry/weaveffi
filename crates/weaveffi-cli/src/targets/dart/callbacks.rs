//! Callback-interface rendering: the abstract class a consumer implements,
//! the `Struct` mirroring the C vtable (the `size`/`flags`/`free` header,
//! then one entry per method), the vtable entries, the dispatcher that
//! delivers forwarded void methods, and the vtable itself.
//!
//! A method that returns a value is a `NativeCallable.isolateLocal`
//! trampoline: it runs synchronously on the isolate's thread, receives its
//! borrowed arguments in place, adopts object arguments, and returns its
//! value to the producer as one C value (a scalar, an optional-scalar
//! struct, a fresh object reference, or a `{prefix}_alloc` run). The vtable
//! is flagged thread-affine, so the producer refuses to call such a method
//! from any other thread (failing it with -4) instead of letting the VM
//! abort. A thrown exception never unwinds: a method that throws a domain
//! reports its code and payload, and anything else is reported with a
//! runtime code. A void method is a `NativeCallable.isolateGroupBound`
//! forwarder: it may run on any thread, copies its arguments into a message
//! for the isolate, and returns at once; the dispatcher then runs the
//! implementation on the event loop.

use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{
    CallbackBinding, CallbackMethodBinding, ErrorStrategy, FieldBinding, Model,
};
use weaveffi_model::ty::Prim;

use crate::targets::dart::calls::Throws;
use crate::targets::dart::codec::unpack_fn;
use crate::targets::dart::docs::Docs;
use crate::targets::dart::entities::payload_fn;
use crate::targets::dart::types::{
    dart_class, dart_ident, dart_in_type, dart_type, ffi_type, ffi_typedef, typed_data_kind,
};
use crate::targets::dart::values::{exceptional_return, hand_over, receive, zero};

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

/// Whether a method returns a value to the producer, which makes it a
/// synchronous, thread-affine trampoline.
pub(crate) fn returns_value(m: &CallbackMethodBinding) -> bool {
    m.ret.is_some()
}

/// Render one callback interface.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    cb: &CallbackBinding,
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
    cb: &CallbackBinding,
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
fn entry_typedef(cb: &CallbackBinding, m: &CallbackMethodBinding) -> String {
    ffi_typedef(&format!("{}_{}", cb.c_tag, m.name))
}

/// The `Struct` whose layout is the C vtable: the `size`, `flags`, and
/// `free` header, then one function pointer per method in declaration order.
fn render_vtable_struct(w: &mut CodeWriter, cb: &CallbackBinding, class: &str) {
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

/// Whether an argument is received into a local first: objects (adopted
/// references) and buffers (which may carry object tokens), so each
/// reference is owned by a finalizable wrapper even if a later step throws.
fn owns_references(p: &FieldBinding) -> bool {
    matches!(
        crossing(&p.ty),
        Crossing::Object { .. } | Crossing::Buffer(_)
    )
}

/// A value-returning method's `isolateLocal` trampoline: adopt object
/// arguments first (so a later failure still leaves each reference owned by
/// a finalizable wrapper), receive the borrowed ones in place, call the
/// implementation, and return its value to the producer. A thrown exception
/// is reported through `out_err` and the zero value is returned.
fn render_trampoline(
    w: &mut CodeWriter,
    model: &Model,
    cb: &CallbackBinding,
    m: &CallbackMethodBinding,
    class: &str,
) {
    let throws = &Throws::of(model, &m.error);
    let ret = m.ret.as_ref().expect("a value-returning method");
    let (_, dart_ret) = ffi_type(&m.abi.ret);
    let out_err = out_err(m);
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
                for (i, p) in m
                    .params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| owns_references(p))
                {
                    let local = format!("_{}Arg", dart_ident(&p.name));
                    w.line(format!(
                        "final {local} = {};",
                        receive(&p.ty, &dart_ident(&p.name), false)
                    ));
                    args[i] = local;
                }
                for (i, p) in m.params.iter().enumerate() {
                    if !owns_references(p) {
                        args[i] = receive(&p.ty, &dart_ident(&p.name), false);
                    }
                }
                w.line(format!(
                    "final _result = (_callbackTarget(ctx) as {class}).{}({});",
                    dart_ident(&m.name),
                    args.join(", ")
                ));
                w.line(format!("return {};", hand_over(ret, "_result")));
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
            w.line(format!("return {};", zero(ret)));
        },
    );
}

/// The `_CallbackMessage` cascade step posting the argument `p` of a
/// forwarded void method.
fn post_arg(p: &FieldBinding) -> String {
    let n = dart_ident(&p.name);
    let scalar = |prim: Prim| match prim {
        Prim::Bool => "Bool",
        Prim::F32 | Prim::F64 => "Float64",
        _ => "Int64",
    };
    match crossing(&p.ty) {
        Crossing::Scalar(Prim::Bool) => format!("..boolean({n})"),
        Crossing::Scalar(Prim::F32 | Prim::F64) => format!("..float64({n})"),
        Crossing::Scalar(_) | Crossing::Enum(_) => format!("..int64({n})"),
        Crossing::OptScalar(prim) => {
            format!("..maybe{}({n}.has, {n}.value)", scalar(prim))
        }
        Crossing::OptEnum(_) => format!("..maybeInt64({n}.has, {n}.value)"),
        Crossing::Slice(elem) => format!(
            "..array({n}, {}, _CallbackMessage.{})",
            elem.size().expect("a typed-array element is a scalar"),
            typed_data_kind(elem)
        ),
        Crossing::Utf8 | Crossing::Bytes | Crossing::Buffer(_) => format!("..run({n})"),
        Crossing::Object { .. } => format!("..pointer({n})"),
    }
}

/// A void method's `isolateGroupBound` forwarder: it runs outside the
/// isolate, so it only copies its slots into a message (borrowed runs are
/// copied by the post) and touches no Dart globals.
fn render_forwarder(
    w: &mut CodeWriter,
    cb: &CallbackBinding,
    m: &CallbackMethodBinding,
    class: &str,
) {
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
                "_CallbackMessage(ctx, {}, {})",
                m.slot,
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
fn render_dispatch(w: &mut CodeWriter, cb: &CallbackBinding, class: &str) {
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
                for m in cb.methods.iter().filter(|m| !returns_value(m)) {
                    // Object arguments are adopted first, so a failure while
                    // decoding the rest still leaves each transferred
                    // reference owned by a finalizable wrapper.
                    w.block(format!("case {}: {{", m.slot), "}", |w| {
                        let is_object =
                            |p: &FieldBinding| matches!(crossing(&p.ty), Crossing::Object { .. });
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
fn message_arg(w: &mut CodeWriter, p: &FieldBinding, i: usize) {
    let field = format!("message[{i}]");
    let local = format!("a{i}");
    let value = match crossing(&p.ty) {
        Crossing::Enum(n) => format!("{}.fromValue({field}! as int)", dart_class(n)),
        Crossing::Scalar(_) | Crossing::Slice(_) => {
            format!("{field}! as {}", dart_type(&p.ty))
        }
        Crossing::OptEnum(n) => {
            let raw = format!("{local}Value");
            w.line(format!("final {raw} = {field} as int?;"));
            format!("{raw} == null ? null : {}.fromValue({raw})", dart_class(n))
        }
        Crossing::OptScalar(_) => format!("{field} as {}", dart_type(&p.ty)),
        Crossing::Utf8 => format!("utf8.decode({field}! as Uint8List)"),
        Crossing::Bytes => format!("{field}! as Uint8List"),
        Crossing::Buffer(t) => format!("_decode({field}! as Uint8List, {})", unpack_fn(t)),
        Crossing::Object {
            interface,
            nullable,
        } => {
            let address = format!("{local}Address");
            w.line(format!("final {address} = {field}! as int;"));
            let adopt = format!(
                "{}._(Pointer<Void>.fromAddress({address}))",
                dart_class(interface)
            );
            if nullable {
                format!("{address} == 0 ? null : {adopt}")
            } else {
                adopt
            }
        }
    };
    w.line(format!("final {local} = {value};"));
}

/// The vtable every instance passed to the producer from this isolate
/// shares; the per-instance state travels in `ctx`. It's flagged
/// thread-affine: only this isolate's thread may call a value-returning
/// method.
fn render_vtable(w: &mut CodeWriter, cb: &CallbackBinding, class: &str) {
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
                    w.line("..free = _callbackFree");
                    for (i, m) in cb.methods.iter().enumerate() {
                        let td = entry_typedef(cb, m);
                        let entry = entry_fn(class, &m.name);
                        let callable = match &m.ret {
                            None => format!("NativeCallable<{td}>.isolateGroupBound({entry})"),
                            Some(ret) => match exceptional_return(ret) {
                                Some(zero) => format!(
                                    "NativeCallable<{td}>.isolateLocal({entry}, exceptionalReturn: {zero})"
                                ),
                                None => format!("NativeCallable<{td}>.isolateLocal({entry})"),
                            },
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
        .callbacks
        .iter()
        .flat_map(|cb| &cb.methods)
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
