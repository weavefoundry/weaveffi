//! Call rendering: sync, async, and iterator wrappers and their module-level
//! `ctypes` bindings.
//!
//! Every C prototype is bound once, at import time, from the model's lowered
//! [`AbiFn`] (`argtypes` and `restype` come straight from its slots).
//! Marshalling dispatch is driven by the shared plans: [`ArgPass`] decides
//! how each parameter crosses and [`Family`] how a result is received, so
//! this module never re-derives those shapes.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    AbiFn, CallShape, ErrorBinding, FnBinding, IteratorBinding, ModuleBinding, ParamBinding,
};
use weaveffi_model::plan::ArgPass;
use weaveffi_model::ty::{Family, Ty};

use crate::targets::python::codec::{decode_expr, encode_stmts};
use crate::targets::python::docs::{fn_docstring, with_deprecation};
use crate::targets::python::entities::py_raise_factory;
use crate::targets::python::types::{
    py_binding_name, py_ctype, py_local, py_member_name, py_name, py_object_member, py_return_hint,
    py_slot_hint, py_str_literal, py_type_hint, Slot,
};
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

/// The lowered C function a callable's wrapper invokes.
fn launcher(f: &FnBinding) -> &AbiFn {
    match &f.shape {
        CallShape::Sync(abi) => abi,
        CallShape::Async(a) => &a.launch,
        CallShape::Iterator(it) => &it.launch,
    }
}

/// `_bind("symbol", restype, argtypes...)` for one lowered function. The
/// async launcher's `callback` slot is typed with the completion's
/// `CFUNCTYPE` (`callback_type`).
fn bind_line(g: &Gen<'_>, abi: &AbiFn, callback_type: Option<&str>) -> String {
    let mut parts = vec![
        format!("\"{}\"", abi.symbol),
        py_ctype(&abi.ret, Slot::Recv),
    ];
    parts.extend(abi.params.iter().map(|p| match (&p.ty, callback_type) {
        (CType::Named(_), Some(cb)) if p.name == "callback" => cb.to_string(),
        _ => py_ctype(&p.ty, Slot::Arg),
    }));
    format!(
        "{} = _bind({})",
        py_binding_name(&abi.symbol, g.prefix),
        parts.join(", ")
    )
}

/// The `CFUNCTYPE(...)` spelling for a C function-pointer signature.
pub(crate) fn cfunctype(ret: &CType, params: &[AbiParam]) -> String {
    let parts: Vec<String> = std::iter::once(py_ctype(ret, Slot::Recv))
        .chain(params.iter().map(|p| py_ctype(&p.ty, Slot::Recv)))
        .collect();
    format!("ctypes.CFUNCTYPE({})", parts.join(", "))
}

/// The annotated parameter list of a function `ctypes` calls back with the
/// C slots `params` (a trampoline or an async completion).
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
    let binding = py_binding_name(&launcher(f).symbol, g.prefix);
    binding.strip_prefix("_c").unwrap_or(&binding).to_string()
}

/// The module-level iterator class for an iterator-returning callable:
/// `_` plus the iterator tag without its prefix.
fn iterator_class(g: &Gen<'_>, it: &IteratorBinding) -> String {
    py_binding_name(&it.iter_tag, g.prefix).replacen("_c_", "_", 1)
}

/// The expression receiving one producer-owned value of `ty` (a sync
/// return, an async result, or an iterator element) from its pointer (or
/// direct value) expression `p` and length expression `len`, releasing what
/// it owes.
fn recv_expr(ty: &Ty, p: &str, len: &str) -> String {
    match ty.family() {
        Family::String => format!("_take_str({p}, {len})"),
        Family::Bytes => format!("_take_bytes({p}, {len})"),
        Family::Buffer => decode_expr(&format!("_take_bytes({p}, {len})"), ty),
        Family::Object { nullable } => adopt_expr(ty, p, nullable),
        Family::Direct => match ty {
            Ty::Enum(name) => format!("{name}({p})"),
            _ => p.to_string(),
        },
        Family::Callback { .. } | Family::Iterator => {
            unreachable!("callback interfaces and iterators are never received as values")
        }
    }
}

/// The expression adopting the object pointer `p` (one strong reference)
/// into a new wrapper of `ty`'s interface; `None` for a null `I?`.
pub(crate) fn adopt_expr(ty: &Ty, p: &str, nullable: bool) -> String {
    let class = ty.interface_name().expect("object names an interface");
    if nullable {
        format!("{class}._adopt({p}) if {p} else None")
    } else {
        format!("{class}._adopt(_required({p}))")
    }
}

/// Emit every module-level piece a callable needs ahead of its wrapper: the
/// bound launcher, plus the iterator class (iterator shape) or the static
/// completion trampoline (async shape). `owner` names the callable in docs
/// (`Store.get` for a member); `packed` appends a sync member's binding to
/// the block the previous member started, with no blank lines between.
pub(crate) fn render_bindings(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    owner: &str,
    packed: bool,
) {
    match &f.shape {
        CallShape::Sync(abi) => {
            if !packed {
                w.blank().blank();
            }
            w.line(bind_line(g, abi, None));
        }
        CallShape::Iterator(it) => {
            w.blank().blank();
            w.line(bind_line(g, &it.launch, None));
            render_iterator_class(w, g, f, it, error, owner);
        }
        CallShape::Async(a) => {
            let stem = completion_stem(g, f);
            let cb_type = py_binding_name(&a.callback_type, g.prefix);
            let factory = py_raise_factory(f, error);
            let value = match &f.ret {
                None => "None".to_string(),
                Some(ty) => match ty.family() {
                    Family::String | Family::Bytes | Family::Buffer => {
                        recv_expr(ty, "result_ptr", "result_len")
                    }
                    _ => recv_expr(ty, "result", ""),
                },
            };
            w.blank().blank();
            w.line(format!(
                "{cb_type} = {}",
                cfunctype(&CType::Void, &a.callback_params)
            ));
            w.line(bind_line(g, &a.launch, Some(&cb_type)));
            w.blank().blank();
            w.line(format!(
                "def {stem}_complete({}) -> None:",
                slot_params(&a.callback_params)
            ));
            w.scope(|w| {
                w.line("# Runs once, on a producer thread: take ownership of the result");
                w.line("# here, then hand it to the awaiting event loop.");
                w.line("try:");
                w.scope(|w| {
                    w.line("if err:");
                    w.scope(|w| {
                        w.line(format!(
                            "_async_settle(context, _async_error(err, {factory}), None)"
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
    }
}

/// Render the module-level class for one iterator-returning callable,
/// satisfying the pull contract of
/// [`weaveffi_model::plan::IteratorProtocol`]: one producer `next` call per
/// `__next__`, per-element releases after copying, and exactly one
/// `destroy` (on exhaustion, `close()`, or garbage collection).
fn render_iterator_class(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    f: &FnBinding,
    it: &IteratorBinding,
    error: Option<&ErrorBinding>,
    owner: &str,
) {
    let class = iterator_class(g, it);
    let next = py_binding_name(&it.next.symbol, g.prefix);
    let destroy = py_binding_name(&it.destroy_symbol, g.prefix);
    let factory = py_raise_factory(f, error);
    let item_ty = py_ctype(it.item_ctype(), Slot::Recv);
    let has_len = it.next.params.iter().any(|p| p.name == "out_len");

    w.line(bind_line(g, &it.next, None));
    w.line(format!(
        "{destroy} = _bind(\"{}\", None, ctypes.c_void_p)",
        it.destroy_symbol
    ));
    w.blank().blank();
    w.line(format!("class {class}(_Iterator):"));
    w.scope(|w| {
        w.line(format!(
            "\"\"\"The lazy iterator `{owner}` returns: each step pulls one element"
        ));
        w.line("from the producer.\"\"\"");
        w.blank();
        w.line(format!("_destroy = staticmethod({destroy})"));
        w.blank();
        w.line(format!("def __next__(self) -> {}:", py_type_hint(&it.elem)));
        w.scope(|w| {
            w.line(format!("_item = {item_ty}()"));
            let mut args = vec!["_p".to_string(), "ctypes.byref(_item)".into()];
            if has_len {
                w.line("_len = ctypes.c_size_t()");
                args.push("ctypes.byref(_len)".into());
            }
            args.push("ctypes.byref(_err)".into());
            w.line("_err = _ErrorStruct()");
            w.line("_p = self._step()");
            w.line("try:");
            w.scope(|w| {
                w.line(format!("_more = {next}({})", args.join(", ")));
            });
            w.line("finally:");
            w.scope(|w| {
                w.line("self._release()");
            });
            w.line("if _err.code:");
            w.scope(|w| {
                w.line(format!("raise {factory}(*_read_error(_err))"));
            });
            w.line("if not _more:");
            w.scope(|w| {
                w.line("self.close()");
                w.line("raise StopIteration");
            });
            w.line(format!(
                "return {}",
                recv_expr(&it.elem, "_item.value", "_len.value")
            ));
        });
    });
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

/// One parameter's local preparation and its C argument expressions.
struct ParamPlan {
    /// Statements run before any object is lent (encoding).
    prep: Vec<String>,
    /// Statements run inside the innermost borrow scope, just before the
    /// call (finishing value buffers, registering callbacks).
    late: Vec<String>,
    /// The object lent for the call.
    lend: Option<Lend>,
    /// The C argument expressions, in slot order.
    args: Vec<String>,
}

fn plan_param(p: &ParamBinding) -> ParamPlan {
    let n = py_name(&p.name);
    let mut plan = ParamPlan {
        prep: vec![],
        late: vec![],
        lend: None,
        args: vec![],
    };
    match p.arg_pass() {
        ArgPass::Direct { .. } => plan.args.push(n),
        ArgPass::String { .. } => {
            plan.prep.push(format!("_{n}_b = {n}.encode(\"utf-8\")"));
            plan.args = vec![format!("_{n}_b"), format!("len(_{n}_b)")];
        }
        ArgPass::Bytes { .. } => {
            plan.prep.push(format!("_{n}_b = bytes({n})"));
            plan.args = vec![format!("_{n}_b"), format!("len(_{n}_b)")];
        }
        // Encode now, mint object tokens later: `finish()` runs once every
        // parameter has encoded and every lent object is held, so a failure
        // anywhere before the call leaks no strong reference.
        ArgPass::Buffer { .. } => {
            plan.prep
                .extend(encode_stmts(&format!("_{n}_w"), &n, &p.ty));
            plan.late.push(format!("_{n}_b = _{n}_w.finish()"));
            plan.args = vec![format!("_{n}_b"), format!("len(_{n}_b)")];
        }
        ArgPass::Object { nullable, .. } => {
            let class = p.ty.interface_name().expect("object names an interface");
            plan.lend = Some(if nullable {
                Lend {
                    local: format!("_{n}_p"),
                    acquire: format!("_lend_opt({n}, {class})"),
                    release: format!("_release_opt({n})"),
                }
            } else {
                Lend {
                    local: format!("_{n}_p"),
                    acquire: format!("_lend({n}, {class})"),
                    release: format!("{n}._release()"),
                }
            });
            plan.args.push(format!("_{n}_p"));
        }
        // Registered last, so nothing between registration and the call can
        // fail and strand the entry; the producer releases it with `free`.
        ArgPass::Callback { nullable, .. } => {
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback names a callback interface");
            let vtable = format!("_{cb}_vtable_ptr");
            if nullable {
                plan.late
                    .push(format!("_{n}_ctx = _callback_register_opt({n}, {cb})"));
                plan.args = vec![
                    format!("_{n}_ctx"),
                    format!("{vtable} if {n} is not None else None"),
                ];
            } else {
                plan.late
                    .push(format!("_{n}_ctx = _callback_register({n}, {cb})"));
                plan.args = vec![format!("_{n}_ctx"), vtable];
            }
        }
    }
    plan
}

/// Render one callable's wrapper `def` (its bindings are emitted separately
/// by [`render_bindings`]). A throwing callable raises the error domain in
/// scope for `module`; any other failure raises the unchecked trap.
pub(crate) fn render_callable(
    w: &mut CodeWriter,
    g: &Gen<'_>,
    module: &ModuleBinding,
    f: &FnBinding,
    scope: FnScope,
) {
    let error = g.model.error_domain(module);
    let raises = error.filter(|_| f.throws).map(|eb| {
        (
            eb.type_name.as_str(),
            "If the call reports one of the domain's error codes.",
        )
    });
    let abi = launcher(f);
    let binding = py_binding_name(&abi.symbol, g.prefix);

    let mut sig: Vec<String> = match scope {
        FnScope::Method | FnScope::Init => vec!["self".into()],
        FnScope::Factory => vec!["cls".into()],
        FnScope::Free | FnScope::Static => vec![],
    };
    sig.extend(
        f.params
            .iter()
            .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty))),
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
    let mut doc = with_deprecation(f.doc.as_deref(), f.deprecated.as_deref());
    if let CallShape::Iterator(_) = f.shape {
        let streaming = "Returns a lazy iterator: each step pulls one element from the\n\
                         producer. Exhaust or close() the iterator to release its native\n\
                         handle (garbage collection also releases it).";
        doc = Some(match doc {
            Some(d) => format!("{d}\n\n{streaming}"),
            None => streaming.to_string(),
        });
    }
    fn_docstring(w, doc.as_deref(), &f.params, raises);

    if let Some(msg) = &f.deprecated {
        w.line(format!(
            "warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)",
            py_str_literal(msg)
        ));
    }

    let plans: Vec<ParamPlan> = f.params.iter().map(plan_param).collect();
    for line in plans.iter().flat_map(|p| &p.prep) {
        w.line(line);
    }

    // The C arguments: `self`, each parameter's slots, then the trailing
    // slots the lowering appended, named by the model.
    let mut args: Vec<String> = Vec::new();
    let mut consumed = 0;
    if f.has_self {
        args.push("_self_p".into());
        consumed = 1;
    }
    for (p, plan) in f.params.iter().zip(&plans) {
        args.extend(plan.args.iter().cloned());
        consumed += p.abi.len();
    }
    for slot in &abi.params[consumed..] {
        args.push(match slot.name.as_str() {
            "out_len" => "ctypes.byref(_out_len)".to_string(),
            "out_err" => "ctypes.byref(_err)".to_string(),
            "cancel_token" => "_token".to_string(),
            "callback" => format!("{}_completion", completion_stem(g, f)),
            "context" => "_call".to_string(),
            other => unreachable!("unexpected trailing slot `{other}`"),
        });
    }
    let call = format!("{binding}({})", args.join(", "));

    let mut lends: Vec<Lend> = Vec::new();
    if f.has_self {
        lends.push(Lend {
            local: "_self_p".into(),
            acquire: "self._acquire()".into(),
            release: "self._release()".into(),
        });
    }
    lends.extend(plans.iter().filter_map(|p| p.lend.clone()));
    let late: Vec<String> = plans.iter().flat_map(|p| p.late.clone()).collect();

    if let CallShape::Async(_) = f.shape {
        w.line("_call, _future = _async_begin()");
        if f.cancellable {
            w.line("_token = _cancel_token_create()");
        }
        w.line("try:");
        w.scope(|w| emit_lent_call(w, &lends, &late, &call));
        w.line("except BaseException:");
        w.scope(|w| {
            w.line("_async_abandon(_call)");
            if f.cancellable {
                w.line("_cancel_token_destroy(_token)");
            }
            w.line("raise");
        });
        let wait = if f.cancellable {
            "await _async_wait_cancellable(_future, _token)"
        } else {
            "await _future"
        };
        // The completion built the value; the annotation hands its type to
        // checkers.
        match &f.ret {
            Some(ty) => {
                w.line(format!("_result: {} = {wait}", py_type_hint(ty)));
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
    if abi.params.iter().any(|p| p.name == "out_len") {
        w.line("_out_len = ctypes.c_size_t()");
    }
    // A direct result is used as returned, so its annotation tells checkers
    // what the untyped `ctypes` call produced.
    let call = match &f.ret {
        Some(ty @ Ty::Prim(_)) if ty.family() == Family::Direct => {
            format!("_ret: {} = {call}", py_type_hint(ty))
        }
        Some(_) => format!("_ret = {call}"),
        None if matches!(scope, FnScope::Init | FnScope::Factory) => format!("_ret = {call}"),
        None => call,
    };
    emit_lent_call(w, &lends, &late, &call);
    w.line("if _err.code:");
    w.scope(|w| {
        w.line(format!(
            "raise {}(*_read_error(_err))",
            py_raise_factory(f, error)
        ));
    });

    match (scope, &f.shape) {
        (FnScope::Init, _) => {
            w.line("self._init_handle(_required(_ret))");
        }
        (FnScope::Factory, _) => {
            w.line("return cls._adopt(_required(_ret))");
        }
        (_, CallShape::Iterator(it)) => {
            w.line(format!("return {}._adopt(_ret)", iterator_class(g, it)));
        }
        _ => {
            if let Some(ty) = &f.ret {
                w.line(format!(
                    "return {}",
                    recv_expr(ty, "_ret", "_out_len.value")
                ));
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
