//! TypeScript declaration (`.d.ts`) rendering: the error class surface,
//! record and enum type shapes, ambient interface classes, and the nested
//! module interface the loader's promise resolves to.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use weaveffi_core::codegen::common::DocCommentStyle;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{
    BindingModel, CallShape, EnumBinding, ErrorBinding, FnBinding, InterfaceBinding,
    ListenerBinding, ModuleBinding,
};
use weaveffi_core::utils::{render_prelude, render_trailer, CommentStyle};

use crate::docs::{emit_doc, emit_fn_doc};
use crate::types::{js_code_class_name, js_fn_name, js_param_name, ts_type_for};

/// Render the `<module_name>.d.ts` companion: error classes, record
/// interfaces, enum constants, rich-enum unions, ambient interface classes,
/// the `<Name>Module` API shape, and the `load<Name>` signature. Async
/// members and listeners are omitted in Emscripten mode, turning their
/// runtime stubs into compile-time errors for TS consumers.
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
            "export function {load_fn}(url: string): Promise<{interface_name}>;\n\n"
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
/// streaming note for iterator-returning callables, then the `@throws` tag
/// matching the throws split (the typed domain error for throwing callables,
/// the generic brand error otherwise).
fn dts_fn_tags(f: &FnBinding, error: Option<&ErrorBinding>) -> Vec<String> {
    let mut tags = Vec::new();
    if let Some(msg) = &f.deprecated {
        tags.push(format!("@deprecated {msg}"));
    }
    if matches!(f.shape, CallShape::Iterator(_)) {
        tags.push(
            "@returns A lazy iterator: one producer step per `next()` call. Exhaust it or \
             call `return()` to release the producer handle (a `for...of` loop does both \
             automatically); an abandoned iterator leaks the handle."
                .to_string(),
        );
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

/// Emit one module's member block inside the `<Name>Module` interface:
/// function signatures (async ones omitted in Emscripten mode), listener
/// pairs (omitted in Emscripten mode), `typeof` bindings for the interface
/// classes, and nested submodule blocks, skipping subtrees with no declared
/// content.
fn render_dts_module_interface(
    out: &mut String,
    model: &BindingModel,
    mb: &ModuleBinding,
    indent: &str,
    emscripten: bool,
) {
    fn tree_has_content(model: &BindingModel, mb: &ModuleBinding, include_listeners: bool) -> bool {
        !mb.functions.is_empty()
            || !mb.interfaces.is_empty()
            || (include_listeners && !mb.listeners.is_empty())
            || model
                .children(mb)
                .any(|sub| tree_has_content(model, sub, include_listeners))
    }
    if !tree_has_content(model, mb, !emscripten) {
        return;
    }
    let error = mb.error.as_ref();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    w.block(format!("{}: {{", mb.name), "};", |w| {
        let inner = w.indent_str();
        for f in &mb.functions {
            // Async functions are throwing stubs in Emscripten mode; omitting
            // them here makes the gap a compile-time error for TS consumers.
            if emscripten && f.is_async {
                continue;
            }
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
        // Listeners are throwing stubs in Emscripten mode; omitting them here
        // makes the gap a compile-time error for TS consumers.
        if !emscripten {
            for l in &mb.listeners {
                let mut tmp = String::new();
                render_dts_listener(&mut tmp, mb, l, &inner);
                w.raw(tmp);
            }
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

/// Emit the TypeScript declarations for one listener's register/unregister
/// pair. The callback parameter types come from the referenced callback
/// typedef; the subscription id is a plain `number` (the loader keys
/// subscriptions by its own context id, so the producer's `uint64_t` id never
/// reaches the public surface).
fn render_dts_listener(out: &mut String, mb: &ModuleBinding, l: &ListenerBinding, indent: &str) {
    let Some(cb) = mb.callback(&l.event_callback) else {
        // Validation guarantees the referenced callback exists in-module.
        unreachable!("listener '{}' references unknown callback", l.name);
    };
    let register_name = format!("register_{}", l.name).to_lower_camel_case();
    let unregister_name = format!("unregister_{}", l.name).to_lower_camel_case();
    let cb_params: Vec<String> = cb
        .params
        .iter()
        .map(|p| format!("{}: {}", js_param_name(p), ts_type_for(&p.ty)))
        .collect();
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    let register_doc = match &l.doc {
        Some(d) => format!(
            "{}\n\n@returns A subscription id for `{unregister_name}()`.",
            d.trim()
        ),
        None => format!(
            "Register a listener for the `{}` callback.\n\n@returns A \
             subscription id for `{unregister_name}()`.",
            cb.name
        ),
    };
    let mut doc = String::new();
    emit_doc(&mut doc, &Some(register_doc), indent);
    w.raw(doc);
    w.line(format!(
        "{register_name}(callback: ({}) => void): number;",
        cb_params.join(", ")
    ));
    let mut doc = String::new();
    emit_doc(
        &mut doc,
        &Some(format!(
            "Unregister a listener previously registered with `{register_name}()`."
        )),
        indent,
    );
    w.raw(doc);
    w.line(format!("{unregister_name}(id: number): void;"));
    out.push_str(&w.finish());
}

/// Emit the TypeScript declarations for the error surface: the generic brand
/// error, then one domain class per declaring module with its per-code
/// subclasses (each carrying a literal-typed `CODE` and any declared payload
/// fields) and the static aliases hung on the domain class.
fn emit_dts_error_classes(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    w.line("/** Base error for WeaveFFI failures: domain errors extend it, and it is");
    w.line(" * thrown directly for unknown codes, marshalling failures, and producer");
    w.line(" * panics. Carries the stable ABI `code`. */");
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
/// statics are static members; async members are omitted in Emscripten mode
/// (they are throwing stubs at runtime).
fn emit_dts_interface_class(
    out: &mut String,
    mb: &ModuleBinding,
    i: &InterfaceBinding,
    emscripten: bool,
) {
    let error = mb.error.as_ref();
    let mut w = CodeWriter::two_space();
    w.doc(&i.doc, DocCommentStyle::Javadoc);
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
        for f in &i.methods {
            if emscripten && f.is_async {
                continue;
            }
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
        for f in &i.statics {
            if emscripten && f.is_async {
                continue;
            }
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
        w.line("/** Releases the producer-owned handle exactly once. */");
        w.line("free(): void;");
    });
    w.blank();
    out.push_str(&w.finish());
}
