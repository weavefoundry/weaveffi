//! Callback interfaces: the abstract base class the consumer subclasses,
//! and the ABI side the producer calls through: one `CFUNCTYPE` per method,
//! the `ctypes.Structure` mirroring the C vtable (its `size`, `flags`, and
//! `free` header first), one trampoline per method, and the single static
//! vtable whose function objects are pinned at module scope.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::CType;
use weaveffi_model::model::{
    CallbackInterfaceBinding, CallbackMethodBinding, CallbackParamBinding,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};

use crate::targets::python::calls::{adopt_expr, cfunctype, direct_value, slot_params};
use crate::targets::python::codec::{decode_expr, encode_stmts};
use crate::targets::python::docs::{fn_docstring, ParamDoc};
use crate::targets::python::types::{
    int_checker, prim_kind, py_local, py_member_name, py_name, py_slot_hint, py_type_hint, Dir,
};
use crate::targets::python::Gen;

/// Render one callback interface: the ABC, the vtable layout, the
/// trampolines, and the static vtable.
///
/// Each trampoline looks its implementation up by the integer `ctx`,
/// converts the arguments, calls the method, and hands the return back
/// through the C return or the out slots. An exception is reported through
/// `out_err` (see [`fail_call`]) and nothing unwinds through the C frame.
/// `ctypes` acquires the GIL on entry, so the producer may call from any
/// thread; the vtable's `flags` are 0 (not thread-affine).
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    cb: &CallbackInterfaceBinding,
) {
    let name = &cb.name;

    w.blank().blank();
    w.line(format!("class {name}(abc.ABC):"));
    w.scope(|w| {
        let usage = format!(
            "Subclass it and implement every method to pass an instance where the\n\
             API takes a `{name}`. The producer may call the methods from any\n\
             thread until it releases the instance. An exception a method raises\n\
             reaches the producer as a failure of that call (a method that throws\n\
             an error domain reports that domain's codes with their fields)."
        );
        let doc = match g.doc(&cb.doc, &cb.deprecated) {
            Some(d) => format!("{d}\n\n{usage}"),
            None => usage,
        };
        fn_docstring(w, Some(&doc), &[], &[]);
        for m in &cb.methods {
            let mut sig: Vec<String> = vec!["self".into()];
            sig.extend(
                m.params
                    .iter()
                    .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty, Dir::Out))),
            );
            let ret = m
                .ret
                .as_ref()
                .map_or_else(|| "None".into(), |t| py_type_hint(t, Dir::In));
            w.blank();
            w.line("@abc.abstractmethod");
            w.line(format!(
                "def {}({}) -> {ret}:",
                py_member_name(&m.name),
                sig.join(", "),
            ));
            w.scope(|w| {
                let doc = g.doc(&m.doc, &m.deprecated);
                let raises = callback_raises(g, &m.error);
                let params: Vec<ParamDoc> = m
                    .params
                    .iter()
                    .map(|p| ParamDoc {
                        name: py_name(&p.name),
                        hint: py_type_hint(&p.ty, Dir::Out),
                        doc: g.text(&p.doc),
                    })
                    .collect();
                if doc.is_none() && raises.is_empty() && params.iter().all(|p| p.doc.is_none()) {
                    w.line("...");
                } else {
                    fn_docstring(w, doc.as_deref(), &params, &raises);
                }
            });
        }
    });

    // One function-pointer type per method, then the vtable layout: the
    // header (`size`, `flags`, `free`), then the methods in declaration
    // order.
    w.blank().blank();
    for m in &cb.methods {
        w.line(format!(
            "_{name}_{}_t = {}",
            m.name,
            cfunctype(&m.abi.ret, &m.abi.params)
        ));
    }
    w.blank().blank();
    w.line(format!("class _{name}Vtable(ctypes.Structure):"));
    w.scope(|w| {
        w.line(format!("\"\"\"The C vtable `{}`.\"\"\"", cb.vtable_tag));
        w.blank();
        w.line("_fields_ = [");
        w.scope(|w| {
            w.line("(\"size\", ctypes.c_uint32),");
            w.line("(\"flags\", ctypes.c_uint32),");
            w.line("(\"free\", _CallbackFree),");
            for m in &cb.methods {
                w.line(format!("(\"{0}\", _{name}_{0}_t),", m.name));
            }
        });
        w.line("]");
    });

    for m in &cb.methods {
        render_trampoline(w, g, cb, m);
    }

    // The one static vtable. Each field keeps its function object alive, and
    // the vtable itself lives at module scope for the process lifetime.
    w.blank().blank();
    w.line(format!("_{name}_vtable = _{name}Vtable("));
    w.scope(|w| {
        w.line(format!("ctypes.sizeof(_{name}Vtable),"));
        w.line("0,  # flags: callable from any thread");
        w.line("_callback_free_fn,");
        for m in &cb.methods {
            w.line(format!("_{name}_{0}_t(_{name}_{0}),", m.name));
        }
    });
    w.line(")");
    w.line(format!(
        "_{name}_vtable_ptr = ctypes.addressof(_{name}_vtable)"
    ));
}

/// The `Raises` entries documenting what an implementation may raise to
/// report a typed failure.
fn callback_raises(g: &Gen<'_>, error: &ErrorStrategy) -> Vec<(String, String)> {
    match error {
        ErrorStrategy::Domain(name) => vec![(
            g.domain_class(name).to_string(),
            "To report one of the domain's codes, with its fields, to the producer.".into(),
        )],
        ErrorStrategy::Untyped | ErrorStrategy::Trap => vec![],
    }
}

/// The expression converting one trampoline parameter's C slots into the
/// value handed to the implementation. Strings, bytes, buffers, and typed
/// arrays are borrowed for the call and copied; an object transfers one
/// strong reference, which a new wrapper adopts.
fn trampoline_arg(p: &CallbackParamBinding) -> String {
    let local = |slot: &weaveffi_model::abi::AbiParam| py_local(&slot.name);
    match &p.pass {
        ArgPass::Direct { slot } => direct_value(&local(slot), &slot.ty),
        ArgPass::OptDirect { has, value, .. } => format!(
            "{} if {} else None",
            direct_value(&local(value), &value.ty),
            local(has)
        ),
        ArgPass::Slice { ptr, len, elem } => format!(
            "_peek_array({}, {}, \"{}\")",
            local(ptr),
            local(len),
            prim_kind(*elem)
        ),
        ArgPass::String { ptr, len } => {
            format!(
                "_peek_bytes({}, {}).decode(\"utf-8\")",
                local(ptr),
                local(len)
            )
        }
        ArgPass::Bytes { ptr, len } => format!("_peek_bytes({}, {})", local(ptr), local(len)),
        ArgPass::Buffer { ptr, len } => decode_expr(
            &format!("_peek_bytes({}, {})", local(ptr), local(len)),
            &p.ty,
        ),
        ArgPass::Object {
            slot,
            nullable,
            interface,
        } => adopt_expr(interface, &local(slot), *nullable),
        ArgPass::Callback { .. } => {
            unreachable!("validation rejects a callback as a callback method parameter")
        }
    }
}

/// The statement reporting the caught exception `exc` through `out_err`:
/// a method that throws a domain reports an exception of that domain as its
/// code with its fields, and anything else as code -1 with the message
/// (`throws` a domain or `any`) or -4 (no `throws`).
fn fail_call(g: &Gen<'_>, error: &ErrorStrategy) -> String {
    match error {
        ErrorStrategy::Trap => "_callback_fail(out_err, exc, _FOREIGN)".into(),
        ErrorStrategy::Untyped => "_callback_fail(out_err, exc, _GENERIC)".into(),
        ErrorStrategy::Domain(name) => format!(
            "_callback_fail(out_err, exc, _GENERIC, {})",
            g.domain_class(name)
        ),
    }
}

/// The expression checking a direct value `expr` an implementation returned
/// for a slot of C type `ty`, inside the trampoline's `try` (so a wrong type
/// or an out-of-range integer is reported like any other failure, instead
/// of `ctypes` truncating it or failing after the trampoline returned).
fn returned_scalar(expr: &str, ty: &CType, what: &str) -> String {
    match (int_checker(ty), ty) {
        (Some(check), _) => format!("{check}({expr}, \"{what}\")"),
        (None, CType::Bool) => format!("bool({expr})"),
        (None, _) => format!("_float({expr}, \"{what}\")"),
    }
}

/// Render the trampoline for one callback method: a `def` whose parameters
/// are the vtable entry's C slots (`ctx`, the parameter slots, the return's
/// out slots, `out_err`).
fn render_trampoline(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) {
    let what = format!("{}.{}() result", cb.name, py_member_name(&m.name));
    w.blank().blank();
    w.line(format!(
        "def _{}_{}({}) -> {}:",
        cb.name,
        m.name,
        slot_params(&m.abi.params),
        py_slot_hint(&m.abi.ret)
    ));
    w.scope(|w| {
        w.line("try:");
        w.scope(|w| {
            // Arguments carrying objects (one, or tokens in a buffer) are
            // adopted first, so a failure converting another argument (or
            // finding the implementation) still releases them.
            let args: Vec<String> = m
                .params
                .iter()
                .map(|p| {
                    if p.ty.contains_object() {
                        let local = format!("_{}", py_name(&p.name));
                        w.line(format!("{local} = {}", trampoline_arg(p)));
                        local
                    } else {
                        trampoline_arg(p)
                    }
                })
                .collect();
            let call = format!(
                "_callback_get(ctx).{}({})",
                py_member_name(&m.name),
                args.join(", ")
            );
            match &m.ret_pass {
                CallbackRetPass::Void => {
                    w.line(call);
                }
                CallbackRetPass::Direct => {
                    w.line(format!(
                        "return {}",
                        returned_scalar(&call, &m.abi.ret, &what)
                    ));
                }
                CallbackRetPass::OptDirect { out_value } => {
                    let pointee = match &out_value.ty {
                        CType::Ptr { pointee, .. } => pointee.as_ref(),
                        other => other,
                    };
                    w.line(format!("_ret = {call}"));
                    w.line("if _ret is None:");
                    w.scope(|w| {
                        w.line("return False");
                    });
                    w.line(format!(
                        "{}[0] = {}",
                        py_local(&out_value.name),
                        returned_scalar("_ret", pointee, &what)
                    ));
                    w.line("return True");
                }
                CallbackRetPass::Slice {
                    out_ptr,
                    out_len,
                    elem,
                } => {
                    w.line(format!(
                        "_callback_return_array({}, {}, {call}, \"{}\", \"{what}\")",
                        py_local(&out_ptr.name),
                        py_local(&out_len.name),
                        prim_kind(*elem)
                    ));
                }
                CallbackRetPass::String { out_ptr, out_len } => {
                    w.line(format!(
                        "_callback_return_bytes({}, {}, {call}.encode(\"utf-8\"))",
                        py_local(&out_ptr.name),
                        py_local(&out_len.name)
                    ));
                }
                CallbackRetPass::Bytes { out_ptr, out_len } => {
                    w.line(format!(
                        "_callback_return_bytes({}, {}, bytes({call}))",
                        py_local(&out_ptr.name),
                        py_local(&out_len.name)
                    ));
                }
                CallbackRetPass::Buffer { out_ptr, out_len } => {
                    let ty = m.ret.as_ref().expect("a buffered return has a type");
                    w.line(format!("_ret = {call}"));
                    for line in encode_stmts("_w", "_ret", ty) {
                        w.line(line);
                    }
                    w.line(format!(
                        "_callback_return_bytes({}, {}, _w.finish())",
                        py_local(&out_ptr.name),
                        py_local(&out_len.name)
                    ));
                }
                CallbackRetPass::Object {
                    nullable,
                    interface,
                    ..
                } => {
                    let helper = if *nullable {
                        "_callback_return_object_opt"
                    } else {
                        "_callback_return_object"
                    };
                    w.line(format!("return {helper}({call}, {interface})"));
                }
            }
        });
        w.line("except BaseException as exc:");
        w.scope(|w| {
            w.line(fail_call(g, &m.error));
            // The C return after a failure: the producer ignores it.
            let fallback = match &m.abi.ret {
                CType::Void => None,
                CType::Bool => Some("False"),
                CType::Float | CType::Double => Some("0.0"),
                ty if py_slot_hint(ty) == "int" => Some("0"),
                _ => Some("None"),
            };
            if let Some(value) = fallback {
                w.line(format!("return {value}"));
            }
        });
    });
}
