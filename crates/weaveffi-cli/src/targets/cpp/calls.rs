//! Callable rendering: free functions and interface members in the sync,
//! iterator, and async call shapes, each marshalled per value by its
//! [`Crossing`] and failing per its [`ErrorStrategy`].
//!
//! Interface members are rendered twice: a declaration inside the class body
//! (so the class is complete before any record that holds it by value) and an
//! out-of-line `inline` definition after every value type and codec exists.
//! Free functions are defined inline at namespace scope, rendered last.

use std::collections::HashSet;

use crate::codegen::common::DocCommentStyle;
use crate::codegen::docs::Doc;
use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use weaveffi_model::abi::{CType, Position};
use weaveffi_model::model::{AsyncBinding, ErrorStrategy, FnBinding, IteratorBinding};
use weaveffi_model::ty::{ParamTy, Ty};

use crate::targets::cpp::types::{
    cpp_error_class, cpp_param_decl, cpp_param_name, cpp_prim, cpp_ret_type, cpp_type, error_class,
    slot_decl, slot_name, Ctx,
};

// ── Receiving values ──

/// The C++ value of a `ty` the producer handed over as the C value `v` (a
/// return, an async result, or an iterator item), adopting what it owns: a
/// run is copied or decoded and then released, an object is adopted into
/// its wrapper, an optional scalar is unpacked, and a C-style enum converts
/// from its discriminant.
pub(crate) fn lift(ty: &Ty, v: &str) -> String {
    match crossing(ty) {
        Crossing::Scalar(_) => v.to_string(),
        Crossing::Enum(name) => format!("static_cast<{name}>({v})"),
        Crossing::OptScalar(p) => format!("detail::from_opt<{}>({v})", cpp_prim(p)),
        Crossing::OptEnum(name) => format!("detail::from_opt<{name}>({v})"),
        Crossing::Utf8 => format!("detail::take_string({v})"),
        Crossing::Bytes => format!("detail::take_bytes({v})"),
        Crossing::Slice(p) => format!("detail::take_slice<{}>({v})", cpp_prim(p)),
        Crossing::Buffer(ty) => format!("detail::take<{}>({v})", cpp_type(ty)),
        Crossing::Object {
            interface,
            nullable: true,
        } => format!("detail::adopt_optional<{interface}>({v})"),
        Crossing::Object {
            interface,
            nullable: false,
        } => format!("{interface}(adopt, {v})"),
    }
}

/// The C type of a `ty` the producer hands over, which a local receiving
/// it (an iterator's `out_item`) is declared with.
fn transfer_ctype(ctx: &Ctx<'_>, ty: &Ty) -> String {
    CType::of(ty, Position::Transfer).render_c(ctx.prefix)
}

// ── Locals ──

/// The names a wrapper body declares, kept apart from its parameters: a
/// local gets a trailing underscore while its preferred name is taken.
struct Locals(HashSet<String>);

impl Locals {
    fn new(f: &FnBinding) -> Self {
        Self(f.params.iter().map(|p| cpp_param_name(&p.name)).collect())
    }

    fn fresh(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        while self.0.contains(&name) {
            name.push('_');
        }
        self.0.insert(name.clone());
        name
    }
}

// ── Parameters ──

/// Append the setup statements for one parameter and return the C argument
/// expression it passes, per its [`Crossing`]:
///
/// * A string, bytes, or typed array passes a run over its own storage (a
///   string's UTF-8 without a terminator, so interior NULs survive).
/// * An optional scalar or enum passes its `{p}_opt_*` struct.
/// * A buffered value is encoded into a local `detail::BufferWriter` that
///   outlives the call, and passes a run over it.
/// * An object is borrowed: the wrapper's pointer is passed and the wrapper
///   keeps its reference. `Interface?` passes null for none.
/// * A callback interface moves the caller's `std::shared_ptr` into a heap
///   box that becomes the `{p}_{Cb}` struct's `ctx`; the producer deletes it
///   through the vtable's `free`. A `std::unique_ptr` owns the box until the
///   argument list releases it, so a throw while marshalling a later
///   parameter frees it. An optional callback interface passes a null
///   vtable for none.
fn emit_param_setup(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    locals: &mut Locals,
    ty: &ParamTy,
    name: &str,
) -> String {
    let prefix = ctx.prefix;
    let ty = match ty {
        ParamTy::Callback {
            name: iface,
            nullable,
        } => {
            let tag = &ctx.model.callback(iface).c_tag;
            let box_ = locals.fresh(&format!("{name}_ctx"));
            let vtable = format!("&detail::Callbacks<{iface}>::vtable()");
            if *nullable {
                let vt = locals.fresh(&format!("{name}_vtable"));
                w.line(format!("auto {box_} = detail::lend(std::move({name}));"));
                w.line(format!("const auto* {vt} = {box_} ? {vtable} : nullptr;"));
                return format!("{tag}{{{box_}.release(), {vt}}}");
            }
            w.line(format!(
                "auto {box_} = detail::lend_required(std::move({name}), \"{name}\");"
            ));
            return format!("{tag}{{{box_}.release(), {vtable}}}");
        }
        ParamTy::Value(ty) => ty,
    };
    match crossing(ty) {
        Crossing::Scalar(_) => name.to_string(),
        Crossing::Enum(_) => format!("static_cast<int32_t>({name})"),
        Crossing::OptScalar(p) => format!("detail::to_opt<{prefix}_opt_{}>({name})", p.snake()),
        Crossing::OptEnum(_) => format!("detail::to_opt<{prefix}_opt_i32>({name})"),
        Crossing::Utf8 => format!("detail::string_run({name})"),
        Crossing::Bytes => format!("detail::bytes_run({name})"),
        Crossing::Slice(_) => format!("detail::slice_run({name})"),
        Crossing::Buffer(_) => {
            let buf = locals.fresh(&format!("{name}_buf"));
            w.line(format!("const auto {buf} = detail::encode({name});"));
            format!("{buf}.view()")
        }
        Crossing::Object { nullable: true, .. } => format!("detail::handle_of({name})"),
        Crossing::Object {
            nullable: false, ..
        } => format!("{name}.handle()"),
    }
}

// ── Callable kinds ──

/// How a rendered callable is declared in the C++ surface.
#[derive(Clone, Copy)]
pub(crate) enum FnKind<'a> {
    /// A namespace-scope free function.
    Free,
    /// An instance method: passes the wrapped pointer as the leading C
    /// argument and is `const` (the ABI receiver is a const pointer).
    Method {
        /// The wrapper class name.
        class: &'a str,
    },
    /// A static member: interface statics and the factory form of
    /// constructors not named `new`.
    Static {
        /// The wrapper class name.
        class: &'a str,
    },
    /// The interface constructor named `new`, rendered as a C++ constructor.
    Ctor {
        /// The wrapper class name.
        class: &'a str,
    },
}

/// The C++ return type of a callable: the mapped type (or `void`) for sync,
/// `Range<T>` for an iterator, and `std::future<T>` for async. `None` for a
/// constructor.
fn return_type(f: &FnBinding, kind: FnKind<'_>) -> Option<String> {
    if matches!(kind, FnKind::Ctor { .. }) {
        return None;
    }
    let value = f.ret.as_ref().map_or("void".to_string(), cpp_ret_type);
    Some(if f.is_async() {
        format!("std::future<{value}>")
    } else {
        value
    })
}

/// The C++ parameter list of a callable. A cancellable async call takes a
/// trailing `const CancelToken&`; `with_defaults` adds its
/// `= CancelToken::none()` default, which belongs on the first declaration
/// only.
fn param_decls(f: &FnBinding, with_defaults: bool) -> String {
    let mut decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_param_name(&p.name)))
        .collect();
    if f.cancellable() {
        let default = if with_defaults {
            " = CancelToken::none()"
        } else {
            ""
        };
        decls.push(format!("const CancelToken& cancel_token{default}"));
    }
    decls.join(", ")
}

/// The `@throws` line of a callable that declares errors.
fn throws_tag(error: &ErrorStrategy) -> Option<String> {
    match error {
        ErrorStrategy::Trap => None,
        ErrorStrategy::Untyped => Some(
            "@throws Error with code -1 and the producer's message when the call fails.".into(),
        ),
        ErrorStrategy::Domain(name) => Some(format!(
            "@throws {} (or one of its codes' classes) when the call fails with one of the \
             domain's codes.",
            cpp_error_class(name)
        )),
    }
}

/// Append the doc comment and any `[[deprecated]]` attribute of a callable:
/// its IDL doc, `@param` for each documented parameter, what an iterator or
/// async call returns, and the exceptions it throws.
fn emit_callable_attrs(w: &mut CodeWriter, ctx: &Ctx<'_>, f: &FnBinding) {
    let spell = |s: &str| ctx.spell(s);
    let doc = Doc::new(&f.doc, &f.deprecated);
    let mut sections: Vec<String> = doc.text(spell).into_iter().collect();
    let mut tags: Vec<String> = f
        .params
        .iter()
        .filter_map(|p| {
            Doc::new(&p.doc, &None)
                .text(spell)
                .map(|d| format!("@param {} {d}", cpp_param_name(&p.name)))
        })
        .collect();
    if f.cancellable() {
        tags.push("@param cancel_token Cancels the call; the future then throws Cancelled.".into());
    }
    if f.iterator().is_some() {
        tags.push("@return A lazy, single-pass range; each step makes one producer call.".into());
    } else if f.is_async() {
        tags.push(
            "@return A future settled from a producer thread; get() rethrows the call's error."
                .into(),
        );
    }
    tags.extend(throws_tag(&f.error));
    if !tags.is_empty() {
        sections.push(tags.join("\n"));
    }
    let text = (!sections.is_empty()).then(|| sections.join("\n\n"));
    w.doc(&text, DocCommentStyle::Javadoc);
    if let Some(msg) = doc.deprecation(spell) {
        let escaped = msg.replace('\\', "\\\\").replace('"', "\\\"");
        w.line(format!("[[deprecated(\"{escaped}\")]]"));
    }
}

/// Append the in-class declaration of an interface member, with its doc
/// comment and deprecation marker.
pub(crate) fn render_member_decl(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
) {
    emit_callable_attrs(w, ctx, f);
    let params = param_decls(f, true);
    match (kind, return_type(f, kind)) {
        // Explicit, so no argument list converts into a new producer object
        // behind the caller's back.
        (FnKind::Ctor { class }, _) if f.params.is_empty() => w.line(format!("{class}();")),
        (FnKind::Ctor { class }, _) => w.line(format!("explicit {class}({params});")),
        (FnKind::Method { .. }, Some(ret)) => w.line(format!("{ret} {cpp_name}({params}) const;")),
        (FnKind::Static { .. }, Some(ret)) => w.line(format!("static {ret} {cpp_name}({params});")),
        _ => unreachable!("free functions are defined inline, never declared"),
    };
    w.blank();
}

/// Append the definition of a callable: an `inline` free function (with its
/// doc comment), or the out-of-line `inline` definition of an interface
/// member declared by [`render_member_decl`].
///
/// Wrappers are never `noexcept`: a call that declares no errors still
/// surfaces a producer bug as `InternalError`.
///
/// Every free function, constructor, and static member begins with
/// `check_library()`, which verifies the linked library once. Instance
/// methods skip it: their receiver could only come from a checked call.
pub(crate) fn render_definition(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
) {
    if matches!(kind, FnKind::Free) {
        emit_callable_attrs(w, ctx, f);
    }
    let params = param_decls(f, matches!(kind, FnKind::Free));
    let header = match (kind, return_type(f, kind)) {
        (FnKind::Ctor { class }, _) => format!("inline {class}::{class}({params}) {{"),
        (FnKind::Method { class }, Some(ret)) => {
            format!("inline {ret} {class}::{cpp_name}({params}) const {{")
        }
        (FnKind::Static { class }, Some(ret)) => {
            format!("inline {ret} {class}::{cpp_name}({params}) {{")
        }
        (FnKind::Free, Some(ret)) => format!("inline {ret} {cpp_name}({params}) {{"),
        _ => unreachable!("every non-constructor callable has a return type"),
    };
    w.block(header, "}", |w| {
        if !matches!(kind, FnKind::Method { .. }) {
            w.line("check_library();");
        }
        let mut locals = Locals::new(f);
        let mut c_args = Vec::new();
        if matches!(kind, FnKind::Method { .. }) {
            c_args.push("raw_.get()".to_string());
        }
        for p in &f.params {
            c_args.push(emit_param_setup(
                w,
                ctx,
                &mut locals,
                &p.ty,
                &cpp_param_name(&p.name),
            ));
        }
        if let Some(a) = f.async_binding() {
            emit_async_body(w, ctx, f, a, &mut locals, c_args);
        } else if let Some(it) = f.iterator() {
            emit_iterator_body(w, ctx, f, it, &mut locals, c_args);
        } else {
            emit_sync_body(w, ctx, f, kind, &mut locals, c_args);
        }
    });
    w.blank();
}

/// Append a synchronous callable's call, error check, and return. A
/// constructor adopts the returned reference. A failed call returns a zero
/// value, which owns nothing, so checking before lifting leaks nothing.
fn emit_sync_body(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    f: &FnBinding,
    kind: FnKind<'_>,
    locals: &mut Locals,
    mut c_args: Vec<String>,
) {
    let err = locals.fresh("err");
    w.line(format!("{}_error {err}{{}};", ctx.prefix));
    c_args.push(format!("&{err}"));
    let call = format!("{}({})", f.abi.symbol, c_args.join(", "));
    let check = format!("detail::check<{}>({err});", error_class(&f.error));
    match (kind, f.ret_value()) {
        (_, None) => {
            w.line(format!("{call};"));
            w.line(check);
        }
        (FnKind::Ctor { .. }, Some(_)) => {
            let result = locals.fresh("result");
            w.line(format!("auto* {result} = {call};"));
            w.line(check);
            w.line(format!("raw_.reset({result});"));
        }
        (_, Some(ty)) => {
            let result = locals.fresh("result");
            w.line(format!("auto {result} = {call};"));
            w.line(check);
            w.line(format!("return {};", lift(ty, &result)));
        }
    }
}

// ── Iterators ──

/// Append an iterator-returning callable's body: call the launcher, check
/// its error, and wrap the producer iterator in a `Range<T>` with two
/// captureless lambdas: one pulling an element through `_next` (receiving
/// it like a return of its type and failing per the callable's
/// [`ErrorStrategy`]), and one releasing the iterator with `_destroy`.
fn emit_iterator_body(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    f: &FnBinding,
    it: &IteratorBinding,
    locals: &mut Locals,
    mut c_args: Vec<String>,
) {
    let prefix = ctx.prefix;
    let elem = cpp_type(&it.elem);
    let tag = &it.iter_tag;
    let errors = error_class(&f.error);
    let err = locals.fresh("err");
    let iter = locals.fresh("iter");
    w.line(format!("{prefix}_error {err}{{}};"));
    c_args.push(format!("&{err}"));
    w.line(format!(
        "{tag}* {iter} = {}({});",
        f.abi.symbol,
        c_args.join(", ")
    ));
    w.line(format!("detail::check<{errors}>({err});"));
    // The lambdas are captureless, but their names stay clear of the
    // wrapper's parameters and locals so nothing shadows.
    let raw = locals.fresh("raw");
    let item = locals.fresh("item");
    let out_item = locals.fresh("out_item");
    let next_err = locals.fresh("next_err");
    w.line(format!(
        "return Range<{elem}>(adopt, {iter}, [](void* {raw}, std::optional<{elem}>& {item}) {{"
    ));
    w.scope(|w| {
        w.line(format!("{prefix}_error {next_err}{{}};"));
        w.line(format!("{} {out_item}{{}};", transfer_ctype(ctx, &it.elem)));
        w.block(
            format!(
                "if ({}(static_cast<{tag}*>({raw}), &{out_item}, &{next_err}) == 0) {{",
                it.next.symbol
            ),
            "}",
            |w| {
                w.line(format!("detail::check<{errors}>({next_err});"));
                w.line("return false;");
            },
        );
        w.line(format!("{item}.emplace({});", lift(&it.elem, &out_item)));
        w.line("return true;");
    });
    w.line(format!(
        "}}, [](void* {raw}) {{ {}(static_cast<{tag}*>({raw})); }});",
        it.destroy_symbol
    ));
}

// ── Async ──

/// Append an asynchronous callable's body: a `std::future` wrapper.
///
/// The parameters are marshalled first, then a heap-allocated promise
/// travels through the C `context`, and the completion settles it exactly
/// once through `detail::settle`, which adopts the promise and the boxed
/// error, raises the call's exception for a failure, and otherwise receives
/// the result like a return of its type. A cancellable call passes its
/// token's native handle (null for `CancelToken::none()`); the producer
/// takes its own reference.
fn emit_async_body(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    f: &FnBinding,
    a: &AsyncBinding,
    locals: &mut Locals,
    mut c_args: Vec<String>,
) {
    let value = f.ret_value().map_or("void".to_string(), cpp_type);
    let promise = locals.fresh("promise");
    let future = locals.fresh("future");
    let params: Vec<String> = a
        .callback
        .params
        .iter()
        .map(|p| slot_decl(p, ctx.prefix))
        .collect();
    let (context, err) = (
        slot_name(&a.callback.params[0]),
        slot_name(&a.callback.params[1]),
    );
    if a.cancellable {
        c_args.push("cancel_token.handle()".to_string());
    }
    w.line(format!(
        "auto {promise} = std::make_unique<std::promise<{value}>>();"
    ));
    w.line(format!("auto {future} = {promise}->get_future();"));
    c_args.push(format!("[]({}) {{", params.join(", ")));
    w.line(format!("{}({}", f.abi.symbol, c_args.join(", ")));
    w.scope(|w| {
        let settle = format!("detail::settle<{}, {value}>", error_class(&f.error));
        match (f.ret_value(), a.callback.params.get(2)) {
            (Some(ty), Some(result)) => {
                w.line(format!(
                    "{settle}({context}, {err}, [&] {{ return {}; }});",
                    lift(ty, &slot_name(result))
                ));
            }
            _ => {
                w.line(format!("{settle}({context}, {err}, [] {{}});"));
            }
        }
    });
    w.line(format!("}}, {promise}.release());"));
    w.line(format!("return {future};"));
}
