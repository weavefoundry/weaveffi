//! Callback interfaces: the abstract base class the consumer subclasses,
//! and the ABI side the producer calls through, satisfying
//! [`weaveffi_model::plan::CallbackProtocol`]: one `CFUNCTYPE` per method,
//! the `ctypes.Structure` mirroring the C vtable (its `size`, `flags`, and
//! `free` header first), one trampoline per method, and the single static
//! vtable whose function objects are pinned at module scope.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{CallbackInterfaceBinding, CallbackMethodBinding, ParamBinding};
use weaveffi_model::ty::{Family, Prim, Ty};

use crate::targets::python::calls::{adopt_expr, cfunctype, slot_params};
use crate::targets::python::codec::{decode_expr, encode_stmts};
use crate::targets::python::docs::{fn_docstring, with_deprecation};
use crate::targets::python::types::{
    py_local, py_member_name, py_name, py_return_hint, py_slot_hint, py_type_hint,
};
use crate::targets::python::Gen;

/// Render one callback interface: the ABC, the vtable layout, the
/// trampolines, and the static vtable.
///
/// Each trampoline looks its implementation up by the integer `ctx`,
/// converts the arguments, calls the method, and hands the return back
/// through the C return or the `out_ptr`/`out_len` slots. An exception is
/// reported through `out_err` (as a domain code when the method declares
/// errors and raised one, else `-4`) and nothing unwinds through the C
/// frame. `ctypes` acquires the GIL on entry, so the producer may call from
/// any thread.
pub(crate) fn render_callback_interface(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    cb: &CallbackInterfaceBinding,
    domain: Option<&str>,
) {
    let name = &cb.name;

    w.blank().blank();
    w.line(format!("class {name}(abc.ABC):"));
    w.scope(|w| {
        let mut usage = format!(
            "Subclass it and implement every method to pass an instance where the\n\
             API takes a `{name}`. The producer may call the methods from any\n\
             thread until it releases the instance."
        );
        if let Some(domain) = domain.filter(|_| cb.methods.iter().any(|m| m.throws)) {
            usage.push_str(&format!(
                " A method that declares errors may\n\
                 raise a `{domain}` code, which reaches the producer with its fields;\n\
                 any other exception reaches it as {}.FOREIGN_ERROR_CODE (-4).",
                g.root_error
            ));
        } else {
            usage.push_str(&format!(
                " An exception a method raises\n\
                 reaches the producer as {}.FOREIGN_ERROR_CODE (-4).",
                g.root_error
            ));
        }
        let doc = with_deprecation(cb.doc.as_deref(), cb.deprecated.as_deref());
        let doc = match doc {
            Some(d) => format!("{d}\n\n{usage}"),
            None => usage,
        };
        fn_docstring(w, Some(&doc), &[], None);
        for m in &cb.methods {
            let mut sig: Vec<String> = vec!["self".into()];
            sig.extend(
                m.params
                    .iter()
                    .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty))),
            );
            w.blank();
            w.line("@abc.abstractmethod");
            w.line(format!(
                "def {}({}) -> {}:",
                py_member_name(&m.name),
                sig.join(", "),
                py_return_hint(m.ret.as_ref())
            ));
            w.scope(|w| {
                let doc = with_deprecation(m.doc.as_deref(), m.deprecated.as_deref());
                let raises = domain.filter(|_| m.throws).map(|d| {
                    (
                        d,
                        "To report one of the domain's error codes to the producer.",
                    )
                });
                if doc.is_none() && raises.is_none() && m.params.iter().all(|p| p.doc.is_none()) {
                    w.line("...");
                } else {
                    fn_docstring(w, doc.as_deref(), &m.params, raises);
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
            cfunctype(&m.abi_ret, &m.abi_params)
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
        render_trampoline(w, cb, m, domain.filter(|_| m.throws));
    }

    // The one static vtable. Each field keeps its function object alive, and
    // the vtable itself lives at module scope for the process lifetime.
    w.blank().blank();
    w.line(format!("_{name}_vtable = _{name}Vtable("));
    w.scope(|w| {
        w.line(format!("ctypes.sizeof(_{name}Vtable),"));
        w.line("0,");
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

/// The expression converting one trampoline parameter's C slots into the
/// value handed to the implementation. Strings, bytes, and buffers are
/// borrowed for the call and copied; an object transfers one strong
/// reference, which a new wrapper adopts.
fn trampoline_arg(p: &ParamBinding) -> String {
    let data = || {
        format!(
            "_peek_bytes({}, {})",
            py_local(&format!("{}_ptr", p.name)),
            py_local(&format!("{}_len", p.name))
        )
    };
    let n = py_local(&p.name);
    match p.ty.family() {
        Family::String => format!("{}.decode(\"utf-8\")", data()),
        Family::Bytes => data(),
        Family::Buffer => decode_expr(&data(), &p.ty),
        Family::Object { nullable } => adopt_expr(&p.ty, &n, nullable),
        Family::Direct => match &p.ty {
            Ty::Enum(name) => format!("{name}({n})"),
            _ => n,
        },
        Family::Callback { .. } | Family::Iterator => {
            unreachable!("callback interfaces and iterators are never callback arguments")
        }
    }
}

/// `(coercion, default)` for a direct return: the conversion applied to the
/// implementation's result (inside the `try`, so a wrong type is reported
/// like any other failure), and the value returned after a failure.
fn direct_return(ty: &Ty) -> (&'static str, &'static str) {
    match ty {
        Ty::Prim(Prim::Bool) => ("bool", "False"),
        Ty::Prim(Prim::F32 | Prim::F64) => ("float", "0.0"),
        // Integers and C-style enums (an `IntEnum` is an `int`).
        _ => ("int", "0"),
    }
}

/// Render the trampoline for one callback method: a `def` whose parameters
/// are the vtable entry's C slots (`ctx`, the parameter slots, the return's
/// out slots, `out_err`). `domain` names the error domain whose codes the
/// method may report (only for a method that declares errors).
fn render_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    domain: Option<&str>,
) {
    w.blank().blank();
    w.line(format!(
        "def _{}_{}({}) -> {}:",
        cb.name,
        m.name,
        slot_params(&m.abi_params),
        py_slot_hint(&m.abi_ret)
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
            let Some(ty) = &m.ret else {
                w.line(call);
                return;
            };
            if ty.family() == Family::Direct {
                w.line(format!("return {}({call})", direct_return(ty).0));
                return;
            }
            w.line(format!("_ret = {call}"));
            match ty.family() {
                Family::Object { nullable } => {
                    let class = ty.interface_name().expect("object names an interface");
                    let helper = if nullable {
                        "_callback_return_object_opt"
                    } else {
                        "_callback_return_object"
                    };
                    w.line(format!("return {helper}(_ret, {class})"));
                }
                Family::String => {
                    w.line("_callback_return_bytes(out_ptr, out_len, _ret.encode(\"utf-8\"))");
                }
                Family::Bytes => {
                    w.line("_callback_return_bytes(out_ptr, out_len, bytes(_ret))");
                }
                Family::Buffer => {
                    for line in encode_stmts("_w", "_ret", ty) {
                        w.line(line);
                    }
                    w.line("_callback_return_bytes(out_ptr, out_len, _w.finish())");
                }
                Family::Direct => unreachable!("returned above"),
                Family::Callback { .. } | Family::Iterator => {
                    unreachable!("validation rejects {ty} as a callback return")
                }
            }
        });
        w.line("except BaseException as exc:");
        w.scope(|w| {
            match domain {
                Some(domain) => w.line(format!("_callback_fail(out_err, exc, {domain})")),
                None => w.line("_callback_fail(out_err, exc)"),
            };
            match m.ret.as_ref().map(|ty| (ty, ty.family())) {
                Some((ty, Family::Direct)) => {
                    w.line(format!("return {}", direct_return(ty).1));
                }
                Some((_, Family::Object { .. })) => {
                    w.line("return None");
                }
                _ => {}
            }
        });
    });
}
