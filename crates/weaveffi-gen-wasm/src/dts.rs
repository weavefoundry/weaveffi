//! TypeScript declaration (`.d.ts`) rendering: the error class surface,
//! record and enum type shapes, callback interfaces, ambient interface
//! classes, and the nested module interface the loader's promise resolves to.

use heck::ToUpperCamelCase;
use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    BindingModel, CallShape, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, ModuleBinding,
};
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::docs::{emit_doc, emit_fn_doc};
use crate::types::{
    emscripten_stub, js_cb_method_name, js_code_class_name, js_fn_name, js_param_name, ts_type_for,
};

/// Render the `<module_name>.d.ts` companion: error classes, record
/// interfaces, enum constants, rich-enum unions, callback interfaces, ambient
/// interface classes, the `<Name>Module` API shape, and the `load<Name>`
/// signature. Async members, callback interfaces, and the functions taking
/// them are omitted in Emscripten mode, turning their runtime stubs into
/// compile-time errors for TS consumers.
pub(crate) fn render_wasm_dts(
    model: &BindingModel,
    module_name: &str,
    input_basename: &str,
    filename: &str,
    emscripten: bool,
) -> String {
    let pascal_name = module_name.to_upper_camel_case();
    let interface_name = format!("{pascal_name}Module");
    let load_fn = format!("load{pascal_name}");
    let mut out = render_prelude(CommentStyle::DoubleSlash, input_basename);
    out.push_str("// Generated TypeScript declarations for WeaveFFI Wasm bindings\n\n");

    emit_dts_error_classes(&mut out, model);

    for mb in &model.modules {
        for s in &mb.structs {
            emit_doc(&mut out, &s.doc, "");
            out.push_str(&format!("export interface {} {{\n", s.name));
            for field in &s.fields {
                emit_doc(&mut out, &field.doc, "  ");
                out.push_str(&format!("  {}: {};\n", field.name, ts_type_for(&field.ty)));
            }
            out.push_str("}\n\n");
        }

        for e in &mb.enums {
            // A rich (algebraic) enum is a tagged plain-object union, not a
            // by-value discriminant constant.
            if e.is_rich() {
                emit_dts_rich_enum_type(&mut out, e);
                continue;
            }
            emit_doc(&mut out, &e.doc, "");
            // The const object holds the values; the same-named type alias
            // is their union, so `Mode` works in both value and type positions.
            out.push_str(&format!("export declare const {}: Readonly<{{\n", e.name));
            for v in &e.variants {
                emit_doc(&mut out, &v.doc, "  ");
                out.push_str(&format!("  {}: {};\n", v.name, v.value));
            }
            out.push_str("}>;\n");
            out.push_str(&format!(
                "export type {0} = (typeof {0})[keyof typeof {0}];\n\n",
                e.name
            ));
        }

        // Callback interfaces are unsupported (and never referenced) in
        // Emscripten mode, so their shapes are omitted with the functions
        // that take them.
        if !emscripten {
            for cb in &mb.callback_interfaces {
                emit_dts_callback_interface(&mut out, cb);
            }
        }

        for i in &mb.interfaces {
            emit_dts_interface_class(&mut out, mb, i, emscripten);
        }
    }

    out.push_str(&format!("export interface {interface_name} {{\n"));
    if model
        .modules
        .iter()
        .any(|m| !m.functions.is_empty() || !m.interfaces.is_empty())
    {
        // In Emscripten mode `_raw` is the loader's export-binding object, a
        // plain record, not a `WebAssembly.Exports`.
        if emscripten {
            out.push_str("  _raw: Record<string, unknown>;\n");
        } else {
            out.push_str("  _raw: WebAssembly.Exports;\n");
        }
        for module in model.roots() {
            render_dts_module_interface(&mut out, model, module, "  ", emscripten);
        }
    }
    out.push_str("}\n\n");

    if emscripten {
        out.push_str(&format!(
            "export function {load_fn}(module: object | Promise<object>): Promise<{interface_name}>;\n\n"
        ));
    } else {
        out.push_str(&format!(
            "export function {load_fn}(source: string | URL | BufferSource | WebAssembly.Module): Promise<{interface_name}>;\n\n"
        ));
    }
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, filename));
    out
}

/// Emit the TypeScript declaration for a rich (algebraic) enum: a
/// discriminated union of plain object shapes, one member per variant, keyed
/// by the string `tag`. Mirrors the runtime representation the buffer codecs
/// pack and unpack.
fn emit_dts_rich_enum_type(out: &mut String, e: &EnumBinding) {
    let name = &e.name;
    let mut w = CodeWriter::two_space();
    w.doc(&e.doc, DocCommentStyle::Javadoc);
    w.line(format!("export type {name} ="));
    w.scope(|w| {
        let last = e.variants.len().saturating_sub(1);
        for (i, v) in e.variants.iter().enumerate() {
            let fields: String = v
                .fields
                .iter()
                .map(|f| format!("; {}: {}", f.name, ts_type_for(&f.ty)))
                .collect();
            let term = if i == last { ";" } else { "" };
            w.line(format!("| {{ tag: \"{}\"{fields} }}{term}", v.name));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the TypeScript `interface` a consumer implements for one callback
/// interface: one method per declared method, camelCase, with the same
/// parameter and return typing as any other callable. Object parameters
/// arrive as wrappers the implementation owns (and should `close()`).
fn emit_dts_callback_interface(out: &mut String, cb: &CallbackInterfaceBinding) {
    let mut w = CodeWriter::two_space();
    let mut tags = Vec::new();
    if let Some(msg) = &cb.deprecated {
        tags.push(format!("@deprecated {msg}"));
    }
    let doc = cb
        .doc
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map_or_else(
            || {
                "Implemented by the consumer and passed to the producer; the producer \
                 calls these methods synchronously. An exception thrown from a method \
                 is reported to the producer as an error (code -4) and the method's \
                 default value is returned in its place."
                    .to_string()
            },
            |d| {
                format!(
                    "{d}\n\nImplemented by the consumer; the producer calls these methods \
                     synchronously. An exception thrown from a method is reported to the \
                     producer as an error (code -4)."
                )
            },
        );
    let mut rendered = String::new();
    emit_fn_doc(&mut rendered, &Some(doc), &[], "", &tags);
    w.raw(rendered);
    w.block(format!("export interface {} {{", cb.name), "}", |w| {
        let inner = w.indent_str();
        for m in &cb.methods {
            let mut tags = Vec::new();
            if let Some(msg) = &m.deprecated {
                tags.push(format!("@deprecated {msg}"));
            }
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &m.doc, &m.params, &inner, &tags);
            w.raw(doc);
            let params: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{}: {}", js_param_name(p), ts_type_for(&p.ty)))
                .collect();
            let ret = m
                .ret
                .as_ref()
                .map_or_else(|| "void".to_string(), ts_type_for);
            w.line(format!(
                "{}({}): {ret};",
                js_cb_method_name(&m.name),
                params.join(", ")
            ));
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The TypeScript parameter list for one callable: camelCase names typed by
/// [`ts_type_for`].
fn dts_params(f: &FnBinding) -> String {
    f.params
        .iter()
        .map(|p| format!("{}: {}", js_param_name(p), ts_type_for(&p.ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The TypeScript return annotation for one callable (`Promise<...>` when
/// async, `void` for no return).
fn dts_ret(f: &FnBinding) -> String {
    let base = f
        .ret
        .as_ref()
        .map(ts_type_for)
        .unwrap_or_else(|| "void".into());
    if f.is_async {
        format!("Promise<{base}>")
    } else {
        base
    }
}

/// The JSDoc tag list for one callable: `@deprecated` first when present, a
/// streaming note for iterator-returning callables, an ownership note for
/// object-returning callables, then the `@throws` tag matching the throws
/// split (the typed domain error for throwing callables, the generic brand
/// error otherwise).
fn dts_fn_tags(f: &FnBinding, error: Option<&ErrorBinding>) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(msg) = &f.deprecated {
        tags.push(format!("@deprecated {msg}"));
    }
    match (&f.shape, f.ret.as_ref()) {
        (CallShape::Iterator(_), _) => tags.push(
            "@returns A lazy iterator: one producer step per `next()` call. Exhaust it or \
             call `return()` to release the producer handle (a `for...of` loop does both \
             automatically); an abandoned iterator leaks the handle."
                .to_string(),
        ),
        (_, Some(ret)) if ret.interface_name().is_some() => tags.push(
            "@returns A wrapper owning one reference; `close()` it (or bind it with \
             `using`) when done."
                .to_string(),
        ),
        _ => {}
    }
    match error {
        Some(eb) if f.throws => tags.push(format!(
            "@throws {{{}}} on a domain error code",
            eb.type_name
        )),
        _ => tags.push(format!(
            "@throws {{{ERROR_BRAND}}} if the native call fails"
        )),
    }
    tags
}

/// Whether a callable is declared at all: in Emscripten mode async functions
/// and functions taking callback interfaces are runtime stubs, so leaving
/// them out makes the gap a compile-time error for TS consumers.
fn declared(f: &FnBinding, emscripten: bool) -> bool {
    !(emscripten && emscripten_stub(f))
}

/// Emit one module's member block inside the `<Name>Module` interface:
/// function signatures (stubbed ones omitted in Emscripten mode), `typeof`
/// bindings for the interface classes, and nested submodule blocks, skipping
/// subtrees with no declared content.
fn render_dts_module_interface(
    out: &mut String,
    model: &BindingModel,
    mb: &ModuleBinding,
    indent: &str,
    emscripten: bool,
) {
    fn tree_has_content(model: &BindingModel, mb: &ModuleBinding) -> bool {
        !mb.functions.is_empty()
            || !mb.interfaces.is_empty()
            || model.children(mb).any(|sub| tree_has_content(model, sub))
    }
    if !tree_has_content(model, mb) {
        return;
    }
    let error = mb.error.as_ref();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", mb.name), "};", |w| {
        let inner = w.indent_str();
        for f in mb.functions.iter().filter(|f| declared(f, emscripten)) {
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &f.doc, &f.params, &inner, &dts_fn_tags(f, error));
            w.raw(doc);
            w.line(format!(
                "{}({}): {};",
                js_fn_name(f),
                dts_params(f),
                dts_ret(f)
            ));
        }
        // The module object carries the interface class itself, so statics,
        // factories, and `new` are reachable as `api.kv.Store...`.
        for i in &mb.interfaces {
            w.line(format!("{}: typeof {};", i.name, i.name));
        }
        for sub in model.children(mb) {
            let mut tmp = String::new();
            render_dts_module_interface(&mut tmp, model, sub, &inner, emscripten);
            w.raw(tmp);
        }
    });
    out.push_str(&w.finish());
}

/// Emit the TypeScript declarations for the error surface: the generic brand
/// error, then one domain class per declaring module with its per-code
/// subclasses (each carrying a literal-typed `CODE` and any declared payload
/// fields) and the static aliases hung on the domain class.
fn emit_dts_error_classes(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    w.line("/** Base error for WeaveFFI failures: domain errors extend it, and it is");
    w.line(" * thrown directly for unknown codes, marshalling failures, producer");
    w.line(" * panics, and callback-interface implementations that raised (code -4).");
    w.line(" * Carries the stable ABI `code`. */");
    w.block(
        format!("export declare class {ERROR_BRAND} extends Error {{"),
        "}",
        |w| {
            w.line("constructor(code: number, message?: string);");
            w.line("code: number;");
        },
    );
    w.blank();
    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|eb| eb.declared_here) else {
            continue;
        };
        let domain = &eb.type_name;
        w.line(format!(
            "/** Base error for the `{}` module's error domain. */",
            m.path
        ));
        w.block(
            format!("export declare class {domain} extends {ERROR_BRAND} {{"),
            "}",
            |w| {
                for c in &eb.codes {
                    let class = js_code_class_name(&c.name);
                    w.line(format!("static readonly {class}: typeof {class};"));
                }
            },
        );
        w.blank();
        for c in &eb.codes {
            let class = js_code_class_name(&c.name);
            let doc = c
                .doc
                .clone()
                .filter(|d| !d.trim().is_empty())
                .or_else(|| Some(c.message.clone()));
            w.doc(&doc, DocCommentStyle::Javadoc);
            w.block(
                format!("export declare class {class} extends {domain} {{"),
                "}",
                |w| {
                    w.line("constructor(message?: string);");
                    w.line(format!("static readonly CODE: {};", c.value));
                    for f in &c.fields {
                        w.doc(&f.doc, DocCommentStyle::Javadoc);
                        w.line(format!("readonly {}: {};", f.name, ts_type_for(&f.ty)));
                    }
                },
            );
            w.blank();
        }
    }
    out.push_str(&w.finish());
}

/// Emit the TypeScript declaration for an interface: an ambient class whose
/// runtime binding is reached through the module object (`api.kv.Store`). The
/// canonical `new` constructor declares `constructor`; other constructors and
/// statics are static members; stubbed members are omitted in Emscripten mode.
/// Every class declares `close()`; the `Symbol.dispose` method is present at
/// runtime but left undeclared so the file type-checks without the
/// `esnext.disposable` lib.
fn emit_dts_interface_class(
    out: &mut String,
    mb: &ModuleBinding,
    i: &InterfaceBinding,
    emscripten: bool,
) {
    let error = mb.error.as_ref();
    let mut w = CodeWriter::two_space();
    let doc = i
        .doc
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .map_or_else(
            || "Wraps one reference to a producer object; release it with `close()`.".to_string(),
            |d| {
                format!(
                    "{d}\n\nWraps one reference to a producer object; release it with `close()`."
                )
            },
        );
    w.doc(&Some(doc), DocCommentStyle::Javadoc);
    w.block(format!("export declare class {} {{", i.name), "}", |w| {
        let inner = w.indent_str();
        if let Some(c) = i.constructors.iter().find(|c| c.name == "new") {
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &c.doc, &c.params, &inner, &dts_fn_tags(c, error));
            w.raw(doc);
            w.line(format!("constructor({});", dts_params(c)));
        }
        for c in i.constructors.iter().filter(|c| c.name != "new") {
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &c.doc, &c.params, &inner, &dts_fn_tags(c, error));
            w.raw(doc);
            w.line(format!(
                "static {}({}): {};",
                js_fn_name(c),
                dts_params(c),
                dts_ret(c)
            ));
        }
        for f in i.methods.iter().filter(|f| declared(f, emscripten)) {
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &f.doc, &f.params, &inner, &dts_fn_tags(f, error));
            w.raw(doc);
            w.line(format!(
                "{}({}): {};",
                js_fn_name(f),
                dts_params(f),
                dts_ret(f)
            ));
        }
        for f in i.statics.iter().filter(|f| declared(f, emscripten)) {
            let mut doc = String::new();
            emit_fn_doc(&mut doc, &f.doc, &f.params, &inner, &dts_fn_tags(f, error));
            w.raw(doc);
            w.line(format!(
                "static {}({}): {};",
                js_fn_name(f),
                dts_params(f),
                dts_ret(f)
            ));
        }
        w.line("/** Releases this wrapper's reference exactly once; later calls are");
        w.line(" * no-ops. Also reachable as `[Symbol.dispose]()` for `using`");
        w.line(" * declarations. A wrapper collected unclosed is released by a");
        w.line(" * finalizer where the runtime provides one. */");
        w.line("close(): void;");
    });
    w.blank();
    out.push_str(&w.finish());
}
