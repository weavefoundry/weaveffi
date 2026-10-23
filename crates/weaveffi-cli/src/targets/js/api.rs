//! The idiomatic JavaScript API, shared by both transports.
//!
//! Everything here calls the native side through `$raw`, an object with one
//! function per C symbol (keyed by the symbol name) that the transport
//! provides: the N-API addon on Node.js, linear-memory glue on WebAssembly.
//! The raw calling convention, which both transports implement, is:
//!
//! * **Arguments**, in ABI order (an instance method's handle first), one
//!   per parameter: a direct value as a `number`, `bigint`, or `boolean`; an
//!   optional scalar (OptDirect) as its value, or `null` (or `undefined`)
//!   for none; a typed array (Slice) as the matching typed array
//!   (`Float64Array` for `[f64]`); a string as a `string`; bytes and value
//!   buffers as a `Uint8Array`; an object as its handle (`null` for an
//!   absent `Interface?`); a callback interface as an adapter object
//!   (`null` for an absent `Cb?`); and, last, a cancellable call's cancel
//!   token handle (or `null`). The transport range-checks direct and
//!   optional integers (`RangeError`) and type-checks everything.
//! * **Results** come back the same way: a direct value, an optional
//!   scalar's value or `null`, a typed array (a copy the caller owns), a
//!   `string`, a `Uint8Array`, or an object handle (one strong reference,
//!   or `null`). An async launcher returns a `Promise` of its result; an
//!   iterator launcher returns a handle, and the iterator's `next` returns
//!   an element or `undefined` when the iterator is exhausted. A module's
//!   contract function (`{prefix}_{module}_contract`) returns its table as
//!   a `BigUint64Array` of `id, hash` pairs.
//! * **Failures** throw (or reject with) a runtime `$Fault`.
//! * **Callback adapters** have one method per callback method, named as in
//!   the IDL, taking raw arguments (objects as adopted handles, optional
//!   scalars as a value or `null`, typed arrays as copies) and returning
//!   the raw value of the method's return: a direct value, an optional
//!   scalar's value or `null`, a typed array, a `string`, a `Uint8Array`
//!   (bytes or an encoded buffer, which the transport copies into a run
//!   from `{prefix}_alloc`), or an object handle that is a fresh strong
//!   reference (`null` for none). An adapter method that throws a `$Fault`
//!   reports its code, message, and payload; any other exception is
//!   reported as code -4 with its message.
//!
//! The transport also defines `$token`, which turns an object token read
//! from a value buffer into a handle.

use std::collections::BTreeSet;

use weaveffi_model::model::{
    CallbackInterfaceBinding, CallbackMethodBinding, FnBinding, InterfaceBinding, Model,
    ModuleBinding, ParamBinding,
};
use weaveffi_model::plan::{
    ArgPass, CallbackRetPass, ErrorStrategy, ItemPass, ResultPass, RetPass,
};
use weaveffi_model::ty::{RetTy, Ty};

use crate::codegen::errors::{self, ErrorTable};
use crate::codegen::CodeWriter;
use crate::targets::js::codec::{emit_codecs, read_expr, reader_fn, write_expr, writer_fn};
use crate::targets::js::names::{
    callback_method_name, decl, error_mapper, fn_name, helper, js_string, member_name, module_name,
    param_name, scalar_kind, type_decl,
};

/// Render the shared API section of `index.js`: everything after the
/// transport has defined `$raw` and `$token`.
pub(crate) fn render_api(w: &mut CodeWriter, model: &Model) {
    let prefix = model.prefix();

    if model.callables().any(|(_, f)| f.cancellable()) {
        w.line("// The native cancel tokens behind `AbortSignal`s.");
        w.block("const $tokens = {", "};", |w| {
            for op in ["create", "cancel", "destroy"] {
                let arg = if op == "create" { "" } else { "t" };
                w.line(format!(
                    "{op}: ({arg}) => $raw.{prefix}_cancel_token_{op}({arg}),"
                ));
            }
        });
        w.blank();
    }

    w.line("// The leak counters behind the package's `./debug` export.");
    w.line(format!(
        "$setLive((kind) => $raw.{prefix}_debug_live(kind));"
    ));
    w.blank();

    let raised = raised_domains(model);
    for table in errors::tables(model, "Error") {
        emit_error_domain(w, model, &table, raised.contains(&table.domain.name));
    }
    for m in &model.modules {
        for e in m.enums.iter().filter(|e| !e.is_rich()) {
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
            emit_adapter(w, model, cb);
        }
        for i in &m.interfaces {
            emit_class(w, model, m, i);
        }
        for f in &m.functions {
            let key = decl(&m.segments, &f.name);
            emit_iterator_spec(w, model, f, &key);
            let name = fn_name(&f.name);
            w.block(
                format!(
                    "const {} = {}function {name}({}) {{",
                    decl(&m.segments, &name),
                    if f.is_async() { "async " } else { "" },
                    js_params(f).join(", ")
                ),
                "};",
                |w| emit_body(w, model, f, None, &key, Finish::Return),
            );
            w.blank();
        }
    }

    for m in model.roots() {
        emit_namespace(
            w,
            model,
            m,
            &format!("export const {} = ", module_name(&m.name)),
        );
        w.blank();
    }
}

/// The JavaScript parameter names of a callable, with the trailing
/// `$options` of a cancellable one.
fn js_params(f: &FnBinding) -> Vec<String> {
    let mut out: Vec<String> = f.params.iter().map(|p| param_name(&p.name)).collect();
    if f.cancellable() {
        out.push("$options".into());
    }
    out
}

/// The error domains some callback method throws, which need a
/// `$raise$...` mapper.
fn raised_domains(model: &Model) -> BTreeSet<String> {
    model
        .callback_interfaces()
        .flat_map(|(_, cb)| &cb.methods)
        .filter_map(|m| m.error.domain().map(str::to_string))
        .collect()
}

/// Emit one error domain: the domain class, one class per code (whose
/// constructor takes the code's fields first, when it has any), and the
/// mapper (`$from$kv$KvError`) its throwing callables route faults through.
/// When a callback method may raise the domain (`raised`), also its fields'
/// writers and the mapper (`$raise$kv$KvError`) that turns a raised error
/// into the fault the transport reports.
fn emit_error_domain(w: &mut CodeWriter, model: &Model, table: &ErrorTable<'_>, raised: bool) {
    let segments = &table.module.segments;
    let eb = table.domain;
    let domain = decl(segments, &table.type_name);
    let key = type_decl(model, &eb.name);
    w.line(format!(
        "const {domain} = class {} extends $Error {{}};",
        table.type_name
    ));
    for row in &table.codes {
        let c = row.code;
        let class = &row.type_name;
        w.block(
            format!(
                "const {} = class {class} extends {domain} {{",
                decl(segments, class)
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
    let codes: Vec<String> = table
        .codes
        .iter()
        .map(|row| format!("[{}, {}]", row.code.value, decl(segments, &row.type_name)))
        .collect();
    emit_map(w, &format!("$codes${key}"), &codes);
    let fielded = || table.codes.iter().filter(|row| !row.code.fields.is_empty());
    let payloads: Vec<String> = fielded()
        .map(|row| {
            let fields: Vec<String> = row
                .code
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, read_expr(model, &f.ty)))
                .collect();
            format!("[{}, (r) => ({{ {} }})]", row.code.value, fields.join(", "))
        })
        .collect();
    emit_map(w, &format!("$payloads${key}"), &payloads);
    w.block(format!("function $from${key}(e) {{"), "}", |w| {
        w.line(format!(
            "return $domain(e, {domain}, $codes${key}, $payloads${key});"
        ));
    });
    if raised {
        let writers: Vec<String> = fielded()
            .map(|row| {
                let fields: Vec<String> = row
                    .code
                    .fields
                    .iter()
                    .map(|f| format!("{};", write_expr(model, &f.ty, &format!("e.{}", f.name))))
                    .collect();
                format!("[{}, (w, e) => {{ {} }}]", row.code.value, fields.join(" "))
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

/// How a raw value received from the native side becomes its idiomatic
/// value.
enum Lift<'a> {
    /// Unchanged: a direct value, an optional scalar, a string, or bytes.
    Plain,
    /// A typed array that surfaces as a plain array.
    List,
    /// A value buffer decoded as this type.
    Decode(&'a Ty),
    /// An object handle adopted into a new wrapper.
    Adopt { interface: &'a str, nullable: bool },
}

impl<'a> Lift<'a> {
    fn of_ret(pass: &'a RetPass, ty: Option<&'a Ty>) -> Self {
        match pass {
            RetPass::Void
            | RetPass::Direct
            | RetPass::OptDirect { .. }
            | RetPass::String { .. }
            | RetPass::Bytes { .. }
            | RetPass::Iterator(_) => Lift::Plain,
            RetPass::Slice { .. } => Lift::List,
            RetPass::Buffer { .. } => Lift::Decode(ty.expect("a buffered return has a type")),
            RetPass::Object {
                interface,
                nullable,
                ..
            } => Lift::Adopt {
                interface,
                nullable: *nullable,
            },
        }
    }

    fn of_result(pass: &'a ResultPass, ty: Option<&'a Ty>) -> Self {
        match pass {
            ResultPass::Void
            | ResultPass::Direct { .. }
            | ResultPass::OptDirect { .. }
            | ResultPass::String { .. }
            | ResultPass::Bytes { .. } => Lift::Plain,
            ResultPass::Slice { .. } => Lift::List,
            ResultPass::Buffer { .. } => Lift::Decode(ty.expect("a buffered result has a type")),
            ResultPass::Object {
                interface,
                nullable,
                ..
            } => Lift::Adopt {
                interface,
                nullable: *nullable,
            },
        }
    }

    fn of_item(pass: &'a ItemPass, ty: &'a Ty) -> Self {
        match pass {
            ItemPass::Direct { .. }
            | ItemPass::OptDirect { .. }
            | ItemPass::String { .. }
            | ItemPass::Bytes { .. } => Lift::Plain,
            ItemPass::Slice { .. } => Lift::List,
            ItemPass::Buffer { .. } => Lift::Decode(ty),
            ItemPass::Object {
                interface,
                nullable,
                ..
            } => Lift::Adopt {
                interface,
                nullable: *nullable,
            },
        }
    }

    /// A callback method's parameter, as the producer passes it.
    fn of_arg(pass: &'a ArgPass, ty: &'a Ty) -> Self {
        match pass {
            ArgPass::Direct { .. }
            | ArgPass::OptDirect { .. }
            | ArgPass::String { .. }
            | ArgPass::Bytes { .. } => Lift::Plain,
            ArgPass::Slice { .. } => Lift::List,
            ArgPass::Buffer { .. } => Lift::Decode(ty),
            ArgPass::Object {
                interface,
                nullable,
                ..
            } => Lift::Adopt {
                interface,
                nullable: *nullable,
            },
            ArgPass::Callback { .. } => unreachable!("callback methods take value types only"),
        }
    }

    /// The idiomatic value of `raw`.
    fn apply(&self, model: &Model, raw: &str) -> String {
        match self {
            Lift::Plain => raw.to_string(),
            Lift::List => format!("Array.from({raw})"),
            Lift::Decode(ty) => format!("$decode({raw}, {})", reader_fn(model, ty)),
            Lift::Adopt {
                interface,
                nullable,
            } => {
                let adopt = if *nullable { "$adoptOpt" } else { "$adopt" };
                format!("{adopt}({}, {raw})", type_decl(model, interface))
            }
        }
    }
}

/// Emit the adapter for one callback interface: it checks the consumer's
/// implementation and returns the raw object the transport calls, whose
/// methods (named as in the IDL) convert raw arguments into idiomatic values
/// (decoding buffers, adopting objects, turning typed arrays into arrays),
/// call the implementation, and turn the return value into its raw form
/// (checking and range-checking direct values, strings, bytes, and typed
/// arrays, encoding buffers, cloning objects). How a failure reaches the
/// producer follows the method's error strategy: a method that doesn't
/// throw lets the exception reach the transport, which reports code -4; a
/// `throws: any` method reports code -1 with the message; a method that
/// throws a domain reports that domain's errors with their code and
/// fields, and anything else as code -1.
fn emit_adapter(w: &mut CodeWriter, model: &Model, cb: &CallbackInterfaceBinding) {
    w.block(
        format!("function {}(impl) {{", helper(model, "adapt", &cb.name)),
        "}",
        |w| {
            w.line(format!("$impl(impl, {});", js_string(&cb.name)));
            w.block("return {", "};", |w| {
                for method in &cb.methods {
                    let raw: Vec<String> =
                        (0..method.params.len()).map(|i| format!("$a{i}")).collect();
                    let args: Vec<String> = method
                        .params
                        .iter()
                        .zip(&raw)
                        .map(|(p, a)| Lift::of_arg(&p.pass, &p.ty).apply(model, a))
                        .collect();
                    let call = format!(
                        "impl.{}({})",
                        callback_method_name(&method.name),
                        args.join(", ")
                    );
                    let stmt = callback_return(model, cb, method, &call);
                    let map = match &method.error {
                        ErrorStrategy::Trap => None,
                        ErrorStrategy::Untyped => Some("$untyped".to_string()),
                        ErrorStrategy::Domain(d) => Some(helper(model, "raise", d)),
                    };
                    w.block(
                        format!("{}({}) {{", method.name, raw.join(", ")),
                        "},",
                        |w| match &map {
                            Some(map) => {
                                w.line("try {");
                                w.scope(|w| {
                                    w.line(&stmt);
                                });
                                w.line("} catch ($e) {");
                                w.scope(|w| {
                                    w.line(format!("throw {map}($e);"));
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
    call: &str,
) -> String {
    let what = || {
        js_string(&format!(
            "the return value of {}.{}",
            cb.name,
            callback_method_name(&method.name)
        ))
    };
    let ty = || {
        method
            .ret
            .as_ref()
            .expect("a method with a return returns a type")
    };
    let value = match &method.ret_pass {
        CallbackRetPass::Void => return format!("{call};"),
        CallbackRetPass::Direct => {
            format!("$check.{}({call}, {})", scalar_kind(ty()), what())
        }
        CallbackRetPass::OptDirect { .. } => {
            let Ty::Optional(inner) = ty() else {
                unreachable!("an OptDirect return is optional")
            };
            format!("$opt({call}, $check.{}, {})", scalar_kind(inner), what())
        }
        CallbackRetPass::Slice { elem, .. } => {
            format!("$slice.{}({call}, {})", elem.pascal(), what())
        }
        CallbackRetPass::String { .. } => format!("$check.String({call}, {})", what()),
        CallbackRetPass::Bytes { .. } => format!("$check.Bytes({call}, {})", what()),
        CallbackRetPass::Buffer { .. } => format!("$encode({call}, {})", writer_fn(model, ty())),
        CallbackRetPass::Object {
            nullable,
            interface,
            ..
        } => {
            let clone = if *nullable { "$cloneOpt" } else { "$clone" };
            format!("{clone}({call}, {})", type_decl(model, interface))
        }
    };
    format!("return {value};")
}

/// Emit the iterator spec of an iterator-returning callable (`key` names
/// the callable), the shared `next`/`destroy`/error mapping/element
/// conversion its `$Iterator`s use.
fn emit_iterator_spec(w: &mut CodeWriter, model: &Model, f: &FnBinding, key: &str) {
    let Some(it) = f.iterator() else {
        return;
    };
    let convert = match Lift::of_item(&it.item, &it.elem) {
        Lift::Plain => "null".to_string(),
        lift => format!("(v) => {}", lift.apply(model, "v")),
    };
    w.block(format!("const $it${key} = {{"), "};", |w| {
        w.line(format!("next: (h) => $raw.{}(h),", it.next.symbol));
        w.line(format!("destroy: (h) => $raw.{}(h),", it.destroy_symbol));
        w.line(format!("map: {},", error_mapper(model, &f.error)));
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
    let canonical = canonical_constructor(i);
    for f in i.members() {
        emit_iterator_spec(w, model, f, &format!("{cls}${}", f.name));
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
                            emit_body(w, model, c, None, &cls, Finish::Own);
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
                let head = format!(
                    "{}{}{}({}) {{",
                    if is_static { "static " } else { "" },
                    if f.is_async() { "async " } else { "" },
                    member_name(&f.name, is_static),
                    js_params(f).join(", ")
                );
                let recv = (!is_static).then_some(cls.as_str());
                let key = format!("{cls}${}", f.name);
                w.block(head, "}", |w| {
                    emit_body(w, model, f, recv, &key, Finish::Return);
                });
            }
        },
    );
    w.blank();
}

/// The constructor that becomes an interface's JavaScript constructor: the
/// synchronous one named `new`.
pub(crate) fn canonical_constructor(i: &InterfaceBinding) -> Option<&FnBinding> {
    i.constructors
        .iter()
        .find(|c| c.name == "new" && !c.is_async())
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
    let symbol = &f.abi.symbol;
    let call = if f.cancellable() {
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
    let ty = f.ret.as_ref().map(RetTy::elem);
    let stmt = match (finish, f.async_binding()) {
        (Finish::Own, _) => format!("$own(this, {call});"),
        (Finish::Return, Some(a)) => match &a.result {
            ResultPass::Void => format!("{call};"),
            pass => format!("return {};", Lift::of_result(pass, ty).apply(model, &call)),
        },
        (Finish::Return, None) => match &f.ret_pass {
            RetPass::Void => format!("{call};"),
            RetPass::Iterator(_) => format!("return new $Iterator({call}, $it${key});"),
            pass => format!("return {};", Lift::of_ret(pass, ty).apply(model, &call)),
        },
    };
    emit_loans(w, &loans, &stmt, &error_mapper(model, &f.error));
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
    match &p.pass {
        ArgPass::Direct { .. }
        | ArgPass::OptDirect { .. }
        | ArgPass::String { .. }
        | ArgPass::Bytes { .. } => name,
        ArgPass::Slice { elem, .. } => {
            format!("$slice.{}({name}, {})", elem.pascal(), js_string(&name))
        }
        ArgPass::Buffer { .. } => {
            let ty = p.ty.value().expect("a buffered parameter is a value");
            format!("$encode({name}, {})", writer_fn(model, ty))
        }
        ArgPass::Object {
            nullable,
            interface,
            ..
        } => {
            let local = format!("$o{}", loans.len());
            loans.push(Loan {
                value: name,
                local: local.clone(),
                cls: type_decl(model, interface),
                nullable: *nullable,
            });
            local
        }
        ArgPass::Callback {
            nullable,
            interface,
            ..
        } => {
            let adapt = format!("{}({name})", helper(model, "adapt", interface));
            if *nullable {
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
        for table in errors::tables(model, "Error")
            .iter()
            .filter(|t| t.module.index == m.index)
        {
            w.line(format!(
                "{}: {},",
                table.type_name,
                decl(&m.segments, &table.type_name)
            ));
            for row in &table.codes {
                w.line(format!(
                    "{}: {},",
                    row.type_name,
                    decl(&m.segments, &row.type_name)
                ));
            }
        }
        for e in m.enums.iter().filter(|e| !e.is_rich()) {
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
            emit_namespace(w, model, child, &format!("{}: ", module_name(&child.name)));
        }
    });
    w.line(if m.parent.is_none() { "});" } else { "})," });
}
