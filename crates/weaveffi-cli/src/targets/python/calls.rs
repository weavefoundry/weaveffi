//! Call rendering: sync, async, and iterator wrappers and their module-level
//! `ctypes` bindings.
//!
//! Every C prototype is bound once, at import time, from the model's lowered
//! [`AbiFn`] (`argtypes` and `restype` come straight from its slots).
//! Marshalling is driven by the stored passing contracts: [`ArgPass`]
//! decides how each parameter crosses, [`RetPass`], [`ResultPass`], and
//! [`ItemPass`] how a sync return, an async result, and an iterator item are
//! received, and [`ErrorStrategy`](weaveffi_model::plan::ErrorStrategy)
//! what a failure raises, so this module never re-derives those shapes.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{AbiFn, AsyncBinding, FnBinding, IteratorBinding};
use weaveffi_model::plan::{ArgPass, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{Prim, RetTy, Ty};

use crate::targets::python::codec::{decode_expr, encode_stmts};
use crate::targets::python::docs::{fn_docstring, ParamDoc};
use crate::targets::python::types::{
    enum_class, int_checker, prim_kind, py_binding_name, py_ctype, py_local, py_member_name,
    py_name, py_object_member, py_out_local, py_param_hint, py_restype, py_return_hint,
    py_slot_hint, py_str_literal, py_type_hint, Dir,
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

/// `_bind("symbol", restype, argtypes...)` for one lowered function. The
/// async launcher's `callback` slot is typed with the completion's
/// `CFUNCTYPE` (`callback_type`).
fn bind_line(g: &Gen<'_>, abi: &AbiFn, callback_type: Option<&str>) -> String {
    let mut parts = vec![format!("\"{}\"", abi.symbol), py_restype(&abi.ret)];
    parts.extend(abi.params.iter().map(|p| match callback_type {
        Some(cb) if p.name == "callback" => cb.to_string(),
        _ => py_ctype(&p.ty, true),
    }));
    format!(
        "{} = _bind({})",
        py_binding_name(&abi.symbol, g.prefix),
        parts.join(", ")
    )
}

/// The `CFUNCTYPE(...)` spelling for a C function-pointer signature the
/// producer calls (a vtable method or an async completion).
pub(crate) fn cfunctype(ret: &CType, params: &[AbiParam]) -> String {
    let parts: Vec<String> = std::iter::once(py_restype(ret))
        .chain(params.iter().map(|p| py_ctype(&p.ty, false)))
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
    let binding = py_binding_name(&f.abi.symbol, g.prefix);
    binding.strip_prefix("_c").unwrap_or(&binding).to_string()
}

/// The module-level function pulling one element of an iterator:
/// `_pull_` plus the iterator tag without its prefix.
fn pull_fn(g: &Gen<'_>, it: &IteratorBinding) -> String {
    py_binding_name(&it.iter_tag, g.prefix).replacen("_c_", "_pull_", 1)
}

/// The value expression `expr` of a direct slot of C type `ty` as Python
/// hands it out: re-wrapped in its `IntEnum` for a C-style enum.
pub(crate) fn direct_value(expr: &str, ty: &CType) -> String {
    match enum_class(ty) {
        Some(class) => format!("{class}({expr})"),
        None => expr.to_string(),
    }
}

/// One value the producer hands over (a sync return, an async result, or
/// an iterator item) as the expressions that hold its slots.
pub(crate) enum Recv<'a> {
    /// A number, `bool`, or enum, by value.
    Direct { value: String, ty: &'a CType },
    /// A present flag and a value (ignored when absent).
    OptDirect {
        has: String,
        value: String,
        ty: &'a CType,
    },
    /// An owned typed-array run: address and element count.
    Slice {
        ptr: String,
        len: String,
        elem: Prim,
    },
    /// An owned UTF-8 run.
    String { ptr: String, len: String },
    /// An owned byte run.
    Bytes { ptr: String, len: String },
    /// An owned value buffer of type `ty`.
    Buffer {
        ptr: String,
        len: String,
        ty: &'a Ty,
    },
    /// One strong reference to an object.
    Object {
        ptr: String,
        nullable: bool,
        interface: &'a str,
    },
}

/// The expression taking ownership of a received value: copying it into a
/// Python value and releasing what the producer handed over.
pub(crate) fn recv_expr(r: &Recv<'_>) -> String {
    match r {
        Recv::Direct { value, ty } => direct_value(value, ty),
        Recv::OptDirect { has, value, ty } => {
            format!("{} if {has} else None", direct_value(value, ty))
        }
        Recv::Slice { ptr, len, elem } => {
            format!("_take_array({ptr}, {len}, \"{}\")", prim_kind(*elem))
        }
        Recv::String { ptr, len } => format!("_take_str({ptr}, {len})"),
        Recv::Bytes { ptr, len } => format!("_take_bytes({ptr}, {len})"),
        Recv::Buffer { ptr, len, ty } => decode_expr(&format!("_take_bytes({ptr}, {len})"), ty),
        Recv::Object {
            ptr,
            nullable,
            interface,
        } => adopt_expr(interface, ptr, *nullable),
    }
}

/// The expression adopting the object pointer `p` (one strong reference)
/// into a new wrapper of `class`; `None` for a null `I?`.
pub(crate) fn adopt_expr(class: &str, p: &str, nullable: bool) -> String {
    if nullable {
        format!("{class}._adopt({p}) if {p} else None")
    } else {
        format!("{class}._adopt(_required({p}))")
    }
}

/// The local holding an out slot's value: `_` plus the slot name
/// (`_out_len`, `_out_value`, `_out_item`).
fn out_local(slot: &AbiParam) -> String {
    format!("_{}", slot.name)
}

/// How an async completion's result slots become the awaited value.
fn result_recv<'a>(result: &'a ResultPass, elem: Option<&'a Ty>) -> Option<Recv<'a>> {
    let local = |slot: &AbiParam| py_local(&slot.name);
    Some(match result {
        ResultPass::Void => return None,
        ResultPass::Direct { result } => Recv::Direct {
            value: local(result),
            ty: &result.ty,
        },
        ResultPass::OptDirect { has, value } => Recv::OptDirect {
            has: local(has),
            value: local(value),
            ty: &value.ty,
        },
        ResultPass::Slice { ptr, len, elem } => Recv::Slice {
            ptr: local(ptr),
            len: local(len),
            elem: *elem,
        },
        ResultPass::String { ptr, len } => Recv::String {
            ptr: local(ptr),
            len: local(len),
        },
        ResultPass::Bytes { ptr, len } => Recv::Bytes {
            ptr: local(ptr),
            len: local(len),
        },
        ResultPass::Buffer { ptr, len } => Recv::Buffer {
            ptr: local(ptr),
            len: local(len),
            ty: elem.expect("a buffered result has a value type"),
        },
        ResultPass::Object {
            result,
            nullable,
            interface,
            ..
        } => Recv::Object {
            ptr: local(result),
            nullable: *nullable,
            interface,
        },
    })
}

/// How an iterator's `_next` out slots become the yielded item. The slots
/// are locals named by [`out_local`], read through `.value`.
fn item_recv<'a>(item: &'a ItemPass, elem: &'a Ty) -> Recv<'a> {
    let value = |slot: &AbiParam| format!("{}.value", out_local(slot));
    let pointee = |slot: &'a AbiParam| match &slot.ty {
        CType::Ptr { pointee, .. } => pointee.as_ref(),
        other => other,
    };
    match item {
        ItemPass::Direct { out_item } => Recv::Direct {
            value: value(out_item),
            ty: pointee(out_item),
        },
        ItemPass::OptDirect { out_has, out_item } => Recv::OptDirect {
            has: value(out_has),
            value: value(out_item),
            ty: pointee(out_item),
        },
        ItemPass::Slice {
            out_item,
            out_len,
            elem,
        } => Recv::Slice {
            ptr: value(out_item),
            len: value(out_len),
            elem: *elem,
        },
        ItemPass::String { out_item, out_len } => Recv::String {
            ptr: value(out_item),
            len: value(out_len),
        },
        ItemPass::Bytes { out_item, out_len } => Recv::Bytes {
            ptr: value(out_item),
            len: value(out_len),
        },
        ItemPass::Buffer { out_item, out_len } => Recv::Buffer {
            ptr: value(out_item),
            len: value(out_len),
            ty: elem,
        },
        ItemPass::Object {
            out_item,
            nullable,
            interface,
            ..
        } => Recv::Object {
            ptr: value(out_item),
            nullable: *nullable,
            interface,
        },
    }
}

/// How a sync call's C return (`_ret`) and out slots become the returned
/// value; `None` for a void call or an iterator (handled by the caller).
fn ret_recv<'a>(ret_pass: &'a RetPass, ret: Option<&'a RetTy>, abi: &'a AbiFn) -> Option<Recv<'a>> {
    let value = |slot: &AbiParam| format!("{}.value", out_local(slot));
    let ret_ptr = || "_ret".to_string();
    Some(match ret_pass {
        RetPass::Void | RetPass::Iterator(_) => return None,
        RetPass::Direct => Recv::Direct {
            value: ret_ptr(),
            ty: &abi.ret,
        },
        RetPass::OptDirect { out_value } => Recv::OptDirect {
            has: ret_ptr(),
            value: value(out_value),
            ty: match &out_value.ty {
                CType::Ptr { pointee, .. } => pointee,
                other => other,
            },
        },
        RetPass::Slice { out_len, elem } => Recv::Slice {
            ptr: ret_ptr(),
            len: value(out_len),
            elem: *elem,
        },
        RetPass::String { out_len } => Recv::String {
            ptr: ret_ptr(),
            len: value(out_len),
        },
        RetPass::Bytes { out_len } => Recv::Bytes {
            ptr: ret_ptr(),
            len: value(out_len),
        },
        RetPass::Buffer { out_len } => Recv::Buffer {
            ptr: ret_ptr(),
            len: value(out_len),
            ty: ret
                .and_then(RetTy::value)
                .expect("a buffered return has a value type"),
        },
        RetPass::Object {
            nullable,
            interface,
            ..
        } => Recv::Object {
            ptr: ret_ptr(),
            nullable: *nullable,
            interface,
        },
    })
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
    w.blank().blank();
    w.line(format!(
        "def {}(_p: int) -> {}:",
        pull_fn(g, it),
        py_type_hint(&it.elem, Dir::Out)
    ));
    w.scope(|w| {
        let mut args = vec!["_p".to_string()];
        for slot in it.item.slots() {
            w.line(format!(
                "{} = {}()",
                out_local(slot),
                py_out_local(&slot.ty)
            ));
            args.push(format!("ctypes.byref({})", out_local(slot)));
        }
        w.line("_err = _ErrorStruct()");
        args.push("ctypes.byref(_err)".into());
        w.line(format!("_more = {next}({})", args.join(", ")));
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
            recv_expr(&item_recv(&it.item, &it.elem))
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
    let elem = f.ret.as_ref().and_then(RetTy::value);
    let value = result_recv(&a.result, elem).map_or_else(|| "None".into(), |r| recv_expr(&r));
    w.blank().blank();
    w.line(format!(
        "{cb_type} = {}",
        cfunctype(&CType::Void, &a.callback_params)
    ));
    w.line(bind_line(g, &f.abi, Some(&cb_type)));
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

/// One parameter's local preparation and its C argument expressions.
#[derive(Default)]
struct ParamPlan {
    /// Statements run before any object is lent (checking and encoding).
    prep: Vec<String>,
    /// Range checks: `(local, expression)`, inlined into the argument list
    /// when nothing is lent or registered before the call, else bound to the
    /// local in `prep` (a failing check must not strand a registration).
    checks: Vec<(String, String)>,
    /// Statements run inside the innermost borrow scope, just before the
    /// call (finishing value buffers, registering callbacks).
    late: Vec<String>,
    /// The object lent for the call.
    lend: Option<Lend>,
    /// The C argument expressions, in slot order.
    args: Vec<String>,
}

/// The checked argument for a direct value `n` of C type `ty`: the
/// runtime's range check for an integer or enum, the value itself for a
/// float or `bool`.
fn checked(plan: &mut ParamPlan, n: &str, local: String, ty: &CType) -> String {
    match int_checker(ty) {
        Some(check) => {
            plan.checks
                .push((local.clone(), format!("{check}({n}, \"{n}\")")));
            local
        }
        None => n.to_string(),
    }
}

/// Plan how parameter `n` crosses per its passing contract.
fn plan_param(n: &str, pass: &ArgPass) -> ParamPlan {
    let mut plan = ParamPlan::default();
    match pass {
        ArgPass::Direct { slot } => {
            let arg = checked(&mut plan, n, format!("_{n}_v"), &slot.ty);
            plan.args.push(arg);
        }
        ArgPass::OptDirect { value, .. } => {
            let zero = match value.ty {
                CType::Float | CType::Double => "0.0",
                CType::Bool => "False",
                _ => "0",
            };
            let local = format!("_{n}_v");
            let present = match int_checker(&value.ty) {
                Some(check) => format!("{check}({n}, \"{n}\")"),
                None => n.to_string(),
            };
            plan.checks.push((
                local.clone(),
                format!("{present} if {n} is not None else {zero}"),
            ));
            plan.args = vec![format!("{n} is not None"), local];
        }
        ArgPass::Slice { elem, .. } => {
            plan.prep.push(format!(
                "_{n}_a = _array({n}, \"{}\", \"{n}\")",
                prim_kind(*elem)
            ));
            plan.args = vec![format!("_{n}_a.buffer_info()[0]"), format!("len(_{n}_a)")];
        }
        ArgPass::String { .. } => {
            plan.prep.push(format!("_{n}_b = {n}.encode(\"utf-8\")"));
            plan.args = vec![format!("_{n}_b"), format!("len(_{n}_b)")];
        }
        ArgPass::Bytes { .. } => {
            plan.prep.push(format!("_{n}_b = bytes({n})"));
            plan.args = vec![format!("_{n}_b"), format!("len(_{n}_b)")];
        }
        // Encoded with every other parameter; buffers are finished (object
        // tokens minted) later.
        ArgPass::Buffer { .. } => {
            plan.args = vec![format!("_{n}_b"), format!("len(_{n}_b)")];
        }
        ArgPass::Object {
            nullable,
            interface,
            ..
        } => {
            plan.lend = Some(if *nullable {
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
            });
            plan.args.push(format!("_{n}_p"));
        }
        // Registered last, so nothing between registration and the call can
        // fail and strand the entry; the producer releases it with `free`.
        ArgPass::Callback {
            nullable,
            interface,
            ..
        } => {
            let vtable = format!("_{interface}_vtable_ptr");
            if *nullable {
                plan.late.push(format!(
                    "_{n}_ctx = _callback_register_opt({n}, {interface})"
                ));
                plan.args = vec![
                    format!("_{n}_ctx"),
                    format!("{vtable} if {n} is not None else None"),
                ];
            } else {
                plan.late
                    .push(format!("_{n}_ctx = _callback_register({n}, {interface})"));
                plan.args = vec![format!("_{n}_ctx"), vtable];
            }
        }
    }
    plan
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
        .map(|p| plan_param(&py_name(&p.name), &p.pass))
        .collect();

    // Lent objects (the receiver first), and the late statements: finishing
    // value buffers and registering callbacks.
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
        if let ArgPass::Buffer { .. } = p.pass {
            let n = py_name(&p.name);
            late.push(format!("_{n}_b = _{n}_w.finish()"));
        }
        late.extend(plan.late.iter().cloned());
    }
    // Range checks run in the argument list unless something is lent or
    // registered first.
    let inline = lends.is_empty() && late.is_empty();
    for plan in &mut plans {
        let checks = std::mem::take(&mut plan.checks);
        for (local, expr) in checks {
            if inline {
                for arg in &mut plan.args {
                    if *arg == local {
                        arg.clone_from(&expr);
                    }
                }
            } else {
                plan.prep.push(format!("{local} = {expr}"));
            }
        }
    }
    for (p, plan) in f.params.iter().zip(&plans) {
        for line in &plan.prep {
            w.line(line);
        }
        if let (ArgPass::Buffer { .. }, Some(ty)) = (&p.pass, p.ty.value()) {
            let n = py_name(&p.name);
            for line in encode_stmts(&format!("_{n}_w"), &n, ty) {
                w.line(line);
            }
        }
    }

    // The C arguments: `self`, each parameter's slots, then the trailing
    // slots of the shape.
    let mut args: Vec<String> = Vec::new();
    if f.has_self() {
        args.push("_self_p".into());
    }
    for plan in &plans {
        args.extend(plan.args.iter().cloned());
    }

    if let Some(a) = f.async_binding() {
        if a.cancellable() {
            args.push("_token".into());
        }
        args.push(format!("{}_completion", completion_stem(g, f)));
        args.push("_call".into());
        debug_assert_eq!(args.len(), f.abi.params.len(), "{}", f.abi.symbol);
        let call = format!("{binding}({})", args.join(", "));
        w.line("_call, _future = _async_begin()");
        if a.cancellable() {
            w.line("_token = _cancel_token_create()");
        }
        w.line("try:");
        w.scope(|w| emit_lent_call(w, &lends, &late, &call));
        w.line("except BaseException:");
        w.scope(|w| {
            w.line("_async_abandon(_call)");
            if a.cancellable() {
                w.line("_cancel_token_destroy(_token)");
            }
            w.line("raise");
        });
        let wait = if a.cancellable() {
            "await _async_wait_cancellable(_future, _token)"
        } else {
            "await _future"
        };
        // The completion built the value; the annotation hands its type to
        // checkers.
        match f.ret.as_ref().and_then(RetTy::value) {
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

    for slot in f.ret_pass.out_slots() {
        w.line(format!(
            "{} = {}()",
            out_local(slot),
            py_out_local(&slot.ty)
        ));
        args.push(format!("ctypes.byref({})", out_local(slot)));
    }
    w.line("_err = _ErrorStruct()");
    args.push("ctypes.byref(_err)".into());
    debug_assert_eq!(args.len(), f.abi.params.len(), "{}", f.abi.symbol);
    let call = format!("{binding}({})", args.join(", "));
    // A direct scalar result is used as returned, so its annotation tells
    // checkers what the untyped `ctypes` call produced.
    let call = match (&f.ret_pass, f.ret.as_ref().and_then(RetTy::value)) {
        (RetPass::Void, _) if !matches!(scope, FnScope::Init | FnScope::Factory) => call,
        (RetPass::Direct, Some(ty @ Ty::Prim(_))) => {
            format!("_ret: {} = {call}", py_type_hint(ty, Dir::Out))
        }
        _ => format!("_ret = {call}"),
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
            if let RetPass::Iterator(it) = &f.ret_pass {
                w.line(format!(
                    "return _iterate(_required(_ret), {}, {})",
                    pull_fn(g, it),
                    py_binding_name(&it.destroy_symbol, g.prefix)
                ));
            } else if let Some(r) = ret_recv(&f.ret_pass, f.ret.as_ref(), &f.abi) {
                w.line(format!("return {}", recv_expr(&r)));
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
