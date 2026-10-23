//! The linear-memory glue: one raw entry point per C symbol, built on the
//! fixed `$Linear` transport (`runtime/linear.js`).
//!
//! Each entry point follows the raw calling convention of the shared
//! JavaScript layer ([`crate::targets::js`]): it checks and converts direct
//! arguments (range-checking integers), splits optional scalars into a flag
//! and a value, stages strings, byte runs, and typed arrays in linear memory
//! for the call, passes the reused error and out slots, throws the reported
//! error, and turns results back into JavaScript values (copying and freeing
//! returned strings, buffers, and typed arrays). Callback-interface vtables
//! and async completions are JavaScript functions installed in the module's
//! function table; a callback's string, bytes, buffer, or typed-array
//! return is copied into a run from `{prefix}_alloc` that the producer
//! adopts.
//!
//! Every wasm call is built from the model's lowered signature ([`AbiFn`]):
//! each slot's argument is looked up by the slot's name, so the argument
//! order is the model's by construction.

use std::collections::HashMap;

use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    contract_symbol, AbiFn, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding,
    IteratorBinding, Model,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ItemPass, ResultPass, RetPass};

use crate::codegen::CodeWriter;
use crate::targets::js::names::typed_array;

/// The wasm value type of one C slot (on wasm32, pointers, `size_t`,
/// `bool`, and every integer of 32 bits or fewer are `i32`).
fn valtype(ty: &CType) -> &'static str {
    match ty {
        CType::Int64 | CType::Uint64 => "i64",
        CType::Float => "f32",
        CType::Double => "f64",
        _ => "i32",
    }
}

/// The `[params], [results]` pair of a table function.
fn signature(params: &[AbiParam], ret: &CType) -> String {
    let ps: Vec<String> = params
        .iter()
        .map(|p| format!("'{}'", valtype(&p.ty)))
        .collect();
    let results = match ret {
        CType::Void => String::new(),
        t => format!("'{}'", valtype(t)),
    };
    format!("[{}], [{results}]", ps.join(", "))
}

/// The checked wasm argument of the scalar JavaScript value `v` for a slot
/// of type `ty`.
fn scalar_arg(ty: &CType, v: &str) -> String {
    match ty {
        CType::Bool => format!("m.bool({v})"),
        CType::Int8 => format!("m.i8({v})"),
        CType::Int16 => format!("m.i16({v})"),
        CType::Int32 | CType::Enum { .. } => format!("m.i32({v})"),
        CType::Uint8 => format!("m.u8({v})"),
        CType::Uint16 => format!("m.u16({v})"),
        CType::Uint32 => format!("m.u32({v})"),
        CType::Int64 => format!("m.i64({v})"),
        CType::Uint64 => format!("m.u64({v})"),
        CType::Float | CType::Double => format!("m.num({v})"),
        other => unreachable!("{other:?} is not a scalar slot"),
    }
}

/// The zero of a scalar slot's wasm value (`0n` for 64-bit integers).
fn scalar_zero(ty: &CType) -> &'static str {
    match ty {
        CType::Int64 | CType::Uint64 => "0n",
        _ => "0",
    }
}

/// The JavaScript value of the wasm value `r` of a scalar slot of type
/// `ty`: narrow integers are normalized from their `i32` carrier, unsigned
/// ones reinterpreted, and bools compared.
fn scalar_value(ty: &CType, r: &str) -> String {
    match ty {
        CType::Bool => format!("{r} !== 0"),
        CType::Int8 => format!("({r} << 24) >> 24"),
        CType::Int16 => format!("({r} << 16) >> 16"),
        CType::Uint8 => format!("{r} & 0xff"),
        CType::Uint16 => format!("{r} & 0xffff"),
        CType::Uint32 => format!("{r} >>> 0"),
        CType::Uint64 => format!("BigInt.asUintN(64, {r})"),
        CType::Int32 | CType::Enum { .. } | CType::Int64 | CType::Float | CType::Double => {
            r.to_string()
        }
        other => unreachable!("{other:?} is not a scalar slot"),
    }
}

/// The `DataView` read of a scalar of type `ty` stored at `addr`.
fn load(ty: &CType, addr: &str) -> String {
    let get = match ty {
        CType::Bool => return format!("m.view().getUint8({addr}) !== 0"),
        CType::Int8 => return format!("m.view().getInt8({addr})"),
        CType::Uint8 => return format!("m.view().getUint8({addr})"),
        CType::Int16 => "getInt16",
        CType::Uint16 => "getUint16",
        CType::Int32 | CType::Enum { .. } => "getInt32",
        CType::Uint32 => "getUint32",
        CType::Int64 => "getBigInt64",
        CType::Uint64 => "getBigUint64",
        CType::Float => "getFloat32",
        CType::Double => "getFloat64",
        other => unreachable!("{other:?} is not a scalar slot"),
    };
    format!("m.view().{get}({addr}, true)")
}

/// The `DataView` write of the scalar JavaScript value `v` (already
/// checked) of type `ty` at `addr`.
fn store(ty: &CType, addr: &str, v: &str) -> String {
    let set = match ty {
        CType::Bool => return format!("m.view().setUint8({addr}, {v} ? 1 : 0)"),
        CType::Int8 => return format!("m.view().setInt8({addr}, {v})"),
        CType::Uint8 => return format!("m.view().setUint8({addr}, {v})"),
        CType::Int16 => "setInt16",
        CType::Uint16 => "setUint16",
        CType::Int32 | CType::Enum { .. } => "setInt32",
        CType::Uint32 => "setUint32",
        CType::Int64 => "setBigInt64",
        CType::Uint64 => "setBigUint64",
        CType::Float => "setFloat32",
        CType::Double => "setFloat64",
        other => unreachable!("{other:?} is not a scalar slot"),
    };
    format!("m.view().{set}({addr}, {v}, true)")
}

/// The type an out slot (`T* out_x`) points to.
fn pointee(ty: &CType) -> &CType {
    match ty {
        CType::Ptr { pointee, .. } => pointee,
        other => unreachable!("{other:?} is not an out slot"),
    }
}

/// The marshalled arguments of one entry point.
#[derive(Default)]
struct Args {
    /// The JavaScript parameter names.
    params: Vec<String>,
    /// Checks and conversions that run first, in parameter order, so a bad
    /// argument throws before anything is staged or registered.
    prep: Vec<String>,
    /// Staging statements (inside the `try`).
    stage: Vec<String>,
    /// Staged locals released in the `finally`.
    staged: Vec<String>,
    /// The wasm argument expression of each slot, by slot name.
    slots: HashMap<String, String>,
}

impl Args {
    fn slot(&mut self, slot: &AbiParam, expr: impl Into<String>) {
        self.slots.insert(slot.name.clone(), expr.into());
    }

    /// The call of `abi` through `x`, every slot in the model's order.
    fn call(&self, x: &str, abi: &AbiFn) -> String {
        let args: Vec<&str> = abi
            .params
            .iter()
            .map(|p| {
                self.slots
                    .get(&p.name)
                    .unwrap_or_else(|| {
                        panic!("no argument for slot `{}` of {}", p.name, abi.symbol)
                    })
                    .as_str()
            })
            .collect();
        format!("{x}.{}({})", abi.symbol, args.join(", "))
    }
}

fn marshal(model: &Model, f: &FnBinding) -> Args {
    let mut a = Args::default();
    if let Some(recv) = &f.receiver {
        a.params.push("self".into());
        a.slot(recv, "m.handle(self)");
    }
    for (n, p) in f.params.iter().enumerate() {
        let v = format!("a{n}");
        a.params.push(v.clone());
        match &p.pass {
            ArgPass::Direct { slot } => {
                a.prep
                    .push(format!("const c{n} = {};", scalar_arg(&slot.ty, &v)));
                a.slot(slot, format!("c{n}"));
            }
            ArgPass::OptDirect { has, value, .. } => {
                a.prep.push(format!("const h{n} = m.some({v});"));
                a.prep.push(format!(
                    "const c{n} = h{n} ? {} : {};",
                    scalar_arg(&value.ty, &v),
                    scalar_zero(&value.ty)
                ));
                a.slot(has, format!("h{n} ? 1 : 0"));
                a.slot(value, format!("c{n}"));
            }
            ArgPass::Slice { ptr, len, elem } => {
                let s = format!("s{n}");
                a.stage
                    .push(format!("{s} = m.slice({v}, {});", typed_array(*elem)));
                a.staged.push(s.clone());
                a.slot(ptr, format!("{s}[0]"));
                a.slot(len, format!("{s}[1]"));
            }
            ArgPass::String { ptr, len }
            | ArgPass::Bytes { ptr, len }
            | ArgPass::Buffer { ptr, len } => {
                let s = format!("s{n}");
                let stage = if matches!(p.pass, ArgPass::String { .. }) {
                    "str"
                } else {
                    "data"
                };
                a.stage.push(format!("{s} = m.{stage}({v});"));
                a.staged.push(s.clone());
                a.slot(ptr, format!("{s}[0]"));
                a.slot(len, format!("{s}[1]"));
            }
            ArgPass::Object { slot, nullable, .. } => {
                let check = if *nullable { "handleOpt" } else { "handle" };
                a.prep.push(format!("const c{n} = m.{check}({v});"));
                a.slot(slot, format!("c{n}"));
            }
            ArgPass::Callback {
                ctx,
                vtable,
                nullable,
                interface,
            } => {
                let tag = &model.callback_interface(interface).c_tag;
                a.slot(ctx, format!("m.register({v})"));
                a.slot(
                    vtable,
                    if *nullable {
                        format!("m.vtableOpt({v}, '{tag}', () => $vt_{tag}(m))")
                    } else {
                        format!("m.vtable('{tag}', () => $vt_{tag}(m))")
                    },
                );
            }
        }
    }
    if let Some(token) = f.async_binding().and_then(|ab| ab.cancel_token.as_ref()) {
        a.params.push("token".into());
        a.slot(token, "m.handleOpt(token)");
    }
    a
}

/// Emit `body` (the lines of an entry point) after the argument checks,
/// wrapped in the staging `try`/`finally` when anything is staged.
fn emit_staged(w: &mut CodeWriter, a: &Args, body: &[String]) {
    for l in &a.prep {
        w.line(l);
    }
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
/// `$Linear`, preceded by the callback-interface vtable builders. Releases
/// (`_destroy`, cancel tokens) go through `m.q`, which does nothing once the
/// instance is poisoned.
pub(crate) fn render_bind(w: &mut CodeWriter, model: &Model) {
    let prefix = model.prefix();
    for (_, cb) in model.callback_interfaces() {
        emit_vtable(w, cb);
    }

    w.line("// The raw entry points, one per C symbol.");
    w.block("function $bind(m) {", "}", |w| {
        w.line("const x = m.x;");
        w.line("const q = m.q;");
        w.block("return {", "};", |w| {
            w.line(format!(
                "{prefix}_abi_version: () => x.{prefix}_abi_version() >>> 0,"
            ));
            w.line(format!(
                "{prefix}_debug_live: (kind) => m.live(x.{prefix}_debug_live(m.i32(kind)), kind),"
            ));
            w.line(format!(
                "{prefix}_cancel_token_create: () => x.{prefix}_cancel_token_create(),"
            ));
            for op in ["cancel", "destroy"] {
                w.line(format!(
                    "{prefix}_cancel_token_{op}: (t) => q.{prefix}_cancel_token_{op}(m.handle(t)),"
                ));
            }
            for root in model.roots() {
                let sym = contract_symbol(prefix, &root.name);
                w.line(format!("{sym}: () => m.contract(x.{sym}),"));
            }
            for m in &model.modules {
                for i in &m.interfaces {
                    w.line(format!("{0}: (h) => x.{0}(m.handle(h)),", i.clone_symbol));
                    w.line(format!("{0}: (h) => q.{0}(m.handle(h)),", i.destroy_symbol));
                }
                for f in m.callables() {
                    emit_callable(w, model, f);
                }
            }
        });
    });
}

fn emit_callable(w: &mut CodeWriter, model: &Model, f: &FnBinding) {
    let mut a = marshal(model, f);
    let head = |sym: &str, params: &[String]| format!("{sym}: ({}) => {{", params.join(", "));
    let symbol = &f.abi.symbol;
    if let Some(ab) = f.async_binding() {
        let names: Vec<String> = ab.result.slots().iter().map(|p| p.name.clone()).collect();
        let value = match &ab.result {
            ResultPass::Void => "undefined".to_string(),
            ResultPass::Direct { result } => scalar_value(&result.ty, &result.name),
            ResultPass::OptDirect { has, value } => format!(
                "{} !== 0 ? {} : null",
                has.name,
                scalar_value(&value.ty, &value.name)
            ),
            ResultPass::Slice { ptr, len, elem } => format!(
                "m.takeSlice({}, {}, {})",
                ptr.name,
                len.name,
                typed_array(*elem)
            ),
            ResultPass::String { ptr, len } => format!("m.takeStr({}, {})", ptr.name, len.name),
            ResultPass::Bytes { ptr, len } | ResultPass::Buffer { ptr, len } => {
                format!("m.takeData({}, {})", ptr.name, len.name)
            }
            ResultPass::Object {
                result, nullable, ..
            } => {
                if *nullable {
                    format!("{0} === 0 ? null : {0}", result.name)
                } else {
                    result.name.clone()
                }
            }
        };
        let convert = format!("({}) => {value}", names.join(", "));
        a.slots.insert("callback".into(), "cb".into());
        a.slots.insert("context".into(), "ctx".into());
        let body = vec![format!(
            "return m.launch({}, (cb, ctx) => {}, {convert});",
            signature(&ab.callback_params, &CType::Void),
            a.call("x", &f.abi)
        )];
        w.block(head(symbol, &a.params), "},", |w| emit_staged(w, &a, &body));
        return;
    }

    let out_ptr = |slot: &AbiParam| -> &'static str {
        if slot.name == "out_value" {
            "m.out"
        } else {
            "m.len"
        }
    };
    for slot in f.ret_pass.out_slots() {
        a.slot(slot, out_ptr(slot));
    }
    a.slots.insert("out_err".into(), "m.err".into());
    let call = a.call("x", &f.abi);
    let value = match &f.ret_pass {
        RetPass::Void => None,
        RetPass::Direct => Some(scalar_value(&f.abi.ret, "r")),
        RetPass::OptDirect { out_value } => Some(format!(
            "r !== 0 ? {} : null",
            load(pointee(&out_value.ty), "m.out")
        )),
        RetPass::Slice { elem, .. } => Some(format!(
            "m.takeSlice(r, m.outLen(), {})",
            typed_array(*elem)
        )),
        RetPass::String { .. } => Some("m.takeStr(r, m.outLen())".into()),
        RetPass::Bytes { .. } | RetPass::Buffer { .. } => Some("m.takeData(r, m.outLen())".into()),
        RetPass::Object { nullable: true, .. } => Some("r === 0 ? null : r".into()),
        RetPass::Object { .. } | RetPass::Iterator(_) => Some("r".into()),
    };
    let body = match &value {
        None => vec![format!("{call};"), "m.check();".into()],
        Some(v) => vec![
            format!("const r = {call};"),
            "m.check();".into(),
            format!("return {v};"),
        ],
    };
    w.block(head(symbol, &a.params), "},", |w| emit_staged(w, &a, &body));

    if let Some(it) = f.iterator() {
        emit_iterator(w, it);
    }
}

/// Emit an iterator's `next` (`undefined` once exhausted, else the element,
/// `null` for an absent optional one) and `destroy` entry points.
fn emit_iterator(w: &mut CodeWriter, it: &IteratorBinding) {
    let mut a = Args::default();
    a.slot(&it.next.params[0], "m.handle(h)");
    a.slots.insert("out_err".into(), "m.err".into());
    let value = match &it.item {
        ItemPass::Direct { out_item } => {
            a.slot(out_item, "m.item");
            load(pointee(&out_item.ty), "m.item")
        }
        ItemPass::OptDirect { out_has, out_item } => {
            a.slot(out_has, "m.has");
            a.slot(out_item, "m.item");
            format!(
                "m.view().getUint8(m.has) !== 0 ? {} : null",
                load(pointee(&out_item.ty), "m.item")
            )
        }
        ItemPass::Slice {
            out_item,
            out_len,
            elem,
        } => {
            a.slot(out_item, "m.item");
            a.slot(out_len, "m.len");
            format!(
                "m.takeSlice(m.ptrAt(m.item), m.outLen(), {})",
                typed_array(*elem)
            )
        }
        ItemPass::String { out_item, out_len } => {
            a.slot(out_item, "m.item");
            a.slot(out_len, "m.len");
            "m.takeStr(m.ptrAt(m.item), m.outLen())".into()
        }
        ItemPass::Bytes { out_item, out_len } | ItemPass::Buffer { out_item, out_len } => {
            a.slot(out_item, "m.item");
            a.slot(out_len, "m.len");
            "m.takeData(m.ptrAt(m.item), m.outLen())".into()
        }
        ItemPass::Object {
            out_item, nullable, ..
        } => {
            a.slot(out_item, "m.item");
            if *nullable {
                "m.ptrAt(m.item) || null".into()
            } else {
                "m.ptrAt(m.item)".into()
            }
        }
    };
    w.block(format!("{}: (h) => {{", it.next.symbol), "},", |w| {
        w.line(format!("const has = {};", a.call("x", &it.next)));
        w.line("m.check();");
        w.line("if (has === 0) return undefined;");
        w.line(format!("return {value};"));
    });
    w.line(format!(
        "{0}: (h) => q.{0}(m.handle(h)),",
        it.destroy_symbol
    ));
}

/// The JavaScript parameter of a callback method slot.
fn field(slot: &AbiParam) -> String {
    format!("p_{}", slot.name)
}

/// Emit `$vt_{tag}(m)`, the table functions of one callback interface's
/// vtable methods: each converts the raw slots into the values the adapter
/// takes, calls it, and hands the result back (a direct value or object
/// handle as the return value, an optional scalar as the `bool` return and
/// a value behind `out_value`, a string, bytes, buffer, or typed array as a
/// run in the `out_ptr`/`out_len` slots), reporting an exception through
/// `out_err` instead of letting it unwind into the module.
fn emit_vtable(w: &mut CodeWriter, cb: &CallbackInterfaceBinding) {
    w.block(format!("function $vt_{}(m) {{", cb.c_tag), "}", |w| {
        w.block("return [", "];", |w| {
            for method in &cb.methods {
                emit_vtable_method(w, method);
            }
        });
    });
    w.blank();
}

fn emit_vtable_method(w: &mut CodeWriter, method: &CallbackMethodBinding) {
    let args: Vec<String> = method
        .params
        .iter()
        .map(|p| match &p.pass {
            ArgPass::Direct { slot } => scalar_value(&slot.ty, &field(slot)),
            ArgPass::OptDirect { has, value, .. } => format!(
                "{} !== 0 ? {} : null",
                field(has),
                scalar_value(&value.ty, &field(value))
            ),
            ArgPass::Slice { ptr, len, elem } => format!(
                "m.readSlice({}, {}, {})",
                field(ptr),
                field(len),
                typed_array(*elem)
            ),
            ArgPass::String { ptr, len } => format!("m.readStr({}, {})", field(ptr), field(len)),
            ArgPass::Bytes { ptr, len } | ArgPass::Buffer { ptr, len } => {
                format!("m.readData({}, {})", field(ptr), field(len))
            }
            ArgPass::Object { slot, nullable, .. } => {
                if *nullable {
                    format!("{0} === 0 ? null : {0}", field(slot))
                } else {
                    field(slot)
                }
            }
            ArgPass::Callback { .. } => unreachable!("callback methods take value types only"),
        })
        .collect();
    let call = format!("m.adapter(p_ctx).{}({})", method.name, args.join(", "));
    let ret = &method.abi.ret;
    let (body, fallback): (Vec<String>, Option<&str>) = match &method.ret_pass {
        CallbackRetPass::Void => (vec![format!("{call};")], None),
        CallbackRetPass::Direct => {
            let value = if *ret == CType::Bool {
                format!("{call} ? 1 : 0")
            } else {
                call
            };
            (vec![format!("return {value};")], Some(scalar_zero(ret)))
        }
        CallbackRetPass::OptDirect { out_value } => (
            vec![
                format!("const v = {call};"),
                "if (v === null) return 0;".into(),
                format!(
                    "{};",
                    store(
                        pointee(&out_value.ty),
                        &format!("{} >>> 0", field(out_value)),
                        "v"
                    )
                ),
                "return 1;".into(),
            ],
            Some("0"),
        ),
        CallbackRetPass::Slice {
            out_ptr,
            out_len,
            elem,
        } => (
            vec![format!(
                "m.giveSlice({}, {}, {call}, {});",
                field(out_ptr),
                field(out_len),
                typed_array(*elem)
            )],
            None,
        ),
        CallbackRetPass::String { out_ptr, out_len } => (
            vec![format!(
                "m.giveStr({}, {}, {call});",
                field(out_ptr),
                field(out_len)
            )],
            None,
        ),
        CallbackRetPass::Bytes { out_ptr, out_len }
        | CallbackRetPass::Buffer { out_ptr, out_len } => (
            vec![format!(
                "m.give({}, {}, {call});",
                field(out_ptr),
                field(out_len)
            )],
            None,
        ),
        CallbackRetPass::Object { .. } => (vec![format!("return m.handleOpt({call});")], Some("0")),
    };
    let params: Vec<String> = method.abi.params.iter().map(field).collect();
    let err = field(method.abi.params.last().expect("out_err is the last slot"));
    w.line(format!(
        "[{}, ({}) => {{",
        signature(&method.abi.params, ret),
        params.join(", ")
    ));
    w.scope(|w| {
        w.line("try {");
        w.scope(|w| {
            for l in &body {
                w.line(l);
            }
        });
        w.line("} catch (e) {");
        w.scope(|w| {
            w.line(format!("m.foreign({err}, e);"));
            if let Some(zero) = fallback {
                w.line(format!("return {zero};"));
            }
        });
        w.line("}");
    });
    w.line("}],");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_integers_are_normalized_from_their_carrier() {
        assert_eq!(scalar_value(&CType::Int8, "r"), "(r << 24) >> 24");
        assert_eq!(scalar_value(&CType::Uint16, "r"), "r & 0xffff");
        assert_eq!(scalar_value(&CType::Uint64, "r"), "BigInt.asUintN(64, r)");
        assert_eq!(scalar_arg(&CType::Uint8, "a0"), "m.u8(a0)");
        assert_eq!(scalar_zero(&CType::Int64), "0n");
        assert_eq!(
            store(&CType::Int16, "p", "v"),
            "m.view().setInt16(p, v, true)"
        );
    }
}
