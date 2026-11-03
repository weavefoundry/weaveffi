//! Callable rendering: the `lookupFunction` bindings and idiomatic Dart
//! wrappers for sync, async, and iterator callables.
//!
//! FFI signatures come straight from the model's lowered [`AbiFn`]s, so they
//! match the C header slot for slot. Every value is one C value, marshalled
//! by its [`Crossing`](crate::codegen::values::Crossing) through
//! [`values`](super::values), and failures are mapped by the callable's
//! [`ErrorStrategy`]; nothing here re-derives a transport from a type.
//!
//! Every call runs in its own `_Frame` (the runtime's pooled per-call
//! scratch): its error slot, its out slot, and the arena its arguments are
//! staged in belong to that call alone, so a call made from a callback
//! during another call never disturbs the outer one.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiFn, AbiParam, CType};
use weaveffi_model::model::{AsyncBinding, ErrorStrategy, FnBinding, IteratorBinding, Model};
use weaveffi_model::ty::ParamTy;

use crate::targets::dart::callbacks::{dispatch_fn, vtable_var};
use crate::targets::dart::docs::Docs;
use crate::targets::dart::types::{
    dart_ident, dart_type, exception_class, ffi_type, ffi_typedef, ffi_var, param_type, return_type,
};
use crate::targets::dart::values::{is_struct, receive, stage, stages};

/// How a callable's failures surface, resolved from its [`ErrorStrategy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Throws {
    /// Not declared to throw: a failure is a producer bug, raised as a
    /// `NativeError`.
    Trap,
    /// `throws: Domain`: the domain's exception class (open: an unknown
    /// positive code is the base class itself).
    Domain(String),
    /// `throws: any`: a `NativeException` carrying -1 and the message.
    Untyped,
}

impl Throws {
    /// The surface of `error`, naming a domain's exception class.
    pub(crate) fn of(model: &Model, error: &ErrorStrategy) -> Self {
        match error {
            ErrorStrategy::Trap => Self::Trap,
            ErrorStrategy::Domain(name) => {
                Self::Domain(exception_class(&model.error_domain(name).name))
            }
            ErrorStrategy::Untyped => Self::Untyped,
        }
    }

    /// The `_ErrorMapper` a failure goes through.
    pub(crate) fn mapper(&self) -> String {
        match self {
            Self::Trap => "_trap".into(),
            Self::Domain(exc) => mapper_fn(exc),
            Self::Untyped => "_runtimeException".into(),
        }
    }

    /// The statement checking frame `f`'s error slot after a call.
    fn check(&self, f: &str) -> String {
        match self {
            Self::Trap => format!("{f}.check();"),
            other => format!("{f}.check({});", other.mapper()),
        }
    }
}

/// The `_map{Exception}` mapper of a domain exception class.
pub(crate) fn mapper_fn(exception: &str) -> String {
    format!("_map{exception}")
}

/// How one wrapper is declared: a top-level function, or a member of an
/// interface class.
pub(crate) enum DartDecl<'a> {
    /// A top-level function.
    TopLevel,
    /// An instance method: the call borrows the wrapper's own pointer.
    Method,
    /// A `static` method of an interface class.
    Static,
    /// A factory constructor; `named` is `false` for the canonical `new`.
    Factory {
        /// The interface class the factory constructs.
        class_name: &'a str,
        /// `false` for the canonical `new` constructor.
        named: bool,
    },
}

impl DartDecl<'_> {
    /// The declaration's opening line (through the `{`). `ret` is the public
    /// return type and `suffix` an optional body modifier (`sync*`).
    fn open_line(&self, ret: &str, name: &str, params: &str, suffix: &str) -> String {
        let suffix = if suffix.is_empty() {
            String::new()
        } else {
            format!(" {suffix}")
        };
        match self {
            DartDecl::TopLevel | DartDecl::Method => format!("{ret} {name}({params}){suffix} {{"),
            DartDecl::Static => format!("static {ret} {name}({params}){suffix} {{"),
            DartDecl::Factory {
                class_name,
                named: false,
            } => format!("factory {class_name}({params}) {{"),
            DartDecl::Factory {
                class_name,
                named: true,
            } => format!("factory {class_name}.{name}({params}) {{"),
        }
    }
}

/// Bind every C symbol `f` calls, at top level. `leaf` marks synchronous
/// calls as leaf calls (only sound when the API declares no callback
/// interfaces, so no call can re-enter Dart).
pub(crate) fn emit_bindings(w: &mut CodeWriter, f: &FnBinding, leaf: bool) {
    if let Some(a) = f.async_binding() {
        let cb = ffi_typedef(&a.callback_type);
        let (natives, _) = slot_types(&a.callback.params, None);
        w.blank();
        w.line(format!(
            "typedef {cb} = Void Function({});",
            natives.join(", ")
        ));
        emit_lookup(w, &f.abi, false, Some(&cb));
    } else if let Some(it) = f.iterator() {
        emit_lookup(w, &f.abi, leaf, None);
        emit_lookup(w, &it.next, leaf, None);
        emit_destroy(w, &it.destroy_symbol, leaf);
    } else {
        emit_lookup(w, &f.abi, leaf, None);
    }
}

/// Render `f`'s wrapper, declared as `kind` and named `name`.
pub(crate) fn emit_wrapper(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    f: &FnBinding,
    kind: &DartDecl,
    name: &str,
) {
    let throws = Throws::of(model, &f.error);
    w.blank();
    docs.write_wrapper(w, f, &throws);
    if let Some(a) = f.async_binding() {
        render_async(w, f, a, kind, name, &throws);
    } else if let Some(it) = f.iterator() {
        render_iterator(w, f, it, kind, name, &throws);
    } else {
        render_sync(w, f, kind, name, &throws);
    }
}

/// The (native, Dart) FFI types of `params`, substituting a pointer to
/// `callback` for a bare named slot (an async launcher's completion).
fn slot_types(params: &[AbiParam], callback: Option<&str>) -> (Vec<String>, Vec<String>) {
    params
        .iter()
        .map(|p| match (&p.ty, callback) {
            (CType::Named(_), Some(cb)) => {
                let t = format!("Pointer<NativeFunction<{cb}>>");
                (t.clone(), t)
            }
            (ty, _) => ffi_type(ty),
        })
        .unzip()
}

/// Bind one C symbol with `lookupFunction`.
fn emit_lookup(w: &mut CodeWriter, abi: &AbiFn, leaf: bool, callback: Option<&str>) {
    let (natives, darts) = slot_types(&abi.params, callback);
    let (ret_n, ret_d) = ffi_type(&abi.ret);
    lookup(
        w,
        &abi.symbol,
        &format!("{ret_n} Function({})", natives.join(", ")),
        &format!("{ret_d} Function({})", darts.join(", ")),
        leaf,
    );
}

/// Emit `final _sym = _lib.lookupFunction<...>('sym');`, binding C symbol
/// `sym` with the given native and Dart function types.
pub(crate) fn lookup(w: &mut CodeWriter, sym: &str, native: &str, dart: &str, leaf: bool) {
    let leaf = if leaf { ", isLeaf: true" } else { "" };
    w.blank();
    w.line(format!("final {} = _lib.lookupFunction<", ffi_var(sym)));
    w.line(format!("    {native},"));
    w.line(format!("    {dart}>('{sym}'{leaf});"));
}

/// Bind a destroy symbol `sym` (`void (*)(void*)`) and the `NativeFinalizer`
/// over it, named after the binding.
pub(crate) fn emit_destroy(w: &mut CodeWriter, sym: &str, leaf: bool) {
    lookup(
        w,
        sym,
        "Void Function(Pointer<Void>)",
        "void Function(Pointer<Void>)",
        leaf,
    );
    w.line(format!(
        "final {}Finalizer = NativeFinalizer(",
        ffi_var(sym)
    ));
    w.line(format!(
        "    _lib.lookup<NativeFunction<Void Function(Pointer<Void>)>>('{sym}'));"
    ));
}

/// The wrapper's parameter list: the IDL parameters, plus a named
/// `cancelToken` for a cancellable async call.
fn wrapper_params(f: &FnBinding) -> String {
    let mut params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{} {}", param_type(&p.ty), dart_ident(&p.name)))
        .collect();
    if f.cancellable() {
        params.push(format!("{{CancelToken? {}}}", cancel_param(f)));
    }
    params.join(", ")
}

/// The name of a cancellable call's token parameter: `cancelToken`, unless
/// an IDL parameter already took it.
fn cancel_param(f: &FnBinding) -> &'static str {
    if f.params
        .iter()
        .any(|p| dart_ident(&p.name) == "cancelToken")
    {
        "cancelToken_"
    } else {
        "cancelToken"
    }
}

/// The marshalling of a callable's inputs into frame `_f`: staging
/// statements and call arguments.
struct Inputs {
    /// Statements run before the call (staging, borrowing, registering).
    stage: Vec<String>,
    /// The call's argument expressions, in ABI order.
    args: Vec<String>,
}

impl Inputs {
    /// Whether staging allocates from (or borrows through) the frame's
    /// arena.
    fn uses_arena(&self) -> bool {
        self.stage
            .iter()
            .chain(&self.args)
            .any(|s| s.contains("_f.arena"))
    }
}

/// Marshal `f`'s inputs. An instance method's call passes `_self` first.
/// Every value that can fail to stage is staged into a local first, and
/// callback registrations run last, after everything that can throw, so a
/// failed staging step never strands a registered implementation.
fn marshal_inputs(f: &FnBinding) -> Inputs {
    let mut stage_lines = Vec::new();
    let mut registrations = Vec::new();
    let mut args = Vec::new();
    if f.has_self() {
        args.push("_self".into());
    }
    for p in &f.params {
        let name = dart_ident(&p.name);
        match &p.ty {
            ParamTy::Value(ty) if stages(ty) => {
                let local = format!("_{name}Arg");
                stage_lines.push(format!("final {local} = {};", stage(ty, &name, "_f.arena")));
                args.push(local);
            }
            ParamTy::Value(ty) => args.push(stage(ty, &name, "_f.arena")),
            // The producer adopts the registration and releases it through
            // the vtable's `free`; a null vtable means no implementation.
            ParamTy::Callback {
                name: interface,
                nullable,
            } => {
                let local = format!("_{name}Arg");
                let register = format!(
                    "_registerCallback({name}, {}, {}.cast())",
                    dispatch_fn(interface),
                    vtable_var(interface)
                );
                registrations.push(if *nullable {
                    format!("final {local} = {name} == null ? _noCallback() : {register};")
                } else {
                    format!("final {local} = {register};")
                });
                args.push(local);
            }
        }
    }
    stage_lines.extend(registrations);
    Inputs {
        stage: stage_lines,
        args,
    }
}

/// Write `body` inside the try/finally that scopes `_self` and frame `_f`.
fn framed(w: &mut CodeWriter, has_self: bool, body: impl FnOnce(&mut CodeWriter)) {
    if has_self {
        w.line("final _self = _enter();");
    }
    w.line("final _f = _Frame.take();");
    w.line("try {");
    w.scope(body);
    w.line("} finally {");
    w.scope(|w| {
        w.line("_f.release();");
        if has_self {
            w.line("_leave();");
        }
    });
    w.line("}");
}

/// A synchronous wrapper: stage, call, check, receive.
fn render_sync(w: &mut CodeWriter, f: &FnBinding, kind: &DartDecl, name: &str, throws: &Throws) {
    let mut inputs = marshal_inputs(f);
    inputs.args.push("_f.err".into());
    let call = format!("{}({})", ffi_var(&f.abi.symbol), inputs.args.join(", "));
    w.block(
        kind.open_line(&return_type(f), name, &wrapper_params(f), ""),
        "}",
        |w| {
            framed(w, f.has_self(), |w| {
                for s in &inputs.stage {
                    w.line(s);
                }
                match f.ret_value() {
                    None => {
                        w.line(format!("{call};"));
                        w.line(throws.check("_f"));
                    }
                    Some(ret) => {
                        w.line(format!("final _result = {call};"));
                        w.line(throws.check("_f"));
                        w.line(format!("return {};", receive(ret, "_result", true)));
                    }
                }
            });
        },
    );
}

/// An async wrapper: a `Future` completed from a `NativeCallable.listener`
/// the producer invokes exactly once, from any thread. The result and error
/// it receives are owned, so decoding them later on the event loop is safe.
/// A cancellable call binds a native token to its `CancelToken` for the
/// call's duration.
fn render_async(
    w: &mut CodeWriter,
    f: &FnBinding,
    a: &AsyncBinding,
    kind: &DartDecl,
    name: &str,
    throws: &Throws,
) {
    let cb = ffi_typedef(&a.callback_type);
    let ret = return_type(f);
    let mut inputs = marshal_inputs(f);
    if f.cancellable() {
        inputs.args.push("_cancel?.pointer ?? nullptr".into());
    }
    inputs.args.push("_callback.nativeFunction".into());
    inputs.args.push("nullptr".into());
    let call = format!("{}({});", ffi_var(&f.abi.symbol), inputs.args.join(", "));

    // The listener's parameters, named after the completion's slots:
    // `context`, `err`, then `result`.
    let (_, darts) = slot_types(&a.callback.params, None);
    let listener_params: Vec<String> = darts
        .iter()
        .zip(&a.callback.params)
        .map(|(d, p)| format!("{d} {}", dart_ident(&p.name)))
        .collect();
    let complete = match f.ret_value() {
        None => "_completer.complete();".to_string(),
        Some(ty) => format!("_completer.complete({});", receive(ty, "result", true)),
    };

    let open = kind.open_line(&format!("Future<{ret}>"), name, &wrapper_params(f), "");
    let has_self = f.has_self();
    w.block(open, "}", |w| {
        if has_self {
            w.line("final _self = _enter();");
        }
        w.line(format!("final _completer = Completer<{ret}>();"));
        if f.cancellable() {
            w.line(format!(
                "final _cancel = _NativeCancel.bind({});",
                cancel_param(f)
            ));
        }
        w.line(format!("late final NativeCallable<{cb}> _callback;"));
        w.line(format!("_callback = NativeCallable<{cb}>.listener(("));
        w.line(format!("    {}) {{", listener_params.join(", ")));
        w.scope(|w| {
            w.line("_callback.close();");
            if f.cancellable() {
                w.line("_cancel?.release();");
            }
            w.line("try {");
            w.scope(|w| {
                w.line(format!(
                    "if (err != nullptr) throw _takeAsyncError(err, {});",
                    throws.mapper()
                ));
                w.line(&complete);
            });
            w.line("} catch (e, s) {");
            w.scope(|w| {
                w.line("_completer.completeError(e, s);");
            });
            w.line("}");
        });
        w.line("});");
        // Staged arguments live in a frame's arena until the launcher returns.
        let arena = inputs.uses_arena();
        if arena {
            w.line("final _f = _Frame.take();");
        }
        w.line("try {");
        w.scope(|w| {
            for s in &inputs.stage {
                w.line(s);
            }
            w.line(&call);
        });
        w.line("} catch (_) {");
        w.scope(|w| {
            w.line("_callback.close();");
            if f.cancellable() {
                w.line("_cancel?.release();");
            }
            w.line("rethrow;");
        });
        if arena || has_self {
            w.line("} finally {");
            w.scope(|w| {
                if arena {
                    w.line("_f.release();");
                }
                if has_self {
                    w.line("_leave();");
                }
            });
        }
        w.line("}");
        w.line("return _completer.future;");
    });
}

/// An iterator wrapper: a lazy `sync*` body that launches the native
/// iterator on the first pull, issues one `next` per element (each in its
/// own frame, released before the element is yielded), and destroys the
/// iterator exactly once (eagerly on completion or failure, or through the
/// finalizer when the iteration is abandoned).
fn render_iterator(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    kind: &DartDecl,
    name: &str,
    throws: &Throws,
) {
    let mut inputs = marshal_inputs(f);
    inputs.args.push("_f.err".into());
    let launch = format!("{}({})", ffi_var(&f.abi.symbol), inputs.args.join(", "));
    let out = &it.next.params[1];
    let CType::Ptr { pointee, .. } = &out.ty else {
        unreachable!("an iterator's out_item is a pointer");
    };
    let slot = format!("_step.out.cast<{}>()", ffi_type(pointee).0);
    let read = if is_struct(&it.elem) {
        format!("{slot}.ref")
    } else {
        format!("{slot}.value")
    };
    let item = receive(&it.elem, "_out", true);
    let destroy = ffi_var(&it.destroy_symbol);

    w.block(
        kind.open_line(&return_type(f), name, &wrapper_params(f), "sync*"),
        "}",
        |w| {
            w.line("final Pointer<Void> _iter;");
            framed(w, f.has_self(), |w| {
                for s in &inputs.stage {
                    w.line(s);
                }
                w.line(format!("_iter = {launch};"));
                w.line(throws.check("_f"));
            });
            w.line("final _anchor = _IteratorAnchor();");
            w.line(format!(
                "{destroy}Finalizer.attach(_anchor, _iter, detach: _anchor);"
            ));
            w.line("try {");
            w.scope(|w| {
                w.block("while (true) {", "}", |w| {
                    w.line("final _step = _Frame.take();");
                    w.line(format!("final {} _item;", dart_type(&it.elem)));
                    w.line("try {");
                    w.scope(|w| {
                        w.line(format!(
                            "final _more = {}(_iter, _step.out.cast(), _step.err);",
                            ffi_var(&it.next.symbol),
                        ));
                        w.line(throws.check("_step"));
                        w.line("if (_more == 0) break;");
                        w.line(format!("final _out = {read};"));
                        w.line(format!("_item = {item};"));
                    });
                    w.line("} finally {");
                    w.scope(|w| {
                        w.line("_step.release();");
                    });
                    w.line("}");
                    w.line("yield _item;");
                });
            });
            w.line("} finally {");
            w.scope(|w| {
                w.line(format!("{destroy}Finalizer.detach(_anchor);"));
                w.line(format!("{destroy}(_iter);"));
            });
            w.line("}");
        },
    );
}
