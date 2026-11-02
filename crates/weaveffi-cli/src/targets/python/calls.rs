//! Call rendering: sync, async, and iterator wrappers and their module-level
//! `ctypes` bindings.
//!
//! Every C prototype is bound once, at import time, from the model's lowered
//! [`AbiFn`] (`argtypes` and `restype` come straight from its slots). Every
//! value is one C value, marshalled by its [`Crossing`] (see
//! [`values`](crate::targets::python::values)), and the callable's
//! [`ErrorStrategy`](weaveffi_model::model::ErrorStrategy) decides what a
//! failure raises.

use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiFn, AbiParam, CType};
use weaveffi_model::model::{AsyncBinding, FnBinding, IteratorBinding};
use weaveffi_model::ty::{ParamTy, Prim, Ty};

use crate::targets::python::callbacks::lazy_vtable;
use crate::targets::python::codec::encode_stmts;
use crate::targets::python::docs::{fn_docstring, ParamDoc};
use crate::targets::python::types::{
    callback_arg_struct, prim_kind, py_binding_name, py_ctype, py_local, py_member_name, py_name,
    py_object_member, py_out_local, py_param_hint, py_return_hint, py_slot_hint, py_str_literal,
    py_type_hint, Caller, Dir,
};
use crate::targets::python::values::{checked_scalar, opt_value, received, Ownership};
use crate::targets::python::Gen;

/// How a rendered callable is scoped and spelled in the generated Python.
#[derive(Clone, Copy)]
pub(crate) enum FnScope {
    /// A module-level free function.
    Free,
    /// An instance method on an interface wrapper: leading `self` parameter,
    /// whose pointer is lent as the leading C argument.
    Method,
    /// A `@staticmethod` member.
    Static,
    /// A `@classmethod` constructor factory returning a new wrapper instance.
    Factory,
    /// The canonical `new` constructor, emitted as `__init__`.
    Init,
}

/// `_bind("symbol", restype, argtypes...)` for one lowered function. The
/// async launcher's `callback` slot is typed with the completion's
/// `CFUNCTYPE` (`callback_type`).
fn bind_line(g: &Gen<'_>, abi: &AbiFn, callback_type: Option<&str>) -> String {
    let mut parts = vec![
        format!("\"{}\"", abi.symbol),
        py_ctype(&abi.ret, Caller::Python),
    ];
    parts.extend(abi.params.iter().map(|p| match callback_type {
        Some(cb) if p.name == "callback" => cb.to_string(),
        _ => py_ctype(&p.ty, Caller::Python),
    }));
    format!(
        "{} = _bind({})",
        py_binding_name(&abi.symbol, g.prefix),
        parts.join(", ")
    )
}

/// The `ctypes` types of a C function-pointer signature the producer calls
/// (a callback method or an async completion): the return, then each slot.
pub(crate) fn producer_types(ret: &CType, params: &[AbiParam]) -> Vec<String> {
    std::iter::once(py_ctype(ret, Caller::Producer))
        .chain(params.iter().map(|p| py_ctype(&p.ty, Caller::Producer)))
        .collect()
}

/// The annotated parameter list of a function the producer calls with the
/// C slots `params` (a callback method or an async completion).
pub(crate) fn slot_params(params: &[AbiParam]) -> String {
    let all: Vec<String> = params
        .iter()
        .map(|p| format!("{}: {}", py_local(&p.name), py_slot_hint(&p.ty)))
        .collect();
    all.join(", ")
}

/// The module-level stem of an async function's completion objects: the
/// launcher's binding name without `_c`.
fn completion_stem(g: &Gen<'_>, f: &FnBinding) -> String {
    let binding = py_binding_name(&f.abi.symbol, g.prefix);
    binding.strip_prefix("_c").unwrap_or(&binding).to_string()
}

/// The module-level function pulling one element of an iterator:
/// `_pull_` plus the iterator tag without its prefix.
fn pull_fn(g: &Gen<'_>, it: &IteratorBinding) -> String {
    py_binding_name(&it.iter_tag, g.prefix).replacen("_c_", "_pull_", 1)
}

/// The expression holding a received value of type `ty` from a `ctypes`
/// out local `local` of its C type: the structure itself for an optional
/// scalar or a run, and its `.value` for a scalar or an object address.
fn out_value(local: &str, ty: &Ty) -> String {
    match crossing(ty) {
        Crossing::OptScalar(_) | Crossing::OptEnum(_) => local.to_string(),
        c if c.is_run() => local.to_string(),
        _ => format!("{local}.value"),
    }
}

/// Emit every module-level piece a callable needs ahead of its wrapper: the
/// bound entry point, plus the iterator's bound `_next`/`_destroy` and its
/// pull function (iterator returns) or the static completion trampoline
/// (async calls). `packed` appends a plain sync member's binding to the
/// block the previous member started, with no blank lines between.
pub(crate) fn render_bindings(w: &mut CodeWriter, g: &Gen<'_>, f: &FnBinding, packed: bool) {
    if let Some(a) = f.async_binding() {
        render_completion(w, g, f, a);
        return;
    }
    let Some(it) = f.iterator() else {
        if !packed {
            w.blank().blank();
        }
        w.line(bind_line(g, &f.abi, None));
        return;
    };
    w.blank().blank();
    w.line(bind_line(g, &f.abi, None));
    w.line(bind_line(g, &it.next, None));
    w.line(format!(
        "{} = _bind(\"{}\", None, ctypes.c_void_p)",
        py_binding_name(&it.destroy_symbol, g.prefix),
        it.destroy_symbol
    ));
    render_pull(w, g, f, it);
}

/// Render the module-level function pulling one item of an iterator: one
/// producer `_next` call, the item copied out and released, and
/// `StopIteration` once the stream ends. The runtime's `NativeIterator`
/// calls it for each step and destroys the handle exactly once.
fn render_pull(w: &mut CodeWriter, g: &Gen<'_>, f: &FnBinding, it: &IteratorBinding) {
    let next = py_binding_name(&it.next.symbol, g.prefix);
    let slot = &it.next.params[1];
    w.blank().blank();
    w.line(format!(
        "def {}(_p: int) -> {}:",
        pull_fn(g, it),
        py_type_hint(&it.elem, Dir::Out)
    ));
    w.scope(|w| {
        w.line(format!("_item = {}()", py_out_local(&slot.ty)));
        w.line("_err = _ErrorStruct()");
        w.line(format!(
            "_more = {next}(_p, ctypes.byref(_item), ctypes.byref(_err))"
        ));
        w.line("if _err.code:");
        w.scope(|w| {
            w.line(format!(
                "raise {}(*_read_error(_err))",
                g.raise_factory(&f.error)
            ));
        });
        w.line("if not _more:");
        w.scope(|w| {
            w.line("raise StopIteration");
        });
        w.line(format!(
            "return {}",
            received(&out_value("_item", &it.elem), &it.elem, Ownership::Owned)
        ));
    });
}

/// Render an async call's bindings: the completion's `CFUNCTYPE`, the bound
/// launcher, and the one static completion trampoline, which takes
/// ownership of the result on the producer's thread and settles the
/// awaiting future.
fn render_completion(w: &mut CodeWriter, g: &Gen<'_>, f: &FnBinding, a: &AsyncBinding) {
    let stem = completion_stem(g, f);
    let cb_type = py_binding_name(&a.callback_type, g.prefix);
    let value = f.ret_value().map_or_else(
        || "None".into(),
        |ty| received("result", ty, Ownership::Owned),
    );
    w.blank().blank();
    w.line(format!(
        "{cb_type} = ctypes.CFUNCTYPE({})",
        producer_types(&a.callback.ret, &a.callback.params).join(", ")
    ));
    w.line(bind_line(g, &f.abi, Some(&cb_type)));
    w.blank().blank();
    w.line(format!(
        "def {stem}_complete({}) -> None:",
        slot_params(&a.callback.params)
    ));
    w.scope(|w| {
        w.line("# Runs once, on a producer thread: take ownership of the result");
        w.line("# here, then hand it to the awaiting event loop.");
        w.line("try:");
        w.scope(|w| {
            w.line("if err:");
            w.scope(|w| {
                w.line(format!(
                    "_async_settle(context, _async_error(err, {}), None)",
                    g.raise_factory(&f.error)
                ));
            });
            w.line("else:");
            w.scope(|w| {
                w.line(format!("_async_settle(context, None, {value})"));
            });
        });
        w.line("except BaseException as exc:");
        w.scope(|w| {
            w.line("_async_settle(context, exc, None)");
        });
    });
    w.blank().blank();
    w.line(format!("{stem}_completion = {cb_type}({stem}_complete)"));
}

/// The emitted Python name for a callable in `scope`.
fn py_fn_name(f: &FnBinding, scope: FnScope) -> String {
    match scope {
        FnScope::Init => "__init__".to_string(),
        FnScope::Factory | FnScope::Method | FnScope::Static => py_object_member(&f.name),
        FnScope::Free => py_member_name(&f.name),
    }
}

/// One object lent to a call: the local holding its pointer, the expression
/// lending it, and the statement returning it.
#[derive(Clone)]
struct Lend {
    local: String,
    acquire: String,
    release: String,
}

/// One parameter's preparation and its C argument.
#[derive(Default)]
struct ParamPlan {
    /// Statements run before any object is lent (encoding value buffers,
    /// checking callback implementations).
    prep: Vec<String>,
    /// A conversion that may fail, `(local, expression)`: inlined into the
    /// argument when nothing is lent or registered before the call, else
    /// bound to the local in `prep` (a failing conversion must not strand a
    /// lent object or a registration).
    eager: Option<(String, String)>,
    /// Statements run inside the innermost borrow scope, just before the
    /// call (finishing value buffers, registering callbacks).
    late: Vec<String>,
    /// The object lent for the call.
    lend: Option<Lend>,
    /// The C argument expression.
    arg: String,
}

impl ParamPlan {
    /// A plan whose argument is the conversion `expr` of parameter `n`.
    fn eager(n: &str, expr: String) -> Self {
        let local = format!("_{n}_v");
        Self {
            arg: local.clone(),
            eager: Some((local, expr)),
            ..Self::default()
        }
    }
}

/// Plan how parameter `n` of type `ty` crosses as its one C value.
fn plan_param(n: &str, ty: &ParamTy) -> ParamPlan {
    let ty = match ty {
        ParamTy::Value(ty) => ty,
        // Checked (and the vtable built) before anything is lent, and
        // registered last, so nothing between registration and the call can
        // fail and strand the entry; the producer releases it with `free`.
        ParamTy::Callback { name, nullable } => {
            let (check, register) = if *nullable {
                ("_callback_vtable_opt", "_callback_register_opt")
            } else {
                ("_callback_vtable", "_callback_register")
            };
            return ParamPlan {
                prep: vec![format!(
                    "_{n}_vt = {check}({n}, {name}, {})",
                    lazy_vtable(name)
                )],
                late: vec![format!("_{n}_ctx = {register}({n})")],
                arg: format!("{}(_{n}_ctx, _{n}_vt)", callback_arg_struct(name)),
                ..ParamPlan::default()
            };
        }
    };
    match crossing(ty) {
        Crossing::Scalar(p) => {
            let checked = checked_scalar(n, p, n, false);
            if checked == n {
                ParamPlan {
                    arg: n.to_string(),
                    ..ParamPlan::default()
                }
            } else {
                ParamPlan::eager(n, checked)
            }
        }
        Crossing::Enum(_) => ParamPlan::eager(n, checked_scalar(n, Prim::I32, n, false)),
        Crossing::OptScalar(p) => ParamPlan::eager(n, opt_value(n, p, n, false)),
        Crossing::OptEnum(_) => ParamPlan::eager(n, opt_value(n, Prim::I32, n, false)),
        Crossing::Utf8 => ParamPlan::eager(n, format!("_run_in({n}.encode(\"utf-8\"))")),
        Crossing::Bytes => ParamPlan::eager(n, format!("_run_in(bytes({n}))")),
        Crossing::Slice(p) => {
            ParamPlan::eager(n, format!("_array_in({n}, \"{}\", \"{n}\")", prim_kind(p)))
        }
        // Encoded with every other parameter; finished (object tokens
        // minted) just before the call.
        Crossing::Buffer(ty) => ParamPlan {
            prep: encode_stmts(&format!("_{n}_w"), n, ty).into(),
            late: vec![format!("_{n}_b = _{n}_w.finish()")],
            arg: format!("_run_in(_{n}_b)"),
            ..ParamPlan::default()
        },
        Crossing::Object {
            interface,
            nullable,
        } => ParamPlan {
            lend: Some(if nullable {
                Lend {
                    local: format!("_{n}_p"),
                    acquire: format!("_lend_opt({n}, {interface})"),
                    release: format!("_release_opt({n})"),
                }
            } else {
                Lend {
                    local: format!("_{n}_p"),
                    acquire: format!("_lend({n}, {interface})"),
                    release: format!("{n}._release()"),
                }
            }),
            arg: format!("_{n}_p"),
            ..ParamPlan::default()
        },
    }
}

/// Render one callable's wrapper `def` (its bindings are emitted separately
/// by [`render_bindings`]). A callable that throws raises its error domain
/// (or the root error for `throws: any`); any other failure raises the
/// unchecked trap.
pub(crate) fn render_callable(w: &mut CodeWriter, g: &Gen<'_>, f: &FnBinding, scope: FnScope) {
    let binding = py_binding_name(&f.abi.symbol, g.prefix);

    let mut sig: Vec<String> = match scope {
        FnScope::Method | FnScope::Init => vec!["self".into()],
        FnScope::Factory => vec!["cls".into()],
        FnScope::Free | FnScope::Static => vec![],
    };
    sig.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", py_name(&p.name), py_param_hint(&p.ty))),
    );
    let ret_hint = match scope {
        FnScope::Init => "None".to_string(),
        _ => py_return_hint(f.ret.as_ref()),
    };

    w.blank();
    if matches!(scope, FnScope::Free) {
        w.blank();
    }
    match scope {
        FnScope::Static => {
            w.line("@staticmethod");
        }
        FnScope::Factory => {
            w.line("@classmethod");
        }
        _ => {}
    }
    w.line(format!(
        "{}def {}({}) -> {ret_hint}:",
        if f.is_async() { "async " } else { "" },
        py_fn_name(f, scope),
        sig.join(", "),
    ));
    w.indent();
    let mut doc = g.doc(&f.doc, &f.deprecated);
    if f.iterator().is_some() {
        let streaming = "Returns a lazy `NativeIterator`: each step pulls one element from\n\
                         the producer. Exhaust it, `close()` it, or use it in a `with`\n\
                         statement to release its native handle (garbage collection also\n\
                         releases it).";
        doc = Some(match doc {
            Some(d) => format!("{d}\n\n{streaming}"),
            None => streaming.to_string(),
        });
    }
    let params: Vec<ParamDoc> = f
        .params
        .iter()
        .map(|p| ParamDoc {
            name: py_name(&p.name),
            hint: py_param_hint(&p.ty),
            doc: g.text(&p.doc),
        })
        .collect();
    fn_docstring(w, doc.as_deref(), &params, &g.raises(&f.error));

    if let Some(msg) = g.deprecation(&f.deprecated) {
        w.line(format!(
            "warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)",
            py_str_literal(&msg)
        ));
    }

    let mut plans: Vec<ParamPlan> = f
        .params
        .iter()
        .map(|p| plan_param(&py_name(&p.name), &p.ty))
        .collect();

    // Lent objects (the receiver first), and the late statements: finishing
    // value buffers, then registering callbacks (which can't fail).
    let mut lends: Vec<Lend> = Vec::new();
    if f.has_self() {
        lends.push(Lend {
            local: "_self_p".into(),
            acquire: "self._acquire()".into(),
            release: "self._release()".into(),
        });
    }
    lends.extend(plans.iter().filter_map(|p| p.lend.clone()));
    let mut late: Vec<String> = Vec::new();
    for (p, plan) in f.params.iter().zip(&plans) {
        if matches!(p.ty, ParamTy::Value(_)) {
            late.extend(plan.late.iter().cloned());
        }
    }
    for (p, plan) in f.params.iter().zip(&plans) {
        if matches!(p.ty, ParamTy::Callback { .. }) {
            late.extend(plan.late.iter().cloned());
        }
    }
    // Conversions run in the argument list unless something is lent or
    // registered first.
    let inline = lends.is_empty() && late.is_empty();
    for plan in &mut plans {
        if let Some((local, expr)) = plan.eager.take() {
            if inline {
                plan.arg = expr;
            } else {
                plan.prep.push(format!("{local} = {expr}"));
            }
        }
    }
    for plan in &plans {
        for line in &plan.prep {
            w.line(line);
        }
    }

    // The C arguments: `self`, each parameter's value, then the trailing
    // slots of the shape.
    let mut args: Vec<String> = Vec::new();
    if f.has_self() {
        args.push("_self_p".into());
    }
    args.extend(plans.iter().map(|p| p.arg.clone()));

    if let Some(a) = f.async_binding() {
        if a.cancellable {
            args.push("_token".into());
        }
        args.push(format!("{}_completion", completion_stem(g, f)));
        args.push("_call".into());
        debug_assert_eq!(args.len(), f.abi.params.len(), "{}", f.abi.symbol);
        let call = format!("{binding}({})", args.join(", "));
        w.line("_call, _future = _async_begin()");
        if a.cancellable {
            w.line("_token = _cancel_token_create()");
        }
        w.line("try:");
        w.scope(|w| emit_lent_call(w, &lends, &late, &call));
        w.line("except BaseException:");
        w.scope(|w| {
            w.line("_async_abandon(_call)");
            if a.cancellable {
                w.line("_cancel_token_destroy(_token)");
            }
            w.line("raise");
        });
        let wait = if a.cancellable {
            "await _async_wait_cancellable(_future, _token)"
        } else {
            "await _future"
        };
        // The completion built the value; the annotation hands its type to
        // checkers.
        match f.ret_value() {
            Some(ty) => {
                w.line(format!("_result: {} = {wait}", py_type_hint(ty, Dir::Out)));
                w.line("return _result");
            }
            None => {
                w.line(wait);
            }
        }
        w.dedent();
        return;
    }

    w.line("_err = _ErrorStruct()");
    args.push("ctypes.byref(_err)".into());
    debug_assert_eq!(args.len(), f.abi.params.len(), "{}", f.abi.symbol);
    let call = format!("{binding}({})", args.join(", "));
    let value = f.ret.as_ref().map(|r| r.elem());
    let call = match (scope, value, f.iterator()) {
        (FnScope::Init | FnScope::Factory, _, _) | (_, _, Some(_)) => format!("_ret = {call}"),
        (_, None, None) => call,
        // A scalar result is used as returned, so its annotation tells
        // checkers what the untyped `ctypes` call produced.
        (_, Some(ty), None) => match crossing(ty) {
            Crossing::Scalar(_) => format!("_ret: {} = {call}", py_type_hint(ty, Dir::Out)),
            _ => format!("_ret = {call}"),
        },
    };
    emit_lent_call(w, &lends, &late, &call);
    w.line("if _err.code:");
    w.scope(|w| {
        w.line(format!(
            "raise {}(*_read_error(_err))",
            g.raise_factory(&f.error)
        ));
    });

    match scope {
        FnScope::Init => {
            w.line("self._init_handle(_required(_ret))");
        }
        FnScope::Factory => {
            w.line("return cls._adopt(_required(_ret))");
        }
        _ => {
            if let Some(it) = f.iterator() {
                w.line(format!(
                    "return _iterate(_required(_ret), {}, {})",
                    pull_fn(g, it),
                    py_binding_name(&it.destroy_symbol, g.prefix)
                ));
            } else if let Some(ty) = value {
                w.line(format!("return {}", received("_ret", ty, Ownership::Owned)));
            }
        }
    }
    w.dedent();
}

/// Emit the C call inside one `try`/`finally` per lent object (outermost
/// first), with the late statements (buffer finishing, callback
/// registration) just before the call.
fn emit_lent_call(w: &mut CodeWriter, lends: &[Lend], late: &[String], call: &str) {
    match lends.split_first() {
        None => {
            for line in late {
                w.line(line);
            }
            w.line(call);
        }
        Some((lend, rest)) => {
            w.line(format!("{} = {}", lend.local, lend.acquire));
            w.line("try:");
            w.scope(|w| emit_lent_call(w, rest, late, call));
            w.line("finally:");
            w.scope(|w| {
                w.line(&lend.release);
            });
        }
    }
}
