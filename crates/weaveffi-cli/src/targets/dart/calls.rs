//! Callable rendering: the `lookupFunction` bindings and idiomatic Dart
//! wrappers for sync, async, and iterator callables.
//!
//! FFI signatures come straight from the model's lowered [`AbiFn`]s, so they
//! match the C header slot for slot. Parameter marshalling dispatches on the
//! stored [`ArgPass`], results on [`RetPass`], [`ResultPass`], and
//! [`ItemPass`], and error handling on [`ErrorStrategy`]; nothing here
//! re-derives a transport from a type.
//!
//! Every call runs in its own `_Frame` (the runtime's pooled per-call
//! scratch): its error slot, its return out slots, and the arena its
//! arguments are staged in belong to that call alone, so a call made from a
//! callback during another call never disturbs the outer one.

use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{AbiFn, AsyncBinding, FnBinding, IteratorBinding, Model};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{Prim, Ty};

use crate::targets::dart::callbacks::{dispatch_fn, vtable_var};
use crate::targets::dart::codec::{pack_fn, unpack_fn};
use crate::targets::dart::docs::Docs;
use crate::targets::dart::types::{
    dart_class, dart_ident, dart_type, exception_class, ffi_type, ffi_typedef, ffi_var,
    object_class, param_type, return_type, slice_fn, zero_literal,
};

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
        let (natives, _) = slot_types(&a.callback_params, None);
        w.blank();
        w.line(format!(
            "typedef {cb} = Void Function({});",
            natives.join(", ")
        ));
        emit_lookup(w, &f.abi, false, Some(&cb));
    } else if let Some(it) = f.iterator() {
        emit_lookup(w, &f.abi, false, None);
        emit_lookup(w, &it.next, false, None);
        emit_destroy(w, &it.destroy_symbol, false);
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

/// The (native, Dart) FFI types of `params`, substituting `callback` for a
/// bare named slot (an async launcher's completion callback).
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
/// Callback registrations run last, after everything that can throw, so a
/// failed staging step never strands a registered implementation.
fn marshal_inputs(f: &FnBinding) -> Inputs {
    let mut stage = Vec::new();
    let mut registrations = Vec::new();
    let mut args = Vec::new();
    if f.has_self() {
        args.push("_self".into());
    }
    for p in &f.params {
        let name = dart_ident(&p.name);
        let value = p.ty.value();
        match &p.pass {
            ArgPass::Direct { .. } => args.push(match value {
                Some(Ty::Enum(_)) => format!("{name}.value"),
                _ => name,
            }),
            ArgPass::OptDirect { inner, .. } => {
                args.push(format!("{name} != null"));
                args.push(match inner {
                    Ty::Enum(_) => format!("{name}?.value ?? 0"),
                    other => format!("{name} ?? {}", zero_literal(other)),
                });
            }
            ArgPass::Slice { elem, .. } => {
                args.push(format!("{}(_f.arena, {name})", slice_fn("stage", *elem)));
                args.push(format!("{name}.length"));
            }
            ArgPass::String { .. } => {
                stage.push(format!("final _{name}Bytes = utf8.encode({name});"));
                args.push(format!("_stage(_f.arena, _{name}Bytes)"));
                args.push(format!("_{name}Bytes.length"));
            }
            ArgPass::Bytes { .. } => {
                args.push(format!("_stage(_f.arena, {name})"));
                args.push(format!("{name}.length"));
            }
            ArgPass::Buffer { .. } => {
                let ty = value.expect("a buffer parameter is a value");
                stage.push(format!(
                    "final _{name}Bytes = _encode({name}, {});",
                    pack_fn(ty)
                ));
                args.push(format!("_stage(_f.arena, _{name}Bytes)"));
                args.push(format!("_{name}Bytes.length"));
            }
            // The wrapper keeps its reference and is borrowed for the call.
            ArgPass::Object { nullable, .. } => {
                let ptr = format!("_{name}Ptr");
                stage.push(if *nullable {
                    format!("final {ptr} = {name} == null ? nullptr : _borrow(_f.arena, {name});")
                } else {
                    format!("final {ptr} = _borrow(_f.arena, {name});")
                });
                args.push(ptr);
            }
            // The producer owns the registration and releases it through the
            // vtable's `free`; a null vtable means no implementation.
            ArgPass::Callback {
                nullable,
                interface,
                ..
            } => {
                let ctx = format!("_{name}Ctx");
                let register = format!("_registerCallback({name}, {})", dispatch_fn(interface));
                let vtable = format!("{}.cast<Void>()", vtable_var(interface));
                if *nullable {
                    registrations.push(format!(
                        "final {ctx} = {name} == null ? nullptr : {register};"
                    ));
                    args.push(ctx);
                    args.push(format!("{name} == null ? nullptr : {vtable}"));
                } else {
                    registrations.push(format!("final {ctx} = {register};"));
                    args.push(ctx);
                    args.push(vtable);
                }
            }
        }
    }
    stage.extend(registrations);
    Inputs { stage, args }
}

/// How one value arrives from the producer: the expressions holding its
/// slots, per its transport.
pub(crate) enum Arrival {
    /// A scalar (or C-style enum discriminant).
    Direct(String),
    /// A presence flag and a scalar (ignored when absent).
    OptDirect {
        /// The presence flag.
        has: String,
        /// The scalar.
        value: String,
    },
    /// A typed array and its element count.
    Slice {
        /// The array pointer.
        ptr: String,
        /// The element count.
        len: String,
        /// The element type.
        elem: Prim,
    },
    /// A UTF-8 run.
    String {
        /// The run pointer.
        ptr: String,
        /// The byte length.
        len: String,
    },
    /// A byte run.
    Bytes {
        /// The run pointer.
        ptr: String,
        /// The byte length.
        len: String,
    },
    /// A value buffer.
    Buffer {
        /// The run pointer.
        ptr: String,
        /// The byte length.
        len: String,
    },
    /// An object reference (always owned by the receiver).
    Object {
        /// The object pointer.
        ptr: String,
        /// Whether a null pointer means no object.
        nullable: bool,
    },
}

/// The Dart expression turning an arriving value of type `ty` into its
/// surface value. `owned` runs and arrays are copied and released; borrowed
/// ones (a callback's arguments) are copied. Objects are always adopted.
pub(crate) fn receive(ty: &Ty, arrival: Arrival, owned: bool) -> String {
    let enum_of = |t: &Ty, v: &str| match t {
        Ty::Enum(n) => format!("{}.fromValue({v})", dart_class(n)),
        _ => v.to_string(),
    };
    match arrival {
        Arrival::Direct(v) => enum_of(ty, &v),
        Arrival::OptDirect { has, value } => {
            let inner = match ty {
                Ty::Optional(inner) => inner.as_ref(),
                other => other,
            };
            format!("{has} ? {} : null", enum_of(inner, &value))
        }
        Arrival::Slice { ptr, len, elem } => {
            let op = if owned { "take" } else { "copy" };
            format!("{}({ptr}, {len})", slice_fn(op, elem))
        }
        Arrival::String { ptr, len } => {
            let op = if owned { "_takeString" } else { "_readString" };
            format!("{op}({ptr}, {len})")
        }
        Arrival::Bytes { ptr, len } => format!("{}({ptr}, {len})", bytes_fn(owned)),
        Arrival::Buffer { ptr, len } => format!(
            "_decode({}({ptr}, {len}), {})",
            bytes_fn(owned),
            unpack_fn(ty)
        ),
        Arrival::Object { ptr, nullable } => adopt_expr(&ptr, ty, nullable),
    }
}

fn bytes_fn(owned: bool) -> &'static str {
    if owned {
        "_takeBytes"
    } else {
        "_copyBytes"
    }
}

/// The expression adopting the owned object pointer `expr` into its wrapper;
/// a null pointer becomes `null` when `nullable`.
pub(crate) fn adopt_expr(expr: &str, ty: &Ty, nullable: bool) -> String {
    let class = object_class(ty.interface_name().expect("objects are interfaces"));
    if nullable {
        format!("{expr} == nullptr ? null : {class}._({expr})")
    } else {
        format!("{class}._({expr})")
    }
}

/// The Dart expression reading out slot `param` (a `T*`) through the frame
/// slot `slot`.
fn read_slot(slot: &str, param: &AbiParam) -> String {
    match &param.ty {
        CType::Ptr { pointee, .. } => format!("{slot}.cast<{}>().value", ffi_type(pointee).0),
        other => unreachable!("out slot {} is a {other:?}, not a pointer", param.name),
    }
}

/// The frame slot holding out slot number `i` of a call.
fn frame_slot(f: &str, i: usize) -> String {
    format!("{f}.slot{i}")
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

/// The value type of `f`'s return.
fn ret_value(f: &FnBinding) -> &Ty {
    f.ret
        .as_ref()
        .and_then(|r| r.value())
        .expect("a value return")
}

/// A synchronous wrapper: stage, call, check, receive.
fn render_sync(w: &mut CodeWriter, f: &FnBinding, kind: &DartDecl, name: &str, throws: &Throws) {
    let mut inputs = marshal_inputs(f);
    let outs = f.ret_pass.out_slots();
    for i in 0..outs.len() {
        inputs.args.push(format!("{}.cast()", frame_slot("_f", i)));
    }
    inputs.args.push("_f.err".into());
    let call = format!("{}({})", ffi_var(&f.abi.symbol), inputs.args.join(", "));
    let slot = |i: usize| read_slot(&frame_slot("_f", i), outs[i]);
    w.block(
        kind.open_line(&return_type(f), name, &wrapper_params(f), ""),
        "}",
        |w| {
            framed(w, f.has_self(), |w| {
                for s in &inputs.stage {
                    w.line(s);
                }
                let result = match &f.ret_pass {
                    RetPass::Void => {
                        w.line(format!("{call};"));
                        w.line(throws.check("_f"));
                        return;
                    }
                    RetPass::Direct => Arrival::Direct("_result".into()),
                    RetPass::OptDirect { .. } => Arrival::OptDirect {
                        has: "_present".into(),
                        value: slot(0),
                    },
                    RetPass::Slice { elem, .. } => Arrival::Slice {
                        ptr: "_result".into(),
                        len: slot(0),
                        elem: *elem,
                    },
                    RetPass::String { .. } => Arrival::String {
                        ptr: "_result".into(),
                        len: slot(0),
                    },
                    RetPass::Bytes { .. } => Arrival::Bytes {
                        ptr: "_result".into(),
                        len: slot(0),
                    },
                    RetPass::Buffer { .. } => Arrival::Buffer {
                        ptr: "_result".into(),
                        len: slot(0),
                    },
                    RetPass::Object { nullable, .. } => Arrival::Object {
                        ptr: "_result".into(),
                        nullable: *nullable,
                    },
                    RetPass::Iterator(_) => unreachable!("iterators render separately"),
                };
                let local = match &result {
                    Arrival::OptDirect { .. } => "_present",
                    _ => "_result",
                };
                w.line(format!("final {local} = {call};"));
                w.line(throws.check("_f"));
                w.line(format!("return {};", receive(ret_value(f), result, true)));
            });
        },
    );
}

/// An async wrapper: a `Future` completed from a `NativeCallable.listener`
/// the producer invokes exactly once, from any thread. Results and errors
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
    // `context`, `err`, then the result's.
    let (_, darts) = slot_types(&a.callback_params, None);
    let names: Vec<String> = a
        .callback_params
        .iter()
        .map(|p| dart_ident(&p.name))
        .collect();
    let listener_params: Vec<String> = darts
        .iter()
        .zip(&names)
        .map(|(d, n)| format!("{d} {n}"))
        .collect();
    let err = &names[1];
    let n = |p: &AbiParam| dart_ident(&p.name);
    let result = match &a.result {
        ResultPass::Void => None,
        ResultPass::Direct { result } => Some(Arrival::Direct(n(result))),
        ResultPass::OptDirect { has, value } => Some(Arrival::OptDirect {
            has: n(has),
            value: n(value),
        }),
        ResultPass::Slice { ptr, len, elem } => Some(Arrival::Slice {
            ptr: n(ptr),
            len: n(len),
            elem: *elem,
        }),
        ResultPass::String { ptr, len } => Some(Arrival::String {
            ptr: n(ptr),
            len: n(len),
        }),
        ResultPass::Bytes { ptr, len } => Some(Arrival::Bytes {
            ptr: n(ptr),
            len: n(len),
        }),
        ResultPass::Buffer { ptr, len } => Some(Arrival::Buffer {
            ptr: n(ptr),
            len: n(len),
        }),
        ResultPass::Object {
            result, nullable, ..
        } => Some(Arrival::Object {
            ptr: n(result),
            nullable: *nullable,
        }),
    };
    let complete = match result {
        None => "_completer.complete();".to_string(),
        Some(arrival) => format!(
            "_completer.complete({});",
            receive(ret_value(f), arrival, true)
        ),
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
                    "if ({err} != nullptr) throw _takeAsyncError({err}, {});",
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
    let slots = it.item.slots();
    let mut next_args = vec!["_iter".to_string()];
    next_args.extend((0..slots.len()).map(|i| format!("{}.cast()", frame_slot("_step", i))));
    next_args.push("_step.err".into());
    let slot = |i: usize| read_slot(&frame_slot("_step", i), slots[i]);
    let item = match &it.item {
        ItemPass::Direct { .. } => Arrival::Direct(slot(0)),
        ItemPass::OptDirect { .. } => Arrival::OptDirect {
            has: slot(0),
            value: slot(1),
        },
        ItemPass::Slice { elem, .. } => Arrival::Slice {
            ptr: slot(0),
            len: slot(1),
            elem: *elem,
        },
        ItemPass::String { .. } => Arrival::String {
            ptr: slot(0),
            len: slot(1),
        },
        ItemPass::Bytes { .. } => Arrival::Bytes {
            ptr: slot(0),
            len: slot(1),
        },
        ItemPass::Buffer { .. } => Arrival::Buffer {
            ptr: slot(0),
            len: slot(1),
        },
        ItemPass::Object { nullable, .. } => Arrival::Object {
            ptr: slot(0),
            nullable: *nullable,
        },
    };
    let item = receive(&it.elem, item, true);
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
                            "final _more = {}({});",
                            ffi_var(&it.next.symbol),
                            next_args.join(", ")
                        ));
                        w.line(throws.check("_step"));
                        w.line("if (_more == 0) break;");
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
