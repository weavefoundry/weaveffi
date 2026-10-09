//! Public Kotlin wrappers for callables: how each argument lowers to its
//! JNI [`Carrier`], how each result lifts back, and the borrow scopes that
//! keep every object (receiver and arguments) alive and unreleased for the
//! whole native call.

use crate::codegen::CodeWriter;
use weaveffi_model::model::{CallShape, FnBinding};
use weaveffi_model::plan::{ArgPass, RetPass};
use weaveffi_model::ty::{ParamTy, Prim, Ty};

use crate::targets::kotlin::carrier::{Carrier, Kind};
use crate::targets::kotlin::codec::{decode_expr, encode_expr};
use crate::targets::kotlin::docs::Speller;
use crate::targets::kotlin::names::{kt_param, unsigned_conversions, Names};

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

/// The scalar inside an optional (the type itself otherwise).
fn scalar(t: &Ty) -> &Ty {
    match t {
        Ty::Optional(inner) => inner,
        other => other,
    }
}

/// The element type of a list (the type itself otherwise).
fn element(t: &Ty) -> &Ty {
    match t {
        Ty::List(inner) => inner,
        other => other,
    }
}

/// The Kotlin expression lowering the public value `expr` of `t` to its JNI
/// form `c`. An object lowers to a new strong reference (a callback
/// method's return, which the producer adopts); call arguments borrow
/// objects instead (see [`emit_callable`]).
///
/// # Panics
///
/// Panics for [`Carrier::Split`], which lowers to two arguments (see
/// [`lower_split`]).
pub(crate) fn lower(n: &Names, t: &Ty, c: Carrier, expr: &str) -> String {
    match c {
        Carrier::Prim(_) => {
            if let Some((_, to_signed)) = unsigned_conversions(t) {
                format!("{}.{to_signed}()", receiver(expr))
            } else if matches!(t, Ty::Enum(_)) {
                format!("{}.value", receiver(expr))
            } else {
                expr.to_string()
            }
        }
        Carrier::Boxed(_) => {
            let inner = scalar(t);
            if let Some((_, to_signed)) = unsigned_conversions(inner) {
                format!("{}?.{to_signed}()", receiver(expr))
            } else if matches!(inner, Ty::Enum(_)) {
                format!("{}?.value", receiver(expr))
            } else {
                expr.to_string()
            }
        }
        Carrier::Array(k) => match unsigned_conversions(element(t)) {
            Some(_) => format!("{}.to{}ArrayBits()", receiver(expr), k.name()),
            None => format!("{}.to{}Array()", receiver(expr), k.name()),
        },
        Carrier::Bytes => match t {
            Ty::Prim(Prim::String) => format!("encodeUtf8({expr})"),
            Ty::Prim(Prim::Bytes) => expr.to_string(),
            _ => encode_expr(n, t, expr),
        },
        Carrier::Handle => match t {
            Ty::Optional(_) => format!("{}?.cloneHandle() ?: 0L", receiver(expr)),
            _ => format!("{}.cloneHandle()", receiver(expr)),
        },
        Carrier::Split(_) => unreachable!("an optional scalar parameter lowers to two arguments"),
    }
}

/// The two JNI arguments of an optional scalar `expr` of `t` (`T?`): its
/// presence, then its value (the kind's zero when absent).
pub(crate) fn lower_split(t: &Ty, k: Kind, expr: &str) -> (String, String) {
    let inner = scalar(t);
    let value = if let Some((_, to_signed)) = unsigned_conversions(inner) {
        format!("{}?.{to_signed}() ?: {}", receiver(expr), k.zero())
    } else if matches!(inner, Ty::Enum(_)) {
        format!("{}?.value ?: 0", receiver(expr))
    } else {
        format!("{expr} ?: {}", k.zero())
    };
    (format!("{expr} != null"), value)
}

/// The Kotlin expression lifting the JNI form `expr` (in carrier `c`) of
/// `t` back into its public value. Objects are adopted: the wrapper owes
/// the reference's release.
///
/// # Panics
///
/// Panics for [`Carrier::Split`], which lifts from two values (see
/// [`lift_split`]).
pub(crate) fn lift(n: &Names, t: &Ty, c: Carrier, expr: &str) -> String {
    match c {
        Carrier::Prim(_) => {
            if let Some((to_unsigned, _)) = unsigned_conversions(t) {
                format!("{}.{to_unsigned}()", receiver(expr))
            } else if let Ty::Enum(name) = t {
                format!("{}.fromValue({expr})", n.ty(name))
            } else {
                expr.to_string()
            }
        }
        Carrier::Boxed(_) => {
            let inner = scalar(t);
            if let Some((to_unsigned, _)) = unsigned_conversions(inner) {
                format!("{}?.{to_unsigned}()", receiver(expr))
            } else if let Ty::Enum(name) = inner {
                format!("{}?.let {{ {}.fromValue(it) }}", receiver(expr), n.ty(name))
            } else {
                expr.to_string()
            }
        }
        Carrier::Array(k) => match unsigned_conversions(element(t)) {
            Some(_) => format!("{}.toU{}List()", receiver(expr), k.name()),
            None => format!("{}.asList()", receiver(expr)),
        },
        Carrier::Bytes => match t {
            Ty::Prim(Prim::String) => format!("decodeUtf8({expr})"),
            Ty::Prim(Prim::Bytes) => expr.to_string(),
            _ => decode_expr(n, t, expr),
        },
        Carrier::Handle => {
            let name = n.ty(scalar(t).interface_name().unwrap_or_default());
            match t {
                Ty::Optional(_) => format!("{name}.fromHandleOrNull({expr})"),
                _ => format!("{name}.fromHandle({expr})"),
            }
        }
        Carrier::Split(_) => unreachable!("an optional scalar parameter lifts from two values"),
    }
}

/// The Kotlin expression lifting a split optional scalar (`has`, `value`)
/// of `t` (`T?`).
pub(crate) fn lift_split(n: &Names, t: &Ty, k: Kind, has: &str, value: &str) -> String {
    format!(
        "if ({has}) {} else null",
        lift(n, scalar(t), Carrier::Prim(k), value)
    )
}

/// The Kotlin expression lifting `expr`, an `Any?` holding carrier `c` (an
/// async result or iterator item), into the public value of `t`.
pub(crate) fn lift_erased(n: &Names, t: &Ty, c: Carrier, expr: &str) -> String {
    lift(n, t, c, &format!("{expr} as {}", c.kotlin()))
}

/// The name of the presence flag of the split optional parameter `name`
/// (already spelled for Kotlin), mirroring its C slot `has_{name}`. Kotlin
/// spellings never contain an inner underscore, so it can't collide.
pub(crate) fn has_name(name: &str) -> String {
    format!("has_{name}")
}

/// The `external fun` parameters of callable `f`: the receiver, then every
/// parameter in its JNI form (an optional scalar as its flag and value, a
/// callback interface as the implementing object).
pub(crate) fn jni_params(n: &Names, f: &FnBinding) -> Vec<String> {
    let mut out = Vec::new();
    if f.has_self() {
        out.push("_self: Long".to_string());
    }
    for p in &f.params {
        let name = kt_param(&p.name);
        match Carrier::of_arg(&p.pass) {
            Some(Carrier::Split(k)) => {
                out.push(format!("{}: Boolean", has_name(&name)));
                out.push(format!("{name}: {}", k.name()));
            }
            Some(c) => out.push(format!("{name}: {}", c.kotlin())),
            None => out.push(format!("{name}: {}", n.kt_param_type(&p.ty))),
        }
    }
    out
}

/// The borrow scopes and JNI arguments for a call to `f`: the receiver
/// and every object argument open one `borrow` scope each, a callback
/// interface passes the implementing object (the shim pins it), and every
/// other argument lowers inline.
fn lower_args(n: &Names, f: &FnBinding) -> (Vec<String>, Vec<String>) {
    let mut opens = Vec::new();
    let mut args = Vec::new();
    if f.has_self() {
        opens.push("handle.borrow { _self ->".to_string());
        args.push("_self".to_string());
    }
    for (i, p) in f.params.iter().enumerate() {
        let name = kt_param(&p.name);
        let ParamTy::Value(t) = &p.ty else {
            args.push(name);
            continue;
        };
        match (&p.pass, Carrier::of_arg(&p.pass)) {
            (
                ArgPass::Object {
                    nullable: false, ..
                },
                _,
            ) => {
                opens.push(format!("{name}.handle.borrow {{ _h{i} ->"));
                args.push(format!("_h{i}"));
            }
            (ArgPass::Object { nullable: true, .. }, _) => {
                opens.push(format!("{name}?.handle.borrowOrNull {{ _h{i} ->"));
                args.push(format!("_h{i}"));
            }
            (_, Some(Carrier::Split(k))) => {
                let (has, value) = lower_split(t, k, &name);
                args.push(has);
                args.push(value);
            }
            (_, Some(c)) => args.push(lower(n, t, c, &name)),
            (_, None) => args.push(name),
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

/// The `@Deprecated` annotation for a deprecation message.
pub(crate) fn deprecated_line(msg: &str) -> String {
    format!("@Deprecated(\"{}\")", kt_string(msg))
}

/// Escape `s` for the inside of a Kotlin string literal.
pub(crate) fn kt_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// How a public wrapper is declared.
pub(crate) struct Decl<'a> {
    /// The Kotlin name (`count`, `invoke`).
    pub name: &'a str,
    /// Whether it's the companion's `operator fun invoke` (a `new`
    /// constructor).
    pub operator: bool,
    /// Whether to annotate it `@JvmStatic` (module object and companion
    /// functions, so Java sees static methods).
    pub jvm_static: bool,
}

/// Emit the public wrapper for callable `f`, with its KDoc, annotations,
/// and body.
pub(crate) fn emit_callable(
    w: &mut CodeWriter,
    n: &Names,
    sp: &Speller,
    f: &FnBinding,
    decl: Decl,
) {
    sp.fn_doc(
        w,
        sp.text(&f.doc),
        f.params.iter().map(|p| (p.name.as_str(), &p.doc)),
    );
    if let Some(msg) = sp.deprecation(&f.deprecated) {
        w.line(deprecated_line(&msg));
    }
    if let Some(exc) = n.thrown(&f.error) {
        w.line(format!("@Throws({exc}::class)"));
    }
    if decl.jvm_static {
        w.line("@JvmStatic");
    }
    let params: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", kt_param(&p.name), n.kt_param_type(&p.ty)))
        .collect();
    let params = params.join(", ");
    let ret = f
        .ret
        .as_ref()
        .map(|t| format!(": {}", n.kt_ret_type(t)))
        .unwrap_or_default();
    let head = format!(
        "{}fun {}",
        if decl.operator { "operator " } else { "" },
        decl.name
    );
    let (opens, mut args) = lower_args(n, f);
    let native = n.native(&f.abi.symbol);
    match (&f.shape, &f.ret_pass) {
        (CallShape::Sync, RetPass::Iterator(it)) => {
            let launch = format!("JniBridge.{native}({})", args.join(", "));
            let c = Carrier::of_item(&it.item, &it.elem);
            let body = vec![(
                0,
                format!(
                    "NativeIterator({launch}, JniBridge::{}, JniBridge::{}) {{ {} }}",
                    n.native(&it.destroy_symbol),
                    n.native(&it.next.symbol),
                    lift_erased(n, &it.elem, c, "it")
                ),
            )];
            let sig = format!("{head}({params}){ret}");
            emit_body(w, &sig, true, nest(&opens, body));
        }
        (CallShape::Sync, pass) => {
            let call = format!("JniBridge.{native}({})", args.join(", "));
            let value = f.ret.as_ref().and_then(|r| r.value());
            let body = match (Carrier::of_ret(pass, value), value) {
                (Some(c), Some(t)) => lift(n, t, c, &call),
                _ => call,
            };
            let sig = format!("{head}({params}){ret}");
            if opens.is_empty() && f.ret.is_some() {
                w.line(format!("{sig} = {body}"));
            } else {
                emit_body(w, &sig, f.ret.is_some(), nest(&opens, vec![(0, body)]));
            }
        }
        (CallShape::Async(ab), _) => {
            if ab.cancellable() {
                args.push("_token".to_string());
            }
            args.push("_done".to_string());
            let call = format!("JniBridge.{native}({})", args.join(", "));
            let value = f.ret.as_ref().and_then(|r| r.value());
            let convert = match (Carrier::of_result(&ab.result, value), value) {
                (Some(c), Some(t)) => format!("{{ {} }}", lift_erased(n, t, c, "it")),
                _ => "{ }".to_string(),
            };
            let token = if ab.cancellable() { "_token" } else { "_" };
            let mut body = vec![(
                0,
                format!(
                    "awaitNative({}, {}, {convert}) {{ {token}, _done ->",
                    ab.cancellable(),
                    n.domain_index(&f.error)
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
                &format!("suspend {head}({params}){ret}"),
                f.ret.is_some(),
                body,
            );
        }
    }
}
