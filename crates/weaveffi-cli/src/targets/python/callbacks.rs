//! Callback interfaces: the abstract base class the consumer subclasses,
//! and the ABI side the producer calls through: one trampoline per method,
//! the `ctypes.Structure`s mirroring the C vtable (its `size`, `flags`, and
//! `free` header first) and the argument struct (`{ ctx, vtable }`), and
//! the one vtable per interface, built the first time an implementation is
//! passed and kept for the process lifetime.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::CType;
use weaveffi_model::model::{CallbackBinding, CallbackMethodBinding, ErrorStrategy};

use crate::targets::python::calls::{producer_types, slot_params};
use crate::targets::python::docs::{fn_docstring, ParamDoc};
use crate::targets::python::types::{
    callback_arg_struct, py_local, py_member_name, py_name, py_slot_hint, py_type_hint, zero_value,
    Dir,
};
use crate::targets::python::values::{received, returned, Ownership};
use crate::targets::python::Gen;

/// Render one callback interface: the ABC, the trampolines, the vtable and
/// argument layouts, and the lazily built vtable.
///
/// Each trampoline looks its implementation up by the integer `ctx`,
/// converts the arguments, calls the method, and hands the return back as
/// the C return. An exception is reported through `out_err` (see
/// [`fail_call`]) and nothing unwinds through the C frame. `ctypes`
/// acquires the GIL on entry, so the producer may call from any thread; the
/// vtable's `flags` are 0 (not thread-affine).
pub(crate) fn render_callback_interface(w: &mut CodeWriter, g: &Gen<'_>, cb: &CallbackBinding) {
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

    for m in &cb.methods {
        render_trampoline(w, g, cb, m);
    }

    // The vtable layout: the header (`size`, `flags`, `free`), then one
    // entry per method in declaration order, each the address of a
    // function the producer calls.
    w.blank().blank();
    w.line(format!("class _{name}Vtable(ctypes.Structure):"));
    w.scope(|w| {
        w.line(format!("\"\"\"The C vtable `{}`.\"\"\"", cb.vtable_tag));
        w.blank();
        w.line("_fields_ = [");
        w.scope(|w| {
            w.line("(\"size\", ctypes.c_uint32),");
            w.line("(\"flags\", ctypes.c_uint32),");
            w.line("(\"free\", ctypes.c_void_p),");
            for m in &cb.methods {
                w.line(format!("(\"{}\", ctypes.c_void_p),", m.name));
            }
        });
        w.line("]");
    });

    let arg = callback_arg_struct(name);
    w.blank().blank();
    w.line(format!("class {arg}(ctypes.Structure):"));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"The C `{}` argument: a context and a vtable.\"\"\"",
            cb.c_tag
        ));
        w.blank();
        w.line("_fields_ = [(\"ctx\", ctypes.c_void_p), (\"vtable\", ctypes.c_void_p)]");
    });

    // The one vtable, built on first use. Each entry's function object is
    // kept by the runtime, and the vtable itself for the process lifetime.
    w.blank().blank();
    w.line(format!("def _{name}VtableBuild() -> _{name}Vtable:"));
    w.scope(|w| {
        w.line(format!("return _{name}Vtable("));
        w.scope(|w| {
            w.line(format!("ctypes.sizeof(_{name}Vtable),"));
            w.line("0,  # flags: callable from any thread");
            w.line("_callback_free_ptr,");
            for m in &cb.methods {
                let mut parts = vec![format!("_{name}_{}", m.name)];
                parts.extend(producer_types(&m.abi.ret, &m.abi.params));
                let entry = if returns_struct(m) {
                    "_vtable_struct_entry"
                } else {
                    "_vtable_entry"
                };
                w.line(format!("{entry}({}),", parts.join(", ")));
            }
        });
        w.line(")");
    });
    w.blank().blank();
    w.line(format!(
        "{} = _LazyVtable(_{name}VtableBuild)",
        lazy_vtable(name)
    ));
}

/// The module-level `_LazyVtable` of the callback interface `name`,
/// `_{name}VtableOnce` (no `_` after the name, so it never meets a
/// trampoline, `_{name}_{method}`).
pub(crate) fn lazy_vtable(name: &str) -> String {
    format!("_{name}VtableOnce")
}

/// `true` when a callback method returns a structure by value (a run or an
/// optional scalar), which a `ctypes` callback can't, so its vtable entry is
/// a libffi closure the runtime builds directly.
pub(crate) fn returns_struct(m: &CallbackMethodBinding) -> bool {
    matches!(m.abi.ret, CType::Run | CType::Opt(_))
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

/// Render the trampoline for one callback method: a `def` whose parameters
/// are the vtable entry's C slots (`ctx`, one per parameter, `out_err`) and
/// whose return is the method's one C value.
fn render_trampoline(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    cb: &CallbackBinding,
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
                .zip(&m.abi.params[1..])
                .map(|(p, slot)| {
                    let value = received(&py_local(&slot.name), &p.ty, Ownership::Borrowed);
                    if p.ty.contains_object() {
                        let local = format!("_{}", py_name(&p.name));
                        w.line(format!("{local} = {value}"));
                        local
                    } else {
                        value
                    }
                })
                .collect();
            let call = format!(
                "_callback_get(ctx).{}({})",
                py_member_name(&m.name),
                args.join(", ")
            );
            match &m.ret {
                None => {
                    w.line(call);
                }
                Some(ty) => {
                    w.line(format!("_ret = {call}"));
                    let (stmts, value) = returned("_ret", ty, &what);
                    for line in stmts {
                        w.line(line);
                    }
                    w.line(format!("return {value}"));
                }
            }
        });
        w.line("except BaseException as exc:");
        w.scope(|w| {
            w.line(fail_call(g, &m.error));
            if let Some(value) = zero_value(&m.abi.ret) {
                w.line(format!("return {value}"));
            }
        });
    });
}
