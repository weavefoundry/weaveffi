//! Rendering of the **C ABI declarations** from a [`Model`]: the body of
//! `{library}.h`.
//!
//! The header is the C ABI every other target binds to. The C target emits
//! it, and the C++, Swift, Go, Kotlin (JNI shim), and Node.js (addon)
//! targets ship or include the same file, so everything here renders from
//! the lowered model: every slot list is an [`AbiFn`] the model computed,
//! never re-derived.
//!
//! The normative description of what is rendered here is
//! `docs/src/reference/abi.md`.

use std::fmt::Write;

use crate::codegen::common::{self, DocCommentStyle};
use crate::codegen::contract::{self, ContractTable};
use crate::codegen::docs::{ApiNames, Doc};
use crate::codegen::errors::{self, ErrorTable};
use crate::codegen::CodeWriter;
use crate::lang::{is_reserved, CPP_KEYWORDS, C_KEYWORDS};
use weaveffi_model::abi::AbiParam;
use weaveffi_model::model::{
    AbiFn, CallShape, CallbackInterfaceBinding, CallbackMethodBinding, EnumBinding, FnBinding,
    InterfaceBinding, Model, ModuleBinding,
};
use weaveffi_model::plan::ErrorStrategy;

/// The C ABI revision the generated headers declare. Mirrors
/// `weaveffi::abi::ABI_VERSION`; a test in this crate keeps the two equal.
pub(crate) use weaveffi_model::model::ABI_VERSION;

/// The width generated prose in the header wraps at.
const WRAP: usize = 76;

/// Join lowered ABI slots into a `"<c-type> <name>, ..."` declaration string.
///
/// Parameter names are the one position in a header where an IDL-chosen
/// identifier lands verbatim (every other name carries the symbol prefix), so
/// each is escaped with [`c_param_name`] before it's printed. An empty list
/// renders as `void`.
pub(crate) fn params_str(params: &[AbiParam], prefix: &str) -> String {
    if params.is_empty() {
        return "void".to_string();
    }
    params
        .iter()
        .map(|p| format!("{} {}", p.ty.render_c(prefix), c_param_name(&p.name)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The spelling of an IDL parameter (or field) name inside C source.
///
/// The header is consumed from C and, through its `extern "C"` guards, from
/// C++. A name reserved in either language (`register`, `class`, `new`, ...)
/// gains a trailing underscore so the same declaration compiles in both.
/// Derived slot names (`{name}_ptr`, `has_{name}`, `out_len`) never collide
/// and pass through unchanged.
#[must_use]
pub(crate) fn c_param_name(name: &str) -> String {
    if is_reserved(name, C_KEYWORDS) || is_reserved(name, CPP_KEYWORDS) {
        format!("{name}_")
    } else {
        name.to_string()
    }
}

/// The export-visibility macro, `{PREFIX}_API` (see
/// [`render_visibility_macros`]).
fn export_macro(prefix: &str) -> String {
    format!("{}_API", prefix.to_uppercase())
}

/// The deprecation macro, `{PREFIX}_DEPRECATED`, used in place of a bare
/// `__attribute__((deprecated))` so the marker also compiles under MSVC.
fn deprecated_macro(prefix: &str) -> String {
    format!("{}_DEPRECATED", prefix.to_uppercase())
}

/// `s` as the contents of a C string literal.
fn c_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

/// Word-wrap generated prose at [`WRAP`] columns.
fn wrap(text: &str) -> String {
    common::wrap(text, WRAP)
}

/// Render the portable export-visibility and deprecation macros the C ABI
/// declarations are tagged with.
///
/// The header is both consumed (callers link the prebuilt library) and, for
/// non-Rust producers, implemented directly. A bare prototype exports
/// nothing under hidden default visibility (`-fvisibility=hidden`, the norm
/// for release builds and the MSVC default), so these macros fix that
/// portably:
///
/// - `{PREFIX}_API` expands to `__declspec(dllexport)` when the producer
///   defines `{PREFIX}_BUILD`, `__declspec(dllimport)` otherwise on Windows,
///   `__attribute__((visibility("default")))` on GCC and Clang, and nothing
///   elsewhere.
/// - `{PREFIX}_DEPRECATED(msg)` expands to the compiler's deprecation marker.
///
/// Both are `#ifndef`-guarded and named from the symbol prefix, so two
/// WeaveFFI libraries included together never collide.
pub(crate) fn render_visibility_macros(out: &mut String, prefix: &str) {
    let body = r#"#ifndef @U@_API
#  if defined(_WIN32) || defined(__CYGWIN__)
#    ifdef @U@_BUILD
#      define @U@_API __declspec(dllexport)
#    else
#      define @U@_API __declspec(dllimport)
#    endif
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

/// Render a `{API} {ret} {symbol}({params});` declaration for a lowered
/// symbol, tagged with the export-visibility macro.
pub(crate) fn fn_decl(out: &mut String, f: &AbiFn, prefix: &str) {
    let _ = writeln!(
        out,
        "{} {} {}({});",
        export_macro(prefix),
        f.ret.render_c(prefix),
        f.symbol,
        params_str(&f.params, prefix)
    );
}

/// Everything the declaration renderers share: the model, its symbol
/// prefix, and the identifier index that rewrites backticked API names in
/// docs to their C spelling.
pub(crate) struct Decls<'m> {
    model: &'m Model,
    prefix: &'m str,
    names: ApiNames,
}

impl<'m> Decls<'m> {
    /// A renderer for `model`'s declarations.
    pub(crate) fn new(model: &'m Model) -> Self {
        Self {
            model,
            prefix: model.prefix(),
            names: ApiNames::new(model),
        }
    }

    /// The C spelling of a backticked identifier in a doc: the symbol, tag,
    /// or constant it names (when it names exactly one declaration).
    fn spell(&self, ident: &str) -> Option<String> {
        self.names.c_name(ident).map(str::to_string)
    }

    /// A declaration's doc with its deprecation note, in C spelling.
    fn doc_with_deprecation(
        &self,
        doc: &Option<String>,
        deprecated: &Option<String>,
    ) -> Option<String> {
        Doc::new(doc, deprecated).with_deprecation(|i| self.spell(i))
    }

    /// Render the runtime surface every producer exports (the ABI revision
    /// check, the error struct and its helpers, allocation, cancel tokens,
    /// the vtable flags, and the debug leak counter), followed by each
    /// top-level module's contract table.
    pub(crate) fn runtime(&self, out: &mut String) {
        let p = self.prefix;
        let api = export_macro(p);
        let upper = p.to_uppercase();
        let _ = write!(
            out,
            "/* The C ABI revision this header was generated against. The producer\n   \
               exports {p}_abi_version() so a consumer can refuse to load a\n   \
               library built for a different revision. */\n\
             #define {upper}_ABI_VERSION {ABI_VERSION}u\n\
             {api} uint32_t {p}_abi_version(void);\n\n\
             /* Error slot written by every fallible call. `code` is 0 on success,\n   \
               positive for an error domain's code, and negative for a runtime\n   \
               failure: -1 an untyped error (`throws: any`), -2 a producer panic, -3 a\n   \
               marshalling failure, -4 a callback interface implementation failed,\n   \
               -5 cancelled. The message is `message_len` bytes of UTF-8 at\n   \
               `message_ptr`, NOT NUL-terminated. `payload_ptr`/`payload_len` hold\n   \
               the code's fields in the value buffer format (NULL when it declares\n   \
               none). The producer owns both runs; {p}_error_clear releases them. */\n\
             typedef struct {p}_error {{\n    \
               int32_t code;\n    \
               const uint8_t* message_ptr;\n    \
               size_t message_len;\n    \
               const uint8_t* payload_ptr;\n    \
               size_t payload_len;\n\
             }} {p}_error;\n\n\
             /* Fill `err` with `code` and a producer-owned copy of the `message_len`\n   \
               bytes at `message_ptr` (UTF-8), replacing what it held. Callback\n   \
               implementations report failures this way, never by allocating the\n   \
               message themselves. */\n\
             {api} void {p}_error_set({p}_error* err, int32_t code, const uint8_t* message_ptr, size_t message_len);\n\
             /* Replace `err`'s payload with a producer-owned copy of `len` bytes: the\n   \
               fields of a domain code, which a callback method attaches after\n   \
               {p}_error_set. */\n\
             {api} void {p}_error_set_payload({p}_error* err, const uint8_t* ptr, size_t len);\n\
             /* Release `err`'s message and payload and reset it to success. */\n\
             {api} void {p}_error_clear({p}_error* err);\n\n\
             /* Async completions receive a heap-boxed error the consumer owns;\n   \
               {p}_error_free releases the message, the payload, and the box.\n   \
               NULL is a no-op. */\n\
             {api} void {p}_error_free({p}_error* err);\n\n\
             /* Allocate a zero-filled, 8-aligned run of `len` bytes (NULL for 0). A\n   \
               callback method returns a string, bytes, buffer, or typed array as\n   \
               such a run, which the producer adopts. */\n\
             {api} uint8_t* {p}_alloc(size_t len);\n\
             /* Release a run the producer returned (a string, bytes, value buffer,\n   \
               or typed array: `len` is its size in bytes, `count * sizeof(T)` for\n   \
               a typed array) or one from {p}_alloc. NULL or 0 is a no-op. */\n\
             {api} void {p}_free_bytes(uint8_t* ptr, size_t len);\n\n\
             /* Cancellation for `cancellable` async functions. A token starts with\n   \
               one reference owned by the consumer; the producer takes its own for\n   \
               each call it is passed to, so the consumer may cancel and destroy its\n   \
               reference at any time. A cancelled call completes with code -5. */\n\
             typedef struct {p}_cancel_token {p}_cancel_token;\n\
             {api} {p}_cancel_token* {p}_cancel_token_create(void);\n\
             {api} void {p}_cancel_token_cancel({p}_cancel_token* token);\n\
             {api} bool {p}_cancel_token_is_cancelled(const {p}_cancel_token* token);\n\
             {api} void {p}_cancel_token_destroy({p}_cancel_token* token);\n\n\
             /* A callback interface vtable's `flags` bit: the methods that return a\n   \
               value (a non-void C return or any out slot) may only be called on\n   \
               the thread that passed the vtable to the producer, which fails such\n   \
               a call from any other thread with -4 without calling it. */\n\
             #define {upper}_VTABLE_THREAD_AFFINE 1u\n\n\
             /* Live-allocation counters for leak checks in tests: 0 objects,\n   \
               1 callbacks, 2 iterators, 3 cancel tokens, 4 returned allocations.\n   \
               Kind -1 is 1 when the producer counts at all (its `leak-check`\n   \
               feature) and 0 when every counter reads 0 regardless. */\n\
             {api} uint64_t {p}_debug_live(int32_t kind);\n\n\
             /* One declaration's fingerprint in a module's contract table: FNV-1a 64\n   \
               of its dotted path and of its canonical signature. */\n\
             typedef struct {p}_contract_entry {{\n    \
               uint64_t id;\n    \
               uint64_t hash;\n\
             }} {p}_contract_entry;\n\n",
        );
        for table in contract::tables(self.model) {
            self.contract(out, &table);
        }
    }

    /// Render one top-level module's contract: the producer's table
    /// function, the rows these bindings were generated with as
    /// `{PREFIX}_{MODULE}_CONTRACT` and `{PREFIX}_{MODULE}_CONTRACT_LEN`
    /// (each row commented with its path and canonical signature), and a
    /// `static inline` checker returning the id of the first expected row
    /// the producer lacks or hashes differently (`0` when compatible).
    fn contract(&self, out: &mut String, table: &ContractTable<'_>) {
        let p = self.prefix;
        let api = export_macro(p);
        let symbol = &table.symbol;
        let check = &table.check_symbol;
        let upper = symbol.to_uppercase();
        let _ = writeln!(
            out,
            "/* The contract of module `{}`: one entry per declaration, sorted by\n   \
             id. The producer's table comes from the function below; a consumer\n   \
             checks each entry it was generated with against it once, before any\n   \
             other call (producer entries it doesn't know are fine). */\n\
             {api} const {p}_contract_entry* {symbol}(size_t* out_len);\n\
             #define {upper}_LEN {}\n\
             #define {upper} {{ \\",
            table.root.name,
            table.rows.len()
        );
        if table.rows.is_empty() {
            out.push_str("    {0ull, 0ull} \\\n");
        }
        for row in &table.rows {
            let _ = writeln!(
                out,
                "    {{{}ull, {}ull}}, /* {}: {} */ \\",
                contract::hex(row.id),
                contract::hex(row.hash),
                row.path,
                row.signature.replace("*/", "* /")
            );
        }
        out.push_str("}\n");
        if table.rows.is_empty() {
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
               static const {p}_contract_entry expected[] = {upper};\n    \
               size_t len = 0;\n    \
               const {p}_contract_entry* table = {symbol}(&len);\n    \
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

    /// Render an `int32_t` typedef named `type_name` and an anonymous enum of
    /// its constants (`(doc, name, value)`), multi-line when any constant is
    /// documented. Enum types are never `typedef enum`: a C enum's size is
    /// the compiler's choice, and every enum crosses the ABI as `int32_t`.
    fn int_enum(
        &self,
        out: &mut String,
        doc: Option<String>,
        type_name: &str,
        constants: &[(Option<String>, &str, i32)],
    ) {
        let mut w = CodeWriter::four_space();
        w.doc(&doc, DocCommentStyle::Javadoc);
        w.line(format!("typedef int32_t {type_name};"));
        if constants.iter().any(|(doc, ..)| doc.is_some()) {
            w.block("enum {", "};", |w| {
                let last = constants.len();
                for (i, (doc, name, value)) in constants.iter().enumerate() {
                    w.doc(doc, DocCommentStyle::Javadoc);
                    let comma = if i + 1 == last { "" } else { "," };
                    w.line(format!("{name} = {value}{comma}"));
                }
            });
        } else {
            let constants: Vec<String> = constants
                .iter()
                .map(|(_, name, value)| format!("{name} = {value}"))
                .collect();
            w.line(format!("enum {{ {} }};", constants.join(", ")));
        }
        out.push_str(&w.finish());
    }

    /// A C-style enum, or a rich enum's tag type `{c_tag}_Tag` (a rich enum
    /// value crosses in a value buffer whose first field is the tag).
    fn enumeration(&self, out: &mut String, e: &EnumBinding) {
        let doc = self.doc_with_deprecation(&e.doc, &e.deprecated);
        let (type_name, doc) = if e.is_rich() {
            let note = format!(
                "The variant tag of `{}`, the first field of its value-buffer encoding.",
                e.name
            );
            let doc = match doc {
                Some(doc) => format!("{doc}\n\n{note}"),
                None => note,
            };
            (format!("{}_Tag", e.c_tag), Some(doc))
        } else {
            (e.c_tag.clone(), doc)
        };
        let constants: Vec<(Option<String>, &str, i32)> = e
            .variants
            .iter()
            .map(|v| {
                let doc = Doc::new(&v.doc, &None).text(|i| self.spell(i));
                (doc, v.c_const.as_str(), v.value)
            })
            .collect();
        self.int_enum(out, doc, &type_name, &constants);
    }

    /// An error domain's code type and constants: the values a function
    /// declared `throws: {Domain}` stores in `{prefix}_error.code`.
    fn error_domain(&self, out: &mut String, table: &ErrorTable<'_>) {
        let d = table.domain;
        let doc = wrap(&format!(
            "Error codes of the `{}` domain of module `{}` (`{}` in the language \
             bindings), which a function declared `throws: {}` reports in \
             `{}_error.code`. Domains are open: a newer library may report a positive \
             code not listed here, which keeps its message.",
            d.name, table.module.dot_path, table.type_name, d.name, self.prefix
        ));
        let constants: Vec<(Option<String>, &str, i32)> = table
            .codes
            .iter()
            .map(|row| {
                let doc = Doc::new(&row.code.doc, &None).text(|i| self.spell(i));
                (doc, row.code.c_const.as_str(), row.code.value)
            })
            .collect();
        self.int_enum(out, Some(doc), &d.c_tag, &constants);
    }

    /// Phase 1a: every enum, rich-enum tag, and error-domain code type.
    /// These reference no other types, so they come first.
    fn enum_defs(&self, out: &mut String) {
        for m in &self.model.modules {
            for e in &m.enums {
                self.enumeration(out, e);
            }
        }
        for table in errors::tables(self.model, "Error") {
            self.error_domain(out, &table);
        }
    }

    /// Phase 1b: opaque interface and iterator types for one module. The C
    /// ABI only ever uses pointers to these, so a forward typedef is enough
    /// and lets declarations in any module reference them. Records and rich
    /// enums declare no tags: they cross serialized.
    fn type_tags(&self, out: &mut String, module: &ModuleBinding) {
        for i in &module.interfaces {
            let t = &i.c_tag;
            let _ = writeln!(out, "typedef struct {t} {t};");
        }
        for f in module.callables() {
            if let Some(it) = f.iterator() {
                let t = &it.iter_tag;
                let _ = writeln!(out, "typedef struct {t} {t};");
            }
        }
    }

    /// What a callable's failure may carry, for its doc: a domain's codes
    /// or an untyped -1 (runtime codes are possible everywhere).
    fn call_failure(&self, error: &ErrorStrategy) -> Option<String> {
        match error {
            ErrorStrategy::Trap => None,
            ErrorStrategy::Domain(name) => Some(format!(
                "Fails with a `{}` code (and its fields as a payload when it declares any).",
                self.model.error_domain(name).c_tag,
            )),
            ErrorStrategy::Untyped => Some("Fails with code -1 and a message.".to_string()),
        }
    }

    /// What a callback method may report, for its doc.
    fn method_failure(&self, m: &CallbackMethodBinding) -> Option<String> {
        match &m.error {
            ErrorStrategy::Trap => None,
            ErrorStrategy::Domain(name) => Some(wrap(&format!(
                "May fail with a `{}` code (attaching its fields with \
                 `{}_error_set_payload` when it declares any).",
                self.model.error_domain(name).c_tag,
                self.prefix
            ))),
            ErrorStrategy::Untyped => Some("May fail with code -1 and a message.".to_string()),
        }
    }

    /// One callback interface's vtable struct: the fixed header (`size`,
    /// `flags`, `free`), then one function pointer per method in
    /// declaration order.
    fn vtable(&self, out: &mut String, cb: &CallbackInterfaceBinding) {
        let p = self.prefix;
        let upper = p.to_uppercase();
        let mut w = CodeWriter::four_space();
        let contract = wrap(&format!(
            "A consumer-implemented callback interface. The consumer passes a context \
             pointer and a pointer to a vtable that outlives it, with `size` set to \
             `sizeof` the vtable (the producer rejects a smaller one) and `flags` to 0 or \
             `{upper}_VTABLE_THREAD_AFFINE`. The producer may call any method from any \
             thread (with `{upper}_VTABLE_THREAD_AFFINE`, a method that returns a value \
             only from the thread that passed the vtable) until it calls `free(ctx)` \
             exactly once, also from any thread. A method reports failure with \
             `{p}_error_set(out_err, ...)`. It returns a string, bytes, buffer, or typed \
             array as a run it allocates with `{p}_alloc` and stores in `*out_ptr` and \
             `*out_len` (an element count for a typed array), which the producer adopts, \
             and an optional scalar as a `bool` C return (present) with the value in \
             `*out_value`."
        ));
        let doc = match self.doc_with_deprecation(&cb.doc, &cb.deprecated) {
            Some(doc) => format!("{doc}\n\n{contract}"),
            None => contract,
        };
        w.doc(&Some(doc), DocCommentStyle::Javadoc);
        w.block(
            format!("typedef struct {} {{", cb.vtable_tag),
            format!("}} {};", cb.vtable_tag),
            |w| {
                w.line("uint32_t size;");
                w.line("uint32_t flags;");
                w.line("void (*free)(void* ctx);");
                for m in &cb.methods {
                    let doc = self.doc_with_deprecation(&m.doc, &m.deprecated);
                    let doc = match (doc, self.method_failure(m)) {
                        (Some(doc), Some(t)) => Some(format!("{doc}\n\n{t}")),
                        (doc, t) => doc.or(t),
                    };
                    w.doc(&doc, DocCommentStyle::Javadoc);
                    w.line(format!(
                        "{} (*{})({});",
                        m.abi.ret.render_c(p),
                        c_param_name(&m.abi.symbol),
                        params_str(&m.abi.params, p)
                    ));
                }
            },
        );
        out.push_str(&w.finish());
    }

    /// Phase 1c: callback-interface vtables and async completion-callback
    /// typedefs for one module. These may reference enums (by value) and
    /// interfaces (by pointer), so they follow every module's enums and
    /// type tags.
    fn callback_types(&self, out: &mut String, module: &ModuleBinding) {
        for cb in &module.callback_interfaces {
            self.vtable(out, cb);
        }
        for f in module.callables() {
            if let CallShape::Async(a) = &f.shape {
                let _ = writeln!(
                    out,
                    "typedef void (*{})({});",
                    a.callback_type,
                    params_str(&a.callback_params, self.prefix)
                );
            }
        }
    }

    /// One callable's prototypes with its doc and deprecation marker: the
    /// sync entry point, the async launcher, or the iterator launcher with
    /// its `_next` and `_destroy`.
    fn callable(&self, out: &mut String, f: &FnBinding) {
        let p = self.prefix;
        let doc = Doc::new(&f.doc, &f.deprecated);
        let text = match (doc.text(|i| self.spell(i)), self.call_failure(&f.error)) {
            (Some(text), Some(failure)) => Some(format!("{text}\n\n{}", wrap(&failure))),
            (text, failure) => text.or(failure.map(|f| wrap(&f))),
        };
        let mut w = CodeWriter::four_space();
        w.doc(&text, DocCommentStyle::Javadoc);
        out.push_str(&w.finish());
        if let Some(msg) = doc.deprecation(|i| self.spell(i)) {
            let _ = writeln!(out, "{}(\"{}\")", deprecated_macro(p), c_string(&msg));
        }
        fn_decl(out, &f.abi, p);
        if let Some(it) = f.iterator() {
            fn_decl(out, &it.next, p);
            let _ = writeln!(
                out,
                "{} void {}({}* iter);",
                export_macro(p),
                it.destroy_symbol,
                it.iter_tag
            );
        }
    }

    /// One interface's functions: constructors, statics, methods, then the
    /// reference-count pair. The opaque tag is already declared (phase 1b).
    fn interface_fns(&self, out: &mut String, i: &InterfaceBinding) {
        let p = self.prefix;
        let api = export_macro(p);
        let tag = &i.c_tag;
        let mut w = CodeWriter::four_space();
        w.doc(
            &self.doc_with_deprecation(&i.doc, &i.deprecated),
            DocCommentStyle::Javadoc,
        );
        out.push_str(&w.finish());
        for f in i.constructors.iter().chain(&i.statics).chain(&i.methods) {
            self.callable(out, f);
        }
        let _ = writeln!(
            out,
            "/** Returns a new strong reference to the same object (the pointer value is \
             unchanged). Null is a no-op returning null. */\n\
             {api} {tag}* {}(const {tag}* self);\n\
             /** Releases one strong reference; the object is dropped when the last reference \
             is released. Null is a no-op. */\n\
             {api} void {}({tag}* self);\n",
            i.clone_symbol, i.destroy_symbol
        );
    }

    /// Render every declaration in dependency-safe order: all enum and
    /// error-code types, then all opaque type tags, then all vtables and
    /// completion typedefs, then each module's functions under a
    /// `// Module:` comment. Emitting every type before any function lets a
    /// parent module's function reference a child module's interface.
    pub(crate) fn declarations(&self, out: &mut String) {
        self.enum_defs(out);
        for m in &self.model.modules {
            self.type_tags(out, m);
        }
        for m in &self.model.modules {
            self.callback_types(out, m);
        }
        out.push('\n');
        for m in &self.model.modules {
            let _ = writeln!(out, "// Module: {}", m.path);
            for i in &m.interfaces {
                self.interface_fns(out, i);
            }
            for f in &m.functions {
                self.callable(out, f);
            }
            out.push('\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_parameter_names_are_escaped() {
        assert_eq!(c_param_name("class"), "class_");
        assert_eq!(c_param_name("register"), "register_");
        assert_eq!(c_param_name("count"), "count");
    }

    #[test]
    fn string_literals_escape_quotes() {
        assert_eq!(c_string(r#"say "hi" \ now"#), r#"say \"hi\" \\ now"#);
    }
}
