//! The JNI shim (`{library}_jni.c`): the fixed runtime (from `runtime/`),
//! the load-time contract check, callback-interface trampolines and static
//! vtables, and one `Java_..._JniBridge_*` export per `JniBridge` native.
//!
//! Every export reads each argument's C slots off the model's passing
//! contracts ([`ArgPass`], [`RetPass`], [`ResultPass`], [`ItemPass`],
//! [`CallbackRetPass`]), so the C call matches the header exactly. Strings,
//! bytes, value buffers, and typed arrays arrive as Java arrays and are
//! copied once (`Get<Kind>ArrayRegion`) into inline or heap storage for the
//! call; the producer's runs come back as new Java arrays
//! (`New<Kind>Array`), after which the shim frees them with
//! `{prefix}_free_bytes`. An optional scalar parameter arrives as its
//! presence flag and value; an optional scalar result leaves as a box.

use crate::codegen::contract;
use crate::codegen::CodeWriter;
use weaveffi_model::abi::{AbiParam, CType};
use weaveffi_model::model::{
    CallShape, CallbackInterfaceBinding, CallbackMethodBinding, FnBinding, InterfaceBinding,
    IteratorBinding, Model,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ItemPass, ResultPass, RetPass};

use crate::targets::kotlin::bridge::{shim_descriptor, shim_name};
use crate::targets::kotlin::carrier::{Carrier, Kind};
use crate::targets::kotlin::names::Names;

/// The C name of a user parameter's JNI argument inside an export
/// (`p_{name}`); its presence flag is `h_{name}`, its copied run
/// `r_{name}`, and its pinned callback `c_{name}`. The prefixes keep the
/// four apart and away from every fixed local (`env`, `err`, `rv`, ...).
fn arg(name: &str) -> String {
    format!("p_{name}")
}

/// The static vtable the shim passes for callback interface `cb`.
fn vtable_var(n: &Names, cb: &CallbackInterfaceBinding) -> String {
    format!("Jni_{}_vtable", n.native(&cb.c_tag))
}

/// The cached method ID of one callback method's dispatch shim.
fn shim_mid(n: &Names, cb: &CallbackInterfaceBinding, method: &str) -> String {
    format!("Jni_mid_{}_{method}", n.native(&cb.c_tag))
}

/// The C function a vtable entry points at for one callback method.
fn trampoline(n: &Names, cb: &CallbackInterfaceBinding, method: &str) -> String {
    format!("Jni_{}_{method}", n.native(&cb.c_tag))
}

/// The pointee of a pointer slot (an iterator's `out_item`).
fn pointee(t: &CType) -> &CType {
    match t {
        CType::Ptr { pointee, .. } => pointee,
        other => other,
    }
}

/// The C expression of a C scalar `v` as the JNI value of `kind`.
fn to_jni(kind: Kind, v: &str) -> String {
    if kind == Kind::Boolean {
        format!("{v} ? JNI_TRUE : JNI_FALSE")
    } else {
        format!("({}){v}", kind.jni())
    }
}

/// The C expression boxing the C scalar `v` of `kind`.
fn boxed(kind: Kind, v: &str) -> String {
    format!(
        "Jni_box(env, {}, (jvalue){{.{} = {}}})",
        kind.c_const(),
        kind.jvalue(),
        to_jni(kind, v)
    )
}

/// The C expression of the JNI value `v` of `kind` as the C type `ty`.
fn from_jni(kind: Kind, ty: &str, v: &str) -> String {
    if kind == Kind::Boolean {
        format!("{v} == JNI_TRUE")
    } else {
        format!("({ty}){v}")
    }
}

/// One step before a native call that can fail on the JVM side: its
/// statements, the condition under which it failed (with an exception
/// pending), and how to undo it if a later step fails.
struct Step {
    lines: Vec<String>,
    failed: String,
    undo: String,
}

/// One export under construction: its JNI parameters (after `env` and
/// `cls`), the fallible steps, the C call's arguments in slot order, and
/// the releases after the call.
#[derive(Default)]
struct Export {
    params: Vec<String>,
    steps: Vec<Step>,
    args: Vec<String>,
    after: Vec<String>,
}

impl Export {
    /// Emit the steps, each failure undoing the earlier ones and returning
    /// `fail` (` 0`, ` NULL`, or empty for void).
    fn emit_steps(&self, w: &mut CodeWriter, fail: &str) {
        for (i, step) in self.steps.iter().enumerate() {
            for l in &step.lines {
                w.line(l);
            }
            w.line(format!("if ({}) {{", step.failed));
            w.scope(|w| {
                for earlier in self.steps[..i].iter().rev() {
                    w.line(&earlier.undo);
                }
                w.line(format!("return{fail};"));
            });
            w.line("}");
        }
    }
}

/// Lower `f`'s receiver and parameters onto their C slots, borrowing every
/// array argument and pinning every callback implementation.
fn lower_export(n: &Names, model: &Model, f: &FnBinding) -> Export {
    let p = &n.prefix;
    let mut e = Export::default();
    if let Some(slot) = &f.receiver {
        e.params.push("jlong self".into());
        e.args
            .push(format!("({})(intptr_t)self", slot.ty.render_c(p)));
    }
    for param in &f.params {
        let name = arg(&param.name);
        let carrier = Carrier::of_arg(&param.pass);
        match (&param.pass, carrier) {
            (ArgPass::Direct { slot }, Some(Carrier::Prim(k))) => {
                e.params.push(format!("{} {name}", k.jni()));
                e.args.push(from_jni(k, &slot.ty.render_c(p), &name));
            }
            (ArgPass::OptDirect { value, .. }, Some(Carrier::Split(k))) => {
                let has = format!("h_{}", param.name);
                e.params.push(format!("jboolean {has}"));
                e.params.push(format!("{} {name}", k.jni()));
                e.args.push(format!("{has} == JNI_TRUE"));
                e.args.push(from_jni(k, &value.ty.render_c(p), &name));
            }
            (
                ArgPass::Slice { ptr, .. }
                | ArgPass::String { ptr, .. }
                | ArgPass::Bytes { ptr, .. }
                | ArgPass::Buffer { ptr, .. },
                Some(c),
            ) => {
                let kind = c.run_kind().expect("runs have an element kind");
                let run = format!("r_{}", param.name);
                e.params.push(format!("{} {name}", c.jni()));
                e.steps.push(Step {
                    lines: vec![format!("Jni_run {run};")],
                    failed: format!(
                        "!Jni_borrow(env, {name}, {}, &{run})",
                        Carrier::Array(kind).c_kind()
                    ),
                    undo: format!("Jni_unborrow(&{run});"),
                });
                e.args.push(format!("({}){run}.ptr", ptr.ty.render_c(p)));
                e.args.push(format!("{run}.len"));
                e.after.push(format!("Jni_unborrow(&{run});"));
            }
            (ArgPass::Object { slot, .. }, _) => {
                e.params.push(format!("jlong {name}"));
                e.args
                    .push(format!("({})(intptr_t){name}", slot.ty.render_c(p)));
            }
            (
                ArgPass::Callback {
                    nullable,
                    interface,
                    ..
                },
                _,
            ) => {
                let cb = model.callback_interface(interface);
                let ctx = format!("c_{}", param.name);
                e.params.push(format!("jobject {name}"));
                // The producer owns this global reference from the call on
                // and drops it through the vtable's `free`.
                e.steps.push(Step {
                    lines: vec![format!("void* {ctx} = Jni_pin(env, {name});")],
                    failed: format!("{name} != NULL && {ctx} == NULL"),
                    undo: format!("Jni_unpin(env, {ctx});"),
                });
                e.args.push(ctx);
                let vtable = format!("&{}", vtable_var(n, cb));
                e.args.push(if *nullable {
                    format!("{name} != NULL ? {vtable} : NULL")
                } else {
                    vtable
                });
            }
            (pass, c) => unreachable!("no JNI form for {pass:?} as {c:?}"),
        }
    }
    e
}

impl Carrier {
    /// The shim's `Jni_kind` constant of an array or byte-array carrier.
    fn c_kind(self) -> String {
        self.run_kind().unwrap_or(Kind::Byte).c_const()
    }
}

/// Emit `JNIEXPORT {ret} JNICALL {export}(...) {` with the shared leading
/// parameters.
fn open_export(w: &mut CodeWriter, n: &Names, ret: &str, native: &str, params: &[String]) {
    let mut all = vec!["JNIEnv* env".to_string(), "jclass cls".to_string()];
    all.extend(params.iter().cloned());
    w.line(format!(
        "JNIEXPORT {ret} JNICALL {}({}) {{",
        n.jni_export(native),
        all.join(", ")
    ));
}

/// The error check after a call: throw through `domain` and return.
fn error_check(w: &mut CodeWriter, domain: u32, fail: &str) {
    w.line("if (err.code != 0) {");
    w.line(format!("    Jni_throw(env, &err, {domain});"));
    w.line(format!("    return{fail};"));
    w.line("}");
}

/// The zero-initialized error every export passes as `out_err`.
fn err_decl(p: &str) -> String {
    format!("{p}_error err = {{0, NULL, 0, NULL, 0}};")
}

/// A synchronous callable or an iterator launcher.
fn render_sync(w: &mut CodeWriter, n: &Names, model: &Model, f: &FnBinding) {
    let p = &n.prefix;
    let native = n.native(&f.abi.symbol);
    let e = lower_export(n, model, f);
    let value = f.ret.as_ref().and_then(|r| r.value());
    let carrier = Carrier::of_ret(&f.ret_pass, value);
    let (jret, fail) = match carrier {
        None => ("void".to_string(), ""),
        Some(c @ (Carrier::Prim(_) | Carrier::Handle)) => (c.jni(), " 0"),
        Some(c) => (c.jni(), " NULL"),
    };
    open_export(w, n, &jret, &native, &e.params);
    w.scope(|w| {
        w.line(err_decl(p));
        // The return's out slots, then `out_err`.
        let mut args = e.args.clone();
        match &f.ret_pass {
            RetPass::OptDirect { out_value } => {
                w.line(format!(
                    "{} {} = 0;",
                    pointee(&out_value.ty).render_c(p),
                    out_value.name
                ));
                args.push(format!("&{}", out_value.name));
            }
            RetPass::Slice { out_len, .. }
            | RetPass::String { out_len }
            | RetPass::Bytes { out_len }
            | RetPass::Buffer { out_len } => {
                w.line(format!("size_t {} = 0;", out_len.name));
                args.push(format!("&{}", out_len.name));
            }
            _ => {}
        }
        args.push("&err".into());
        w.line("(void)cls;");
        e.emit_steps(w, fail);
        let call = format!("{}({})", f.abi.symbol, args.join(", "));
        if carrier.is_none() {
            w.line(format!("{call};"));
        } else {
            w.line(format!("{} rv = {call};", f.abi.ret.render_c(p)));
        }
        for s in &e.after {
            w.line(s);
        }
        error_check(w, n.domain_index(&f.error), fail);
        match (&f.ret_pass, carrier) {
            (_, None) => {}
            (RetPass::OptDirect { out_value }, Some(Carrier::Boxed(k))) => {
                w.line(format!("return rv ? {} : NULL;", boxed(k, &out_value.name)));
            }
            (
                RetPass::Slice { out_len, .. }
                | RetPass::String { out_len }
                | RetPass::Bytes { out_len }
                | RetPass::Buffer { out_len },
                Some(c),
            ) => {
                w.line(format!(
                    "return ({})Jni_take_array(env, {}, rv, {});",
                    c.jni(),
                    c.c_kind(),
                    out_len.name
                ));
            }
            (_, Some(Carrier::Handle)) => {
                w.line("return (jlong)(intptr_t)rv;");
            }
            (_, Some(Carrier::Prim(k))) => {
                w.line(format!("return {};", to_jni(k, "rv")));
            }
            (pass, c) => unreachable!("no JNI return for {pass:?} as {c:?}"),
        }
    });
    w.line("}");
    w.blank();
}

/// The C expression making the `Any?` Kotlin receives for an async result.
fn result_value(pass: &ResultPass, c: Option<Carrier>) -> String {
    match (pass, c) {
        (ResultPass::Void, _) | (_, None) => "NULL".to_string(),
        (ResultPass::Direct { result }, Some(Carrier::Prim(k))) => boxed(k, &result.name),
        (ResultPass::OptDirect { has, value }, Some(Carrier::Boxed(k))) => {
            format!("{} ? {} : NULL", has.name, boxed(k, &value.name))
        }
        (
            ResultPass::Slice { ptr, len, .. }
            | ResultPass::String { ptr, len }
            | ResultPass::Bytes { ptr, len }
            | ResultPass::Buffer { ptr, len },
            Some(c),
        ) => format!(
            "Jni_take_array(env, {}, {}, {})",
            c.c_kind(),
            ptr.name,
            len.name
        ),
        (ResultPass::Object { result, .. }, _) => {
            boxed(Kind::Long, &format!("(intptr_t){}", result.name))
        }
        (pass, c) => unreachable!("no async result for {pass:?} as {c:?}"),
    }
}

/// An async callable: its completion trampoline, then the launcher export.
fn render_async(w: &mut CodeWriter, n: &Names, model: &Model, f: &FnBinding) {
    let CallShape::Async(ab) = &f.shape else {
        unreachable!("render_async needs an async call shape");
    };
    let p = &n.prefix;
    let native = n.native(&f.abi.symbol);
    let done = format!("Jni_done_{native}");
    let decls: Vec<String> = ab
        .callback_params
        .iter()
        .map(|s| format!("{} {}", s.ty.render_c(p), s.name))
        .collect();
    w.line(format!("static void {done}({}) {{", decls.join(", ")));
    w.scope(|w| {
        w.line("int detach = 0;");
        w.line("JNIEnv* env = Jni_complete_begin(context, err, &detach);");
        w.line("if (env == NULL) {");
        w.line("    return;");
        w.line("}");
        let value = f.ret.as_ref().and_then(|r| r.value());
        let c = Carrier::of_result(&ab.result, value);
        w.line(format!(
            "Jni_complete(env, context, detach, {});",
            result_value(&ab.result, c)
        ));
    });
    w.line("}");
    w.blank();

    let mut e = lower_export(n, model, f);
    let mut args = e.args.clone();
    if let Some(token) = &ab.cancel_token {
        e.params.push("jlong cancel_token".into());
        args.push(format!("({})(intptr_t)cancel_token", token.ty.render_c(p)));
    }
    args.push(done);
    args.push("context".into());
    e.params.push("jobject completion".into());
    e.steps.push(Step {
        lines: vec!["void* context = Jni_pin(env, completion);".into()],
        failed: "context == NULL".into(),
        undo: String::new(),
    });
    open_export(w, n, "void", &native, &e.params);
    w.scope(|w| {
        w.line("(void)cls;");
        e.emit_steps(w, "");
        w.line(format!("{}({});", f.abi.symbol, args.join(", ")));
        for s in &e.after {
            w.line(s);
        }
    });
    w.line("}");
    w.blank();
}

/// An iterator's `_next` and `_destroy` exports. `_next` hands the item to
/// Kotlin through a one-element `Array<Any?>` and returns whether there was
/// one.
fn render_iterator_natives(w: &mut CodeWriter, n: &Names, f: &FnBinding, it: &IteratorBinding) {
    let p = &n.prefix;
    let tag = &it.iter_tag;
    let next = n.native(&it.next.symbol);
    let local = |slot: &AbiParam, init: &str| {
        format!("{} {} = {init};", pointee(&slot.ty).render_c(p), slot.name)
    };
    let c = Carrier::of_item(&it.item, &it.elem);
    let (decls, value) = match (&it.item, c) {
        (ItemPass::Direct { out_item }, Carrier::Prim(k)) => {
            (vec![local(out_item, "0")], boxed(k, &out_item.name))
        }
        (ItemPass::OptDirect { out_has, out_item }, Carrier::Boxed(k)) => (
            vec![local(out_has, "false"), local(out_item, "0")],
            format!("{} ? {} : NULL", out_has.name, boxed(k, &out_item.name)),
        ),
        (
            ItemPass::Slice {
                out_item, out_len, ..
            }
            | ItemPass::String { out_item, out_len }
            | ItemPass::Bytes { out_item, out_len }
            | ItemPass::Buffer { out_item, out_len },
            c,
        ) => (
            vec![local(out_item, "NULL"), local(out_len, "0")],
            format!(
                "Jni_take_array(env, {}, {}, {})",
                c.c_kind(),
                out_item.name,
                out_len.name
            ),
        ),
        (ItemPass::Object { out_item, .. }, _) => (
            vec![local(out_item, "NULL")],
            boxed(Kind::Long, &format!("(intptr_t){}", out_item.name)),
        ),
        (item, c) => unreachable!("no iterator item for {item:?} as {c:?}"),
    };
    let mut args = vec![format!("({tag}*)(intptr_t)iter")];
    args.extend(it.item.slots().iter().map(|s| format!("&{}", s.name)));
    args.push("&err".into());
    open_export(
        w,
        n,
        "jboolean",
        &next,
        &["jlong iter".to_string(), "jobjectArray out".to_string()],
    );
    w.scope(|w| {
        w.line(err_decl(p));
        for d in &decls {
            w.line(d);
        }
        w.line("(void)cls;");
        w.line(format!(
            "int32_t has = {}({});",
            it.next.symbol,
            args.join(", ")
        ));
        error_check(w, n.domain_index(&f.error), " JNI_FALSE");
        w.line("if (has == 0) {");
        w.line("    return JNI_FALSE;");
        w.line("}");
        w.line(format!("return Jni_yield(env, out, {value});"));
    });
    w.line("}");
    w.blank();
    open_export(
        w,
        n,
        "void",
        &n.native(&it.destroy_symbol),
        &["jlong iter".to_string()],
    );
    w.scope(|w| {
        w.line("(void)env;");
        w.line("(void)cls;");
        w.line(format!("{}(({tag}*)(intptr_t)iter);", it.destroy_symbol));
    });
    w.line("}");
    w.blank();
}

/// An interface's `_clone` and `_destroy` exports.
fn render_interface_natives(w: &mut CodeWriter, n: &Names, i: &InterfaceBinding) {
    let tag = &i.c_tag;
    open_export(
        w,
        n,
        "jlong",
        &n.native(&i.clone_symbol),
        &["jlong self".to_string()],
    );
    w.scope(|w| {
        w.line("(void)env;");
        w.line("(void)cls;");
        w.line(format!(
            "return (jlong)(intptr_t){}((const {tag}*)(intptr_t)self);",
            i.clone_symbol
        ));
    });
    w.line("}");
    w.blank();
    open_export(
        w,
        n,
        "void",
        &n.native(&i.destroy_symbol),
        &["jlong self".to_string()],
    );
    w.scope(|w| {
        w.line("(void)env;");
        w.line("(void)cls;");
        w.line(format!("{}(({tag}*)(intptr_t)self);", i.destroy_symbol));
    });
    w.line("}");
    w.blank();
}

/// One trampoline: calls the method's dispatch shim with the implementation
/// (`ctx`), the `out_err` address, and every argument in its JNI form, then
/// hands the result back in the vtable entry's return or out slots.
fn render_trampoline(
    w: &mut CodeWriter,
    n: &Names,
    model: &Model,
    cb: &CallbackInterfaceBinding,
    m: &CallbackMethodBinding,
) {
    let p = &n.prefix;
    // `ctx`, the parameters' slots (as `p_{slot}`), the return's out slots,
    // then `out_err`.
    let mut decls = vec!["void* ctx".to_string()];
    for param in &m.params {
        for s in param.pass.slots() {
            decls.push(format!("{} {}", s.ty.render_c(p), arg(&s.name)));
        }
    }
    for s in m.ret_pass.out_slots() {
        decls.push(format!("{} {}", s.ty.render_c(p), s.name));
    }
    decls.push(format!("{p}_error* out_err"));
    let ret_c = m.abi.ret.render_c(p);
    let fail = if matches!(m.abi.ret, CType::Void) {
        String::new()
    } else {
        format!(" ({ret_c})0")
    };
    // The object arguments are the trampoline's to release when the call
    // can't reach the implementation.
    let adopted: Vec<String> = m
        .params
        .iter()
        .filter_map(|param| match &param.pass {
            ArgPass::Object {
                slot, interface, ..
            } => Some(format!(
                "if ({0} != NULL) {{ {1}({0}); }}",
                arg(&slot.name),
                model.interface(interface).destroy_symbol
            )),
            _ => None,
        })
        .collect();
    w.line(format!(
        "static {ret_c} {}({}) {{",
        trampoline(n, cb, &m.name),
        decls.join(", ")
    ));
    w.scope(|w| {
        w.line("int detach = 0;");
        w.line("JNIEnv* env = Jni_callback_begin(out_err, &detach);");
        w.line("if (env == NULL) {");
        w.scope(|w| {
            for a in &adopted {
                w.line(a);
            }
            w.line(format!("return{fail};"));
        });
        w.line("}");
        let mut args = vec![
            "(jobject)ctx".to_string(),
            "(jlong)(intptr_t)out_err".to_string(),
        ];
        let mut arrays = false;
        for param in &m.params {
            match (&param.pass, Carrier::of_arg(&param.pass)) {
                (ArgPass::Direct { slot }, Some(Carrier::Prim(k))) => {
                    args.push(to_jni(k, &arg(&slot.name)));
                }
                (ArgPass::OptDirect { has, value, .. }, Some(Carrier::Split(k))) => {
                    args.push(to_jni(Kind::Boolean, &arg(&has.name)));
                    args.push(to_jni(k, &arg(&value.name)));
                }
                (
                    ArgPass::Slice { ptr, len, .. }
                    | ArgPass::String { ptr, len }
                    | ArgPass::Bytes { ptr, len }
                    | ArgPass::Buffer { ptr, len },
                    Some(c),
                ) => {
                    let local = format!("a_{}", param.name);
                    w.line(format!(
                        "jarray {local} = Jni_new_array(env, {}, {}, {});",
                        c.c_kind(),
                        arg(&ptr.name),
                        arg(&len.name)
                    ));
                    arrays = true;
                    args.push(local);
                }
                (ArgPass::Object { slot, .. }, _) => {
                    args.push(format!("(jlong)(intptr_t){}", arg(&slot.name)));
                }
                (pass, c) => unreachable!("no callback argument for {pass:?} as {c:?}"),
            }
        }
        if arrays {
            // An argument the JVM couldn't make: the call never happens.
            w.line("if ((*env)->ExceptionCheck(env)) {");
            w.scope(|w| {
                for a in &adopted {
                    w.line(a);
                }
                w.line("Jni_callback_end(env, out_err, detach);");
                w.line(format!("return{fail};"));
            });
            w.line("}");
        }
        let ret = Carrier::of_callback_ret(&m.ret_pass, m.ret.as_ref());
        let stem = ret.map_or("Void", Carrier::call_stem);
        let call = format!(
            "(*env)->CallStatic{stem}Method(env, Jni_bridge, {}, {})",
            shim_mid(n, cb, &m.name),
            args.join(", ")
        );
        match (&m.ret_pass, ret) {
            (CallbackRetPass::Void, _) | (_, None) => {
                w.line(format!("{call};"));
                w.line("Jni_callback_end(env, out_err, detach);");
            }
            (CallbackRetPass::Direct, Some(Carrier::Prim(k))) => {
                // A failure leaves the zero value the shim (or JNI) returned.
                w.line(format!("{} rv = {call};", k.jni()));
                w.line("Jni_callback_end(env, out_err, detach);");
                w.line(format!("return {};", from_jni(k, &ret_c, "rv")));
            }
            (CallbackRetPass::OptDirect { out_value }, Some(Carrier::Boxed(k))) => {
                w.line("jvalue value = {0};");
                w.line(format!(
                    "bool present = Jni_callback_opt(env, {}, {call}, &value);",
                    k.c_const()
                ));
                w.line("if (present) {");
                w.line(format!(
                    "    *{} = {};",
                    out_value.name,
                    from_jni(
                        k,
                        &pointee(&out_value.ty).render_c(p),
                        &format!("value.{}", k.jvalue())
                    )
                ));
                w.line("}");
                w.line("Jni_callback_end(env, out_err, detach);");
                w.line("return present;");
            }
            (
                CallbackRetPass::Slice {
                    out_ptr, out_len, ..
                }
                | CallbackRetPass::String { out_ptr, out_len }
                | CallbackRetPass::Bytes { out_ptr, out_len }
                | CallbackRetPass::Buffer { out_ptr, out_len },
                Some(c),
            ) => {
                w.line(format!("jarray rv = (jarray){call};"));
                w.line(format!(
                    "*{} = ({})Jni_callback_run(env, {}, rv, {}, out_err);",
                    out_ptr.name,
                    pointee(&out_ptr.ty).render_c(p),
                    c.c_kind(),
                    out_len.name
                ));
                w.line("Jni_callback_end(env, out_err, detach);");
            }
            (CallbackRetPass::Object { .. }, Some(Carrier::Handle)) => {
                w.line(format!("jlong rv = {call};"));
                w.line("Jni_callback_end(env, out_err, detach);");
                w.line(format!("return ({ret_c})(intptr_t)rv;"));
            }
            (pass, c) => unreachable!("no callback return for {pass:?} as {c:?}"),
        }
    });
    w.line("}");
    w.blank();
}

/// One callback interface: the cached shim method IDs, one trampoline per
/// method, and the static vtable (header first: its size, flags 0, and the
/// `free` hook that drops the implementation's global reference).
fn render_callback_interface(
    w: &mut CodeWriter,
    n: &Names,
    model: &Model,
    cb: &CallbackInterfaceBinding,
) {
    for m in &cb.methods {
        w.line(format!(
            "static jmethodID {} = NULL;",
            shim_mid(n, cb, &m.name)
        ));
    }
    w.blank();
    for m in &cb.methods {
        render_trampoline(w, n, model, cb, m);
    }
    let mut entries = vec![
        format!("sizeof({})", cb.vtable_tag),
        "0".to_string(),
        "Jni_release_callback".to_string(),
    ];
    entries.extend(cb.methods.iter().map(|m| trampoline(n, cb, &m.name)));
    w.line(format!(
        "static const {} {} = {{{}}};",
        cb.vtable_tag,
        vtable_var(n, cb),
        entries.join(", ")
    ));
    w.blank();
}

/// The contract tables these bindings were generated with (one per
/// top-level module that declares anything), then `Jni_load`: the contract
/// checks and the classes and method IDs the shim caches.
fn render_load(w: &mut CodeWriter, n: &Names, model: &Model) {
    let tables: Vec<_> = contract::tables(model)
        .into_iter()
        .filter(|t| !t.rows.is_empty())
        .collect();
    let var = |root: &str| format!("Jni_contract_{root}");
    for table in &tables {
        w.line(format!(
            "static const Jni_contract_entry {}[] = {{",
            var(&table.root.name)
        ));
        w.scope(|w| {
            for row in &table.rows {
                w.line(format!(
                    "{{UINT64_C({}), UINT64_C({}), \"{}\"}}, /* {} */",
                    contract::hex(row.id),
                    contract::hex(row.hash),
                    row.path,
                    row.signature
                ));
            }
        });
        w.line("};");
        w.blank();
    }
    w.line("static jint Jni_load(JNIEnv* env) {");
    w.scope(|w| {
        for table in &tables {
            let v = var(&table.root.name);
            w.line(format!(
                "if (Jni_check_contract(env, {}, {v}, sizeof {v} / sizeof {v}[0]) != JNI_OK) {{",
                table.symbol
            ));
            w.line("    return JNI_ERR;");
            w.line("}");
        }
        w.line("if (Jni_load_boxes(env) != JNI_OK) {");
        w.line("    return JNI_ERR;");
        w.line("}");
        if model.has_async() {
            w.line("if (Jni_load_async(env) != JNI_OK) {");
            w.line("    return JNI_ERR;");
            w.line("}");
        }
        for (_, cb) in model.callback_interfaces() {
            for m in &cb.methods {
                let mid = shim_mid(n, cb, &m.name);
                w.line(format!(
                    "{mid} = (*env)->GetStaticMethodID(env, Jni_bridge, \"{}\", \"{}\");",
                    shim_name(n, cb, m),
                    shim_descriptor(n, cb, m)
                ));
                w.line(format!("if ({mid} == NULL) {{"));
                w.line("    return JNI_ERR;");
                w.line("}");
            }
        }
        w.line("return JNI_OK;");
    });
    w.line("}");
    w.blank();
}

/// Render the generated part of the shim (after the runtime templates).
pub(crate) fn render_exports(n: &Names, model: &Model) -> String {
    let mut w = CodeWriter::four_space();
    w.blank();
    for (_, cb) in model.callback_interfaces() {
        render_callback_interface(&mut w, n, model, cb);
    }
    render_load(&mut w, n, model);
    for m in &model.modules {
        for i in &m.interfaces {
            render_interface_natives(&mut w, n, i);
        }
        for f in m.callables() {
            match (&f.shape, &f.ret_pass) {
                (CallShape::Async(_), _) => render_async(&mut w, n, model, f),
                (CallShape::Sync, RetPass::Iterator(it)) => {
                    render_sync(&mut w, n, model, f);
                    render_iterator_natives(&mut w, n, f, it);
                }
                (CallShape::Sync, _) => render_sync(&mut w, n, model, f),
            }
        }
    }
    w.finish()
}
