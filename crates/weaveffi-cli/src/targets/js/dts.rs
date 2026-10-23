//! TypeScript declarations (`index.d.ts`), shared by both transports.
//!
//! Each top-level IDL module is an exported namespace (`kv`), nested
//! modules are nested namespaces, and user types are referenced by their
//! full path from the root (`kv.Entry`). Doc and deprecation text name API
//! declarations in their JavaScript spelling.

use weaveffi_model::model::{
    CallbackInterfaceBinding, EnumBinding, FnBinding, InterfaceBinding, Model, ModuleBinding,
    StructBinding,
};
use weaveffi_model::plan::{ArgPass, CallbackRetPass, ErrorStrategy};
use weaveffi_model::ty::{RetTy, Ty};

use crate::codegen::docs::{ApiNames, Doc};
use crate::codegen::errors::{self, ErrorTable};
use crate::codegen::CodeWriter;
use crate::targets::js::api::canonical_constructor;
use crate::targets::js::names::{
    callback_method_name, doc_spelling, fn_name, js_string, member_name, module_name, param_name,
    rewrite_doc, ts_domain, ts_path_in, ts_slice_input, ts_type,
};

/// What the declarations render with: the model and its identifier index.
struct Decls<'a> {
    model: &'a Model,
    names: ApiNames,
    root_error: &'a str,
}

/// Render the declarations body: the root error classes, `extra` (the
/// transport's own exports, such as WebAssembly's `init`), the iterator
/// type, and one namespace per top-level module. `root_error` is the root
/// error class name.
pub(crate) fn render_declarations(
    w: &mut CodeWriter,
    model: &Model,
    root_error: &str,
    extra: &str,
) {
    let d = Decls {
        model,
        names: ApiNames::new(model),
        root_error,
    };
    w.line("/// <reference lib=\"esnext.disposable\" />");
    w.blank();
    w.line("/**");
    w.line(" * The root of every error these bindings throw. `code` is the ABI error");
    w.line(" * code: positive for a domain error, negative for a runtime failure (-1");
    w.line(" * an untyped failure, -2 producer panic, -3 marshalling failure, -4 a");
    w.line(" * callback implementation failed, -5 cancelled).");
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
    w.line("/**");
    w.line(" * A lazy iterator over a native `iter<T>`: each `next()` makes one native");
    w.line(" * call. The native iterator is released when it is exhausted or fails, by");
    w.line(" * `return()` (which `for...of` calls on `break`), by `close()` or a `using`");
    w.line(" * declaration, or when the iterator is garbage collected.");
    w.line(" */");
    w.block(
        "export interface NativeIterator<T> extends IterableIterator<T> {",
        "}",
        |w| {
            w.line("/** Release the native iterator now. Safe to call more than once. */");
            w.line("close(): void;");
            w.line("/** Alias of `close()` for `using` declarations. */");
            w.line("[Symbol.dispose](): void;");
        },
    );
    w.raw(extra);
    let tables = errors::tables(model, "Error");
    for m in model.roots() {
        w.blank();
        d.doc(w, &m.doc, &None, &[]);
        w.block(
            format!("export declare namespace {} {{", module_name(&m.name)),
            "}",
            |w| d.namespace_body(w, m, &tables),
        );
    }
}

impl Decls<'_> {
    fn namespace_body(&self, w: &mut CodeWriter, m: &ModuleBinding, tables: &[ErrorTable<'_>]) {
        for table in tables.iter().filter(|t| t.module.index == m.index) {
            self.error_domain(w, table);
        }
        for e in &m.enums {
            self.enumeration(w, e);
        }
        for s in &m.structs {
            self.record(w, s);
        }
        for cb in &m.callback_interfaces {
            self.callback_interface(w, cb);
        }
        for i in &m.interfaces {
            self.class(w, i);
        }
        for f in &m.functions {
            self.doc(w, &f.doc, &f.deprecated, &self.fn_tags(f));
            w.line(format!(
                "export function {}({}): {};",
                fn_name(&f.name),
                self.params(f),
                self.ret(f)
            ));
        }
        for child in self.model.children(m) {
            self.doc(w, &child.doc, &None, &[]);
            w.block(
                format!("export namespace {} {{", module_name(&child.name)),
                "}",
                |w| self.namespace_body(w, child, tables),
            );
        }
    }

    /// Emit a JSDoc block: the doc text, then `tags` (each a full tag line
    /// such as `@param x the count`), then `@deprecated` with the
    /// deprecation message. Nothing when all are empty.
    fn doc(
        &self,
        w: &mut CodeWriter,
        doc: &Option<String>,
        deprecated: &Option<String>,
        tags: &[String],
    ) {
        let spell = doc_spelling(&self.names);
        let doc = Doc::new(doc, deprecated);
        let mut lines: Vec<String> = doc
            .text(&spell)
            .map(|t| t.lines().map(str::to_string).collect())
            .unwrap_or_default();
        lines.extend(tags.iter().cloned());
        if let Some(msg) = doc.deprecation(&spell) {
            lines.push(format!("@deprecated {msg}"));
        }
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

    /// The `@param` tags of documented parameters.
    fn param_tags<'p>(
        &self,
        params: impl Iterator<Item = (&'p str, &'p Option<String>)>,
    ) -> Vec<String> {
        params
            .filter_map(|(name, doc)| {
                let d = doc.as_deref()?.trim();
                (!d.is_empty()).then(|| {
                    format!(
                        "@param {} {}",
                        param_name(name),
                        rewrite_doc(&self.names, &d.replace('\n', " "))
                    )
                })
            })
            .collect()
    }

    /// The JSDoc tags of a callable: documented parameters and what it
    /// throws.
    fn fn_tags(&self, f: &FnBinding) -> Vec<String> {
        let mut tags = self.param_tags(f.params.iter().map(|p| (p.name.as_str(), &p.doc)));
        match &f.error {
            ErrorStrategy::Domain(domain) => tags.push(format!(
                "@throws {{{}}} when the call fails with one of its codes",
                ts_domain(self.model, domain)
            )),
            ErrorStrategy::Untyped => {
                tags.push(format!(
                    "@throws {{{}}} (code -1) when the call fails",
                    self.root_error
                ));
            }
            ErrorStrategy::Trap => {}
        }
        tags
    }

    fn params(&self, f: &FnBinding) -> String {
        let mut out: Vec<String> = f
            .params
            .iter()
            .map(|p| {
                let ty = match &p.pass {
                    ArgPass::Slice { elem, .. } => ts_slice_input(*elem),
                    ArgPass::Callback {
                        interface,
                        nullable,
                        ..
                    } => {
                        let path = ts_type(self.model, &Ty::Interface(interface.clone()));
                        if *nullable {
                            format!("{path} | null")
                        } else {
                            path
                        }
                    }
                    _ => ts_type(self.model, p.ty.value().expect("a value parameter")),
                };
                format!("{}: {ty}", param_name(&p.name))
            })
            .collect();
        if f.cancellable() {
            out.push("options?: { signal?: AbortSignal }".into());
        }
        out.join(", ")
    }

    fn ret(&self, f: &FnBinding) -> String {
        let base = match &f.ret {
            Some(RetTy::Iterator(elem)) => {
                format!("NativeIterator<{}>", ts_type(self.model, elem))
            }
            Some(RetTy::Value(ty)) => ts_type(self.model, ty),
            None => "void".into(),
        };
        if f.is_async() {
            format!("Promise<{base}>")
        } else {
            base
        }
    }

    fn error_domain(&self, w: &mut CodeWriter, table: &ErrorTable<'_>) {
        let domain_ts = ts_path_in(&table.module.segments, &table.type_name);
        w.line(format!(
            "/** The errors of the `{}` domain (an unknown code is this class itself). */",
            table.domain.name
        ));
        w.block(
            format!("export class {} extends $Error {{", table.type_name),
            "}",
            |w| {
                w.line("constructor(code: number, message?: string);");
            },
        );
        for row in &table.codes {
            let c = row.code;
            let doc = Some(c.doc.clone().unwrap_or_else(|| c.message.clone()));
            self.doc(w, &doc, &None, &[]);
            w.block(
                format!("export class {} extends {domain_ts} {{", row.type_name),
                "}",
                |w| {
                    w.line(format!("static readonly CODE: {};", c.value));
                    for f in &c.fields {
                        self.doc(w, &f.doc, &None, &[]);
                        w.line(format!(
                            "readonly {}: {};",
                            f.name,
                            ts_type(self.model, &f.ty)
                        ));
                    }
                    if c.fields.is_empty() {
                        w.line("constructor(message?: string);");
                    } else {
                        let fields: Vec<String> = c
                            .fields
                            .iter()
                            .map(|f| format!("{}: {}", f.name, ts_type(self.model, &f.ty)))
                            .collect();
                        w.line(format!(
                            "constructor(fields: {{ {} }}, message?: string);",
                            fields.join("; ")
                        ));
                    }
                },
            );
        }
    }

    fn enumeration(&self, w: &mut CodeWriter, e: &EnumBinding) {
        self.doc(w, &e.doc, &e.deprecated, &[]);
        if e.is_rich() {
            w.line(format!("export type {} =", e.name));
            w.scope(|w| {
                let last = e.variants.len().saturating_sub(1);
                for (i, v) in e.variants.iter().enumerate() {
                    self.doc(w, &v.doc, &None, &[]);
                    let fields: String = v
                        .fields
                        .iter()
                        .map(|f| format!("; {}: {}", f.name, ts_type(self.model, &f.ty)))
                        .collect();
                    let end = if i == last { ";" } else { "" };
                    w.line(format!("| {{ tag: {}{fields} }}{end}", js_string(&v.name)));
                }
            });
            return;
        }
        w.block(format!("export enum {} {{", e.name), "}", |w| {
            for v in &e.variants {
                self.doc(w, &v.doc, &None, &[]);
                w.line(format!("{} = {},", v.name, v.value));
            }
        });
    }

    fn record(&self, w: &mut CodeWriter, s: &StructBinding) {
        self.doc(w, &s.doc, &s.deprecated, &[]);
        w.block(format!("export interface {} {{", s.name), "}", |w| {
            for f in &s.fields {
                self.doc(w, &f.doc, &None, &[]);
                w.line(format!("{}: {};", f.name, ts_type(self.model, &f.ty)));
            }
        });
    }

    /// A callback interface: any object with these methods implements it.
    /// A method that throws a domain may raise that domain's errors, which
    /// reach the library with their codes and fields.
    fn callback_interface(&self, w: &mut CodeWriter, cb: &CallbackInterfaceBinding) {
        self.doc(w, &cb.doc, &cb.deprecated, &[]);
        w.block(format!("export interface {} {{", cb.name), "}", |w| {
            for m in &cb.methods {
                let mut tags = self.param_tags(m.params.iter().map(|p| (p.name.as_str(), &p.doc)));
                match &m.error {
                    ErrorStrategy::Domain(domain) => tags.push(format!(
                        "@throws {{{}}} to report one of its codes, with its fields, to the library",
                        ts_domain(self.model, domain)
                    )),
                    ErrorStrategy::Untyped => {
                        tags.push("@throws to report a failure, with its message, to the library".into());
                    }
                    ErrorStrategy::Trap => {}
                }
                self.doc(w, &m.doc, &m.deprecated, &tags);
                let ps: Vec<String> = m
                    .params
                    .iter()
                    .map(|p| {
                        format!(
                            "{}: {}",
                            param_name(&p.name),
                            ts_type(self.model, &p.ty)
                        )
                    })
                    .collect();
                let r = match (&m.ret_pass, &m.ret) {
                    (CallbackRetPass::Slice { elem, .. }, _) => ts_slice_input(*elem),
                    (_, Some(ty)) => ts_type(self.model, ty),
                    (_, None) => "void".to_string(),
                };
                w.line(format!(
                    "{}({}): {r};",
                    callback_method_name(&m.name),
                    ps.join(", ")
                ));
            }
        });
    }

    fn class(&self, w: &mut CodeWriter, i: &InterfaceBinding) {
        let self_ty = ts_type(self.model, &Ty::Interface(i.name.clone()));
        self.doc(w, &i.doc, &i.deprecated, &[]);
        w.block(format!("export class {} {{", i.name), "}", |w| {
            let canonical = canonical_constructor(i);
            match canonical {
                Some(c) => {
                    self.doc(w, &c.doc, &c.deprecated, &self.fn_tags(c));
                    w.line(format!("constructor({});", self.params(c)));
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
                self.doc(w, &c.doc, &c.deprecated, &self.fn_tags(c));
                let r = if c.is_async() {
                    format!("Promise<{self_ty}>")
                } else {
                    self_ty.clone()
                };
                w.line(format!(
                    "static {}({}): {r};",
                    member_name(&c.name, true),
                    self.params(c)
                ));
            }
            for f in &i.methods {
                self.doc(w, &f.doc, &f.deprecated, &self.fn_tags(f));
                w.line(format!(
                    "{}({}): {};",
                    member_name(&f.name, false),
                    self.params(f),
                    self.ret(f)
                ));
            }
            for f in &i.statics {
                self.doc(w, &f.doc, &f.deprecated, &self.fn_tags(f));
                w.line(format!(
                    "static {}({}): {};",
                    member_name(&f.name, true),
                    self.params(f),
                    self.ret(f)
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
}
