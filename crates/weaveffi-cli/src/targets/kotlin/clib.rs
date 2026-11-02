//! `CLibrary.kt`: the JNA binding of the C ABI and the plumbing around it.
//!
//! * `CLibrary`, the JNA `Library` interface with one method per C function
//!   the header declares (named after its symbol, so JNA finds it by name),
//!   extending the fixed runtime surface (`CRuntime`);
//! * `CONTRACT`, the contract rows the loader checks;
//! * `nativeError`, which maps a failure to its exception;
//! * `Completions`, one static JNA callback per async result type, which
//!   hands each completion to the call's coroutine;
//! * one `{Cb}Vtable` object per callback interface: a static vtable whose
//!   entries are static JNA callbacks dispatching on the context to the
//!   Kotlin implementation, so every callback object the library can call
//!   stays reachable for good.

use crate::codegen::contract;
use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiFn, CType};
use weaveffi_model::model::{
    CallbackBinding, CallbackMethodBinding, ErrorStrategy, FnBinding, Model,
};
use weaveffi_model::ty::Prim;

use crate::targets::kotlin::calls::{
    completion_result, completion_stem, completion_value, jna_type, kt_string, lift, lower,
    slot_name, Flow,
};
use crate::targets::kotlin::names::{kt_member, kt_param, Names, TRAP_DOMAIN, UNTYPED_DOMAIN};
use crate::targets::kotlin::runtime::opt_fn;

/// The `CLibrary` method declaring the C function `abi`, with `callback`
/// the completion type of an async launcher.
fn declare(w: &mut CodeWriter, abi: &AbiFn, completion: Option<&str>) {
    let params: Vec<String> = abi
        .params
        .iter()
        .map(|p| {
            let ty = match (&p.ty, completion) {
                (CType::Named(_), Some(c)) => format!("Completions.{c}"),
                (ty, _) => jna_type(ty, false),
            };
            format!("{}: {ty}", slot_name(&p.name))
        })
        .collect();
    let ret = match &abi.ret {
        CType::Void => String::new(),
        ty => format!(": {}", jna_type(ty, false)),
    };
    w.line(format!("fun {}({}){ret}", abi.symbol, params.join(", ")));
}

/// The `CLibrary` methods behind callable `f`: its entry point, plus an
/// iterator's `_next` and `_destroy`.
fn declare_callable(w: &mut CodeWriter, f: &FnBinding) {
    let completion = f
        .async_binding()
        .map(|_| format!("{}Callback", completion_stem(completion_result(f))));
    declare(w, &f.abi, completion.as_deref());
    if let Some(it) = f.iterator() {
        w.blank();
        declare(w, &it.next, None);
        w.blank();
        w.line(format!("fun {}(iter: Pointer?)", it.destroy_symbol));
    }
}

/// The `CLibrary` interface.
fn render_interface(w: &mut CodeWriter, model: &Model) {
    w.blank();
    w.line("/**");
    w.line(" * The library's C functions, one per symbol the C header declares. JNA");
    w.line(" * binds each by name; [NativeLibrary] loads it and checks its contract.");
    w.line(" */");
    w.block("internal interface CLibrary : CRuntime {", "}", |w| {
        let mut first = true;
        let mut sep = |w: &mut CodeWriter| {
            if !std::mem::take(&mut first) {
                w.blank();
            }
        };
        for i in &model.interfaces {
            for f in i.members() {
                sep(w);
                declare_callable(w, f);
            }
            sep(w);
            w.line(format!("fun {}(self: Pointer?): Pointer?", i.clone_symbol));
            w.blank();
            w.line(format!("fun {}(self: Pointer?)", i.destroy_symbol));
        }
        for f in &model.functions {
            sep(w);
            declare_callable(w, f);
        }
    });
}

/// The contract rows.
fn render_contract(w: &mut CodeWriter, model: &Model) {
    w.blank();
    w.line("/** The declarations these bindings were generated with, checked at load. */");
    w.block(
        "internal val CONTRACT: Array<ContractRow> = arrayOf(",
        ")",
        |w| {
            for row in contract::rows(model) {
                w.line(format!(
                    "ContractRow(\"{}\", {}uL.toLong(), \"{}\"),",
                    row.symbol,
                    contract::hex(row.hash),
                    kt_string(&row.path)
                ));
            }
        },
    );
}

/// `nativeError`, the factory every failure maps through.
fn render_errors(w: &mut CodeWriter, n: &Names) {
    w.blank();
    w.line("/**");
    w.line(" * The exception for a native failure of a call whose errors map through");
    w.line(format!(
        " * [domain]: {TRAP_DOMAIN} for a call that can't fail ([NativeBugException]), {UNTYPED_DOMAIN} for"
    ));
    w.line(" * `throws: any` ([FfiException]), or an error domain, whose unknown codes");
    w.line(" * map to the domain's base exception. Runtime (negative) codes are");
    w.line(" * [FfiException]s.");
    w.line(" */");
    w.line(
        "internal fun nativeError(domain: Int, code: Int, message: String, payload: ByteArray?): Throwable =",
    );
    w.scope(|w| {
        w.block("when {", "}", |w| {
            w.line(format!(
                "domain == {TRAP_DOMAIN} -> NativeBugException(code, message)"
            ));
            w.line("code < 0 -> FfiException(code, message)");
            for (i, exc) in n.domains() {
                w.line(format!(
                    "domain == {i} -> {exc}.fromCode(code, message, payload)"
                ));
            }
            w.line("else -> FfiException(code, message)");
        });
    });
}

/// `Completions`: one static completion callback per async result type.
fn render_completions(w: &mut CodeWriter, model: &Model) {
    let mut results: Vec<Option<&CType>> = Vec::new();
    for f in model.callables().filter(|f| f.is_async()) {
        let r = completion_result(f);
        if !results
            .iter()
            .any(|x| completion_stem(*x) == completion_stem(r))
        {
            results.push(r);
        }
    }
    if results.is_empty() {
        return;
    }
    w.blank();
    w.line("/**");
    w.line(" * The async completions, one static callback per result type, so each stays");
    w.line(" * reachable for as long as the library can call it. Each hands the outcome");
    w.line(" * to the call its context names (see [PendingCalls]).");
    w.line(" */");
    w.block("internal object Completions {", "}", |w| {
        for (i, r) in results.iter().enumerate() {
            if i > 0 {
                w.blank();
            }
            let stem = completion_stem(*r);
            let (param, arg) = match r {
                Some(ct) => (format!(", result: {}", jna_type(ct, true)), "result"),
                None => (String::new(), "null"),
            };
            let result_param = if r.is_some() { ", result" } else { "" };
            w.block(
                format!("fun interface {stem}Callback : Callback {{"),
                "}",
                |w| {
                    w.line(format!(
                        "fun invoke(context: Pointer?, err: Pointer?{param})"
                    ));
                },
            );
            w.blank();
            w.line("@JvmField");
            w.line(format!(
                "val {} = {stem}Callback {{ context, err{result_param} -> PendingCalls.complete(context, err, {arg}) }}",
                completion_value(*r)
            ));
        }
    });
}

/// The zero C value a failed callback method returns.
fn zero(ct: &CType) -> String {
    match ct {
        CType::Scalar(p) => match p {
            Prim::Bool | Prim::I8 | Prim::U8 => "0.toByte()".to_string(),
            Prim::I16 | Prim::U16 => "0.toShort()".to_string(),
            Prim::I64 | Prim::U64 => "0L".to_string(),
            Prim::F32 => "0f".to_string(),
            Prim::F64 => "0.0".to_string(),
            _ => "0".to_string(),
        },
        CType::Enum(_) => "0".to_string(),
        CType::Opt(p) => format!("{}(null)", opt_fn(*p)),
        CType::Run => "Run.ByValue(null, 0)".to_string(),
        _ => "null".to_string(),
    }
}

/// One callback interface's vtable object.
fn render_vtable(w: &mut CodeWriter, n: &Names, cb: &CallbackBinding) {
    let vtable = n.vtable(cb);
    let iface = n.ty(&cb.name);
    w.blank();
    w.line("/**");
    w.line(format!(
        " * The vtable [{iface}] implementations cross with: one static table whose"
    ));
    w.line(" * entries dispatch on the context to the implementation [Callbacks] holds.");
    w.line(" */");
    w.block(format!("internal object {vtable} {{"), "}", |w| {
        w.line("/** The JNA shapes of the table and its entries. */");
        w.block("object Jna {", "}", |w| {
            for m in &cb.methods {
                let params: Vec<String> = m
                    .abi
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", slot_name(&p.name), jna_type(&p.ty, true)))
                    .collect();
                let ret = match &m.abi.ret {
                    CType::Void => String::new(),
                    ty => format!(": {}", jna_type(ty, true)),
                };
                w.block(
                    format!("fun interface Slot{} : Callback {{", m.slot),
                    "}",
                    |w| {
                        w.line(format!("fun invoke({}){ret}", params.join(", ")));
                    },
                );
                w.blank();
            }
            let mut order = vec![
                "\"size\"".to_string(),
                "\"flags\"".to_string(),
                "\"free\"".to_string(),
            ];
            order.extend(cb.methods.iter().map(|m| format!("\"slot{}\"", m.slot)));
            w.line(format!("@Structure.FieldOrder({})", order.join(", ")));
            w.block("class Table : Structure() {", "}", |w| {
                w.line("@JvmField");
                w.line("var size: Int = 0");
                w.blank();
                w.line("@JvmField");
                w.line("var flags: Int = 0");
                w.blank();
                w.line("@JvmField");
                w.line("var free: FreeCallback? = null");
                for m in &cb.methods {
                    w.blank();
                    w.line("@JvmField");
                    w.line(format!("var slot{}: Slot{}? = null", m.slot, m.slot));
                }
            });
        });
        w.blank();
        w.block("private val table = Jna.Table().apply {", "}", |w| {
            w.line("free = Callbacks.FREE");
            for m in &cb.methods {
                dispatch(w, n, cb, m);
            }
            w.line("size = size()");
            w.line("write()");
        });
        w.blank();
        w.line("/** [impl] as the argument the library adopts (and frees). */");
        w.line(format!(
            "fun arg(impl: {iface}): CallbackArg = Callbacks.arg(impl, table)"
        ));
        w.blank();
        w.line("/** [arg], with a null vtable for no implementation. */");
        w.line(format!(
            "fun argOrNone(impl: {iface}?): CallbackArg = if (impl == null) CallbackArg() else arg(impl)"
        ));
    });
}

/// Assign one vtable entry, the dispatch of one callback method: adopts its object arguments (before
/// anything that can fail, so a failure never strands a reference the
/// library handed over), lifts every other argument (runs are borrowed),
/// calls the implementation, and lowers its result (a run the library
/// adopts, a fresh object reference). A failure never unwinds into the
/// library: it's reported through `out_err` with a zero return. A method
/// that throws a domain reports that domain's exceptions with their code and
/// payload.
fn dispatch(w: &mut CodeWriter, n: &Names, cb: &CallbackBinding, m: &CallbackMethodBinding) {
    let mut params = vec!["_ctx".to_string()];
    let mut adopted = Vec::new();
    let mut args = Vec::new();
    for p in &m.params {
        let name = kt_param(&p.name);
        params.push(name.clone());
        let value = lift(n, &p.ty, Flow::Callback, &name);
        if matches!(crossing(&p.ty), Crossing::Object { .. }) {
            adopted.push(format!("val {name}_ = {value}"));
            args.push(format!("{name}_"));
        } else {
            args.push(value);
        }
    }
    params.push("_err".to_string());
    let call = format!(
        "Callbacks.get<{}>(_ctx).{}({})",
        n.ty(&cb.name),
        kt_member(&m.name),
        args.join(", ")
    );
    let result = match &m.ret {
        Some(t) => lower(n, t, Flow::Callback, &call),
        None => call,
    };
    let (typed, fallback) = match &m.error {
        ErrorStrategy::Domain(d) => (format!("_e as? {}", n.exception(d)), -1),
        ErrorStrategy::Untyped => ("null".to_string(), -1),
        ErrorStrategy::Trap => ("null".to_string(), -4),
    };
    w.line(format!(
        "slot{} = Jna.Slot{} {{ {} ->",
        m.slot,
        m.slot,
        params.join(", ")
    ));
    w.scope(|w| {
        for a in &adopted {
            w.line(a);
        }
        w.line("try {");
        w.scope(|w| {
            w.line(&result);
        });
        w.line("} catch (_e: Throwable) {");
        w.scope(|w| {
            w.line(format!("reportFailure(_err, _e, {typed}, {fallback})"));
            if !matches!(m.abi.ret, CType::Void) {
                w.line(zero(&m.abi.ret));
            }
        });
        w.line("}");
    });
    w.line("}");
}

/// Render the body of `CLibrary.kt` (after the package line).
pub(crate) fn render_clibrary(n: &Names, model: &Model) -> String {
    let mut w = CodeWriter::four_space();
    render_interface(&mut w, model);
    render_contract(&mut w, model);
    render_errors(&mut w, n);
    render_completions(&mut w, model);
    for cb in &model.callbacks {
        render_vtable(&mut w, n, cb);
    }
    w.finish()
}
