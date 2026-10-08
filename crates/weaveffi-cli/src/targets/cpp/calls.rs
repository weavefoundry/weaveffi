//! Callable rendering: free functions and interface members in the sync,
//! iterator, and async call shapes, each marshalled per the shared passing
//! plans ([`ArgPass`], [`RetPass`], `IteratorProtocol`, `AsyncProtocol`).
//!
//! Interface members are rendered twice: a declaration inside the class body
//! (so the class is complete before any record that holds it by value) and an
//! out-of-line `inline` definition after every value type and codec exists.
//! Free functions are defined inline inside their module namespace, which is
//! rendered last.

use crate::codegen::common::DocCommentStyle;
use crate::codegen::CodeWriter;
use heck::ToUpperCamelCase;
use weaveffi_model::model::{
    AbiFn, AsyncBinding, CallShape, ErrorBinding, FnBinding, IteratorBinding, ModuleBinding,
    ParamBinding,
};
use weaveffi_model::plan::{ArgPass, ErrorStrategy, RetPass};
use weaveffi_model::ty::Ty;

use crate::targets::cpp::codec::{read_fn, write_stmt};
use crate::targets::cpp::entities::{check_fn, make_error_fn};
use crate::targets::cpp::types::{
    cpp_fn_name, cpp_ident, cpp_namespace_path, cpp_param_decl, cpp_type, render_param_decls,
    slot_name, vtable_accessor,
};

// ── Error routing ──

/// The domain a callable reports typed errors from: the one in scope when
/// it's declared `throws`, else `None` (its failures are bugs).
fn throwing_domain<'a>(f: &FnBinding, error: Option<&'a ErrorBinding>) -> Option<&'a ErrorBinding> {
    match f.error_strategy() {
        ErrorStrategy::Throws => error,
        ErrorStrategy::Trap => None,
    }
}

/// The `detail::check*` helper a wrapper calls after the C call returns: the
/// domain's (throwing its typed exceptions) for a throwing callable, the
/// `InternalError` check otherwise.
fn check_helper(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    throwing_domain(f, error).map_or_else(|| "detail::check".to_string(), check_fn)
}

/// The expression an async completion uses to turn its boxed `err` into the
/// `std::exception_ptr` it settles the promise with.
fn make_error_call(f: &FnBinding, error: Option<&ErrorBinding>) -> String {
    match throwing_domain(f, error) {
        Some(eb) => format!(
            "{}(err->code, message, err->payload_ptr, err->payload_len)",
            make_error_fn(eb)
        ),
        None => "detail::make_internal_error(err->code, message)".to_string(),
    }
}

// ── Parameter and return marshalling ──

/// The wrapper class behind an object-passed type: the interface itself, or
/// the interface inside `Interface?`.
fn object_class(ty: &Ty) -> &str {
    ty.interface_name()
        .expect("object passing only applies to interfaces")
}

/// Append the setup statements for one parameter and return the C argument
/// expressions its ABI slots receive, dispatching on its [`ArgPass`].
///
/// * A string passes its UTF-8 pointer and length (no terminator, so
///   interior NULs survive); bytes likewise.
/// * A buffered value is encoded into a local `detail::BufferWriter` that
///   outlives the call.
/// * An object is borrowed: the wrapper's pointer is passed and the wrapper
///   keeps its reference. `Interface?` passes null for none.
/// * A callback interface moves the caller's `std::shared_ptr` into a heap
///   box that becomes `ctx`; the producer deletes it through the vtable's
///   `free`. A `std::unique_ptr` owns the box until the argument list
///   releases it, so a throw while marshalling a later parameter frees it.
///   An optional callback interface passes a null vtable for an empty
///   pointer.
fn emit_param_setup(w: &mut CodeWriter, p: &ParamBinding, prefix: &str) -> Vec<String> {
    let name = cpp_ident(&p.name);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            let buf = format!("{name}_buf");
            w.line(format!("detail::BufferWriter {buf};"));
            w.line(write_stmt(&p.ty, &name, &buf));
            vec![format!("{buf}.data()"), format!("{buf}.size()")]
        }
        ArgPass::String { .. } => vec![format!("detail::utf8({name})"), format!("{name}.size()")],
        ArgPass::Bytes { .. } => vec![format!("{name}.data()"), format!("{name}.size()")],
        ArgPass::Object { nullable: true, .. } => {
            vec![format!("{name}.has_value() ? {name}->handle() : nullptr")]
        }
        ArgPass::Object {
            nullable: false, ..
        } => vec![format!("{name}.handle()")],
        ArgPass::Callback { nullable, .. } => {
            let iface =
                p.ty.callback_interface_name()
                    .expect("callback passing only applies to callback interfaces");
            let vtable = format!("&detail::{}()", vtable_accessor(iface));
            let ctx = format!("{name}_ctx");
            if nullable {
                w.line(format!(
                    "const auto* {name}_vtable = {name} ? {vtable} : nullptr;"
                ));
                w.line(format!(
                    "std::unique_ptr<std::shared_ptr<{iface}>> {ctx}({name} ? new std::shared_ptr<{iface}>(std::move({name})) : nullptr);"
                ));
                vec![
                    format!("static_cast<void*>({ctx}.release())"),
                    format!("{name}_vtable"),
                ]
            } else {
                w.line(format!(
                    "if (!{name}) throw std::invalid_argument(\"{name}: null callback interface\");"
                ));
                w.line(format!(
                    "auto {ctx} = std::make_unique<std::shared_ptr<{iface}>>(std::move({name}));"
                ));
                vec![format!("static_cast<void*>({ctx}.release())"), vtable]
            }
        }
        ArgPass::Direct { slot } => match &p.ty {
            Ty::Enum(_) => vec![format!(
                "static_cast<{}>(static_cast<int32_t>({name}))",
                slot.ty.render_c(prefix)
            )],
            _ => vec![name],
        },
    }
}

/// The C++ value of a returned element held in C slots `value` (and `len`
/// for strings, bytes, and buffers), per its [`RetPass`]: strings, bytes, and
/// buffers are copied or decoded and then released, an object is adopted,
/// and a direct value converts. `Void` has no value.
fn received_value(ty: &Ty, value: &str, len: &str) -> String {
    match RetPass::of(Some(ty)) {
        RetPass::Buffer => format!("detail::take({value}, {len}, &{})", read_fn(ty)),
        RetPass::String => format!("detail::take_string({value}, {len})"),
        RetPass::Bytes => format!("detail::take_bytes({value}, {len})"),
        RetPass::Object {
            nullable: false, ..
        } => {
            format!("{}(adopt, {value})", object_class(ty))
        }
        RetPass::Object { nullable: true, .. } => {
            let class = object_class(ty);
            format!(
                "{value} != nullptr ? std::optional<{class}>(std::in_place, adopt, {value}) : std::nullopt"
            )
        }
        RetPass::Direct => match ty {
            Ty::Enum(n) => format!("static_cast<{n}>({value})"),
            _ => value.to_string(),
        },
        RetPass::Void => unreachable!("void returns carry no value"),
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

impl<'a> FnKind<'a> {
    /// The owning class, for member kinds.
    fn class(self) -> Option<&'a str> {
        match self {
            FnKind::Free => None,
            FnKind::Method { class } | FnKind::Static { class } | FnKind::Ctor { class } => {
                Some(class)
            }
        }
    }
}

/// The name of the range class an iterator-returning callable yields:
/// `{PascalName}Iterator` for a free function and
/// `{Class}{PascalName}Iterator` for an interface member.
pub(crate) fn iterator_class_name(f: &FnBinding, kind: FnKind<'_>) -> String {
    let pascal = f.name.to_upper_camel_case();
    match kind.class() {
        Some(class) => format!("{class}{pascal}Iterator"),
        None => format!("{pascal}Iterator"),
    }
}

/// The C++ return type of a callable: the mapped type (or `void`) for sync,
/// `std::future<T>` for async, and the range class for an iterator. `None`
/// for a constructor.
fn return_type(f: &FnBinding, kind: FnKind<'_>) -> Option<String> {
    if matches!(kind, FnKind::Ctor { .. }) {
        return None;
    }
    let value = || f.ret.as_ref().map_or("void".to_string(), cpp_type);
    Some(match &f.shape {
        CallShape::Sync(_) => value(),
        CallShape::Async(_) => format!("std::future<{}>", value()),
        CallShape::Iterator(_) => iterator_class_name(f, kind),
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
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name)))
        .collect();
    if f.is_async() && f.cancellable {
        let default = if with_defaults {
            " = CancelToken::none()"
        } else {
            ""
        };
        decls.push(format!("const CancelToken& cancel_token{default}"));
    }
    decls.join(", ")
}

/// Append the doc comment and any `[[deprecated]]` attribute of a callable:
/// its IDL doc, `@param` for each documented parameter, what an iterator or
/// async call returns, and the exceptions it throws.
fn emit_callable_attrs(
    w: &mut CodeWriter,
    f: &FnBinding,
    kind: FnKind<'_>,
    error: Option<&ErrorBinding>,
) {
    let mut sections: Vec<String> = f.doc.iter().cloned().collect();
    let mut tags: Vec<String> = f
        .params
        .iter()
        .filter_map(|p| {
            p.doc
                .as_ref()
                .map(|d| format!("@param {} {d}", cpp_ident(&p.name)))
        })
        .collect();
    if f.is_async() && f.cancellable {
        tags.push("@param cancel_token Cancels the call; the future then throws Cancelled.".into());
    }
    match &f.shape {
        CallShape::Iterator(_) => tags.push(format!(
            "@return A lazy `{}` range that pulls one element per step and releases the \
             producer iterator when exhausted or destroyed.",
            iterator_class_name(f, kind)
        )),
        CallShape::Async(_) => tags.push(
            "@return A future settled from a producer thread; get() rethrows the call's error."
                .into(),
        ),
        CallShape::Sync(_) => {}
    }
    if let Some(eb) = throwing_domain(f, error) {
        tags.push(format!(
            "@throws {} when the call fails with one of the domain's codes.",
            eb.type_name
        ));
    }
    if !tags.is_empty() {
        sections.push(tags.join("\n"));
    }
    let doc = (!sections.is_empty()).then(|| sections.join("\n\n"));
    w.doc(&doc, DocCommentStyle::Javadoc);
    if let Some(msg) = &f.deprecated {
        w.line(format!("[[deprecated(\"{}\")]]", msg.replace('"', "\\\"")));
    }
}

/// Append the in-class declaration of an interface member, with its doc
/// comment and deprecation marker.
pub(crate) fn render_member_decl(
    w: &mut CodeWriter,
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    error: Option<&ErrorBinding>,
) {
    emit_callable_attrs(w, f, kind, error);
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
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    if matches!(kind, FnKind::Free) {
        emit_callable_attrs(w, f, kind, error);
    }
    let params = param_decls(f, matches!(kind, FnKind::Free));
    let header = match (kind, return_type(f, kind)) {
        // `raw_` starts null so a throw from the error check leaves
        // nothing for the destructor to release.
        (FnKind::Ctor { class }, _) => {
            format!("inline {class}::{class}({params}) : raw_(nullptr) {{")
        }
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
        let mut c_args = Vec::new();
        if matches!(kind, FnKind::Method { .. }) {
            c_args.push("raw_".to_string());
        }
        for p in &f.params {
            c_args.extend(emit_param_setup(w, p, prefix));
        }
        match &f.shape {
            CallShape::Sync(abi) => emit_sync_body(w, f, abi, kind, c_args, error, prefix),
            CallShape::Iterator(it) => {
                emit_iterator_launch_body(w, f, it, kind, c_args, error, prefix);
            }
            CallShape::Async(a) => emit_async_body(w, f, a, c_args, error, prefix),
        }
    });
    w.blank();
}

/// Append a synchronous callable's call, error check, and return. A
/// constructor stores the returned reference in `raw_`.
fn emit_sync_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    abi: &AbiFn,
    kind: FnKind<'_>,
    mut c_args: Vec<String>,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    // A string, bytes, or buffered return carries a trailing `size_t* out_len`.
    let has_out_len = matches!(
        RetPass::of(f.ret.as_ref()),
        RetPass::Buffer | RetPass::String | RetPass::Bytes
    );
    if has_out_len {
        w.line("size_t out_len = 0;");
        c_args.push("&out_len".into());
    }
    c_args.push("&err".into());
    w.line(format!("{prefix}_error err{{}};"));
    let call = format!("{}({})", abi.symbol, c_args.join(", "));
    if f.ret.is_none() {
        w.line(format!("{call};"));
    } else {
        w.line(format!("auto result = {call};"));
    }
    w.line(format!("{}(err);", check_helper(f, error)));
    match (kind, &f.ret) {
        (FnKind::Ctor { .. }, _) => {
            w.line("raw_ = result;");
        }
        (_, Some(ret)) => {
            w.line(format!(
                "return {};",
                received_value(ret, "result", "out_len")
            ));
        }
        (_, None) => {}
    }
}

// ── Iterators ──

/// Append the lazy range class an iterator-returning callable yields.
///
/// The range is move-only, owns the producer iterator, and pulls exactly one
/// element per step (`weaveffi_model::plan::IteratorProtocol`):
///
/// * `begin()`/`end()` expose a single-pass input iterator with a sentinel
///   end, so `for (auto&& item : fn())` streams in constant memory.
/// * Each element is received per its family (strings, bytes, and buffers
///   copied or decoded then released; objects adopted).
/// * `_destroy` runs exactly once: eagerly on exhaustion or a `next` error,
///   from the destructor otherwise.
/// * Errors follow the callable's [`ErrorStrategy`].
pub(crate) fn render_iterator_range(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    let elem = cpp_type(&it.elem);
    let class = iterator_class_name(f, kind);
    let tag = &it.iter_tag;
    let destroy = &it.destroy_symbol;
    let owner = match kind.class() {
        Some(c) => format!("{c}::{cpp_name}()"),
        None => format!("{cpp_name}()"),
    };

    w.doc(
        &Some(format!(
            "A lazy, move-only range over the `{elem}` elements `{owner}` produces.\n\n\
             Each step pulls one element from the producer, so results stream in \
             constant memory. The range releases the producer iterator exactly once: \
             when it's exhausted, or from the destructor when iteration stops early."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("class {class} {{"));
    w.scope(|w| {
        w.line(format!("{tag}* handle_;"));
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        w.line("/** Adopts a producer iterator. */");
        w.line(format!(
            "explicit {class}(adopt_t, {tag}* h) noexcept : handle_(h) {{}}"
        ));
        w.blank();
        w.line("/** Releases the producer iterator if it's still held. */");
        w.block(format!("~{class}() {{"), "}", |w| {
            w.line(format!("if (handle_ != nullptr) {destroy}(handle_);"));
        });
        w.blank();
        w.line(format!("{class}(const {class}&) = delete;"));
        w.line(format!("{class}& operator=(const {class}&) = delete;"));
        w.blank();
        w.line("/** Transfers `other`'s iterator; `other` becomes empty. */");
        w.line(format!(
            "{class}({class}&& other) noexcept : handle_(other.handle_) {{ other.handle_ = nullptr; }}"
        ));
        w.blank();
        w.line("/** Releases the current iterator and takes over `other`'s. */");
        w.block(
            format!("{class}& operator=({class}&& other) noexcept {{"),
            "}",
            |w| {
                w.block("if (this != &other) {", "}", |w| {
                    w.line(format!("if (handle_ != nullptr) {destroy}(handle_);"));
                    w.line("handle_ = other.handle_;");
                    w.line("other.handle_ = nullptr;");
                });
                w.line("return *this;");
            },
        );
        w.blank();
        render_iterator_next(w, f, it, error, prefix);
        w.line("/** Sentinel type marking the end of the range. */");
        w.line("struct sentinel {};");
        w.blank();
        w.line("/** Single-pass input iterator; each increment pulls one element. */");
        w.line("class iterator {");
        w.scope(|w| {
            w.line(format!("{class}* range_;"));
            w.line(format!("std::optional<{elem}> current_;"));
            w.blank();
        });
        w.line("public:");
        w.scope(|w| {
            w.line("using iterator_category = std::input_iterator_tag;");
            w.line(format!("using value_type = {elem};"));
            w.line("using difference_type = std::ptrdiff_t;");
            w.line(format!("using pointer = {elem}*;"));
            w.line(format!("using reference = {elem}&;"));
            w.blank();
            w.line("/** Binds to `range` and pulls the first element. */");
            w.line(format!(
                "explicit iterator({class}* range) : range_(range), current_(range->next()) {{}}"
            ));
            w.line("reference operator*() { return *current_; }");
            w.line("pointer operator->() { return &*current_; }");
            w.line("iterator& operator++() { current_ = range_->next(); return *this; }");
            w.line("void operator++(int) { current_ = range_->next(); }");
            w.line("bool operator==(sentinel) const { return !current_.has_value(); }");
            w.line("bool operator!=(sentinel) const { return current_.has_value(); }");
        });
        w.line("};");
        w.blank();
        w.line("/** Begins iteration by pulling the first element. */");
        w.line("iterator begin() { return iterator(this); }");
        w.blank();
        w.line("/** The past-the-end sentinel. */");
        w.line("sentinel end() const { return sentinel{}; }");
    });
    w.line("};");
    w.blank();
}

/// Append an iterator-returning callable's launch: call the launcher, check
/// `out_err`, and wrap the returned pointer in the range class.
fn emit_iterator_launch_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    kind: FnKind<'_>,
    mut c_args: Vec<String>,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    c_args.push("&err".into());
    w.line(format!("{prefix}_error err{{}};"));
    w.line(format!(
        "{}* iter = {}({});",
        it.iter_tag,
        it.launch.symbol,
        c_args.join(", ")
    ));
    w.line(format!("{}(err);", check_helper(f, error)));
    w.line(format!(
        "return {}(adopt, iter);",
        iterator_class_name(f, kind)
    ));
}

/// Append the range class's `next()`: one producer `_next` call yielding the
/// received element, or `std::nullopt` on exhaustion, destroying the
/// iterator exactly once on exhaustion or error.
fn render_iterator_next(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    let elem = cpp_type(&it.elem);
    let destroy = &it.destroy_symbol;
    let item_ty = it.item_ctype().render_c(prefix);
    let raises = match throwing_domain(f, error) {
        Some(eb) => eb.type_name.clone(),
        None => "InternalError".to_string(),
    };
    w.doc(
        &Some(format!(
            "Pulls the next element, or `std::nullopt` once exhausted (which releases \
             the producer iterator). A producer error releases the iterator and throws \
             {raises}."
        )),
        DocCommentStyle::Javadoc,
    );
    w.block(format!("std::optional<{elem}> next() {{"), "}", |w| {
        w.line("if (handle_ == nullptr) return std::nullopt;");
        w.line(format!("{prefix}_error err{{}};"));
        w.line(format!("{item_ty} item{{}};"));
        let mut args = vec!["handle_".to_string(), "&item".to_string()];
        if matches!(
            RetPass::of(Some(&it.elem)),
            RetPass::Buffer | RetPass::String | RetPass::Bytes
        ) {
            w.line("size_t item_len = 0;");
            args.push("&item_len".to_string());
        }
        args.push("&err".to_string());
        w.line(format!(
            "int32_t has_item = {}({});",
            it.next.symbol,
            args.join(", ")
        ));
        w.block("if (err.code != 0 || has_item == 0) {", "}", |w| {
            w.line(format!("{destroy}(handle_);"));
            w.line("handle_ = nullptr;");
            w.line(format!("{}(err);", check_helper(f, error)));
            w.line("return std::nullopt;");
        });
        // A nullable object element yields an engaged outer optional holding
        // an empty inner one for null, which is distinct from exhaustion.
        w.line(format!(
            "return std::optional<{elem}>(std::in_place, {});",
            received_value(&it.elem, "item", "item_len")
        ));
    });
    w.blank();
}

// ── Async ──

/// Append an asynchronous callable's body: a `std::future` wrapper.
///
/// The parameters are marshalled first, then a heap-allocated promise
/// travels through the C `context` and the completion adopts it back into a
/// `std::unique_ptr`, settling it exactly once. A failure settles the
/// promise with the typed domain exception (a throwing call), `Cancelled`
/// for -5, or `InternalError`. The completion owns everything it receives:
/// it releases the boxed error and any string or buffer result after copying
/// and adopts an object result. A cancellable call passes its token's native
/// handle (null for `CancelToken::none()`); the producer takes its own
/// reference.
fn emit_async_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    a: &AsyncBinding,
    mut c_args: Vec<String>,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    let value = f.ret.as_ref().map_or("void".to_string(), cpp_type);
    let promise = format!("std::promise<{value}>");
    let cb_params = render_param_decls(&a.callback_params, prefix).join(", ");
    if f.cancellable {
        c_args.push("cancel_token.handle()".to_string());
    }
    w.line(format!("auto promise = std::make_unique<{promise}>();"));
    w.line("auto future = promise->get_future();");
    c_args.push(format!("[]({cb_params}) {{"));
    w.line(format!("{}({}", a.launch.symbol, c_args.join(", ")));
    w.scope(|w| {
        w.line(format!(
            "std::unique_ptr<{promise}> p(static_cast<{promise}*>(context));"
        ));
        // The completion runs on a producer thread inside a C frame, so a
        // decode failure (of the result or an error payload) settles the
        // promise instead of unwinding.
        w.line("try {");
        w.scope(|w| {
            w.line("if (err != nullptr && err->code != 0) {");
            w.scope(|w| {
                w.line("std::string message = detail::error_message(*err);");
                w.line(format!("p->set_exception({});", make_error_call(f, error)));
            });
            w.line("} else {");
            w.scope(|w| match &f.ret {
                Some(ret) => {
                    let slots = &a.callback_params[2..];
                    let slot = |i: usize| slots.get(i).map(slot_name).unwrap_or_default();
                    w.line(format!(
                        "p->set_value({});",
                        received_value(ret, &slot(0), &slot(1))
                    ));
                }
                None => {
                    w.line("p->set_value();");
                }
            });
            w.line("}");
        });
        w.line("} catch (...) {");
        w.scope(|w| {
            w.line("p->set_exception(std::current_exception());");
        });
        w.line("}");
        w.line(format!("{prefix}_error_free(err);"));
    });
    w.line("}, static_cast<void*>(promise.release()));");
    w.line("return future;");
}

// ── Module namespaces ──

/// Append one module's nested namespace holding its free functions
/// (`namespace kv::stats { ... }`). An iterator-returning function is
/// preceded by its range class. A module without functions emits nothing;
/// its types live at the namespace root.
pub(crate) fn render_cpp_module_ns(
    w: &mut CodeWriter,
    module: &ModuleBinding,
    error: Option<&ErrorBinding>,
    prefix: &str,
) {
    if module.functions.is_empty() {
        return;
    }
    let ns = cpp_namespace_path(module);
    w.line(format!("namespace {ns} {{"));
    w.blank();
    for f in &module.functions {
        let name = cpp_fn_name(&f.name);
        if let CallShape::Iterator(it) = &f.shape {
            render_iterator_range(w, f, it, &name, FnKind::Free, error, prefix);
        }
        render_definition(w, f, &name, FnKind::Free, error, prefix);
    }
    w.line(format!("}} // namespace {ns}"));
    w.blank();
}
