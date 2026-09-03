//! The idiomatic JS surface layered over the addon: error classes, interface
//! classes, listener wrappers, function wrappers, and the `index.js`
//! assembler that composes them with the runtime prelude.

use heck::ToLowerCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::{type_name as error_type_name, ERROR_BRAND};
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    BindingModel, ErrorBinding, FnBinding, InterfaceBinding, ListenerBinding, ModuleBinding,
    ParamBinding,
};
use weaveffi_core::plan::ArgPass;
use weaveffi_core::utils::{
    local_type_name, render_prelude, render_trailer, wrapper_name, CommentStyle,
};

use crate::codec::{
    js_read_expr, js_reader_fn, js_writer_fn, model_uses_buffers, render_pack_fns_js,
};
use crate::runtime::{
    model_has_iterators, render_error_brand_js, render_iterator_class_js, render_loader_js,
    BUFFER_RUNTIME_JS,
};
use crate::types::{iface_member_base, js_fn_name, js_param_name, js_str_literal};

/// Recognize an interface-typed return carried directly or as `Interface?`
/// (the only optional that stays a nullable pointer). Buffered returns are
/// handled separately by the wrapper body.
struct RetWrap {
    /// The local JS class name.
    cls: String,
    /// `true` for `Interface?`: the addon surfaces `null` for the absent case.
    optional: bool,
}

/// Recognize a class-typed (interface) return, direct or optional.
fn js_ret_wrap(ret: Option<&Ty>) -> Option<RetWrap> {
    fn direct(ty: &Ty, optional: bool) -> Option<RetWrap> {
        match ty {
            Ty::Interface(n) => Some(RetWrap {
                cls: local_type_name(n).to_string(),
                optional,
            }),
            _ => None,
        }
    }
    match ret? {
        Ty::Optional(inner) => direct(inner, true),
        ty => direct(ty, false),
    }
}

/// The addon-argument expression for one logical parameter, dispatching on
/// its passing contract. Buffered values pack into a `Buffer` via the
/// generated writer; interface instances unwrap to their raw `_handle` (a
/// borrow; the callee never takes ownership); everything else passes through.
fn js_arg_expr(js_name: &str, p: &ParamBinding) -> String {
    match p.arg_pass() {
        ArgPass::Buffer { .. } => format!("__encode({}, {js_name})", js_writer_fn(&p.ty)),
        ArgPass::Object { .. } => {
            let cls = match &p.ty {
                Ty::Interface(n) => local_type_name(n),
                Ty::Optional(inner) => match inner.as_ref() {
                    Ty::Interface(n) => local_type_name(n),
                    other => unreachable!("non-interface optional is buffered: {other:?}"),
                },
                other => unreachable!("object-passed parameter with type {other:?}"),
            };
            format!("{js_name} instanceof {cls} ? {js_name}._handle : {js_name}")
        }
        ArgPass::Direct { .. } | ArgPass::String { .. } | ArgPass::Bytes { .. } => {
            js_name.to_string()
        }
    }
}

/// The rebranding factory a callable's failures route through: the declaring
/// module's domain factory when the callable `throws`, the generic
/// [`ERROR_BRAND`] constructor otherwise (panics and marshalling failures).
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

/// Emit one declaring module's typed error surface onto `wv`: the domain
/// class extending the generic brand, one subclass per code carrying its
/// stable `CODE` and default message, and the factory mapping a raw ABI code
/// (plus the raw payload buffer) to the matching class. Codes that declare
/// payload fields get a decoder that unpacks the buffer and attaches the
/// fields as properties on the error instance; unknown codes fall back to
/// the generic brand (panics and marshalling failures).
///
/// Negative codes are reserved by the runtime (generic error, producer
/// panic, marshalling failure) and domain codes are validated positive-only,
/// so the table lookup never matches a negative code and the trap idiom
/// (fall back to [`ERROR_BRAND`]) holds by construction.
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

/// Emit one wrapper callable's body: pack buffered arguments and unwrap
/// class-typed ones, invoke the addon binding through the rebranding helper,
/// then decode a buffered result or wrap an interface-typed one.
/// Iterator-returning callables launch the native iterator and hand its
/// external to the shared lazy iterator class, decoding buffered elements per
/// step. Shared by free functions and interface members (`self_expr` supplies
/// the leading handle of an instance method).
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
        // Buffered elements arrive as encoded buffers decoded per step.
        let wrap_elem = if inner.is_buffered() {
            format!("(_e) => __decode({}, _e)", js_reader_fn(inner))
        } else {
            "null".to_string()
        };
        w.line(format!("const _it = {call};"));
        w.line(format!(
            "return new WeaveFFIIterator(_it, addon.{addon_name}_iterNext, addon.{addon_name}_iterDestroy, {map_expr}, {wrap_elem});"
        ));
        return;
    }

    if let Some(ret) = f.ret.as_ref() {
        if ret.is_buffered() {
            let reader = js_reader_fn(ret);
            if f.is_async {
                w.line(format!(
                    "return {call}.then((_r) => __decode({reader}, _r));"
                ));
            } else {
                w.line(format!("const _r = {call};"));
                w.line(format!("return __decode({reader}, _r);"));
            }
            return;
        }
    }

    let Some(wrap) = js_ret_wrap(f.ret.as_ref()) else {
        w.line(format!("return {call};"));
        return;
    };
    let cls = &wrap.cls;
    let rewrap = format!("{cls}._fromHandle(_r)");
    match (f.is_async, wrap.optional) {
        (false, false) => {
            w.line(format!("const _r = {call};"));
            w.line(format!("return {rewrap};"));
        }
        (false, true) => {
            w.line(format!("const _r = {call};"));
            w.line(format!("return _r == null ? null : {rewrap};"));
        }
        (true, false) => {
            w.line(format!("return {call}.then((_r) => {rewrap});"));
        }
        (true, true) => {
            w.line(format!(
                "return {call}.then((_r) => (_r == null ? null : {rewrap}));"
            ));
        }
    }
}

/// Emit one interface's JS class onto `wv`. The class owns the opaque handle
/// and frees it once, via explicit `destroy()` or a `FinalizationRegistry`
/// safety net. A sync constructor named `new` becomes the JS `constructor`;
/// every other constructor becomes a static factory; methods pass the wrapped
/// handle as the leading addon argument; statics are static methods.
fn render_interface_class_js(
    out: &mut String,
    i: &InterfaceBinding,
    m: &ModuleBinding,
    strip: bool,
) {
    let name = &i.name;
    let destroy_js = wrapper_name(&m.path, &iface_member_base(name, "destroy"), strip);
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
                emit_wrapper_body_js(w, f, &addon_name, Some("this._handle"), &map);
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
        // Explicit cleanup; guarded so a double `destroy()` (or destroy-then-GC)
        // is a no-op rather than a double free.
        w.block("destroy() {", "}", |w| {
            w.block("if (this._handle) {", "}", |w| {
                w.line(format!("{name}._cleanup.unregister(this);"));
                w.line(format!("addon.{destroy_js}(this._handle);"));
                w.line("this._handle = 0;");
            });
        });
    });

    // Wrap an owned handle returned by the addon without running the public
    // constructor (which would invoke the native constructor again).
    w.block(
        format!("{name}._fromHandle = function (handle) {{"),
        "};",
        |w| {
            w.line(format!("const _o = Object.create({name}.prototype);"));
            w.line("_o._handle = handle;");
            w.line(format!("{name}._cleanup.register(_o, handle, _o);"));
            w.line("return _o;");
        },
    );
    w.block(
        format!("{name}._cleanup = new FinalizationRegistry((handle) => {{"),
        "});",
        |w| {
            w.line(format!("if (handle) {{ addon.{destroy_js}(handle); }}"));
        },
    );
    w.line(format!("wv.{name} = {name};"));
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the wrapping `wv.registerX` for a listener whose callback carries
/// buffered arguments: the addon delivers those as borrowed-then-copied
/// `Buffer`s, so the wrapper decodes them before invoking the user's
/// callback. Listeners with only direct arguments keep the plain addon
/// re-export.
fn render_listener_wrapper_js(
    out: &mut String,
    m: &ModuleBinding,
    l: &ListenerBinding,
    strip: bool,
) {
    let Some(cb) = m.callback(&l.event_callback) else {
        return;
    };
    if !cb.params.iter().any(|p| p.ty.is_buffered()) {
        return;
    }
    let register = js_fn_name(&m.path, &format!("register_{}", l.name), strip);
    let params: Vec<String> = cb.params.iter().map(|p| js_param_name(&p.name)).collect();
    let args: Vec<String> = cb
        .params
        .iter()
        .map(|p| {
            let n = js_param_name(&p.name);
            if p.ty.is_buffered() {
                format!("__decode({}, {n})", js_reader_fn(&p.ty))
            } else {
                n
            }
        })
        .collect();
    let mut w = CodeWriter::two_space();
    w.block(
        format!("wv.{register} = function (callback) {{"),
        "};",
        |w| {
            w.block(
                format!(
                    "return addon.{register}(function ({}) {{",
                    params.join(", ")
                ),
                "});",
                |w| {
                    w.line(format!("callback({});", args.join(", ")));
                },
            );
        },
    );
    out.push_str(&w.finish());
}

/// The JS loader (`index.js`). Re-exports the native addon's bindings, then
/// layers the idiomatic surface on top: the generic error brand plus one
/// typed error class per declared domain, the private value-buffer runtime
/// with one pack/unpack pair per record and rich enum, wrapper classes for
/// interfaces, and one wrapper per module function so failures rebrand as the
/// right error class and value types cross as plain objects rather than raw
/// buffers.
pub(crate) fn render_node_index(model: &BindingModel, strip: bool, input_basename: &str) -> String {
    let dbl = CommentStyle::DoubleSlash;
    let mut out = render_prelude(dbl, input_basename);
    render_loader_js(&mut out);
    render_error_brand_js(&mut out);

    if model_uses_buffers(model) {
        out.push_str(BUFFER_RUNTIME_JS);
        for m in &model.modules {
            render_pack_fns_js(&mut out, m);
        }
    }

    if model_has_iterators(model) {
        render_iterator_class_js(&mut out);
    }

    for m in &model.modules {
        if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
            render_error_classes_js(&mut out, eb);
        }
        for i in &m.interfaces {
            render_interface_class_js(&mut out, i, m, strip);
        }
        for l in &m.listeners {
            render_listener_wrapper_js(&mut out, m, l, strip);
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
