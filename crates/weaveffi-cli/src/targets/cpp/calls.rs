//! Callable rendering: free functions and interface members in the sync,
//! iterator, and async call shapes, each marshalled per the passing
//! contracts the model stores ([`ArgPass`], [`RetPass`], [`ResultPass`],
//! [`ItemPass`]) and failing per its [`ErrorStrategy`].
//!
//! Interface members are rendered twice: a declaration inside the class body
//! (so the class is complete before any record that holds it by value) and an
//! out-of-line `inline` definition after every value type and codec exists.
//! Free functions are defined inline inside their module namespace, which is
//! rendered last.

use std::collections::HashSet;

use crate::codegen::common::DocCommentStyle;
use crate::codegen::docs::Doc;
use crate::codegen::CodeWriter;
use weaveffi_model::abi::AbiParam;
use weaveffi_model::model::{AsyncBinding, FnBinding, IteratorBinding, ModuleBinding};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, ItemPass, ResultPass, RetPass};
use weaveffi_model::ty::{ParamTy, Ty};

use crate::targets::cpp::types::{
    cpp_error_class, cpp_fn_name, cpp_ident, cpp_namespace_path, cpp_param_decl, cpp_ret_type,
    cpp_type, error_class, pointee, slot_decl, slot_name, Ctx,
};

// ── Receiving values ──

/// The C slots a value arrives in, whatever the position (a return, an
/// async result, an iterator item): the common shape of [`RetPass`],
/// [`ResultPass`], and [`ItemPass`], with each slot as the C++ expression
/// holding it.
pub(crate) enum Recv<'a> {
    /// A scalar, `bool`, or C-style enum.
    Direct(String),
    /// An optional scalar, `bool`, or C-style enum: presence and value.
    OptDirect(String, String),
    /// An owned typed array: pointer and element count.
    Slice(String, String),
    /// An owned UTF-8 run: pointer and byte length.
    String(String, String),
    /// An owned byte run: pointer and byte length.
    Bytes(String, String),
    /// An owned value buffer: pointer and byte length.
    Buffer(String, String),
    /// One strong object reference.
    Object {
        /// The pointer.
        ptr: String,
        /// Whether null is a legal "none".
        nullable: bool,
        /// The interface's name.
        interface: &'a str,
    },
}

/// The C++ value of a received `ty` held in C slots `recv`, adopting what
/// they own: strings, bytes, typed arrays, and buffers are copied or decoded
/// and then released, an object is adopted into its wrapper, and a C-style
/// enum converts from its discriminant.
pub(crate) fn lift(ty: &Ty, recv: Recv<'_>) -> String {
    match recv {
        Recv::Direct(v) => match ty {
            Ty::Enum(n) => format!("static_cast<{n}>({v})"),
            _ => v,
        },
        Recv::OptDirect(has, v) => {
            let inner = match ty {
                Ty::Optional(inner) => cpp_type(inner),
                other => cpp_type(other),
            };
            format!("detail::lift_optional<{inner}>({has}, {v})")
        }
        Recv::Slice(p, n) => format!("detail::take_slice({p}, {n})"),
        Recv::String(p, n) => format!("detail::take_string({p}, {n})"),
        Recv::Bytes(p, n) => format!("detail::take_bytes({p}, {n})"),
        Recv::Buffer(p, n) => format!("detail::take<{}>({p}, {n})", cpp_type(ty)),
        Recv::Object {
            ptr,
            nullable: true,
            interface,
        } => format!("detail::adopt_optional<{interface}>({ptr})"),
        Recv::Object {
            ptr,
            nullable: false,
            interface,
        } => format!("{interface}(adopt, {ptr})"),
    }
}

/// A sync return's [`Recv`]: `result` is the C return, and the out slots
/// are locals named after them.
fn ret_recv<'a>(pass: &'a RetPass, result: &str) -> Option<Recv<'a>> {
    let result = result.to_string();
    Some(match pass {
        RetPass::Void | RetPass::Iterator(_) => return None,
        RetPass::Direct => Recv::Direct(result),
        RetPass::OptDirect { out_value } => Recv::OptDirect(result, slot_name(out_value)),
        RetPass::Slice { out_len, .. } => Recv::Slice(result, slot_name(out_len)),
        RetPass::String { out_len } => Recv::String(result, slot_name(out_len)),
        RetPass::Bytes { out_len } => Recv::Bytes(result, slot_name(out_len)),
        RetPass::Buffer { out_len } => Recv::Buffer(result, slot_name(out_len)),
        RetPass::Object {
            nullable,
            interface,
            ..
        } => Recv::Object {
            ptr: result,
            nullable: *nullable,
            interface,
        },
    })
}

/// An async result's [`Recv`]: the completion's parameters.
fn result_recv(pass: &ResultPass) -> Option<Recv<'_>> {
    let s = slot_name;
    Some(match pass {
        ResultPass::Void => return None,
        ResultPass::Direct { result } => Recv::Direct(s(result)),
        ResultPass::OptDirect { has, value } => Recv::OptDirect(s(has), s(value)),
        ResultPass::Slice { ptr, len, .. } => Recv::Slice(s(ptr), s(len)),
        ResultPass::String { ptr, len } => Recv::String(s(ptr), s(len)),
        ResultPass::Bytes { ptr, len } => Recv::Bytes(s(ptr), s(len)),
        ResultPass::Buffer { ptr, len } => Recv::Buffer(s(ptr), s(len)),
        ResultPass::Object {
            result,
            nullable,
            interface,
            ..
        } => Recv::Object {
            ptr: s(result),
            nullable: *nullable,
            interface,
        },
    })
}

/// An iterator item's [`Recv`]: the out slots of `_next`, as locals named
/// after them.
fn item_recv(pass: &ItemPass) -> Recv<'_> {
    let s = slot_name;
    match pass {
        ItemPass::Direct { out_item } => Recv::Direct(s(out_item)),
        ItemPass::OptDirect { out_has, out_item } => Recv::OptDirect(s(out_has), s(out_item)),
        ItemPass::Slice {
            out_item, out_len, ..
        } => Recv::Slice(s(out_item), s(out_len)),
        ItemPass::String { out_item, out_len } => Recv::String(s(out_item), s(out_len)),
        ItemPass::Bytes { out_item, out_len } => Recv::Bytes(s(out_item), s(out_len)),
        ItemPass::Buffer { out_item, out_len } => Recv::Buffer(s(out_item), s(out_len)),
        ItemPass::Object {
            out_item,
            nullable,
            interface,
            ..
        } => Recv::Object {
            ptr: s(out_item),
            nullable: *nullable,
            interface,
        },
    }
}

/// Append a zero-initialized local for each out slot (`T* out_value` is a
/// `T out_value{}`) and return the arguments that pass their addresses.
fn out_locals(w: &mut CodeWriter, slots: &[&AbiParam], prefix: &str) -> Vec<String> {
    slots
        .iter()
        .map(|slot| {
            let name = slot_name(slot);
            w.line(format!("{} {name}{{}};", pointee(slot, prefix)));
            format!("&{name}")
        })
        .collect()
}

// ── Locals ──

/// The names a wrapper body declares, kept apart from its parameters: a
/// local gets a trailing underscore while its preferred name is taken.
struct Locals(HashSet<String>);

impl Locals {
    fn new(f: &FnBinding) -> Self {
        Self(f.params.iter().map(|p| cpp_ident(&p.name)).collect())
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
/// expressions its slots receive, per its [`ArgPass`].
///
/// * A typed array passes its vector's storage and element count; bytes
///   likewise; a string passes its UTF-8 pointer and length (no
///   terminator, so interior NULs survive).
/// * An optional scalar passes its presence and its value (zero when
///   absent).
/// * A buffered value is encoded into a local `detail::BufferWriter` that
///   outlives the call.
/// * An object is borrowed: the wrapper's pointer is passed and the wrapper
///   keeps its reference. `Interface?` passes null for none.
/// * A callback interface moves the caller's `std::shared_ptr` into a heap
///   box that becomes `ctx`; the producer deletes it through the vtable's
///   `free`. A `std::unique_ptr` owns the box until the argument list
///   releases it, so a throw while marshalling a later parameter frees it.
///   An optional callback interface passes a null vtable for none.
fn emit_param_setup(
    w: &mut CodeWriter,
    locals: &mut Locals,
    ty: &ParamTy,
    pass: &ArgPass,
    name: &str,
) -> Vec<String> {
    match pass {
        ArgPass::Direct { .. } => match ty {
            ParamTy::Value(Ty::Enum(_)) => vec![format!("static_cast<int32_t>({name})")],
            _ => vec![name.to_string()],
        },
        ArgPass::OptDirect { .. } => vec![
            format!("{name}.has_value()"),
            format!("detail::value_or_zero({name})"),
        ],
        ArgPass::Slice { .. } | ArgPass::Bytes { .. } => {
            vec![format!("{name}.data()"), format!("{name}.size()")]
        }
        ArgPass::String { .. } => vec![format!("detail::utf8({name})"), format!("{name}.size()")],
        ArgPass::Buffer { .. } => {
            let buf = locals.fresh(&format!("{name}_buf"));
            w.line(format!("const auto {buf} = detail::encode({name});"));
            vec![format!("{buf}.data()"), format!("{buf}.size()")]
        }
        ArgPass::Object { nullable: true, .. } => vec![format!("detail::handle_of({name})")],
        ArgPass::Object {
            nullable: false, ..
        } => vec![format!("{name}.handle()")],
        ArgPass::Callback {
            nullable,
            interface,
            ..
        } => {
            let ctx = locals.fresh(&format!("{name}_ctx"));
            let vtable = format!("&detail::Callbacks<{interface}>::vtable()");
            if *nullable {
                let vt = locals.fresh(&format!("{name}_vtable"));
                w.line(format!("auto {ctx} = detail::lend(std::move({name}));"));
                w.line(format!("const auto* {vt} = {ctx} ? {vtable} : nullptr;"));
                vec![format!("{ctx}.release()"), vt]
            } else {
                w.line(format!(
                    "auto {ctx} = detail::lend_required(std::move({name}), \"{name}\");"
                ));
                vec![format!("{ctx}.release()"), vtable]
            }
        }
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
        .map(|p| cpp_param_decl(&p.ty, &p.pass, &cpp_ident(&p.name)))
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
                .map(|d| format!("@param {} {d}", cpp_ident(&p.name)))
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

/// Append the definition of a callable: an `inline` free function inside its
/// module namespace (with its doc comment), or the out-of-line `inline`
/// definition of an interface member declared by [`render_member_decl`].
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
            c_args.extend(emit_param_setup(
                w,
                &mut locals,
                &p.ty,
                &p.pass,
                &cpp_ident(&p.name),
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
/// constructor adopts the returned reference.
fn emit_sync_body(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    f: &FnBinding,
    kind: FnKind<'_>,
    locals: &mut Locals,
    mut c_args: Vec<String>,
) {
    let prefix = ctx.prefix;
    let err = locals.fresh("err");
    w.line(format!("{prefix}_error {err}{{}};"));
    c_args.extend(out_locals(w, &f.ret_pass.out_slots(), prefix));
    c_args.push(format!("&{err}"));
    let call = format!("{}({})", f.abi.symbol, c_args.join(", "));
    let check = format!("detail::check<{}>({err});", error_class(&f.error));
    let ret = f.ret.as_ref().and_then(|r| r.value());
    match (kind, ret, &f.ret_pass) {
        (_, None, _) | (_, _, RetPass::Void) => {
            w.line(format!("{call};"));
            w.line(check);
        }
        (FnKind::Ctor { .. }, Some(_), _) => {
            let result = locals.fresh("result");
            w.line(format!("auto* {result} = {call};"));
            w.line(check);
            w.line(format!("raw_.reset({result});"));
        }
        (_, Some(ty), pass) => {
            let result = locals.fresh("result");
            w.line(format!("auto {result} = {call};"));
            w.line(check);
            let recv = ret_recv(pass, &result).expect("a value return has slots");
            w.line(format!("return {};", lift(ty, recv)));
        }
    }
}

// ── Iterators ──

/// Append an iterator-returning callable's body: call the launcher, check
/// its error, and wrap the producer iterator in a `Range<T>` with two
/// captureless lambdas: one pulling an element through `_next` (receiving
/// it per its [`ItemPass`] and failing per the callable's
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
    let next_err = locals.fresh("next_err");
    w.line(format!(
        "return Range<{elem}>(adopt, {iter}, [](void* {raw}, std::optional<{elem}>& {item}) {{"
    ));
    w.scope(|w| {
        w.line(format!("{prefix}_error {next_err}{{}};"));
        let mut args = vec![format!("static_cast<{tag}*>({raw})")];
        args.extend(out_locals(w, &it.item.slots(), prefix));
        args.push(format!("&{next_err}"));
        w.block(
            format!("if ({}({}) == 0) {{", it.next.symbol, args.join(", ")),
            "}",
            |w| {
                w.line(format!("detail::check<{errors}>({next_err});"));
                w.line("return false;");
            },
        );
        w.line(format!(
            "{item}.emplace({});",
            lift(&it.elem, item_recv(&it.item))
        ));
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
/// the result per its [`ResultPass`]. A cancellable call passes its token's
/// native handle (null for `CancelToken::none()`); the producer takes its
/// own reference.
fn emit_async_body(
    w: &mut CodeWriter,
    ctx: &Ctx<'_>,
    f: &FnBinding,
    a: &AsyncBinding,
    locals: &mut Locals,
    mut c_args: Vec<String>,
) {
    let value = f
        .ret
        .as_ref()
        .and_then(|r| r.value())
        .map_or("void".to_string(), cpp_type);
    let promise = locals.fresh("promise");
    let future = locals.fresh("future");
    let params: Vec<String> = a
        .callback_params
        .iter()
        .map(|p| slot_decl(p, ctx.prefix))
        .collect();
    if a.cancellable() {
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
        match (
            f.ret.as_ref().and_then(|r| r.value()),
            result_recv(&a.result),
        ) {
            (Some(ty), Some(recv)) => {
                w.line(format!(
                    "{settle}(context, err, [&] {{ return {}; }});",
                    lift(ty, recv)
                ));
            }
            _ => {
                w.line(format!("{settle}(context, err, [] {{}});"));
            }
        }
    });
    w.line(format!("}}, {promise}.release());"));
    w.line(format!("return {future};"));
}

// ── Module namespaces ──

/// Append one module's nested namespace holding its free functions
/// (`namespace kv::stats { ... }`). A module without functions emits
/// nothing; its types live at the namespace root.
pub(crate) fn render_cpp_module_ns(w: &mut CodeWriter, ctx: &Ctx<'_>, module: &ModuleBinding) {
    if module.functions.is_empty() {
        return;
    }
    let ns = cpp_namespace_path(module);
    w.line(format!("namespace {ns} {{"));
    w.blank();
    for f in &module.functions {
        render_definition(w, ctx, f, &cpp_fn_name(&f.name), FnKind::Free);
    }
    w.line(format!("}} // namespace {ns}"));
    w.blank();
}
