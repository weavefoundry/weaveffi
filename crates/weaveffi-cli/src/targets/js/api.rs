//! The idiomatic JavaScript API, shared by both transports.
//!
//! Everything here calls the native side through `$raw`, an object with one
//! function per C symbol (keyed by the symbol name) that the transport
//! provides: the N-API addon on Node.js, linear-memory glue on WebAssembly.
//! The raw calling convention, which both transports implement, is:
//!
//! * **Arguments**, in ABI order (an instance method's handle first): a
//!   direct value as a `number`, `bigint`, or `boolean`; a string as a
//!   `string`; bytes and value buffers as a `Uint8Array`; an object as its
//!   handle (`null` for an absent `Interface?`); a callback interface as an
//!   adapter object (`null` for an absent `Cb?`); and, last, a cancellable
//!   call's cancel token handle (or `null`).
//! * **Results** come back the same way: a direct value, a `string`, a
//!   `Uint8Array`, or an object handle (one strong reference). An async
//!   launcher returns a `Promise` of its result; an iterator launcher
//!   returns a handle, and the iterator's `next` returns an element or
//!   `undefined` when the iterator is exhausted. A module's contract
//!   function (`{prefix}_{module}_contract`) returns its table as a
//!   `BigUint64Array` of `id, hash` pairs.
//! * **Failures** throw (or reject with) a runtime `$Fault`.
//! * **Callback adapters** have one method per callback method, named as in
//!   the IDL, taking raw arguments (objects as adopted handles) and
//!   returning the raw value of the method's return: a direct value, a
//!   `string`, a `Uint8Array` (bytes or an encoded buffer, which the
//!   transport copies into a run from `{prefix}_alloc`), or an object handle
//!   that is a fresh strong reference (`null` for none). An adapter method
//!   that throws a `$Fault` reports its code, message, and payload; any
//!   other exception is reported as code -4 with its message.
//!
//! The transport also defines `$token`, which turns an object token read
//! from a value buffer into a handle.

use std::collections::BTreeSet;

use weaveffi_model::model::{
    CallShape, CallbackInterfaceBinding, CallbackMethodBinding, ErrorBinding, FnBinding,
    InterfaceBinding, Model, ModuleBinding, ParamBinding,
};
use weaveffi_model::plan::{ArgPass, RetPass};
use weaveffi_model::ty::Ty;

use crate::codegen::CodeWriter;
use crate::targets::js::codec::{emit_codecs, read_expr, reader_fn, write_expr, writer_fn};
use crate::targets::js::names::{
    callback_method_name, code_class, decl, direct_prim, error_mapper, error_owner, fn_name,
    helper, js_string, member_name, module_name, param_name, type_decl,
};

/// Render the shared API section of `index.js`: everything after the
/// transport has defined `$raw` and `$token`.
pub(crate) fn render_api(w: &mut CodeWriter, model: &Model) {
    let prefix = model.prefix();

    if model.callables().any(|(_, f)| f.cancellable) {
        w.line("// The native cancel tokens behind `AbortSignal`s.");
        w.block("const $tokens = {", "};", |w| {
            w.line(format!(
                "create: () => $raw.{prefix}_cancel_token_create(),"
            ));
            w.line(format!(
                "cancel: (t) => $raw.{prefix}_cancel_token_cancel(t),"
            ));
            w.line(format!(
                "destroy: (t) => $raw.{prefix}_cancel_token_destroy(t),"
            ));
        });
        w.blank();
    }

    w.line("/**");
    w.line(" * The native library's live-resource counters (0 objects, 1 callbacks,");
    w.line(" * 2 iterators, 3 cancel tokens, 4 byte runs), for leak tests. Kind -1 is");
    w.line(" * `1n` when the library counts at all (its `leak-check` feature), else");
    w.line(" * every kind is `0n`.");
    w.line(" */");
    w.block("export function __debugLive(kind) {", "}", |w| {
        w.line(format!("return $raw.{prefix}_debug_live(kind);"));
    });
    w.blank();

    let raised = raised_domains(model);
    for m in &model.modules {
        if let Some(eb) = m.errors.as_ref() {
            emit_error_domain(w, model, m, eb, &raised);
        }
        for e in m.enums.iter().filter(|e| !e.rich) {
            w.block(
                format!("const {} = Object.freeze({{", decl(&m.segments, &e.name)),
                "});",
                |w| {
                    for v in &e.variants {
                        w.line(format!("{}: {},", v.name, v.value));
                    }
                    for v in &e.variants {
                        // A negative number isn't a property name literal.
                        let key = if v.value < 0 {
                            js_string(&v.value.to_string())
                        } else {
                            v.value.to_string()
                        };
                        w.line(format!("{key}: {},", js_string(&v.name)));
                    }
                },
            );
            w.blank();
        }
    }

    let before = w.as_str().len();
    emit_codecs(w, model);
    if w.as_str().len() != before {
        w.blank();
    }

    for m in &model.modules {
        for cb in &m.callback_interfaces {
            emit_adapter(w, model, m, cb);
        }
        for i in &m.interfaces {
            emit_class(w, model, m, i);
        }
        for f in &m.functions {
            emit_iterator_spec(w, model, m, f, &decl(&m.segments, &f.name));
            let is_async = if f.is_async() { "async " } else { "" };
            let mut params = js_params(f);
            if f.cancellable {
                params.push("$options".into());
            }
            let name = fn_name(&f.name);
            w.block(
                format!(
                    "const {} = {is_async}function {name}({}) {{",
                    decl(&m.segments, &name),
                    params.join(", ")
                ),
                "};",
                |w| {
                    emit_body(
                        w,
                        model,
                        m,
                        f,
                        None,
                        &decl(&m.segments, &f.name),
                        Finish::Return,
                    );
                },
            );
            w.blank();
        }
    }

    for m in model.roots() {
        emit_namespace(w, model, m, &format!("export const {} = ", module_name(m)));
        w.blank();
    }
}

/// The JavaScript parameter names of a callable.
fn js_params(f: &FnBinding) -> Vec<String> {
    f.params.iter().map(|p| param_name(&p.name)).collect()
}

/// The declaration keys (`kv$KvError`) of the error domains some callback
/// method may raise, which need a `$raise$...` mapper.
fn raised_domains(model: &Model) -> BTreeSet<String> {
    model
        .callback_interfaces()
        .filter(|(_, cb)| cb.methods.iter().any(|m| m.throws))
        .filter_map(|(m, _)| model.error_domain(m))
        .map(|eb| {
            let owner = error_owner(&model.modules, eb);
            decl(&owner.segments, &eb.name)
        })
        .collect()
}

/// Emit one error domain: the domain class, one class per code (whose
/// constructor takes the code's fields first, when it has any), and the
/// mapper (`$from$kv$KvError`) its throwing callables route faults through.
/// When a callback method may raise the domain, also its fields' writers
/// and the mapper (`$raise$kv$KvError`) that turns a raised error into the
/// fault the transport reports.
fn emit_error_domain(
    w: &mut CodeWriter,
    model: &Model,
    m: &ModuleBinding,
    eb: &ErrorBinding,
    raised: &BTreeSet<String>,
) {
    let domain = decl(&m.segments, &eb.type_name);
    let key = decl(&m.segments, &eb.name);
    w.line(format!(
        "const {domain} = class {} extends $Error {{}};",
        eb.type_name
    ));
    for c in &eb.codes {
        let class = code_class(&c.name);
        w.block(
            format!(
                "const {} = class {class} extends {domain} {{",
                decl(&m.segments, &class)
            ),
            "};",
            |w| {
                w.line(format!("static CODE = {};", c.value));
                let message = js_string(&c.message);
                if c.fields.is_empty() {
                    w.block(format!("constructor(message = {message}) {{"), "}", |w| {
                        w.line(format!("super({}, message);", c.value));
                    });
                } else {
                    w.block(
                        format!("constructor(fields, message = {message}) {{"),
                        "}",
                        |w| {
                            w.line(format!("super({}, message);", c.value));
                            for f in &c.fields {
                                w.line(format!("this.{0} = fields.{0};", f.name));
                            }
                        },
                    );
                }
            },
        );
    }
    let codes: Vec<String> = eb
        .codes
        .iter()
        .map(|c| format!("[{}, {}]", c.value, decl(&m.segments, &code_class(&c.name))))
        .collect();
    emit_map(w, &format!("$codes${key}"), &codes);
    let fielded = || eb.codes.iter().filter(|c| !c.fields.is_empty());
    let payloads: Vec<String> = fielded()
        .map(|c| {
            let fields: Vec<String> = c
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, read_expr(model, &f.ty)))
                .collect();
            format!("[{}, (r) => ({{ {} }})]", c.value, fields.join(", "))
        })
        .collect();
    emit_map(w, &format!("$payloads${key}"), &payloads);
    w.block(format!("function $from${key}(e) {{"), "}", |w| {
        w.line(format!("return $domain(e, $codes${key}, $payloads${key});"));
    });
    if raised.contains(&key) {
        let writers: Vec<String> = fielded()
            .map(|c| {
                let fields: Vec<String> = c
                    .fields
                    .iter()
                    .map(|f| format!("{};", write_expr(model, &f.ty, &format!("e.{}", f.name))))
                    .collect();
                format!("[{}, (w, e) => {{ {} }}]", c.value, fields.join(" "))
            })
            .collect();
        emit_map(w, &format!("$fields${key}"), &writers);
        w.block(format!("function $raise${key}(e) {{"), "}", |w| {
            w.line(format!(
                "return $raise(e, {domain}, $codes${key}, $fields${key});"
            ));
        });
    }
    w.blank();
}

/// Emit `const {name} = new Map([...]);`, one entry (`[key, value]`) per
/// line.
fn emit_map(w: &mut CodeWriter, name: &str, entries: &[String]) {
    if entries.is_empty() {
        w.line(format!("const {name} = new Map();"));
        return;
    }
    w.block(format!("const {name} = new Map(["), "]);", |w| {
        for e in entries {
            w.line(format!("{e},"));
        }
    });
}

/// Emit the adapter for one callback interface: it checks the consumer's
/// implementation and returns the raw object the transport calls, whose
/// methods (named as in the IDL) convert raw arguments into idiomatic values
/// (decoding buffers, adopting objects), call the implementation, and turn
/// the return value into its raw form (checking direct values, strings, and
/// bytes, encoding buffers, cloning objects). An exception reaches the
/// transport, which reports it to the producer as a code -4 failure, except
/// that a method declared `throws` first maps its module's domain errors
/// onto the `$Fault` that carries their code and fields.
fn emit_adapter(
    w: &mut CodeWriter,
    model: &Model,
    m: &ModuleBinding,
    cb: &CallbackInterfaceBinding,
) {
    let protocol = cb.protocol();
    let raise = model.error_domain(m).map(|eb| {
        let owner = error_owner(&model.modules, eb);
        format!("$raise${}", decl(&owner.segments, &eb.name))
    });
    w.block(
        format!("function {}(impl) {{", helper(model, "adapt", &cb.name)),
        "}",
        |w| {
            w.line(format!("$impl(impl, {});", js_string(&cb.name)));
            w.block("return {", "};", |w| {
                for ((method, passes), ret) in cb
                    .methods
                    .iter()
                    .zip(&protocol.method_args)
                    .zip(&protocol.method_returns)
                {
                    let raw: Vec<String> =
                        (0..method.params.len()).map(|i| format!("$a{i}")).collect();
                    let args: Vec<String> = method
                        .params
                        .iter()
                        .zip(passes)
                        .zip(&raw)
                        .map(|((p, pass), a)| receive(model, &p.ty, pass, a))
                        .collect();
                    let call = format!(
                        "impl.{}({})",
                        callback_method_name(&method.name),
                        args.join(", ")
                    );
                    let stmt = callback_return(model, cb, method, ret, &call);
                    w.block(
                        format!("{}({}) {{", method.name, raw.join(", ")),
                        "},",
                        |w| match raise.as_deref().filter(|_| method.throws) {
                            Some(raise) => {
                                w.line("try {");
                                w.scope(|w| {
                                    w.line(&stmt);
                                });
                                w.line("} catch ($e) {");
                                w.scope(|w| {
                                    w.line(format!("throw {raise}($e);"));
                                });
                                w.line("}");
                            }
                            None => {
                                w.line(&stmt);
                            }
                        },
                    );
                }
            });
        },
    );
    w.blank();
}

/// The statement an adapter method ends with: the implementation's `call`,
/// with its return value turned into the raw form the transport hands the
/// producer.
fn callback_return(
    model: &Model,
    cb: &CallbackInterfaceBinding,
    method: &CallbackMethodBinding,
    pass: &RetPass,
    call: &str,
) -> String {
    let where_ = || {
        js_string(&format!(
            "{}.{}",
            cb.name,
            callback_method_name(&method.name)
        ))
    };
    let Some(ty) = &method.ret else {
        return format!("{call};");
    };
    let value = match pass {
        RetPass::Void => unreachable!("a method with a return type returns a value"),
        RetPass::Direct => format!("$ret.{}({call}, {})", direct_prim(ty).pascal(), where_()),
        RetPass::String => format!("$ret.String({call}, {})", where_()),
        RetPass::Bytes => format!("$ret.Bytes({call}, {})", where_()),
        RetPass::Buffer => format!("$encode({call}, {})", writer_fn(model, ty)),
        RetPass::Object { nullable } => {
            let cls = type_decl(
                model,
                ty.interface_name().expect("object type names an interface"),
            );
            let clone = if *nullable { "$cloneOpt" } else { "$clone" };
            format!("{clone}({call}, {cls})")
        }
    };
    format!("return {value};")
}

/// The idiomatic value of a raw value `raw` of type `ty` received from the
/// native side per `pass`: decoded when buffered, adopted when an object,
/// unchanged otherwise.
fn receive(model: &Model, ty: &Ty, pass: &RetPass, raw: &str) -> String {
    match pass {
        RetPass::Buffer => format!("$decode({raw}, {})", reader_fn(model, ty)),
        RetPass::Object { nullable } => {
            let cls = type_decl(
                model,
                ty.interface_name().expect("object type names an interface"),
            );
            if *nullable {
                format!("$adoptOpt({cls}, {raw})")
            } else {
                format!("$adopt({cls}, {raw})")
            }
        }
        RetPass::Void | RetPass::Direct | RetPass::String | RetPass::Bytes => raw.to_string(),
    }
}

/// Emit the iterator spec of an iterator-returning callable (`key` names
/// the callable), the shared `next`/`destroy`/error mapping/element
/// conversion its `$Iterator`s use.
fn emit_iterator_spec(
    w: &mut CodeWriter,
    model: &Model,
    m: &ModuleBinding,
    f: &FnBinding,
    key: &str,
) {
    let CallShape::Iterator(it) = &f.shape else {
        return;
    };
    let protocol = it.protocol(f);
    let convert = match receive(model, &it.elem, &protocol.elem, "v") {
        v if v == "v" => "null".to_string(),
        other => format!("(v) => {other}"),
    };
    w.block(format!("const $it${key} = {{"), "};", |w| {
        w.line(format!("next: (h) => $raw.{}(h),", it.next.symbol));
        w.line(format!("destroy: (h) => $raw.{}(h),", it.destroy_symbol));
        w.line(format!(
            "map: {},",
            error_mapper(&model.modules, f, model.error_domain(m))
        ));
        w.line(format!("convert: {convert},"));
    });
}

/// Emit one interface's class. It extends the runtime's `$Object` (which
/// provides `close()` and `[Symbol.dispose]()`), exposes the canonical
/// synchronous `new` constructor as the JavaScript constructor and every
/// other constructor as a static factory, and gives the runtime its native
/// `$destroy` and `$clone`.
fn emit_class(w: &mut CodeWriter, model: &Model, m: &ModuleBinding, i: &InterfaceBinding) {
    let cls = decl(&m.segments, &i.name);
    let canonical = i
        .constructors
        .iter()
        .find(|c| c.name == "new" && !c.is_async());
    for f in i.constructors.iter().chain(&i.methods).chain(&i.statics) {
        emit_iterator_spec(w, model, m, f, &format!("{cls}${}", f.name));
    }
    w.block(
        format!("const {cls} = class {} extends $Object {{", i.name),
        "};",
        |w| {
            match canonical {
                Some(c) => {
                    w.block(
                        format!("constructor({}) {{", js_params(c).join(", ")),
                        "}",
                        |w| {
                            w.line("super();");
                            emit_body(w, model, m, c, None, &cls, Finish::Own);
                        },
                    );
                }
                None => {
                    w.block("constructor() {", "}", |w| {
                        w.line("super();");
                        w.line(format!(
                            "throw new TypeError({});",
                            js_string(&format!(
                                "{} has no public constructor; use its factory functions",
                                i.name
                            ))
                        ));
                    });
                }
            }
            w.block("static $destroy(h) {", "}", |w| {
                w.line(format!("$raw.{}(h);", i.destroy_symbol));
            });
            w.block("static $clone(h) {", "}", |w| {
                w.line(format!("return $raw.{}(h);", i.clone_symbol));
            });
            let members = i
                .constructors
                .iter()
                .filter(|c| canonical.is_none_or(|k| !std::ptr::eq(k, *c)))
                .map(|f| (f, true))
                .chain(i.methods.iter().map(|f| (f, false)))
                .chain(i.statics.iter().map(|f| (f, true)));
            for (f, is_static) in members {
                let mut params = js_params(f);
                if f.cancellable {
                    params.push("$options".into());
                }
                let head = format!(
                    "{}{}{}({}) {{",
                    if is_static { "static " } else { "" },
                    if f.is_async() { "async " } else { "" },
                    member_name(&f.name, is_static),
                    params.join(", ")
                );
                let recv = (!is_static).then_some(cls.as_str());
                let key = format!("{cls}${}", f.name);
                w.block(head, "}", |w| {
                    emit_body(w, model, m, f, recv, &key, Finish::Return);
                });
            }
        },
    );
    w.blank();
}

/// What a callable's body does with the converted result.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Finish {
    /// Return it (or nothing, for a void callable).
    Return,
    /// Bind the returned handle to `this` (the canonical constructor).
    Own,
}

/// One object lent to a call: the wrapper expression, its class, and
/// whether `null` is allowed.
struct Loan {
    value: String,
    local: String,
    cls: String,
    nullable: bool,
}

/// Emit the body of one callable. `recv` is the class of an instance
/// method's receiver; `key` names the callable's iterator spec.
fn emit_body(
    w: &mut CodeWriter,
    model: &Model,
    m: &ModuleBinding,
    f: &FnBinding,
    recv: Option<&str>,
    key: &str,
    finish: Finish,
) {
    let mut loans: Vec<Loan> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    if let Some(cls) = recv {
        loans.push(Loan {
            value: "this".into(),
            local: "$self".into(),
            cls: cls.to_string(),
            nullable: false,
        });
        args.push("$self".into());
    }
    for p in &f.params {
        args.push(arg_expr(model, p, &mut loans));
    }
    let symbol = match &f.shape {
        CallShape::Sync(abi) => &abi.symbol,
        CallShape::Async(a) => &a.launch.symbol,
        CallShape::Iterator(it) => &it.launch.symbol,
    };
    let call = if f.cancellable {
        args.push("$t".into());
        format!(
            "await $cancellable($options?.signal, $tokens, ($t) => $raw.{symbol}({}))",
            args.join(", ")
        )
    } else if f.is_async() {
        format!("await $raw.{symbol}({})", args.join(", "))
    } else {
        format!("$raw.{symbol}({})", args.join(", "))
    };
    let stmt = match (&f.shape, finish) {
        (_, Finish::Own) => format!("$own(this, {call});"),
        (CallShape::Iterator(_), _) => format!("return new $Iterator({call}, $it${key});"),
        _ => match RetPass::of(f.ret.as_ref()) {
            RetPass::Void => format!("{call};"),
            pass => format!(
                "return {};",
                receive(model, f.ret.as_ref().expect("non-void"), &pass, &call)
            ),
        },
    };
    let map = error_mapper(&model.modules, f, model.error_domain(m));
    emit_loans(w, &loans, &stmt, &map);
}

/// Emit the loans (each `$lend` paired with an `$unlend` in a `finally`)
/// around the call statement, whose faults are mapped by `map`.
fn emit_loans(w: &mut CodeWriter, loans: &[Loan], stmt: &str, map: &str) {
    let Some((first, rest)) = loans.split_first() else {
        w.line("try {");
        w.scope(|w| {
            w.line(stmt);
        });
        emit_catch(w, map);
        w.line("}");
        return;
    };
    let lend = if first.nullable { "$lendOpt" } else { "$lend" };
    w.line(format!(
        "const {} = {lend}({}, {});",
        first.local, first.value, first.cls
    ));
    w.line("try {");
    w.scope(|w| {
        if rest.is_empty() {
            w.line(stmt);
        } else {
            emit_loans(w, rest, stmt, map);
        }
    });
    if rest.is_empty() {
        emit_catch(w, map);
    }
    w.line("} finally {");
    w.scope(|w| {
        w.line(format!("$unlend({});", first.value));
    });
    w.line("}");
}

/// Emit the `catch` clause mapping a native fault through `map`.
fn emit_catch(w: &mut CodeWriter, map: &str) {
    w.line("} catch ($e) {");
    w.scope(|w| {
        w.line(format!("throw {map}($e);"));
    });
}

/// The raw argument expression of one parameter, recording any object it
/// lends.
fn arg_expr(model: &Model, p: &ParamBinding, loans: &mut Vec<Loan>) -> String {
    let name = param_name(&p.name);
    match p.arg_pass() {
        ArgPass::Direct { .. } | ArgPass::String { .. } | ArgPass::Bytes { .. } => name,
        ArgPass::Buffer { .. } => format!("$encode({name}, {})", writer_fn(model, &p.ty)),
        ArgPass::Object { nullable, .. } => {
            let cls = type_decl(
                model,
                p.ty.interface_name()
                    .expect("object parameter names an interface"),
            );
            let local = format!("$o{}", loans.len());
            loans.push(Loan {
                value: name,
                local: local.clone(),
                cls,
                nullable,
            });
            local
        }
        ArgPass::Callback { nullable, .. } => {
            let cb =
                p.ty.callback_interface_name()
                    .expect("callback parameter names a callback interface");
            let adapt = format!("{}({name})", helper(model, "adapt", cb));
            if nullable {
                format!("{name} == null ? null : {adapt}")
            } else {
                adapt
            }
        }
    }
}

/// Emit the frozen namespace object of `m` and, nested, its submodules.
/// `lead` precedes the opening line (`export const kv = ` for a root,
/// `stats: ` for a submodule).
fn emit_namespace(w: &mut CodeWriter, model: &Model, m: &ModuleBinding, lead: &str) {
    w.line(format!("{lead}Object.freeze({{"));
    w.scope(|w| {
        if let Some(eb) = m.errors.as_ref() {
            w.line(format!(
                "{}: {},",
                eb.type_name,
                decl(&m.segments, &eb.type_name)
            ));
            for c in &eb.codes {
                let class = code_class(&c.name);
                w.line(format!("{class}: {},", decl(&m.segments, &class)));
            }
        }
        for e in m.enums.iter().filter(|e| !e.rich) {
            w.line(format!("{}: {},", e.name, decl(&m.segments, &e.name)));
        }
        for i in &m.interfaces {
            w.line(format!("{}: {},", i.name, decl(&m.segments, &i.name)));
        }
        for f in &m.functions {
            let name = fn_name(&f.name);
            w.line(format!("{name}: {},", decl(&m.segments, &name)));
        }
        for child in model.children(m) {
            emit_namespace(w, model, child, &format!("{}: ", module_name(child)));
        }
    });
    w.line(if m.segments.len() == 1 { "});" } else { "})," });
}
