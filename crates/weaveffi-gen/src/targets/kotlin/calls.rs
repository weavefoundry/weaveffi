//! Public Kotlin wrappers for callables: how each argument lowers to its
//! JNI form, how each result lifts back, and the borrow scopes that keep
//! every object (receiver and arguments) alive and unreleased for the whole
//! native call.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{CallShape, ErrorBinding, FnBinding, ParamBinding, Ty};

use crate::targets::kotlin::codec::{decode_expr, encode_expr};
use crate::targets::kotlin::names::{kt_param, Names};

/// One line of a generated expression, with its depth relative to the
/// expression's first line.
type Line = (usize, String);

/// The Kotlin expression lowering a non-object argument `expr` of `t` to its
/// JNI form.
pub(crate) fn lower(n: &Names, t: &Ty, expr: &str) -> String {
    match t {
        Ty::Enum(_) => format!("{expr}.value"),
        Ty::StringUtf8 => format!("encodeUtf8({expr})"),
        _ if t.is_buffered() => encode_expr(n, t, expr),
        _ => expr.to_string(),
    }
}

/// The Kotlin expression lifting the JNI form `expr` of `t` back into its
/// public value. Objects are adopted: the wrapper owes the reference's
/// release.
pub(crate) fn lift(n: &Names, t: &Ty, expr: &str) -> String {
    match t {
        Ty::Enum(name) => format!("{}.fromValue({expr})", n.ty(name)),
        Ty::StringUtf8 => format!("decodeUtf8({expr})"),
        Ty::Interface(name) => format!("{}.fromHandle({expr})", n.ty(name)),
        Ty::Optional(inner) if !t.is_buffered() => {
            let name = inner
                .interface_name()
                .expect("only Interface? is unbuffered");
            format!("{}.fromHandleOrNull({expr})", n.ty(name))
        }
        _ if t.is_buffered() => decode_expr(n, t, expr),
        _ => expr.to_string(),
    }
}

/// The borrow scopes and JNI arguments for a call to `f`: the receiver
/// (when `has_self`) and every object argument open one `borrow` scope each,
/// and every other argument lowers inline.
fn lower_args(n: &Names, f: &FnBinding, has_self: bool) -> (Vec<String>, Vec<String>) {
    let mut opens = Vec::new();
    let mut args = Vec::new();
    if has_self {
        opens.push("handle.borrow { _self ->".to_string());
        args.push("_self".to_string());
    }
    for (i, p) in f.params.iter().enumerate() {
        let name = kt_param(&p.name);
        match &p.ty {
            Ty::Interface(_) => {
                opens.push(format!("{name}.handle.borrow {{ _h{i} ->"));
                args.push(format!("_h{i}"));
            }
            Ty::Optional(_) if !p.ty.is_buffered() => {
                opens.push(format!("{name}?.handle.borrowOrNull {{ _h{i} ->"));
                args.push(format!("_h{i}"));
            }
            t => args.push(lower(n, t, &name)),
        }
    }
    (opens, args)
}

/// Wrap `body` (lines relative to depth 0) in the borrow scopes `opens`.
fn nest(opens: &[String], body: Vec<Line>) -> Vec<Line> {
    let mut out: Vec<Line> = opens
        .iter()
        .enumerate()
        .map(|(d, o)| (d, o.clone()))
        .collect();
    out.extend(body.into_iter().map(|(d, l)| (d + opens.len(), l)));
    for d in (0..opens.len()).rev() {
        out.push((d, "}".to_string()));
    }
    out
}

/// Emit a function declaration `sig` (everything before `=` or `{`) whose
/// body is the expression `lines`: an expression body when the function
/// returns a value, a block body otherwise.
fn emit_body(w: &mut CodeWriter, sig: &str, returns: bool, lines: Vec<Line>) {
    let base = w.depth();
    if returns {
        let mut iter = lines.into_iter();
        let (_, first) = iter.next().expect("a body has at least one line");
        w.line(format!("{sig} = {first}"));
        for (d, l) in iter {
            w.raw(format!("{}{l}\n", "    ".repeat(base + d)));
        }
    } else {
        w.line(format!("{sig} {{"));
        for (d, l) in lines {
            w.raw(format!("{}{l}\n", "    ".repeat(base + 1 + d)));
        }
        w.line("}");
    }
}

/// The Kotlin parameter list of a public wrapper.
pub(crate) fn params_sig(n: &Names, params: &[ParamBinding]) -> String {
    params
        .iter()
        .map(|p| format!("{}: {}", kt_param(&p.name), n.kt_type(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `: Type` return annotation of a public wrapper, or nothing for void.
fn ret_sig(n: &Names, ret: Option<&Ty>) -> String {
    ret.map(|t| format!(": {}", n.kt_type(t)))
        .unwrap_or_default()
}

/// The `@Deprecated` annotation for a deprecation message.
pub(crate) fn deprecated_line(msg: &str) -> String {
    format!(
        "@Deprecated(\"{}\")",
        msg.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
    )
}

/// The step function of a `NativeIterator`: pulls one element through the
/// iterator's `_next` native, returning `NativeIterator.DONE` at the end.
fn iterator_step(n: &Names, f: &FnBinding) -> Vec<Line> {
    let CallShape::Iterator(it) = &f.shape else {
        unreachable!("iterator_step needs an iterator call shape");
    };
    let next = n.native(&it.next.symbol);
    match item_slot(&it.elem) {
        None => vec![
            (0, format!("val _item = JniBridge.{next}(_it)")),
            (
                0,
                format!(
                    "if (_item == null) NativeIterator.DONE else {}",
                    lift(n, &it.elem, "_item")
                ),
            ),
        ],
        Some(slot) => vec![
            (0, format!("val _slot = {}(1)", slot.kotlin_array)),
            (
                0,
                format!(
                    "if (JniBridge.{next}(_it, _slot)) {} else NativeIterator.DONE",
                    lift(n, &it.elem, "_slot[0]")
                ),
            ),
        ],
    }
}

/// How an iterator's `_next` native hands back a direct or object element:
/// through a one-element primitive array (strings, bytes, and buffers come
/// back as a nullable `ByteArray` instead).
pub(crate) struct ItemSlot {
    /// The Kotlin array type (`IntArray`).
    pub kotlin_array: &'static str,
    /// The JNI array type (`jintArray`).
    pub jni_array: &'static str,
    /// The JNI element type (`jint`).
    pub jni_elem: &'static str,
    /// The JNI array-region setter stem (`Int` in `SetIntArrayRegion`).
    pub region: &'static str,
}

/// The slot an iterator element of `t` comes back through, or `None` for
/// the `ByteArray` families.
pub(crate) fn item_slot(t: &Ty) -> Option<ItemSlot> {
    if matches!(t, Ty::StringUtf8 | Ty::Bytes) || t.is_buffered() {
        return None;
    }
    let (kotlin_array, jni_array, jni_elem, region) = match t {
        Ty::Bool => ("BooleanArray", "jbooleanArray", "jboolean", "Boolean"),
        Ty::I8 | Ty::U8 => ("ByteArray", "jbyteArray", "jbyte", "Byte"),
        Ty::I16 | Ty::U16 => ("ShortArray", "jshortArray", "jshort", "Short"),
        Ty::I32 | Ty::Enum(_) => ("IntArray", "jintArray", "jint", "Int"),
        Ty::F32 => ("FloatArray", "jfloatArray", "jfloat", "Float"),
        Ty::F64 => ("DoubleArray", "jdoubleArray", "jdouble", "Double"),
        _ => ("LongArray", "jlongArray", "jlong", "Long"),
    };
    Some(ItemSlot {
        kotlin_array,
        jni_array,
        jni_elem,
        region,
    })
}

/// Emit the public wrapper for callable `f`: `decl` is everything before the
/// parameter list (`fun count`, `operator fun invoke`, `suspend fun fetch`
/// is added for async callables), `has_self` borrows the receiver.
pub(crate) fn emit_callable(
    w: &mut CodeWriter,
    n: &Names,
    f: &FnBinding,
    decl: &str,
    has_self: bool,
    error: Option<&ErrorBinding>,
) {
    if let Some(msg) = &f.deprecated {
        w.line(deprecated_line(msg));
    }
    let params = params_sig(n, &f.params);
    let ret = ret_sig(n, f.ret.as_ref());
    let (opens, mut args) = lower_args(n, f, has_self);
    match &f.shape {
        CallShape::Sync(abi) => {
            let call = format!("JniBridge.{}({})", n.native(&abi.symbol), args.join(", "));
            let body = match &f.ret {
                Some(t) => lift(n, t, &call),
                None => call,
            };
            let sig = format!("{decl}({params}){ret}");
            if opens.is_empty() && f.ret.is_some() {
                w.line(format!("{sig} = {body}"));
            } else {
                emit_body(w, &sig, f.ret.is_some(), nest(&opens, vec![(0, body)]));
            }
        }
        CallShape::Iterator(it) => {
            let launch = format!(
                "JniBridge.{}({})",
                n.native(&it.launch.symbol),
                args.join(", ")
            );
            let mut body = vec![(
                0,
                format!(
                    "NativeIterator({launch}, JniBridge::{}) {{ _it ->",
                    n.native(&it.destroy_symbol)
                ),
            )];
            body.extend(iterator_step(n, f).into_iter().map(|(d, l)| (d + 1, l)));
            body.push((0, "}".to_string()));
            emit_body(
                w,
                &format!("{decl}({params}){ret}"),
                true,
                nest(&opens, body),
            );
        }
        CallShape::Async(ab) => {
            if f.cancellable {
                args.push("_token".to_string());
            }
            args.push("_done".to_string());
            let call = format!(
                "JniBridge.{}({})",
                n.native(&ab.launch.symbol),
                args.join(", ")
            );
            let convert = match &f.ret {
                None => "{ }".to_string(),
                // `lift` only ever places its operand alone or in argument
                // position, so the bare cast needs no parentheses.
                Some(t) => format!("{{ {} }}", lift(n, t, &format!("it as {}", n.jni_type(t)))),
            };
            let token = if f.cancellable { "_token" } else { "_" };
            let mut body = vec![(
                0,
                format!(
                    "awaitNative({}, {}, {convert}) {{ {token}, _done ->",
                    f.cancellable,
                    n.domain(f, error)
                ),
            )];
            body.extend(
                nest(&opens, vec![(0, call)])
                    .into_iter()
                    .map(|(d, l)| (d + 1, l)),
            );
            body.push((0, "}".to_string()));
            emit_body(
                w,
                &format!("suspend {decl}({params}){ret}"),
                f.ret.is_some(),
                body,
            );
        }
    }
}
