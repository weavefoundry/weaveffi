//! The idiomatic JavaScript API, shared by both transports.
//!
//! Everything here calls the native library through `$f`, the functions the
//! transport bound from the C signatures in `$sigs` (see
//! [`render_tables`]), and `$m`, the loaded transport itself (its vtables,
//! object tokens, and leak counters). A bound function takes one JavaScript
//! value per C parameter and returns one per C return (the transport fills
//! in the error, item, completion, and context slots and throws a reported
//! failure as a `$Fault`), so every value is marshalled here, by its
//! [`Crossing`]:
//!
//! | Crossing | Into the library | Out of the library |
//! |---|---|---|
//! | scalar, enum | range-checked number, `bigint`, or boolean | as is |
//! | optional scalar or enum | the checked value, or `null` | the value, or `null` |
//! | string | its UTF-8 bytes | decoded from UTF-8 |
//! | bytes | the `Uint8Array` | as is |
//! | typed array | the bytes of the typed array (a plain array is checked and copied into one first) | a plain array of the elements |
//! | value buffer | encoded with the type's writer | decoded with its reader |
//! | object | the wrapper's handle, lent for the call (a fresh `$clone` for a callback return) | adopted into a new wrapper |
//! | callback interface | `{ ctx, vtable }`: the implementation's adapter, registered last, and the interface's vtable | |
//!
//! The same table covers a return, an async result, an iterator item, and a
//! callback method's arguments (out of the library) and return (into it).

use std::collections::BTreeSet;

use weaveffi_model::abi::{AbiFn, AbiParam, CType};
use weaveffi_model::model::{
    CallbackBinding, CallbackMethodBinding, ErrorStrategy, FnBinding, InterfaceBinding, Model,
    Shape,
};
use weaveffi_model::ty::{ParamTy, RetTy, Ty};

use crate::codegen::errors::{self, ErrorTable};
use crate::codegen::values::{crossing, Crossing};
use crate::codegen::{contract, CodeWriter};
use crate::targets::js::codec::{emit_codecs, read_expr, reader_fn, write_expr, writer_fn};
use crate::targets::js::names::{
    callback_method_name, decl, error_mapper, fn_name, helper, js_string, member_name, param_name,
    typed_array, ErrorNames,
};

/// The slot kind of a C type in `$sigs` (see the transports for the
/// JavaScript value of each).
fn ctype_kind(ty: &CType) -> String {
    match ty {
        CType::Scalar(p) => p.snake().to_string(),
        CType::Enum(_) => "i32".to_string(),
        CType::Opt(p) => format!("opt_{}", p.snake()),
        CType::Run => "run".to_string(),
        CType::Callback(_) => "cb".to_string(),
        CType::Void => "void".to_string(),
        CType::Ptr { .. } => "ptr".to_string(),
        other => unreachable!("{other:?} never crosses on its own"),
    }
}

/// The slot kind of one callable slot: the error slot, an async launcher's
/// completion (with its result's kind) and context, or the value's own.
fn slot_kind(f: &FnBinding, slot: &AbiParam) -> String {
    match (slot.name.as_str(), &slot.ty) {
        ("out_err", CType::Ptr { .. }) => "err".to_string(),
        ("callback", CType::Named(_)) => {
            let result = f
                .async_binding()
                .and_then(|a| a.callback.params.iter().find(|p| p.name == "result"))
                .map_or(String::new(), |p| ctype_kind(&p.ty));
            format!("done:{result}")
        }
        ("context", CType::Ptr { .. }) if f.is_async() => "ctx".to_string(),
        _ => ctype_kind(&slot.ty),
    }
}

/// `[ret, [slot, ...]]` for `$sigs`.
fn sig(ret: &str, slots: &[String]) -> String {
    let slots: Vec<String> = slots.iter().map(|s| js_string(s)).collect();
    format!("[{}, [{}]]", js_string(ret), slots.join(", "))
}

/// The signature of a callable's C function.
fn callable_sig(f: &FnBinding) -> String {
    let slots: Vec<String> = f.abi.params.iter().map(|p| slot_kind(f, p)).collect();
    sig(&ctype_kind(&f.abi.ret), &slots)
}

/// The signature of an iterator's `next`: the handle, the item slot (with
/// the item's kind), and the error slot.
fn next_sig(next: &AbiFn) -> String {
    let slots: Vec<String> = next
        .params
        .iter()
        .map(|p| match (p.name.as_str(), &p.ty) {
            ("out_item", CType::Ptr { pointee, .. }) => format!("out:{}", ctype_kind(pointee)),
            ("out_err", _) => "err".to_string(),
            _ => ctype_kind(&p.ty),
        })
        .collect();
    sig(&ctype_kind(&next.ret), &slots)
}

/// The signature of a callback method's vtable entry: `ctx`, the
/// parameters, then the error slot.
fn method_sig(m: &CallbackMethodBinding) -> (String, Vec<String>) {
    let last = m.abi.params.len() - 1;
    let slots = m
        .abi
        .params
        .iter()
        .enumerate()
        .map(|(i, p)| match i {
            0 => "ctx".to_string(),
            i if i == last => "err".to_string(),
            _ => ctype_kind(&p.ty),
        })
        .collect();
    (ctype_kind(&m.abi.ret), slots)
}

/// Render the tables `index.js` starts with: `$sigs`, the C signature of
/// every function the API calls, and `$contract`, the declarations these
/// bindings were generated with.
pub(crate) fn render_tables(w: &mut CodeWriter, model: &Model) {
    let prefix = model.prefix();
    w.line("// The C signature of every function these bindings call, as");
    w.line("// [return, [slot, ...]] by symbol (the transport binds each).");
    w.block("const $sigs = {", "};", |w| {
        if model.has_cancellable() {
            w.line(format!(
                "{prefix}_cancel_token_create: {},",
                sig("ptr", &[])
            ));
            for op in ["cancel", "destroy"] {
                w.line(format!(
                    "{prefix}_cancel_token_{op}: {},",
                    sig("void", &["ptr".into()])
                ));
            }
        }
        for i in &model.interfaces {
            w.line(format!(
                "{}: {},",
                i.clone_symbol,
                sig("ptr", &["ptr".into()])
            ));
            w.line(format!(
                "{}: {},",
                i.destroy_symbol,
                sig("void", &["ptr".into()])
            ));
        }
        for f in model.callables() {
            w.line(format!("{}: {},", f.abi.symbol, callable_sig(f)));
            if let Some(it) = f.iterator() {
                w.line(format!("{}: {},", it.next.symbol, next_sig(&it.next)));
                w.line(format!(
                    "{}: {},",
                    it.destroy_symbol,
                    sig("void", &["ptr".into()])
                ));
            }
        }
    });
    w.blank();
    w.line("// The declarations these bindings were generated with, as");
    w.line("// [contract function, hash, path]: the library must export each");
    w.line("// function, returning the same hash (see $verify).");
    w.block("const $contract = [", "];", |w| {
        for row in contract::rows(model) {
            w.line(format!(
                "[{}, {}n, {}], // {}",
                js_string(&row.symbol),
                contract::hex(row.hash),
                js_string(&row.path),
                row.signature
            ));
        }
    });
}

/// Render the API section of `index.js`: everything after the transport has
/// defined `$m` and `$f`. Ends with the export list.
pub(crate) fn render_api(w: &mut CodeWriter, model: &Model, names: &ErrorNames) {
    let prefix = model.prefix();
    w.line("// Object tokens in value buffers, as transport handles.");
    w.line("const $token = (t) => $m.token(t);");
    w.line("const $untoken = (h) => $m.untoken(h);");
    if model.has_cancellable() {
        w.line("// The native cancel tokens behind `AbortSignal`s.");
        w.block("const $tokens = {", "};", |w| {
            w.line(format!("create: () => $f.{prefix}_cancel_token_create(),"));
            for op in ["cancel", "destroy"] {
                w.line(format!("{op}: (t) => $f.{prefix}_cancel_token_{op}(t),"));
            }
        });
    }
    w.line("// The leak counters behind the package's `./debug` export.");
    w.line("$setLive((kind) => $m.live(kind));");
    w.blank();

    let mut exports: Vec<String> = Vec::new();
    let raised = raised_domains(model);
    for table in errors::tables(model, "Error") {
        emit_error_domain(w, names, &table, raised.contains(&table.domain.name));
        exports.push(names.domain(&table.domain.name).to_string());
        exports.extend(
            table
                .codes
                .iter()
                .map(|r| names.code(&r.code.name).to_string()),
        );
    }
    for e in model.enums.iter().filter(|e| !e.is_rich()) {
        w.block(
            format!("const {} = Object.freeze({{", decl(&e.name)),
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
        exports.push(e.name.clone());
    }

    let before = w.as_str().len();
    emit_codecs(w, model);
    if w.as_str().len() != before {
        w.blank();
    }

    for cb in &model.callbacks {
        emit_adapter(w, cb);
    }
    for i in &model.interfaces {
        emit_class(w, i);
        exports.push(i.name.clone());
    }
    for f in &model.functions {
        let key = f.name.clone();
        emit_iterator_spec(w, f, &key);
        let name = fn_name(&f.name);
        w.block(
            format!(
                "const {} = {}function {name}({}) {{",
                decl(&name),
                if f.is_async() { "async " } else { "" },
                js_params(f).join(", ")
            ),
            "};",
            |w| emit_body(w, f, None, &key, Finish::Return),
        );
        w.blank();
        exports.push(name);
    }

    if !exports.is_empty() {
        w.block("export {", "};", |w| {
            for name in &exports {
                w.line(format!("{} as {name},", decl(name)));
            }
        });
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
        .callbacks
        .iter()
        .flat_map(|cb| &cb.methods)
        .filter_map(|m| m.error.domain().map(str::to_string))
        .collect()
}

/// Emit one error domain: the domain class, one class per code (whose
/// constructor takes the code's fields first, when it has any), and the
/// mapper (`$from$KvError`) its throwing callables route faults through.
/// When a callback method may raise the domain (`raised`), also its fields'
/// writers and the mapper (`$raise$KvError`) that turns a raised error into
/// the fault the transport reports.
fn emit_error_domain(w: &mut CodeWriter, names: &ErrorNames, table: &ErrorTable<'_>, raised: bool) {
    let eb = table.domain;
    let class = names.domain(&eb.name);
    let domain = decl(class);
    let key = &eb.name;
    w.line(format!(
        "const {domain} = class {class} extends $Error {{}};"
    ));
    for row in &table.codes {
        let c = row.code;
        let code_class = names.code(&c.name);
        w.block(
            format!(
                "const {} = class {code_class} extends {domain} {{",
                decl(code_class)
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
        .map(|row| format!("[{}, {}]", row.code.value, decl(names.code(&row.code.name))))
        .collect();
    emit_map(w, &format!("$codes${key}"), &codes);
    let fielded = || table.codes.iter().filter(|row| !row.code.fields.is_empty());
    let payloads: Vec<String> = fielded()
        .map(|row| {
            let fields: Vec<String> = row
                .code
                .fields
                .iter()
                .map(|f| format!("{}: {}", f.name, read_expr(&f.ty)))
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
                    .map(|f| format!("{};", write_expr(&f.ty, &format!("e.{}", f.name))))
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

/// The idiomatic value of `raw`, a value of type `ty` the library handed
/// over (a return, async result, iterator item, or callback argument).
fn lift(ty: &Ty, raw: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(_)
        | Crossing::Enum(_)
        | Crossing::OptScalar(_)
        | Crossing::OptEnum(_)
        | Crossing::Bytes => raw.to_string(),
        Crossing::Utf8 => format!("$text({raw})"),
        Crossing::Slice(p) => format!("$array({raw}, {})", typed_array(p)),
        Crossing::Buffer(ty) => format!("$decode({raw}, {})", reader_fn(ty)),
        Crossing::Object {
            interface,
            nullable,
        } => {
            let adopt = if nullable { "$adoptOpt" } else { "$adopt" };
            format!("{adopt}({}, {raw})", decl(interface))
        }
    }
}

/// `true` when [`lift`] passes a value of `ty` through unchanged.
fn lifts_plain(ty: &Ty) -> bool {
    matches!(
        crossing(ty),
        Crossing::Scalar(_)
            | Crossing::Enum(_)
            | Crossing::OptScalar(_)
            | Crossing::OptEnum(_)
            | Crossing::Bytes
    )
}

/// The checked C-level value of the idiomatic value `v` of type `ty` going
/// into the library, labeled `what` in error messages: an argument, or a
/// callback method's return. An object is cloned (a callback return moves
/// a reference); an argument lends it instead (see [`emit_body`]).
fn lower(ty: &Ty, v: &str, what: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(p) => format!("$check.{}({v}, {what})", p.pascal()),
        Crossing::Enum(_) => format!("$check.I32({v}, {what})"),
        Crossing::OptScalar(p) => format!("$opt({v}, $check.{}, {what})", p.pascal()),
        Crossing::OptEnum(_) => format!("$opt({v}, $check.I32, {what})"),
        Crossing::Utf8 => format!("$utf8({v}, {what})"),
        Crossing::Bytes => format!("$check.Bytes({v}, {what})"),
        Crossing::Slice(p) => format!("$bytesOf($slice.{}({v}, {what}))", p.pascal()),
        Crossing::Buffer(ty) => format!("$encode({v}, {})", writer_fn(ty)),
        Crossing::Object {
            interface,
            nullable,
        } => {
            let clone = if nullable { "$cloneOpt" } else { "$clone" };
            format!("{clone}({v}, {})", decl(interface))
        }
    }
}

/// Emit a callback interface's adapter, its vtable entries, and the
/// function that turns an implementation into its C argument.
///
/// The adapter (`$adapt$Listener(impl)`) checks the implementation and
/// returns an object with one method per callback method (named as in the
/// IDL), which turns the C-level arguments into idiomatic values (decoding
/// buffers, adopting objects, turning typed arrays into arrays), calls the
/// implementation, and turns its return into the C-level value (checking
/// direct values, strings, bytes, and typed arrays, encoding buffers,
/// cloning objects). How a failure reaches the library follows the
/// method's error strategy: a method that doesn't throw lets the exception
/// reach the transport, which reports code -4; a `throws: any` method
/// reports code -1 with the message; a method that throws a domain reports
/// that domain's errors with their code and fields, and anything else as
/// code -1.
///
/// The entries (`$vt$Listener`) are the vtable's methods as the transport
/// builds them: each method's C signature and the function that dispatches
/// to the adapter registered as `ctx`.
fn emit_adapter(w: &mut CodeWriter, cb: &CallbackBinding) {
    w.block(
        format!("function {}(impl) {{", helper("adapt", &cb.name)),
        "}",
        |w| {
            w.line(format!("$impl(impl, {});", js_string(&cb.name)));
            w.block("return {", "};", |w| {
                for method in &cb.methods {
                    emit_adapter_method(w, cb, method);
                }
            });
        },
    );
    w.block(format!("const {} = [", helper("vt", &cb.name)), "];", |w| {
        for method in &cb.methods {
            let (ret, slots) = method_sig(method);
            let args: Vec<String> = (0..method.params.len()).map(|i| format!("$a{i}")).collect();
            let mut params = vec!["ctx".to_string()];
            params.extend(args.iter().cloned());
            let slots: Vec<String> = slots.iter().map(|s| js_string(s)).collect();
            w.line(format!(
                "[{}, [{}], ({}) => $adapter(ctx).{}({})],",
                js_string(&ret),
                slots.join(", "),
                params.join(", "),
                method.name,
                args.join(", ")
            ));
        }
    });
    w.line(format!(
        "/** The C argument of a `{}` implementation: its adapter, registered, and the vtable. */",
        cb.name
    ));
    w.block(
        format!("function {}(impl) {{", helper("cb", &cb.name)),
        "}",
        |w| {
            w.line(format!(
                "const adapter = {}(impl);",
                helper("adapt", &cb.name)
            ));
            w.line(format!(
                "return {{ ctx: $register(adapter), vtable: $m.vtable({}, {}) }};",
                js_string(&cb.name),
                helper("vt", &cb.name)
            ));
        },
    );
    w.blank();
}

fn emit_adapter_method(w: &mut CodeWriter, cb: &CallbackBinding, method: &CallbackMethodBinding) {
    let raw: Vec<String> = (0..method.params.len()).map(|i| format!("$a{i}")).collect();
    let args: Vec<String> = method
        .params
        .iter()
        .zip(&raw)
        .map(|(p, a)| lift(&p.ty, a))
        .collect();
    let call = format!(
        "impl.{}({})",
        callback_method_name(&method.name),
        args.join(", ")
    );
    let stmt = match &method.ret {
        None => format!("{call};"),
        Some(ty) => {
            let what = js_string(&format!(
                "the return value of {}.{}",
                cb.name,
                callback_method_name(&method.name)
            ));
            format!("return {};", lower(ty, &call, &what))
        }
    };
    let map = match &method.error {
        ErrorStrategy::Trap => None,
        ErrorStrategy::Untyped => Some("$untyped".to_string()),
        ErrorStrategy::Domain(d) => Some(helper("raise", d)),
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

/// Emit the iterator spec of an iterator-returning callable (`key` names
/// the callable), the shared `next`/`destroy`/error mapping/element
/// conversion its `$Iterator`s use.
fn emit_iterator_spec(w: &mut CodeWriter, f: &FnBinding, key: &str) {
    let Some(it) = f.iterator() else {
        return;
    };
    let convert = if lifts_plain(&it.elem) {
        "null".to_string()
    } else {
        format!("(v) => {}", lift(&it.elem, "v"))
    };
    w.block(format!("const {} = {{", helper("it", key)), "};", |w| {
        w.line(format!("next: (h) => $f.{}(h),", it.next.symbol));
        w.line(format!("destroy: (h) => $f.{}(h),", it.destroy_symbol));
        w.line(format!("map: {},", error_mapper(&f.error)));
        w.line(format!("convert: {convert},"));
    });
}

/// Emit one interface's class. It extends the runtime's `$Object` (which
/// provides `close()` and `[Symbol.dispose]()`), exposes the canonical
/// synchronous `new` constructor as the JavaScript constructor and every
/// other constructor as a static factory, and gives the runtime its native
/// `$destroy` and `$clone`.
fn emit_class(w: &mut CodeWriter, i: &InterfaceBinding) {
    let cls = decl(&i.name);
    let canonical = canonical_constructor(i);
    for f in i.members() {
        emit_iterator_spec(w, f, &format!("{}${}", i.name, f.name));
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
                            emit_body(w, c, None, &i.name, Finish::Own);
                        },
                    );
                }
                None => {
                    w.block("constructor() {", "}", |w| {
                        w.line("super();");
                        w.line(format!("$noConstructor({});", js_string(&i.name)));
                    });
                }
            }
            w.block("static $destroy(h) {", "}", |w| {
                w.line(format!("$f.{}(h);", i.destroy_symbol));
            });
            w.block("static $clone(h) {", "}", |w| {
                w.line(format!("return $f.{}(h);", i.clone_symbol));
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
                let key = format!("{}${}", i.name, f.name);
                w.block(head, "}", |w| {
                    emit_body(w, f, recv, &key, Finish::Return);
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

/// One object lent to a call: the wrapper expression, the local holding its
/// handle, its class, and whether `null` is allowed.
struct Loan {
    value: String,
    local: String,
    cls: String,
    nullable: bool,
}

/// Emit the body of one callable. `recv` is the class of an instance
/// method's receiver; `key` names the callable's iterator spec.
///
/// Every argument is checked and converted into a local first, in
/// parameter order, then each callback implementation is registered, and
/// only then is the library called, so a bad argument never leaves an
/// implementation registered that the library won't release.
fn emit_body(w: &mut CodeWriter, f: &FnBinding, recv: Option<&str>, key: &str, finish: Finish) {
    let mut loans: Vec<Loan> = Vec::new();
    let mut args: Vec<String> = Vec::new();
    let mut prep: Vec<String> = Vec::new();
    let mut callbacks: Vec<String> = Vec::new();
    if let Some(cls) = recv {
        loans.push(Loan {
            value: "this".into(),
            local: "$self".into(),
            cls: cls.to_string(),
            nullable: false,
        });
        args.push("$self".into());
    }
    for (n, p) in f.params.iter().enumerate() {
        let name = param_name(&p.name);
        let local = format!("$a{n}");
        match &p.ty {
            ParamTy::Value(ty) => match crossing(ty) {
                Crossing::Object {
                    interface,
                    nullable,
                } => {
                    loans.push(Loan {
                        value: name,
                        local: local.clone(),
                        cls: decl(interface),
                        nullable,
                    });
                }
                _ => {
                    prep.push(format!(
                        "const {local} = {};",
                        lower(ty, &name, &js_string(&name))
                    ));
                }
            },
            ParamTy::Callback { name: cb, nullable } => {
                let make = format!("{}({name})", helper("cb", cb));
                callbacks.push(if *nullable {
                    format!("const {local} = {name} == null ? null : {make};")
                } else {
                    format!("const {local} = {make};")
                });
            }
        }
        args.push(local);
    }
    let symbol = &f.abi.symbol;
    let call = if f.cancellable() {
        args.push("$t".into());
        format!(
            "await $cancellable($options?.signal, $tokens, ($t) => $f.{symbol}({}))",
            args.join(", ")
        )
    } else if f.is_async() {
        format!("await $f.{symbol}({})", args.join(", "))
    } else {
        format!("$f.{symbol}({})", args.join(", "))
    };
    let value = f.ret.as_ref().and_then(RetTy::value);
    let last = match (finish, &f.shape, value) {
        (Finish::Own, _, _) => format!("$own(this, {call});"),
        (Finish::Return, Shape::Iterator(_), _) => {
            format!("return new $Iterator({call}, {});", helper("it", key))
        }
        (Finish::Return, _, None) => format!("{call};"),
        (Finish::Return, _, Some(ty)) => format!("return {};", lift(ty, &call)),
    };
    let mut stmts = prep;
    stmts.extend(callbacks);
    stmts.push(last);
    emit_loans(w, &loans, &stmts, &error_mapper(&f.error));
}

/// Emit the loans (each `$lend` paired with an `$unlend` in a `finally`)
/// around the call's statements, whose faults are mapped by `map`.
fn emit_loans(w: &mut CodeWriter, loans: &[Loan], stmts: &[String], map: &str) {
    let Some((first, rest)) = loans.split_first() else {
        w.line("try {");
        w.scope(|w| {
            for s in stmts {
                w.line(s);
            }
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
            for s in stmts {
                w.line(s);
            }
        } else {
            emit_loans(w, rest, stmts, map);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::js::test_api;

    fn api() -> String {
        let model = test_api("acme-kv");
        let mut w = CodeWriter::two_space();
        render_tables(&mut w, &model);
        render_api(&mut w, &model, &ErrorNames::new(&model));
        w.finish()
    }

    fn has(js: &str, needle: &str) {
        assert!(js.contains(needle), "missing `{needle}` in:\n{js}");
    }

    #[test]
    fn signatures_name_every_slot_kind() {
        let js = api();
        has(
            &js,
            "acme_kv_maybe: ['opt_f32', ['opt_u16', 'opt_i32', 'err']],",
        );
        has(&js, "acme_kv_totals: ['run', ['run', 'run', 'err']],");
        has(&js, "acme_kv_subscribe: ['void', ['cb', 'i32', 'err']],");
        has(
            &js,
            "acme_kv_Store_wait: ['void', ['ptr', 'i64', 'ptr', 'done:i64', 'ctx']],",
        );
        has(
            &js,
            "acme_kv_Store_fetch: ['void', ['ptr', 'run', 'done:ptr', 'ctx']],",
        );
        has(
            &js,
            "acme_kv_LevelsIterator_next: ['i32', ['ptr', 'out:opt_bool', 'err']],",
        );
        has(
            &js,
            "acme_kv_ChunksIterator_next: ['i32', ['ptr', 'out:run', 'err']],",
        );
        has(&js, "acme_kv_Store_clone: ['ptr', ['ptr']],");
        has(&js, "acme_kv_cancel_token_create: ['ptr', []],");
        has(&js, "['acme_kv_contract_");
    }

    #[test]
    fn declarations_are_exported_under_their_public_names() {
        let js = api();
        for needle in [
            "  delete_$ as delete_,\n",
            "  Store$ as Store,\n",
            "  KeyNotFoundError$ as KeyNotFoundError,\n",
            "  Mode$ as Mode,\n",
            "const delete_$ = function delete_(class_) {",
            "  close_() {",
            "  7: 'Safe',\n  '-1': 'Off',\n",
            "const count$ = function count(store) {",
        ] {
            has(&js, needle);
        }
    }

    #[test]
    fn values_cross_by_their_family() {
        let js = api();
        // Optional scalars in and out, typed arrays as bytes in and arrays out.
        has(&js, "const $a0 = $opt(x, $check.U16, 'x');");
        has(&js, "const $a1 = $opt(mode, $check.I32, 'mode');");
        has(&js, "const $a0 = $bytesOf($slice.I32(xs, 'xs'));");
        has(
            &js,
            "return $array($f.acme_kv_totals($a0, $a1), Float64Array);",
        );
        // A callback implementation is registered after every other argument.
        has(
            &js,
            "const $a1 = $check.I32(mode, 'mode');\n    const $a0 = $cb$Listener(listener);",
        );
        has(
            &js,
            "const $a0 = policy == null ? null : $cb$Policy(policy);",
        );
        // Callback returns are checked, encoded, and cloned.
        has(
            &js,
            "return $opt(impl.limit($a0), $check.I16, 'the return value of Policy.limit');",
        );
        has(&js, "return $cloneOpt(impl.maybe(), Store$);");
        has(&js, "return $bytesOf($slice.F32(impl.scores($array($a0, BigUint64Array)), 'the return value of Policy.scores'));");
        has(
            &js,
            "['opt_i16', ['ctx', 'opt_i32', 'err'], (ctx, $a0) => $adapter(ctx).limit($a0)],",
        );
        has(
            &js,
            "return { ctx: $register(adapter), vtable: $m.vtable('Listener', $vt$Listener) };",
        );
        // Async results, cancellation, and iterators.
        has(
            &js,
            "return $adoptOpt(Store$, await $f.acme_kv_Store_fetch($self, $a0));",
        );
        has(&js, "await $cancellable($options?.signal, $tokens, ($t) => $f.acme_kv_Store_wait($self, $a0, $t))");
        has(
            &js,
            "return new $Iterator($f.acme_kv_Store_scan($self), $it$Store$scan);",
        );
        has(&js, "convert: (v) => $array(v, Int16Array),");
    }
}
