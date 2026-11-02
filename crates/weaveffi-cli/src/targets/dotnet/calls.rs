//! Call-path renderers: the wrapper methods for every call shape (sync,
//! async, iterator), the marshalling of every value by its [`Crossing`],
//! and the library class holding the free functions.
//!
//! Every value is one C value in every position, so four expressions cover
//! every crossing:
//!
//! * [`Marshal`] lends a parameter to the producer: a scalar as is, a
//!   `bool` as one byte, an optional scalar as its `FfiOpt*` struct, a
//!   string, bytes, typed array, or value buffer as an `FfiRun` over pinned
//!   memory, an object as its `SafeHandle`, and a callback interface as an
//!   `FfiCallback` of a pinned implementation and its static vtable;
//! * [`take`] receives a value the producer hands over (a return, an async
//!   result, an iterator item), releasing its run with `{p}_run_free`;
//! * [`borrow`] receives a callback method's argument, whose run stays the
//!   producer's;
//! * [`give`] hands a callback method's return to the producer, as a run
//!   from `{p}_alloc` or a fresh object reference.
//!
//! Every wrapper follows one frame: encode arguments (pooled UTF-8, pooled
//! value buffers, callback registrations), pin them with `fixed`, call the
//! import with a stack `FfiError`, check the error, and receive the result.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_model::model::{AsyncBinding, FnBinding, IteratorBinding, Model, ParamBinding};
use weaveffi_model::ty::{ParamTy, Prim, Ty};

use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use crate::targets::dotnet::codec::{read_lambda, write_call, write_lambda};
use crate::targets::dotnet::docs::Docs;
use crate::targets::dotnet::errors::ErrCtx;
use crate::targets::dotnet::types::{
    camel, cs_ctype, function_cs, opt_struct, owned_cs, param_cs, prim_cs, vtable_class_cs, Cx,
};

/// The `[UnmanagedCallersOnly]` attribute every native entry point carries.
pub(crate) const UNMANAGED_CALLERS_ONLY: &str =
    "[UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]";

/// The receiver of a wrapper: what the method is a member of.
#[derive(Clone, Copy)]
pub(crate) enum Receiver<'a> {
    /// A free function or interface static: a `static` method.
    Static,
    /// An interface instance method: passes `Handle` as `self`.
    Instance,
    /// The interface's `new` constructor: a real C# constructor of the
    /// named class.
    Constructor(&'a str),
}

/// The expression converting `v`, a C value of type `ty` the producer
/// handed over, to its C# value: a run is decoded and released, an object
/// reference adopted into a wrapper.
pub(crate) fn take(cx: Cx<'_>, ty: &Ty, v: &str) -> String {
    match crossing(ty) {
        Crossing::Utf8 => format!("Ffi.TakeString({v})"),
        Crossing::Bytes => format!("Ffi.TakeBytes({v})"),
        Crossing::Slice(p) => format!("Ffi.TakeArray<{}>({v})", prim_cs(p)),
        Crossing::Buffer(ty) => format!("Ffi.TakeBuffer({v}, {})", read_lambda(cx, ty, 0)),
        _ => direct_out(cx, ty, v),
    }
}

/// The expression converting `v`, a callback method's argument of type
/// `ty`, to its C# value: a run is borrowed for the call (a typed array or
/// bytes as a span over the producer's memory), and an object reference,
/// which moves to the consumer, is adopted.
pub(crate) fn borrow(cx: Cx<'_>, ty: &Ty, v: &str) -> String {
    match crossing(ty) {
        Crossing::Utf8 => format!("Ffi.ReadString({v})"),
        Crossing::Bytes => format!("Ffi.Borrow<byte>({v})"),
        Crossing::Slice(p) => format!("Ffi.Borrow<{}>({v})", prim_cs(p)),
        Crossing::Buffer(ty) => format!("Ffi.ReadBuffer({v}, {})", read_lambda(cx, ty, 0)),
        _ => direct_out(cx, ty, v),
    }
}

/// The C# value of a C value that isn't a run.
fn direct_out(cx: Cx<'_>, ty: &Ty, v: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(Prim::Bool) => format!("{v} != 0"),
        Crossing::Scalar(_) | Crossing::Enum(_) => v.to_string(),
        Crossing::OptScalar(_) => format!("{v}.ToNullable()"),
        Crossing::OptEnum(name) => format!("({}?){v}.ToNullable()", cx.ty(name)),
        Crossing::Object {
            interface,
            nullable,
        } => adopt(cx, v, nullable, interface),
        Crossing::Utf8 | Crossing::Bytes | Crossing::Slice(_) | Crossing::Buffer(_) => {
            unreachable!("a run is taken or borrowed")
        }
    }
}

/// The expression handing `v`, a callback method's C# return of type `ty`,
/// to the producer, which adopts it: a run allocated with `{p}_alloc`, or a
/// fresh strong reference. A null for a required object passes through as
/// null, which the producer rejects as a marshalling failure (-3).
pub(crate) fn give(ty: &Ty, v: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(Prim::Bool) => format!("Ffi.Bool({v})"),
        Crossing::Scalar(_) | Crossing::Enum(_) => v.to_string(),
        Crossing::OptScalar(p) => format!("{}.Of({v})", opt_struct(p)),
        Crossing::OptEnum(_) => format!("FfiOptI32.Of((int?){v})"),
        Crossing::Utf8 => format!("Ffi.ReturnString({v})"),
        Crossing::Bytes => format!("Ffi.ReturnBytes({v})"),
        Crossing::Slice(_) => format!("Ffi.ReturnArray({v})"),
        Crossing::Buffer(ty) => format!("Ffi.ReturnBuffer({v}, {})", write_lambda(ty, 0)),
        Crossing::Object { .. } => format!("{v}?.CloneHandle() ?? IntPtr.Zero"),
    }
}

/// The argument marshalling for one parameter list: statements run before
/// the call, the `fixed` pins it needs, one expression per ABI slot, and
/// the callback registrations to release if the call never reaches the
/// producer.
struct Marshal {
    setup: Vec<String>,
    pins: Vec<String>,
    args: Vec<String>,
    callbacks: Vec<String>,
}

impl Marshal {
    /// Plan the marshalling of `params`.
    fn new(cx: Cx<'_>, params: &[ParamBinding]) -> Self {
        let mut m = Marshal {
            setup: Vec::new(),
            pins: Vec::new(),
            args: Vec::new(),
            callbacks: Vec::new(),
        };
        let mut registrations = Vec::new();
        for p in params {
            let n = camel(&p.name);
            let local = p.name.to_lower_camel_case();
            let ty = match &p.ty {
                ParamTy::Value(ty) => ty,
                // The producer owns the registration once the call reaches
                // it, even when another argument fails: its vtable
                // `free(ctx)` releases it. An absent optional
                // implementation passes a null vtable.
                ParamTy::Callback { name, nullable } => {
                    let ctx = format!("{local}Ctx");
                    let pointer = format!("{}.Pointer", vtable_class_cs(name));
                    let vtable = if *nullable {
                        format!("{n} == null ? IntPtr.Zero : {pointer}")
                    } else {
                        pointer
                    };
                    m.args.push(format!("new FfiCallback({ctx}, {vtable})"));
                    registrations.push(format!("var {ctx} = Ffi.Register({n});"));
                    m.callbacks.push(ctx);
                    continue;
                }
            };
            match crossing(ty) {
                Crossing::Scalar(Prim::Bool) => m.args.push(format!("Ffi.Bool({n})")),
                Crossing::Scalar(_) | Crossing::Enum(_) => m.args.push(n),
                Crossing::OptScalar(p) => m.args.push(format!("{}.Of({n})", opt_struct(p))),
                Crossing::OptEnum(_) => m.args.push(format!("FfiOptI32.Of((int?){n})")),
                Crossing::Utf8 => {
                    m.setup
                        .push(format!("using var {local}Utf8 = new FfiUtf8({n});"));
                    m.pins.push(format!("byte* {local}Ptr = {local}Utf8"));
                    m.args
                        .push(format!("new FfiRun({local}Ptr, {local}Utf8.Length)"));
                }
                Crossing::Bytes => {
                    m.pins.push(format!("byte* {local}Ptr = {n}"));
                    m.args
                        .push(format!("new FfiRun({local}Ptr, (nuint){n}.Length)"));
                }
                // `len` counts bytes: the element count times its width.
                Crossing::Slice(p) => {
                    let size = p.size().expect("a typed array's element is a scalar");
                    m.pins.push(format!("{}* {local}Ptr = {n}", prim_cs(p)));
                    m.args.push(format!(
                        "new FfiRun((byte*){local}Ptr, (nuint){n}.Length * {size})"
                    ));
                }
                Crossing::Buffer(ty) => {
                    let writer = format!("{local}Writer");
                    m.setup
                        .push(format!("using var {writer} = new FfiBufferWriter();"));
                    m.setup.push(format!("{};", write_call(ty, &writer, &n, 0)));
                    m.pins.push(format!("byte* {local}Ptr = {writer}.Written"));
                    m.args
                        .push(format!("new FfiRun({local}Ptr, (nuint){writer}.Length)"));
                }
                Crossing::Object {
                    interface,
                    nullable,
                } => {
                    if nullable {
                        m.args.push(format!(
                            "{n}?.Handle ?? {}.NativeHandle.Null",
                            cx.ty(interface)
                        ));
                    } else {
                        m.args.push(format!("{n}.Handle"));
                    }
                }
            }
        }
        // Register last, so a failing encode above can't strand a
        // registration.
        m.setup.extend(registrations);
        m
    }

    /// Emit the setup statements.
    fn write_setup(&self, w: &mut CodeWriter) {
        for line in &self.setup {
            w.line(line);
        }
    }

    /// Emit `body` inside one stacked `fixed` statement per pin, or bare
    /// when nothing needs pinning.
    fn pinned(&self, w: &mut CodeWriter, body: impl FnOnce(&mut CodeWriter)) {
        if self.pins.is_empty() {
            body(w);
            return;
        }
        for pin in &self.pins {
            w.line(format!("fixed ({pin})"));
        }
        w.line("{");
        w.scope(body);
        w.line("}");
    }

    /// Emit `call` (a statement), releasing every callback registration if
    /// it throws before reaching the producer. `declare` is the declaration
    /// of the variable the statement assigns, when it assigns one.
    fn guarded(&self, w: &mut CodeWriter, declare: Option<String>, call: &str) {
        if self.callbacks.is_empty() {
            match declare {
                Some(_) => w.line(format!("var {call}")),
                None => w.line(call),
            };
            return;
        }
        if let Some(decl) = declare {
            w.line(decl);
        }
        w.line("try");
        w.block("{", "}", |w| {
            w.line(call);
        });
        w.line("catch");
        w.block("{", "}", |w| {
            self.unregister(w);
            w.line("throw;");
        });
    }

    /// The statements releasing every callback registration.
    fn unregister(&self, w: &mut CodeWriter) {
        for ctx in &self.callbacks {
            w.line(format!("Ffi.Unregister({ctx});"));
        }
    }
}

/// The public signature's parameter list.
fn params_sig(params: &[ParamBinding]) -> Vec<String> {
    params
        .iter()
        .map(|p| format!("{} {}", param_cs(&p.ty), camel(&p.name)))
        .collect()
}

/// The slot arguments of a call: the receiver (when any), every marshalled
/// parameter slot, then `extra`.
fn call_args(receiver: Receiver<'_>, m: &Marshal, extra: &[String]) -> String {
    let mut args = Vec::new();
    if matches!(receiver, Receiver::Instance) {
        args.push("Handle".to_string());
    }
    args.extend(m.args.iter().cloned());
    args.extend(extra.iter().cloned());
    args.join(", ")
}

/// The modifiers, return type, and name opening a method (or the bare class
/// name of a constructor).
fn opener(receiver: Receiver<'_>, ret: &str, name: &str) -> String {
    match receiver {
        Receiver::Static => format!("public static {ret} {name}"),
        Receiver::Instance => format!("public {ret} {name}"),
        Receiver::Constructor(class) => format!("public {class}"),
    }
}

/// The expression adopting an object pointer into a wrapper (`null` for a
/// null pointer of a nullable object).
fn adopt(cx: Cx<'_>, ptr: &str, nullable: bool, interface: &str) -> String {
    let class = cx.ty(interface);
    if nullable {
        format!("{ptr} == IntPtr.Zero ? null : {class}.Adopt({ptr})")
    } else {
        format!("{class}.Adopt({ptr})")
    }
}

/// Render one wrapper (any shape) named `name`.
pub(crate) fn render_callable(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    if let Some(a) = f.async_binding() {
        render_async(w, docs, f, a, name, receiver, err, cx);
    } else if let Some(it) = f.iterator() {
        render_iterator(w, docs, f, it, name, receiver, err, cx);
    } else {
        render_sync(w, docs, f, name, receiver, err, cx);
    }
}

/// The documentation, `<exception>`, and `[Obsolete]` lines of a wrapper.
fn write_header(w: &mut CodeWriter, docs: &Docs, f: &FnBinding, err: &ErrCtx) {
    docs.summary(w, &f.doc);
    for p in &f.params {
        docs.param(w, &camel(&p.name), &p.doc);
    }
    err.write_doc(w);
    docs.obsolete(w, &f.deprecated);
}

/// A synchronous wrapper: one call, one check, one received result. A
/// constructor stores the adopted pointer as its handle instead of
/// returning it.
fn render_sync(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    let value = f.ret_value();
    let ret = match (receiver, value) {
        (Receiver::Constructor(_), _) => String::new(),
        (_, Some(ty)) => owned_cs(ty),
        (_, None) => "void".into(),
    };
    let m = Marshal::new(cx, &f.params);
    let call = format!(
        "NativeMethods.{}({})",
        f.abi.symbol,
        call_args(receiver, &m, &["&ffiErr".to_string()])
    );

    write_header(w, docs, f, err);
    w.line(format!(
        "{}({})",
        opener(receiver, &ret, name),
        params_sig(&f.params).join(", ")
    ));
    w.block("{", "}", |w| {
        m.write_setup(w);
        w.line("var ffiErr = default(FfiError);");
        m.pinned(w, |w| {
            if value.is_some() {
                let decl = format!("{} ffiResult;", cs_ctype(cx.ns, &f.abi.ret));
                m.guarded(w, Some(decl), &format!("ffiResult = {call};"));
            } else {
                m.guarded(w, None, &format!("{call};"));
            }
            w.line(err.check("ffiErr"));
            match (value, receiver) {
                (Some(_), Receiver::Constructor(_)) => {
                    w.line("Handle = new NativeHandle(ffiResult);");
                }
                (Some(ty), _) => {
                    w.line(format!("return {};", take(cx, ty, "ffiResult")));
                }
                (None, _) => {}
            }
        });
    });
    w.blank();
}

/// An async wrapper: a `Task`-returning method launching the call with a
/// pending `FfiCall` as `context`, plus the `[UnmanagedCallersOnly]`
/// completion that resolves it. Every async method takes a
/// `CancellationToken`: a cancellable function links it to the native
/// cancel token; any other stops waiting when it fires.
#[allow(clippy::too_many_arguments)]
fn render_async(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    a: &AsyncBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    let value = f.ret_value();
    let result = value.map(owned_cs);
    let task = match &result {
        Some(t) => format!("Task<{t}>"),
        None => "Task".into(),
    };
    let call_ty = result.clone().unwrap_or_else(|| "FfiVoid".into());
    let complete = format!("FfiComplete{name}");
    let m = Marshal::new(cx, &f.params);
    let mut extra = Vec::new();
    if a.cancellable {
        extra.push("ffiCall.CancelToken()".to_string());
    }
    extra.push(format!("&{complete}"));
    extra.push("ffiCall.Context".to_string());
    let call = format!(
        "NativeMethods.{}({});",
        f.abi.symbol,
        call_args(receiver, &m, &extra)
    );

    write_header(w, docs, f, err);
    if a.cancellable {
        w.line("/// <param name=\"cancellationToken\">Cancels the native call; the task then");
        w.line("/// completes as canceled.</param>");
    } else {
        w.line("/// <param name=\"cancellationToken\">Stops waiting: the task completes as");
        w.line("/// canceled at once, and the native call, which can't be cancelled, finishes");
        w.line("/// in the background and releases its result.</param>");
    }
    let mut sig = params_sig(&f.params);
    sig.push("CancellationToken cancellationToken = default".into());
    let opener = match receiver {
        Receiver::Instance => format!("public {task} {name}"),
        _ => format!("public static {task} {name}"),
    };
    w.line(format!("{opener}({})", sig.join(", ")));
    w.block("{", "}", |w| {
        w.line("if (cancellationToken.IsCancellationRequested)");
        w.block("{", "}", |w| {
            let from = match &result {
                Some(t) => format!("Task.FromCanceled<{t}>(cancellationToken)"),
                None => "Task.FromCanceled(cancellationToken)".into(),
            };
            w.line(format!("return {from};"));
        });
        m.write_setup(w);
        w.line(format!(
            "var ffiCall = new FfiCall<{call_ty}>(cancellationToken);"
        ));
        w.line("try");
        w.block("{", "}", |w| {
            m.pinned(w, |w| {
                w.line(&call);
            });
        });
        w.line("catch");
        w.block("{", "}", |w| {
            w.line("ffiCall.Abandon();");
            m.unregister(w);
            w.line("throw;");
        });
        w.line("return ffiCall.Launched();");
    });
    w.blank();

    // The completion: fires exactly once, from any thread.
    let slots: Vec<String> = a
        .callback
        .params
        .iter()
        .map(|s| format!("{} {}", cs_ctype(cx.ns, &s.ty), s.name))
        .collect();
    w.line(UNMANAGED_CALLERS_ONLY);
    w.line(format!(
        "private static void {complete}({})",
        slots.join(", ")
    ));
    w.block("{", "}", |w| {
        w.line(format!(
            "var ffiCall = FfiCall<{call_ty}>.Complete(context);"
        ));
        w.line("try");
        w.block("{", "}", |w| {
            w.line("if (err != null)");
            w.block("{", "}", |w| {
                w.line(format!("ffiCall.SetError(err, {});", err.map));
                w.line("return;");
            });
            let received = match value {
                Some(ty) => take(cx, ty, "result"),
                None => "default".into(),
            };
            w.line(format!("ffiCall.SetResult({received});"));
        });
        w.line("catch (Exception e)");
        w.block("{", "}", |w| {
            w.line("ffiCall.SetException(e);");
        });
    });
    w.blank();
}

/// An iterator wrapper: a re-enumerable `IEnumerable<T>` whose every
/// enumeration launches a new native iterator through an `FfiLaunch{Name}`
/// helper and pulls one item per `MoveNext` through a static
/// `FfiNext{Name}`. Span parameters are copied once, since the sequence
/// outlives the call.
#[allow(clippy::too_many_arguments)]
fn render_iterator(
    w: &mut CodeWriter,
    docs: &Docs,
    f: &FnBinding,
    it: &IteratorBinding,
    name: &str,
    receiver: Receiver<'_>,
    err: &ErrCtx,
    cx: Cx<'_>,
) {
    let elem = owned_cs(&it.elem);
    let launch = format!("FfiLaunch{name}");
    let next = format!("FfiNext{name}");
    let m = Marshal::new(cx, &f.params);

    write_header(w, docs, f, err);
    w.line("/// <remarks>Streams lazily, one native call per item. Each enumeration launches a");
    w.line("/// new native iterator, so a failure to start throws from");
    w.line("/// <c>GetEnumerator</c> (the start of a <c>foreach</c>); disposing the enumerator");
    w.line("/// (as <c>foreach</c> does) releases it.</remarks>");
    let ret = format!("IEnumerable<{elem}>");
    w.line(format!(
        "{}({})",
        opener(receiver, &ret, name),
        params_sig(&f.params).join(", ")
    ));
    w.block("{", "}", |w| {
        let mut args = Vec::new();
        for p in &f.params {
            let n = camel(&p.name);
            let lent =
                p.ty.value()
                    .is_some_and(|ty| matches!(crossing(ty), Crossing::Slice(_) | Crossing::Bytes));
            if lent {
                let copy = format!("{}Copy", p.name.to_lower_camel_case());
                w.line(format!("var {copy} = {n}.ToArray();"));
                args.push(copy);
            } else {
                args.push(n);
            }
        }
        w.line(format!(
            "return new FfiSequence<{elem}>(() => {launch}({}), &{next});",
            args.join(", ")
        ));
    });
    w.blank();

    // The launcher: a sync call returning the native iterator.
    let call = format!(
        "NativeMethods.{}({})",
        f.abi.symbol,
        call_args(receiver, &m, &["&ffiErr".to_string()])
    );
    let modifier = match receiver {
        Receiver::Instance => "private",
        _ => "private static",
    };
    w.line(format!(
        "{modifier} FfiIteratorHandle {launch}({})",
        params_sig(&f.params).join(", ")
    ));
    w.block("{", "}", |w| {
        m.write_setup(w);
        w.line("var ffiErr = default(FfiError);");
        m.pinned(w, |w| {
            m.guarded(
                w,
                Some("IntPtr ffiResult;".into()),
                &format!("ffiResult = {call};"),
            );
            w.line(err.check("ffiErr"));
            w.line(format!(
                "return new FfiIteratorHandle(ffiResult, &NativeMethods.{});",
                it.destroy_symbol
            ));
        });
    });
    w.blank();

    // `_next`: one item per call; 0 means exhausted.
    let item_ty = cs_ctype(
        cx.ns,
        &weaveffi_model::abi::CType::of(&it.elem, weaveffi_model::abi::Position::Transfer),
    );
    w.line(format!(
        "private static bool {next}(FfiIteratorHandle iter, out {elem} item)"
    ));
    w.block("{", "}", |w| {
        w.line(format!("var ffiItem = default({item_ty});"));
        w.line("var ffiErr = default(FfiError);");
        w.line(format!(
            "var ffiMore = NativeMethods.{}(iter, &ffiItem, &ffiErr);",
            it.next.symbol
        ));
        w.line(err.check("ffiErr"));
        w.line("if (ffiMore == 0)");
        w.block("{", "}", |w| {
            w.line("item = default!;");
            w.line("return false;");
        });
        w.line(format!("item = {};", take(cx, &it.elem, "ffiItem")));
        w.line("return true;");
    });
    w.blank();
}

/// Render the free functions as static methods of the library class (the
/// generated half of the `partial` class the runtime declares with
/// `Check()` and `AbiVersion`).
pub(crate) fn render_functions(
    w: &mut CodeWriter,
    model: &Model,
    docs: &Docs,
    class: &str,
    cx: Cx<'_>,
) {
    if model.functions.is_empty() {
        return;
    }
    w.line(format!("public static unsafe partial class {class}"));
    w.block("{", "}", |w| {
        for f in &model.functions {
            let name = function_cs(&f.name, class);
            let err = ErrCtx::new(model, &f.error, cx);
            render_callable(w, docs, f, &name, Receiver::Static, &err, cx);
        }
    });
    w.blank();
}

/// The PascalCase spelling of a callback method.
pub(crate) fn method_cs(name: &str) -> String {
    name.to_upper_camel_case()
}
