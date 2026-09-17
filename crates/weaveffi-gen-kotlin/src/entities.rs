//! Kotlin entity rendering: plain and rich enums, record data classes, the
//! exception hierarchy, `Cleaner`-backed interface wrapper classes, the lazy
//! iterator classes, and callback interfaces with their JNI dispatch objects,
//! plus the per-type buffer codecs.

use weaveffi_core::codegen::common::pascal_case;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    BindingModel, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding,
    IteratorBinding, StructBinding,
};
use weaveffi_core::utils::local_type_name;

use crate::calls::{
    interface_native_call, interface_native_decl, interface_native_name, kotlin_error_mapper,
    render_kotlin_async_fun, write_kotlin_sync_wrapper,
};
use crate::codec::{kt_decode_expr, kt_read_expr, kt_write_field};
use crate::docs::{splice, writer_doc, writer_fn_doc};
use crate::types::{
    camel_params, kotlin_callback_dispatch_name, kotlin_iterator_class_name,
    kotlin_shim_param_type, kotlin_type, kt_escape, kt_param,
};

/// The Kotlin exception type for an error domain: the shared exception brand
/// naming, so `KvError` becomes `KvException`.
pub(crate) fn kotlin_exception_name(eb: &ErrorBinding) -> String {
    errors::exception_type_name(&eb.name)
}

/// Render a C-style enum as a Kotlin `enum class` with a raw `value` and a
/// `fromValue` companion factory; rich (algebraic) enums divert to
/// [`render_kotlin_rich_enum`].
pub(crate) fn render_kotlin_enum(out: &mut String, e: &EnumBinding) {
    // A rich (algebraic) enum is a value type crossing the ABI in a value
    // buffer, so it is emitted as a sealed class with per-variant subtypes,
    // never as a plain `enum class`.
    if e.is_rich() {
        render_kotlin_rich_enum(out, e);
        return;
    }
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &e.doc);
    w.line(format!("enum class {}(val value: Int) {{", e.name));
    w.scope(|w| {
        for (i, v) in e.variants.iter().enumerate() {
            writer_doc(w, &v.doc);
            let comma = if i < e.variants.len() - 1 { "," } else { ";" };
            w.line(format!("{}({}){}", kt_escape(&v.name), v.value, comma));
        }
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line(format!(
                "fun fromValue(value: Int): {} = entries.first {{ it.value == value }}",
                e.name
            ));
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Render a rich (algebraic) enum as a sealed class with one `data class`
/// per data-carrying variant and one `object` per unit variant, plus the
/// internal `pack{Name}`/`unpack{Name}` buffer codecs. Values cross the ABI
/// serialized in value buffers (an `i32` tag followed by the active variant's
/// fields in declaration order); no C symbols exist for a rich enum.
pub(crate) fn render_kotlin_rich_enum(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &e.doc);
    w.line(format!("sealed class {name} {{"));
    w.scope(|w| {
        for v in &e.variants {
            writer_doc(w, &v.doc);
            let vn = pascal_case(&v.name);
            if v.fields.is_empty() {
                w.line(format!("object {vn} : {name}()"));
            } else if v.fields.iter().any(|f| f.doc.is_some()) {
                w.line(format!("data class {vn}("));
                w.scope(|w| {
                    for f in &v.fields {
                        writer_doc(w, &f.doc);
                        w.line(format!(
                            "val {}: {},",
                            kt_escape(&f.name),
                            kotlin_type(&f.ty)
                        ));
                    }
                });
                w.line(format!(") : {name}()"));
            } else {
                let fields: Vec<String> = v
                    .fields
                    .iter()
                    .map(|f| format!("val {}: {}", kt_escape(&f.name), kotlin_type(&f.ty)))
                    .collect();
                w.line(format!("data class {vn}({}) : {name}()", fields.join(", ")));
            }
        }
    });
    w.line("}");
    out.push_str(&w.finish());
    render_kotlin_rich_enum_codecs(out, e);
}

/// Render the internal buffer codecs for one rich enum: `pack{Name}` writes
/// the `i32` tag then the active variant's fields; `unpack{Name}` dispatches
/// on the tag and rejects unknown values.
pub(crate) fn render_kotlin_rich_enum_codecs(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line(format!(
        "internal fun pack{name}(w: WeaveBufferWriter, v: {name}) {{"
    ));
    w.scope(|w| {
        w.line("when (v) {");
        w.scope(|w| {
            for v in &e.variants {
                let vn = pascal_case(&v.name);
                if v.fields.is_empty() {
                    w.line(format!("is {name}.{vn} -> w.writeI32({})", v.value));
                } else {
                    w.line(format!("is {name}.{vn} -> {{"));
                    w.scope(|w| {
                        w.line(format!("w.writeI32({})", v.value));
                        for f in &v.fields {
                            w.line(kt_write_field(&f.ty, "w", &f.name));
                        }
                    });
                    w.line("}");
                }
            }
        });
        w.line("}");
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun unpack{name}(r: WeaveBufferReader): {name} = when (val tag = r.readI32()) {{"
    ));
    w.scope(|w| {
        for v in &e.variants {
            let vn = pascal_case(&v.name);
            if v.fields.is_empty() {
                w.line(format!("{} -> {name}.{vn}", v.value));
            } else {
                let args: Vec<String> = v.fields.iter().map(|f| kt_read_expr(&f.ty, "r")).collect();
                w.line(format!("{} -> {name}.{vn}({})", v.value, args.join(", ")));
            }
        }
        w.line(format!(
            "else -> throw {}(-2, \"malformed WeaveFFI value buffer: unknown {name} tag $tag\")",
            errors::EXCEPTION_BRAND
        ));
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Render the exception surface: the open generic brand exception plus one
/// sealed exception class per *declared* error domain, each with a per-code
/// subclass and a `fromCode` factory mapping raw ABI codes (and the optional
/// serialized payload) to typed instances. Codes that declare payload fields
/// expose them as constructor properties, decoded from the value buffer.
///
/// Domain codes are validated positive-only, and the negative range is
/// reserved for the runtime (generic error, producer panic, marshalling
/// failure, a callback interface implementation raised), so `fromCode` maps
/// only the declared codes and every other code (all negatives included)
/// falls through to the generic branded exception.
pub(crate) fn render_kotlin_error_types(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line("/** Generic WeaveFFI failure: panics, marshalling errors, callback implementation failures (code -4), and unknown codes. */");
    w.line(format!(
        "open class {}(val code: Int, message: String) : Exception(message)",
        errors::EXCEPTION_BRAND
    ));
    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) else {
            continue;
        };
        let exc = kotlin_exception_name(eb);
        w.blank();
        w.line(format!(
            "/** Typed error domain `{}` declared by module `{}`. */",
            eb.name, eb.owner_path
        ));
        w.line(format!(
            "sealed class {exc}(code: Int, message: String) : {}(code, message) {{",
            errors::EXCEPTION_BRAND
        ));
        w.scope(|w| {
            for ec in &eb.codes {
                writer_doc(w, &ec.doc);
                let default_msg = ec.message.replace('"', "\\\"");
                if ec.fields.is_empty() {
                    w.line(format!(
                        "class {}(message: String = \"{default_msg}\") : {exc}({}, message)",
                        errors::pascal(&ec.name),
                        ec.value
                    ));
                } else {
                    // Payload fields become constructor properties after the
                    // message, in declaration (and wire) order.
                    let fields: Vec<String> = ec
                        .fields
                        .iter()
                        .map(|f| format!("val {}: {}", kt_escape(&f.name), kotlin_type(&f.ty)))
                        .collect();
                    w.line(format!(
                        "class {}(message: String = \"{default_msg}\", {}) : {exc}({}, message)",
                        errors::pascal(&ec.name),
                        fields.join(", "),
                        ec.value
                    ));
                }
            }
            w.blank();
            w.line("companion object {");
            w.scope(|w| {
                w.line(format!(
                    "/** Map a raw `{}` code and payload to the typed exception; unknown codes yield the generic [{}]. */",
                    eb.name,
                    errors::EXCEPTION_BRAND
                ));
                w.line(format!(
                    "@JvmStatic fun fromCode(code: Int, message: String, payload: ByteArray?): {} = when (code) {{",
                    errors::EXCEPTION_BRAND
                ));
                w.scope(|w| {
                    for ec in &eb.codes {
                        let ctor = errors::pascal(&ec.name);
                        if ec.fields.is_empty() {
                            w.line(format!("{} -> {ctor}(message)", ec.value));
                        } else {
                            // A missing payload violates the contract for a
                            // code with declared fields; fall back to the
                            // generic exception rather than fabricate values.
                            let reads: Vec<String> = ec
                                .fields
                                .iter()
                                .map(|f| kt_read_expr(&f.ty, "r"))
                                .collect();
                            w.line(format!(
                                "{} -> if (payload != null) weaveDecode(payload) {{ r -> {ctor}(message, {}) }} else {}(code, message)",
                                ec.value,
                                reads.join(", "),
                                errors::EXCEPTION_BRAND
                            ));
                        }
                    }
                    w.line(format!(
                        "else -> {}(code, message)",
                        errors::EXCEPTION_BRAND
                    ));
                });
                w.line("}");
            });
            w.line("}");
        });
        w.line("}");
    }
    out.push_str(&w.finish());
}

/// Render a record as a plain Kotlin `data class` with typed properties, plus
/// the internal `pack{Name}`/`unpack{Name}` buffer codecs. Records are value
/// types crossing the ABI serialized in value buffers (fields in declaration
/// order); they have no C symbols, native handles, or disposal.
pub(crate) fn render_kotlin_struct(out: &mut String, s: &StructBinding) {
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &s.doc);
    if s.fields.is_empty() {
        w.line(format!("class {}", s.name));
    } else if s.fields.iter().any(|f| f.doc.is_some()) {
        w.line(format!("data class {}(", s.name));
        w.scope(|w| {
            for f in &s.fields {
                writer_doc(w, &f.doc);
                w.line(format!(
                    "val {}: {},",
                    kt_escape(&f.name),
                    kotlin_type(&f.ty)
                ));
            }
        });
        w.line(")");
    } else {
        let fields: Vec<String> = s
            .fields
            .iter()
            .map(|f| format!("val {}: {}", kt_escape(&f.name), kotlin_type(&f.ty)))
            .collect();
        w.line(format!("data class {}({})", s.name, fields.join(", ")));
    }
    out.push_str(&w.finish());
    render_kotlin_struct_codecs(out, s);
}

/// Render the internal buffer codecs for one record: `pack{Name}` writes the
/// fields in declaration order; `unpack{Name}` reads them back.
pub(crate) fn render_kotlin_struct_codecs(out: &mut String, s: &StructBinding) {
    let name = &s.name;
    let mut w = CodeWriter::four_space();
    w.blank();
    if s.fields.is_empty() {
        w.line(format!(
            "internal fun pack{name}(w: WeaveBufferWriter, v: {name}) {{}}"
        ));
        w.blank();
        w.line(format!(
            "internal fun unpack{name}(r: WeaveBufferReader): {name} = {name}()"
        ));
        out.push_str(&w.finish());
        return;
    }
    w.line(format!(
        "internal fun pack{name}(w: WeaveBufferWriter, v: {name}) {{"
    ));
    w.scope(|w| {
        for f in &s.fields {
            w.line(kt_write_field(&f.ty, "w", &f.name));
        }
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "internal fun unpack{name}(r: WeaveBufferReader): {name} = {name}("
    ));
    w.scope(|w| {
        for f in &s.fields {
            w.line(format!("{},", kt_read_expr(&f.ty, "r")));
        }
    });
    w.line(")");
    out.push_str(&w.finish());
}

/// The Kotlin expression converting a boxed element pulled from `nativeNext`
/// (typed `Any`, spelled `raw`) into the iterator's public element type.
pub(crate) fn kotlin_iter_elem_convert(elem: &Ty) -> String {
    // A buffered element crosses as a packed `ByteArray`: decode it into the
    // idiomatic Kotlin value.
    if elem.is_buffered() {
        return kt_decode_expr(elem, "(raw as ByteArray)");
    }
    match elem {
        Ty::Enum(name) => format!("{}.fromValue(raw as Int)", local_type_name(name)),
        Ty::Interface(name) => format!("{}.fromHandle(raw as Long)", local_type_name(name)),
        // Only `Interface?` reaches here: 0L crosses for none.
        Ty::Optional(inner) => {
            let Ty::Interface(name) = inner.as_ref() else {
                unreachable!("non-interface optionals are buffered")
            };
            format!(
                "(raw as Long).takeIf {{ it != 0L }}?.let {{ {}.fromHandle(it) }}",
                local_type_name(name)
            )
        }
        other => format!("raw as {}", kotlin_type(other)),
    }
}

/// Render the lazy Kotlin iterator wrapper class for one `iter<T>` callable.
/// The class implements `Iterator<T>` with a lookahead slot (one producer
/// `next` per consumer step) and `AutoCloseable` disposal through a
/// `WeaveNativeRef` registered with the shared `Cleaner`, so an abandoned
/// iterator's native handle is still destroyed exactly once.
pub(crate) fn render_kotlin_iterator_class(
    out: &mut String,
    f: &FnBinding,
    it: &IteratorBinding,
    c_prefix: &str,
) {
    let class = kotlin_iterator_class_name(it, c_prefix);
    let elem_pub = kotlin_type(&it.elem);
    let convert = kotlin_iter_elem_convert(&it.elem);
    let mut w = CodeWriter::four_space();
    w.blank();
    w.line("/**");
    w.line(format!(
        " * A lazy iterator over the `{}` elements streamed by [{}]. Each step pulls",
        elem_pub,
        kt_param(&f.name)
    ));
    w.line(" * exactly one element from the native producer. The native handle is");
    w.line(" * released when the producer is exhausted, when [close] is called, or by");
    w.line(" * the `Cleaner` if the iterator is abandoned, whichever comes first.");
    w.line(" */");
    w.line(format!(
        "class {class} internal constructor(handle: Long) : Iterator<{elem_pub}>, AutoCloseable {{"
    ));
    w.scope(|w| {
        w.line("private val ref = WeaveNativeRef(handle) { h -> nativeDestroy(h) }");
        w.line("private val cleanable = weaveCleaner.register(this, ref)");
        w.line("private var nextSlot: Array<Any?>? = null");
        w.blank();
        w.line("override fun hasNext(): Boolean {");
        w.scope(|w| {
            w.line("if (nextSlot != null) return true");
            w.line("if (ref.peek() == 0L) return false");
            w.line("val slot = nativeNext(ref.get())");
            w.line("if (slot == null) {");
            w.scope(|w| {
                w.line("close()");
                w.line("return false");
            });
            w.line("}");
            w.line("nextSlot = slot");
            w.line("return true");
        });
        w.line("}");
        w.blank();
        w.line(format!("override fun next(): {elem_pub} {{"));
        w.scope(|w| {
            w.line("if (!hasNext()) throw NoSuchElementException()");
            w.line("val raw = nextSlot!![0]");
            w.line("nextSlot = null");
            w.line(format!("return {convert}"));
        });
        w.line("}");
        w.blank();
        w.line("/** Releases the native iterator; safe to call more than once. */");
        w.line("override fun close() {");
        w.scope(|w| {
            w.line("cleanable.clean()");
        });
        w.line("}");
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line("init { WeaveNativeLibrary.ensureLoaded() }");
            w.blank();
            w.line("@JvmStatic private external fun nativeNext(handle: Long): Array<Any?>?");
            w.line("@JvmStatic private external fun nativeDestroy(handle: Long)");
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// Render the Kotlin class for one interface: the wrapper owns one strong
/// native reference through a `WeaveNativeRef` registered with the shared
/// `Cleaner`, so `destroy` runs exactly once, from `close()` (`AutoCloseable`)
/// or from the cleaner backstop. `cloneHandle()` exposes the `_clone` symbol
/// for the value-buffer codec. Companion factories wrap constructors (the
/// `new` constructor becomes `operator fun invoke`) and statics; instance
/// methods pass the borrowed handle as the leading native argument. Async
/// members become `suspend fun`s resuming through `WeaveContinuation` with
/// `error`-typed exception mapping.
pub(crate) fn render_kotlin_interface(
    out: &mut String,
    i: &InterfaceBinding,
    error: Option<&ErrorBinding>,
    c_prefix: &str,
) {
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &i.doc);
    // The raw-handle constructor is private (not internal) so that a `new`
    // constructor taking one integer, exposed as `operator fun invoke(Long)`,
    // can't be shadowed by it when a consumer is compiled in the same module:
    // Kotlin prefers a visible constructor over the invoke convention, and an
    // `internal` one would silently adopt the consumer's integer as a pointer.
    w.line(format!(
        "class {} private constructor(handle: Long) : AutoCloseable {{",
        i.name
    ));
    w.scope(|w| {
        w.line("private val ref = WeaveNativeRef(handle) { h -> nativeDestroy(h) }");
        w.line("private val cleanable = weaveCleaner.register(this, ref)");
        w.blank();
        w.line("/** The native pointer, borrowed for one call; throws after [close]. */");
        w.line("internal val handle: Long get() = ref.get()");
        w.blank();
        w.line("/** A new strong reference, written into value buffers as an object token. */");
        w.line("internal fun cloneHandle(): Long = nativeClone(ref.get())");
        w.blank();
        w.line("companion object {");
        w.scope(|w| {
            w.line("init { WeaveNativeLibrary.ensureLoaded() }");
            w.blank();
            w.line("/** Adopts one strong native reference (a constructor return, a `_clone`, or a buffer token) into a wrapper that owes its `_destroy`. */");
            w.line(format!(
                "internal fun fromHandle(handle: Long): {} = {}(handle)",
                i.name, i.name
            ));
            for f in i.constructors.iter().chain(i.statics.iter()) {
                w.line(interface_native_decl(f, false));
            }
            for f in &i.methods {
                w.line(interface_native_decl(f, true));
            }
            w.line("@JvmStatic private external fun nativeClone(handle: Long): Long");
            w.line("@JvmStatic private external fun nativeDestroy(handle: Long)");

            // Constructors are never async (validation rejects that), so each
            // is a plain factory; `new` becomes `operator fun invoke` so
            // construction reads as `Store(...)`.
            for c in &i.constructors {
                w.blank();
                writer_fn_doc(w, &c.doc, &camel_params(&c.params));
                let decl = if c.name == "new" {
                    "operator fun invoke".to_string()
                } else {
                    format!("fun {}", kt_param(&c.name))
                };
                let call = interface_native_call(c, None);
                write_kotlin_sync_wrapper(w, c, &decl, &call, c_prefix);
            }
            for f in &i.statics {
                w.blank();
                writer_fn_doc(w, &f.doc, &camel_params(&f.params));
                if f.is_async {
                    let mapper = kotlin_error_mapper(f, error);
                    splice(w, |o| {
                        render_kotlin_async_fun(
                            o,
                            f,
                            &kt_param(&f.name),
                            &interface_native_name(f),
                            false,
                            "",
                            false,
                            2,
                            &mapper,
                        )
                    });
                } else {
                    let decl = format!("fun {}", kt_param(&f.name));
                    let call = interface_native_call(f, None);
                    write_kotlin_sync_wrapper(w, f, &decl, &call, c_prefix);
                }
            }
        });
        w.line("}");

        for f in &i.methods {
            w.blank();
            writer_fn_doc(w, &f.doc, &camel_params(&f.params));
            if f.is_async {
                let mapper = kotlin_error_mapper(f, error);
                splice(w, |o| {
                    render_kotlin_async_fun(
                        o,
                        f,
                        &kt_param(&f.name),
                        &interface_native_name(f),
                        true,
                        "",
                        false,
                        1,
                        &mapper,
                    )
                });
            } else {
                let decl = format!("fun {}", kt_param(&f.name));
                let call = interface_native_call(f, Some("handle"));
                write_kotlin_sync_wrapper(w, f, &decl, &call, c_prefix);
            }
        }
        w.blank();

        w.line("/** Releases this wrapper's native reference; safe to call more than once. */");
        w.line("override fun close() {");
        w.scope(|w| {
            w.line("cleanable.clean()");
        });
        w.line("}");
    });
    w.line("}");
    out.push_str(&w.finish());
}

/// The Kotlin expression converting one callback-method argument as the JNI
/// trampoline delivers it (see [`kotlin_shim_param_type`]) into the public
/// type the implementation receives: buffers decode, enums round-trip through
/// `fromValue`, and object pointers are adopted into a new wrapper (`0L`
/// becomes `null` for `Interface?`).
fn kotlin_shim_arg_convert(t: &Ty, expr: &str) -> String {
    if t.is_buffered() {
        return kt_decode_expr(t, expr);
    }
    match t {
        Ty::Enum(name) => format!("{}.fromValue({expr})", local_type_name(name)),
        Ty::Interface(name) => format!("{}.fromHandle({expr})", local_type_name(name)),
        Ty::Optional(inner) => {
            let Ty::Interface(name) = inner.as_ref() else {
                unreachable!("non-interface optionals are buffered")
            };
            format!(
                "{expr}.takeIf {{ it != 0L }}?.let {{ {}.fromHandle(it) }}",
                local_type_name(name)
            )
        }
        _ => expr.to_string(),
    }
}

/// Render one callback interface: the public Kotlin `interface` the consumer
/// implements, plus the internal `{Name}Jni` dispatch object whose
/// `@JvmStatic` shims the native vtable trampolines call through cached
/// `jmethodID`s. Each shim takes the implementing object first, then every
/// argument in its trampoline form, converts them (decoding buffers and
/// adopting object references), and forwards to the implementation; an enum
/// return is lowered back to its raw `Int`.
pub(crate) fn render_kotlin_callback_interface(out: &mut String, cb: &CallbackInterfaceBinding) {
    let name = &cb.name;
    let dispatch = kotlin_callback_dispatch_name(name);
    let mut w = CodeWriter::four_space();
    w.blank();
    writer_doc(&mut w, &cb.doc);
    if let Some(msg) = &cb.deprecated {
        w.line(format!("@Deprecated(\"{}\")", msg.replace('"', "\\\"")));
    }
    w.line(format!("interface {name} {{"));
    w.scope(|w| {
        for m in &cb.methods {
            writer_fn_doc(w, &m.doc, &camel_params(&m.params));
            if let Some(msg) = &m.deprecated {
                w.line(format!("@Deprecated(\"{}\")", msg.replace('"', "\\\"")));
            }
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_type(&p.ty)))
                .collect();
            let ret = m
                .ret
                .as_ref()
                .map(|t| format!(": {}", kotlin_type(t)))
                .unwrap_or_default();
            w.line(format!(
                "fun {}({}){ret}",
                kt_param(&m.name),
                params.join(", ")
            ));
        }
    });
    w.line("}");
    w.blank();
    w.line(format!(
        "/** JNI dispatch shims for [{name}]; the native vtable trampolines call these, nothing else does. */"
    ));
    w.line(format!("internal object {dispatch} {{"));
    w.scope(|w| {
        for m in &cb.methods {
            let mut params = vec![format!("impl: {name}")];
            params.extend(
                m.params
                    .iter()
                    .map(|p| format!("{}: {}", kt_param(&p.name), kotlin_shim_param_type(&p.ty))),
            );
            let args: Vec<String> = m
                .params
                .iter()
                .map(|p| kotlin_shim_arg_convert(&p.ty, &kt_param(&p.name)))
                .collect();
            let call = format!("impl.{}({})", kt_param(&m.name), args.join(", "));
            let line = match &m.ret {
                None => format!(
                    "@JvmStatic fun {}({}) {{ {call} }}",
                    kt_param(&m.name),
                    params.join(", ")
                ),
                Some(Ty::Enum(_)) => format!(
                    "@JvmStatic fun {}({}): Int = {call}.value",
                    kt_param(&m.name),
                    params.join(", ")
                ),
                Some(t) => format!(
                    "@JvmStatic fun {}({}): {} = {call}",
                    kt_param(&m.name),
                    params.join(", "),
                    kotlin_shim_param_type(t)
                ),
            };
            w.line(line);
        }
    });
    w.line("}");
    out.push_str(&w.finish());
}
