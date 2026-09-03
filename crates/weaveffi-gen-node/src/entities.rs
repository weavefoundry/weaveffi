//! The idiomatic JS surface layered over the addon: error classes, interface
//! classes, callback-interface adapters, function wrappers, and the
//! `index.js` assembler that composes them with the runtime prelude.

use heck::ToLowerCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::{type_name as error_type_name, ERROR_BRAND};
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    BindingModel, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding,
    ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{ArgPass, RetPass};
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};

use crate::codec::{js_read_expr, js_reader_fn, js_writer_fn, render_pack_fns_js};
use crate::runtime::{
    render_error_brand_js, render_iterator_class_js, render_loader_js, render_object_helpers_js,
    BUFFER_RUNTIME_JS,
};
use crate::types::{
    iface_lifecycle_base, iface_member_base, js_adapter_name, js_fn_name, js_method_name,
    js_param_name, js_str_literal,
};

/// Recognize an interface-typed value carried directly or as `Interface?`
/// (the only optional that stays a nullable pointer). Buffered values are
/// handled separately by the wrapper body.
struct ObjectShape {
    /// The local JS class name.
    cls: String,
    /// `true` for `Interface?`: the addon surfaces `null` for the absent case.
    nullable: bool,
}

/// Recognize a class-typed (interface) value, direct or optional.
fn js_object_shape(ty: Option<&Ty>) -> Option<ObjectShape> {
    let ty = ty?;
    let nullable = matches!(ty, Ty::Optional(_));
    ty.interface_name().map(|n| ObjectShape {
        cls: local_type_name(n).to_string(),
        nullable,
    })
}

/// The JS expression adopting one raw addon value `expr` (an object handle,
/// or `null` for an absent `Interface?`) into a wrapper instance.
fn js_adopt_expr(shape: &ObjectShape, expr: &str) -> String {
    let cls = &shape.cls;
    if shape.nullable {
        format!("({expr} == null ? null : {cls}._adopt({expr}))")
    } else {
        format!("{cls}._adopt({expr})")
    }
}

/// The JS expression converting one raw addon value `expr` of type `ty`
/// received *from* the producer (a return, an async result, an iterator
/// element, or a callback-method argument) into its idiomatic form: buffers
/// decode, object handles are adopted, everything else passes through.
fn js_receive_expr(ty: &Ty, expr: &str) -> String {
    if ty.is_buffered() {
        return format!("__decode({}, {expr})", js_reader_fn(ty));
    }
    match js_object_shape(Some(ty)) {
        Some(shape) => js_adopt_expr(&shape, expr),
        None => expr.to_string(),
    }
}

/// The addon-argument expression for one logical parameter, dispatching on
/// its passing contract. Buffered values pack into a `Buffer` via the
/// generated writer; interface instances are borrowed to their raw handle
/// (the callee never takes ownership; `Interface?` passes `null` for none);
/// callback-interface implementations are wrapped in the interface's adapter
/// so the addon can hand them raw values; everything else passes through.
fn js_arg_expr(js_name: &str, p: &ParamBinding) -> String {
    match p.arg_pass() {
        ArgPass::Buffer { .. } => format!("__encode({}, {js_name})", js_writer_fn(&p.ty)),
        ArgPass::Object { nullable, .. } => {
            let cls = local_type_name(
                p.ty.interface_name()
                    .expect("object-passed parameter names an interface"),
            );
            if nullable {
                format!("({js_name} == null ? null : __borrow({js_name}, {cls}))")
            } else {
                format!("__borrow({js_name}, {cls})")
            }
        }
        ArgPass::Callback { .. } => {
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback-passed parameter names a callback interface");
            format!("{}({js_name})", js_adapter_name(cb))
        }
        ArgPass::Direct { .. } | ArgPass::String { .. } | ArgPass::Bytes { .. } => {
            js_name.to_string()
        }
    }
}

/// The rebranding factory a callable's failures route through: the declaring
/// module's domain factory when the callable `throws`, the generic
/// [`ERROR_BRAND`] constructor otherwise. Negative runtime codes (a producer
/// panic, a marshalling failure, a callback implementation that raised) never
/// match a domain table, so both paths surface them as the generic brand.
fn js_error_map_expr(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match error {
        Some(eb) if f.throws => js_error_factory_name(eb),
        _ => "__generic".to_string(),
    }
}

/// `__kvErrorFrom`, the code-to-class factory of the domain declared by
/// `owner_path`. Derived from the owner so inheriting submodules name the
/// same function.
fn js_error_factory_name(eb: &ErrorBinding) -> String {
    format!("__{}ErrorFrom", eb.owner_path.to_lower_camel_case())
}

/// Emit one C-style enum as a frozen runtime object on `wv`, the value a
/// `types.d.ts` `export enum` declaration promises: every variant name maps
/// to its discriminant and, as TypeScript's own `enum` lowering does, every
/// discriminant maps back to its variant name. Rich enums are tagged unions
/// with no runtime value and are skipped.
fn render_enum_js(out: &mut String, e: &EnumBinding) {
    if e.is_rich() {
        return;
    }
    let mut w = CodeWriter::two_space();
    w.block(format!("wv.{} = Object.freeze({{", e.name), "});", |w| {
        for v in &e.variants {
            w.line(format!("{}: {},", v.name, v.value));
        }
        for v in &e.variants {
            w.line(format!("{}: '{}',", v.value, v.name));
        }
    });
    out.push_str(&w.finish());
}

/// Emit one declaring module's typed error surface onto `wv`: the domain
/// class extending the generic brand, one subclass per code carrying its
/// stable `CODE` and default message, and the factory mapping a raw ABI code
/// (plus the raw payload buffer) to the matching class. Codes that declare
/// payload fields get a decoder that unpacks the buffer and attaches the
/// fields as properties on the error instance; unknown codes fall back to
/// the generic brand.
///
/// Negative codes are reserved by the runtime (generic error, producer
/// panic, marshalling failure, foreign callback failure) and domain codes are
/// validated positive-only, so the table lookup never matches a negative code
/// and the trap idiom (fall back to [`ERROR_BRAND`]) holds by construction.
fn render_error_classes_js(out: &mut String, eb: &ErrorBinding) {
    let domain = &eb.type_name;
    let factory = js_error_factory_name(eb);
    let table = format!("__{}ErrorCodes", eb.owner_path.to_lower_camel_case());
    let payloads = format!("__{}ErrorPayloads", eb.owner_path.to_lower_camel_case());
    let has_payloads = eb.codes.iter().any(|c| !c.fields.is_empty());

    let mut w = CodeWriter::two_space();
    w.block(
        format!("class {domain} extends {ERROR_BRAND} {{"),
        "}",
        |w| {
            w.block("constructor(code, message) {", "}", |w| {
                w.line("super(code, message);");
                w.line(format!("this.name = '{domain}';"));
            });
        },
    );
    w.line(format!("wv.{domain} = {domain};"));
    for c in &eb.codes {
        let class = error_type_name(&c.name, "Error");
        let default_msg = js_str_literal(&c.message);
        w.block(format!("class {class} extends {domain} {{"), "}", |w| {
            w.block("constructor(message) {", "}", |w| {
                w.line(format!("super({}, message || '{default_msg}');", c.value));
                w.line(format!("this.name = '{class}';"));
            });
        });
        w.line(format!("{class}.CODE = {};", c.value));
        w.line(format!("wv.{class} = {class};"));
    }
    let entries: Vec<String> = eb
        .codes
        .iter()
        .map(|c| format!("{}: {}", c.value, error_type_name(&c.name, "Error")))
        .collect();
    w.line(format!(
        "const {table} = Object.freeze({{ {} }});",
        entries.join(", ")
    ));
    if has_payloads {
        // One payload decoder per code that declares fields, reading the
        // code's fields in declaration (wire) order.
        let decoders: Vec<String> = eb
            .codes
            .iter()
            .filter(|c| !c.fields.is_empty())
            .map(|c| {
                let fields: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| format!("{}: {}", f.name, js_read_expr(&f.ty)))
                    .collect();
                format!("{}: (r) => ({{ {} }})", c.value, fields.join(", "))
            })
            .collect();
        w.line(format!(
            "const {payloads} = Object.freeze({{ {} }});",
            decoders.join(", ")
        ));
    }
    w.block(
        format!("function {factory}(code, message, payload) {{"),
        "}",
        |w| {
            w.line(format!("const _cls = {table}[code];"));
            w.line(format!(
                "const _err = _cls === undefined ? new {ERROR_BRAND}(code, message) : new _cls(message);"
            ));
            if has_payloads {
                w.line(format!("const _decode = {payloads}[code];"));
                w.block(
                    "if (_decode !== undefined && payload != null) {",
                    "}",
                    |w| {
                        w.line("Object.assign(_err, __decode(_decode, payload));");
                    },
                );
            }
            w.line("return _err;");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// Emit one wrapper callable's body: pack buffered arguments, borrow
/// class-typed ones, and adapt callback implementations; invoke the addon
/// binding through the rebranding helper; then decode a buffered result or
/// adopt an object-typed one. Iterator-returning callables launch the native
/// iterator and hand its external to the shared lazy iterator class, decoding
/// or adopting each element per step. Shared by free functions and interface
/// members (`self_expr` supplies the leading handle of an instance method).
fn emit_wrapper_body_js(
    w: &mut CodeWriter,
    f: &FnBinding,
    addon_name: &str,
    self_expr: Option<&str>,
    map_expr: &str,
) {
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = self_expr {
        args.push(s.to_string());
    }
    for p in &f.params {
        args.push(js_arg_expr(&js_param_name(&p.name), p));
    }
    let args = args.join(", ");
    let invoke = if f.is_async {
        "__invokeAsync"
    } else {
        "__invoke"
    };
    let call = format!("{invoke}(addon.{addon_name}, [{args}], {map_expr})");

    if let Some(Ty::Iterator(inner)) = f.ret.as_ref() {
        // Launch, then wrap the external in the lazy iterator: one native
        // `next` per consumer step, `destroy` on exhaustion or early exit.
        // Buffered elements arrive as encoded buffers decoded per step;
        // object elements arrive as owned handles adopted per step.
        let received = js_receive_expr(inner, "_e");
        let wrap_elem = if received == "_e" {
            "null".to_string()
        } else {
            format!("(_e) => {received}")
        };
        w.line(format!("const _it = {call};"));
        w.line(format!(
            "return new WeaveFFIIterator(_it, addon.{addon_name}_iterNext, addon.{addon_name}_iterDestroy, {map_expr}, {wrap_elem});"
        ));
        return;
    }

    let Some(ret) = f.ret.as_ref() else {
        w.line(format!("return {call};"));
        return;
    };
    let received = js_receive_expr(ret, "_r");
    if received == "_r" {
        w.line(format!("return {call};"));
    } else if f.is_async {
        w.line(format!("return {call}.then((_r) => {received});"));
    } else {
        w.line(format!("const _r = {call};"));
        w.line(format!("return {received};"));
    }
}

/// Emit one interface's JS class onto `wv`. The class holds the object's
/// native handle (one strong reference, as a `bigint`) and releases it
/// exactly once: through `close()`, through `Symbol.dispose` (which aliases
/// `close`, for `using` declarations), or through a `FinalizationRegistry`
/// backstop when the wrapper is collected unclosed. `_adopt` wraps a handle
/// received from the producer without running the public constructor, and
/// `_cloneHandle` produces a second strong reference (through the producer's
/// `_clone` symbol) for the value-buffer codec to write as an object token.
///
/// A sync constructor named `new` becomes the JS `constructor`; every other
/// constructor becomes a static factory; methods pass the wrapped handle as
/// the leading addon argument; statics are static methods.
fn render_interface_class_js(
    out: &mut String,
    i: &InterfaceBinding,
    m: &ModuleBinding,
    strip: bool,
) {
    let name = &i.name;
    let destroy_js = wrapper_name(&m.path, &iface_lifecycle_base(name, "destroy"), strip);
    let clone_js = wrapper_name(&m.path, &iface_lifecycle_base(name, "clone"), strip);
    let error = m.error.as_ref();

    let mut w = CodeWriter::two_space();
    w.block(format!("class {name} {{"), "}", |w| {
        let canonical = i
            .constructors
            .iter()
            .find(|c| c.name == "new" && !c.is_async);
        if let Some(c) = canonical {
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &c.name), strip);
            let params: Vec<String> = c.params.iter().map(|p| js_param_name(&p.name)).collect();
            let args: Vec<String> = c
                .params
                .iter()
                .map(|p| js_arg_expr(&js_param_name(&p.name), p))
                .collect();
            let map = js_error_map_expr(c, error);
            w.block(format!("constructor({}) {{", params.join(", ")), "}", |w| {
                w.line(format!(
                    "this._handle = __invoke(addon.{addon_name}, [{}], {map});",
                    args.join(", ")
                ));
                w.line(format!(
                    "{name}._cleanup.register(this, this._handle, this);"
                ));
            });
        }
        for c in &i.constructors {
            if canonical.is_some_and(|canon| std::ptr::eq(canon, c)) {
                continue;
            }
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &c.name), strip);
            let factory = c.name.to_lower_camel_case();
            let params: Vec<String> = c.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(c, error);
            w.block(
                format!("static {factory}({}) {{", params.join(", ")),
                "}",
                |w| {
                    emit_wrapper_body_js(w, c, &addon_name, None, &map);
                },
            );
        }
        for f in &i.methods {
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &f.name), strip);
            let method = f.name.to_lower_camel_case();
            let params: Vec<String> = f.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(f, error);
            w.block(format!("{method}({}) {{", params.join(", ")), "}", |w| {
                emit_wrapper_body_js(
                    w,
                    f,
                    &addon_name,
                    Some(&format!("__borrow(this, {name})")),
                    &map,
                );
            });
        }
        for f in &i.statics {
            let addon_name = wrapper_name(&m.path, &iface_member_base(name, &f.name), strip);
            let method = f.name.to_lower_camel_case();
            let params: Vec<String> = f.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(f, error);
            w.block(
                format!("static {method}({}) {{", params.join(", ")),
                "}",
                |w| {
                    emit_wrapper_body_js(w, f, &addon_name, None, &map);
                },
            );
        }
        // Explicit release; guarded so a double `close()` (or close-then-GC)
        // is a no-op rather than a double release.
        w.block("close() {", "}", |w| {
            w.block("if (this._handle) {", "}", |w| {
                w.line(format!("{name}._cleanup.unregister(this);"));
                w.line(format!("addon.{destroy_js}(this._handle);"));
                w.line("this._handle = 0n;");
            });
        });
        // A second strong reference for the value-buffer codec: the object
        // token written into a buffer must never be the handle this wrapper
        // still owns.
        w.block("_cloneHandle() {", "}", |w| {
            w.line(format!("return addon.{clone_js}(__borrow(this, {name}));"));
        });
    });

    // Adopt an owned handle received from the addon without running the
    // public constructor (which would invoke the native constructor again).
    w.block(format!("{name}._adopt = function (handle) {{"), "};", |w| {
        w.line(format!("const _o = Object.create({name}.prototype);"));
        w.line("_o._handle = handle;");
        w.line(format!("{name}._cleanup.register(_o, handle, _o);"));
        w.line("return _o;");
    });
    w.block(
        format!("{name}._cleanup = new FinalizationRegistry((handle) => {{"),
        "});",
        |w| {
            w.line(format!("if (handle) {{ addon.{destroy_js}(handle); }}"));
        },
    );
    w.block("if (typeof Symbol.dispose === 'symbol') {", "}", |w| {
        w.line(format!(
            "{name}.prototype[Symbol.dispose] = {name}.prototype.close;"
        ));
    });
    w.line(format!("wv.{name} = {name};"));
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the adapter for one callback interface. The consumer passes any
/// object with the interface's camelCase methods; the adapter is what the
/// addon actually holds (behind a `napi_ref`) and calls by raw IDL method
/// name with raw values: strings, numbers, `bigint`s, booleans, and `Buffer`s
/// for buffered arguments (borrowed and already copied by the addon), and
/// `bigint` handles (or `null` for an absent `Interface?`) for object
/// arguments, each carrying one strong reference the adapter adopts into a
/// wrapper before invoking the implementation. Return values pass straight
/// back; the addon converts them to the method's C return type. An exception
/// thrown by the implementation propagates to the addon's trampoline, which
/// reports it through `out_err` as a foreign error.
fn render_callback_adapter_js(
    out: &mut String,
    m: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
    prefix: &str,
) {
    let adapter = js_adapter_name(&cb.name);
    let protocol = cb.protocol(&m.path, prefix);
    let mut w = CodeWriter::two_space();
    w.block(format!("function {adapter}(impl) {{"), "}", |w| {
        w.block("if (impl === null || impl === undefined) {", "}", |w| {
            w.line(format!(
                "throw new {ERROR_BRAND}(-3, '{} implementation must be an object');",
                cb.name
            ));
        });
        w.block("return {", "};", |w| {
            for (method, args) in cb.methods.iter().zip(&protocol.method_args) {
                let params: Vec<String> = method
                    .params
                    .iter()
                    .map(|p| js_param_name(&p.name))
                    .collect();
                let converted: Vec<String> = method
                    .params
                    .iter()
                    .zip(args)
                    .map(|(p, pass)| {
                        let n = js_param_name(&p.name);
                        match pass {
                            RetPass::Buffer | RetPass::Object { .. } => js_receive_expr(&p.ty, &n),
                            _ => n,
                        }
                    })
                    .collect();
                w.block(
                    format!("{}({}) {{", method.name, params.join(", ")),
                    "},",
                    |w| {
                        w.line(format!(
                            "return impl.{}({});",
                            js_method_name(&method.name),
                            converted.join(", ")
                        ));
                    },
                );
            }
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The JS loader (`index.js`). Re-exports the native addon's bindings, then
/// layers the idiomatic surface on top: the generic error brand plus one
/// typed error class per declared domain, the private value-buffer runtime
/// with one pack/unpack pair per record and rich enum, wrapper classes for
/// interfaces, one adapter per callback interface, and one wrapper per module
/// function so failures rebrand as the right error class and value types
/// cross as plain objects rather than raw buffers.
pub(crate) fn render_node_index(model: &BindingModel, strip: bool, input_basename: &str) -> String {
    let dbl = CommentStyle::DoubleSlash;
    let mut out = render_prelude(dbl, input_basename);
    render_loader_js(&mut out);
    render_error_brand_js(&mut out);

    if model.has_interfaces() {
        render_object_helpers_js(&mut out);
    }

    if model.has_buffers() {
        out.push_str(BUFFER_RUNTIME_JS);
        for m in &model.modules {
            render_pack_fns_js(&mut out, m);
        }
    }

    if model.has_iterators() {
        render_iterator_class_js(&mut out);
    }

    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error_classes_js(&mut out, eb);
        }
        for e in &m.enums {
            render_enum_js(&mut out, e);
        }
        for cb in &m.callback_interfaces {
            render_callback_adapter_js(&mut out, m, cb, &model.prefix);
        }
        for i in &m.interfaces {
            render_interface_class_js(&mut out, i, m, strip);
        }
    }

    // One wrapper per module function, so every failure is rebranded and
    // buffered or class-typed values cross as idiomatic values.
    for m in &model.modules {
        for f in &m.functions {
            let js = js_fn_name(&m.path, &f.name, strip);
            let params: Vec<String> = f.params.iter().map(|p| js_param_name(&p.name)).collect();
            let map = js_error_map_expr(f, m.error.as_ref());
            let mut w = CodeWriter::two_space();
            w.block(
                format!("wv.{js} = function ({}) {{", params.join(", ")),
                "};",
                |w| {
                    emit_wrapper_body_js(w, f, &js, None, &map);
                },
            );
            out.push_str(&w.finish());
        }
    }

    out.push_str("\nmodule.exports = wv;\n\n");
    out.push_str(&render_trailer(dbl, "index.js"));
    out
}
