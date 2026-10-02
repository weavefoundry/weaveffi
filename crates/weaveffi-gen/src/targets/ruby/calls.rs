//! Callable rendering: FFI attachments, async completion trampolines, the
//! sync, async, and iterator wrapper bodies, and the parameter and return
//! marshalling they share.
//!
//! Marshalling dispatch goes through the shared plan layer ([`ArgPass`],
//! [`RetPass`]), so this backend can't drift from the others on
//! call-boundary semantics: strings, bytes, and value buffers cross as
//! borrowed `(ptr, len)` pairs, objects are borrowed as parameters (pinned
//! for the call through `_wv_pin`) and adopted as returns, and a callback
//! interface crosses as a handle-table key plus the interface's static
//! vtable.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use crate::utils::{local_type_name, wrapper_name};
use heck::{ToShoutySnakeCase, ToSnakeCase};
use weaveffi_model::model::{AsyncBinding, CallShape, FnBinding, IteratorBinding, ModuleBinding};
use weaveffi_model::model::{ParamBinding, Ty};
use weaveffi_model::plan::{self, ArgPass, ErrorStrategy, RetPass};

use crate::targets::ruby::callbacks::rb_vtable_const;
use crate::targets::ruby::codec::render_wv_read;
use crate::targets::ruby::codec::render_wv_write;
use crate::targets::ruby::docs::emit_param_docs;
use crate::targets::ruby::entities::{rb_checker_name, rb_error_factory_name};
use crate::targets::ruby::types::{
    rb_abi_types, rb_direct_type, rb_ffi_type, rb_param_name, rb_str_literal,
};

/// How a rendered Ruby callable is scoped and spelled in the generated
/// module: at module scope as a singleton method, or inside an interface
/// class as a constructor, instance method, or class method.
pub(crate) enum RbScope<'a> {
    /// A module-level free function (`def self.name` on the top-level module).
    Free {
        /// The owning module's underscore-joined path.
        module_path: &'a str,
        /// Whether the emitted name drops the module-path prefix.
        strip_module_prefix: bool,
    },
    /// An instance method on an interface class: `def name`, borrowing the
    /// wrapper's own pointer as the leading C argument.
    Method {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// A static member of an interface class (`def self.name`).
    Static {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// A non-`new` constructor: a class method adopting the returned
    /// reference through `_from_ptr` (never re-running `initialize`).
    Factory {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// The canonical `new` constructor, emitted as `initialize`.
    Init {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
}

impl RbScope<'_> {
    /// The receiver prefix for module singleton calls (attached C symbols,
    /// error checkers, runtime helpers): `"{ModuleName}."` inside a class
    /// body, empty at module scope, where `self` already is the module.
    fn qualifier(&self) -> String {
        match self {
            RbScope::Free { .. } => String::new(),
            RbScope::Method { module_name }
            | RbScope::Static { module_name }
            | RbScope::Factory { module_name }
            | RbScope::Init { module_name } => format!("{module_name}."),
        }
    }

    /// Two-space indent depth of the `def` line (1 at module scope, 2 inside
    /// an interface class).
    fn depth(&self) -> usize {
        if matches!(self, RbScope::Free { .. }) {
            1
        } else {
            2
        }
    }

    /// The `def` opener for `f` with the given formal parameters.
    fn def_open(&self, f: &FnBinding, params: &[String]) -> String {
        let args = if params.is_empty() {
            String::new()
        } else {
            format!("({})", params.join(", "))
        };
        match self {
            RbScope::Free {
                module_path,
                strip_module_prefix,
            } => format!(
                "def self.{}{args}",
                wrapper_name(module_path, &f.name, *strip_module_prefix).to_snake_case()
            ),
            RbScope::Method { .. } => format!("def {}{args}", f.name.to_snake_case()),
            RbScope::Static { .. } | RbScope::Factory { .. } => {
                format!("def self.{}{args}", f.name.to_snake_case())
            }
            RbScope::Init { .. } => format!("def initialize{args}"),
        }
    }
}

/// Render one callable: a free function or an interface member. `module`
/// supplies the error domain for throwing callables; `scope` picks the def
/// spelling, receiver, indent, and result handling.
pub(crate) fn render_callable(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    scope: &RbScope,
) {
    let mut w = CodeWriter::two_space().with_depth(scope.depth());
    w.blank();
    w.doc(&f.doc, DocCommentStyle::Hash);
    emit_param_docs(&mut w, &f.params);
    if let Some(msg) = &f.deprecated {
        w.line(format!("# @deprecated {msg}"));
    }
    match &f.shape {
        CallShape::Sync(abi) => render_sync(&mut w, module, f, &abi.symbol, scope),
        CallShape::Async(a) => render_async(&mut w, f, a, scope),
        CallShape::Iterator(it) => render_iterator(&mut w, module, f, it, scope),
    }
    out.push_str(&w.finish());
}

/// The trailing `, blocking: true` of an attachment that releases the GVL.
fn blocking_opt(blocking: bool) -> &'static str {
    if blocking {
        ", blocking: true"
    } else {
        ""
    }
}

/// Attach the C symbols for one callable: the plain symbol for a sync shape,
/// the launcher plus its completion trampoline for an async shape, and the
/// launch/next/destroy triple for an iterator.
///
/// `blocking` marks calls that may call back into Ruby (every call in an API
/// with callback interfaces); they release the GVL so a producer thread can
/// run a callback while the call is in flight. Async launches and iterator
/// steps always release it.
pub(crate) fn render_attach_function(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    blocking: bool,
) {
    let mut w = CodeWriter::two_space().with_depth(1);
    match &f.shape {
        CallShape::Sync(abi) => {
            w.line(format!(
                "attach_function :{}, [{}], {}{}",
                abi.symbol,
                rb_abi_types(&abi.params).join(", "),
                rb_ffi_type(&abi.ret),
                blocking_opt(blocking)
            ));
        }
        CallShape::Async(a) => {
            w.line(format!(
                "attach_function :{}, [{}], :void, blocking: true",
                a.launch.symbol,
                rb_abi_types(&a.launch.params).join(", ")
            ));
            render_async_trampoline(&mut w, module, f, a);
        }
        CallShape::Iterator(it) => {
            w.line(format!(
                "attach_function :{}, [{}], :pointer, blocking: true",
                it.launch.symbol,
                rb_abi_types(&it.launch.params).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [{}], :int32, blocking: true",
                it.next.symbol,
                rb_abi_types(&it.next.params).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [:pointer], :void{}",
                it.destroy_symbol,
                blocking_opt(blocking)
            ));
        }
    }
    out.push_str(&w.finish());
}

/// The constant pinning an async function's completion trampoline, named
/// after its C callback typedef: `kvstore_kv_Store_compact_callback` becomes
/// `KVSTORE_KV_STORE_COMPACT_CALLBACK`.
fn rb_async_const(a: &AsyncBinding) -> String {
    a.callback_type.to_shouty_snake_case()
}

/// The error a completion's `taken` triple becomes: the domain error for a
/// throwing function, the generic error otherwise.
fn rb_error_from_taken(module: &ModuleBinding, f: &FnBinding, q: &str) -> String {
    match (f.error_strategy(), module.error.as_ref()) {
        (ErrorStrategy::Throws, Some(eb)) => {
            format!("{q}{}(*taken)", rb_error_factory_name(eb))
        }
        _ => format!("{q}_wv_error(taken[0], taken[1])"),
    }
}

/// Render the module-level completion trampoline of one async function. It
/// resolves the call's queue from `context`, converts the error or result
/// (copying and releasing any owned buffer), and pushes it; nothing raised
/// while converting escapes into the C frame, so the waiting call always
/// wakes up.
fn render_async_trampoline(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    f: &FnBinding,
    a: &AsyncBinding,
) {
    let mut formals = vec!["ctx".to_string(), "err".to_string()];
    formals.extend(a.callback_params.iter().skip(2).map(|p| p.name.clone()));
    w.blank();
    w.line("# @api private");
    w.line(format!("# Completion trampoline for {}.", f.name));
    w.block(
        format!(
            "{} = FFI::Function.new(:void, [{}]) do |{}|",
            rb_async_const(a),
            rb_abi_types(&a.callback_params).join(", "),
            formals.join(", ")
        ),
        "end",
        |w| {
            w.line("queue = _wv_async_finish(ctx)");
            w.line("begin");
            w.scope(|w| {
                w.line("taken = _wv_take_boxed_error(err)");
                w.line("if taken");
                w.scope(|w| {
                    w.line(format!("queue << {}", rb_error_from_taken(module, f, "")));
                });
                w.line("else");
                w.scope(|w| {
                    let ptr = match plan::ret_pass(f.ret.as_ref(), "") {
                        RetPass::String | RetPass::Bytes | RetPass::Buffer => "result_ptr",
                        _ => "result",
                    };
                    receive_value(w, f.ret.as_ref(), ptr, "result_len", "", "value = ");
                    w.line("queue << value");
                });
                w.line("end");
            });
            w.line("rescue Exception => e # rubocop:disable Lint/RescueException");
            w.scope(|w| {
                w.line("queue << e");
            });
            w.line("end");
        },
    );
    w.blank();
}

/// Emit the statements that turn a received value into its Ruby value: a
/// sync return (`ptr` names the C result, `len` the length read from
/// `out_len`), an async result, or an iterator element. Owned strings, bytes,
/// and buffers are copied and released; objects are adopted. The final
/// expression is prefixed with `dest` (an assignment such as `"value = "`,
/// or empty to leave it as the block's value).
fn receive_value(w: &mut CodeWriter, ty: Option<&Ty>, ptr: &str, len: &str, q: &str, dest: &str) {
    match plan::ret_pass(ty, "") {
        RetPass::Void => {
            w.line(format!("{dest}nil"));
        }
        RetPass::Direct => {
            w.line(format!("{dest}{ptr}"));
        }
        RetPass::String => {
            w.line(format!("{dest}{q}_wv_take_string({ptr}, {len})"));
        }
        RetPass::Bytes => {
            w.line(format!("{dest}{q}_wv_take_bytes({ptr}, {len})"));
        }
        RetPass::Buffer => {
            let ty = ty.expect("buffered value has a type");
            w.block(
                format!("{dest}{q}_wv_decode({ptr}, {len}) do |_wv_r|"),
                "end",
                |w| {
                    render_wv_read(w, "_wv_r", "_wv_value", ty, 0, q);
                    w.line("_wv_value");
                },
            );
        }
        RetPass::Object { nullable, .. } => {
            let class = local_type_name(
                ty.and_then(Ty::interface_name)
                    .expect("object value names an interface"),
            );
            if nullable {
                w.line(format!(
                    "{dest}{ptr}.null? ? nil : {class}._from_ptr({ptr})"
                ));
            } else {
                w.line(format!(
                    "raise Error.new(GENERIC_ERROR_CODE, 'null object pointer') if {ptr}.null?"
                ));
                w.line(format!("{dest}{class}._from_ptr({ptr})"));
            }
        }
    }
}

/// The objects a call borrows, as `(Ruby expression, pinned pointer local)`
/// pairs: the receiver for an instance method, then each object parameter.
fn pinned_objects(f: &FnBinding, scope: &RbScope) -> Vec<(String, String)> {
    let mut pins = Vec::new();
    if matches!(scope, RbScope::Method { .. }) {
        pins.push(("self".to_string(), "_wv_self".to_string()));
    }
    for p in &f.params {
        if matches!(p.arg_pass(), ArgPass::Object { .. }) {
            let name = rb_param_name(&p.name);
            pins.push((name.clone(), format!("_wv_{name}")));
        }
    }
    pins
}

/// Run `body` inside a `_wv_pin` block over `pins` (each object stays alive,
/// and its wrapper can't release its reference, until the block returns), or
/// directly when nothing is borrowed.
fn with_pins(
    w: &mut CodeWriter,
    q: &str,
    pins: &[(String, String)],
    body: impl FnOnce(&mut CodeWriter),
) {
    if pins.is_empty() {
        body(w);
        return;
    }
    let exprs: Vec<&str> = pins.iter().map(|(e, _)| e.as_str()).collect();
    let vars: Vec<&str> = pins.iter().map(|(_, v)| v.as_str()).collect();
    w.block(
        format!("{q}_wv_pin({}) do |{}|", exprs.join(", "), vars.join(", ")),
        "end",
        body,
    );
}

/// Emit the statements converting the parameters into the locals the C call
/// borrows. Strings and bytes become private binary copies, buffered values
/// are encoded, and callback implementations are registered last, so nothing
/// that can raise runs between a registration and the call that hands it to
/// the producer.
fn render_param_prep(w: &mut CodeWriter, f: &FnBinding, q: &str) {
    for p in &f.params {
        let name = rb_param_name(&p.name);
        match p.arg_pass() {
            ArgPass::String { .. } => {
                w.line(format!("{name}_s = {q}_wv_str({name})"));
            }
            ArgPass::Bytes { .. } => {
                w.line(format!("{name}_s = {q}_wv_bytes({name})"));
            }
            ArgPass::Buffer { .. } => {
                w.line(format!("{name}_w = WvBufferWriter.new"));
                render_wv_write(w, &format!("{name}_w"), &name, &p.ty, 0, q);
            }
            _ => {}
        }
    }
}

/// Emit the statements that hand the producer what it adopts, immediately
/// before the call: seal the buffers (minting the references their object
/// tokens carry; a buffer without objects seals to its bytes as is) and then
/// register every callback-interface parameter. Registration can't raise,
/// so nothing between here and the call can strand a minted reference or a
/// registration.
fn render_handoff(w: &mut CodeWriter, f: &FnBinding, q: &str) {
    let sealed: Vec<String> = f
        .params
        .iter()
        .filter(|p| matches!(p.arg_pass(), ArgPass::Buffer { .. }))
        .map(|p| rb_param_name(&p.name))
        .collect();
    match sealed.as_slice() {
        [] => {}
        [one] => {
            w.line(format!("{one}_s = {q}_wv_seal({one}_w).first"));
        }
        many => {
            w.line(format!(
                "{} = {q}_wv_seal({})",
                many.iter()
                    .map(|n| format!("{n}_s"))
                    .collect::<Vec<_>>()
                    .join(", "),
                many.iter()
                    .map(|n| format!("{n}_w"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    render_callback_registrations(w, f, q);
}

/// Register every callback-interface parameter (see [`render_handoff`]).
fn render_callback_registrations(w: &mut CodeWriter, f: &FnBinding, q: &str) {
    for p in &f.params {
        if matches!(p.arg_pass(), ArgPass::Callback { .. }) {
            let name = rb_param_name(&p.name);
            w.line(format!("{name}_ctx = {q}_wv_cb_register({name})"));
        }
    }
}

/// The Ruby argument expressions one parameter contributes to the C call,
/// per its [`ArgPass`] contract.
fn rb_call_args(p: &ParamBinding) -> Vec<String> {
    let name = rb_param_name(&p.name);
    match p.arg_pass() {
        ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
            vec![format!("{name}_s"), format!("{name}_s.bytesize")]
        }
        ArgPass::Object { .. } => vec![format!("_wv_{name}")],
        ArgPass::Callback { .. } => {
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback plan names a callback interface");
            vec![
                format!("{name}_ctx"),
                format!("{}.to_ptr", rb_vtable_const(cb)),
            ]
        }
        ArgPass::Direct { .. } if matches!(p.ty, Ty::Bool) => {
            vec![format!("({name} ? true : false)")]
        }
        ArgPass::Direct { .. } => vec![name],
    }
}

/// The leading C arguments of a call: the pinned receiver, then every
/// parameter's slots.
fn call_args(f: &FnBinding, scope: &RbScope) -> Vec<String> {
    let mut args = Vec::new();
    if matches!(scope, RbScope::Method { .. }) {
        args.push("_wv_self".to_string());
    }
    for p in &f.params {
        args.extend(rb_call_args(p));
    }
    args
}

fn formals(f: &FnBinding) -> Vec<String> {
    f.params.iter().map(|p| rb_param_name(&p.name)).collect()
}

fn render_deprecation(w: &mut CodeWriter, f: &FnBinding) {
    if let Some(msg) = &f.deprecated {
        w.line(format!("warn '[DEPRECATED] {}'", rb_str_literal(msg)));
    }
}

/// Render the sync wrapper: convert the parameters, pin the borrowed
/// objects, make the C call with a fresh `ErrorStruct`, route the out-err
/// slot through the function's checker, then receive the result per the
/// scope.
fn render_sync(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    f: &FnBinding,
    symbol: &str,
    scope: &RbScope,
) {
    let q = scope.qualifier();
    let checker = rb_checker_name(f, module.error.as_ref());
    let has_out_len = matches!(
        plan::ret_pass(f.ret.as_ref(), ""),
        RetPass::String | RetPass::Bytes | RetPass::Buffer
    );
    w.block(scope.def_open(f, &formals(f)), "end", |w| {
        render_deprecation(w, f);
        render_param_prep(w, f, &q);
        with_pins(w, &q, &pinned_objects(f, scope), |w| {
            w.line("err = ErrorStruct.new");
            let mut args = call_args(f, scope);
            if has_out_len {
                w.line("out_len = FFI::MemoryPointer.new(:size_t)");
                args.push("out_len".into());
            }
            args.push("err".into());
            render_handoff(w, f, &q);
            let call = format!("{q}{symbol}({})", args.join(", "));
            if f.ret.is_some() {
                w.line(format!("result = {call}"));
            } else {
                w.line(call);
            }
            w.line(format!("{q}{checker}(err)"));
            match scope {
                RbScope::Init { .. } => {
                    w.line(
                        "raise Error.new(GENERIC_ERROR_CODE, 'null object pointer') if result.null?",
                    );
                    w.line("_wv_init(result)");
                }
                RbScope::Factory { .. } => {
                    w.line(
                        "raise Error.new(GENERIC_ERROR_CODE, 'null object pointer') if result.null?",
                    );
                    w.line("_from_ptr(result)");
                }
                _ if f.ret.is_some() => {
                    receive_value(w, f.ret.as_ref(), "result", "out_len.read(:size_t)", &q, "");
                }
                _ => {}
            }
        });
    });
}

/// Render the async wrapper: launch the call with the function's completion
/// trampoline and block on a `Queue` until it fires (`Queue#pop` releases
/// the GVL; the ffi gem runs the trampoline on a Ruby thread). Blocking is
/// the idiomatic Ruby surface; callers wanting concurrency run the call in a
/// Thread.
///
/// A cancellable function takes a `cancel:` keyword. Without one, the
/// wrapper still passes a private token, so interrupting the wait
/// (`Thread#raise`, `Timeout`) cancels the producer's work too.
fn render_async(w: &mut CodeWriter, f: &FnBinding, a: &AsyncBinding, scope: &RbScope) {
    let q = scope.qualifier();
    let mut params = formals(f);
    if f.cancellable {
        params.push("cancel: nil".into());
    }
    w.line("# Blocks until the call completes on a producer thread.");
    if f.cancellable {
        w.line("# @param cancel [CancelToken, nil] cancels the call; it then raises Cancelled");
    }
    w.block(scope.def_open(f, &params), "end", |w| {
        render_deprecation(w, f);
        render_param_prep(w, f, &q);
        if f.cancellable {
            w.line("own_token = CancelToken.new if cancel.nil?");
            w.line("token = cancel || own_token");
        }
        let pins = pinned_objects(f, scope);
        if !pins.is_empty() {
            // Assigned inside the pin block, read after it.
            w.line("queue = nil");
        }
        with_pins(w, &q, &pins, |w| {
            render_handoff(w, f, &q);
            w.line(format!("ctx, queue = {q}_wv_async_begin"));
            let mut args = call_args(f, scope);
            if f.cancellable {
                args.push("token._wv_ptr".into());
            }
            args.push(rb_async_const(a));
            args.push("ctx".into());
            w.line(format!("{q}{}({})", a.launch.symbol, args.join(", ")));
        });
        if f.cancellable {
            w.line(format!("{q}_wv_async_wait(queue, token)"));
        } else {
            w.line(format!("{q}_wv_async_wait(queue)"));
        }
        if f.cancellable {
            w.dedent();
            w.line("ensure");
            w.indent();
            w.line("own_token&.close");
        }
    });
}

/// Render the iterator wrapper: a lazy `Enumerator` per the pull contract of
/// [`weaveffi_model::plan::IteratorProtocol`].
///
/// The producer iterator launches inside the enumerator block, on the first
/// pull, so nothing leaks when the enumerator is never started (launch
/// errors therefore raise on the first pull). Each step issues exactly one
/// `next` call, each element is received like a return of its type, and
/// `destroy` runs exactly once from an `ensure`, so an early `break` or an
/// error mid-iteration still releases the handle.
fn render_iterator(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    f: &FnBinding,
    it: &IteratorBinding,
    scope: &RbScope,
) {
    let q = scope.qualifier();
    let checker = rb_checker_name(f, module.error.as_ref());
    let elem_pass = plan::ret_pass(Some(&it.elem), "");
    let needs_len = matches!(
        elem_pass,
        RetPass::String | RetPass::Bytes | RetPass::Buffer
    );
    w.line("# Returns a lazy Enumerator that pulls one element per step. The");
    w.line("# producer iterator starts on the first pull and is released when");
    w.line("# iteration finishes or is abandoned early.");
    w.block(scope.def_open(f, &formals(f)), "end", |w| {
        render_deprecation(w, f);
        render_param_prep(w, f, &q);
        // The block closes over the converted arguments, so they stay
        // referenced until the launch call runs.
        w.block("Enumerator.new do |y|", "end", |w| {
            w.line("err = ErrorStruct.new");
            let mut args = call_args(f, scope);
            args.push("err".into());
            let launch = format!("{q}{}({})", it.launch.symbol, args.join(", "));
            let pins = pinned_objects(f, scope);
            let mut handoff = CodeWriter::two_space();
            render_handoff(&mut handoff, f, &q);
            let handoff = handoff.finish();
            if pins.is_empty() {
                w.block_raw(&handoff);
                w.line(format!("iter = {launch}"));
            } else if handoff.is_empty() {
                w.line(format!(
                    "iter = {q}_wv_pin({}) {{ |{}| {launch} }}",
                    pins.iter()
                        .map(|(e, _)| e.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    pins.iter()
                        .map(|(_, v)| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            } else {
                w.line("iter = nil");
                with_pins(w, &q, &pins, |w| {
                    w.block_raw(&handoff);
                    w.line(format!("iter = {launch}"));
                });
            }
            w.line("begin");
            w.scope(|w| {
                w.line(format!("{q}{checker}(err)"));
                let item_type = match elem_pass {
                    RetPass::Direct => rb_direct_type(&it.elem),
                    _ => ":pointer",
                };
                w.line(format!("out_item = FFI::MemoryPointer.new({item_type})"));
                let mut next_args = vec!["iter", "out_item"];
                if needs_len {
                    w.line("out_len = FFI::MemoryPointer.new(:size_t)");
                    next_args.push("out_len");
                }
                next_args.push("err");
                w.block("loop do", "end", |w| {
                    w.line(format!(
                        "has_item = {q}{}({})",
                        it.next.symbol,
                        next_args.join(", ")
                    ));
                    w.line(format!("{q}{checker}(err)"));
                    w.line("break if has_item.zero?");
                    let item = match elem_pass {
                        RetPass::Direct => format!("out_item.read({item_type})"),
                        _ => "out_item.read_pointer".to_string(),
                    };
                    w.line(format!("item = {item}"));
                    receive_value(
                        w,
                        Some(&it.elem),
                        "item",
                        "out_len.read(:size_t)",
                        &q,
                        "value = ",
                    );
                    w.line("y << value");
                });
            });
            w.line("ensure");
            w.scope(|w| {
                w.line(format!("{q}{}(iter) unless iter.null?", it.destroy_symbol));
            });
            w.line("end");
        });
    });
}
