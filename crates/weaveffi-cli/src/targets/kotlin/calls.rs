//! The call layer: how every value lowers to its one C value and lifts back
//! (dispatched on [`Crossing`]), the JNA type each C value crosses as, and
//! the public Kotlin wrapper of every callable, with the borrow scopes that
//! keep every object (receiver and arguments) alive and unreleased for the
//! whole native call.

use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use weaveffi_model::abi::CType;
use weaveffi_model::model::{FnBinding, Shape};
use weaveffi_model::ty::{ParamTy, Prim, Ty};

use crate::targets::kotlin::codec::{decode_expr, encode_expr};
use crate::targets::kotlin::docs::Speller;
use crate::targets::kotlin::names::{jna_prim, kt_escape, kt_param, Names};
use crate::targets::kotlin::runtime::{opt_class, opt_fn};

/// `expr` in receiver position: as is when it's a postfix chain (a name,
/// member, call, call with a trailing lambda, or index, with no other space
/// outside its brackets), else parenthesized.
pub(crate) fn receiver(expr: &str) -> String {
    let mut depth = 0usize;
    for (i, c) in expr.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 && !expr[i..].trim_start().starts_with('{') => {
                return format!("({expr})");
            }
            _ => {}
        }
    }
    expr.to_string()
}

/// Where a C value comes from or goes to, which decides who owns a run.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flow {
    /// A callable's parameter (lowered; the library borrows runs) or its
    /// return, async result, or iterator item (lifted; the bindings release
    /// runs).
    Call,
    /// A callback method's argument (lifted; runs are borrowed) or return
    /// (lowered; the library adopts runs, allocated with `{p}_alloc`).
    Callback,
}

/// The Kotlin type a C value crosses JNA as. In a callback (`callback`),
/// the error slot is a raw `Pointer?` the dispatch reports through.
pub(crate) fn jna_type(ct: &CType, callback: bool) -> String {
    match ct {
        CType::Scalar(p) => jna_prim(*p).to_string(),
        CType::Size => "SizeT".to_string(),
        CType::Void => "Unit".to_string(),
        CType::Enum(_) => "Int".to_string(),
        CType::Opt(p) => format!("{}.ByValue", opt_class(*p)),
        CType::Run => "Run.ByValue".to_string(),
        CType::Callback(_) => "CallbackArg".to_string(),
        CType::Ptr { pointee, .. } => match pointee.as_ref() {
            CType::Error if !callback => "NativeError?".to_string(),
            // An iterator's out item.
            CType::Run => "Run".to_string(),
            CType::Opt(p) => opt_class(*p),
            CType::Scalar(p) => format!("{}ByReference", jna_prim(*p)),
            CType::Enum(_) => "IntByReference".to_string(),
            CType::Ptr { .. } => "PointerByReference".to_string(),
            _ => "Pointer?".to_string(),
        },
        CType::Named(_)
        | CType::Object(_)
        | CType::Vtable(_)
        | CType::Error
        | CType::CancelToken => {
            unreachable!("{ct:?} never crosses by value")
        }
    }
}

/// The completion callback type stem for an async result's C type (`Run`,
/// `OptI64`, `Object`, `Void`).
pub(crate) fn completion_stem(result: Option<&CType>) -> String {
    match result {
        None => "Void".to_string(),
        Some(CType::Scalar(p)) => p.pascal().to_string(),
        Some(CType::Enum(_)) => "I32".to_string(),
        Some(CType::Opt(p)) => opt_class(*p),
        Some(CType::Run) => "Run".to_string(),
        Some(_) => "Object".to_string(),
    }
}

/// The `Completions` member holding the completion of an async result of
/// C type `result` (`runDone`).
pub(crate) fn completion_value(result: Option<&CType>) -> String {
    let stem = completion_stem(result);
    let mut chars = stem.chars();
    let first = chars
        .next()
        .map(|c| c.to_ascii_lowercase())
        .unwrap_or_default();
    format!("{first}{}Done", chars.as_str())
}

/// The result slot of an async callable's completion.
pub(crate) fn completion_result(f: &FnBinding) -> Option<&CType> {
    let a = f.async_binding()?;
    a.callback.params.get(2).map(|p| &p.ty)
}

/// The primitive-array stem of a typed-array element (`Int` for `IntArray`).
fn array_stem(p: Prim) -> &'static str {
    match p {
        Prim::I8 | Prim::U8 => "Byte",
        Prim::I16 | Prim::U16 => "Short",
        Prim::I32 | Prim::U32 => "Int",
        Prim::I64 | Prim::U64 => "Long",
        Prim::F32 => "Float",
        Prim::F64 => "Double",
        _ => unreachable!("{p} is not a typed-array element"),
    }
}

/// The `Run` reader of a typed array's elements (`ints`).
fn run_reader(p: Prim) -> &'static str {
    match p {
        Prim::I8 | Prim::U8 => "bytes",
        Prim::I16 | Prim::U16 => "shorts",
        Prim::I32 | Prim::U32 => "ints",
        Prim::I64 | Prim::U64 => "longs",
        Prim::F32 => "floats",
        Prim::F64 => "doubles",
        _ => unreachable!("{p} is not a typed-array element"),
    }
}

/// The conversion of a scalar's public value to its JNA carrier.
fn lower_scalar(p: Prim, expr: &str) -> String {
    let r = receiver(expr);
    match p {
        Prim::Bool => format!("{r}.asByte()"),
        Prim::U8 => format!("{r}.toByte()"),
        Prim::U16 => format!("{r}.toShort()"),
        Prim::U32 => format!("{r}.toInt()"),
        Prim::U64 => format!("{r}.toLong()"),
        _ => expr.to_string(),
    }
}

/// The conversion of a scalar's JNA carrier to its public value.
fn lift_scalar(p: Prim, expr: &str) -> String {
    let r = receiver(expr);
    match p {
        Prim::Bool => format!("{r}.asBoolean()"),
        Prim::U8 => format!("{r}.toUByte()"),
        Prim::U16 => format!("{r}.toUShort()"),
        Prim::U32 => format!("{r}.toUInt()"),
        Prim::U64 => format!("{r}.toULong()"),
        _ => expr.to_string(),
    }
}

/// The Kotlin expression lowering the public value `expr` of `t` to its C
/// value. A run is staged for the library to borrow ([`Flow::Call`]) or
/// allocated with `{p}_alloc` for it to adopt ([`Flow::Callback`]), and an
/// object returned from a callback is a fresh strong reference.
///
/// # Panics
///
/// Panics for an object parameter of a call, which borrows the wrapper's
/// reference instead (see [`emit_callable`]).
pub(crate) fn lower(n: &Names, t: &Ty, flow: Flow, expr: &str) -> String {
    let run = match flow {
        Flow::Call => "runOf",
        Flow::Callback => "allocRun",
    };
    let r = receiver(expr);
    match crossing(t) {
        Crossing::Scalar(p) => lower_scalar(p, expr),
        Crossing::Enum(_) => format!("{r}.value"),
        Crossing::OptScalar(p) => format!("{}({expr})", opt_fn(p)),
        Crossing::OptEnum(_) => format!("{}({r}?.value)", opt_fn(Prim::I32)),
        Crossing::Utf8 => format!("{run}(encodeUtf8({expr}))"),
        Crossing::Bytes => format!("{run}({expr})"),
        Crossing::Slice(p) => {
            let bits = matches!(p, Prim::U16 | Prim::U32 | Prim::U64);
            let suffix = if bits { "ArrayBits" } else { "Array" };
            format!("{run}({r}.to{}{suffix}())", array_stem(p))
        }
        Crossing::Buffer(ty) => format!("{run}({})", encode_expr(n, ty, expr)),
        Crossing::Object { nullable, .. } => {
            assert!(
                flow == Flow::Callback,
                "a call borrows its object arguments"
            );
            if nullable {
                format!("{r}?.cloneHandle()")
            } else {
                format!("{r}.cloneHandle()")
            }
        }
    }
}

/// The Kotlin expression lifting `expr`, the C value of `t`, into its public
/// value. A run the call returned ([`Flow::Call`]) is read and released; a
/// callback argument's ([`Flow::Callback`]) is borrowed. An object is
/// adopted: the wrapper owes the reference's release.
pub(crate) fn lift(n: &Names, t: &Ty, flow: Flow, expr: &str) -> String {
    let read = |body: &dyn Fn(&str) -> String| match flow {
        Flow::Call => format!("take({expr}) {{ _run -> {} }}", body("_run")),
        Flow::Callback => body(&receiver(expr)),
    };
    match crossing(t) {
        Crossing::Scalar(p) => lift_scalar(p, expr),
        Crossing::Enum(name) => format!("{}.fromValue({expr})", n.ty(name)),
        Crossing::OptScalar(_) => format!("{}.get()", receiver(expr)),
        Crossing::OptEnum(name) => format!(
            "{}.get()?.let {{ _v -> {}.fromValue(_v) }}",
            receiver(expr),
            n.ty(name)
        ),
        Crossing::Utf8 => read(&|r| format!("{r}.string()")),
        Crossing::Bytes => read(&|r| format!("{r}.bytes()")),
        Crossing::Slice(p) => {
            let list = match p {
                Prim::U16 => "toUShortList()",
                Prim::U32 => "toUIntList()",
                Prim::U64 => "toULongList()",
                _ => "asList()",
            };
            read(&|r| format!("{r}.{}().{list}", run_reader(p)))
        }
        Crossing::Buffer(ty) => read(&|r| decode_expr(n, ty, r)),
        Crossing::Object {
            interface,
            nullable,
        } => {
            let class = n.ty(interface);
            if nullable {
                format!("{class}.fromHandleOrNull({expr})")
            } else {
                format!("{class}.fromHandle({expr})")
            }
        }
    }
}

/// The JNA carrier class an `Any?` holding the C value of `t` (an async
/// result) is cast to.
fn carrier(t: &Ty) -> String {
    match crossing(t) {
        Crossing::Scalar(p) => jna_prim(p).to_string(),
        Crossing::Enum(_) => "Int".to_string(),
        Crossing::OptScalar(p) => opt_class(p),
        Crossing::OptEnum(_) => opt_class(Prim::I32),
        Crossing::Object { .. } => "Pointer?".to_string(),
        _ => "Run".to_string(),
    }
}

/// The out-item holder an iterator's `_next` fills for an element of `t`,
/// and the expression reading the element from it.
fn item_holder(t: &Ty) -> (String, &'static str) {
    match crossing(t) {
        Crossing::Scalar(p) => (format!("{}ByReference()", jna_prim(p)), "_item.value"),
        Crossing::Enum(_) => ("IntByReference()".to_string(), "_item.value"),
        Crossing::OptScalar(p) => (format!("{}()", opt_class(p)), "_item"),
        Crossing::OptEnum(_) => (format!("{}()", opt_class(Prim::I32)), "_item"),
        Crossing::Object { .. } => ("PointerByReference()".to_string(), "_item.value"),
        _ => ("Run()".to_string(), "_item"),
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
    /// Whether to annotate it `@JvmStatic` (library object and companion
    /// functions, so Java sees static methods).
    pub jvm_static: bool,
}

/// The pieces of one call: the locals staged before it, the borrow scopes
/// around it, and its C arguments (without the error slot or, for an async
/// call, the token, completion, and context).
struct Args {
    locals: Vec<String>,
    opens: Vec<String>,
    args: Vec<String>,
}

/// Lower every argument of `f`. Runs and optional scalars are staged into
/// locals first (`key_` for `key`: generated temporaries start with `_`, so
/// neither collides with a parameter), the receiver and every object argument open one `borrow`
/// scope each, and a callback interface registers its implementation inline
/// in the call, after everything that can fail, so a failure before the call
/// never strands a registered implementation.
fn lower_args(n: &Names, f: &FnBinding) -> Args {
    let mut out = Args {
        locals: Vec::new(),
        opens: Vec::new(),
        args: Vec::new(),
    };
    if f.has_self() {
        out.opens.push("handle.borrow { _self ->".to_string());
        out.args.push("_self".to_string());
    }
    for (i, p) in f.params.iter().enumerate() {
        let name = kt_param(&p.name);
        let t = match &p.ty {
            ParamTy::Callback { name: cb, nullable } => {
                let vtable = n.vtable_named(cb);
                out.args.push(if *nullable {
                    format!("{vtable}.argOrNone({name})")
                } else {
                    format!("{vtable}.arg({name})")
                });
                continue;
            }
            ParamTy::Value(t) => t,
        };
        match crossing(t) {
            Crossing::Object { nullable, .. } => {
                out.opens.push(if nullable {
                    format!("{name}?.handle.borrowOrNull {{ _h{i} ->")
                } else {
                    format!("{name}.handle.borrow {{ _h{i} ->")
                });
                out.args.push(format!("_h{i}"));
            }
            Crossing::Scalar(_) | Crossing::Enum(_) => {
                out.args.push(lower(n, t, Flow::Call, &name));
            }
            _ => {
                out.locals
                    .push(format!("val {name}_ = {}", lower(n, t, Flow::Call, &name)));
                out.args.push(format!("{name}_"));
            }
        }
    }
    out
}

/// Write `body` (lines relative to depth 0) inside the borrow scopes
/// `opens`, one level deeper per scope.
fn nested(w: &mut CodeWriter, opens: &[String], body: &[String], first_prefix: &str) {
    for (d, open) in opens.iter().enumerate() {
        let open = if d == 0 {
            format!("{first_prefix}{open}")
        } else {
            open.clone()
        };
        w.line(open);
        w.indent();
    }
    for (i, line) in body.iter().enumerate() {
        if opens.is_empty() && i == 0 {
            w.line(format!("{first_prefix}{line}"));
        } else {
            w.line(line);
        }
    }
    for _ in opens {
        w.dedent();
        w.line("}");
    }
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
    let ret = f
        .ret
        .as_ref()
        .map(|t| format!(": {}", n.kt_ret_type(t)))
        .unwrap_or_default();
    let suspend = if f.is_async() { "suspend " } else { "" };
    let operator = if decl.operator { "operator " } else { "" };
    let sig = format!(
        "{suspend}{operator}fun {}({}){ret}",
        decl.name,
        params.join(", ")
    );
    let domain = n.domain_index(&f.error);
    let symbol = &f.abi.symbol;
    let Args {
        locals,
        opens,
        mut args,
    } = lower_args(n, f);
    let returns = f.ret.is_some();
    let body: Vec<String> = match &f.shape {
        Shape::Sync => {
            args.push("_err".to_string());
            let call = format!(
                "callNative({domain}) {{ _err -> Lib.c.{symbol}({}) }}",
                args.join(", ")
            );
            vec![match f.ret_value() {
                Some(t) => lift(n, t, Flow::Call, &call),
                None => call,
            }]
        }
        Shape::Iterator(it) => {
            args.push("_err".to_string());
            let elem = n.kt_type(&it.elem);
            let (holder, item) = item_holder(&it.elem);
            vec![
                format!("NativeSequence<{elem}>("),
                format!(
                    "    nonNull(callNative({domain}) {{ _err -> Lib.c.{symbol}({}) }}),",
                    args.join(", ")
                ),
                format!("    Lib.c::{},", it.destroy_symbol),
                ") { _iter ->".to_string(),
                format!("    val _item = {holder}"),
                format!(
                    "    if (callNative({domain}) {{ _err -> Lib.c.{}(_iter, _item, _err) }} == 0) {{",
                    it.next.symbol
                ),
                "        NativeSequence.DONE".to_string(),
                "    } else {".to_string(),
                format!("        {}", lift(n, &it.elem, Flow::Call, item)),
                "    }".to_string(),
                "}".to_string(),
            ]
        }
        Shape::Async(a) => {
            if a.cancellable {
                args.push("_token".to_string());
            }
            let result = completion_result(f);
            args.push(format!("Completions.{}", completion_value(result)));
            args.push("_context".to_string());
            let lift_fn = match f.ret_value() {
                Some(t) => format!(
                    "{{ _raw -> {} }}",
                    lift(n, t, Flow::Call, &format!("_raw as {}", carrier(t)))
                ),
                None => "{ }".to_string(),
            };
            let token = if a.cancellable { "_token" } else { "_" };
            let mut inner = CodeWriter::four_space();
            nested(
                &mut inner,
                &opens,
                &[format!("Lib.c.{symbol}({})", args.join(", "))],
                "",
            );
            let mut lines = vec![format!(
                "awaitNative({}, {domain}, {lift_fn}) {{ {token}, _context ->",
                a.cancellable
            )];
            lines.extend(inner.finish().lines().map(|l| format!("    {l}")));
            lines.push("}".to_string());
            // The borrows live inside the launch, so none spans a suspension.
            return emit_body(w, &sig, returns, &locals, &[], &lines);
        }
    };
    emit_body(w, &sig, returns, &locals, &opens, &body);
}

/// Emit a function `sig` whose body stages `locals`, then evaluates `body`
/// inside the borrow scopes `opens`: an expression body when it has neither
/// locals nor scopes and returns a value, a block body otherwise.
fn emit_body(
    w: &mut CodeWriter,
    sig: &str,
    returns: bool,
    locals: &[String],
    opens: &[String],
    body: &[String],
) {
    if returns && locals.is_empty() && opens.is_empty() {
        w.line(format!("{sig} = {}", body[0]));
        for line in &body[1..] {
            w.line(line);
        }
        return;
    }
    w.block(format!("{sig} {{"), "}", |w| {
        for local in locals {
            w.line(local);
        }
        nested(w, opens, body, if returns { "return " } else { "" });
    });
}

/// The Kotlin spelling of a C slot name (a JNA interface parameter).
pub(crate) fn slot_name(name: &str) -> String {
    kt_escape(name)
}
