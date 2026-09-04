//! Callable rendering: free functions and interface members in the sync,
//! iterator, and async call shapes, each marshalled per the shared passing
//! plans ([`ArgPass`], [`RetPass`], `IteratorProtocol`, `AsyncProtocol`).
//!
//! Interface members are rendered twice: a declaration inside the class body
//! (so the class is complete before any record that holds it by value) and an
//! out-of-line `inline` definition after every value type and codec exists.
//! Free functions are defined inline inside their module namespace, which is
//! rendered last.

use heck::ToUpperCamelCase;
use weaveffi_core::abi;
use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    AbiFn, AsyncBinding, CallShape, FnBinding, IteratorBinding, ModuleBinding, ParamBinding,
};
use weaveffi_core::plan::{ret_pass, ArgPass, ErrorStrategy, RetPass};
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};

use crate::codec::{emit_read_decl, emit_write_value};
use crate::types::{
    cpp_fn_name, cpp_ident, cpp_namespace_path, cpp_param_decl, cpp_type, render_param_decls,
    vtable_accessor,
};

// ── Error routing ──

/// The `detail::check*` helper a wrapper calls after the C call returns,
/// selected by the callable's [`ErrorStrategy`]: the per-domain variant
/// (throwing the typed exception) for [`ErrorStrategy::Throws`] in a module
/// with an error domain in scope, the generic trap (`WeaveFFIError`)
/// otherwise.
pub(crate) fn check_helper(f: &FnBinding, module: &ModuleBinding) -> String {
    match (&module.error, f.error_strategy()) {
        (Some(eb), ErrorStrategy::Throws) => format!("detail::check_{}", eb.owner_path),
        _ => "detail::check".to_string(),
    }
}

/// The full `detail::make*_error(...)` call expression an async trampoline
/// uses to convert a callback error into the `std::exception_ptr` set on the
/// promise. The typed domain helper also receives the borrowed payload slots;
/// the generic helper takes only the code and message.
fn make_error_call(f: &FnBinding, module: &ModuleBinding) -> String {
    match (&module.error, f.error_strategy()) {
        (Some(eb), ErrorStrategy::Throws) => format!(
            "detail::make_{}_error(err->code, msg, err->payload_ptr, err->payload_len)",
            eb.owner_path
        ),
        _ => "detail::make_error(err->code, msg)".to_string(),
    }
}

// ── Parameter and return marshalling ──

/// The local wrapper class name behind an object-passed type: the interface
/// itself, or the interface inside `Interface?`.
fn object_class(ty: &Ty) -> &str {
    local_type_name(
        ty.interface_name()
            .expect("object passing only applies to interfaces"),
    )
}

/// Emit the setup statements for one C++ parameter and return the C argument
/// expressions its ABI slots receive, dispatching on the parameter's
/// [`ArgPass`] plan.
///
/// * A buffered parameter is encoded into a local `detail::BufferWriter` and
///   passed as `(data(), size())`; the caller owns the encoding for the
///   duration of the call.
/// * An object parameter is borrowed: the wrapper's pointer is passed and the
///   wrapper keeps its own reference. `Interface?` passes null for none.
/// * A callback interface moves the caller's `std::shared_ptr` into a heap
///   box that becomes `ctx`; the producer releases it through the vtable's
///   `free`. The vtable is the process-wide static for the interface.
fn emit_param_setup(
    w: &mut CodeWriter,
    p: &ParamBinding,
    module: &str,
    prefix: &str,
) -> Vec<String> {
    let name = cpp_ident(&p.name);
    match p.arg_pass() {
        ArgPass::Buffer { .. } => {
            let buf = format!("{name}_buf");
            w.line(format!("detail::BufferWriter {buf};"));
            emit_write_value(w, &p.ty, &name, &buf, 0);
            vec![format!("{buf}.data()"), format!("{buf}.size()")]
        }
        ArgPass::String { .. } => vec![format!("{name}.c_str()")],
        ArgPass::Bytes { .. } => {
            vec![format!("{name}.data()"), format!("{name}.size()")]
        }
        ArgPass::Object { nullable, .. } => {
            if nullable {
                vec![format!("{name}.has_value() ? {name}->handle() : nullptr")]
            } else {
                vec![format!("{name}.handle()")]
            }
        }
        ArgPass::Callback { .. } => {
            let iface = local_type_name(
                p.ty.callback_interface_name()
                    .expect("callback passing only applies to callback interfaces"),
            );
            w.line(format!(
                "if (!{name}) throw std::invalid_argument(\"{name}: null callback interface\");"
            ));
            w.line(format!(
                "auto* {name}_ctx = new std::shared_ptr<{iface}>(std::move({name}));"
            ));
            vec![
                format!("static_cast<void*>({name}_ctx)"),
                format!("&detail::{}()", vtable_accessor(iface)),
            ]
        }
        ArgPass::Direct { .. } => match &p.ty {
            Ty::Enum(e) => vec![format!(
                "static_cast<{}>(static_cast<int32_t>({name}))",
                c_abi_struct_name(e, module, prefix)
            )],
            _ => vec![name],
        },
    }
}

/// Marshal a sync callable's C result (already error-checked) into the C++
/// return value at the writer's current depth, dispatching on the return's
/// [`RetPass`] plan. A buffered return decodes the producer's buffer and
/// releases it with `{prefix}_free_bytes` via a scope guard, so the release
/// happens even when decoding throws. An object return is adopted into the
/// RAII wrapper, which owes the eventual `_destroy`.
fn emit_sync_return(w: &mut CodeWriter, ty: &Ty, module: &str, prefix: &str) {
    match ret_pass(Some(ty), module, prefix) {
        RetPass::Buffer => {
            w.line("detail::BufferGuard result_guard{result, out_len};");
            w.line("detail::BufferReader result_r(result, out_len);");
            emit_read_decl(w, ty, "ret", "result_r", module, prefix);
            w.line("result_r.expect_end();");
            w.line("return ret;");
        }
        RetPass::String => {
            w.line("std::string ret(result);");
            w.line(format!("{prefix}_free_string(result);"));
            w.line("return ret;");
        }
        RetPass::Bytes => {
            w.line("std::vector<uint8_t> ret(result, result + out_len);");
            w.line(format!(
                "{prefix}_free_bytes(const_cast<uint8_t*>(result), out_len);"
            ));
            w.line("return ret;");
        }
        RetPass::Object { nullable, .. } => {
            if nullable {
                w.line("if (!result) return std::nullopt;");
            }
            w.line(format!("return {}(result);", object_class(ty)));
        }
        RetPass::Direct => match ty {
            Ty::Enum(n) => {
                w.line(format!(
                    "return static_cast<{}>(result);",
                    local_type_name(n)
                ));
            }
            _ => {
                w.line("return result;");
            }
        },
        RetPass::Void => unreachable!("void returns emit no result marshalling"),
    }
}

// ── Callable kinds ──

/// How a rendered callable is declared in the C++ surface.
#[derive(Clone, Copy)]
pub(crate) enum FnKind<'a> {
    /// A namespace-scope free function (`inline` linkage).
    Free,
    /// An instance method on an interface class: passes the wrapped pointer as
    /// the leading C argument and is declared `const` (the ABI receiver is a
    /// const pointer).
    Method {
        /// The wrapper class name.
        class: &'a str,
    },
    /// A static member function: interface statics and the factory form of
    /// constructors not named `new`.
    Static {
        /// The wrapper class name.
        class: &'a str,
    },
    /// The canonical constructor (an interface constructor named `new`):
    /// rendered as a real C++ constructor adopting the returned pointer.
    Ctor {
        /// The wrapper class name.
        class: &'a str,
    },
}

impl<'a> FnKind<'a> {
    /// The expression passed as the leading `self` C argument, when present.
    fn self_arg(self) -> Option<String> {
        match self {
            FnKind::Method { .. } => Some("handle_".to_string()),
            _ => None,
        }
    }

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

/// The name of the lazy range class an iterator-returning callable yields:
/// `{PascalName}Iterator` for a free function (defined beside it in the module
/// namespace) and `{Class}{PascalName}Iterator` for an interface member
/// (defined at namespace scope after every value type).
pub(crate) fn iterator_class_name(f: &FnBinding, kind: FnKind<'_>) -> String {
    let pascal = f.name.to_upper_camel_case();
    match kind.class() {
        Some(class) => format!("{class}{pascal}Iterator"),
        None => format!("{pascal}Iterator"),
    }
}

/// The C++ return type of a callable in its call shape: the mapped type (or
/// `void`) for sync, `std::future<T>` for async, and the range class for an
/// iterator. `None` for a canonical constructor, which has no return type.
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

/// The C++ parameter list of a callable. `with_defaults` adds the
/// `= nullptr` default on a cancellable async call's `cancel_token`, which
/// belongs on the declaration only.
fn param_decls(f: &FnBinding, prefix: &str, with_defaults: bool) -> String {
    let mut decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name)))
        .collect();
    if f.is_async && f.cancellable {
        let default = if with_defaults { " = nullptr" } else { "" };
        decls.push(format!("{prefix}_cancel_token* cancel_token{default}"));
    }
    decls.join(", ")
}

/// Emit the doc comment and any `[[deprecated]]` attribute for a callable.
/// An iterator-returning callable's doc also names the range class it yields.
fn emit_callable_attrs(w: &mut CodeWriter, f: &FnBinding, kind: FnKind<'_>) {
    let doc = if let CallShape::Iterator(_) = &f.shape {
        let class_name = iterator_class_name(f, kind);
        let return_doc = format!(
            "@return A lazy `{class_name}` range that streams one element per iteration \
             step and releases the producer iterator when exhausted or destroyed."
        );
        Some(match &f.doc {
            Some(d) => format!("{d}\n\n{return_doc}"),
            None => return_doc,
        })
    } else {
        f.doc.clone()
    };
    w.doc(&doc, DocCommentStyle::Javadoc);
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('"', "\\\"");
        w.line(format!("[[deprecated(\"{escaped}\")]]"));
    }
}

/// Render the in-class declaration of an interface member (one level of
/// indentation), with its doc comment and deprecation marker.
pub(crate) fn render_member_decl(
    out: &mut String,
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    prefix: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(1);
    emit_callable_attrs(&mut w, f, kind);
    let params = param_decls(f, prefix, true);
    match (kind, return_type(f, kind)) {
        (FnKind::Ctor { class }, _) => w.line(format!("{class}({params});")),
        (FnKind::Method { .. }, Some(ret)) => w.line(format!("{ret} {cpp_name}({params}) const;")),
        (FnKind::Static { .. }, Some(ret)) => w.line(format!("static {ret} {cpp_name}({params});")),
        _ => unreachable!("free functions are defined inline, never declared"),
    };
    w.blank();
    out.push_str(&w.finish());
}

/// Render the definition of a callable: an `inline` free function inside its
/// module namespace (with doc comment and deprecation marker), or the
/// out-of-line `inline` definition of an interface member declared earlier
/// by [`render_member_decl`].
///
/// Wrappers are deliberately never marked `noexcept`: a callable with
/// `throws == false` still surfaces producer panics as the generic
/// `WeaveFFIError`.
pub(crate) fn render_definition(
    out: &mut String,
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let mut w = CodeWriter::four_space();
    if matches!(kind, FnKind::Free) {
        emit_callable_attrs(&mut w, f, kind);
    }
    let params = param_decls(f, prefix, false);
    let ret = return_type(f, kind);
    let header = match (kind, ret) {
        (FnKind::Ctor { class }, _) => {
            // The canonical constructor adopts the pointer the C constructor
            // returns; `handle_` starts null so a throw from the error check
            // leaves nothing for the destructor to release.
            format!("inline {class}::{class}({params}) : handle_(nullptr) {{")
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
    w.line(header);
    w.scope(|w| match &f.shape {
        CallShape::Sync(abi) => emit_sync_body(w, f, abi, kind, module, prefix),
        CallShape::Iterator(it) => emit_iterator_launch_body(w, f, it, kind, module, prefix),
        CallShape::Async(a) => emit_async_body(w, f, a, kind, module, prefix),
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit a synchronous callable's body: marshal the parameters (packing
/// buffered values into local buffers), call the C symbol, run the
/// throws-split error check, and marshal the return value (decoding buffered
/// returns then releasing the producer buffer, adopting object returns). For
/// a canonical constructor the "return" adopts the pointer instead.
fn emit_sync_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    abi: &AbiFn,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let check = check_helper(f, module);
    let mut c_args = Vec::new();
    if let Some(self_arg) = kind.self_arg() {
        c_args.push(self_arg);
    }
    for p in &f.params {
        c_args.extend(emit_param_setup(w, p, &module.path, prefix));
    }

    // A bytes or buffered return carries a trailing `size_t* out_len`.
    let has_out_len = matches!(
        ret_pass(f.ret.as_ref(), &module.path, prefix),
        RetPass::Buffer | RetPass::Bytes
    );
    if has_out_len {
        w.line("size_t out_len = 0;");
        c_args.push("&out_len".into());
    }
    c_args.push("&err".into());
    let args_str = c_args.join(", ");

    w.line(format!("{prefix}_error err{{}};"));
    if f.ret.is_none() {
        w.line(format!("{}({args_str});", abi.symbol));
    } else {
        w.line(format!("auto result = {}({args_str});", abi.symbol));
    }
    w.line(format!("{check}(err);"));

    if matches!(kind, FnKind::Ctor { .. }) {
        w.line("handle_ = result;");
    } else if let Some(ret) = &f.ret {
        emit_sync_return(w, ret, &module.path, prefix);
    }
}

// ── Iterators ──

/// Render the lazy range class an iterator-returning callable yields.
///
/// The C ABI yields an opaque iterator pointer plus `_next`/`_destroy`. The
/// range class is move-only, owns the pointer, and pulls exactly one element
/// per consumer step, honoring the `iter<T>` streaming contract
/// (`weaveffi_core::plan::IteratorProtocol`):
///
/// * `begin()`/`end()` expose a single-pass input iterator with a sentinel
///   end, so `for (auto&& item : fn())` streams in constant memory.
/// * Each pulled element is received per the plan's `elem` (strings copied
///   then `{prefix}_free_string`; bytes and buffered values copied or decoded
///   then `{prefix}_free_bytes`; objects adopted into their RAII wrapper).
/// * `destroy` runs exactly once: eagerly on exhaustion or a `next` error,
///   from the destructor otherwise. The pointer is nulled on every path.
/// * Launch and per-`next` errors follow the callable's [`ErrorStrategy`]
///   (the typed domain exception for `Throws`, the generic `WeaveFFIError`
///   trap otherwise).
pub(crate) fn render_iterator_range(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let elem_cpp = cpp_type(&it.elem);
    let class_name = iterator_class_name(f, kind);
    let iter_tag = &it.iter_tag;
    let destroy = &it.destroy_symbol;
    let check = check_helper(f, module);
    let owner = match kind.class() {
        Some(class) => format!("{class}::{cpp_name}()"),
        None => format!("{cpp_name}()"),
    };

    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "A lazy, move-only range over the `{elem_cpp}` elements produced by \
             `{owner}`.\n\nEach iteration step pulls exactly one element from the \
             producer, so results stream in constant memory. The range owns the \
             producer-side iterator and releases it exactly once: eagerly when the \
             range is exhausted, or from the destructor when iteration is abandoned \
             early."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("class {class_name} {{"));
    w.scope(|w| {
        w.line(format!("{iter_tag}* handle_;"));
        w.blank();
    });
    w.line("public:");
    w.scope(|w| {
        w.line("/** Adopts ownership of the raw producer iterator. */");
        w.line(format!(
            "explicit {class_name}({iter_tag}* h) : handle_(h) {{}}"
        ));
        w.blank();

        w.line(format!("~{class_name}() {{"));
        w.scope(|w| {
            w.line(format!("if (handle_) {destroy}(handle_);"));
        });
        w.line("}");
        w.blank();

        w.line(format!("{class_name}(const {class_name}&) = delete;"));
        w.line(format!(
            "{class_name}& operator=(const {class_name}&) = delete;"
        ));
        w.blank();

        w.line(format!(
            "{class_name}({class_name}&& other) noexcept : handle_(other.handle_) {{"
        ));
        w.scope(|w| {
            w.line("other.handle_ = nullptr;");
        });
        w.line("}");
        w.blank();

        w.line(format!(
            "{class_name}& operator=({class_name}&& other) noexcept {{"
        ));
        w.scope(|w| {
            w.line("if (this != &other) {");
            w.scope(|w| {
                w.line(format!("if (handle_) {destroy}(handle_);"));
                w.line("handle_ = other.handle_;");
                w.line("other.handle_ = nullptr;");
            });
            w.line("}");
            w.line("return *this;");
        });
        w.line("}");
        w.blank();

        render_iterator_next_method(w, f, it, module, prefix, &check);

        w.line("/** Sentinel type marking the end of the range. */");
        w.line("struct sentinel {};");
        w.blank();

        w.line(
            "/** Single-pass input iterator; each increment pulls one element from the producer. */",
        );
        w.line("class iterator {");
        w.scope(|w| {
            w.line(format!("{class_name}* range_;"));
            w.line(format!("std::optional<{elem_cpp}> current_;"));
            w.blank();
        });
        w.line("public:");
        w.scope(|w| {
            w.line("using iterator_category = std::input_iterator_tag;");
            w.line(format!("using value_type = {elem_cpp};"));
            w.line("using difference_type = std::ptrdiff_t;");
            w.line(format!("using pointer = {elem_cpp}*;"));
            w.line(format!("using reference = {elem_cpp}&;"));
            w.blank();
            w.line("/** Binds to `range` and pulls the first element. */");
            w.line(format!(
                "explicit iterator({class_name}* range) : range_(range), current_(range->next()) {{}}"
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
    out.push_str(&w.finish());
}

/// Emit an iterator-returning callable's body: marshal the parameters, call
/// the launcher, check `out_err`, and wrap the returned pointer in the range
/// class.
fn emit_iterator_launch_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let class_name = iterator_class_name(f, kind);
    let check = check_helper(f, module);
    let mut c_args = Vec::new();
    if let Some(self_arg) = kind.self_arg() {
        c_args.push(self_arg);
    }
    for p in &f.params {
        c_args.extend(emit_param_setup(w, p, &module.path, prefix));
    }
    c_args.push("&err".into());
    w.line(format!("{prefix}_error err{{}};"));
    w.line(format!(
        "{}* iter = {}({});",
        it.iter_tag,
        it.launch.symbol,
        c_args.join(", ")
    ));
    w.line(format!("{check}(err);"));
    w.line(format!("return {class_name}(iter);"));
}

/// Emit the range class's `next()` member: one producer `next` call that
/// yields the received element (or `std::nullopt` on exhaustion), releasing
/// the pulled slot per the plan's `elem` and destroying the iterator exactly
/// once on exhaustion or error.
fn render_iterator_next_method(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    module: &ModuleBinding,
    prefix: &str,
    check: &str,
) {
    let elem_cpp = cpp_type(&it.elem);
    let destroy = &it.destroy_symbol;
    let item_ret = abi::lower_return(&it.elem, &module.path);
    let item_ty = item_ret.ret.render_c(prefix);
    let strategy_doc = match f.error_strategy() {
        ErrorStrategy::Throws => "throws the module's typed exception",
        ErrorStrategy::Trap => "throws the generic WeaveFFIError",
    };

    w.doc(
        &Some(format!(
            "Pulls the next element from the producer, or `std::nullopt` once \
             exhausted (which releases the producer iterator eagerly). A producer \
             error {strategy_doc} after releasing the iterator."
        )),
        DocCommentStyle::Javadoc,
    );
    w.line(format!("std::optional<{elem_cpp}> next() {{"));
    w.scope(|w| {
        w.line("if (!handle_) return std::nullopt;");
        w.line(format!("{prefix}_error err{{}};"));
        w.line(format!("{item_ty} item{{}};"));
        let mut next_args = vec!["handle_".to_string(), "&item".to_string()];
        if !item_ret.out_params.is_empty() {
            w.line("size_t item_len = 0;");
            next_args.push("&item_len".to_string());
        }
        next_args.push("&err".to_string());
        w.line(format!(
            "int32_t has_item = {}({});",
            it.next.symbol,
            next_args.join(", ")
        ));
        w.line("if (err.code != 0) {");
        w.scope(|w| {
            w.line(format!("{destroy}(handle_);"));
            w.line("handle_ = nullptr;");
            w.line(format!("{check}(err);"));
        });
        w.line("}");
        w.line("if (has_item == 0) {");
        w.scope(|w| {
            w.line(format!("{destroy}(handle_);"));
            w.line("handle_ = nullptr;");
            w.line("return std::nullopt;");
        });
        w.line("}");
        match ret_pass(Some(&it.elem), &module.path, prefix) {
            RetPass::Buffer => {
                // A buffered element is producer-allocated: decode into an
                // owned value, then release with free_bytes via the scope
                // guard.
                w.line("detail::BufferGuard item_guard{item, item_len};");
                w.line("detail::BufferReader item_r(item, item_len);");
                emit_read_decl(w, &it.elem, "value", "item_r", &module.path, prefix);
                w.line("item_r.expect_end();");
                w.line("return value;");
            }
            RetPass::Bytes => {
                w.line("std::vector<uint8_t> value(item, item + item_len);");
                w.line(format!(
                    "{prefix}_free_bytes(const_cast<uint8_t*>(item), item_len);"
                ));
                w.line("return value;");
            }
            RetPass::String => {
                w.line("std::string value(item);");
                w.line(format!("{prefix}_free_string(item);"));
                w.line("return value;");
            }
            // An object element transfers one strong reference: adopt it. A
            // nullable element yields an engaged outer optional holding an
            // empty inner one for null, which is distinct from exhaustion.
            RetPass::Object { nullable, .. } => {
                let class = object_class(&it.elem);
                if nullable {
                    w.line(format!("{elem_cpp} value;"));
                    w.line("if (item) value.emplace(item);");
                    w.line("return value;");
                } else {
                    w.line(format!("return {class}(item);"));
                }
            }
            RetPass::Direct => match &it.elem {
                Ty::Enum(n) => {
                    let n = local_type_name(n);
                    w.line(format!("return static_cast<{n}>(item);"));
                }
                _ => {
                    w.line("return item;");
                }
            },
            RetPass::Void => unreachable!("iterator elements are never void"),
        }
    });
    w.line("}");
    w.blank();
}

// ── Async ──

/// Emit an asynchronous callable's body as a `std::future` wrapper. The
/// promise is heap-allocated, threaded through the C `context` pointer,
/// settled by the completion callback, and deleted exactly once. A callback
/// error settles the promise with the typed domain exception (payload fields
/// decoded) when the callable throws, or the generic `WeaveFFIError`
/// otherwise. The callback owns everything it receives: the boxed error and
/// any string or buffer result are released through the runtime free symbols
/// after copying, and an object result is adopted.
fn emit_async_body(
    w: &mut CodeWriter,
    f: &FnBinding,
    a: &AsyncBinding,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let cpp_ret = f.ret.as_ref().map_or("void".to_string(), cpp_type);
    let cb_params = render_param_decls(&a.callback_params, prefix).join(", ");
    let make_error = make_error_call(f, module);

    w.line(format!(
        "auto* promise_ptr = new std::promise<{cpp_ret}>();"
    ));
    w.line("auto future = promise_ptr->get_future();");

    let mut c_args = Vec::new();
    if let Some(self_arg) = kind.self_arg() {
        c_args.push(self_arg);
    }
    for p in &f.params {
        c_args.extend(emit_param_setup(w, p, &module.path, prefix));
    }
    if f.cancellable {
        c_args.push("cancel_token".to_string());
    }

    if c_args.is_empty() {
        w.line(format!("{}([]({cb_params}) {{", a.launch.symbol));
    } else {
        w.line(format!(
            "{}({}, []({cb_params}) {{",
            a.launch.symbol,
            c_args.join(", ")
        ));
    }
    w.scope(|w| {
        w.line(format!(
            "auto* p = static_cast<std::promise<{cpp_ret}>*>(context);"
        ));
        // The callback runs on a producer thread inside a C frame, so a
        // decode failure (of the result or of an error payload) must settle
        // the promise, never unwind.
        w.line("try {");
        w.scope(|w| {
            w.line("if (err && err->code != 0) {");
            w.scope(|w| {
                w.line("std::string msg(err->message ? err->message : \"unknown error\");");
                w.line(format!("p->set_exception({make_error});"));
            });
            w.line("} else {");
            w.scope(|w| {
                if let Some(ret) = &f.ret {
                    emit_async_set_value(w, ret, &module.path, prefix);
                } else {
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
        w.line("delete p;");
    });
    w.line("}, static_cast<void*>(promise_ptr));");
    w.line("return future;");
}

/// Settle an async promise from the callback's result slots at the writer's
/// current depth, dispatching on the result's [`RetPass`] plan.
///
/// Per the async completion contract (`weaveffi_core::plan::AsyncProtocol`),
/// result buffers handed to the callback (strings, bytes, and buffered
/// values) are *owned by the consumer*: the wrapper copies or decodes them,
/// then releases the producer allocation through the runtime free symbols.
/// An owned interface result is instead adopted into the RAII wrapper.
fn emit_async_set_value(w: &mut CodeWriter, ty: &Ty, module: &str, prefix: &str) {
    match ret_pass(Some(ty), module, prefix) {
        RetPass::Buffer => {
            w.line("detail::BufferGuard result_guard{result_ptr, result_len};");
            w.line("detail::BufferReader result_r(result_ptr, result_len);");
            emit_read_decl(w, ty, "value", "result_r", module, prefix);
            w.line("result_r.expect_end();");
            w.line("p->set_value(std::move(value));");
        }
        RetPass::String => {
            w.line("std::string value(result ? result : \"\");");
            w.line(format!("{prefix}_free_string(result);"));
            w.line("p->set_value(std::move(value));");
        }
        RetPass::Bytes => {
            w.line("std::vector<uint8_t> value(result, result + result_len);");
            w.line(format!(
                "{prefix}_free_bytes(const_cast<uint8_t*>(result), result_len);"
            ));
            w.line("p->set_value(std::move(value));");
        }
        RetPass::Object { nullable, .. } => {
            let class = object_class(ty);
            if nullable {
                w.line("if (!result) {");
                w.scope(|w| {
                    w.line("p->set_value(std::nullopt);");
                });
                w.line("} else {");
                w.scope(|w| {
                    w.line(format!("p->set_value({class}(result));"));
                });
                w.line("}");
            } else {
                w.line(format!("p->set_value({class}(result));"));
            }
        }
        RetPass::Direct => match ty {
            Ty::Enum(n) => {
                w.line(format!(
                    "p->set_value(static_cast<{}>(result));",
                    local_type_name(n)
                ));
            }
            _ => {
                w.line("p->set_value(result);");
            }
        },
        RetPass::Void => unreachable!("void results settle the promise with no value"),
    }
}

// ── Namespace: per-module function namespaces ──

/// Emit one module's nested namespace holding its free functions with bare
/// snake_case names (`namespace kv::stats { ... }`). An iterator-returning
/// function is preceded by its range class. Modules with no functions emit
/// nothing; their types live at the namespace root.
pub(crate) fn render_cpp_module_ns(out: &mut String, module: &ModuleBinding, prefix: &str) {
    if module.functions.is_empty() {
        return;
    }
    let ns = cpp_namespace_path(module);
    out.push_str(&format!("namespace {ns} {{\n\n"));
    for f in &module.functions {
        let name = cpp_fn_name(&f.name);
        if let CallShape::Iterator(it) = &f.shape {
            render_iterator_range(out, f, it, &name, FnKind::Free, module, prefix);
        }
        render_definition(out, f, &name, FnKind::Free, module, prefix);
    }
    out.push_str(&format!("}} // namespace {ns}\n\n"));
}
