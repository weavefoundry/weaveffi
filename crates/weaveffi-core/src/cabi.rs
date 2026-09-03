//! Shared rendering of the **C ABI declarations** from a
//! [`BindingModel`](crate::model::BindingModel).
//!
//! Both the C generator (which emits the canonical `{prefix}.h`) and the C++
//! generator (whose idiomatic wrapper opens an `extern "C"` block re-declaring
//! the same symbols) render their C declarations through this module. Before it
//! existed the two re-derived the ABI independently and drifted. Most visibly,
//! the C++ `extern "C"` block lowered `iter<T>` as a list and omitted callbacks
//! and listeners entirely. Routing both through one model-driven renderer makes
//! that class of drift impossible.

use std::collections::BTreeSet;
use std::fmt::Write;

use crate::abi::{AbiParam, CType};
use crate::codegen::common::{emit_doc as common_emit_doc, DocCommentStyle};
use crate::codegen::CodeWriter;
use crate::lang::{is_reserved, CPP_KEYWORDS, C_KEYWORDS};
use crate::model::{
    AbiFn, CallShape, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding, ModuleBinding,
};

/// The revision of the WeaveFFI C ABI the generators emit bindings for.
///
/// Mirrors `weaveffi_abi::ABI_VERSION`; the two are kept equal by a test in
/// the `weaveffi` facade crate, which depends on both. Generated consumers
/// embed this value and, where a load-time check is cheap, compare it against
/// the producer's exported `{prefix}_abi_version()` before making any other
/// call.
pub const ABI_VERSION: u32 = 1;

/// Emit a `/** ... */` doc comment at `indent`.
pub fn emit_doc(out: &mut String, doc: &Option<String>, indent: &str) {
    common_emit_doc(out, doc, indent, DocCommentStyle::Javadoc);
}

/// Join lowered ABI slots into a `"<c-type> <name>, ..."` declaration string.
///
/// Parameter names are the one position in a header where an IDL-chosen
/// identifier lands verbatim (every other name carries the symbol prefix), so
/// each is escaped with [`c_param_name`] before it's printed.
pub fn params_str(params: &[AbiParam], prefix: &str) -> String {
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), c_param_name(&p.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The spelling of an IDL parameter name inside a C prototype.
///
/// The header is consumed from C and, through `#ifdef __cplusplus` guards or
/// the C++ generator's inlined `extern "C"` block, from C++. A name reserved
/// in either language (`register`, `class`, `new`, ...) gains the shared
/// trailing-underscore escape so the same declaration compiles in both.
/// Derived slot names (`{name}_ptr`, `out_len`) never collide and pass
/// through unchanged.
#[must_use]
pub fn c_param_name(name: &str) -> String {
    if is_reserved(name, C_KEYWORDS) || is_reserved(name, CPP_KEYWORDS) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The export-visibility macro name for `prefix`, for example `WEAVEFFI_API`.
///
/// Every exported function prototype is tagged with this macro so a non-Rust
/// producer that implements the header can export the symbols under hidden
/// default visibility, and Windows consumers import them through `dllimport`.
/// See [`render_visibility_macros`] for the macro's definition.
fn export_macro(prefix: &str) -> String {
    format!("{}_API", prefix.to_uppercase())
}

/// The deprecation macro name for `prefix`, for example `WEAVEFFI_DEPRECATED`.
///
/// Used in place of a bare `__attribute__((deprecated))` so the marker also
/// compiles under MSVC (which spells it `__declspec(deprecated(...))`).
fn deprecated_macro(prefix: &str) -> String {
    format!("{}_DEPRECATED", prefix.to_uppercase())
}

/// Render the portable export-visibility and deprecation macros that the C ABI
/// declarations are tagged with.
///
/// The C ABI header is both consumed (callers link the prebuilt library) and,
/// for non-Rust producers, implemented directly (C, C++, or Zig supply the
/// symbols). A bare prototype exports nothing under hidden default visibility
/// (`-fvisibility=hidden`, the norm for release builds and the MSVC default),
/// so an implementing library compiled that way ships no usable symbols. These
/// macros fix that portably:
///
/// - `{PREFIX}_API` expands to `__declspec(dllexport)` when the producer
///   defines `{PREFIX}_BUILD`, `__declspec(dllimport)` otherwise on Windows,
///   `__attribute__((used, visibility("default")))` under Emscripten,
///   `__attribute__((visibility("default")))` on GCC and Clang, and nothing
///   elsewhere. The Emscripten spelling matches `EMSCRIPTEN_KEEPALIVE`: the
///   `used` attribute keeps every tagged symbol alive through Emscripten's
///   aggressive dead-code elimination, so the exports survive without the
///   producer enumerating them in `-sEXPORTED_FUNCTIONS`.
/// - `{PREFIX}_DEPRECATED(msg)` expands to the compiler's deprecation marker.
///
/// Both definitions are wrapped in `#ifndef` guards so a translation unit that
/// includes both the C header and the C++ header (which inlines the same
/// declarations) defines each macro only once. The names are derived from the
/// configured symbol prefix so two WeaveFFI libraries included together never
/// collide.
pub fn render_visibility_macros(out: &mut String, prefix: &str) {
    let body = r#"#ifndef @U@_API
#  if defined(_WIN32) || defined(__CYGWIN__)
#    ifdef @U@_BUILD
#      define @U@_API __declspec(dllexport)
#    else
#      define @U@_API __declspec(dllimport)
#    endif
#  elif defined(__EMSCRIPTEN__)
#    define @U@_API __attribute__((used, visibility("default")))
#  elif defined(__GNUC__) && (__GNUC__ >= 4)
#    define @U@_API __attribute__((visibility("default")))
#  else
#    define @U@_API
#  endif
#endif

#ifndef @U@_DEPRECATED
#  if defined(_MSC_VER)
#    define @U@_DEPRECATED(msg) __declspec(deprecated(msg))
#  elif defined(__GNUC__) || defined(__clang__)
#    define @U@_DEPRECATED(msg) __attribute__((deprecated(msg)))
#  else
#    define @U@_DEPRECATED(msg)
#  endif
#endif

"#;
    out.push_str(&body.replace("@U@", &prefix.to_uppercase()));
}

/// Render a full `{API} {ret} {symbol}({params});` declaration for a lowered
/// symbol, tagged with the export-visibility macro (see
/// [`render_visibility_macros`]).
pub fn fn_decl(out: &mut String, f: &AbiFn, prefix: &str) {
    let _ = writeln!(
        out,
        "{} {} {}({});",
        export_macro(prefix),
        f.ret.render_c(prefix),
        f.symbol,
        params_str(&f.params, prefix)
    );
}

/// Render the runtime typedefs and helper prototypes (`handle_t`, `error`,
/// `free_*`, `alloc`/`dealloc`, `cancel_token`) that every WeaveFFI C surface
/// depends on.
///
/// `alloc`/`dealloc` back the Wasm JavaScript glue, which stages strings,
/// bytes, and arrays into linear memory before each call. Native consumers
/// never call them, but a producer targeting WebAssembly (for example a C
/// library built with Emscripten) must export them; the generated
/// `{prefix}.c` convenience file provides malloc/free-backed defaults.
pub fn render_runtime_decls(out: &mut String, prefix: &str) {
    let api = export_macro(prefix);
    let upper = prefix.to_uppercase();
    let _ = write!(
        out,
        "/* The WeaveFFI C ABI revision this header was generated against. The\n   \
           producer exports {prefix}_abi_version() so a consumer can refuse to load\n   \
           a library built for a different revision instead of misreading its\n   \
           error struct or value buffers. */\n\
         #define {upper}_ABI_VERSION {ABI_VERSION}u\n\
         {api} uint32_t {prefix}_abi_version(void);\n\n\
         typedef uint64_t {prefix}_handle_t;\n\n\
         /* Error slot written by every fallible call. `payload_ptr`/`payload_len`\n   \
           hold the matched error code's fields serialized in the WeaveFFI value\n   \
           buffer format (null when the code declares no fields); both the message\n   \
           and the payload are released by {prefix}_error_clear. */\n\
         typedef struct {prefix}_error {{\n    \
           int32_t code;\n    \
           const char* message;\n    \
           const uint8_t* payload_ptr;\n    \
           size_t payload_len;\n\
         }} {prefix}_error;\n\n\
         {api} void {prefix}_error_clear({prefix}_error* err);\n\n\
         /* Async completion callbacks receive a heap-boxed error the consumer\n   \
           owns; {prefix}_error_free releases the message, the payload, and the\n   \
           box itself. Passing NULL is a safe no-op. */\n\
         {api} void {prefix}_error_free({prefix}_error* err);\n\
         {api} void {prefix}_free_string(const char* ptr);\n\
         {api} void {prefix}_free_bytes(uint8_t* ptr, size_t len);\n\n\
         /* Linear-memory allocator used by the Wasm JS glue to stage call\n   \
           arguments. Native consumers never call these; producers targeting\n   \
           WebAssembly must export them (the generated {prefix}.c provides\n   \
           malloc/free-backed defaults). */\n\
         {api} uint8_t* {prefix}_alloc(uint32_t size);\n\
         {api} void {prefix}_dealloc(uint8_t* ptr, uint32_t size);\n\n\
         typedef struct {prefix}_cancel_token {prefix}_cancel_token;\n\
         {api} {prefix}_cancel_token* {prefix}_cancel_token_create(void);\n\
         {api} void {prefix}_cancel_token_cancel({prefix}_cancel_token* token);\n\
         {api} bool {prefix}_cancel_token_is_cancelled(const {prefix}_cancel_token* token);\n\
         {api} void {prefix}_cancel_token_destroy({prefix}_cancel_token* token);\n\n",
    );
}

/// Render an enum's discriminant constants as a C `typedef enum` named
/// `type_name`. Multi-line when any variant is documented.
fn render_enum_constants(out: &mut String, e: &EnumBinding, type_name: &str) {
    let mut w = CodeWriter::four_space();
    w.doc(&e.doc, DocCommentStyle::Javadoc);
    if e.variants.iter().any(|v| v.doc.is_some()) {
        w.block("typedef enum {", format!("}} {type_name};"), |w| {
            let last = e.variants.len();
            for (i, v) in e.variants.iter().enumerate() {
                w.doc(&v.doc, DocCommentStyle::Javadoc);
                let comma = if i + 1 == last { "" } else { "," };
                w.line(format!("{} = {}{comma}", v.c_const, v.value));
            }
        });
    } else {
        let variants: Vec<String> = e
            .variants
            .iter()
            .map(|v| format!("{} = {}", v.c_const, v.value))
            .collect();
        w.line(format!(
            "typedef enum {{ {} }} {type_name};",
            variants.join(", ")
        ));
    }
    out.push_str(&w.finish());
}

/// Render a C-style enum typedef. Multi-line when any variant is documented.
pub fn render_enum_decl(out: &mut String, e: &EnumBinding) {
    render_enum_constants(out, e, &e.c_tag);
}

/// Render the *tag* enum of a rich (algebraic) enum, named `{c_tag}_Tag`.
/// A rich enum value crosses the ABI serialized in a value buffer whose first
/// field is an `int32_t` holding one of these discriminant constants.
fn render_rich_enum_tag_decl(out: &mut String, e: &EnumBinding) {
    let tag_enum = format!("{}_Tag", e.c_tag);
    render_enum_constants(out, e, &tag_enum);
}

/// Render an error domain's code constants as a C `typedef enum` named by the
/// domain's `c_tag`. These are the values a throwing function stores in
/// `{prefix}_error.code`.
fn render_error_domain_decl(out: &mut String, e: &ErrorBinding) {
    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "Error codes reported by throwing functions in the `{}` module tree.",
            e.owner_path.replace('_', ".")
        )),
        DocCommentStyle::Javadoc,
    );
    if e.codes.iter().any(|c| c.doc.is_some()) {
        w.block("typedef enum {", format!("}} {};", e.c_tag), |w| {
            let last = e.codes.len();
            for (i, c) in e.codes.iter().enumerate() {
                w.doc(&c.doc, DocCommentStyle::Javadoc);
                let comma = if i + 1 == last { "" } else { "," };
                w.line(format!("{} = {}{comma}", c.c_const, c.value));
            }
        });
    } else {
        let codes: Vec<String> = e
            .codes
            .iter()
            .map(|c| format!("{} = {}", c.c_const, c.value))
            .collect();
        w.line(format!(
            "typedef enum {{ {} }} {};",
            codes.join(", "),
            e.c_tag
        ));
    }
    out.push_str(&w.finish());
}

/// Phase 1a: enum and error-code definitions for one module. These reference
/// no other types, so they are emitted first across all modules.
pub fn render_module_enum_defs(out: &mut String, module: &ModuleBinding) {
    for e in &module.enums {
        if e.is_rich() {
            render_rich_enum_tag_decl(out, e);
        } else {
            render_enum_decl(out, e);
        }
    }
    if let Some(err) = &module.error {
        if err.declared_here {
            render_error_domain_decl(out, err);
        }
    }
}

/// Phase 1b: opaque interface/iterator forward typedefs for one module.
/// Pointers to these are all the C ABI ever uses, so a forward typedef is
/// sufficient and lets declarations in any module reference any type. Records
/// and rich enums declare no tags: they are value types crossing the ABI as
/// serialized buffers.
pub fn render_module_type_tags(out: &mut String, module: &ModuleBinding) {
    for i in &module.interfaces {
        let t = &i.c_tag;
        let _ = writeln!(out, "typedef struct {t} {t};");
    }
    for f in module.callables() {
        if let CallShape::Iterator(it) = &f.shape {
            let t = &it.iter_tag;
            let _ = writeln!(out, "typedef struct {t} {t};");
        }
    }
}

/// Record `ty`'s struct tag (if any) into `tags`, recursing through pointers.
fn walk_struct_tags(ty: &CType, prefix: &str, tags: &mut BTreeSet<String>) {
    match ty {
        CType::StructTag { .. } => {
            tags.insert(ty.render_c(prefix));
        }
        CType::Ptr { pointee, .. } => walk_struct_tags(pointee, prefix, tags),
        _ => {}
    }
}

/// Collect every struct tag reachable from one module's lowered ABI
/// signatures. With records and rich enums crossing by value, the struct tags
/// left in signatures are interface receivers and typed-handle targets; the
/// latter have no other declaration site, so [`render_decls`] forward-declares
/// any tag phase 1b did not already emit.
fn collect_signature_struct_tags(
    module: &ModuleBinding,
    prefix: &str,
    tags: &mut BTreeSet<String>,
) {
    let walk_fn = |f: &AbiFn, tags: &mut BTreeSet<String>| {
        for p in &f.params {
            walk_struct_tags(&p.ty, prefix, tags);
        }
        walk_struct_tags(&f.ret, prefix, tags);
    };
    for f in module.callables() {
        match &f.shape {
            CallShape::Sync(abi) => walk_fn(abi, tags),
            CallShape::Async(a) => {
                walk_fn(&a.launch, tags);
                for p in &a.callback_params {
                    walk_struct_tags(&p.ty, prefix, tags);
                }
            }
            CallShape::Iterator(it) => {
                walk_fn(&it.launch, tags);
                walk_fn(&it.next, tags);
            }
        }
    }
    for cb in &module.callbacks {
        for p in &cb.abi_params {
            walk_struct_tags(&p.ty, prefix, tags);
        }
    }
}

/// Phase 1c: callback / async-callback function-pointer typedefs for one
/// module. These may reference enums (by value) and structs (by pointer), so
/// they are emitted after every module's enums and type tags.
pub fn render_module_callback_types(out: &mut String, module: &ModuleBinding, prefix: &str) {
    for cb in &module.callbacks {
        emit_doc(out, &cb.doc, "");
        let _ = writeln!(
            out,
            "typedef void (*{})({});",
            cb.c_fn_type,
            params_str(&cb.abi_params, prefix)
        );
    }
    for f in module.callables() {
        if let CallShape::Async(a) = &f.shape {
            let _ = writeln!(
                out,
                "typedef void (*{})({});",
                a.callback_type,
                params_str(&a.callback_params, prefix)
            );
        }
    }
}

/// Render one callable's prototypes according to its call shape (sync, async
/// launcher, or iterator launch/next/destroy triple), with doc comment and
/// deprecation marker.
fn render_callable_decl(out: &mut String, f: &FnBinding, prefix: &str) {
    let api = export_macro(prefix);
    let deprecated = deprecated_macro(prefix);
    emit_doc(out, &f.doc, "");
    if let Some(msg) = &f.deprecated {
        let _ = writeln!(out, "{deprecated}(\"{}\")", msg.replace('"', "\\\""));
    }
    match &f.shape {
        CallShape::Iterator(it) => {
            let t = &it.iter_tag;
            fn_decl(out, &it.launch, prefix);
            fn_decl(out, &it.next, prefix);
            let _ = writeln!(out, "{api} void {}({t}* iter);", it.destroy_symbol);
        }
        CallShape::Async(a) => {
            fn_decl(out, &a.launch, prefix);
        }
        CallShape::Sync(abi) => {
            fn_decl(out, abi, prefix);
        }
    }
}

/// Render the function surface of one interface: constructors, statics,
/// methods, then the destructor. Assumes the opaque tag is already
/// forward-declared (phase 1b).
fn render_interface_fn_decls(out: &mut String, i: &InterfaceBinding, prefix: &str) {
    let api = export_macro(prefix);
    let tag = &i.c_tag;
    emit_doc(out, &i.doc, "");
    for c in &i.constructors {
        render_callable_decl(out, c, prefix);
    }
    for s in &i.statics {
        render_callable_decl(out, s, prefix);
    }
    for m in &i.methods {
        render_callable_decl(out, m, prefix);
    }
    let _ = writeln!(out, "{api} void {}({tag}* self);", i.destroy_symbol);
    out.push('\n');
}

/// Phase 2: every function prototype for one module: struct create/destroy/
/// getters and builders, interface members, listeners, then sync/async/
/// iterator functions. All type tags and callback typedefs are assumed already
/// emitted (phases 1a–1c). Caller controls the leading `// Module:` comment
/// and any framing.
pub fn render_module_fn_decls(out: &mut String, module: &ModuleBinding, prefix: &str) {
    let api = export_macro(prefix);
    for i in &module.interfaces {
        render_interface_fn_decls(out, i, prefix);
    }
    for l in &module.listeners {
        emit_doc(out, &l.doc, "");
        let _ = writeln!(
            out,
            "{api} uint64_t {}({} callback, void* context);",
            l.register_symbol, l.callback_c_fn_type
        );
        emit_doc(out, &l.doc, "");
        let _ = writeln!(out, "{api} void {}(uint64_t id);", l.unregister_symbol);
    }
    for f in &module.functions {
        render_callable_decl(out, f, prefix);
    }
}

/// Render the complete C ABI declaration surface for `modules` in
/// dependency-safe order: all enum definitions, then all opaque type tags, then
/// all callback typedefs, then per-module function prototypes. Emitting every
/// type tag before any function lets a parent module's function reference a
/// child module's struct: cross-module forward references the previous
/// per-module interleaving could not express.
///
/// The runtime decls (`handle_t`, `error`, `free_*`, cancel token) are *not*
/// emitted here; callers render those first (the C generator inserts its map
/// convention comment in between).
pub fn render_decls(
    out: &mut String,
    modules: &[ModuleBinding],
    prefix: &str,
    module_comments: bool,
) {
    for m in modules {
        render_module_enum_defs(out, m);
    }
    let mut declared: BTreeSet<String> = BTreeSet::new();
    for m in modules {
        render_module_type_tags(out, m);
        for i in &m.interfaces {
            declared.insert(i.c_tag.clone());
        }
        for f in m.callables() {
            if let CallShape::Iterator(it) = &f.shape {
                declared.insert(it.iter_tag.clone());
            }
        }
    }
    // Typed-handle targets are the one struct tag left in signatures with no
    // declaration of their own (records are value types now), so forward-
    // declare any tag phase 1b did not cover, e.g. `handle<other.Type>`.
    let mut used: BTreeSet<String> = BTreeSet::new();
    for m in modules {
        collect_signature_struct_tags(m, prefix, &mut used);
    }
    for t in used.difference(&declared) {
        let _ = writeln!(out, "typedef struct {t} {t};");
    }
    for m in modules {
        render_module_callback_types(out, m, prefix);
    }
    out.push('\n');
    for m in modules {
        if module_comments {
            let _ = writeln!(out, "// Module: {}", m.path);
        }
        render_module_fn_decls(out, m, prefix);
        out.push('\n');
    }
}
