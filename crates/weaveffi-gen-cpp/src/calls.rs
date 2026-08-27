//! Callable rendering: free functions, interface members, listeners, and the
//! sync, iterator, and async call shapes, each marshalled per the shared
//! passing plans ([`ArgPass`], [`RetPass`], `elem_free`).

use heck::{ToSnakeCase, ToUpperCamelCase};
use weaveffi_core::abi;
use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::{
    AbiFn, AsyncBinding, CallShape, FnBinding, IteratorBinding, ListenerBinding, ModuleBinding,
    ParamBinding,
};
use weaveffi_core::plan::{elem_free, ret_pass, ArgPass, ElemFree, ErrorStrategy, RetPass};
use weaveffi_core::utils::{c_abi_struct_name, local_type_name};
use weaveffi_ir::ir::TypeRef;

use crate::codec::{emit_read_decl, emit_read_into, emit_write_value};
use crate::types::{
    cpp_fn_name, cpp_ident, cpp_namespace_path, cpp_param_decl, cpp_type, render_param_decls,
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

/// The local interface name behind an object-passed type: the interface
/// itself, or the interface inside `Interface?`.
fn object_iface_name(ty: &TypeRef) -> &str {
    match ty {
        TypeRef::Interface(n) => n,
        TypeRef::Optional(inner) => object_iface_name(inner),
        _ => unreachable!("object passing only applies to interfaces"),
    }
}

/// Emit the setup statements for one C++ parameter and return the C argument
/// expressions its ABI slots receive, dispatching on the parameter's
/// [`ArgPass`] plan. A buffered parameter is encoded into a local
/// `detail::BufferWriter` and passed as `(data(), size())`; the caller owns
/// the encoding for the duration of the call.
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
        // An interface argument borrows: pass its raw handle as a const
        // pointer, leaving ownership with the wrapper object. A nullable
        // object (`Interface?`) passes null for "none".
        ArgPass::Object { nullable, .. } => {
            let tag = c_abi_struct_name(object_iface_name(&p.ty), module, prefix);
            if nullable {
                vec![format!(
                    "{name}.has_value() ? static_cast<const {tag}*>({name}.value().handle()) : nullptr"
                )]
            } else {
                vec![format!("static_cast<const {tag}*>({name}.handle())")]
            }
        }
        ArgPass::Direct { .. } => match &p.ty {
            TypeRef::Handle => vec![format!(
                "static_cast<{prefix}_handle_t>(reinterpret_cast<uintptr_t>({name}))"
            )],
            TypeRef::Enum(e) => vec![format!(
                "static_cast<{}>(static_cast<int32_t>({name}))",
                c_abi_struct_name(e, module, prefix)
            )],
            // Scalars pass through; a typed handle is already the raw
            // prefixed tag pointer.
            _ => vec![name],
        },
    }
}

/// Marshal a sync callable's C result (already error-checked) into the C++
/// return value at the writer's current depth, dispatching on the return's
/// [`RetPass`] plan. A buffered return decodes the producer's buffer and
/// releases it with `{prefix}_free_bytes` via a scope guard, so the release
/// happens even when decoding throws.
fn emit_sync_return(w: &mut CodeWriter, ty: &TypeRef, module: &str, prefix: &str) {
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
        // An owned interface pointer is adopted by the RAII class, which
        // destroys it when the wrapper drops. A nullable return maps null to
        // `std::nullopt`.
        RetPass::Object { nullable, .. } => {
            if nullable {
                w.line("if (!result) return std::nullopt;");
            }
            w.line(format!(
                "return {}(result);",
                local_type_name(object_iface_name(ty))
            ));
        }
        RetPass::Direct => match ty {
            TypeRef::Handle => {
                w.line("return reinterpret_cast<void*>(static_cast<uintptr_t>(result));");
            }
            TypeRef::Enum(n) => {
                w.line(format!(
                    "return static_cast<{}>(result);",
                    local_type_name(n)
                ));
            }
            // Scalars and typed handles (the raw tag pointer) pass through.
            _ => {
                w.line("return result;");
            }
        },
        RetPass::Void => unreachable!("void returns emit no result marshalling"),
    }
}

// ── Callable rendering (free functions and interface members) ──

/// How a rendered callable is declared in the C++ surface.
#[derive(Clone, Copy)]
pub(crate) enum FnKind<'a> {
    /// A namespace-scope free function (`inline` linkage).
    Free,
    /// An instance method on an interface class: passes the wrapped handle as
    /// the leading C argument and is declared `const` (the ABI receiver is a
    /// const pointer).
    Method {
        /// The interface's opaque C tag, used to cast `handle_` for the call.
        c_tag: &'a str,
    },
    /// A static member function: interface statics and the factory form of
    /// constructors not named `new`.
    Static,
    /// The canonical constructor (an interface constructor named `new`):
    /// rendered as a real C++ constructor adopting the returned handle.
    Ctor,
}

impl FnKind<'_> {
    /// Leading declaration keyword for this kind.
    fn keyword(self) -> &'static str {
        match self {
            FnKind::Free => "inline ",
            FnKind::Method { .. } | FnKind::Ctor => "",
            FnKind::Static => "static ",
        }
    }

    /// Nesting depth of the declaration: class members are one level deep.
    fn depth(self) -> usize {
        match self {
            FnKind::Free => 0,
            _ => 1,
        }
    }

    /// The expression passed as the leading `self` C argument, when present.
    fn self_arg(self) -> Option<String> {
        match self {
            FnKind::Method { c_tag } => Some(format!("static_cast<const {c_tag}*>(handle_)")),
            _ => None,
        }
    }

    /// Trailing cv-qualifier on the declaration (methods are `const`).
    fn const_qual(self) -> &'static str {
        match self {
            FnKind::Method { .. } => " const",
            _ => "",
        }
    }
}

/// Emit the doc comment and any `[[deprecated]]` attribute for a callable.
fn emit_callable_attrs(w: &mut CodeWriter, f: &FnBinding) {
    w.doc(&f.doc, DocCommentStyle::Javadoc);
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('"', "\\\"");
        w.line(format!("[[deprecated(\"{escaped}\")]]"));
    }
}

/// Render one callable (free function or interface member) in whatever call
/// shape it lowers to. `cpp_name` is the already-cased C++ name (the class
/// name for a canonical constructor).
///
/// Wrappers are deliberately never marked `noexcept`: a callable with
/// `throws == false` still surfaces producer panics as the generic
/// `WeaveFFIError`.
pub(crate) fn render_cpp_callable(
    out: &mut String,
    f: &FnBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    match &f.shape {
        CallShape::Sync(abi) => render_sync_callable(out, f, abi, cpp_name, kind, module, prefix),
        CallShape::Iterator(it) => {
            render_iterator_callable(out, f, it, cpp_name, kind, module, prefix)
        }
        CallShape::Async(a) => render_async_callable(out, f, a, cpp_name, kind, module, prefix),
    }
}

/// Render a synchronous callable: marshal the parameters (packing buffered
/// values into local buffers), call the C symbol, run the throws-split error
/// check, and marshal the return value (decoding buffered returns then
/// releasing the producer buffer). For a canonical constructor the "return"
/// adopts the handle instead.
fn render_sync_callable(
    out: &mut String,
    f: &FnBinding,
    abi: &AbiFn,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let mut w = CodeWriter::four_space().with_depth(kind.depth());
    emit_callable_attrs(&mut w, f);

    let decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name), &module.path, prefix))
        .collect();

    let is_ctor = matches!(kind, FnKind::Ctor);
    if is_ctor {
        // The canonical constructor adopts the handle the C constructor
        // returns; `handle_` starts null so a throw from the error check
        // leaves nothing for the destructor to free.
        w.line(format!(
            "{cpp_name}({}) : handle_(nullptr) {{",
            decls.join(", ")
        ));
    } else {
        let cpp_ret = f
            .ret
            .as_ref()
            .map_or("void".to_string(), |r| cpp_type(r, &module.path, prefix));
        w.line(format!(
            "{}{cpp_ret} {cpp_name}({}){} {{",
            kind.keyword(),
            decls.join(", "),
            kind.const_qual()
        ));
    }

    let check = check_helper(f, module);
    w.scope(|w| {
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

        if is_ctor {
            w.line("handle_ = result;");
        } else if let Some(ret) = &f.ret {
            emit_sync_return(w, ret, &module.path, prefix);
        }
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Render an iterator-returning callable as a lazy range.
///
/// The C ABI yields an opaque iterator handle plus `_next`/`_destroy`. The
/// wrapper emits a per-function move-only RAII range class (named
/// `{PascalName}Iterator`) that owns the handle and pulls exactly one element
/// per consumer step, honoring the `iter<T>` streaming contract
/// (`weaveffi_core::plan::IteratorProtocol`):
///
/// * `begin()`/`end()` expose a single-pass input iterator with a sentinel
///   end, so `for (auto&& item : fn())` streams in constant memory.
/// * Each pulled element is converted and then released per the plan's
///   `elem_free` (strings copied then `{prefix}_free_string`; bytes and
///   buffered values copied or decoded then `{prefix}_free_bytes`).
/// * `destroy` runs exactly once: eagerly on exhaustion or a `next` error,
///   from the destructor otherwise. The handle is nulled on every path.
/// * Launch and per-`next` errors follow the callable's [`ErrorStrategy`]
///   (the typed domain exception for `Throws`, the generic `WeaveFFIError`
///   trap otherwise).
fn render_iterator_callable(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let elem_cpp = cpp_type(&it.elem, &module.path, prefix);
    let class_name = format!("{}Iterator", f.name.to_upper_camel_case());
    let iter_tag = &it.iter_tag;
    let destroy = &it.destroy_symbol;
    let check = check_helper(f, module);

    // ── The lazy range class ──
    let mut w = CodeWriter::four_space().with_depth(kind.depth());
    w.doc(
        &Some(format!(
            "A lazy, move-only range over the `{elem_cpp}` elements produced by \
             `{cpp_name}()`.\n\nEach iteration step pulls exactly one element from the \
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
        w.line("/** Adopts ownership of the raw producer iterator handle. */");
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

    // ── The launching wrapper ──
    let return_doc = format!(
        "@return A lazy `{class_name}` range that streams one element per iteration \
         step and releases the producer iterator when exhausted or destroyed."
    );
    let fn_doc = match &f.doc {
        Some(d) => format!("{d}\n\n{return_doc}"),
        None => return_doc,
    };
    w.doc(&Some(fn_doc), DocCommentStyle::Javadoc);
    if let Some(msg) = &f.deprecated {
        let escaped = msg.replace('"', "\\\"");
        w.line(format!("[[deprecated(\"{escaped}\")]]"));
    }

    let decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name), &module.path, prefix))
        .collect();
    w.line(format!(
        "{}{class_name} {cpp_name}({}){} {{",
        kind.keyword(),
        decls.join(", "),
        kind.const_qual()
    ));

    w.scope(|w| {
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
            "{iter_tag}* iter = {}({});",
            it.launch.symbol,
            c_args.join(", ")
        ));
        w.line(format!("{check}(err);"));
        w.line(format!("return {class_name}(iter);"));
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the range class's `next()` member: one producer `next` call that
/// yields the converted element (or `std::nullopt` on exhaustion), releasing
/// the pulled slot per the plan's `elem_free` and destroying the handle
/// exactly once on exhaustion or error.
fn render_iterator_next_method(
    w: &mut CodeWriter,
    f: &FnBinding,
    it: &IteratorBinding,
    module: &ModuleBinding,
    prefix: &str,
    check: &str,
) {
    let elem_cpp = cpp_type(&it.elem, &module.path, prefix);
    let destroy = &it.destroy_symbol;
    let item_ret = abi::lower_return(&it.elem, &module.path);
    let item_ty = item_ret.ret.render_c(prefix);
    let ef = elem_free(&it.elem);
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
        if abi::is_buffered(&it.elem) {
            // A buffered element is producer-allocated: decode into an owned
            // value, then release with free_bytes via the scope guard.
            w.line("detail::BufferGuard item_guard{item, item_len};");
            w.line("detail::BufferReader item_r(item, item_len);");
            emit_read_decl(w, &it.elem, "value", "item_r", &module.path, prefix);
            w.line("item_r.expect_end();");
            w.line("return value;");
        } else {
            match (&it.elem, &ef) {
                // Byte-buffer elements copy then release the producer buffer.
                (TypeRef::Bytes | TypeRef::BorrowedBytes, _) => {
                    w.line("std::vector<uint8_t> value(item, item + item_len);");
                    w.line(format!(
                        "{prefix}_free_bytes(const_cast<uint8_t*>(item), item_len);"
                    ));
                    w.line("return value;");
                }
                (_, ElemFree::String) => {
                    w.line("std::string value(item);");
                    w.line(format!("{prefix}_free_string(item);"));
                    w.line("return value;");
                }
                (TypeRef::Enum(n), _) => {
                    let n = local_type_name(n);
                    w.line(format!("return static_cast<{n}>(item);"));
                }
                (TypeRef::Handle, _) => {
                    w.line("return reinterpret_cast<void*>(static_cast<uintptr_t>(item));");
                }
                _ => {
                    w.line("return item;");
                }
            }
        }
    });
    w.line("}");
    w.blank();
}

/// Render an asynchronous callable as a `std::future` wrapper. The promise is
/// heap-allocated, threaded through the C `context` pointer, settled by the
/// completion callback, and deleted exactly once. A callback error settles
/// the promise with the typed domain exception (payload fields decoded) when
/// the callable throws, or the generic `WeaveFFIError` otherwise. Borrowed
/// result buffers are copied or decoded inside the callback, before the
/// producer reclaims them.
fn render_async_callable(
    out: &mut String,
    f: &FnBinding,
    a: &AsyncBinding,
    cpp_name: &str,
    kind: FnKind<'_>,
    module: &ModuleBinding,
    prefix: &str,
) {
    let cpp_ret = f
        .ret
        .as_ref()
        .map_or("void".to_string(), |r| cpp_type(r, &module.path, prefix));
    let mut w = CodeWriter::four_space().with_depth(kind.depth());
    emit_callable_attrs(&mut w, f);

    let mut decls: Vec<String> = f
        .params
        .iter()
        .map(|p| cpp_param_decl(&p.ty, &cpp_ident(&p.name), &module.path, prefix))
        .collect();
    if f.cancellable {
        decls.push(format!("{prefix}_cancel_token* cancel_token = nullptr"));
    }
    w.line(format!(
        "{}std::future<{cpp_ret}> {cpp_name}({}){} {{",
        kind.keyword(),
        decls.join(", "),
        kind.const_qual()
    ));

    let cb_params = render_param_decls(&a.callback_params, prefix).join(", ");
    let make_error = make_error_call(f, module);
    w.scope(|w| {
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
            w.line("delete p;");
        });
        w.line("}, static_cast<void*>(promise_ptr));");
        w.line("return future;");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}

/// Settle an async promise from the callback's result slots at the writer's
/// current depth, dispatching on the result's [`RetPass`] plan.
///
/// Per the async completion contract (`weaveffi_core::plan::AsyncProtocol`),
/// result buffers handed to the callback (strings, bytes, and buffered
/// values) are *borrowed*: they stay owned by the producer and are valid only
/// for the callback's duration, so the wrapper deep-copies or decodes them
/// and never frees them. An owned interface result is the exception: the
/// callback receives ownership and adopts the pointer into the RAII wrapper.
fn emit_async_set_value(w: &mut CodeWriter, ty: &TypeRef, module: &str, prefix: &str) {
    match ret_pass(Some(ty), module, prefix) {
        RetPass::Buffer => {
            // Borrowed `(result_ptr, result_len)` buffer: decode, never free.
            w.line("detail::BufferReader result_r(result_ptr, result_len);");
            emit_read_decl(w, ty, "value", "result_r", module, prefix);
            w.line("result_r.expect_end();");
            w.line("p->set_value(std::move(value));");
        }
        // Borrowed for the callback's duration: copy, do not free.
        RetPass::String => {
            w.line("p->set_value(std::string(result));");
        }
        RetPass::Bytes => {
            w.line("p->set_value(std::vector<uint8_t>(result, result + result_len));");
        }
        // Owned interface result: the callback receives ownership; adopt it.
        // A nullable result maps null to `std::nullopt`.
        RetPass::Object { nullable, .. } => {
            let ln = local_type_name(object_iface_name(ty));
            if nullable {
                w.line("if (!result) {");
                w.scope(|w| {
                    w.line("p->set_value(std::nullopt);");
                });
                w.line("} else {");
                w.scope(|w| {
                    w.line(format!("p->set_value({ln}(result));"));
                });
                w.line("}");
            } else {
                w.line(format!("p->set_value({ln}(result));"));
            }
        }
        RetPass::Direct => match ty {
            TypeRef::Handle => {
                w.line("p->set_value(reinterpret_cast<void*>(static_cast<uintptr_t>(result)));");
            }
            TypeRef::Enum(n) => {
                w.line(format!(
                    "p->set_value(static_cast<{}>(result));",
                    local_type_name(n)
                ));
            }
            // Scalars and typed handles (the raw tag pointer) pass through.
            _ => {
                w.line("p->set_value(result);");
            }
        },
        RetPass::Void => unreachable!("void results settle the promise with no value"),
    }
}

// ── Namespace: per-module function namespaces and listeners ──

/// Emit one module's nested namespace holding its listeners and free
/// functions with bare snake_case names (`namespace kv::stats { ... }`).
/// Modules with no functions or listeners emit nothing; their types live at
/// the namespace root.
pub(crate) fn render_cpp_module_ns(out: &mut String, module: &ModuleBinding, prefix: &str) {
    if module.functions.is_empty() && module.listeners.is_empty() {
        return;
    }
    let ns = cpp_namespace_path(module);
    out.push_str(&format!("namespace {ns} {{\n\n"));
    for l in &module.listeners {
        render_cpp_listener(out, module, l, prefix);
    }
    for f in &module.functions {
        render_cpp_callable(out, f, &cpp_fn_name(&f.name), FnKind::Free, module, prefix);
    }
    out.push_str(&format!("}} // namespace {ns}\n\n"));
}

/// The C++ type one callback parameter surfaces as in the user callback.
/// Buffered values are decoded before dispatch, so they surface as full C++
/// value types. Interface and typed-handle parameters stay raw borrowed
/// pointers: wrapping a borrowed handle in the owning RAII class would
/// `_destroy` it on destruction.
fn cpp_cb_param_type(ty: &TypeRef, module: &str, prefix: &str) -> String {
    match ty {
        TypeRef::Interface(n) => format!("const {}*", c_abi_struct_name(n, module, prefix)),
        TypeRef::Optional(inner) if matches!(inner.as_ref(), TypeRef::Interface(_)) => {
            cpp_cb_param_type(inner, module, prefix)
        }
        other => cpp_type(other, module, prefix),
    }
}

/// Emit any decode statements for one callback parameter and return the
/// expression handed to the user's `std::function`, dispatching on the
/// parameter's [`ArgPass`] plan. Buffered arguments are borrowed `(ptr, len)`
/// pairs valid only during the dispatch, so they are decoded into owned C++
/// values before the user callback runs.
fn emit_cb_arg(w: &mut CodeWriter, p: &ParamBinding, module: &str, prefix: &str) -> String {
    match p.arg_pass() {
        ArgPass::Buffer { ptr, len } => {
            let n0 = &ptr.name;
            let n1 = &len.name;
            let var = format!("{}_val", p.name);
            let rdr = format!("{}_r", p.name);
            let cpp = cpp_type(&p.ty, module, prefix);
            w.line(format!("{cpp} {var}{{}};"));
            w.line(format!("if ({n0} != nullptr) {{"));
            w.scope(|w| {
                w.line(format!("detail::BufferReader {rdr}({n0}, {n1});"));
                emit_read_into(w, &p.ty, &var, &var, &rdr, module, prefix);
                w.line(format!("{rdr}.expect_end();"));
            });
            w.line("}");
            format!("std::move({var})")
        }
        ArgPass::String { slot } => {
            let n0 = &slot.name;
            format!("std::string({n0} ? {n0} : \"\")")
        }
        ArgPass::Bytes { ptr, len } => {
            let n0 = &ptr.name;
            let n1 = &len.name;
            format!("{n0} ? std::vector<uint8_t>({n0}, {n0} + {n1}) : std::vector<uint8_t>{{}}")
        }
        // Borrowed for the duration of the callback; passed through raw.
        ArgPass::Object { slot, .. } => slot.name.clone(),
        ArgPass::Direct { slot } => {
            let n0 = &slot.name;
            match &p.ty {
                TypeRef::Enum(e) => format!(
                    "static_cast<{}>(static_cast<int32_t>({n0}))",
                    local_type_name(e)
                ),
                TypeRef::Handle => {
                    format!("reinterpret_cast<void*>(static_cast<uintptr_t>({n0}))")
                }
                // Typed handles stay the raw borrowed tag pointer.
                _ => n0.clone(),
            }
        }
    }
}

/// The register/unregister pair for one listener. The user `std::function` is
/// heap-boxed and threaded through the C `context` pointer; a capture-free
/// lambda (convertible to the C function pointer) unboxes and invokes it,
/// decoding any borrowed buffered arguments first.
fn render_cpp_listener(
    out: &mut String,
    module: &ModuleBinding,
    l: &ListenerBinding,
    prefix: &str,
) {
    let Some(cb) = module.callback(&l.event_callback) else {
        unreachable!("validation guarantees the listener's callback exists");
    };

    let fn_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| cpp_cb_param_type(&p.ty, &module.path, prefix))
        .collect();
    let std_fn = format!("std::function<void({})>", fn_params.join(", "));

    let lambda_params = render_param_decls(&cb.abi_params, prefix).join(", ");

    let register_name = format!("register_{}", l.name.to_snake_case());
    let unregister_name = format!("unregister_{}", l.name.to_snake_case());

    let mut w = CodeWriter::four_space();
    w.doc(&l.doc, DocCommentStyle::Javadoc);
    w.line(format!(
        "/** @return A subscription id for {unregister_name}(). */"
    ));
    w.line(format!(
        "inline uint64_t {register_name}({std_fn} callback) {{"
    ));
    w.scope(|w| {
        w.line(format!(
            "auto fn = std::make_shared<{std_fn}>(std::move(callback));"
        ));
        w.line(format!("uint64_t id = {}(", l.register_symbol));
        w.scope(|w| {
            w.line(format!("[]({lambda_params}) {{"));
            w.scope(|w| {
                w.line(format!("auto& cb = *static_cast<{std_fn}*>(context);"));
                let args: Vec<String> = cb
                    .params
                    .iter()
                    .map(|p| emit_cb_arg(w, p, &module.path, prefix))
                    .collect();
                w.line(format!("cb({});", args.join(", ")));
            });
            w.line("},");
            w.line("fn.get());");
        });
        w.line("std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());");
        w.line("detail::wv_listener_registry()[id] = fn;");
        w.line("return id;");
    });
    w.line("}");
    w.blank();

    w.line(format!(
        "/** Unregisters a listener previously registered with {register_name}(). */"
    ));
    w.line(format!("inline void {unregister_name}(uint64_t id) {{"));
    w.scope(|w| {
        w.line(format!("{}(id);", l.unregister_symbol));
        w.line("std::lock_guard<std::mutex> lock(detail::wv_listener_mutex());");
        w.line("detail::wv_listener_registry().erase(id);");
    });
    w.line("}");
    w.blank();
    out.push_str(&w.finish());
}
