//! Rendering of the `{library}_buffer.h` value-buffer helper header.
//!
//! Records, rich enums, optionals, lists, and maps cross the C ABI as
//! serialized value buffers. This header gives a C consumer a plain C struct
//! for every record and rich enum (and for every error code that carries
//! fields), a struct for every list and map shape the API uses, and
//! `static inline` codecs for each: `{name}_write`, `{name}_read`,
//! `{name}_decode`, and `{name}_free`. Optionals are pointers (`NULL` is
//! absent), so they have codecs but no struct.
//!
//! The fixed reader and writer live in `runtime/buffer_runtime.h` and are
//! spliced in with the prefix substituted; everything after them is rendered
//! from the [`Model`].

use std::collections::HashMap;

use crate::cabi::c_param_name;
use crate::codegen::codecs;
use crate::codegen::common::{self, DocCommentStyle};
use crate::codegen::docs::{ApiNames, Doc};
use crate::codegen::errors;
use crate::codegen::CodeWriter;
use crate::utils::{render_prelude, render_trailer, CommentStyle};
use weaveffi_model::model::{EnumBinding, FieldBinding, InterfaceBinding, Model};
use weaveffi_model::ty::{Prim, Ty, WireType};

/// The fixed reader, writer, and string view types.
const RUNTIME: &str = include_str!("runtime/buffer_runtime.h");

/// Nesting depth past which the recursive size and ownership queries stop.
/// Only a by-value record cycle (which no producer can build) gets there.
const MAX_DEPTH: usize = 64;

/// The fields a generated struct is built from.
enum Body<'a> {
    /// A record or an error code's payload: fields in wire order.
    Fields(&'a [FieldBinding]),
    /// A rich enum: an `int32_t` tag, then the active variant's fields.
    Variants(&'a EnumBinding),
}

/// A record, rich enum, or error payload rendered as a named C struct.
struct UserType<'a> {
    /// The struct typedef, which is also the stem of its codec functions.
    name: String,
    doc: Option<String>,
    body: Body<'a>,
}

/// Every type the header renders, indexed for lookup by resolved name.
struct Codecs<'a> {
    model: &'a Model,
    prefix: &'a str,
    /// Records and rich enums in declaration order, then error payloads.
    users: Vec<UserType<'a>>,
    /// Record or rich enum name to an index into `users`.
    user_index: HashMap<&'a str, usize>,
    /// Optional, list, and map shapes, innermost first, without duplicates
    /// (see [`codecs::composites`]).
    composites: Vec<Ty>,
}

/// The header file name for a library: `{library}_buffer.h`.
#[must_use]
pub(crate) fn buffer_header_name(library: &str) -> String {
    format!("{library}_buffer.h")
}

/// Render `{library}_buffer.h`, or `None` when no type in the API crosses
/// the ABI as a value buffer.
///
/// `header_name` is the main header the helper includes; `file_name` is the
/// helper's own name, used in its trailer.
#[must_use]
pub(crate) fn render_buffer_header(
    model: &Model,
    header_name: &str,
    file_name: &str,
) -> Option<String> {
    if !model.has_buffers() {
        return None;
    }
    let codecs = Codecs::new(model);
    let prefix = model.prefix();
    let guard = format!("{}_BUFFER_H", prefix.to_uppercase());

    let mut out = render_prelude(CommentStyle::DoubleSlash);
    out.push_str(&format!("#ifndef {guard}\n#define {guard}\n\n"));
    out.push_str(&format!("#include \"{header_name}\"\n\n"));
    out.push_str("#include <stdlib.h>\n#include <string.h>\n\n");
    out.push_str("#ifdef __cplusplus\nextern \"C\" {\n#endif\n\n");
    out.push_str(&RUNTIME.replace("{{p}}", prefix));
    out.push('\n');
    out.push_str(&codecs.render());
    out.push_str("#ifdef __cplusplus\n}\n#endif\n\n");
    out.push_str(&format!("#endif // {guard}\n\n"));
    out.push_str(&render_trailer(CommentStyle::DoubleSlash, file_name));
    Some(out)
}

/// The four codec functions of one named type, as body lines.
struct Unit {
    name: String,
    /// The C type a decoded value is stored in.
    vtype: String,
    /// The type of `{name}_write`'s value parameter.
    write_param: String,
    write: Vec<String>,
    read: Vec<String>,
    free: Vec<String>,
}

impl<'a> Codecs<'a> {
    fn new(model: &'a Model) -> Self {
        let prefix = model.prefix();
        let names = ApiNames::new(model);
        let doc = |doc: &Option<String>, deprecated: &Option<String>| {
            Doc::new(doc, deprecated)
                .with_deprecation(|ident| names.c_name(ident).map(str::to_string))
        };
        let mut users = Vec::new();
        let mut user_index = HashMap::new();
        for m in &model.modules {
            for s in &m.structs {
                user_index.insert(s.name.as_str(), users.len());
                users.push(UserType {
                    name: s.c_tag.clone(),
                    doc: doc(&s.doc, &s.deprecated),
                    body: Body::Fields(&s.fields),
                });
            }
            for e in m.enums.iter().filter(|e| e.rich) {
                user_index.insert(e.name.as_str(), users.len());
                users.push(UserType {
                    name: e.c_tag.clone(),
                    doc: doc(&e.doc, &e.deprecated),
                    body: Body::Variants(e),
                });
            }
        }
        for table in errors::tables(model, "Error") {
            for row in table.codes.iter().filter(|r| !r.code.fields.is_empty()) {
                users.push(UserType {
                    name: row.code.payload_tag(),
                    doc: Some(common::wrap(
                        &format!(
                            "The fields of a `{}` error (`{}` in the language bindings), \
                             which it carries in `payload_ptr`/`payload_len`.",
                            row.code.c_const, row.type_name
                        ),
                        76,
                    )),
                    body: Body::Fields(&row.code.fields),
                });
            }
        }

        Codecs {
            model,
            prefix,
            users,
            user_index,
            composites: codecs::composites(model),
        }
    }

    fn user(&self, name: &str) -> &UserType<'a> {
        let idx = self
            .user_index
            .get(name)
            .unwrap_or_else(|| panic!("unresolved record or rich enum '{name}'"));
        &self.users[*idx]
    }

    fn interface(&self, name: &str) -> &InterfaceBinding {
        self.model.interface(name)
    }

    fn enum_tag(&self, name: &str) -> &str {
        &self.model.enumeration(name).c_tag
    }

    /// The codec stem of a record, rich enum, optional, list, or map.
    fn named(&self, ty: &Ty) -> Option<String> {
        match ty {
            Ty::Record(n) | Ty::RichEnum(n) => Some(self.user(n).name.clone()),
            Ty::Optional(_) | Ty::List(_) | Ty::Map(_, _) => {
                Some(format!("{}_{}", self.prefix, codecs::stem(ty)))
            }
            _ => None,
        }
    }

    /// The C type a decoded value of `ty` is stored in.
    fn vtype(&self, ty: &Ty) -> String {
        let p = self.prefix;
        match ty {
            Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
                self.named(ty).unwrap_or_default()
            }
            Ty::Enum(n) => self.enum_tag(n).to_string(),
            Ty::Interface(n) => format!("{}*", self.interface(n).c_tag),
            Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => self.vtype(inner),
            Ty::Optional(inner) => format!("{}*", self.vtype(inner)),
            Ty::Prim(Prim::String) => format!("{p}_str"),
            Ty::Prim(Prim::Bytes) => format!("{p}_bytes"),
            _ => prim_ctype(prim(ty)).to_string(),
        }
    }

    /// The parameter type of `{name}_write` for a named type: a pointer to
    /// the value, except for an optional, which takes the (nullable) pointer
    /// it is stored as.
    fn write_param(&self, ty: &Ty) -> String {
        let target = match ty {
            Ty::Optional(inner) if matches!(inner.as_ref(), Ty::Interface(_)) => {
                return format!("const {}", self.vtype(ty));
            }
            Ty::Optional(inner) => self.vtype(inner),
            _ => self.vtype(ty),
        };
        const_ptr(&target)
    }

    /// A statement appending the value at lvalue `lv` to writer `w`.
    fn write_stmt(&self, ty: &Ty, lv: &str) -> String {
        let p = self.prefix;
        match ty {
            Ty::Enum(_) => format!("{p}_writer_put_i32(w, (int32_t){lv});"),
            Ty::Interface(n) => format!(
                "{p}_writer_put_object(w, {}({lv}));",
                self.interface(n).clone_symbol
            ),
            Ty::Optional(_) => format!("{}_write(w, {lv});", self.named(ty).unwrap_or_default()),
            Ty::Record(_) | Ty::RichEnum(_) | Ty::List(_) | Ty::Map(_, _) => {
                format!("{}_write(w, &{lv});", self.named(ty).unwrap_or_default())
            }
            _ => format!("{p}_writer_put_{}(w, {lv});", prim(ty).snake()),
        }
    }

    /// A statement decoding one value from reader `r` into lvalue `lv`.
    fn read_stmt(&self, ty: &Ty, lv: &str) -> String {
        let p = self.prefix;
        match ty {
            Ty::Enum(n) => format!("{lv} = ({}){p}_reader_get_i32(r);", self.enum_tag(n)),
            Ty::Interface(n) => format!(
                "{lv} = ({}*){p}_reader_get_object(r);",
                self.interface(n).c_tag
            ),
            _ => match self.named(ty) {
                Some(name) => format!("{name}_read(r, &{lv});"),
                None => format!("{lv} = {p}_reader_get_{}(r);", prim(ty).snake()),
            },
        }
    }

    /// A statement releasing what the decoded value at lvalue `lv` owns, or
    /// `None` when it owns nothing.
    fn free_stmt(&self, ty: &Ty, lv: &str) -> Option<String> {
        if !self.needs_free(ty, 0) {
            return None;
        }
        let p = self.prefix;
        Some(match ty {
            Ty::Prim(Prim::String) => format!("{p}_str_free(&{lv});"),
            Ty::Prim(Prim::Bytes) => format!("{p}_bytes_free(&{lv});"),
            Ty::Interface(n) => format!("{}({lv});", self.interface(n).destroy_symbol),
            _ => format!("{}_free(&{lv});", self.named(ty).unwrap_or_default()),
        })
    }

    /// Whether a decoded value of `ty` owns memory or object references.
    fn needs_free(&self, ty: &Ty, depth: usize) -> bool {
        match ty {
            Ty::Prim(Prim::String | Prim::Bytes)
            | Ty::Interface(_)
            | Ty::Optional(_)
            | Ty::List(_)
            | Ty::Map(_, _) => true,
            Ty::Record(n) | Ty::RichEnum(n) => {
                depth > MAX_DEPTH || self.body_needs_free(&self.user(n).body, depth + 1)
            }
            _ => false,
        }
    }

    fn body_needs_free(&self, body: &Body<'_>, depth: usize) -> bool {
        match body {
            Body::Fields(fields) => fields.iter().any(|f| self.needs_free(&f.ty, depth)),
            Body::Variants(e) => e
                .variants
                .iter()
                .flat_map(|v| &v.fields)
                .any(|f| self.needs_free(&f.ty, depth)),
        }
    }

    /// The fewest bytes one encoded value of `ty` can occupy, which bounds
    /// how many elements a list or map count can honestly claim.
    fn min_wire(&self, ty: &Ty, depth: usize) -> usize {
        match ty {
            Ty::Enum(_) => 4,
            Ty::Interface(_) => 8,
            Ty::Optional(_) => 1,
            Ty::List(_) | Ty::Map(_, _) => 4,
            Ty::RichEnum(_) => 4,
            Ty::Record(n) => match self.user(n).body {
                Body::Fields(fields) if depth <= MAX_DEPTH => {
                    fields.iter().map(|f| self.min_wire(&f.ty, depth + 1)).sum()
                }
                _ => 0,
            },
            _ => match prim(ty) {
                Prim::Bool | Prim::I8 | Prim::U8 => 1,
                Prim::I16 | Prim::U16 => 2,
                Prim::I32 | Prim::U32 | Prim::F32 | Prim::String | Prim::Bytes => 4,
                Prim::I64 | Prim::U64 | Prim::F64 => 8,
            },
        }
    }

    fn render(&self) -> String {
        let p = self.prefix;
        let mut w = CodeWriter::four_space();
        w.line("/* ---------------------------------------------------------------------------");
        w.line(" * Value types. A record is a struct of its fields; a rich enum is a `tag`");
        w.line(" * plus a union `as` with one struct per payload-carrying variant; a list is");
        w.line(" * `items` + `len`; a map is parallel `keys` and `values` arrays + `len`; an");
        w.line(" * optional is a pointer that is NULL when absent; an object is its pointer;");
        w.line(" * a string or byte string is a view (`ptr` + `len`).");
        w.line(" * ------------------------------------------------------------------------- */");
        w.blank();
        for u in &self.users {
            w.line(format!("typedef struct {0} {0};", u.name));
        }
        let structs: Vec<&Ty> = self
            .composites
            .iter()
            .filter(|t| matches!(t, Ty::List(_) | Ty::Map(_, _)))
            .collect();
        for ty in &structs {
            w.line(format!(
                "typedef struct {0} {0};",
                self.named(ty).unwrap_or_default()
            ));
        }
        w.blank();
        for ty in &structs {
            self.render_composite_struct(&mut w, ty);
        }
        for idx in self.struct_order() {
            self.render_user_struct(&mut w, &self.users[idx]);
        }

        let units = self.units();
        w.line("/* ---------------------------------------------------------------------------");
        w.line(" * Codecs. Each value type T above, and each optional shape, has four");
        w.line(" * functions:");
        w.line(" *");
        w.line(" *   T_write(w, v)            appends the value at `v` to writer `w`");
        w.line(" *   T_read(r, out)           decodes one value from reader `r`");
        w.line(" *   T_decode(ptr, len, out)  decodes a whole buffer; on a malformed");
        w.line(" *                            buffer it frees what it built and");
        w.line(" *                            returns false");
        w.line(" *   T_free(v)                releases a decoded value");
        w.line(" *");
        w.line(" * An optional's T_write takes the nullable pointer itself. Writing an");
        w.line(" * object stores a fresh `_clone` reference for the reader to adopt;");
        w.line(" * decoding adopts each object reference, and T_free destroys it. T_free");
        w.line(" * releases only what T_read or T_decode allocated; never call it on a");
        w.line(" * value that borrows caller memory.");
        w.line(" * ------------------------------------------------------------------------- */");
        w.blank();
        for u in &units {
            let n = &u.name;
            let v = &u.vtype;
            w.line(format!(
                "static inline void {n}_write({p}_writer* w, {} v);",
                u.write_param
            ));
            w.line(format!(
                "static inline void {n}_read({p}_reader* r, {v}* out);"
            ));
            w.line(format!(
                "static inline bool {n}_decode(const uint8_t* ptr, size_t len, {v}* out);"
            ));
            w.line(format!("static inline void {n}_free({v}* v);"));
        }
        for u in &units {
            let n = &u.name;
            let v = &u.vtype;
            w.blank();
            w.block(
                format!(
                    "static inline void {n}_write({p}_writer* w, {} v) {{",
                    u.write_param
                ),
                "}",
                |w| {
                    u.write.iter().for_each(|l| {
                        w.line(l);
                    })
                },
            );
            w.blank();
            w.block(
                format!("static inline void {n}_read({p}_reader* r, {v}* out) {{"),
                "}",
                |w| {
                    u.read.iter().for_each(|l| {
                        w.line(l);
                    })
                },
            );
            w.blank();
            w.block(
                format!(
                    "static inline bool {n}_decode(const uint8_t* ptr, size_t len, {v}* out) {{"
                ),
                "}",
                |w| {
                    w.line(format!("{p}_reader r = {p}_reader_of(ptr, len);"));
                    w.line(format!("{n}_read(&r, out);"));
                    w.block(format!("if (!{p}_reader_finish(&r)) {{"), "}", |w| {
                        w.line(format!("{n}_free(out);"));
                        w.line("return false;");
                    });
                    w.line("return true;");
                },
            );
            w.blank();
            w.block(
                format!("static inline void {n}_free({v}* v) {{"),
                "}",
                |w| {
                    u.free.iter().for_each(|l| {
                        w.line(l);
                    })
                },
            );
        }
        w.blank();
        w.finish()
    }

    /// User structs ordered so every by-value field's struct is defined
    /// before the struct that embeds it.
    fn struct_order(&self) -> Vec<usize> {
        fn visit(c: &Codecs<'_>, idx: usize, done: &mut Vec<bool>, order: &mut Vec<usize>) {
            if done[idx] {
                return;
            }
            done[idx] = true;
            let fields: Vec<&FieldBinding> = match c.users[idx].body {
                Body::Fields(fields) => fields.iter().collect(),
                Body::Variants(e) => e.variants.iter().flat_map(|v| &v.fields).collect(),
            };
            for f in fields {
                if let Ty::Record(n) | Ty::RichEnum(n) = &f.ty {
                    if let Some(dep) = c.user_index.get(n.as_str()) {
                        visit(c, *dep, done, order);
                    }
                }
            }
            order.push(idx);
        }
        let mut done = vec![false; self.users.len()];
        let mut order = Vec::with_capacity(self.users.len());
        for idx in 0..self.users.len() {
            visit(self, idx, &mut done, &mut order);
        }
        order
    }

    fn render_composite_struct(&self, w: &mut CodeWriter, ty: &Ty) {
        let name = self.named(ty).unwrap_or_default();
        w.line(format!("/** `{ty}` */"));
        w.block(format!("struct {name} {{"), "};", |w| match ty {
            Ty::List(elem) => {
                w.line(format!("{}* items;", self.vtype(elem)));
                w.line("size_t len;");
            }
            Ty::Map(k, v) => {
                w.line(format!("{}* keys;", self.vtype(k)));
                w.line(format!("{}* values;", self.vtype(v)));
                w.line("size_t len;");
            }
            _ => {}
        });
        w.blank();
    }

    fn render_fields(&self, w: &mut CodeWriter, fields: &[FieldBinding]) {
        for f in fields {
            w.doc(&f.doc, DocCommentStyle::Javadoc);
            w.line(format!("{} {};", self.vtype(&f.ty), c_param_name(&f.name)));
        }
    }

    fn render_user_struct(&self, w: &mut CodeWriter, u: &UserType<'_>) {
        w.doc(&u.doc, DocCommentStyle::Javadoc);
        w.block(format!("struct {} {{", u.name), "};", |w| match u.body {
            Body::Fields([]) => {
                w.line("uint8_t empty_;");
            }
            Body::Fields(fields) => self.render_fields(w, fields),
            Body::Variants(e) => {
                w.line(format!("{}_Tag tag;", e.c_tag));
                let payloads: Vec<_> = e.variants.iter().filter(|v| !v.fields.is_empty()).collect();
                if !payloads.is_empty() {
                    w.block("union {", "} as;", |w| {
                        for v in payloads {
                            w.doc(&v.doc, DocCommentStyle::Javadoc);
                            w.block("struct {", format!("}} {};", c_param_name(&v.name)), |w| {
                                self.render_fields(w, &v.fields);
                            });
                        }
                    });
                }
            }
        });
        w.blank();
    }

    /// The codec surface of every named type: user structs, then the
    /// optional, list, and map shapes.
    fn units(&self) -> Vec<Unit> {
        let mut units: Vec<Unit> = self.users.iter().map(|u| self.user_unit(u)).collect();
        units.extend(self.composites.iter().map(|ty| self.composite_unit(ty)));
        units
    }

    fn user_unit(&self, u: &UserType<'_>) -> Unit {
        let p = self.prefix;
        let mut write = Vec::new();
        let mut read = vec!["memset(out, 0, sizeof *out);".to_string()];
        let mut free = Vec::new();
        match u.body {
            Body::Fields(fields) => {
                if fields.is_empty() {
                    write.push("(void)w;".into());
                    write.push("(void)v;".into());
                    read.push("(void)r;".into());
                }
                for f in fields {
                    let field = c_param_name(&f.name);
                    write.push(self.write_stmt(&f.ty, &format!("v->{field}")));
                    read.push(self.read_stmt(&f.ty, &format!("out->{field}")));
                    free.extend(self.free_stmt(&f.ty, &format!("v->{field}")));
                }
            }
            Body::Variants(e) => {
                let member =
                    |v: &str, f: &str| format!("as.{}.{}", c_param_name(v), c_param_name(f));
                write.push(format!("{p}_writer_put_i32(w, (int32_t)v->tag);"));
                write.push("switch (v->tag) {".into());
                read.push(format!("int32_t tag = {p}_reader_get_i32(r);"));
                read.push("switch (tag) {".into());
                let mut free_cases = Vec::new();
                for v in &e.variants {
                    write.push(format!("case {}:", v.c_const));
                    read.push(format!("case {}:", v.c_const));
                    read.push(format!("    out->tag = {};", v.c_const));
                    let mut frees = Vec::new();
                    for f in &v.fields {
                        let m = member(&v.name, &f.name);
                        write.push(format!(
                            "    {}",
                            self.write_stmt(&f.ty, &format!("v->{m}"))
                        ));
                        read.push(format!(
                            "    {}",
                            self.read_stmt(&f.ty, &format!("out->{m}"))
                        ));
                        frees.extend(self.free_stmt(&f.ty, &format!("v->{m}")));
                    }
                    write.push("    break;".into());
                    read.push("    break;".into());
                    if !frees.is_empty() {
                        free_cases.push(format!("case {}:", v.c_const));
                        free_cases.extend(frees.into_iter().map(|l| format!("    {l}")));
                        free_cases.push("    break;".into());
                    }
                }
                write.extend(
                    ["default:", "    w->failed = true;", "    break;", "}"].map(String::from),
                );
                read.extend(
                    ["default:", "    r->failed = true;", "    break;", "}"].map(String::from),
                );
                if !free_cases.is_empty() {
                    free.push("switch (v->tag) {".into());
                    free.extend(free_cases);
                    free.extend(["default:", "    break;", "}"].map(String::from));
                }
            }
        }
        free.push("memset(v, 0, sizeof *v);".into());
        Unit {
            name: u.name.clone(),
            vtype: u.name.clone(),
            write_param: format!("const {}*", u.name),
            write,
            read,
            free,
        }
    }

    fn composite_unit(&self, ty: &Ty) -> Unit {
        let p = self.prefix;
        let mut write = Vec::new();
        let mut read = Vec::new();
        let mut free = Vec::new();
        match ty {
            Ty::List(elem) => {
                let ev = self.vtype(elem);
                write.push(format!("{p}_writer_put_len(w, v->len);"));
                write.push("for (size_t i = 0; i < v->len; i++) {".into());
                write.push(format!("    {}", self.write_stmt(elem, "v->items[i]")));
                write.push("}".into());
                read.push("memset(out, 0, sizeof *out);".into());
                read.push(format!("size_t n = {p}_reader_get_u32(r);"));
                read.push(format!(
                    "out->items = ({ev}*){p}_reader_alloc(r, n, sizeof({ev}), {});",
                    self.min_wire(elem, 0)
                ));
                read.extend(["if (out->items == NULL) {", "    return;", "}"].map(String::from));
                read.push("out->len = n;".into());
                read.push("for (size_t i = 0; i < n; i++) {".into());
                read.push(format!("    {}", self.read_stmt(elem, "out->items[i]")));
                read.push("}".into());
                if let Some(stmt) = self.free_stmt(elem, "v->items[i]") {
                    free.push("for (size_t i = 0; i < v->len; i++) {".into());
                    free.push(format!("    {stmt}"));
                    free.push("}".into());
                }
                free.push("free(v->items);".into());
                free.push("memset(v, 0, sizeof *v);".into());
            }
            Ty::Map(k, val) => {
                let kv = self.vtype(k);
                let vv = self.vtype(val);
                write.push(format!("{p}_writer_put_len(w, v->len);"));
                write.push("for (size_t i = 0; i < v->len; i++) {".into());
                write.push(format!("    {}", self.write_stmt(k, "v->keys[i]")));
                write.push(format!("    {}", self.write_stmt(val, "v->values[i]")));
                write.push("}".into());
                read.push("memset(out, 0, sizeof *out);".into());
                read.push(format!("size_t n = {p}_reader_get_u32(r);"));
                read.push(format!(
                    "out->keys = ({kv}*){p}_reader_alloc(r, n, sizeof({kv}), {});",
                    self.min_wire(k, 0) + self.min_wire(val, 0)
                ));
                read.push(format!(
                    "out->values = ({vv}*){p}_reader_alloc(r, n, sizeof({vv}), 0);"
                ));
                read.extend(
                    [
                        "if (out->keys == NULL || out->values == NULL) {",
                        "    free(out->keys);",
                        "    free(out->values);",
                        "    out->keys = NULL;",
                        "    out->values = NULL;",
                        "    return;",
                        "}",
                    ]
                    .map(String::from),
                );
                read.push("out->len = n;".into());
                read.push("for (size_t i = 0; i < n; i++) {".into());
                read.push(format!("    {}", self.read_stmt(k, "out->keys[i]")));
                read.push(format!("    {}", self.read_stmt(val, "out->values[i]")));
                read.push("}".into());
                let frees: Vec<String> = [
                    self.free_stmt(k, "v->keys[i]"),
                    self.free_stmt(val, "v->values[i]"),
                ]
                .into_iter()
                .flatten()
                .collect();
                if !frees.is_empty() {
                    free.push("for (size_t i = 0; i < v->len; i++) {".into());
                    free.extend(frees.into_iter().map(|l| format!("    {l}")));
                    free.push("}".into());
                }
                free.push("free(v->keys);".into());
                free.push("free(v->values);".into());
                free.push("memset(v, 0, sizeof *v);".into());
            }
            Ty::Optional(inner) => {
                if let Ty::Interface(n) = inner.as_ref() {
                    let iface = self.interface(n);
                    write.push(format!("{p}_writer_put_bool(w, v != NULL);"));
                    write.push("if (v != NULL) {".into());
                    write.push(format!(
                        "    {p}_writer_put_object(w, {}(v));",
                        iface.clone_symbol
                    ));
                    write.push("}".into());
                    read.push("*out = NULL;".into());
                    read.push(format!("if ({p}_reader_get_bool(r)) {{"));
                    read.push(format!(
                        "    *out = ({}*){p}_reader_get_object(r);",
                        iface.c_tag
                    ));
                    read.push("}".into());
                    free.push(format!("{}(*v);", iface.destroy_symbol));
                    free.push("*v = NULL;".into());
                } else {
                    let iv = self.vtype(inner);
                    write.push(format!("{p}_writer_put_bool(w, v != NULL);"));
                    write.push("if (v != NULL) {".into());
                    write.push(format!("    {}", self.write_stmt(inner, "(*v)")));
                    write.push("}".into());
                    read.push("*out = NULL;".into());
                    read.extend([
                        format!("if (!{p}_reader_get_bool(r)) {{"),
                        "    return;".into(),
                        "}".into(),
                    ]);
                    read.push(format!("{iv}* value = ({iv}*)calloc(1, sizeof({iv}));"));
                    read.extend(
                        [
                            "if (value == NULL) {",
                            "    r->failed = true;",
                            "    return;",
                            "}",
                        ]
                        .map(String::from),
                    );
                    read.push(self.read_stmt(inner, "(*value)"));
                    read.push("*out = value;".into());
                    free.extend(["if (*v == NULL) {", "    return;", "}"].map(String::from));
                    free.extend(self.free_stmt(inner, "(**v)"));
                    free.push("free(*v);".into());
                    free.push("*v = NULL;".into());
                }
            }
            _ => unreachable!("only optional, list, and map shapes are composites"),
        }
        Unit {
            name: self.named(ty).unwrap_or_default(),
            vtype: self.vtype(ty),
            write_param: self.write_param(ty),
            write,
            read,
            free,
        }
    }
}

/// The wire primitive of a scalar, string, or bytes type.
fn prim(ty: &Ty) -> Prim {
    match ty.wire() {
        WireType::Prim(p) => p,
        _ => unreachable!("{ty} is not a wire primitive"),
    }
}

/// The C type a decoded primitive is stored in.
fn prim_ctype(p: Prim) -> &'static str {
    match p {
        Prim::Bool => "bool",
        Prim::I8 => "int8_t",
        Prim::I16 => "int16_t",
        Prim::I32 => "int32_t",
        Prim::I64 => "int64_t",
        Prim::U8 => "uint8_t",
        Prim::U16 => "uint16_t",
        Prim::U32 => "uint32_t",
        Prim::U64 => "uint64_t",
        Prim::F32 => "float",
        Prim::F64 => "double",
        Prim::String | Prim::Bytes => unreachable!("strings and bytes are views"),
    }
}

/// A pointer-to-const spelling of `target` that also reads correctly when
/// `target` is itself a pointer (`int64_t* const*`).
fn const_ptr(target: &str) -> String {
    if target.ends_with('*') {
        format!("{target} const*")
    } else {
        format!("const {target}*")
    }
}
