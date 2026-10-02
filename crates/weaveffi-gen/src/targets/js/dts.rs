//! TypeScript declarations (`index.d.ts`), shared by both transports.
//!
//! Each top-level IDL module is an exported namespace (`kv`), nested
//! modules are nested namespaces, and user types are referenced by their
//! full path from the root (`kv.Entry`), so equal names in different modules
//! never collide.

use weaveffi_model::model::{
    BindingModel, CallShape, CallbackInterfaceBinding, EnumBinding, ErrorBinding, FnBinding,
    InterfaceBinding, ModuleBinding, ParamBinding, StructBinding, Ty,
};

use crate::codegen::CodeWriter;
use crate::targets::js::names::{
    callback_method_name, code_class, error_owner, fn_name, js_string, member_name, module_name,
    param_name, ts_path, ts_type,
};

/// Render the declarations body: the root error classes, `extra` (the
/// transport's own exports, such as WebAssembly's `init`), the leak-counter
/// hook, and one namespace per top-level module. `root_error` is the root
/// error class name.
pub(crate) fn render_declarations(model: &BindingModel, root_error: &str, extra: &str) -> String {
    let mut w = CodeWriter::two_space();
    w.line("/// <reference lib=\"esnext.disposable\" />");
    w.blank();
    w.line("/**");
    w.line(" * The root of every error these bindings throw. `code` is the ABI error");
    w.line(" * code: positive for a declared domain error, negative for a runtime trap");
    w.line(" * (-1 generic, -2 producer panic, -3 marshalling failure, -4 a callback");
    w.line(" * implementation failed, -5 cancelled).");
    w.line(" */");
    w.block(
        format!("export declare class {root_error} extends Error {{"),
        "}",
        |w| {
            w.line("readonly code: number;");
            w.line("constructor(code: number, message?: string);");
        },
    );
    w.line("/** The error a cancelled async call rejects with (code -5). */");
    w.block(
        format!("export declare class CancelledError extends {root_error} {{"),
        "}",
        |w| {
            w.line("constructor(message?: string);");
        },
    );
    w.line("/** The root error class, under a name no namespace member can shadow. */");
    w.line(format!("declare const $Error: typeof {root_error};"));
    w.raw(extra);
    w.line("/**");
    w.line(" * The native library's live-allocation counters (0 objects, 1 callbacks,");
    w.line(" * 2 iterators, 3 cancel tokens, 4 returned buffers), for leak tests.");
    w.line(" * @internal");
    w.line(" */");
    w.line("export declare function __debugLive(kind: number): bigint;");
    for m in model.roots() {
        w.blank();
        doc(&mut w, m.doc.as_deref(), &[]);
        w.block(
            format!("export declare namespace {} {{", module_name(m)),
            "}",
            |w| namespace_body(w, model, m),
        );
    }
    w.finish()
}

fn namespace_body(w: &mut CodeWriter, model: &BindingModel, m: &ModuleBinding) {
    if let Some(eb) = m.error.as_ref().filter(|e| e.declared_here) {
        error_domain(w, m, eb);
    }
    for e in &m.enums {
        enumeration(w, e);
    }
    for s in &m.structs {
        record(w, s);
    }
    for cb in &m.callback_interfaces {
        callback_interface(w, cb);
    }
    for i in &m.interfaces {
        class(w, model, m, i);
    }
    for f in &m.functions {
        doc(w, f.doc.as_deref(), &fn_tags(model, m, f));
        w.line(format!(
            "export function {}({}): {};",
            fn_name(&f.name),
            params(f),
            ret(f)
        ));
    }
    for child in model.children(m) {
        doc(w, child.doc.as_deref(), &[]);
        w.block(
            format!("export namespace {} {{", module_name(child)),
            "}",
            |w| namespace_body(w, model, child),
        );
    }
}

/// Emit a JSDoc block: the doc text, then `tags` (each a full tag line such
/// as `@deprecated use x`). Nothing when both are empty.
fn doc(w: &mut CodeWriter, text: Option<&str>, tags: &[String]) {
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    let mut lines: Vec<String> = text
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default();
    lines.extend(tags.iter().cloned());
    match lines.as_slice() {
        [] => {}
        [one] => {
            w.line(format!("/** {one} */"));
        }
        _ => {
            w.line("/**");
            for l in &lines {
                if l.is_empty() {
                    w.line(" *");
                } else {
                    w.line(format!(" * {l}"));
                }
            }
            w.line(" */");
        }
    }
}

fn deprecated(msg: Option<&String>) -> Vec<String> {
    msg.map(|m| vec![format!("@deprecated {m}")])
        .unwrap_or_default()
}

/// The JSDoc tags of a callable: documented parameters, the domain it
/// throws, and any deprecation.
fn fn_tags(model: &BindingModel, m: &ModuleBinding, f: &FnBinding) -> Vec<String> {
    let mut tags = param_tags(&f.params);
    if let (true, Some(eb)) = (f.throws, m.error.as_ref()) {
        let owner = error_owner(&model.modules, eb);
        tags.push(format!(
            "@throws {{{}}}",
            ts_path(&format!("{}.{}", owner.dot_path, eb.type_name))
        ));
    }
    tags.extend(deprecated(f.deprecated.as_ref()));
    tags
}

fn param_tags(params: &[ParamBinding]) -> Vec<String> {
    params
        .iter()
        .filter_map(|p| {
            let d = p.doc.as_deref()?.trim();
            (!d.is_empty())
                .then(|| format!("@param {} {}", param_name(&p.name), d.replace('\n', " ")))
        })
        .collect()
}

fn params(f: &FnBinding) -> String {
    let mut out: Vec<String> = f
        .params
        .iter()
        .map(|p| format!("{}: {}", param_name(&p.name), ts_type(&p.ty)))
        .collect();
    if f.cancellable {
        out.push("options?: { signal?: AbortSignal }".into());
    }
    out.join(", ")
}

fn ret(f: &FnBinding) -> String {
    let base = match (&f.shape, &f.ret) {
        (CallShape::Iterator(it), _) => format!("IterableIterator<{}>", ts_type(&it.elem)),
        (_, Some(ty)) => ts_type(ty),
        (_, None) => "void".into(),
    };
    if f.is_async {
        format!("Promise<{base}>")
    } else {
        base
    }
}

fn error_domain(w: &mut CodeWriter, m: &ModuleBinding, eb: &ErrorBinding) {
    let domain_ts = ts_path(&format!("{}.{}", m.dot_path, eb.type_name));
    w.line(format!(
        "/** The errors of the `{}` error domain. */",
        m.dot_path
    ));
    w.block(
        format!("export class {} extends $Error {{", eb.type_name),
        "}",
        |w| {
            w.line("constructor(code: number, message?: string);");
        },
    );
    for c in &eb.codes {
        doc(w, Some(c.doc.as_deref().unwrap_or(&c.message)), &[]);
        w.block(
            format!(
                "export class {} extends {domain_ts} {{",
                code_class(&c.name)
            ),
            "}",
            |w| {
                w.line(format!("static readonly CODE: {};", c.value));
                for f in &c.fields {
                    doc(w, f.doc.as_deref(), &[]);
                    w.line(format!("readonly {}: {};", f.name, ts_type(&f.ty)));
                }
                w.line("constructor(message?: string);");
            },
        );
    }
}

fn enumeration(w: &mut CodeWriter, e: &EnumBinding) {
    doc(w, e.doc.as_deref(), &deprecated(e.deprecated.as_ref()));
    if e.rich {
        w.line(format!("export type {} =", e.name));
        w.scope(|w| {
            let last = e.variants.len().saturating_sub(1);
            for (i, v) in e.variants.iter().enumerate() {
                doc(w, v.doc.as_deref(), &[]);
                let fields: String = v
                    .fields
                    .iter()
                    .map(|f| format!("; {}: {}", f.name, ts_type(&f.ty)))
                    .collect();
                let end = if i == last { ";" } else { "" };
                w.line(format!("| {{ tag: {}{fields} }}{end}", js_string(&v.name)));
            }
        });
        return;
    }
    w.block(format!("export enum {} {{", e.name), "}", |w| {
        for v in &e.variants {
            doc(w, v.doc.as_deref(), &[]);
            w.line(format!("{} = {},", v.name, v.value));
        }
    });
}

fn record(w: &mut CodeWriter, s: &StructBinding) {
    doc(w, s.doc.as_deref(), &deprecated(s.deprecated.as_ref()));
    w.block(format!("export interface {} {{", s.name), "}", |w| {
        for f in &s.fields {
            doc(w, f.doc.as_deref(), &[]);
            w.line(format!("{}: {};", f.name, ts_type(&f.ty)));
        }
    });
}

/// A callback interface: any object with these methods implements it.
pub(crate) fn callback_interface(w: &mut CodeWriter, cb: &CallbackInterfaceBinding) {
    doc(w, cb.doc.as_deref(), &deprecated(cb.deprecated.as_ref()));
    w.block(format!("export interface {} {{", cb.name), "}", |w| {
        for m in &cb.methods {
            let mut tags = param_tags(&m.params);
            tags.extend(deprecated(m.deprecated.as_ref()));
            doc(w, m.doc.as_deref(), &tags);
            let ps: Vec<String> = m
                .params
                .iter()
                .map(|p| format!("{}: {}", param_name(&p.name), ts_type(&p.ty)))
                .collect();
            let r = m.ret.as_ref().map_or_else(|| "void".to_string(), ts_type);
            w.line(format!(
                "{}({}): {r};",
                callback_method_name(&m.name),
                ps.join(", ")
            ));
        }
    });
}

fn class(w: &mut CodeWriter, model: &BindingModel, m: &ModuleBinding, i: &InterfaceBinding) {
    let self_ty = ts_type(&Ty::Interface(format!("{}.{}", m.dot_path, i.name)));
    doc(w, i.doc.as_deref(), &deprecated(i.deprecated.as_ref()));
    w.block(format!("export class {} {{", i.name), "}", |w| {
        let canonical = i
            .constructors
            .iter()
            .find(|c| c.name == "new" && !c.is_async);
        match canonical {
            Some(c) => {
                doc(w, c.doc.as_deref(), &fn_tags(model, m, c));
                w.line(format!("constructor({});", params(c)));
            }
            None => {
                w.line("private constructor();");
            }
        }
        for c in i
            .constructors
            .iter()
            .filter(|c| canonical.is_none_or(|k| !std::ptr::eq(k, *c)))
        {
            doc(w, c.doc.as_deref(), &fn_tags(model, m, c));
            let r = if c.is_async {
                format!("Promise<{self_ty}>")
            } else {
                self_ty.clone()
            };
            w.line(format!(
                "static {}({}): {r};",
                member_name(&c.name, true),
                params(c)
            ));
        }
        for f in &i.methods {
            doc(w, f.doc.as_deref(), &fn_tags(model, m, f));
            w.line(format!(
                "{}({}): {};",
                member_name(&f.name, false),
                params(f),
                ret(f)
            ));
        }
        for f in &i.statics {
            doc(w, f.doc.as_deref(), &fn_tags(model, m, f));
            w.line(format!(
                "static {}({}): {};",
                member_name(&f.name, true),
                params(f),
                ret(f)
            ));
        }
        w.line("/**");
        w.line(" * Release this wrapper's reference to the native object. Safe to call");
        w.line(" * more than once; a wrapper that is never closed is released when it is");
        w.line(" * garbage collected. Using the wrapper after `close()` throws.");
        w.line(" */");
        w.line("close(): void;");
        w.line("/** Alias of `close()` for `using` declarations. */");
        w.line("[Symbol.dispose](): void;");
    });
}
