//! Call rendering: sync, async, and iterator wrappers, plus the
//! consumer-facing abstract base class, static vtable, and trampolines of
//! each callback interface.
//!
//! Marshalling dispatch is driven by the shared plans: [`ArgPass`] decides
//! how each parameter crosses, [`RetPass`] how a result (or a trampoline
//! argument) is received, [`plan::elem_free`] what an iterator step owes, and
//! [`CallbackProtocol`] how each trampoline receives its arguments, so this
//! module never re-derives those shapes from `Ty` matches.

use weaveffi_core::abi;
use weaveffi_core::codegen::common::pascal_case;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::lang;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    CallShape, CallbackInterfaceBinding, CallbackMethodBinding, ErrorBinding, FnBinding,
    ModuleBinding, ParamBinding,
};
use weaveffi_core::model::{Prim, WireType};
use weaveffi_core::plan::{self, ArgPass, CallbackProtocol, ErrorStrategy, Free, RetPass};
use weaveffi_core::utils::local_type_name;

use crate::codec::{
    py_decode_borrowed_expr, py_decode_owned_expr, py_pack_fn_name, py_read_expr,
    py_unpack_fn_name, py_write_stmts,
};
use crate::docs::{emit_docstring, emit_fn_docstring};
use crate::entities::{py_checker_name, py_error_factory_name};
use crate::types::{
    py_ctype, py_ctypes_scalar, py_member_name, py_name, py_param_argtypes, py_return_info,
    py_type_hint, py_vtable_class_name, py_vtable_instance_name, py_wrapper_fn_name,
};

/// How a rendered callable is scoped and spelled in the generated Python.
#[derive(Clone, Copy)]
pub(crate) enum FnScope<'a> {
    /// A module-level free function.
    Free {
        /// The owning module's underscore-joined path.
        module_path: &'a str,
        /// Whether the emitted name drops the module-path prefix.
        strip_module_prefix: bool,
    },
    /// An instance method on an interface wrapper: leading `self` parameter,
    /// `self._ptr` passed as the leading C argument. Carries the wrapper
    /// class name, which qualifies the member's iterator helper class.
    Method {
        /// The wrapper class name.
        class: &'a str,
    },
    /// A `@staticmethod` member; carries the wrapper class name, which
    /// qualifies the member's iterator helper class.
    Static {
        /// The wrapper class name.
        class: &'a str,
    },
    /// A `@classmethod` constructor factory returning a new wrapper instance.
    Factory,
    /// The canonical `new` constructor, emitted as `__init__`.
    Init,
}

impl FnScope<'_> {
    /// True for every scope rendered inside a class body (depth 1).
    fn is_member(&self) -> bool {
        !matches!(self, FnScope::Free { .. })
    }

    /// True when the C call receives `self._ptr` as its leading argument.
    fn has_self_slot(&self) -> bool {
        matches!(self, FnScope::Method { .. })
    }

    /// The owner stem of a member's iterator helper class name:
    /// `{Interface}_{member}` for members, the bare function name otherwise.
    fn iterator_owner(&self, fn_name: &str) -> String {
        match self {
            FnScope::Method { class } | FnScope::Static { class } => {
                format!("{class}_{fn_name}")
            }
            _ => fn_name.to_string(),
        }
    }

    /// Indentation depth of the `def` line (0 at module scope, 1 in a class).
    fn depth(&self) -> usize {
        usize::from(self.is_member())
    }
}

/// The emitted Python name for a callable in `scope`: `__init__` for the
/// canonical constructor, otherwise the snake_case member name, with the
/// module-path prefix applied to free functions when configured.
fn py_fn_name(f: &FnBinding, scope: &FnScope) -> String {
    match scope {
        FnScope::Free {
            module_path,
            strip_module_prefix,
        } => py_wrapper_fn_name(module_path, &f.name, *strip_module_prefix),
        FnScope::Init => "__init__".to_string(),
        _ => py_member_name(&f.name),
    }
}

/// Render one callable: a free function or an interface member. `error` is
/// the module's error domain (used when the callable throws); `scope` picks
/// the def spelling, receiver, indent, and result handling. Sync, async, and
/// iterator shapes all route through here so members reuse the free-function
/// marshalling paths.
pub(crate) fn render_callable(
    out: &mut String,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    scope: &FnScope,
) {
    let func_name = py_fn_name(f, scope);
    let depth = scope.depth();
    let ind = "    ".repeat(depth + 1);
    let checker = py_checker_name(f, error);
    let raises = error.filter(|_| f.throws).map(|eb| eb.type_name.as_str());

    let receiver = match scope {
        FnScope::Method { .. } | FnScope::Init => Some("self"),
        FnScope::Factory => Some("cls"),
        _ => None,
    };
    let mut params_sig: Vec<String> = Vec::new();
    if let Some(r) = receiver {
        params_sig.push(r.to_string());
    }
    params_sig.extend(
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

    let is_iterator_ret = matches!(f.shape, CallShape::Iterator(_));

    // The `_...Iterator` helper class is module-level; a member's helper is
    // emitted by `render_interface` ahead of the wrapper class instead.
    if let (Some(Ty::Iterator(inner)), CallShape::Iterator(it)) = (&f.ret, &f.shape) {
        if !scope.is_member() {
            render_iterator_class(out, &it.iter_tag, &f.name, inner, &checker);
        }
    }

    let decorator = match scope {
        FnScope::Static { .. } => Some("@staticmethod"),
        FnScope::Factory => Some("@classmethod"),
        _ => None,
    };

    let mut w = CodeWriter::four_space().with_depth(depth);
    w.blank().blank();
    if let Some(d) = decorator {
        w.line(d);
    }
    w.line(format!(
        "{}def {}({}) -> {}:",
        if f.is_async { "async " } else { "" },
        func_name,
        params_sig.join(", "),
        ret_hint
    ));
    w.indent();

    // An iterator-returning wrapper documents the streaming contract next to
    // whatever the IDL says about the function itself.
    let doc = if is_iterator_ret {
        let streaming = "Returns a lazy iterator: each step pulls one element from the \
                         producer. Exhaust or close() the iterator to release its native \
                         handle (garbage collection also releases it)."
            .to_string();
        Some(match &f.doc {
            Some(d) => format!("{}\n\n{streaming}", d.trim()),
            None => streaming,
        })
    } else {
        f.doc.clone()
    };
    let mut fdoc = String::new();
    emit_fn_docstring(&mut fdoc, &doc, &f.params, &ind, raises);
    w.raw(fdoc);

    // Set before any fallible statement so `__del__` never sees a
    // half-constructed instance when the constructor raises.
    if matches!(scope, FnScope::Init) {
        w.line("self._ptr = None");
    }

    if let Some(msg) = &f.deprecated {
        w.line("import warnings");
        w.line(format!(
            "warnings.warn(\"{}\", DeprecationWarning, stacklevel=2)",
            msg.replace('"', "\\\"")
        ));
    }

    if f.is_async {
        // The async FFI call body is rendered at the function-body indent and
        // spliced in verbatim.
        let mut body = String::new();
        render_async_ffi_call_body(&mut body, f, error, &ind, scope.has_self_slot());
        w.raw(body);
        out.push_str(&w.finish());
    } else {
        let sym = match &f.shape {
            CallShape::Sync(abi) => abi.symbol.as_str(),
            CallShape::Iterator(it) => it.launch.symbol.as_str(),
            CallShape::Async(_) => unreachable!("async handled above"),
        };
        w.line(format!("_fn = _lib.{sym}"));

        let mut argtypes: Vec<String> = Vec::new();
        if scope.has_self_slot() {
            argtypes.push("ctypes.c_void_p".into());
        }
        for p in &f.params {
            argtypes.extend(py_param_argtypes(p));
        }
        let mut out_ret_argtypes = Vec::new();
        let restype;
        if let Some(ret_ty) = &f.ret {
            let (rt, oat) = py_return_info(ret_ty);
            argtypes.extend(oat.iter().cloned());
            restype = rt;
            out_ret_argtypes = oat;
        } else {
            restype = "None".to_string();
        }
        argtypes.push("ctypes.POINTER(_WeaveFFIErrorStruct)".into());

        w.line(format!("_fn.argtypes = [{}]", argtypes.join(", ")));
        w.line(format!("_fn.restype = {restype}"));

        for p in &f.params {
            for line in py_param_conversion(p, &ind) {
                w.raw(&line).raw("\n");
            }
        }

        w.line("_err = _WeaveFFIErrorStruct()");

        // Both bytes and buffered returns carry one trailing `size_t*
        // out_len` slot beside the returned pointer.
        let has_out_len = !out_ret_argtypes.is_empty();
        if has_out_len {
            w.line("_out_len = ctypes.c_size_t(0)");
        }

        let mut call_args: Vec<String> = Vec::new();
        if scope.has_self_slot() {
            call_args.push("_borrow(self)".into());
        }
        for p in &f.params {
            call_args.extend(py_param_call_args(p));
        }
        if has_out_len {
            call_args.push("ctypes.byref(_out_len)".into());
        }
        call_args.push("ctypes.byref(_err)".into());

        let call_expr = format!("_fn({})", call_args.join(", "));
        if f.ret.is_some() {
            w.line(format!("_result = {call_expr}"));
        } else {
            w.line(call_expr);
        }

        w.line(format!("{checker}(_err)"));

        match scope {
            // Constructors receive the owned pointer directly rather than
            // routing through the generic return path.
            FnScope::Init => {
                w.line("if _result is None:");
                w.scope(|w| {
                    w.line("raise WeaveFFIError(-1, \"null pointer\")");
                });
                w.line("self._ptr = _result");
                out.push_str(&w.finish());
            }
            FnScope::Factory => {
                w.line("if _result is None:");
                w.scope(|w| {
                    w.line("raise WeaveFFIError(-1, \"null pointer\")");
                });
                w.line("return cls._from_ptr(_result)");
                out.push_str(&w.finish());
            }
            _ => {
                if is_iterator_ret {
                    // Lazy: hand the caller the iterator wrapper; each step
                    // pulls one element and the wrapper owns the handle.
                    let class = py_iterator_class_name(&scope.iterator_owner(&f.name));
                    w.line(format!("return {class}(_result)"));
                    out.push_str(&w.finish());
                } else {
                    out.push_str(&w.finish());
                    if let Some(ret_ty) = &f.ret {
                        render_return_value(out, ret_ty, &ind);
                    }
                }
            }
        }
    }
}

// ── Param helpers ──

/// The statements preparing one parameter's locals ahead of the C call: a
/// packed `_{name}_buf` for buffered types, a `_{name}_arr` element array
/// for bytes, a `_{name}_ctx` handle-table key for a callback interface.
/// Direct, string, and object types need no preparation.
fn py_param_conversion(p: &ParamBinding, ind: &str) -> Vec<String> {
    let name = py_name(&p.name);
    match p.arg_pass() {
        // Register the implementation in the handle table; the integer key
        // is what the producer receives as `ctx` and hands back to every
        // trampoline and, finally, to `free`.
        ArgPass::Callback { .. } => vec![format!("{ind}_{name}_ctx = _cb_register({name})")],
        // Records and rich enums pack through their dedicated helpers; other
        // buffered shapes (optionals, lists, maps) inline their write
        // statements through a per-parameter writer (so several buffered
        // parameters in one call never collide).
        ArgPass::Buffer { .. } => match p.ty.wire() {
            WireType::User(n) => {
                vec![format!("{ind}_{name}_buf = {}({name})", py_pack_fn_name(n))]
            }
            _ => {
                let mut w = CodeWriter::four_space().with_depth(ind.len() / 4);
                let writer = format!("_{name}_w");
                w.line(format!("{writer} = _BufferWriter()"));
                py_write_stmts(&mut w, &writer, &name, &p.ty, 0);
                w.line(format!("_{name}_buf = {writer}.finish()"));
                w.finish().lines().map(str::to_string).collect()
            }
        },
        ArgPass::Bytes { .. } => {
            vec![format!(
                "{ind}_{name}_arr = (ctypes.c_uint8 * len({name}))(*{name})"
            )]
        }
        _ => vec![],
    }
}

/// The C argument expressions one parameter contributes, in slot order.
fn py_param_call_args(p: &ParamBinding) -> Vec<String> {
    let name = py_name(&p.name);
    match p.arg_pass() {
        // A buffered parameter fills its `(ptr, len)` slot pair with the
        // packed bytes; ctypes passes `bytes` directly for a `c_char_p`
        // argtype.
        ArgPass::Buffer { .. } => vec![format!("_{name}_buf"), format!("len(_{name}_buf)")],
        ArgPass::Bytes { .. } => vec![format!("_{name}_arr"), format!("len({name})")],
        ArgPass::String { .. } => vec![format!("_string_to_bytes({name})")],
        // Interface parameters are borrowed: lend the wrapper's raw pointer
        // and keep the wrapper's own reference; the producer clones if it
        // retains the object. A nullable `Interface?` maps `None` onto a
        // null pointer.
        ArgPass::Object {
            nullable: false, ..
        } => vec![format!("_borrow({name})")],
        ArgPass::Object { nullable: true, .. } => {
            vec![format!("(_borrow({name}) if {name} is not None else None)")]
        }
        // The handle-table key as `ctx`, and the address of the interface's
        // one static vtable.
        ArgPass::Callback { .. } => {
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback family names a callback interface");
            vec![
                format!("_{name}_ctx"),
                format!("ctypes.byref({})", py_vtable_instance_name(cb)),
            ]
        }
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Bool => vec![format!("1 if {name} else 0")],
            Ty::Enum(_) => vec![format!("{name}.value")],
            // Scalars are already the raw slot value the C signature wants.
            _ => vec![name],
        },
    }
}

// ── Return helpers ──

/// The interface name behind a direct or optional interface return.
fn object_interface_name(ty: &Ty) -> &str {
    ty.interface_name()
        .expect("object returns are direct or optional interfaces")
}

/// Append the statements converting `_result` (and `_out_len` when present)
/// into the wrapper's return value, honoring the receive-and-release
/// contract of [`RetPass`].
fn render_return_value(out: &mut String, ty: &Ty, ind: &str) {
    let mut w = CodeWriter::four_space().with_depth(ind.len() / 4);
    // Module and prefix only shape an object return's destroy symbol, which
    // Python defers to the wrapper class's `__del__`; empty context is fine.
    match plan::ret_pass(Some(ty), "", "") {
        // A buffered return hands over one owned encoded value: copy the
        // bytes, release them with `weaveffi_free_bytes`, then decode.
        RetPass::Buffer => {
            w.line("_data = _take_buffer(_result, _out_len.value)");
            match ty.wire() {
                WireType::User(name) => {
                    w.line(format!("return {}(_data)", py_unpack_fn_name(name)));
                }
                _ => {
                    w.line(format!(
                        "return _decode_buffer(_data, lambda _r: {})",
                        py_read_expr(ty, 0)
                    ));
                }
            }
        }
        // Owned string: copy, then release via `weaveffi_free_string`.
        RetPass::String => {
            w.line("return _take_string(_result) or \"\"");
        }
        // Owned buffer: copy, then release via `weaveffi_free_bytes`.
        RetPass::Bytes => {
            w.line("if not _result:");
            w.scope(|w| {
                w.line("return b\"\"");
            });
            w.line("_val = bytes(_result[:_out_len.value])");
            w.line("_lib.weaveffi_free_bytes(_result, ctypes.c_size_t(_out_len.value))");
            w.line("return _val");
        }
        // A returned interface is a new owned reference: wrap it without
        // re-running the class's FFI constructor. A nullable return maps
        // null to `None` instead of raising.
        RetPass::Object { nullable, .. } => {
            let name = local_type_name(object_interface_name(ty));
            w.line("if _result is None:");
            if nullable {
                w.scope(|w| {
                    w.line("return None");
                });
            } else {
                w.scope(|w| {
                    w.line("raise WeaveFFIError(-1, \"null pointer\")");
                });
            }
            w.line(format!("return {name}._from_ptr(_result)"));
        }
        RetPass::Direct => match ty {
            Ty::Bool => {
                w.line("return bool(_result)");
            }
            Ty::Enum(name) => {
                let name = local_type_name(name);
                w.line(format!("return {name}(_result)"));
            }
            _ => {
                w.line("return _result");
            }
        },
        // Callers only render a value for non-void returns.
        RetPass::Void => unreachable!("void returns render no value"),
    }
    out.push_str(&w.finish());
}

// ── Async rendering ──

/// `(param_name, ctypes_type)` pairs for async C callback parameters after `(context, err)`.
fn py_async_cb_trailing_fields(ret: &Option<Ty>) -> Vec<(String, String)> {
    match ret {
        None => vec![],
        Some(ty) => abi::callback_result_params(ty, "")
            .into_iter()
            .map(|p| {
                // An owned string result must keep its raw address (a
                // `c_char_p` slot would auto-convert to `bytes` and lose the
                // pointer `weaveffi_free_string` needs).
                let cty = match py_ctype(&p.ty).as_str() {
                    "ctypes.c_char_p" => "ctypes.c_void_p".to_string(),
                    other => other.to_string(),
                };
                (p.name, cty)
            })
            .collect(),
    }
}

/// Append the success branch of an async completion trampoline: convert the
/// owned `result` slots into the idiomatic value and store it in
/// `_state["val"]`. Async results transfer ownership to the consumer: strings
/// are released with `weaveffi_free_string`, value buffers with
/// `weaveffi_free_bytes`, and an owned interface pointer is adopted by its
/// wrapper class.
fn append_async_success_handler(out: &mut String, ret: &Option<Ty>, ind: &str) {
    let Some(ty) = ret else {
        out.push_str(&format!("{ind}_state[\"val\"] = None\n"));
        return;
    };
    match plan::ret_pass(Some(ty), "", "") {
        // The owned `(result_ptr, result_len)` pair holds one encoded value;
        // `_take_buffer` copies it and frees the producer allocation.
        RetPass::Buffer => {
            out.push_str(&format!(
                "{ind}_state[\"val\"] = {}\n",
                py_decode_owned_expr("result_ptr", "result_len", ty)
            ));
        }
        RetPass::String => {
            // The owned C string is copied and freed by `_take_string` (the
            // callback slot is typed `c_void_p` so the address survives).
            out.push_str(&format!(
                "{ind}_state[\"val\"] = _take_string(result) or \"\"\n"
            ));
        }
        RetPass::Bytes => {
            // Copy the owned buffer, then release the producer allocation.
            out.push_str(&format!("{ind}if not result:\n"));
            out.push_str(&format!("{ind}    _state[\"val\"] = b\"\"\n"));
            out.push_str(&format!("{ind}else:\n"));
            out.push_str(&format!("{ind}    _n = int(result_len)\n"));
            out.push_str(&format!(
                "{ind}    _state[\"val\"] = _take_buffer(ctypes.cast(result, ctypes.c_void_p).value, _n)\n"
            ));
        }
        // A returned interface transfers ownership of a new object reference;
        // wrap it without re-running the class's FFI constructor. A nullable
        // return maps null to `None` instead of an error.
        RetPass::Object { nullable, .. } => {
            let name = local_type_name(object_interface_name(ty));
            if nullable {
                out.push_str(&format!("{ind}if not result:\n"));
                out.push_str(&format!("{ind}    _state[\"val\"] = None\n"));
                out.push_str(&format!("{ind}else:\n"));
                out.push_str(&format!(
                    "{ind}    _state[\"val\"] = {name}._from_ptr(result)\n"
                ));
            } else {
                out.push_str(&format!("{ind}if result is None:\n"));
                out.push_str(&format!(
                    "{ind}    _state[\"err\"] = WeaveFFIError(-1, \"null pointer\")\n"
                ));
                out.push_str(&format!("{ind}else:\n"));
                out.push_str(&format!(
                    "{ind}    _state[\"val\"] = {name}._from_ptr(result)\n"
                ));
            }
        }
        RetPass::Direct => match ty {
            Ty::Bool => {
                out.push_str(&format!("{ind}_state[\"val\"] = bool(result)\n"));
            }
            Ty::Enum(name) => {
                let name = local_type_name(name);
                out.push_str(&format!("{ind}_state[\"val\"] = {name}(result)\n"));
            }
            _ => {
                out.push_str(&format!("{ind}_state[\"val\"] = result\n"));
            }
        },
        // The `let Some(ty)` guard above already returned for void.
        RetPass::Void => unreachable!("void handled above"),
    }
}

/// Render the callback-driven body of an `async def` wrapper at the body
/// indent `ind`.
///
/// The wrapper creates a future on the running `asyncio` loop, builds the
/// `CFUNCTYPE` completion trampoline for the launcher's callback typedef,
/// pins it in `_async_pending` until completion, invokes the launcher (which
/// returns immediately), and awaits the future. The trampoline runs on an
/// arbitrary producer thread: it takes ownership of the result (freeing
/// strings and value buffers through the runtime symbols; owned object
/// pointers are adopted), then resolves the future via
/// `call_soon_threadsafe`. A throwing callable maps the completion
/// error through the module domain's factory (from `error`); a non-throwing
/// one traps with the generic `WeaveFFIError`. When `has_self` is set (an
/// instance method), the launcher receives `self._ptr` as its leading
/// argument.
fn render_async_ffi_call_body(
    out: &mut String,
    f: &FnBinding,
    error: Option<&ErrorBinding>,
    ind: &str,
    has_self: bool,
) {
    let CallShape::Async(a) = &f.shape else {
        unreachable!("render_async_ffi_call_body requires an async call shape");
    };
    // A throwing callable routes the completion error through the domain
    // factory, including the borrowed payload buffer (copied before the
    // producer reclaims it). Traps carry no payload, so the generic error
    // skips it.
    let (err_expr, wants_payload) = match (f.error_strategy(), error) {
        (ErrorStrategy::Throws, Some(eb)) => (
            format!("{}(_code, _msg, _payload)", py_error_factory_name(eb)),
            true,
        ),
        _ => ("WeaveFFIError(_code, _msg)".to_string(), false),
    };

    out.push_str(&format!("{ind}_fn = _lib.{}\n", a.launch.symbol));
    out.push_str(&format!("{ind}_loop = asyncio.get_running_loop()\n"));
    out.push_str(&format!("{ind}_fut = _loop.create_future()\n"));

    let trailing = py_async_cb_trailing_fields(&f.ret);
    let mut cb_param_list: Vec<String> = vec!["context".into(), "err".into()];
    cb_param_list.extend(trailing.iter().map(|(n, _)| n.clone()));
    let cb_params_joined = cb_param_list.join(", ");

    out.push('\n');
    out.push_str(&format!("{ind}def _cb_impl({cb_params_joined}):\n"));
    out.push_str(&format!(
        "{ind}    # Fires exactly once, on a producer thread: take ownership of\n"
    ));
    out.push_str(&format!(
        "{ind}    # the result here, then hop back to the event loop.\n"
    ));
    out.push_str(&format!(
        "{ind}    _state = {{\"err\": None, \"val\": None}}\n"
    ));
    out.push_str(&format!("{ind}    if err and err.contents.code != 0:\n"));
    out.push_str(&format!("{ind}        _code = err.contents.code\n"));
    out.push_str(&format!(
        "{ind}        _msg = err.contents.message.decode(\"utf-8\") if err.contents.message else \"\"\n"
    ));
    if wants_payload {
        out.push_str(&format!(
            "{ind}        _payload = ctypes.string_at(err.contents.payload_ptr, err.contents.payload_len) if err.contents.payload_ptr else b\"\"\n"
        ));
    }
    out.push_str(&format!("{ind}        _lib.weaveffi_error_free(err)\n"));
    out.push_str(&format!("{ind}        _state[\"err\"] = {err_expr}\n"));
    out.push_str(&format!("{ind}    else:\n"));
    // Decoding a malformed result buffer raises; surface that through the
    // future rather than letting the exception escape the C callback.
    out.push_str(&format!("{ind}        try:\n"));
    append_async_success_handler(out, &f.ret, &format!("{ind}            "));
    out.push_str(&format!("{ind}        except Exception as _exc:\n"));
    out.push_str(&format!("{ind}            _state[\"err\"] = _exc\n"));
    out.push('\n');
    out.push_str(&format!("{ind}    def _resolve():\n"));
    out.push_str(&format!("{ind}        _async_pending.pop(_token, None)\n"));
    out.push_str(&format!(
        "{ind}        # A cancelled future must not be resolved.\n"
    ));
    out.push_str(&format!("{ind}        if _fut.cancelled():\n"));
    out.push_str(&format!("{ind}            return\n"));
    out.push_str(&format!("{ind}        if _state[\"err\"] is not None:\n"));
    out.push_str(&format!(
        "{ind}            _fut.set_exception(_state[\"err\"])\n"
    ));
    out.push_str(&format!("{ind}        else:\n"));
    out.push_str(&format!(
        "{ind}            _fut.set_result(_state[\"val\"])\n"
    ));
    out.push('\n');
    out.push_str(&format!("{ind}    _loop.call_soon_threadsafe(_resolve)\n"));
    out.push('\n');

    let mut cf_parts: Vec<String> = vec![
        "ctypes.c_void_p".into(),
        "ctypes.POINTER(_WeaveFFIErrorStruct)".into(),
    ];
    cf_parts.extend(trailing.iter().map(|(_, t)| t.clone()));
    out.push_str(&format!(
        "{ind}_cb_type = ctypes.CFUNCTYPE(None, {})\n",
        cf_parts.join(", ")
    ));
    out.push_str(&format!("{ind}_cb = _cb_type(_cb_impl)\n"));
    out.push_str(&format!(
        "{ind}_token = _async_register(_cb)  # pinned until completion\n"
    ));

    let mut argtypes: Vec<String> = Vec::new();
    if has_self {
        argtypes.push("ctypes.c_void_p".into());
    }
    for p in &f.params {
        argtypes.extend(py_param_argtypes(p));
    }
    if f.cancellable {
        argtypes.push("ctypes.c_void_p".into());
    }
    argtypes.push("_cb_type".into());
    argtypes.push("ctypes.c_void_p".into());

    out.push_str(&format!("{ind}_fn.argtypes = [{}]\n", argtypes.join(", ")));
    out.push_str(&format!("{ind}_fn.restype = None\n"));

    for p in &f.params {
        for line in py_param_conversion(p, ind) {
            out.push_str(&line);
            out.push('\n');
        }
    }

    let mut call_args: Vec<String> = Vec::new();
    if has_self {
        call_args.push("_borrow(self)".into());
    }
    for p in &f.params {
        call_args.extend(py_param_call_args(p));
    }
    if f.cancellable {
        call_args.push("None".into());
    }
    call_args.push("_cb".into());
    call_args.push("None".into());

    out.push_str(&format!("{ind}_fn({})\n", call_args.join(", ")));
    if f.ret.is_some() {
        out.push_str(&format!("{ind}return await _fut\n"));
    } else {
        out.push_str(&format!("{ind}await _fut\n"));
    }
}

// ── Iterator helpers ──

/// The `_out_item` ctypes local for one iterator element slot, plus whether
/// the `next` signature carries a trailing `size_t* out_len`. Owned pointer
/// elements (strings, bytes, buffered values) stay raw `c_void_p` addresses
/// so they survive to be freed, per [`plan::elem_free`].
fn py_iter_slots(inner: &Ty) -> (String, bool) {
    match plan::elem_free(inner) {
        Free::Bytes => ("ctypes.c_void_p".into(), true),
        Free::String => ("ctypes.c_void_p".into(), false),
        Free::None => (py_ctypes_scalar(inner).into(), false),
    }
}

/// The Python expression converting a pulled `_out_item` slot into the
/// element the consumer receives, honoring the owed per-element release
/// ([`plan::elem_free`]).
fn py_read_iter_item(inner: &Ty) -> String {
    match plan::elem_free(inner) {
        // A `ptr` + `len` element: copy the bytes, free them with
        // `weaveffi_free_bytes` (via `_take_buffer`), then decode any
        // buffered value.
        Free::Bytes => match inner.wire() {
            WireType::Prim(Prim::Bytes) => "_take_buffer(_out_item.value, _out_len.value)".into(),
            WireType::User(name) => format!(
                "{}(_take_buffer(_out_item.value, _out_len.value))",
                py_unpack_fn_name(name)
            ),
            _ => format!(
                "_decode_buffer(_take_buffer(_out_item.value, _out_len.value), lambda _r: {})",
                py_read_expr(inner, 0)
            ),
        },
        // Owned string element: copy, then `weaveffi_free_string`. The out
        // slot is a raw `c_void_p`, so the address survives to be freed.
        Free::String => "_take_string(_out_item.value)".into(),
        Free::None => match inner {
            Ty::Enum(name) => {
                format!("{}(_out_item.value)", local_type_name(name))
            }
            // An owned interface element transfers one strong reference,
            // adopted by its wrapper class. A nullable element maps null to
            // `None`.
            Ty::Interface(name) => {
                format!("{}._from_ptr(_out_item.value)", local_type_name(name))
            }
            Ty::Optional(obj) if matches!(obj.as_ref(), Ty::Interface(_)) => format!(
                "({}._from_ptr(_out_item.value) if _out_item.value else None)",
                local_type_name(object_interface_name(inner))
            ),
            Ty::Bool => "bool(_out_item.value)".into(),
            _ => "_out_item.value".into(),
        },
    }
}

/// The module-level helper class name for one iterator-returning callable.
/// `owner` is the function name for a free function, or
/// `{Interface}_{member}` for an interface member.
fn py_iterator_class_name(owner: &str) -> String {
    format!("_{}Iterator", pascal_case(owner))
}

/// Render the module-level `_...Iterator` helper class for one
/// iterator-returning callable, satisfying the pull contract of
/// [`weaveffi_core::plan::IteratorProtocol`]: one producer `next` call per
/// `__next__`, per-element releases after copying, and exactly one `destroy`
/// (eagerly on exhaustion, or from `close()`/`__del__` when iteration is
/// abandoned early). `checker` is the error-check helper the `next` calls
/// route their out-err slot through (typed for a throwing callable, generic
/// otherwise).
pub(crate) fn render_iterator_class(
    out: &mut String,
    iter_tag: &str,
    func_name: &str,
    inner: &Ty,
    checker: &str,
) {
    let class_name = py_iterator_class_name(func_name);
    let (item_scalar, has_out_len) = py_iter_slots(inner);
    let read_expr = py_read_iter_item(inner);
    let mut next_argtypes = vec![
        "ctypes.c_void_p".to_string(),
        format!("ctypes.POINTER({item_scalar})"),
    ];
    if has_out_len {
        next_argtypes.push("ctypes.POINTER(ctypes.c_size_t)".into());
    }
    next_argtypes.push("ctypes.POINTER(_WeaveFFIErrorStruct)".into());
    let mut next_args = vec!["self._ptr".to_string(), "ctypes.byref(_out_item)".into()];
    if has_out_len {
        next_args.push("ctypes.byref(_out_len)".into());
    }
    next_args.push("ctypes.byref(_err)".into());

    out.push_str(&format!("\n\nclass {class_name}:"));
    out.push_str("\n    \"\"\"Lazy iterator over a producer stream: each step pulls one element");
    out.push_str("\n    across the C boundary. The native handle is released exactly once, on");
    out.push_str("\n    exhaustion, on close(), or when the iterator is garbage collected.\"\"\"");
    out.push_str("\n\n    def __init__(self, ptr):");
    out.push_str("\n        self._ptr = ptr");
    out.push_str("\n        self._done = False");

    out.push_str("\n\n    def __iter__(self):");
    out.push_str("\n        return self");

    out.push_str("\n\n    def __next__(self):");
    out.push_str("\n        if self._done:");
    out.push_str("\n            raise StopIteration");
    out.push_str(&format!("\n        _next_fn = _lib.{iter_tag}_next"));
    out.push_str(&format!(
        "\n        _next_fn.argtypes = [{}]",
        next_argtypes.join(", ")
    ));
    out.push_str("\n        _next_fn.restype = ctypes.c_int32");
    out.push_str(&format!("\n        _out_item = {item_scalar}()"));
    if has_out_len {
        out.push_str("\n        _out_len = ctypes.c_size_t(0)");
    }
    out.push_str("\n        _err = _WeaveFFIErrorStruct()");
    out.push_str(&format!(
        "\n        _has = _next_fn({})",
        next_args.join(", ")
    ));
    out.push_str(&format!("\n        {checker}(_err)"));
    out.push_str("\n        if not _has:");
    out.push_str("\n            self._done = True");
    out.push_str("\n            self._destroy()");
    out.push_str("\n            raise StopIteration");
    out.push_str(&format!("\n        return {read_expr}"));

    out.push_str("\n\n    def close(self):");
    out.push_str("\n        \"\"\"Release the native iterator without draining it.\"\"\"");
    out.push_str("\n        self._done = True");
    out.push_str("\n        self._destroy()");

    out.push_str("\n\n    def _destroy(self):");
    out.push_str("\n        if self._ptr is not None:");
    out.push_str(&format!(
        "\n            _destroy_fn = _lib.{iter_tag}_destroy"
    ));
    out.push_str("\n            _destroy_fn.argtypes = [ctypes.c_void_p]");
    out.push_str("\n            _destroy_fn.restype = None");
    out.push_str("\n            _destroy_fn(self._ptr)");
    out.push_str("\n            self._ptr = None");

    out.push_str("\n\n    def __del__(self):");
    out.push_str("\n        self._destroy()");
    out.push('\n');
}

// ── Callback interfaces ──

/// The Python spelling of a trampoline slot name: the ABI slot name,
/// keyword-escaped. Derived slots (`{name}_ptr`, `out_err`) never collide
/// with a keyword, so only single-slot parameters can change spelling.
fn py_slot_name(slot: &str) -> String {
    lang::escape_ident(slot, lang::PYTHON_KEYWORDS)
}

/// The expression converting one trampoline parameter's borrowed (or, for
/// objects, transferred) C slots into the idiomatic value handed to the
/// implementation, per the receiving contract in `pass`.
fn py_trampoline_arg_expr(p: &ParamBinding, pass: &RetPass) -> String {
    let n = &p.name;
    let esc = py_slot_name(n);
    match pass {
        // A buffered argument is a borrowed `({n}_ptr, {n}_len)` pair valid
        // only during the dispatch: copy and decode before returning. Object
        // tokens inside are adopted by the decoded wrappers.
        RetPass::Buffer => py_decode_borrowed_expr(&format!("{n}_ptr"), &format!("{n}_len"), &p.ty),
        RetPass::Bytes => {
            format!("(ctypes.string_at({n}_ptr, {n}_len) if {n}_ptr else b\"\")")
        }
        // `c_char_p` already delivered a `bytes` copy of the borrowed string.
        RetPass::String => format!("_bytes_to_string({esc})"),
        // An object argument transfers one strong reference: adopt it into a
        // wrapper whose disposal owes the destroy symbol. Null is `None` for
        // an `Interface?` slot.
        RetPass::Object { nullable, .. } => {
            let class = local_type_name(object_interface_name(&p.ty));
            if *nullable {
                format!("({class}._from_ptr({esc}) if {esc} else None)")
            } else {
                format!("{class}._from_ptr({esc})")
            }
        }
        RetPass::Direct => match &p.ty {
            Ty::Bool => format!("bool({esc})"),
            Ty::Enum(name) => format!("{}({esc})", local_type_name(name)),
            _ => esc,
        },
        RetPass::Void => unreachable!("parameters are never void"),
    }
}

/// `(coercion, default)` for a callback method's direct return: the Python
/// conversion applied to the implementation's result before it is written
/// into the C return slot, and the value returned after a failure has been
/// reported through `out_err`. `None` for a void method.
fn py_trampoline_return(ret: Option<&Ty>) -> Option<(&'static str, &'static str)> {
    match ret {
        None => None,
        Some(Ty::Bool) => Some(("1 if {} else 0", "0")),
        Some(Ty::F32 | Ty::F64) => Some(("float({})", "0.0")),
        // Integers and C-style enums (an `IntEnum` is an `int`).
        Some(_) => Some(("int({})", "0")),
    }
}

/// The module-level stems of one callback method's trampoline objects:
/// `_{Name}_{method}_cfunctype`, `_{Name}_{method}_trampoline`, and
/// `_{Name}_{method}_cfunc`.
fn py_trampoline_stem(cb: &CallbackInterfaceBinding, method: &str) -> String {
    format!("_{}_{method}", cb.name)
}

/// Render one callback interface: the abstract base class the consumer
/// subclasses, then the ABI side that satisfies every clause of
/// [`CallbackProtocol`]: one `CFUNCTYPE` per method plus the trailing `free`,
/// the `ctypes.Structure` mirroring the C vtable, one trampoline per method,
/// and the single process-wide static vtable instance whose function objects
/// are pinned at module scope for the process lifetime.
///
/// Each trampoline looks its implementation up by the integer `ctx`, decodes
/// borrowed string, bytes, and buffer arguments (freeing nothing), adopts
/// object arguments, and calls the method. Any exception is reported through
/// `{prefix}_error_set(out_err, -4, message)` and a default value is
/// returned, so nothing ever unwinds through the C frame. `free` removes the
/// handle-table entry. `ctypes` acquires the GIL on entry, so the producer
/// may call from any thread.
pub(crate) fn render_callback_interface(
    out: &mut String,
    module: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    prefix: &str,
) {
    let protocol: CallbackProtocol<'_> = cb.protocol(&module.path, prefix);
    let name = &cb.name;
    let vtable_class = py_vtable_class_name(name);
    let vtable_instance = py_vtable_instance_name(name);
    let free_stem = format!("_{name}_vtable_free");

    let mut w = CodeWriter::four_space();

    // The consumer-facing abstract base class.
    w.blank().blank();
    w.line(format!("class {name}(abc.ABC):"));
    w.indent();
    let base_doc = "Consumer-implemented callback interface. Subclass it, implement every \
                    abstract method, and pass an instance wherever the API takes a \
                    `{name}`; the producer may call the methods from any thread until it \
                    releases the instance. An exception raised by a method is reported to \
                    the producer as WeaveFFIError.FOREIGN_ERROR_CODE (-4) and aborts the \
                    call that was in progress."
        .replace("{name}", name);
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
        let ret_hint = m
            .ret
            .as_ref()
            .map(py_type_hint)
            .unwrap_or_else(|| "None".to_string());
        w.blank();
        w.line("@abc.abstractmethod");
        w.line(format!(
            "def {}({}) -> {ret_hint}:",
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

    // One CFUNCTYPE per vtable entry, in declaration order, then `free`.
    w.blank().blank();
    for m in &cb.methods {
        let parts: Vec<String> = std::iter::once(py_ctype(&m.abi_ret))
            .chain(m.abi_params.iter().map(|p| py_ctype(&p.ty)))
            .collect();
        w.line(format!(
            "{}_cfunctype = ctypes.CFUNCTYPE({})",
            py_trampoline_stem(cb, &m.name),
            parts.join(", ")
        ));
    }
    w.line(format!(
        "{free_stem}_cfunctype = ctypes.CFUNCTYPE(None, ctypes.c_void_p)"
    ));

    // The vtable layout: the C struct `{vtable_tag}`.
    w.blank().blank();
    w.line(format!("class {vtable_class}(ctypes.Structure):"));
    w.scope(|w| {
        w.line(format!("\"\"\"The C vtable `{}`.\"\"\"", cb.vtable_tag));
        w.blank();
        w.line("_fields_ = [");
        w.scope(|w| {
            for m in &cb.methods {
                w.line(format!(
                    "(\"{}\", {}_cfunctype),",
                    m.name,
                    py_trampoline_stem(cb, &m.name)
                ));
            }
            w.line(format!("(\"free\", {free_stem}_cfunctype),"));
        });
        w.line("]");
    });

    // One trampoline per method.
    for (m, args) in cb.methods.iter().zip(&protocol.method_args) {
        render_callback_trampoline(&mut w, cb, m, args);
    }

    w.blank().blank();
    w.line(format!("def {free_stem}_trampoline(ctx):"));
    w.scope(|w| {
        w.line("# The producer's last reference is gone; it never touches `ctx` again.");
        w.line("_cb_impls.pop(ctx, None)");
    });

    // The single static vtable, with every function object pinned.
    w.blank().blank();
    w.line(format!(
        "# The one process-wide vtable for {name}. The CFUNCTYPE objects are held at"
    ));
    w.line("# module scope so their function pointers stay valid for the process lifetime.");
    for m in &cb.methods {
        let stem = py_trampoline_stem(cb, &m.name);
        w.line(format!(
            "{stem}_cfunc = {stem}_cfunctype({stem}_trampoline)"
        ));
    }
    w.line(format!(
        "{free_stem}_cfunc = {free_stem}_cfunctype({free_stem}_trampoline)"
    ));
    let entries: Vec<String> = cb
        .methods
        .iter()
        .map(|m| format!("{}_cfunc", py_trampoline_stem(cb, &m.name)))
        .chain(std::iter::once(format!("{free_stem}_cfunc")))
        .collect();
    w.line(format!(
        "{vtable_instance} = {vtable_class}({})",
        entries.join(", ")
    ));

    out.push_str(&w.finish());
}

/// Render the trampoline for one callback method: the `def` whose parameters
/// are the vtable entry's C slots (`ctx`, the parameter slots, `out_err`),
/// receiving each argument per `args` and reporting any exception through
/// `error_set` with the foreign error code.
fn render_callback_trampoline(
    w: &mut CodeWriter,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
    args: &[RetPass],
) {
    let stem = py_trampoline_stem(cb, &m.name);
    let slots: Vec<String> = m.abi_params.iter().map(|p| py_slot_name(&p.name)).collect();
    let call_args: Vec<String> = m
        .params
        .iter()
        .zip(args)
        .map(|(p, pass)| py_trampoline_arg_expr(p, pass))
        .collect();
    let call = format!(
        "_impl.{}({})",
        py_member_name(&m.name),
        call_args.join(", ")
    );
    let ret = py_trampoline_return(m.ret.as_ref());

    w.blank().blank();
    w.line(format!("def {stem}_trampoline({}):", slots.join(", ")));
    w.scope(|w| {
        w.line("try:");
        w.scope(|w| {
            w.line("_impl = _cb_impls[ctx]");
            match ret {
                Some((coerce, _)) => {
                    w.line(format!("_ret = {call}"));
                    w.line(format!("return {}", coerce.replace("{}", "_ret")));
                }
                None => {
                    w.line(call);
                }
            }
        });
        w.line("except Exception as _exc:");
        w.scope(|w| {
            w.line("# Never unwind through the C frame: report the failure and return a");
            w.line("# default; the producer aborts its call with FOREIGN_ERROR_CODE.");
            w.line(
                "_lib.weaveffi_error_set(out_err, -4, str(_exc).encode(\"utf-8\", \"replace\"))",
            );
            if let Some((_, default)) = ret {
                w.line(format!("return {default}"));
            }
        });
    });
}
