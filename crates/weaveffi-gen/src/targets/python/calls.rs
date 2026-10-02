//! Call rendering: sync, async, and iterator wrappers, their module-level
//! `ctypes` bindings, and the consumer-facing abstract base class, static
//! vtable, and trampolines of each callback interface.
//!
//! Every C prototype is bound once, at import time, from the model's lowered
//! [`AbiFn`] (`argtypes` and `restype` come straight from its slots).
//! Marshalling dispatch is driven by the shared plans: [`ArgPass`] decides
//! how each parameter crosses and [`Family`] how a result (or a trampoline
//! argument) is received, so this module never re-derives those shapes.

use crate::codegen::CodeWriter;
use crate::utils::local_type_name;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    AbiFn, CallShape, CallbackInterfaceBinding, CallbackMethodBinding, ErrorBinding, Family,
    FnBinding, IteratorBinding, ModuleBinding, ParamBinding, Ty,
};
use weaveffi_model::plan::ArgPass;

use crate::targets::python::codec::{py_decode_expr, py_write_stmts};
use crate::targets::python::docs::{emit_docstring, emit_fn_docstring};
use crate::targets::python::entities::{py_checker_name, py_factory_name};
use crate::targets::python::types::{
    py_binding_name, py_ctype, py_local, py_member_name, py_name, py_str_literal, py_type_hint,
    py_wrapper_fn_name, Slot,
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

impl FnScope {
    /// Indentation depth of the `def` line (0 at module scope, 1 in a class).
    fn depth(self) -> usize {
        usize::from(!matches!(self, FnScope::Free))
    }
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
fn cfunctype(ret: &CType, params: &[AbiParam]) -> String {
    let parts: Vec<String> = std::iter::once(py_ctype(ret, Slot::Recv))
        .chain(params.iter().map(|p| py_ctype(&p.ty, Slot::Recv)))
        .collect();
    format!("ctypes.CFUNCTYPE({})", parts.join(", "))
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
fn py_recv_expr(ty: &Ty, p: &str, len: &str) -> String {
    match ty.family() {
        Family::String => format!("_take_str({p}, {len})"),
        Family::Bytes => format!("_take_bytes({p}, {len})"),
        Family::Buffer => py_decode_expr(&format!("_take_bytes({p}, {len})"), ty),
        Family::Object { nullable } => {
            let class = local_type_name(ty.interface_name().expect("object names an interface"));
            if nullable {
                format!("({class}._adopt({p}) if {p} else None)")
            } else {
                format!("{class}._adopt(_required({p}))")
            }
        }
        Family::Direct => match ty {
            Ty::Enum(name) => format!("{}({p})", local_type_name(name)),
            _ => p.to_string(),
        },
        Family::Callback | Family::Iterator => {
            unreachable!("callback interfaces and iterators are never received as values")
        }
    }
}

/// Emit every module-level piece a callable needs ahead of its wrapper: the
/// bound launcher, plus the iterator class (iterator shape) or the static
/// completion trampoline (async shape). `owner` names the callable in docs
/// (`Store.get` for a member); `packed` appends a sync member's binding to
/// the block the previous member started, with no blank lines between.
pub(crate) fn render_bindings(
    out: &mut String,
    g: &Gen<'_>,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    owner: &str,
    packed: bool,
) {
    let mut w = CodeWriter::four_space();
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
            render_iterator_class(&mut w, g, f, it, error, owner);
        }
        CallShape::Async(a) => {
            let stem = completion_stem(g, f);
            let cb_type = py_binding_name(&a.callback_type, g.prefix);
            let factory = match error {
                Some(eb) if f.throws => py_factory_name(eb),
                _ => "_error_from".to_string(),
            };
            let slots: Vec<String> = a
                .callback_params
                .iter()
                .map(|p| py_slot_name(&p.name))
                .collect();
            let value = match &f.ret {
                None => "None".to_string(),
                Some(ty) => match ty.family() {
                    Family::String | Family::Bytes | Family::Buffer => {
                        py_recv_expr(ty, "result_ptr", "result_len")
                    }
                    _ => py_recv_expr(ty, "result", ""),
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
                slots.join(", ")
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
    out.push_str(&w.finish());
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
    let checker = py_checker_name(f, error);
    let item_ty = it
        .next
        .params
        .iter()
        .find(|p| p.name == "out_item")
        .and_then(|p| match &p.ty {
            CType::Ptr { pointee, .. } => Some(py_ctype(pointee, Slot::Recv)),
            _ => None,
        })
        .expect("iterator next writes through out_item");
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
            w.line(format!("{checker}(_err)"));
            w.line("if not _more:");
            w.scope(|w| {
                w.line("self.close()");
                w.line("raise StopIteration");
            });
            w.line(format!(
                "return {}",
                py_recv_expr(&it.elem, "_item.value", "_len.value")
            ));
        });
    });
}

/// The emitted Python name for a callable in `scope`.
fn py_fn_name(g: &Gen<'_>, module: &ModuleBinding, f: &FnBinding, scope: FnScope) -> String {
    match scope {
        FnScope::Free => py_wrapper_fn_name(&module.path, &f.name, g.strip_module_prefix),
        FnScope::Init => "__init__".to_string(),
        _ => py_member_name(&f.name),
    }
}

/// One parameter's local preparation and its C argument expressions.
struct ParamPlan {
    /// Statements run before any object is lent (encoding).
    prep: Vec<String>,
    /// Statements run inside the innermost borrow scope, just before the
    /// call (finishing value buffers, registering callbacks).
    late: Vec<String>,
    /// The object lent for the call: `(local, acquire expr, release stmt)`.
    lend: Option<(String, String, String)>,
    /// The C argument expressions, in slot order.
    args: Vec<String>,
}

fn plan_param(p: &ParamBinding, next: &mut usize) -> ParamPlan {
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
            let mut w = CodeWriter::four_space();
            w.line(format!("_{n}_w = _Writer()"));
            py_write_stmts(&mut w, &format!("_{n}_w"), &n, &p.ty, next);
            plan.prep = w.finish().lines().map(str::to_string).collect();
            plan.late.push(format!("_{n}_b = _{n}_w.finish()"));
            plan.args = vec![format!("_{n}_b"), format!("len(_{n}_b)")];
        }
        ArgPass::Object { nullable, .. } => {
            let class = local_type_name(p.ty.interface_name().expect("object names an interface"));
            let (acquire, release) = if nullable {
                (
                    format!("_lend_opt({n}, {class})"),
                    format!("_release_opt({n})"),
                )
            } else {
                (format!("_lend({n}, {class})"), format!("{n}._release()"))
            };
            plan.lend = Some((format!("_{n}_p"), acquire, release));
            plan.args.push(format!("_{n}_p"));
        }
        ArgPass::Callback { .. } => {
            let cb = local_type_name(
                p.ty.callback_interface_name()
                    .expect("callback names a callback interface"),
            );
            plan.late
                .push(format!("_{n}_ctx = _callback_register({n}, {cb})"));
            plan.args = vec![
                format!("_{n}_ctx"),
                format!("ctypes.addressof(_{cb}_vtable)"),
            ];
        }
    }
    plan
}

/// Render one callable's wrapper `def` (its bindings are emitted separately
/// by [`render_bindings`]). A throwing callable raises `module`'s error domain.
pub(crate) fn render_callable(
    out: &mut String,
    g: &Gen<'_>,
    module: &ModuleBinding,
    f: &FnBinding,
    scope: FnScope,
) {
    let error = module.error.as_ref();
    let depth = scope.depth();
    let raises = error.filter(|_| f.throws).map(|eb| eb.type_name.as_str());
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
        _ => f
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".to_string()),
    };

    let mut w = CodeWriter::four_space().with_depth(depth);
    w.blank();
    if depth == 0 {
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
        if f.is_async { "async " } else { "" },
        py_fn_name(g, module, f, scope),
        sig.join(", "),
    ));
    w.indent();
    let doc = match (&f.shape, &f.doc) {
        (CallShape::Iterator(_), d) => {
            let streaming = "Returns a lazy iterator: each step pulls one element from the\n\
                             producer. Exhaust or close() the iterator to release its native\n\
                             handle (garbage collection also releases it).";
            Some(match d {
                Some(d) => format!("{}\n\n{streaming}", d.trim()),
                None => streaming.to_string(),
            })
        }
        (_, d) => d.clone(),
    };
    let mut fdoc = String::new();
    emit_fn_docstring(&mut fdoc, &doc, &f.params, &w.indent_str(), raises);
    w.raw(fdoc);

    if let Some(msg) = &f.deprecated {
        w.line(format!(
            "warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)",
            py_str_literal(msg)
        ));
    }

    let mut next = 0;
    let plans: Vec<ParamPlan> = f.params.iter().map(|p| plan_param(p, &mut next)).collect();
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

    let mut lends: Vec<(String, String, String)> = Vec::new();
    if f.has_self {
        lends.push((
            "_self_p".into(),
            "self._acquire()".into(),
            "self._release()".into(),
        ));
    }
    lends.extend(plans.iter().filter_map(|p| p.lend.clone()));
    let late: Vec<String> = plans.iter().flat_map(|p| p.late.clone()).collect();

    let is_async = matches!(f.shape, CallShape::Async(_));
    if is_async {
        w.line("_call, _future = _async_begin()");
        if f.cancellable {
            w.line("_token = _cancel_token_create()");
        }
        w.line("try:");
        w.indent();
        emit_lent_call(&mut w, &lends, &late, &call);
        w.dedent();
        w.line("except BaseException:");
        w.scope(|w| {
            w.line("_async_abandon(_call)");
            if f.cancellable {
                w.line("_cancel_token_destroy(_token)");
            }
            w.line("raise");
        });
        if f.cancellable {
            w.line("return await _async_wait_cancellable(_future, _token)");
        } else {
            w.line("return await _future");
        }
        out.push_str(&w.finish());
        return;
    }

    w.line("_err = _ErrorStruct()");
    if abi.params.iter().any(|p| p.name == "out_len") {
        w.line("_out_len = ctypes.c_size_t()");
    }
    let has_ret = f.ret.is_some();
    let call = if has_ret {
        format!("_ret = {call}")
    } else {
        call
    };
    emit_lent_call(&mut w, &lends, &late, &call);
    w.line(format!("{}(_err)", py_checker_name(f, error)));

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
                    py_recv_expr(ty, "_ret", "_out_len.value")
                ));
            }
        }
    }
    out.push_str(&w.finish());
}

/// Emit the C call inside one `try`/`finally` per lent object (outermost
/// first), with the late statements (buffer finishing, callback
/// registration) just before the call.
fn emit_lent_call(
    w: &mut CodeWriter,
    lends: &[(String, String, String)],
    late: &[String],
    call: &str,
) {
    match lends.split_first() {
        None => {
            for line in late {
                w.line(line);
            }
            w.line(call);
        }
        Some(((local, acquire, release), rest)) => {
            w.line(format!("{local} = {acquire}"));
            w.line("try:");
            w.indent();
            emit_lent_call(w, rest, late, call);
            w.dedent();
            w.line("finally:");
            w.scope(|w| {
                w.line(release);
            });
        }
    }
}

// ── Callback interfaces ──

/// The Python spelling of a trampoline slot name: the ABI slot name,
/// keyword-escaped.
fn py_slot_name(slot: &str) -> String {
    py_local(slot)
}

/// The expression converting one trampoline parameter's C slots into the
/// value handed to the implementation. Strings, bytes, and buffers are
/// borrowed for the dispatch and copied; an object transfers one strong
/// reference, which the wrapper adopts.
fn py_trampoline_arg(p: &ParamBinding) -> String {
    let n = py_slot_name(&p.name);
    let data = || {
        format!(
            "_peek_bytes({}, {})",
            py_slot_name(&format!("{}_ptr", p.name)),
            py_slot_name(&format!("{}_len", p.name))
        )
    };
    match p.ty.family() {
        Family::String => format!("{}.decode(\"utf-8\")", data()),
        Family::Bytes => data(),
        Family::Buffer => py_decode_expr(&data(), &p.ty),
        Family::Object { nullable } => {
            let class = local_type_name(p.ty.interface_name().expect("object names an interface"));
            if nullable {
                format!("({class}._adopt({n}) if {n} else None)")
            } else {
                format!("{class}._adopt(_required({n}))")
            }
        }
        Family::Direct => match &p.ty {
            Ty::Enum(name) => format!("{}({n})", local_type_name(name)),
            _ => n,
        },
        Family::Callback | Family::Iterator => {
            unreachable!("callback interfaces and iterators are never callback arguments")
        }
    }
}

/// `(coercion, default)` for a callback method's direct return: the Python
/// conversion applied to the implementation's result (inside the `try`, so
/// a wrong type is reported like any other failure), and the value returned
/// after a failure. `None` for a void method.
fn py_trampoline_return(ret: Option<&Ty>) -> Option<(&'static str, &'static str)> {
    match ret {
        None => None,
        Some(Ty::Bool) => Some(("bool", "False")),
        Some(Ty::F32 | Ty::F64) => Some(("float", "0.0")),
        // Integers and C-style enums (an `IntEnum` is an `int`).
        Some(_) => Some(("int", "0")),
    }
}

/// Render one callback interface: the abstract base class the consumer
/// subclasses, then the ABI side satisfying
/// [`weaveffi_model::plan::CallbackProtocol`]: one `CFUNCTYPE` per method
/// plus `free`, the `ctypes.Structure` mirroring the C vtable, one
/// trampoline per method, and the single process-wide static vtable whose
/// function objects are pinned at module scope.
///
/// Each trampoline looks its implementation up by the integer `ctx`,
/// decodes the arguments, and calls the method. Any exception is reported
/// through `{prefix}_error_set(out_err, -4, message)` and a default value is
/// returned, so nothing unwinds through the C frame. `ctypes` acquires the
/// GIL on entry, so the producer may call from any thread.
pub(crate) fn render_callback_interface(
    out: &mut String,
    g: &Gen<'_>,
    cb: &CallbackInterfaceBinding,
) {
    let name = &cb.name;
    let mut w = CodeWriter::four_space();

    w.blank().blank();
    w.line(format!("class {name}(abc.ABC):"));
    w.indent();
    let base_doc = format!(
        "Consumer-implemented callback interface. Subclass it, implement every\n\
         abstract method, and pass an instance wherever the API takes a `{name}`;\n\
         the producer may call the methods from any thread until it releases the\n\
         instance. An exception raised by a method is reported to the producer,\n\
         which fails the call in progress with {}.FOREIGN_ERROR_CODE (-4).",
        g.root_error
    );
    let class_doc = match &cb.doc {
        Some(d) if !d.trim().is_empty() => format!("{}\n\n{base_doc}", d.trim()),
        _ => base_doc,
    };
    let mut doc = String::new();
    emit_docstring(&mut doc, &Some(class_doc), &w.indent_str());
    w.raw(doc);
    for m in &cb.methods {
        let mut sig: Vec<String> = vec!["self".into()];
        sig.extend(
            m.params
                .iter()
                .map(|p| format!("{}: {}", py_name(&p.name), py_type_hint(&p.ty))),
        );
        let ret = m
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".to_string());
        w.blank();
        w.line("@abc.abstractmethod");
        w.line(format!(
            "def {}({}) -> {ret}:",
            py_member_name(&m.name),
            sig.join(", ")
        ));
        w.indent();
        let mut mdoc = String::new();
        emit_fn_docstring(&mut mdoc, &m.doc, &m.params, &w.indent_str(), None);
        if mdoc.is_empty() {
            w.line("...");
        } else {
            w.raw(mdoc);
        }
        w.dedent();
    }
    w.dedent();

    // One function-pointer type per vtable entry, then the vtable layout:
    // methods in declaration order, then `free`.
    w.blank().blank();
    for m in &cb.methods {
        w.line(format!(
            "_{name}_{}_t = {}",
            m.name,
            cfunctype(&m.abi_ret, &m.abi_params)
        ));
    }
    w.line(format!(
        "_{name}_free_t = ctypes.CFUNCTYPE(None, ctypes.c_void_p)"
    ));
    w.blank().blank();
    w.line(format!("class _{name}Vtable(ctypes.Structure):"));
    w.scope(|w| {
        w.line(format!("\"\"\"The C vtable `{}`.\"\"\"", cb.vtable_tag));
        w.blank();
        w.line("_fields_ = [");
        w.scope(|w| {
            for m in &cb.methods {
                w.line(format!("(\"{0}\", _{name}_{0}_t),", m.name));
            }
            w.line(format!("(\"free\", _{name}_free_t),"));
        });
        w.line("]");
    });

    for m in &cb.methods {
        render_trampoline(&mut w, cb, m);
    }

    // The one static vtable. Each field keeps its function object alive, and
    // the vtable itself lives at module scope for the process lifetime.
    w.blank().blank();
    w.line(format!("_{name}_vtable = _{name}Vtable("));
    w.scope(|w| {
        for m in &cb.methods {
            w.line(format!("_{name}_{0}_t(_{name}_{0}),", m.name));
        }
        w.line(format!("_{name}_free_t(_callback_free),"));
    });
    w.line(")");
    out.push_str(&w.finish());
}

/// Render the trampoline for one callback method: a `def` whose parameters
/// are the vtable entry's C slots (`ctx`, the parameter slots, `out_err`).
fn render_trampoline(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, m: &CallbackMethodBinding) {
    let slots: Vec<String> = m.abi_params.iter().map(|p| py_slot_name(&p.name)).collect();
    let args: Vec<String> = m.params.iter().map(py_trampoline_arg).collect();
    let call = format!(
        "_callback_get(ctx).{}({})",
        py_member_name(&m.name),
        args.join(", ")
    );
    let ret = py_trampoline_return(m.ret.as_ref());
    let ret_hint = match ret {
        Some((coerce, _)) => coerce,
        None => "None",
    };
    w.blank().blank();
    w.line(format!(
        "def _{}_{}({}) -> {ret_hint}:",
        cb.name,
        m.name,
        slots.join(", ")
    ));
    w.scope(|w| {
        w.line("try:");
        w.scope(|w| match ret {
            Some((coerce, _)) => {
                w.line(format!("return {coerce}({call})"));
            }
            None => {
                w.line(&call);
            }
        });
        w.line("except BaseException as exc:");
        w.scope(|w| {
            w.line("_callback_fail(out_err, exc)");
            if let Some((_, default)) = ret {
                w.line(format!("return {default}"));
            }
        });
    });
}
