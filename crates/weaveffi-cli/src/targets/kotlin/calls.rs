//! Public Kotlin wrappers for callables: how each argument lowers to its
//! JNI form, how each result lifts back, and the borrow scopes that keep
//! every object (receiver and arguments) alive and unreleased for the whole
//! native call.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{CallShape, ErrorBinding, FnBinding, ParamBinding};
use weaveffi_model::ty::{Family, Prim, Ty};

use crate::targets::kotlin::codec::{decode_expr, encode_expr};
use crate::targets::kotlin::names::{jni_kind, kt_param, unsigned_conversions, Names};

/// One line of a generated body, with its depth relative to the body's
/// first line.
type Line = (usize, String);

/// `expr` in receiver position: as is when it's a postfix chain (a name,
/// member, call, or index, with no space outside its brackets), else
/// parenthesized.
fn receiver(expr: &str) -> String {
    let mut depth = 0usize;
    for c in expr.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 => return format!("({expr})"),
            _ => {}
        }
    }
    expr.to_string()
}

/// The Kotlin expression lowering the public value `expr` of `t` to its JNI
/// form. An object lowers to a new strong reference (a callback method's
/// return, which the producer adopts); call arguments borrow objects
/// instead (see [`emit_callable`]).
pub(crate) fn lower(n: &Names, t: &Ty, expr: &str) -> String {
    if let Some((_, to_signed)) = unsigned_conversions(t) {
        return format!("{}.{to_signed}()", receiver(expr));
    }
    match t.family() {
        Family::Direct if matches!(t, Ty::Enum(_)) => format!("{}.value", receiver(expr)),
        Family::String => format!("encodeUtf8({expr})"),
        Family::Buffer => encode_expr(n, t, expr),
        Family::Object { nullable: false } => format!("{}.cloneHandle()", receiver(expr)),
        Family::Object { nullable: true } => format!("{}?.cloneHandle() ?: 0L", receiver(expr)),
        _ => expr.to_string(),
    }
}

/// The Kotlin expression lifting the JNI form `expr` of `t` back into its
/// public value. Objects are adopted: the wrapper owes the reference's
/// release.
pub(crate) fn lift(n: &Names, t: &Ty, expr: &str) -> String {
    if let Some((to_unsigned, _)) = unsigned_conversions(t) {
        return format!("{}.{to_unsigned}()", receiver(expr));
    }
    match t {
        Ty::Enum(name) => format!("{}.fromValue({expr})", n.ty(name)),
        Ty::Prim(Prim::String) => format!("decodeUtf8({expr})"),
        Ty::Interface(name) => format!("{}.fromHandle({expr})", n.ty(name)),
        Ty::Optional(inner) if !t.is_buffered() => {
            let name = inner
                .interface_name()
                .expect("only Interface? is an unbuffered optional value");
            format!("{}.fromHandleOrNull({expr})", n.ty(name))
        }
        _ if t.is_buffered() => decode_expr(n, t, expr),
        _ => expr.to_string(),
    }
}

/// The borrow scopes and JNI arguments for a call to `f`: the receiver
/// (when `has_self`) and every object argument open one `borrow` scope each,
/// a callback interface passes the implementing object (the shim pins it),
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
        match p.ty.family() {
            Family::Object { nullable: false } => {
                opens.push(format!("{name}.handle.borrow {{ _h{i} ->"));
                args.push(format!("_h{i}"));
            }
            Family::Object { nullable: true } => {
                opens.push(format!("{name}?.handle.borrowOrNull {{ _h{i} ->"));
                args.push(format!("_h{i}"));
            }
            Family::Callback { .. } => args.push(name),
            _ => args.push(lower(n, &p.ty, &name)),
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

/// Write `lines` one level below the writer's depth plus each line's own.
fn write_lines(w: &mut CodeWriter, lines: impl IntoIterator<Item = Line>) {
    for (d, l) in lines {
        for _ in 0..d {
            w.indent();
        }
        w.line(l);
        for _ in 0..d {
            w.dedent();
        }
    }
}

/// Emit a function declaration `sig` (everything before `=` or `{`) whose
/// body is the expression `lines`: an expression body when the function
/// returns a value, a block body otherwise.
fn emit_body(w: &mut CodeWriter, sig: &str, returns: bool, lines: Vec<Line>) {
    if returns {
        let mut iter = lines.into_iter();
        let (_, first) = iter.next().expect("a body has at least one line");
        w.line(format!("{sig} = {first}"));
        write_lines(w, iter);
    } else {
        w.line(format!("{sig} {{"));
        w.scope(|w| write_lines(w, lines));
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
pub(crate) fn ret_sig(n: &Names, ret: Option<&Ty>) -> String {
    ret.map(|t| format!(": {}", n.kt_type(t)))
        .unwrap_or_default()
}

/// The `@Deprecated` annotation for a deprecation message.
pub(crate) fn deprecated_line(msg: &str) -> String {
    format!("@Deprecated(\"{}\")", kt_string(msg))
}

/// Escape `s` for the inside of a Kotlin string literal.
pub(crate) fn kt_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
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
/// through a one-element primitive array of its JNI carrier (strings,
/// bytes, and buffers come back as a nullable `ByteArray` instead).
pub(crate) struct ItemSlot {
    /// The Kotlin array type (`IntArray`).
    pub kotlin_array: String,
    /// The JNI array type (`jintArray`).
    pub jni_array: String,
    /// The JNI element type (`jint`).
    pub jni_elem: String,
    /// The JNI array-region setter stem (`Int` in `SetIntArrayRegion`).
    pub region: &'static str,
}

/// The slot an iterator element of `t` comes back through, or `None` for
/// the `ByteArray` families.
pub(crate) fn item_slot(t: &Ty) -> Option<ItemSlot> {
    match t.family() {
        Family::Direct | Family::Object { .. } => {
            let kind = jni_kind(t);
            Some(ItemSlot {
                kotlin_array: format!("{kind}Array"),
                jni_array: format!("j{}Array", kind.to_lowercase()),
                jni_elem: format!("j{}", kind.to_lowercase()),
                region: kind,
            })
        }
        _ => None,
    }
}

/// Emit the public wrapper for callable `f`: `decl` is everything before the
/// parameter list (`fun count`, `operator fun invoke`; `suspend` is added
/// for async callables), `has_self` borrows the receiver, and `error` is the
/// domain a throwing callable maps failures through.
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
                Some(t) => format!(
                    "{{ _raw -> {} }}",
                    lift(n, t, &format!("_raw as {}", n.jni_type(t)))
                ),
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
