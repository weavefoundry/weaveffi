//! TypeScript declarations (`index.d.ts`), shared by both transports.
//!
//! Every declaration is a top-level export of the package, named as
//! `index.js` exports it. Doc and deprecation text name API declarations in
//! their JavaScript spelling.

use weaveffi_model::model::{
    CallbackBinding, EnumBinding, ErrorStrategy, FnBinding, InterfaceBinding, Model, RecordBinding,
};
use weaveffi_model::ty::{ParamTy, RetTy};

use crate::codegen::docs::{ApiNames, Doc};
use crate::codegen::errors::{self, ErrorTable};
use crate::codegen::values::{crossing, Crossing};
use crate::codegen::CodeWriter;
use crate::targets::js::api::canonical_constructor;
use crate::targets::js::names::{
    callback_method_name, doc_spelling, fn_name, js_string, member_name, param_name, rewrite_doc,
    ts_slice_input, ts_type, ErrorNames,
};

/// What the declarations render with: the model, its identifier index, and
/// its error class names.
struct Decls<'a> {
    names: ApiNames,
    errors: &'a ErrorNames,
}

/// Render the declarations body: the root error classes, `extra` (the
/// transport's own exports, such as WebAssembly's `init`), the iterator
/// type, and every declaration of the API.
pub(crate) fn render_declarations(
    w: &mut CodeWriter,
    model: &Model,
    errors: &ErrorNames,
    extra: &str,
) {
    let d = Decls {
        names: ApiNames::new(model),
        errors,
    };
    let root = &errors.root;
    w.line("/// <reference lib=\"esnext.disposable\" />");
    w.blank();
    if model.doc.is_some() {
        d.doc(w, &model.doc, &None, &["@packageDocumentation".into()]);
        w.blank();
    }
    w.line("/**");
    w.line(" * The root of every error these bindings throw. `code` is the ABI error");
    w.line(" * code: positive for a domain error, negative for a runtime failure (-1");
    w.line(" * an untyped failure, -2 producer panic, -3 marshalling failure, -4 a");
    w.line(" * callback implementation failed, -5 cancelled).");
    w.line(" */");
    w.block(
        format!("export declare class {root} extends Error {{"),
        "}",
        |w| {
            w.line("readonly code: number;");
            w.line("constructor(code: number, message?: string);");
        },
    );
    w.line("/** The error a cancelled async call rejects with (code -5). */");
    w.block(
        format!("export declare class CancelledError extends {root} {{"),
        "}",
        |w| {
            w.line("constructor(message?: string);");
        },
    );
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
    for table in errors::tables(model, "Error") {
        d.error_domain(w, &table);
    }
    for e in &model.enums {
        d.enumeration(w, e);
    }
    for s in &model.records {
        d.record(w, s);
    }
    for cb in &model.callbacks {
        d.callback_interface(w, cb);
    }
    for i in &model.interfaces {
        d.class(w, i);
    }
    for f in &model.functions {
        d.doc(w, &f.doc, &f.deprecated, &d.fn_tags(f));
        w.line(format!(
            "export declare function {}({}): {};",
            fn_name(&f.name),
            d.params(f),
            d.ret(f)
        ));
    }
}

impl Decls<'_> {
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
        let spell = doc_spelling(&self.names, self.errors);
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
                        rewrite_doc(&self.names, self.errors, &d.replace('\n', " "))
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
                self.errors.domain(domain)
            )),
            ErrorStrategy::Untyped => {
                tags.push(format!(
                    "@throws {{{}}} (code -1) when the call fails",
                    self.errors.root
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
                let ty = match &p.ty {
                    ParamTy::Value(ty) => match crossing(ty) {
                        Crossing::Slice(elem) => ts_slice_input(elem),
                        _ => ts_type(ty),
                    },
                    ParamTy::Callback { name, nullable } => {
                        if *nullable {
                            format!("{name} | null")
                        } else {
                            name.clone()
                        }
                    }
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
            Some(RetTy::Iterator(elem)) => format!("NativeIterator<{}>", ts_type(elem)),
            Some(RetTy::Value(ty)) => ts_type(ty),
            None => "void".into(),
        };
        if f.is_async() {
            format!("Promise<{base}>")
        } else {
            base
        }
    }

    fn error_domain(&self, w: &mut CodeWriter, table: &ErrorTable<'_>) {
        let domain = self.errors.domain(&table.domain.name);
        let mut doc = table.domain.doc.clone().unwrap_or_default();
        if !doc.is_empty() {
            doc.push_str("\n\n");
        }
        doc.push_str(&format!(
            "The errors of the `{}` domain (an unknown code is this class itself).",
            table.domain.name
        ));
        self.doc(w, &Some(doc), &None, &[]);
        w.block(
            format!(
                "export declare class {domain} extends {} {{",
                self.errors.root
            ),
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
                format!(
                    "export declare class {} extends {domain} {{",
                    self.errors.code(&c.name)
                ),
                "}",
                |w| {
                    w.line(format!("static readonly CODE: {};", c.value));
                    for f in &c.fields {
                        self.doc(w, &f.doc, &None, &[]);
                        w.line(format!("readonly {}: {};", f.name, ts_type(&f.ty)));
                    }
                    if c.fields.is_empty() {
                        w.line("constructor(message?: string);");
                    } else {
                        let fields: Vec<String> = c
                            .fields
                            .iter()
                            .map(|f| format!("{}: {}", f.name, ts_type(&f.ty)))
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
                        .map(|f| format!("; {}: {}", f.name, ts_type(&f.ty)))
                        .collect();
                    let end = if i == last { ";" } else { "" };
                    w.line(format!("| {{ tag: {}{fields} }}{end}", js_string(&v.name)));
                }
            });
            return;
        }
        w.block(format!("export declare enum {} {{", e.name), "}", |w| {
            for v in &e.variants {
                self.doc(w, &v.doc, &None, &[]);
                w.line(format!("{} = {},", v.name, v.value));
            }
        });
    }

    fn record(&self, w: &mut CodeWriter, s: &RecordBinding) {
        self.doc(w, &s.doc, &s.deprecated, &[]);
        w.block(format!("export interface {} {{", s.name), "}", |w| {
            for f in &s.fields {
                self.doc(w, &f.doc, &None, &[]);
                w.line(format!("{}: {};", f.name, ts_type(&f.ty)));
            }
        });
    }

    /// A callback interface: any object with these methods implements it.
    /// A method that throws a domain may raise that domain's errors, which
    /// reach the library with their codes and fields.
    fn callback_interface(&self, w: &mut CodeWriter, cb: &CallbackBinding) {
        self.doc(w, &cb.doc, &cb.deprecated, &[]);
        w.block(format!("export interface {} {{", cb.name), "}", |w| {
            for m in &cb.methods {
                let mut tags = self.param_tags(m.params.iter().map(|p| (p.name.as_str(), &p.doc)));
                match &m.error {
                    ErrorStrategy::Domain(domain) => tags.push(format!(
                        "@throws {{{}}} to report one of its codes, with its fields, to the library",
                        self.errors.domain(domain)
                    )),
                    ErrorStrategy::Untyped => {
                        tags.push(
                            "@throws to report a failure, with its message, to the library".into(),
                        );
                    }
                    ErrorStrategy::Trap => {}
                }
                self.doc(w, &m.doc, &m.deprecated, &tags);
                let ps: Vec<String> = m
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", param_name(&p.name), ts_type(&p.ty)))
                    .collect();
                let r = match &m.ret {
                    Some(ty) => match crossing(ty) {
                        Crossing::Slice(elem) => ts_slice_input(elem),
                        _ => ts_type(ty),
                    },
                    None => "void".to_string(),
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
        self.doc(w, &i.doc, &i.deprecated, &[]);
        w.block(format!("export declare class {} {{", i.name), "}", |w| {
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
                    format!("Promise<{}>", i.name)
                } else {
                    i.name.clone()
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
