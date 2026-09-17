//! Callable rendering: FFI attachments, the sync/async/iterator wrapper
//! bodies, and the parameter and return marshalling they share.
//!
//! Marshalling dispatch goes through the shared plan layer ([`ArgPass`],
//! [`RetPass`], [`Free`]) rather than crate-local `Ty` folds, so this
//! backend cannot drift from the others on call-boundary semantics: objects
//! are borrowed as parameters and adopted as returns, and a callback
//! interface crosses as a registry key plus the interface's static vtable.

use heck::ToSnakeCase;
use weaveffi_core::abi::CType;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    AsyncBinding, CallShape, FnBinding, IteratorBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{self, ArgPass, ErrorStrategy, Free, RetPass};
use weaveffi_core::utils::{local_type_name, wrapper_name};

use crate::callbacks::rb_vtable_const;
use crate::docs::{emit_doc, emit_param_docs};
use crate::entities::{rb_checker_name, rb_error_factory_name};
use crate::types::{
    rb_abi_types, rb_direct_from_c, rb_ffi_type, rb_mem_type, rb_param_name, rb_read_method,
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
    /// An instance method on an interface class: `def name`, passing the
    /// wrapper's borrowed `handle` as the leading C argument.
    Method {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// A static member of an interface class (`def self.name`).
    Static {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// A non-`new` constructor: a class method wrapping the returned owned
    /// pointer via `_from_ptr` (never re-running `initialize`).
    Factory {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
    },
    /// The canonical `new` constructor, emitted as `initialize`.
    Init {
        /// The top-level Ruby module name qualifying module singleton calls.
        module_name: &'a str,
        /// The interface's `FFI::AutoPointer` subclass wrapping the handle.
        ptr_class: &'a str,
    },
}

impl<'a> RbScope<'a> {
    /// The top-level Ruby module name when calls must be explicitly
    /// qualified (inside a class body); `None` at module scope, where the
    /// implicit `self` already is the module.
    fn module_name(&self) -> Option<&'a str> {
        match self {
            RbScope::Free { .. } => None,
            RbScope::Method { module_name }
            | RbScope::Static { module_name }
            | RbScope::Factory { module_name }
            | RbScope::Init { module_name, .. } => Some(module_name),
        }
    }

    /// The receiver prefix for module singleton calls (attached C symbols,
    /// error checkers, `weaveffi_free_*`): `"{ModuleName}."` inside a class
    /// body, empty at module scope.
    fn qualifier(&self) -> String {
        self.module_name()
            .map(|m| format!("{m}."))
            .unwrap_or_default()
    }

    /// Two-space indent depth of the `def` line (1 at module scope, 2 inside
    /// an interface class).
    fn depth(&self) -> usize {
        if self.module_name().is_none() {
            1
        } else {
            2
        }
    }

    /// The borrowed object pointer instance methods pass as the leading C
    /// slot, through the `handle` accessor so a closed wrapper raises instead
    /// of passing NULL.
    fn self_arg(&self) -> Option<&'static str> {
        matches!(self, RbScope::Method { .. }).then_some("handle")
    }

    /// The `def` opener for `f` with the given formal parameter names.
    fn def_open(&self, f: &FnBinding, params: &[String]) -> String {
        let args = params.join(", ");
        match self {
            RbScope::Free {
                module_path,
                strip_module_prefix,
            } => format!(
                "def self.{}({args})",
                wrapper_name(module_path, &f.name, *strip_module_prefix).to_snake_case()
            ),
            RbScope::Method { .. } => format!("def {}({args})", f.name.to_snake_case()),
            RbScope::Static { .. } | RbScope::Factory { .. } => {
                format!("def self.{}({args})", f.name.to_snake_case())
            }
            RbScope::Init { .. } => format!("def initialize({args})"),
        }
    }
}

/// Render one callable: a free function or an interface member. `module`
/// supplies the error domain for throwing callables; `scope` picks the def
/// spelling, receiver, indent, and result handling. Sync, async, and
/// iterator shapes all route through here so members reuse the free-function
/// marshalling paths.
pub(crate) fn render_callable(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    scope: &RbScope,
) {
    match &f.shape {
        CallShape::Sync(_) => render_sync_function_wrapper(out, module, f, scope),
        CallShape::Async(a) => render_async_function_wrapper(out, module, f, a, scope),
        CallShape::Iterator(it) => render_iterator_function_wrapper(out, module, f, it, scope),
    }
}

/// Attach the C symbols for one callable: the plain symbol for a sync shape,
/// the completion callback type plus launcher for an async shape, and the
/// launch/next/destroy triple for an iterator.
pub(crate) fn render_attach_function(out: &mut String, f: &FnBinding) {
    let mut w = CodeWriter::two_space().with_depth(1);
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, "  ");
    w.raw(d);
    match &f.shape {
        CallShape::Sync(abi) => {
            w.line(format!(
                "attach_function :{}, [{}], {}",
                abi.symbol,
                rb_abi_types(&abi.params, false).join(", "),
                rb_ffi_type(&abi.ret, true)
            ));
        }
        CallShape::Async(a) => {
            // Completion callback: result strings/bytes stay `:pointer`
            // (the wrapper owns and frees them); the launcher takes the
            // declared callback type plus the opaque context.
            w.line(format!(
                "callback :{}, [{}], :void",
                a.callback_type,
                rb_abi_types(&a.callback_params, true).join(", ")
            ));
            let argtypes: Vec<String> = a
                .launch
                .params
                .iter()
                .map(|p| match &p.ty {
                    // The `callback` slot is lowered as a Named C type; bind
                    // it to the callback symbol declared above.
                    CType::Named(_) => format!(":{}", a.callback_type),
                    ty => rb_ffi_type(ty, false).to_string(),
                })
                .collect();
            w.line(format!(
                "attach_function :{}, [{}], :void",
                a.launch.symbol,
                argtypes.join(", ")
            ));
        }
        CallShape::Iterator(it) => {
            w.line(format!(
                "attach_function :{}, [{}], :pointer",
                it.launch.symbol,
                rb_abi_types(&it.launch.params, false).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [{}], :int32",
                it.next.symbol,
                // Every `next` slot is a pointer (iter, out_item, out lens, err).
                rb_abi_types(&it.next.params, true).join(", ")
            ));
            w.line(format!(
                "attach_function :{}, [:pointer], :void",
                it.destroy_symbol
            ));
        }
    }
    out.push_str(&w.finish());
}

/// Render the sync wrapper for one callable: convert the parameters, make
/// the C call with a stack `ErrorStruct`, route the out-err slot through the
/// function's checker, then convert the result per the scope.
fn render_sync_function_wrapper(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    scope: &RbScope,
) {
    let c_sym = &f.c_base;
    let depth = scope.depth();
    let ind = "  ".repeat(depth + 1);
    let doc_ind = "  ".repeat(depth);
    let q = scope.qualifier();
    let checker = rb_checker_name(f, module.error.as_ref());

    let params: Vec<String> = f.params.iter().map(|p| rb_param_name(&p.name)).collect();
    let mut w = CodeWriter::two_space().with_depth(depth);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, &doc_ind);
    w.raw(d);
    emit_param_docs(&mut w, &f.params);
    w.block(scope.def_open(f, &params), "end", |w| {
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('"', "\\\"");
            w.line(format!("warn \"[DEPRECATED] {escaped}\""));
        }

        w.line("err = ErrorStruct.new");

        for p in &f.params {
            let mut pc = String::new();
            render_param_conversion(&mut pc, p, &ind, scope.module_name());
            w.raw(pc);
        }

        // Bytes and buffered returns carry a trailing `out_len` slot.
        let has_out_len = matches!(
            plan::ret_pass(f.ret.as_ref(), "", ""),
            RetPass::Bytes | RetPass::Buffer
        );

        if has_out_len {
            w.line("out_len = FFI::MemoryPointer.new(:size_t)");
        }

        let mut call_args: Vec<String> = Vec::new();
        if let Some(recv) = scope.self_arg() {
            call_args.push(recv.into());
        }
        for p in &f.params {
            call_args.extend(rb_call_args(p));
        }
        if has_out_len {
            call_args.push("out_len".into());
        }
        call_args.push("err".into());

        let call = format!("{q}{c_sym}({})", call_args.join(", "));
        if f.ret.is_some() {
            w.line(format!("result = {call}"));
        } else {
            w.line(call);
        }

        w.line(format!("{q}{checker}(err)"));

        match scope {
            // Constructors receive the owned pointer directly rather than
            // routing through the generic return path.
            RbScope::Init { ptr_class, .. } => {
                w.line("raise Error.new(-1, 'null pointer') if result.null?");
                w.line(format!("@handle = {ptr_class}.new(result)"));
            }
            RbScope::Factory { .. } => {
                w.line("raise Error.new(-1, 'null pointer') if result.null?");
                w.line("_from_ptr(result)");
            }
            _ => {
                if let Some(ret_ty) = &f.ret {
                    let mut tmp = String::new();
                    render_return_code(&mut tmp, ret_ty, &ind, scope.module_name());
                    w.raw(tmp);
                }
            }
        }
    });
    out.push_str(&w.finish());
}

/// Async wrapper: launches the `_async` C symbol with an `FFI::Function`
/// completion trampoline and blocks on a `Queue` until it fires (`Queue#pop`
/// releases the GVL, and the ffi gem delivers cross-thread callbacks safely).
/// Blocking is the idiomatic Ruby surface; callers needing concurrency wrap
/// the call in their own Thread or Fiber scheduler.
///
/// The trampoline (`callback` local) stays referenced by the wrapper's stack
/// frame until `queue.pop` returns, which happens only after the producer has
/// invoked it, so the GC cannot collect it mid-flight. Per
/// [`weaveffi_core::plan::AsyncProtocol`], everything the trampoline receives
/// is owned by the consumer: it copies result buffers and then releases them
/// through the runtime free symbols, and it releases a reported error with
/// `weaveffi_error_free`; the error slot follows the function's
/// [`ErrorStrategy`].
fn render_async_function_wrapper(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    a: &AsyncBinding,
    scope: &RbScope,
) {
    let depth = scope.depth();
    let ind = "  ".repeat(depth + 1);
    let doc_ind = "  ".repeat(depth);
    let q = scope.qualifier();
    // A completion error raises the typed domain error for throwing
    // callables; the generic Error otherwise (panics, marshalling). Typed
    // errors also copy the borrowed payload buffer so declared fields decode.
    let typed_error = matches!(
        (f.error_strategy(), module.error.as_ref()),
        (ErrorStrategy::Throws, Some(_))
    );
    let error_expr = match (f.error_strategy(), module.error.as_ref()) {
        (ErrorStrategy::Throws, Some(eb)) => {
            format!("{q}{}(code, msg, payload)", rb_error_factory_name(eb))
        }
        _ => "Error.new(code, msg)".to_string(),
    };
    let params: Vec<String> = f.params.iter().map(|p| rb_param_name(&p.name)).collect();

    let mut w = CodeWriter::two_space().with_depth(depth);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, &doc_ind);
    w.raw(d);
    w.line("# Blocks the current thread until the async producer completes; the");
    w.line(format!(
        "# result (or error) is delivered through the completion callback{}.",
        if f.cancellable {
            " (cancellation token not exposed; pass-through is NULL)"
        } else {
            ""
        }
    ));
    w.block(scope.def_open(f, &params), "end", |w| {
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('"', "\\\"");
            w.line(format!("warn \"[DEPRECATED] {escaped}\""));
        }

        w.line("queue = Queue.new");

        // Completion trampoline: (context, err, <result slots>).
        let cb_types = rb_abi_types(&a.callback_params, true);
        let mut cb_formals: Vec<String> = vec!["_context".into(), "err_ptr".into()];
        cb_formals.extend(a.callback_params.iter().skip(2).map(|p| p.name.clone()));
        w.block(
            format!(
                "callback = FFI::Function.new(:void, [{}]) do |{}|",
                cb_types.join(", "),
                cb_formals.join(", ")
            ),
            "end",
            |w| {
                // Producers pass err = NULL on success, so guard before dereferencing.
                w.line("err = err_ptr.null? ? nil : ErrorStruct.new(err_ptr)");
                w.line("if err && err[:code] != 0");
                w.scope(|w| {
                    w.line("code = err[:code]");
                    w.line(
                        "msg = err[:message].null? ? '' : \
                         err[:message].read_string.force_encoding(Encoding::UTF_8)",
                    );
                    if typed_error {
                        // Copy the payload before releasing the boxed error.
                        w.line(
                            "payload = err[:payload_ptr].null? ? nil : \
                             err[:payload_ptr].read_string(err[:payload_len])",
                        );
                    }
                    w.line(format!("{q}weaveffi_error_free(err_ptr)"));
                    w.line(format!("queue << {error_expr}"));
                });
                w.line("else");
                w.scope(|w| {
                    let mut tmp = String::new();
                    render_async_result_push(
                        &mut tmp,
                        &f.ret,
                        &format!("{ind}    "),
                        scope.module_name(),
                    );
                    w.raw(tmp);
                });
                w.line("end");
            },
        );

        for p in &f.params {
            let mut pc = String::new();
            render_param_conversion(&mut pc, p, &ind, scope.module_name());
            w.raw(pc);
        }
        let mut call_args: Vec<String> = Vec::new();
        if let Some(recv) = scope.self_arg() {
            call_args.push(recv.into());
        }
        for p in &f.params {
            call_args.extend(rb_call_args(p));
        }
        if f.cancellable {
            call_args.push("FFI::Pointer::NULL".into());
        }
        call_args.push("callback".into());
        call_args.push("FFI::Pointer::NULL".into());
        w.line(format!("{q}{}({})", a.launch.symbol, call_args.join(", ")));
        w.line("value = queue.pop");
        w.line("raise value if value.is_a?(Error)");
        w.line("value");
    });
    out.push_str(&w.finish());
}

/// Push the converted async result onto the queue. Result slots are named by
/// the shared ABI lowering: `result` (plus `result_len` for bytes), or
/// `result_ptr`/`result_len` for a buffered value.
///
/// Per the async completion contract ([`weaveffi_core::plan::AsyncProtocol`]),
/// string, bytes, and buffered result buffers are owned by the consumer: the
/// callback deep-copies or decodes them, then releases them through the
/// runtime free symbols. Owned interface results are adopted by a
/// finalizer-bearing wrapper instead.
fn render_async_result_push(
    out: &mut String,
    ret: &Option<Ty>,
    ind: &str,
    qualifier: Option<&str>,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match plan::ret_pass(ret.as_ref(), "", "") {
        RetPass::Void => {
            w.line("queue << nil");
        }
        RetPass::Buffer => {
            // Owned buffer: copy, free, then decode from the copy. A decode
            // failure surfaces through the queue so the caller thread raises
            // it.
            let ty = ret.as_ref().expect("buffered return has a type");
            w.line("begin");
            w.scope(|w| {
                w.line(
                    "_wv_r = WvBufferReader.new(result_ptr.null? ? ''.b : \
                     result_ptr.read_string(result_len))",
                );
                w.line(format!(
                    "{m}weaveffi_free_bytes(result_ptr, result_len) unless result_ptr.null?"
                ));
                crate::codec::render_wv_read(w, "_wv_r", "_wv_v", ty, 0, &m);
                w.line("_wv_r.expect_end!");
                w.line("queue << _wv_v");
            });
            w.line("rescue Error => e");
            w.scope(|w| {
                w.line("queue << e");
            });
            w.line("end");
        }
        RetPass::String => {
            // Owned by the consumer: copy (tagged UTF-8, since ffi's
            // read_string yields BINARY), then free.
            w.line(
                "_wv_s = result.null? ? '' : result.read_string.force_encoding(Encoding::UTF_8)",
            );
            w.line(format!(
                "{m}weaveffi_free_string(result) unless result.null?"
            ));
            w.line("queue << _wv_s");
        }
        RetPass::Bytes => {
            // Owned by the consumer: copy, then free.
            w.line("_wv_b = result.null? ? ''.b : result.read_string(result_len)");
            w.line(format!(
                "{m}weaveffi_free_bytes(result, result_len) unless result.null?"
            ));
            w.line("queue << _wv_b");
        }
        // A returned interface transfers ownership of a new object
        // reference; wrap it without re-running initialize. A nullable
        // return surfaces nil for null instead of trapping.
        RetPass::Object { nullable, .. } => {
            let name = match ret.as_ref() {
                Some(Ty::Interface(name)) => name,
                Some(Ty::Optional(inner)) => match inner.as_ref() {
                    Ty::Interface(name) => name,
                    _ => unreachable!("only optional interfaces adopt object returns"),
                },
                _ => unreachable!("object return must be an interface type"),
            };
            let local = local_type_name(name);
            if nullable {
                w.line(format!(
                    "queue << (result.null? ? nil : {local}._from_ptr(result))"
                ));
            } else {
                w.line("if result.null?");
                w.scope(|w| {
                    w.line("queue << Error.new(-1, 'null pointer')");
                });
                w.line("else");
                w.scope(|w| {
                    w.line(format!("queue << {local}._from_ptr(result)"));
                });
                w.line("end");
            }
        }
        RetPass::Direct => {
            let ty = ret.as_ref().expect("direct return has a type");
            w.line(format!("queue << {}", rb_direct_from_c(ty, "result")));
        }
    }
    out.push_str(&w.finish());
}

/// Iterator wrapper: returns a lazy `Enumerator` per the pull contract stated
/// by [`weaveffi_core::plan::IteratorProtocol`].
///
/// The producer iterator launches *inside* the enumerator block, on the first
/// pull, so a handle cannot leak when the returned enumerator is never
/// started (launch errors therefore raise on the first pull rather than at
/// call time). Each consumer step issues exactly one C `next` call, each
/// yielded element is received per its element plan (strings and bytes
/// copied then freed, value buffers decoded then freed, object references
/// adopted into finalizer-bearing wrappers), and `destroy` runs exactly once
/// from an `ensure` block, so an early `break` or an error raised
/// mid-iteration still releases the handle. Launch and per-`next` errors
/// follow the function's [`ErrorStrategy`].
fn render_iterator_function_wrapper(
    out: &mut String,
    module: &ModuleBinding,
    f: &FnBinding,
    it: &IteratorBinding,
    scope: &RbScope,
) {
    let depth = scope.depth();
    let ind = "  ".repeat(depth + 1);
    let doc_ind = "  ".repeat(depth);
    let q = scope.qualifier();
    let checker = rb_checker_name(f, module.error.as_ref());
    let params: Vec<String> = f.params.iter().map(|p| rb_param_name(&p.name)).collect();
    let protocol = it.protocol(f, &module.path, "");

    let mut w = CodeWriter::two_space().with_depth(depth);
    w.blank();
    let mut d = String::new();
    emit_doc(&mut d, &f.doc, &doc_ind);
    w.raw(d);
    w.line("# Returns a lazy Enumerator that streams one element per pull; call");
    w.line("# `.to_a` to collect eagerly. The underlying producer iterator is");
    w.line("# launched on the first pull, so launch errors raise at that point");
    w.line("# rather than when this method returns. The iterator handle is");
    w.line("# released exactly once, when iteration finishes or is abandoned");
    w.line("# early (for example by `break`).");
    w.block(scope.def_open(f, &params), "end", |w| {
        if let Some(msg) = &f.deprecated {
            let escaped = msg.replace('"', "\\\"");
            w.line(format!("warn \"[DEPRECATED] {escaped}\""));
        }
        for p in &f.params {
            let mut pc = String::new();
            render_param_conversion(&mut pc, p, &ind, scope.module_name());
            w.raw(pc);
        }
        let mut call_args: Vec<String> = Vec::new();
        if let Some(recv) = scope.self_arg() {
            call_args.push(recv.into());
        }
        for p in &f.params {
            call_args.extend(rb_call_args(p));
        }
        call_args.push("err".into());
        // The block closes over the converted argument buffers above, so they
        // stay referenced (and un-collected) until the launch call runs.
        w.block("Enumerator.new do |y|", "end", |w| {
            w.line("err = ErrorStruct.new");
            w.line(format!(
                "iter = {q}{}({})",
                it.launch.symbol,
                call_args.join(", ")
            ));
            w.line("begin");
            w.scope(|w| {
                w.line(format!("{q}{checker}(err)"));
                w.line("unless iter.null?");
                w.scope(|w| {
                    w.block("loop do", "end", |w| {
                        // `next` params: (iter, out_item, <elem out slots>, out_err).
                        let elem = &it.elem;
                        // A pointer/length element pair (bytes, or any
                        // buffered value) carries an extra out-length slot.
                        let needs_len = matches!(protocol.elem_free, Free::Bytes);
                        let item_mem = rb_mem_type(elem);
                        w.line(format!("out_item = FFI::MemoryPointer.new({item_mem})"));
                        if needs_len {
                            w.line("out_item_len = FFI::MemoryPointer.new(:size_t)");
                        }
                        w.line("item_err = ErrorStruct.new");
                        let next_args = if needs_len {
                            "iter, out_item, out_item_len, item_err"
                        } else {
                            "iter, out_item, item_err"
                        };
                        w.line(format!("has_item = {q}{}({next_args})", it.next.symbol));
                        w.line(format!("{q}{checker}(item_err)"));
                        w.line("break if has_item.zero?");
                        let mut tmp = String::new();
                        render_iterator_item_yield(
                            &mut tmp,
                            elem,
                            &protocol.elem,
                            &"  ".repeat(depth + 5),
                            scope.module_name(),
                        );
                        w.raw(tmp);
                    });
                });
                w.line("end");
            });
            w.line("ensure");
            w.scope(|w| {
                // Exactly one destroy per launched handle: this ensure runs
                // once whether iteration exhausts, raises, or is abandoned by
                // an early break from the consumer.
                w.line(format!("{q}{}(iter) unless iter.null?", it.destroy_symbol));
            });
            w.line("end");
        });
    });
    out.push_str(&w.finish());
}

/// Convert the value written into `out_item` and yield it to the enumerator's
/// yielder `y`, receiving the element per its [`RetPass`] plan first (copy or
/// decode, free, then yield, so an early `break` during the yield cannot
/// leak the element; an object element is adopted). `qualifier` is the
/// top-level Ruby module name when rendering inside a class body, where
/// `weaveffi_free_*` calls need an explicit receiver.
fn render_iterator_item_yield(
    out: &mut String,
    elem: &Ty,
    pass: &RetPass,
    ind: &str,
    qualifier: Option<&str>,
) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match pass {
        RetPass::Void => unreachable!("iterator elements always have a type"),
        RetPass::Buffer => {
            // A buffered element is a producer-allocated value buffer: copy
            // the bytes, release them, then decode and yield the value.
            w.line("item_ptr = out_item.read_pointer");
            w.line("item_len = out_item_len.read(:size_t)");
            w.line("_wv_data = item_ptr.null? ? ''.b : item_ptr.read_string(item_len)");
            w.line(format!(
                "{m}weaveffi_free_bytes(item_ptr, item_len) unless item_ptr.null?"
            ));
            w.line("_wv_r = WvBufferReader.new(_wv_data)");
            crate::codec::render_wv_read(&mut w, "_wv_r", "_wv_item", elem, 0, &m);
            w.line("_wv_r.expect_end!");
            w.line("y << _wv_item");
        }
        RetPass::String => {
            w.line("item_ptr = out_item.read_pointer");
            w.line("if item_ptr.null?");
            w.scope(|w| {
                w.line("y << ''");
            });
            w.line("else");
            w.scope(|w| {
                w.line("item = item_ptr.read_string.force_encoding(Encoding::UTF_8)");
                w.line(format!("{m}weaveffi_free_string(item_ptr)"));
                w.line("y << item");
            });
            w.line("end");
        }
        RetPass::Bytes => {
            w.line("item_ptr = out_item.read_pointer");
            w.line("item_len = out_item_len.read(:size_t)");
            w.line("if item_ptr.null?");
            w.scope(|w| {
                w.line("y << ''.b");
            });
            w.line("else");
            w.scope(|w| {
                w.line("item = item_ptr.read_string(item_len)");
                w.line(format!("{m}weaveffi_free_bytes(item_ptr, item_len)"));
                w.line("y << item");
            });
            w.line("end");
        }
        // A yielded object is one strong reference the wrapper adopts,
        // without re-running initialize. A nullable element yields nil for a
        // null pointer.
        RetPass::Object { nullable, .. } => {
            let local = local_type_name(
                elem.interface_name()
                    .expect("object element names an interface"),
            );
            w.line("item_ptr = out_item.read_pointer");
            if *nullable {
                w.line(format!(
                    "y << (item_ptr.null? ? nil : {local}._from_ptr(item_ptr))"
                ));
            } else {
                w.line("raise Error.new(-1, 'null pointer') if item_ptr.null?");
                w.line(format!("y << {local}._from_ptr(item_ptr)"));
            }
        }
        RetPass::Direct => {
            let read = format!("out_item.{}", rb_read_method(elem));
            w.line(format!("y << {}", rb_direct_from_c(elem, &read)));
        }
    }
    out.push_str(&w.finish());
}

/// The Ruby argument expressions one wrapper parameter contributes to the C
/// call, driven by its [`ArgPass`] contract. A buffered parameter contributes
/// its packed `(ptr, len)` pair, bytes contribute a copied native buffer plus
/// its length, an object contributes its borrowed pointer, a callback
/// interface contributes its registry key plus the interface's static
/// vtable, and everything else is a single expression.
fn rb_call_args(p: &ParamBinding) -> Vec<String> {
    let name = rb_param_name(&p.name);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => vec![format!("{name}_buf"), format!("{name}_data.bytesize")],
        ArgPass::Bytes { .. } => vec![format!("{name}_buf"), format!("{name}.bytesize")],
        ArgPass::String { .. } => vec![name],
        // Borrowed for the call: the wrapper keeps its own reference.
        ArgPass::Object {
            nullable: false, ..
        } => vec![format!("{name}.handle")],
        // A nullable borrowed object pointer: nil passes as NULL.
        ArgPass::Object { nullable: true, .. } => vec![format!("{name}&.handle")],
        // The registry key (see `render_param_conversion`) and the one
        // process-wide vtable for the interface.
        ArgPass::Callback { .. } => {
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback plan names a callback interface");
            vec![
                format!("{name}_ctx"),
                format!("{}.to_ptr", rb_vtable_const(cb)),
            ]
        }
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Bool => vec![format!("{name}_c")],
            _ => vec![name],
        },
    }
}

/// Emit the statements converting one wrapper parameter into the locals its
/// C call slots reference (see [`rb_call_args`]), per its [`ArgPass`]
/// contract. A buffered parameter is packed into its value-buffer encoding
/// and copied into a `MemoryPointer` the C call borrows for its duration; the
/// caller keeps ownership and the callee never frees it. A callback interface
/// implementation is stored in the module's registry, whose key becomes the
/// `ctx` slot. `qualifier` names the top-level Ruby module when rendering
/// inside a class body.
fn render_param_conversion(out: &mut String, p: &ParamBinding, ind: &str, qualifier: Option<&str>) {
    let q = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let name = rb_param_name(&p.name);
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            w.line(format!("{name}_w = WvBufferWriter.new"));
            crate::codec::render_wv_write(&mut w, &format!("{name}_w"), &name, &p.ty, 0, &q);
            w.line(format!("{name}_data = {name}_w.data"));
            w.line(format!(
                "{name}_buf = FFI::MemoryPointer.new(:uint8, {name}_data.bytesize)"
            ));
            w.line(format!("{name}_buf.put_bytes(0, {name}_data)"));
        }
        ArgPass::Bytes { .. } => {
            w.line(format!(
                "{name}_buf = FFI::MemoryPointer.new(:uint8, {name}.bytesize)"
            ));
            w.line(format!("{name}_buf.put_bytes(0, {name})"));
        }
        ArgPass::Callback { .. } => {
            w.line(format!("{name}_ctx = {q}_wv_cb_register({name})"));
        }
        ArgPass::Direct { .. } if matches!(p.ty, Ty::Bool) => {
            w.line(format!("{name}_c = {name} ? 1 : 0"));
        }
        _ => {}
    }
    out.push_str(&w.finish());
}

/// Emit the statements converting the raw C `result` (plus any out-params)
/// into the wrapper's idiomatic Ruby return value, per the return's
/// [`RetPass`] contract. A buffered return is a producer-allocated value
/// buffer paired with `out_len`: the bytes are copied, released with
/// `weaveffi_free_bytes`, then decoded.
fn render_return_code(out: &mut String, ty: &Ty, ind: &str, qualifier: Option<&str>) {
    let m = qualifier.map(|q| format!("{q}.")).unwrap_or_default();
    let mut w = CodeWriter::two_space().with_depth(ind.len() / 2);
    match plan::ret_pass(Some(ty), "", "") {
        RetPass::Void => unreachable!("void returns skip return-code rendering"),
        RetPass::Buffer => {
            w.line("len = out_len.read(:size_t)");
            w.line("data = result.null? ? ''.b : result.read_string(len)");
            w.line(format!(
                "{m}weaveffi_free_bytes(result, len) unless result.null?"
            ));
            w.line("_wv_r = WvBufferReader.new(data)");
            crate::codec::render_wv_read(&mut w, "_wv_r", "_wv_value", ty, 0, &m);
            w.line("_wv_r.expect_end!");
            w.line("_wv_value");
        }
        RetPass::String => {
            // ffi's read_string yields a BINARY string; the ABI guarantees
            // UTF-8, so retag it before handing it to the caller.
            w.line("return '' if result.null?");
            w.line("str = result.read_string.force_encoding(Encoding::UTF_8)");
            w.line(format!("{m}weaveffi_free_string(result)"));
            w.line("str");
        }
        RetPass::Bytes => {
            w.line("return ''.b if result.null?");
            w.line("len = out_len.read(:size_t)");
            w.line("data = result.read_string(len)");
            w.line(format!("{m}weaveffi_free_bytes(result, len)"));
            w.line("data");
        }
        // A returned interface transfers ownership of a new object
        // reference; wrap it without re-running initialize. A nullable
        // return surfaces nil for null.
        RetPass::Object { nullable, .. } => {
            let name = match ty {
                Ty::Interface(name) => name,
                Ty::Optional(inner) => match inner.as_ref() {
                    Ty::Interface(name) => name,
                    _ => unreachable!("only optional interfaces adopt object returns"),
                },
                _ => unreachable!("object return must be an interface type"),
            };
            let local = local_type_name(name);
            if nullable {
                w.line("return nil if result.null?");
                w.line(format!("{local}._from_ptr(result)"));
            } else {
                w.line("raise Error.new(-1, 'null pointer') if result.null?");
                w.line(format!("{local}._from_ptr(result)"));
            }
        }
        RetPass::Direct => {
            w.line(rb_direct_from_c(ty, "result"));
        }
    }
    out.push_str(&w.finish());
}
