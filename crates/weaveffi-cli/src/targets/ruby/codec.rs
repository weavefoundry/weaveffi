//! Value-buffer codecs: the expressions that pack and unpack one value, the
//! per-record and per-rich-enum codec pairs (`_wv_write_{stem}`,
//! `_wv_read_{stem}`), and one pair per distinct composite type (`[Entry?]`,
//! `{string:i64}`, `Store?`) used anywhere in the API, so no call site
//! inlines a loop.
//!
//! Every dispatch here goes through [`Ty::wire`], so this module never
//! re-derives the wire folds (records and rich enums as one user-codec
//! shape, C-style enums as `i32`, interfaces as `u64` object tokens).
//!
//! An object token carries one strong reference. Writing one reserves the
//! token (`write_object`); the runtime's `_wv_seal` mints the references
//! (the interface's `_clone` symbol) just before the call, so the wrapper
//! keeps its own reference and an abandoned encoding strands none. Reading
//! one (`read_object`) adopts the pointer into a fresh wrapper whose
//! finalizer owes the `_destroy`.

use std::collections::{HashMap, HashSet};

use crate::codegen::CodeWriter;
use heck::ToSnakeCase;
use weaveffi_model::model::{
    CallShape, EnumBinding, FieldBinding, FnBinding, Model, StructBinding,
};
use weaveffi_model::ty::{Family, Ty, WireType};

use crate::targets::ruby::types::rb_field_name;

/// The indefinite article for `word` ("a" or "an"), by its first letter.
fn article(word: &str) -> &'static str {
    match word.chars().next().map(|c| c.to_ascii_lowercase()) {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    }
}

/// The longest constructor call kept on one line.
const MAX_LINE: usize = 100;

/// The snake_case stem naming a record's or rich enum's codec pair:
/// `Contact` becomes `contact`, naming `_wv_write_contact` and
/// `_wv_read_contact`.
pub(crate) fn wv_stem(name: &str) -> String {
    name.to_snake_case()
}

/// The structural stem of a composite type before collision handling:
/// `[Entry?]` is `list_opt_entry`, `{string:i64}` is `map_string_i64`.
fn natural_stem(ty: &Ty) -> String {
    match ty {
        Ty::Prim(p) => p.snake().to_string(),
        Ty::Record(n) | Ty::RichEnum(n) | Ty::Enum(n) | Ty::Interface(n) => wv_stem(n),
        Ty::Optional(inner) => format!("opt_{}", natural_stem(inner)),
        Ty::List(inner) => format!("list_{}", natural_stem(inner)),
        Ty::Map(k, v) => format!("map_{}_{}", natural_stem(k), natural_stem(v)),
        Ty::CallbackInterface(_) | Ty::Iterator(_) => {
            unreachable!("{ty} never appears inside a value buffer")
        }
    }
}

/// The records and rich enums of `model` whose fields can hold an object,
/// directly or through another record or rich enum (a fixed point, so
/// recursive types terminate).
fn user_types_with_objects(model: &Model) -> HashSet<String> {
    let mut fields: Vec<(&str, Vec<&Ty>)> = Vec::new();
    for m in &model.modules {
        for s in &m.structs {
            fields.push((&s.name, s.fields.iter().map(|f| &f.ty).collect()));
        }
        for e in m.enums.iter().filter(|e| e.is_rich()) {
            let tys = e.variants.iter().flat_map(|v| &v.fields).map(|f| &f.ty);
            fields.push((&e.name, tys.collect()));
        }
    }
    let mut found: HashSet<String> = HashSet::new();
    loop {
        let before = found.len();
        for (name, tys) in &fields {
            if found.contains(*name) {
                continue;
            }
            let carries = tys.iter().any(|ty| {
                ty.any(&|t| match t {
                    Ty::Interface(_) => true,
                    Ty::Record(n) | Ty::RichEnum(n) => found.contains(n),
                    _ => false,
                })
            });
            if carries {
                found.insert((*name).to_string());
            }
        }
        if found.len() == before {
            return found;
        }
    }
}

/// The composite types (optionals, lists, and maps inside value buffers) an
/// API uses, each with the stem naming its codec pair.
#[derive(Debug, Default)]
pub(crate) struct Codecs {
    order: Vec<Ty>,
    stems: HashMap<Ty, String>,
    /// The records and rich enums whose encoding can hold an object token.
    with_objects: HashSet<String>,
}

impl Codecs {
    /// Collect every composite type `model` encodes or decodes: in
    /// parameters, returns, async results, iterator elements, record
    /// fields, variant fields, error payload fields, and callback
    /// parameters and returns.
    pub(crate) fn collect(model: &Model) -> Self {
        let mut taken: HashSet<String> = HashSet::new();
        for m in &model.modules {
            taken.extend(m.structs.iter().map(|s| wv_stem(&s.name)));
            taken.extend(
                m.enums
                    .iter()
                    .filter(|e| e.is_rich())
                    .map(|e| wv_stem(&e.name)),
            );
        }
        let mut codecs = Codecs {
            with_objects: user_types_with_objects(model),
            ..Codecs::default()
        };
        let mut add = |ty: &Ty, codecs: &mut Codecs| codecs.visit(ty, &mut taken);
        for m in &model.modules {
            for f in m.callables() {
                for p in &f.params {
                    codecs.position(&p.ty, &mut add);
                }
                codecs.result(f, &mut add);
            }
            for s in &m.structs {
                for f in &s.fields {
                    add(&f.ty, &mut codecs);
                }
            }
            for e in &m.enums {
                for f in e.variants.iter().flat_map(|v| &v.fields) {
                    add(&f.ty, &mut codecs);
                }
            }
            if let Some(eb) = &m.errors {
                for f in eb.codes.iter().flat_map(|c| &c.fields) {
                    add(&f.ty, &mut codecs);
                }
            }
            for cb in &m.callback_interfaces {
                for cm in &cb.methods {
                    for p in &cm.params {
                        codecs.position(&p.ty, &mut add);
                    }
                    if let Some(ret) = &cm.ret {
                        codecs.position(ret, &mut add);
                    }
                }
            }
        }
        codecs
    }

    /// Visit a parameter or return type: only a buffered one is encoded
    /// (`Store?` crosses as a pointer, not a buffer).
    fn position(&mut self, ty: &Ty, add: &mut impl FnMut(&Ty, &mut Codecs)) {
        if ty.family() == Family::Buffer {
            add(ty, self);
        }
    }

    /// Visit a callable's result: the return of a sync or async call, or
    /// the element of an iterator.
    fn result(&mut self, f: &FnBinding, add: &mut impl FnMut(&Ty, &mut Codecs)) {
        match &f.shape {
            CallShape::Iterator(it) => self.position(&it.elem, add),
            _ => {
                if let Some(ret) = &f.ret {
                    self.position(ret, add);
                }
            }
        }
    }

    /// Register `ty` (when it's a composite) and every composite inside it.
    fn visit(&mut self, ty: &Ty, taken: &mut HashSet<String>) {
        match ty.wire() {
            WireType::Optional(inner) | WireType::List(inner) => {
                self.register(ty, taken);
                self.visit(inner, taken);
            }
            WireType::Map(k, v) => {
                self.register(ty, taken);
                self.visit(k, taken);
                self.visit(v, taken);
            }
            _ => {}
        }
    }

    fn register(&mut self, ty: &Ty, taken: &mut HashSet<String>) {
        if self.stems.contains_key(ty) {
            return;
        }
        let natural = natural_stem(ty);
        let mut stem = natural.clone();
        let mut n = 2;
        while !taken.insert(stem.clone()) {
            stem = format!("{natural}_{n}");
            n += 1;
        }
        self.order.push(ty.clone());
        self.stems.insert(ty.clone(), stem);
    }

    /// Whether an encoding of `ty` can hold an object token (directly, or
    /// inside a record or rich enum), so its references must be minted by
    /// `_wv_seal` right before the encoding is handed over.
    pub(crate) fn carries_objects(&self, ty: &Ty) -> bool {
        ty.any(&|t| match t {
            Ty::Interface(_) => true,
            Ty::Record(n) | Ty::RichEnum(n) => self.with_objects.contains(n),
            _ => false,
        })
    }

    fn stem(&self, ty: &Ty) -> &str {
        self.stems
            .get(ty)
            .unwrap_or_else(|| panic!("no codec registered for {ty}"))
    }

    /// The statement appending `expr` (a value of type `ty`) to the buffer
    /// writer `w`. `q` is the receiver (`"Kvstore."` or `""`) qualifying the
    /// module's codec methods inside class bodies.
    pub(crate) fn write(&self, ty: &Ty, w: &str, expr: &str, q: &str) -> String {
        match ty.wire() {
            WireType::Prim(p) => format!("{w}.write_{}({expr})", p.snake()),
            WireType::Enum(_) => format!("{w}.write_i32({expr})"),
            WireType::Object(n) => format!("{w}.write_object({expr}, {n})"),
            WireType::User(n) => format!("{q}_wv_write_{}({w}, {expr})", wv_stem(n)),
            WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
                format!("{q}_wv_write_{}({w}, {expr})", self.stem(ty))
            }
        }
    }

    /// The expression reading one `ty` value from the buffer reader `r`.
    pub(crate) fn read(&self, ty: &Ty, r: &str, q: &str) -> String {
        match ty.wire() {
            WireType::Prim(p) => format!("{r}.read_{}", p.snake()),
            WireType::Enum(_) => format!("{r}.read_i32"),
            WireType::Object(n) => format!("{r}.read_object({n})"),
            WireType::User(n) => format!("{q}_wv_read_{}({r})", wv_stem(n)),
            WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
                format!("{q}_wv_read_{}({r})", self.stem(ty))
            }
        }
    }

    /// Render the codec pair of every registered composite type.
    pub(crate) fn render(&self, w: &mut CodeWriter) {
        if self.order.is_empty() {
            return;
        }
        w.blank();
        w.line("# === Value-buffer codecs of the composite types ===");
        for ty in &self.order {
            let stem = self.stem(ty);
            w.blank();
            w.line("# @api private");
            w.line(format!("# Packs `{ty}` into the value-buffer wire format."));
            w.block(
                format!("def self._wv_write_{stem}(w, v)"),
                "end",
                |w| match ty.wire() {
                    WireType::Optional(inner) => {
                        w.line("if v.nil?");
                        w.scope(|w| {
                            w.line("w.write_flag(false)");
                        });
                        w.line("else");
                        w.scope(|w| {
                            w.line("w.write_flag(true)");
                            w.line(self.write(inner, "w", "v", ""));
                        });
                        w.line("end");
                    }
                    WireType::List(elem) => {
                        w.line("w.write_len(v.length)");
                        w.line(format!(
                            "v.each {{ |e| {} }}",
                            self.write(elem, "w", "e", "")
                        ));
                    }
                    WireType::Map(k, val) => {
                        w.line("w.write_len(v.length)");
                        w.block("v.each do |k, e|", "end", |w| {
                            w.line(self.write(k, "w", "k", ""));
                            w.line(self.write(val, "w", "e", ""));
                        });
                    }
                    _ => unreachable!("only composites are registered"),
                },
            );
            w.blank();
            w.line("# @api private");
            w.line(format!(
                "# Unpacks `{ty}` from the value-buffer wire format."
            ));
            w.block(
                format!("def self._wv_read_{stem}(r)"),
                "end",
                |w| match ty.wire() {
                    WireType::Optional(inner) => {
                        w.line(format!("r.read_flag ? {} : nil", self.read(inner, "r", "")));
                    }
                    WireType::List(elem) => {
                        w.line(format!(
                            "Array.new(r.read_len) {{ {} }}",
                            self.read(elem, "r", "")
                        ));
                    }
                    WireType::Map(k, val) => {
                        w.line(format!(
                            "Array.new(r.read_len) {{ [{}, {}] }}.to_h",
                            self.read(k, "r", ""),
                            self.read(val, "r", "")
                        ));
                    }
                    _ => unreachable!("only composites are registered"),
                },
            );
        }
    }

    /// Emit `{lead}{class}.new(...)` with one keyword argument per field,
    /// each read from `r` in wire order (Ruby evaluates arguments left to
    /// right). `positional` comes first when set (an error's message).
    pub(crate) fn emit_new(
        &self,
        w: &mut CodeWriter,
        lead: &str,
        class: &str,
        positional: Option<&str>,
        fields: &[FieldBinding],
        q: &str,
    ) {
        let mut args: Vec<String> = positional.map(str::to_string).into_iter().collect();
        args.extend(
            fields
                .iter()
                .map(|f| format!("{}: {}", rb_field_name(&f.name), self.read(&f.ty, "r", q))),
        );
        let one_line = format!("{lead}{class}.new({})", args.join(", "));
        match args.as_slice() {
            [] => {
                w.line(format!("{lead}{class}.new"));
            }
            _ if w.indent_str().len() + one_line.len() <= MAX_LINE => {
                w.line(one_line);
            }
            many => {
                w.line(format!("{lead}{class}.new("));
                w.scope(|w| {
                    for a in many {
                        w.line(format!("{a},"));
                    }
                });
                w.line(")");
            }
        }
    }

    /// Render the private codec pair of one record: module singleton methods
    /// `_wv_write_{stem}(w, v)` and `_wv_read_{stem}(r)` serializing the
    /// fields in declaration (wire) order. Writing is duck-typed: any object
    /// with the field readers packs.
    pub(crate) fn render_struct(&self, w: &mut CodeWriter, s: &StructBinding) {
        let stem = wv_stem(&s.name);
        w.blank();
        w.line("# @api private");
        w.line(format!(
            "# Packs {} {} into the value-buffer wire format.",
            article(&s.name),
            s.name
        ));
        let writer = if s.fields.is_empty() {
            "_w, _v"
        } else {
            "w, v"
        };
        w.block(format!("def self._wv_write_{stem}({writer})"), "end", |w| {
            for f in &s.fields {
                let field = rb_field_name(&f.name);
                w.line(self.write(&f.ty, "w", &format!("v.{field}"), ""));
            }
        });
        w.blank();
        w.line("# @api private");
        w.line(format!(
            "# Unpacks {} {} from the value-buffer wire format.",
            article(&s.name),
            s.name
        ));
        let reader = if s.fields.is_empty() { "_r" } else { "r" };
        w.block(format!("def self._wv_read_{stem}({reader})"), "end", |w| {
            self.emit_new(w, "", &s.name, None, &s.fields, "");
        });
    }

    /// Render the private codec pair of one rich enum: `_wv_write_{stem}`
    /// dispatches on the variant class and writes the `i32` tag followed by
    /// the variant's fields; `_wv_read_{stem}` switches on the decoded tag.
    pub(crate) fn render_rich_enum(&self, w: &mut CodeWriter, e: &EnumBinding) {
        let stem = wv_stem(&e.name);
        w.blank();
        w.line("# @api private");
        w.line(format!(
            "# Packs {} {} into the value-buffer wire format.",
            article(&e.name),
            e.name
        ));
        w.block(format!("def self._wv_write_{stem}(w, v)"), "end", |w| {
            w.line("case v");
            for v in &e.variants {
                w.line(format!("when {}::{}", e.name, v.name));
                w.scope(|w| {
                    w.line(format!("w.write_i32({})", v.value));
                    for f in &v.fields {
                        let field = rb_field_name(&f.name);
                        w.line(self.write(&f.ty, "w", &format!("v.{field}"), ""));
                    }
                });
            }
            w.line("else");
            w.scope(|w| {
                w.line(format!(
                    "raise TypeError, \"expected a {}, got #{{v.class}}\"",
                    e.name
                ));
            });
            w.line("end");
        });
        w.blank();
        w.line("# @api private");
        w.line(format!(
            "# Unpacks {} {} from the value-buffer wire format.",
            article(&e.name),
            e.name
        ));
        w.block(format!("def self._wv_read_{stem}(r)"), "end", |w| {
            w.line("tag = r.read_i32");
            w.line("case tag");
            for v in &e.variants {
                w.line(format!("when {}", v.value));
                w.scope(|w| {
                    let class = format!("{}::{}", e.name, v.name);
                    self.emit_new(w, "", &class, None, &v.fields, "");
                });
            }
            w.line("else");
            w.scope(|w| {
                w.line(format!(
                    "raise _wv_malformed(\"unknown {} tag #{{tag}}\")",
                    e.name
                ));
            });
            w.line("end");
        });
    }
}
