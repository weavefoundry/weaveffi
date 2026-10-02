//! The linear-memory glue: one raw entry point per C symbol, built on the
//! fixed `$Linear` transport (`runtime/linear.js`).
//!
//! Each entry point follows the raw calling convention of the shared
//! JavaScript layer ([`crate::targets::js`]): it checks and converts direct
//! arguments, stages strings and byte runs in linear memory for the call,
//! passes the reused error and `out_len` slots, throws the reported error,
//! and turns results back into JavaScript values (copying and freeing
//! returned strings and buffers). Callback-interface vtables and async
//! completions are JavaScript functions installed in the module's function
//! table.

use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    BindingModel, CallShape, CallbackInterfaceBinding, FnBinding, Prim, Ty,
};
use weaveffi_model::plan::{self, ArgPass, RetPass};

use crate::codegen::CodeWriter;
use crate::targets::js::names::direct_prim;

/// The wasm value type of one C slot.
fn valtype(ty: &CType) -> &'static str {
    match ty {
        CType::Int64 | CType::Uint64 => "i64",
        CType::Float => "f32",
        CType::Double => "f64",
        _ => "i32",
    }
}

/// The Emscripten signature character of a wasm value type.
fn sig_char(valtype: &str) -> char {
    match valtype {
        "i64" => 'j',
        "f32" => 'f',
        "f64" => 'd',
        _ => 'i',
    }
}

/// The `[params], [results], 'sig'` triple of a table function.
fn signature(params: &[AbiParam], ret: &CType) -> String {
    let ps: Vec<&str> = params.iter().map(|p| valtype(&p.ty)).collect();
    let (results, ret_char) = match ret {
        CType::Void => (String::new(), 'v'),
        t => (format!("'{}'", valtype(t)), sig_char(valtype(t))),
    };
    let sig: String = std::iter::once(ret_char)
        .chain(ps.iter().map(|t| sig_char(t)))
        .collect();
    let quoted: Vec<String> = ps.iter().map(|t| format!("'{t}'")).collect();
    format!("[{}], [{results}], '{sig}'", quoted.join(", "))
}

/// The conversion of a direct JavaScript argument `v` for a wasm call.
fn direct_arg(p: Prim, v: &str) -> String {
    match p {
        Prim::Bool => format!("m.bool({v})"),
        Prim::I64 => format!("m.i64({v})"),
        Prim::U64 => format!("m.u64({v})"),
        _ => format!("m.num({v})"),
    }
}

/// The JavaScript value of a direct wasm value `r`: unsigned integers and
/// bools are reinterpreted, everything else is already right.
fn direct_value(p: Prim, r: &str) -> String {
    match p {
        Prim::Bool => format!("{r} !== 0"),
        Prim::U32 => format!("{r} >>> 0"),
        Prim::U64 => format!("BigInt.asUintN(64, {r})"),
        _ => r.to_string(),
    }
}

/// The JavaScript value of a received value whose slots are `slots` (one,
/// or a pointer and a length). Owned values are freed after copying;
/// borrowed ones (callback arguments) are only copied.
fn receive(ty: &Ty, pass: &RetPass, slots: &[&str], owned: bool) -> String {
    match pass {
        RetPass::Void => "undefined".into(),
        RetPass::Direct => direct_value(direct_prim(ty), slots[0]),
        RetPass::String => format!(
            "m.{}({}, {})",
            if owned { "takeStr" } else { "readStr" },
            slots[0],
            slots[1]
        ),
        RetPass::Bytes | RetPass::Buffer => format!(
            "m.{}({}, {})",
            if owned { "takeData" } else { "readData" },
            slots[0],
            slots[1]
        ),
        RetPass::Object { nullable: true, .. } => format!("{0} === 0 ? null : {0}", slots[0]),
        RetPass::Object { .. } => slots[0].to_string(),
    }
}

/// The `DataView` read of one iterator element of direct type `p` from the
/// `out_item` slot.
fn item_read(p: Prim) -> String {
    let (get, le) = match p {
        Prim::Bool => return "m.view().getUint8(m.item) !== 0".into(),
        Prim::I8 => ("getInt8", false),
        Prim::U8 => ("getUint8", false),
        Prim::I16 => ("getInt16", true),
        Prim::U16 => ("getUint16", true),
        Prim::I32 => ("getInt32", true),
        Prim::U32 => ("getUint32", true),
        Prim::I64 => ("getBigInt64", true),
        Prim::U64 => ("getBigUint64", true),
        Prim::F32 => ("getFloat32", true),
        Prim::F64 => ("getFloat64", true),
        Prim::String | Prim::Bytes => unreachable!("not direct"),
    };
    if le {
        format!("m.view().{get}(m.item, true)")
    } else {
        format!("m.view().{get}(m.item)")
    }
}

/// The marshalled arguments of one entry point.
#[derive(Default)]
struct Args {
    /// The JavaScript parameter names.
    params: Vec<String>,
    /// Staging statements (inside the `try`).
    stage: Vec<String>,
    /// Staged locals released in the `finally`.
    staged: Vec<String>,
    /// The wasm argument expressions, in slot order.
    call: Vec<String>,
}

fn marshal(model: &BindingModel, f: &FnBinding) -> Args {
    let mut a = Args::default();
    if f.has_self {
        a.params.push("self".into());
        a.call.push("m.handle(self)".into());
    }
    for (n, p) in f.params.iter().enumerate() {
        let v = format!("a{n}");
        a.params.push(v.clone());
        match p.arg_pass() {
            ArgPass::Direct { .. } => a.call.push(direct_arg(direct_prim(&p.ty), &v)),
            ArgPass::String { .. } | ArgPass::Bytes { .. } | ArgPass::Buffer { .. } => {
                let s = format!("s{n}");
                let stage = if matches!(p.ty, Ty::StringUtf8) {
                    "str"
                } else {
                    "data"
                };
                a.stage.push(format!("{s} = m.{stage}({v});"));
                a.staged.push(s.clone());
                a.call.push(format!("{s}[0]"));
                a.call.push(format!("{s}[1]"));
            }
            ArgPass::Object { nullable, .. } => a.call.push(format!(
                "m.{}({v})",
                if nullable { "handleOpt" } else { "handle" }
            )),
            ArgPass::Callback { .. } => {
                let cb = find_callback(
                    model,
                    p.ty.callback_interface_name().expect("callback parameter"),
                );
                a.call.push(format!("m.register({v})"));
                a.call.push(format!(
                    "m.vtable('{}', () => $vt_{}(m))",
                    cb.c_tag, cb.c_tag
                ));
            }
        }
    }
    if f.cancellable {
        a.params.push("token".into());
        a.call.push("m.handleOpt(token)".into());
    }
    a
}

/// The callback interface named by the absolute dotted path `dotted`.
fn find_callback<'a>(model: &'a BindingModel, dotted: &str) -> &'a CallbackInterfaceBinding {
    let (module, name) = dotted.rsplit_once('.').expect("absolute type path");
    model
        .modules
        .iter()
        .find(|m| m.dot_path == module)
        .and_then(|m| m.callback_interface(name))
        .expect("callback interface exists")
}

/// Emit `body` (the lines of an entry point) wrapped in the staging
/// `try`/`finally` when anything is staged.
fn emit_staged(w: &mut CodeWriter, a: &Args, body: &[String]) {
    if a.staged.is_empty() {
        for l in body {
            w.line(l);
        }
        return;
    }
    let locals: Vec<String> = a.staged.iter().map(|s| format!("{s} = null")).collect();
    w.line(format!("let {};", locals.join(", ")));
    w.line("try {");
    w.scope(|w| {
        for s in &a.stage {
            w.line(s);
        }
        for l in body {
            w.line(l);
        }
    });
    w.line("} finally {");
    w.scope(|w| {
        for s in &a.staged {
            w.line(format!("m.unstage({s});"));
        }
    });
    w.line("}");
}

/// Render `$bind(m)`, which builds the raw entry points over a loaded
/// `$Linear`, plus the callback-interface vtable builders, and return it
/// with every C symbol the glue calls (Emscripten mode binds them by name).
pub(crate) fn render_bind(model: &BindingModel) -> (String, Vec<String>) {
    let prefix = &model.prefix;
    let mut symbols: Vec<String> = [
        "abi_version",
        "alloc",
        "dealloc",
        "free_bytes",
        "error_set",
        "error_clear",
        "error_free",
        "debug_live",
        "cancel_token_create",
        "cancel_token_cancel",
        "cancel_token_destroy",
    ]
    .iter()
    .map(|s| format!("{prefix}_{s}"))
    .collect();
    let mut w = CodeWriter::two_space();

    for (_, cb) in model.callback_interfaces() {
        emit_vtable(&mut w, cb, prefix);
    }

    w.line("// The raw entry points, one per C symbol.");
    w.block("function $bind(m) {", "}", |w| {
        w.line("const x = m.x;");
        w.block("return {", "};", |w| {
            w.line(format!(
                "{prefix}_abi_version: () => x.{prefix}_abi_version() >>> 0,"
            ));
            w.line(format!(
                "{prefix}_debug_live: (kind) => BigInt.asUintN(64, x.{prefix}_debug_live(m.num(kind))),"
            ));
            w.line(format!(
                "{prefix}_cancel_token_create: () => x.{prefix}_cancel_token_create(),"
            ));
            for op in ["cancel", "destroy"] {
                w.line(format!(
                    "{prefix}_cancel_token_{op}: (t) => x.{prefix}_cancel_token_{op}(m.handle(t)),"
                ));
            }
            for root in model.roots() {
                let sym = weaveffi_model::model::checksum_symbol(prefix, &root.name);
                w.line(format!("{sym}: () => BigInt.asUintN(64, x.{sym}()),"));
                symbols.push(sym);
            }
            for m in &model.modules {
                for i in &m.interfaces {
                    for sym in [&i.clone_symbol, &i.destroy_symbol] {
                        w.line(format!("{sym}: (h) => x.{sym}(m.handle(h)),"));
                        symbols.push(sym.clone());
                    }
                }
                for f in m.callables() {
                    emit_callable(w, model, f, &mut symbols);
                }
            }
        });
    });
    (w.finish(), symbols)
}

fn emit_callable(
    w: &mut CodeWriter,
    model: &BindingModel,
    f: &FnBinding,
    symbols: &mut Vec<String>,
) {
    let prefix = &model.prefix;
    let a = marshal(model, f);
    let head = |sym: &str| format!("{sym}: ({}) => {{", a.params.join(", "));
    match &f.shape {
        CallShape::Sync(abi) => {
            let pass = plan::ret_pass(f.ret.as_ref(), prefix);
            let pair = matches!(pass, RetPass::String | RetPass::Bytes | RetPass::Buffer);
            let mut call = a.call.clone();
            if pair {
                call.push("m.len".into());
            }
            call.push("m.err".into());
            let call = format!("x.{}({})", abi.symbol, call.join(", "));
            let mut body = Vec::new();
            match &pass {
                RetPass::Void => {
                    body.push(format!("{call};"));
                    body.push("m.check();".into());
                }
                pass => {
                    body.push(format!("const r = {call};"));
                    body.push("m.check();".into());
                    let ty = f.ret.as_ref().expect("non-void");
                    body.push(format!(
                        "return {};",
                        receive(ty, pass, &["r", "m.outLen()"], true)
                    ));
                }
            }
            w.block(head(&abi.symbol), "},", |w| emit_staged(w, &a, &body));
            symbols.push(abi.symbol.clone());
        }
        CallShape::Iterator(it) => {
            let mut call = a.call.clone();
            call.push("m.err".into());
            let body = vec![
                format!("const r = x.{}({});", it.launch.symbol, call.join(", ")),
                "m.check();".into(),
                "return r;".into(),
            ];
            w.block(head(&it.launch.symbol), "},", |w| emit_staged(w, &a, &body));
            symbols.push(it.launch.symbol.clone());

            let protocol = it.protocol(f, prefix);
            let pair = matches!(
                protocol.elem,
                RetPass::String | RetPass::Bytes | RetPass::Buffer
            );
            let len = if pair { "m.len, " } else { "" };
            w.block(format!("{}: (h) => {{", it.next.symbol), "},", |w| {
                w.line(format!(
                    "const has = x.{}(m.handle(h), m.item, {len}m.err);",
                    it.next.symbol
                ));
                w.line("m.check();");
                w.line("if (has === 0) return undefined;");
                match &protocol.elem {
                    RetPass::Direct => {
                        w.line(format!("return {};", item_read(direct_prim(&it.elem))));
                    }
                    pass => {
                        w.line("const p = m.view().getUint32(m.item, true);");
                        w.line(format!(
                            "return {};",
                            receive(&it.elem, pass, &["p", "m.outLen()"], true)
                        ));
                    }
                }
            });
            w.line(format!(
                "{0}: (h) => x.{0}(m.handle(h)),",
                it.destroy_symbol
            ));
            symbols.push(it.next.symbol.clone());
            symbols.push(it.destroy_symbol.clone());
        }
        CallShape::Async(ab) => {
            let pass = plan::ret_pass(f.ret.as_ref(), prefix);
            let names: Vec<String> = ab.callback_params[2..]
                .iter()
                .map(|p| p.name.clone())
                .collect();
            let slots: Vec<&str> = names.iter().map(String::as_str).collect();
            let convert = match &f.ret {
                None => "() => undefined".to_string(),
                Some(ty) => format!(
                    "({}) => {}",
                    names.join(", "),
                    receive(ty, &pass, &slots, true)
                ),
            };
            let mut call = a.call.clone();
            call.push("cb".into());
            call.push("ctx".into());
            let body = vec![format!(
                "return m.launch({}, (cb, ctx) => x.{}({}), {convert});",
                signature(&ab.callback_params, &CType::Void),
                ab.launch.symbol,
                call.join(", ")
            )];
            w.block(head(&ab.launch.symbol), "},", |w| emit_staged(w, &a, &body));
            symbols.push(ab.launch.symbol.clone());
        }
    }
}

/// Emit `$vt_{tag}(m)`, the table functions of one callback interface's
/// vtable: each converts the raw slots into the values the adapter takes,
/// calls it, and converts the result back, reporting an exception through
/// `out_err` (code -4) instead of letting it unwind into the module.
fn emit_vtable(w: &mut CodeWriter, cb: &CallbackInterfaceBinding, prefix: &str) {
    let protocol = cb.protocol(prefix);
    w.block(format!("function $vt_{}(m) {{", cb.c_tag), "}", |w| {
        w.block("return [", "];", |w| {
            for (method, passes) in cb.methods.iter().zip(&protocol.method_args) {
                let slots = &method.abi_params[1..method.abi_params.len() - 1];
                let names: Vec<String> = (0..slots.len()).map(|i| format!("p{i}")).collect();
                let mut k = 0usize;
                let args: Vec<String> = method
                    .params
                    .iter()
                    .zip(passes)
                    .map(|(p, pass)| {
                        let s: Vec<&str> = names[k..k + p.abi.len()]
                            .iter()
                            .map(String::as_str)
                            .collect();
                        k += p.abi.len();
                        receive(&p.ty, pass, &s, false)
                    })
                    .collect();
                let call = format!("m.adapter(ctx).{}({})", method.name, args.join(", "));
                let (ret, fallback) = match &method.ret {
                    None => (format!("{call};"), None),
                    Some(ty) => match direct_prim(ty) {
                        Prim::Bool => (format!("return {call} ? 1 : 0;"), Some("return 0;")),
                        Prim::I64 | Prim::U64 => (format!("return {call};"), Some("return 0n;")),
                        _ => (format!("return {call};"), Some("return 0;")),
                    },
                };
                let mut params = vec!["ctx".to_string()];
                params.extend(names.iter().cloned());
                params.push("err".into());
                w.line(format!(
                    "[{}, ({}) => {{",
                    signature(&method.abi_params, &method.abi_ret),
                    params.join(", ")
                ));
                w.scope(|w| {
                    w.line("try {");
                    w.scope(|w| {
                        w.line(&ret);
                    });
                    w.line("} catch (e) {");
                    w.scope(|w| {
                        w.line("m.foreign(err, e);");
                        if let Some(f) = fallback {
                            w.line(f);
                        }
                    });
                    w.line("}");
                });
                w.line("}],");
            }
        });
    });
    w.blank();
}
