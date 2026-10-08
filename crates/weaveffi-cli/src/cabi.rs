//! Shared rendering of the **C ABI declarations** from a
//! [`Model`].
//!
//! Both the C generator (which emits the canonical `{library}.h`) and the C++
//! generator (whose idiomatic wrapper opens an `extern "C"` block re-declaring
//! the same symbols) render their C declarations through this module, so the
//! two can't drift.
//!
//! The normative description of what is rendered here is
//! `docs/src/reference/abi.md`.

use std::fmt::Write;

use crate::codegen::common::{emit_doc, DocCommentStyle};
use crate::codegen::CodeWriter;
use crate::lang::{is_reserved, CPP_KEYWORDS, C_KEYWORDS};
use weaveffi_model::abi::AbiParam;
use weaveffi_model::model::{
    contract_check_symbol, contract_symbol, AbiFn, CallShape, CallbackInterfaceBinding,
    EnumBinding, ErrorBinding, FnBinding, InterfaceBinding, Model, ModuleBinding,
};

/// The revision of the WeaveFFI C ABI the generators emit bindings for.
///
/// The C ABI revision the generated headers declare. Mirrors
/// `weaveffi::abi::ABI_VERSION`; the two are kept equal by a test in this
/// crate.
pub use weaveffi_model::model::ABI_VERSION;

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

/// Render the runtime surface every producer exports (see the C ABI
/// contract): the ABI revision check, the error struct and its helpers, the
/// allocation functions, cancel tokens, and the debug leak counter, followed
/// by each top-level module's contract table (see [`render_contract`]).
pub fn render_runtime_decls(out: &mut String, model: &Model) {
    let prefix = model.prefix();
    let api = export_macro(prefix);
    let upper = prefix.to_uppercase();
    let _ = write!(
        out,
        "/* The C ABI revision this header was generated against. The producer\n   \
           exports {prefix}_abi_version() so a consumer can refuse to load a\n   \
           library built for a different revision. */\n\
         #define {upper}_ABI_VERSION {ABI_VERSION}u\n\
         {api} uint32_t {prefix}_abi_version(void);\n\n\
         /* Error slot written by every fallible call. `message` is NUL-terminated\n   \
           UTF-8. `payload_ptr`/`payload_len` hold the matched error code's fields\n   \
           serialized in the value buffer format (null when the code declares no\n   \
           fields). Both are released by {prefix}_error_clear. Positive codes are\n   \
           the API's declared error codes; negative codes are runtime traps:\n   \
           -1 generic, -2 producer panic, -3 marshalling failure, -4 a callback\n   \
           interface implementation failed, -5 cancelled. */\n\
         typedef struct {prefix}_error {{\n    \
           int32_t code;\n    \
           const char* message;\n    \
           const uint8_t* payload_ptr;\n    \
           size_t payload_len;\n\
         }} {prefix}_error;\n\n\
         /* Fill `err` with `code` and a producer-owned copy of `message`. Callback\n   \
           interface implementations call this to report a failure without\n   \
           allocating with a foreign allocator. */\n\
         {api} void {prefix}_error_set({prefix}_error* err, int32_t code, const char* message);\n\
         /* Replace `err`'s payload with a producer-owned copy of `len` bytes: the\n   \
           fields of a declared error code, which a callback method that throws\n   \
           attaches after {prefix}_error_set. */\n\
         {api} void {prefix}_error_set_payload({prefix}_error* err, const uint8_t* ptr, size_t len);\n\
         {api} void {prefix}_error_clear({prefix}_error* err);\n\n\
         /* Async completions receive a heap-boxed error the consumer owns;\n   \
           {prefix}_error_free releases the message, the payload, and the box.\n   \
           NULL is a no-op. */\n\
         {api} void {prefix}_error_free({prefix}_error* err);\n\n\
         /* Allocate a zero-filled run of `len` bytes (NULL for 0). A callback\n   \
           method returns a string, bytes, or buffer as such a run, which the\n   \
           producer adopts; any other run is released with {prefix}_free_bytes. */\n\
         {api} uint8_t* {prefix}_alloc(size_t len);\n\
         /* Release a string, bytes, or value buffer the producer returned, or a\n   \
           run from {prefix}_alloc, with its exact length. NULL or 0 is a no-op. */\n\
         {api} void {prefix}_free_bytes(uint8_t* ptr, size_t len);\n\n\
         /* Cancellation for `cancellable` async functions. A token starts with\n   \
           one reference owned by the consumer; the producer takes its own for\n   \
           each call it is passed to, so the consumer may cancel and destroy its\n   \
           reference at any time. A cancelled call completes with code -5. */\n\
         typedef struct {prefix}_cancel_token {prefix}_cancel_token;\n\
         {api} {prefix}_cancel_token* {prefix}_cancel_token_create(void);\n\
         {api} void {prefix}_cancel_token_cancel({prefix}_cancel_token* token);\n\
         {api} bool {prefix}_cancel_token_is_cancelled(const {prefix}_cancel_token* token);\n\
         {api} void {prefix}_cancel_token_destroy({prefix}_cancel_token* token);\n\n\
         /* Live-allocation counters for leak checks in tests: 0 objects,\n   \
           1 callbacks, 2 iterators, 3 cancel tokens, 4 returned allocations.\n   \
           Kind -1 is 1 when the producer counts at all (its `leak-check`\n   \
           feature) and 0 when every counter reads 0 regardless. */\n\
         {api} uint64_t {prefix}_debug_live(int32_t kind);\n\n\
         /* One declaration's fingerprint in a module's contract table: FNV-1a 64\n   \
           of its dotted path and of its canonical signature. */\n\
         typedef struct {prefix}_contract_entry {{\n    \
           uint64_t id;\n    \
           uint64_t hash;\n\
         }} {prefix}_contract_entry;\n\n",
    );
    for root in model.roots() {
        render_contract(out, model, root);
    }
}

/// Render one top-level module's contract: the producer's table function,
/// the entries these bindings were generated with as
/// `{PREFIX}_{MODULE}_CONTRACT` and `{PREFIX}_{MODULE}_CONTRACT_LEN`, and a
/// `static inline` checker returning the id of the first expected entry the
/// producer lacks or hashes differently (`0` when compatible).
pub fn render_contract(out: &mut String, model: &Model, root: &ModuleBinding) {
    let prefix = model.prefix();
    let api = export_macro(prefix);
    let symbol = contract_symbol(prefix, &root.name);
    let check = contract_check_symbol(prefix, &root.name);
    let upper = symbol.to_uppercase();
    let entries = weaveffi_model::contract::entries(model, root);
    let _ = writeln!(
        out,
        "/* The contract of module `{}`: one entry per declaration, sorted by\n   \
         id. The producer's table comes from the function below; a consumer\n   \
         checks each entry it was generated with against it once, before any\n   \
         other call (producer entries it doesn't know are fine). */\n\
         {api} const {prefix}_contract_entry* {symbol}(size_t* out_len);\n\
         #define {upper}_LEN {}\n\
         #define {upper} {{ \\",
        root.name,
        entries.len()
    );
    if entries.is_empty() {
        out.push_str("    {0ull, 0ull} \\\n");
    }
    for e in &entries {
        let _ = writeln!(
            out,
            "    {{0x{:016x}ull, 0x{:016x}ull}}, /* {} */ \\",
            e.id, e.hash, e.path
        );
    }
    out.push_str("}\n");
    if entries.is_empty() {
        let _ = write!(
            out,
            "/* Nothing to check: the module declares nothing. */\n\
             static inline uint64_t {check}(void) {{\n    \
               return 0;\n\
             }}\n\n"
        );
        return;
    }
    let _ = write!(
        out,
        "/* The id of the first expected entry the loaded library lacks or\n   \
           declares differently, or 0 when these bindings match it. */\n\
         static inline uint64_t {check}(void) {{\n    \
           static const {prefix}_contract_entry expected[] = {upper};\n    \
           size_t len = 0;\n    \
           const {prefix}_contract_entry* table = {symbol}(&len);\n    \
           for (size_t i = 0; i < {upper}_LEN; i++) {{\n        \
             size_t lo = 0, hi = len;\n        \
             while (lo < hi) {{\n            \
               size_t mid = lo + (hi - lo) / 2;\n            \
               if (table[mid].id < expected[i].id) lo = mid + 1; else hi = mid;\n        \
             }}\n        \
             if (lo == len || table[lo].id != expected[i].id || table[lo].hash != expected[i].hash) {{\n            \
               return expected[i].id;\n        \
             }}\n    \
           }}\n    \
           return 0;\n\
         }}\n\n"
    );
}

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
fn render_error_domain_decl(out: &mut String, e: &ErrorBinding, owner: &str) {
    let mut w = CodeWriter::four_space();
    w.doc(
        &Some(format!(
            "Error codes reported by throwing functions in the `{owner}` module tree."
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
    if let Some(err) = &module.errors {
        render_error_domain_decl(out, err, &module.dot_path);
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

/// Render one callback interface's vtable struct: the fixed header (`size`,
/// `flags`, `free`), then one function pointer per method in declaration
/// order.
fn render_vtable_decl(
    out: &mut String,
    cb: &CallbackInterfaceBinding,
    domain: Option<&ErrorBinding>,
    prefix: &str,
) {
    let mut w = CodeWriter::four_space();
    let contract = "Consumer-implemented callback interface. The consumer passes a context \
                    pointer plus a pointer to a static instance of this vtable, with `size` \
                    set to `sizeof` the vtable and `flags` to 0 (the producer rejects a smaller \
                    vtable). The producer may call any entry from any thread until it calls \
                    `free(ctx)` exactly once, also from any thread. A method reports failure \
                    with `error_set(out_err, ...)`; a string, bytes, or buffer return is a \
                    run the consumer allocates with `alloc` and writes to `out_ptr` and \
                    `out_len`, which the producer adopts.";
    w.doc(
        &Some(match &cb.doc {
            Some(doc) => format!("{doc}\n\n{contract}"),
            None => contract.to_string(),
        }),
        DocCommentStyle::Javadoc,
    );
    w.block(
        format!("typedef struct {} {{", cb.vtable_tag),
        format!("}} {};", cb.vtable_tag),
        |w| {
            w.line("uint32_t size;");
            w.line("uint32_t flags;");
            w.line("void (*free)(void* ctx);");
            for m in &cb.methods {
                let throws = match (m.throws, domain) {
                    (true, Some(d)) => Some(format!(
                        "May fail with a `{}` code (its fields attached with \
                         `{prefix}_error_set_payload`).",
                        d.c_tag
                    )),
                    _ => None,
                };
                let doc = match (&m.doc, throws) {
                    (Some(doc), Some(t)) => Some(format!("{doc}\n\n{t}")),
                    (doc, t) => doc.clone().or(t),
                };
                w.doc(&doc, DocCommentStyle::Javadoc);
                w.line(format!(
                    "{} (*{})({});",
                    m.abi_ret.render_c(prefix),
                    c_param_name(&m.name),
                    params_str(&m.abi_params, prefix)
                ));
            }
        },
    );
    out.push_str(&w.finish());
}

/// Phase 1c: callback-interface vtables and async completion-callback
/// function-pointer typedefs for one module. These may reference enums (by
/// value) and interfaces (by pointer), so they are emitted after every
/// module's enums and type tags.
pub fn render_module_callback_types(
    out: &mut String,
    module: &ModuleBinding,
    modules: &[ModuleBinding],
    prefix: &str,
) {
    // The domain in scope: the module's own, else the nearest ancestor's.
    let domain = (1..=module.segments.len()).rev().find_map(|n| {
        modules
            .iter()
            .find(|m| m.segments[..] == module.segments[..n])
            .and_then(|m| m.errors.as_ref())
    });
    for cb in &module.callback_interfaces {
        render_vtable_decl(out, cb, domain, prefix);
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
    emit_doc(out, &f.doc, "", DocCommentStyle::Javadoc);
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
/// methods, then the reference-count pair. Assumes the opaque tag is already
/// forward-declared (phase 1b).
fn render_interface_fn_decls(out: &mut String, i: &InterfaceBinding, prefix: &str) {
    let api = export_macro(prefix);
    let tag = &i.c_tag;
    emit_doc(out, &i.doc, "", DocCommentStyle::Javadoc);
    for c in &i.constructors {
        render_callable_decl(out, c, prefix);
    }
    for s in &i.statics {
        render_callable_decl(out, s, prefix);
    }
    for m in &i.methods {
        render_callable_decl(out, m, prefix);
    }
    emit_doc(
        out,
        &Some(
            "Returns a new strong reference to the same object (the pointer value is \
             unchanged). Null is a no-op returning null."
                .to_string(),
        ),
        "",
        DocCommentStyle::Javadoc,
    );
    let _ = writeln!(out, "{api} {tag}* {}(const {tag}* self);", i.clone_symbol);
    emit_doc(
        out,
        &Some(
            "Releases one strong reference; the object is dropped when the last reference \
             is released. Null is a no-op."
                .to_string(),
        ),
        "",
        DocCommentStyle::Javadoc,
    );
    let _ = writeln!(out, "{api} void {}({tag}* self);", i.destroy_symbol);
    out.push('\n');
}

/// Phase 2: every function prototype for one module: interface members, then
/// sync/async/iterator functions. All type tags, vtables, and callback
/// typedefs are assumed already emitted (phases 1a-1c). Caller controls the
/// leading `// Module:` comment and any framing.
pub fn render_module_fn_decls(out: &mut String, module: &ModuleBinding, prefix: &str) {
    for i in &module.interfaces {
        render_interface_fn_decls(out, i, prefix);
    }
    for f in &module.functions {
        render_callable_decl(out, f, prefix);
    }
}

/// Render the complete C ABI declaration surface for `modules` in
/// dependency-safe order: all enum definitions, then all opaque type tags, then
/// all vtables and callback typedefs, then per-module function prototypes.
/// Emitting every type tag before any function lets a parent module's function
/// reference a child module's interface: cross-module forward references a
/// per-module interleaving could not express.
///
/// The runtime decls (`error`, `free_*`, cancel token) are *not* emitted here;
/// callers render those first (the C generator inserts its map convention
/// comment in between).
pub fn render_decls(
    out: &mut String,
    modules: &[ModuleBinding],
    prefix: &str,
    module_comments: bool,
) {
    for m in modules {
        render_module_enum_defs(out, m);
    }
    for m in modules {
        render_module_type_tags(out, m);
    }
    for m in modules {
        render_module_callback_types(out, m, modules, prefix);
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
