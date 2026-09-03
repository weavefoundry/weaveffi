//! The JS loader assembler: the `weaveffi_wasm.js` stub with its feature-gated
//! runtime, module-scope enum and error surfaces, loader-scoped interface
//! classes, and the nested API object the loader returns.

use heck::ToUpperCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackBinding, InterfaceBinding, ModuleBinding,
};
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::calls::{
    async_cb_wasm_params, collect_listener_callbacks, emit_js_callable, emit_js_listener_api,
    emit_js_listener_stub, emit_js_listener_trampoline, emit_stage_input,
};
use crate::codec::{emit_js_buffer_codecs, emit_js_buffer_runtime};
use crate::runtime::{
    emit_abi_version_check, emit_bytes_helpers, emit_check_err_ref, emit_error_slot_helpers,
    emit_iterator_class, emit_js_error_checkers, emit_js_error_classes, emit_string_helpers,
    emit_trampoline_helper,
};
use crate::types::{is_string_type, js_checker_name, js_param_name, JsDecl};

/// Every producer entry-point symbol the generated glue calls by name:
/// sync and iterator launchers, iterator `next`/destroy pairs, and interface
/// destroy symbols, deduplicated in first-use order. Async launchers are
/// excluded; Emscripten mode stubs them out, and this list exists to build
/// the Emscripten export-binding table.
pub(crate) fn collect_called_symbols(model: &BindingModel) -> Vec<String> {
    fn push_unique(syms: &mut Vec<String>, s: &str) {
        if !syms.iter().any(|x| x == s) {
            syms.push(s.to_string());
        }
    }
    let mut syms = Vec::new();
    for m in &model.modules {
        for f in m.callables() {
            match &f.shape {
                CallShape::Iterator(it) => {
                    push_unique(&mut syms, &f.c_base);
                    push_unique(&mut syms, &it.next.symbol);
                    push_unique(&mut syms, &it.destroy_symbol);
                }
                CallShape::Async(_) => {}
                CallShape::Sync(_) => push_unique(&mut syms, &f.c_base),
            }
        }
        for i in &m.interfaces {
            push_unique(&mut syms, &i.destroy_symbol);
        }
    }
    syms
}

/// Render the `<module_name>.js` loader: module-scope runtime helpers gated
/// on the features the API actually uses, exported enum objects and error
/// classes, and the async `load<Name>` function that instantiates (or, in
/// Emscripten mode, adopts) the wasm module and returns the nested API
/// object.
pub(crate) fn render_wasm_js_stub(
    model: &BindingModel,
    module_name: &str,
    prefix: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let load_fn = format!("load{pascal_name}");
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);

    // Interface members marshal like free functions, so every callable counts.
    let has_functions = model.modules.iter().any(|m| m.callables().next().is_some());
    // In Emscripten mode async functions are throwing stubs, so none of the
    // trampoline machinery (or its helpers) is emitted.
    let has_async = !emscripten
        && model
            .modules
            .iter()
            .flat_map(ModuleBinding::callables)
            .any(|f| f.is_async);
    // Listeners get real dispatch only in the standard loader; Emscripten
    // mode emits throwing stubs, so no trampolines or registry there either.
    let listener_cbs: Vec<(&str, &CallbackBinding)> = if emscripten {
        Vec::new()
    } else {
        collect_listener_callbacks(model)
    };
    let has_listeners = !listener_cbs.is_empty();
    // Records and rich enums are value types packed and unpacked by the
    // module-scope codecs; any of them (or any buffered type, or an error
    // payload) pulls in the buffer writer/reader runtime.
    let has_codecs = model
        .modules
        .iter()
        .any(|m| !m.structs.is_empty() || m.enums.iter().any(|e| e.is_rich()));
    let has_error_payloads = model.modules.iter().any(|m| {
        m.error
            .as_ref()
            .is_some_and(|e| e.declared_here && e.codes.iter().any(|c| !c.fields.is_empty()))
    });
    let needs_buf = has_codecs || has_error_payloads || model.any_type(&|t| t.is_buffered());
    // The buffer reader (and the codecs) reject malformed input by throwing
    // the brand error, so the error surface is needed whenever buffers are.
    let needs_err = has_functions || needs_buf;
    // Error messages always cross as C strings, so anything needing the error
    // helpers also needs the string-read helpers regardless of declared types.
    let needs_strings = needs_err || model.any_type(&is_string_type);
    // Buffered values are staged and released exactly like bytes, so the byte
    // helpers cover both.
    let needs_bytes = needs_buf || model.any_type(&|t| matches!(t, Ty::Bytes | Ty::BorrowedBytes));
    // Any iterator-returning callable pulls in the shared lazy-iterator
    // wrapper class.
    let has_iterators = model
        .modules
        .iter()
        .flat_map(ModuleBinding::callables)
        .any(|f| matches!(f.shape, CallShape::Iterator(_)));

    out.push_str("// WeaveFFI Wasm bindings (auto-generated)\n");
    out.push_str("//\n");
    if emscripten {
        out.push_str("// Boundary conventions for an Emscripten build:\n");
    } else {
        out.push_str("// Boundary conventions for a wasm32-unknown-unknown build:\n");
    }
    out.push_str("//\n");
    out.push_str("//   Objects   -> i32 pointer into linear memory (0 = null/absent)\n");
    out.push_str("//   Enums     -> i32 discriminant value\n");
    out.push_str("//   i64/u64   -> JavaScript BigInt\n");
    out.push_str("//   Strings   -> NUL-terminated UTF-8 (const char*); a single i32 pointer\n");
    out.push_str("//   Bytes     -> i32 data pointer + i32 length (out_len for returns)\n");
    out.push_str("//   Buffered  -> records, rich enums, optionals, lists, and maps cross\n");
    out.push_str("//                as one value buffer: i32 pointer + i32 length\n");
    out.push('\n');

    if !model.modules.is_empty() {
        emit_abi_version_check(&mut out);
    }

    if needs_err {
        emit_js_error_classes(&mut out, model);
    }

    if needs_strings {
        emit_string_helpers(&mut out);
    }

    if needs_bytes {
        emit_bytes_helpers(&mut out);
    }

    if needs_buf {
        emit_js_buffer_runtime(&mut out);
    }

    if needs_err {
        emit_error_slot_helpers(&mut out);
        emit_js_error_checkers(&mut out, model);
        if has_async {
            emit_check_err_ref(&mut out);
        }
    }

    if has_codecs {
        emit_js_buffer_codecs(&mut out, model);
    }

    if has_iterators {
        emit_iterator_class(&mut out);
    }

    if has_async || has_listeners {
        emit_trampoline_helper(&mut out);
    }

    for module in &model.modules {
        for e in &module.enums {
            // Rich (algebraic) enums are tagged plain-object unions handled by
            // the buffer codecs; only C-style enums surface as a by-value
            // discriminant object.
            if e.is_rich() {
                continue;
            }
            out.push_str(&format!("export const {} = Object.freeze({{\n", e.name));
            for v in &e.variants {
                out.push_str(&format!("  {}: {},\n", v.name, v.value));
            }
            out.push_str("});\n\n");
        }
    }

    out.push_str("/**\n");
    if emscripten {
        out.push_str(" * Load a WeaveFFI API from a pre-initialized Emscripten module.\n");
        out.push_str(" *\n");
        out.push_str(" * @param {Object|Promise<Object>} module - The initialized Emscripten\n");
        out.push_str(" *   module, or the promise returned by its `MODULARIZE` factory.\n");
        if model.modules.is_empty() {
            out.push_str(" * @returns {Promise<Object>} The Emscripten module.\n");
        } else {
            out.push_str(" * @returns {Promise<Object>} The API bindings.\n");
        }
    } else {
        out.push_str(" * Load a WeaveFFI Wasm module from the given URL.\n");
        out.push_str(" *\n");
        out.push_str(" * @param {string} url - URL to the `.wasm` file.\n");
        if model.modules.is_empty() {
            out.push_str(
                " * @returns {Promise<WebAssembly.Exports>} The exported Wasm functions.\n",
            );
        } else {
            out.push_str(" * @returns {Promise<Object>} The API bindings.\n");
        }
    }
    out.push_str(" *\n");
    out.push_str(" * Exported functions follow the C ABI naming convention:\n");
    out.push_str(&format!(
        " *   {prefix}_{{module}}_{{function}}(params...) -> result\n"
    ));
    out.push_str(" *\n");
    out.push_str(" * @example\n");
    if emscripten {
        out.push_str(" * import Module from './your_library.js';\n");
        out.push_str(&format!(" * const api = await {load_fn}(Module());\n"));
    } else {
        out.push_str(&format!(" * const api = await {load_fn}('lib.wasm');\n"));
    }
    out.push_str(" *\n");
    out.push_str(" * // Primitive: plain numbers in, number out.\n");
    out.push_str(" * const sum = api.math.add(1, 2);\n");
    out.push_str(" *\n");
    out.push_str(" * // Record: plain objects in and out (serialized automatically).\n");
    out.push_str(" * const person = api.contacts.create({ name: 'Ada', age: 36 });\n");
    out.push_str(" *\n");
    out.push_str(" * // Enum: pass the integer discriminant.\n");
    out.push_str(" * api.ui.set_color(0); // 0 = first variant\n");
    out.push_str(" *\n");
    out.push_str(" * // Optional: pass null to omit, a value to provide.\n");
    out.push_str(" * api.config.set_timeout(5000); // present\n");
    out.push_str(" * api.config.set_timeout(null); // absent\n");
    out.push_str(" *\n");
    out.push_str(" * // List/Map: pass arrays/objects; receive arrays/objects.\n");
    out.push_str(" * const names = api.data.all_names();\n");
    out.push_str(" */\n");
    if emscripten {
        out.push_str(&format!("export async function {load_fn}(module) {{\n"));
        out.push_str("  const m = await Promise.resolve(module);\n");
    } else {
        out.push_str(&format!("export async function {load_fn}(url) {{\n"));
        out.push_str("  const response = await fetch(url);\n");
        out.push_str("  const bytes = await response.arrayBuffer();\n");
        out.push_str("  const { instance } = await WebAssembly.instantiate(bytes, {});\n");
    }

    if model.modules.is_empty() {
        if emscripten {
            out.push_str("  return m;\n");
        } else {
            out.push_str("  return instance.exports;\n");
        }
    } else {
        if emscripten {
            // Bind the Emscripten exports once, up front, to the exact symbol
            // names the glue above calls. Module access stays in quoted
            // bracket notation so Closure Compiler's advanced property
            // renaming cannot break it, while the rest of the glue keeps
            // consistent dot access on this locally-constructed object.
            let mut bindings: Vec<(String, String)> = vec![
                (
                    "weaveffi_abi_version".to_string(),
                    format!("{prefix}_abi_version"),
                ),
                ("weaveffi_alloc".to_string(), format!("{prefix}_alloc")),
                ("weaveffi_dealloc".to_string(), format!("{prefix}_dealloc")),
            ];
            if needs_strings {
                bindings.push((
                    "weaveffi_free_string".to_string(),
                    format!("{prefix}_free_string"),
                ));
            }
            if needs_bytes {
                bindings.push((
                    "weaveffi_free_bytes".to_string(),
                    format!("{prefix}_free_bytes"),
                ));
            }
            if needs_err {
                bindings.push((
                    "weaveffi_error_clear".to_string(),
                    format!("{prefix}_error_clear"),
                ));
            }
            bindings.extend(collect_called_symbols(model).into_iter().map(|s| {
                let export = s.clone();
                (s, export)
            }));
            out.push_str("  // Bind the underscore-prefixed Emscripten exports to the symbol\n");
            out.push_str("  // names the glue above calls. Quoted bracket access keeps the\n");
            out.push_str("  // bindings safe under Closure Compiler's property renaming.\n");
            out.push_str("  const wasm = {\n");
            out.push_str("    // Emscripten replaces HEAPU8 when linear memory grows, so the\n");
            out.push_str("    // buffer is re-read on every access instead of captured once.\n");
            out.push_str("    get memory() { return { buffer: m['HEAPU8'].buffer }; },\n");
            for (name, export) in &bindings {
                out.push_str(&format!("    {name}: m['_{export}'],\n"));
            }
            out.push_str("  };\n\n");
        } else {
            out.push_str("  const wasm = instance.exports;\n\n");
        }
        out.push_str("  _checkAbiVersion(wasm);\n\n");

        if has_async || has_listeners {
            out.push_str("  const _table = wasm.__indirect_function_table;\n\n");
        }

        if has_async {
            out.push_str("  let _nextCtxId = 1;\n");
            out.push_str("  const _asyncContexts = new Map();\n\n");
            out.push_str("  function _asyncHandler(ctxId, errPtr, ...results) {\n");
            out.push_str("    const ctx = _asyncContexts.get(ctxId);\n");
            out.push_str("    if (!ctx) return;\n");
            out.push_str("    _asyncContexts.delete(ctxId);\n");
            out.push_str("    try {\n");
            out.push_str("      if (errPtr !== 0) _checkErrRef(wasm, errPtr, ctx.mkErr);\n");
            out.push_str(
                "      ctx.resolve(ctx.unwrap ? ctx.unwrap(wasm, ...results) : results[0]);\n",
            );
            out.push_str("    } catch (e) {\n");
            out.push_str("      ctx.reject(e);\n");
            out.push_str("    }\n");
            out.push_str("  }\n\n");

            let mut trampolines: Vec<(String, Vec<&'static str>)> = Vec::new();
            for f in model.modules.iter().flat_map(ModuleBinding::callables) {
                if f.is_async {
                    let params = async_cb_wasm_params(f.ret.as_ref());
                    let key = params.join("_");
                    if !trampolines.iter().any(|(k, _)| k == &key) {
                        trampolines.push((key, params));
                    }
                }
            }
            for (sig_key, params) in &trampolines {
                let params_js: Vec<String> = params.iter().map(|p| format!("'{p}'")).collect();
                out.push_str(&format!(
                    "  const _cbPtr_{sig_key} = _registerTrampoline(_table, [{}], _asyncHandler);\n",
                    params_js.join(", ")
                ));
            }
            out.push('\n');
        }

        if has_listeners {
            out.push_str("  // Listener subscriptions, keyed by the context id the loader\n");
            out.push_str("  // threads through the C ABI's void* context slot. Each entry\n");
            out.push_str("  // holds the JS callback and the producer's subscription id.\n");
            out.push_str("  let _nextLsnId = 1;\n");
            out.push_str("  const _listeners = new Map();\n\n");
            for (path, cb) in &listener_cbs {
                emit_js_listener_trampoline(&mut out, path, cb, "  ");
            }
            out.push('\n');
        }

        // Interface classes close over the loaded `wasm` instance (and the
        // async machinery above), so they live inside the loader rather than
        // at module scope like the value-type codecs.
        for module in &model.modules {
            for i in &module.interfaces {
                emit_interface_class(&mut out, module, i, "  ", prefix, emscripten);
            }
        }

        out.push_str("  return {\n");
        out.push_str("    _raw: wasm,\n");
        for module in model.roots() {
            render_js_module_object(&mut out, model, module, "    ", prefix, emscripten);
        }
        out.push_str("  };\n");
    }

    out.push_str("}\n\n");
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// Whether a module subtree exposes anything at runtime (functions, interface
/// classes, or listeners), so empty namespace objects are not emitted.
/// Records and rich enums contribute nothing here: they are plain value
/// shapes with no runtime members.
fn module_tree_has_content(model: &BindingModel, mb: &ModuleBinding) -> bool {
    !mb.functions.is_empty()
        || !mb.interfaces.is_empty()
        || !mb.listeners.is_empty()
        || model
            .children(mb)
            .any(|sub| module_tree_has_content(model, sub))
}

/// Emit one module's namespace object (`math: { ... },`) inside the returned
/// API literal: free-function wrappers, listener register/unregister pairs
/// (or their Emscripten stubs), the interface classes themselves (so
/// factories, statics, and `instanceof` checks reach them), and nested
/// submodule objects, skipping subtrees with no runtime content.
fn render_js_module_object(
    out: &mut String,
    model: &BindingModel,
    mb: &ModuleBinding,
    indent: &str,
    prefix: &str,
    emscripten: bool,
) {
    if !module_tree_has_content(model, mb) {
        return;
    }
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", mb.name), "},", |w| {
        let inner = w.indent_str();
        for f in &mb.functions {
            let mut tmp = String::new();
            emit_js_callable(
                &mut tmp,
                mb,
                f,
                JsDecl::Object,
                None,
                &inner,
                prefix,
                emscripten,
            );
            w.raw(tmp);
        }
        for l in &mb.listeners {
            let mut tmp = String::new();
            if emscripten {
                emit_js_listener_stub(&mut tmp, l, &inner);
            } else {
                emit_js_listener_api(&mut tmp, l, &inner);
            }
            w.raw(tmp);
        }
        // The interface class itself is exposed on the module object, so
        // factories, statics, and `instanceof` checks all reach it.
        for i in &mb.interfaces {
            w.line(format!("{}: {},", i.name, i.name));
        }
        for sub in model.children(mb) {
            let mut tmp = String::new();
            render_js_module_object(&mut tmp, model, sub, &inner, prefix, emscripten);
            w.raw(tmp);
        }
    });
    out.push_str(&w.finish());
}

/// Emit the loader-scoped `class` for an interface: an opaque-handle wrapper
/// closing over the loaded `wasm` instance. The canonical `new` constructor
/// maps to `constructor`; other constructors and statics are static methods;
/// methods pass `this._handle` as the implicit leading `self` argument. The
/// internal `_wrap(handle)` adopts an owned handle (returns, iterator
/// elements) without invoking the constructor, and `free()` releases the
/// handle exactly once via the destroy symbol.
fn emit_interface_class(
    out: &mut String,
    module: &ModuleBinding,
    i: &InterfaceBinding,
    indent: &str,
    prefix: &str,
    emscripten: bool,
) {
    let cls = &i.name;
    let error = module.error.as_ref();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    if let Some(doc) = i.doc.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        for line in doc.lines() {
            w.line(format!("// {line}"));
        }
    }
    w.block(format!("class {cls} {{"), "}", |w| {
        let inner = w.indent_str();

        // Canonical constructor: `new(...)` becomes `constructor(...)`,
        // assigning the owned handle rather than returning a wrapped value.
        if let Some(c) = i.constructors.iter().find(|c| c.name == "new") {
            let body = format!("{inner}  ");
            let js_params: Vec<String> = c.params.iter().map(js_param_name).collect();
            let checker = js_checker_name(c, error);
            w.block(
                format!("constructor({}) {{", js_params.join(", ")),
                "}",
                |w| {
                    let mut staged = String::new();
                    let mut args = Vec::new();
                    let mut cleanup = Vec::new();
                    for (idx, p) in c.params.iter().enumerate() {
                        emit_stage_input(
                            &mut staged,
                            &body,
                            p,
                            &format!("a{idx}"),
                            &module.path,
                            &mut args,
                            &mut cleanup,
                        );
                    }
                    args.push("_err".to_string());
                    w.raw(staged);
                    w.line("const _err = _allocErr(wasm);");
                    w.line(format!(
                        "const _r = wasm.{}({});",
                        c.c_base,
                        args.join(", ")
                    ));
                    for stmt in &cleanup {
                        w.line(stmt);
                    }
                    w.line(format!("{checker}(wasm, _err);"));
                    w.line("_freeErr(wasm, _err);");
                    w.line("this._handle = _r;");
                },
            );
        }

        // Internal: adopt an owned handle (returns, iterator elements)
        // without running the constructor.
        w.block("static _wrap(handle) {", "}", |w| {
            w.line(format!("const _o = Object.create({cls}.prototype);"));
            w.line("_o._handle = handle;");
            w.line("return _o;");
        });

        // Explicit cleanup: release the producer-owned handle exactly once.
        w.block("free() {", "}", |w| {
            w.block("if (this._handle !== 0) {", "}", |w| {
                w.line(format!("wasm.{}(this._handle);", i.destroy_symbol));
                w.line("this._handle = 0;");
            });
        });

        for c in i.constructors.iter().filter(|c| c.name != "new") {
            let mut tmp = String::new();
            emit_js_callable(
                &mut tmp,
                module,
                c,
                JsDecl::Static,
                None,
                &inner,
                prefix,
                emscripten,
            );
            w.raw(tmp);
        }
        for m in &i.methods {
            let mut tmp = String::new();
            emit_js_callable(
                &mut tmp,
                module,
                m,
                JsDecl::Method,
                Some("this._handle"),
                &inner,
                prefix,
                emscripten,
            );
            w.raw(tmp);
        }
        for s in &i.statics {
            let mut tmp = String::new();
            emit_js_callable(
                &mut tmp,
                module,
                s,
                JsDecl::Static,
                None,
                &inner,
                prefix,
                emscripten,
            );
            w.raw(tmp);
        }
    });
    w.blank();
    out.push_str(&w.finish());
}
