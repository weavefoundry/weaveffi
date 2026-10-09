//! Value-buffer codec emitters.
//!
//! Records, rich enums, optionals, lists, maps, and error payloads cross the
//! ABI as value buffers. The runtime's `$Writer` and `$Reader` implement the
//! wire primitives (`writeI32`, `readString`, ...) and the optional, list,
//! and map combinators. This module emits one writer and one reader
//! function per type a buffer can hold beyond the primitives:
//!
//! * each record and rich enum (`$w$kv$Entry`, `$r$kv$Entry`), named by its
//!   declaring module's path;
//! * each interface carried as an object token (`$w$kv$Store`,
//!   `$r$kv$Store`): writing one clones the wrapper's native object
//!   (`$clone`), and reading one adopts the reference into a new wrapper
//!   (the transport defines `$token`, which turns the `u64` token into its
//!   handle representation);
//! * each distinct optional, list, and map type anywhere in the API
//!   (`$w_list_opt_Entry` writes `[Entry?]`), named after the type's shape
//!   with bare type names, which are unique across the API.
//!
//! Call sites then name these functions instead of building closures. Every
//! dispatch goes through [`Ty::wire`], and primitives collapse onto one arm
//! through [`Prim::pascal`].

use std::collections::{BTreeMap, BTreeSet};

use weaveffi_model::model::{CallShape, Model};
use weaveffi_model::ty::{Family, Ty, WireType};

use crate::codegen::CodeWriter;
use crate::targets::js::names::{helper, js_string, type_decl};

/// The name stem of a composite (optional, list, or map) type's codec
/// functions: `opt_Entry`, `list_opt_Entry`, `map_string_Store`.
fn mangle(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => p.snake().to_string(),
        WireType::Enum(n) | WireType::User(n) | WireType::Object(n) => n.to_string(),
        WireType::Optional(inner) => format!("opt_{}", mangle(inner)),
        WireType::List(inner) => format!("list_{}", mangle(inner)),
        WireType::Map(k, v) => format!("map_{}_{}", mangle(k), mangle(v)),
    }
}

/// A function expression `(w, v) => ...` writing one value of `ty`.
pub(crate) fn writer_fn(model: &Model, ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("$W.{}", p.pascal()),
        WireType::Enum(_) => "$W.I32".into(),
        WireType::User(n) | WireType::Object(n) => helper(model, "w", n),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("$w_{}", mangle(ty))
        }
    }
}

/// A function expression `(r) => value` reading one value of `ty`.
pub(crate) fn reader_fn(model: &Model, ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("$R.{}", p.pascal()),
        WireType::Enum(_) => "$R.I32".into(),
        WireType::User(n) | WireType::Object(n) => helper(model, "r", n),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("$r_{}", mangle(ty))
        }
    }
}

/// The statement expression writing `val` of type `ty` to the writer `w`.
pub(crate) fn write_expr(model: &Model, ty: &Ty, val: &str) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("w.write{}({val})", p.pascal()),
        WireType::Enum(_) => format!("w.writeI32({val})"),
        _ => format!("{}(w, {val})", writer_fn(model, ty)),
    }
}

/// The expression reading one value of type `ty` from the reader `r`.
pub(crate) fn read_expr(model: &Model, ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("r.read{}()", p.pascal()),
        WireType::Enum(_) => "r.readI32()".into(),
        _ => format!("{}(r)", reader_fn(model, ty)),
    }
}

/// The writer of one map key (see the runtime's `$WK`).
fn key_writer_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("$WK.{}", p.pascal()),
        WireType::Enum(_) => "$WK.I32".into(),
        other => unreachable!("validation admits no {other:?} map keys"),
    }
}

/// Every type a value buffer in `model` can hold that needs codec functions
/// of its own beyond records and rich enums: the composites by name stem,
/// and the interfaces carried as object tokens.
#[derive(Default)]
struct Shapes<'a> {
    composites: BTreeMap<String, &'a Ty>,
    objects: BTreeSet<&'a str>,
}

impl<'a> Shapes<'a> {
    fn of(model: &'a Model) -> Self {
        let mut shapes = Shapes::default();
        for m in &model.modules {
            let fields = m
                .structs
                .iter()
                .flat_map(|s| &s.fields)
                .chain(
                    m.enums
                        .iter()
                        .flat_map(|e| e.variants.iter())
                        .flat_map(|v| &v.fields),
                )
                .chain(
                    m.errors
                        .iter()
                        .flat_map(|e| &e.codes)
                        .flat_map(|c| &c.fields),
                );
            for f in fields {
                shapes.visit(&f.ty);
            }
            for f in m.callables() {
                for p in &f.params {
                    shapes.visit_slot(&p.ty);
                }
                match &f.shape {
                    CallShape::Iterator(it) => shapes.visit_slot(&it.elem),
                    _ => {
                        if let Some(ty) = &f.ret {
                            shapes.visit_slot(ty);
                        }
                    }
                }
            }
            for method in m.callback_interfaces.iter().flat_map(|cb| &cb.methods) {
                for p in &method.params {
                    shapes.visit_slot(&p.ty);
                }
                if let Some(ty) = &method.ret {
                    shapes.visit_slot(ty);
                }
            }
        }
        shapes
    }

    /// A parameter, return, or element type: only buffers hold codecs.
    fn visit_slot(&mut self, ty: &'a Ty) {
        if ty.family() == Family::Buffer {
            self.visit(ty);
        }
    }

    /// A type inside a value buffer.
    fn visit(&mut self, ty: &'a Ty) {
        match ty.wire() {
            WireType::Prim(_) | WireType::Enum(_) | WireType::User(_) => {}
            WireType::Object(n) => {
                self.objects.insert(n);
            }
            WireType::Optional(inner) | WireType::List(inner) => {
                self.composites.insert(mangle(ty), ty);
                self.visit(inner);
            }
            WireType::Map(k, v) => {
                self.composites.insert(mangle(ty), ty);
                self.visit(k);
                self.visit(v);
            }
        }
    }
}

/// Emit the writer and reader of every record, rich enum, object token, and
/// composite type in the model.
pub(crate) fn emit_codecs(w: &mut CodeWriter, model: &Model) {
    let shapes = Shapes::of(model);
    for m in &model.modules {
        for s in &m.structs {
            let name = &s.name;
            w.block(
                format!("function {}(w, v) {{", helper(model, "w", name)),
                "}",
                |w| {
                    for f in &s.fields {
                        w.line(format!(
                            "{};",
                            write_expr(model, &f.ty, &format!("v.{}", f.name))
                        ));
                    }
                },
            );
            w.block(
                format!("function {}(r) {{", helper(model, "r", name)),
                "}",
                |w| {
                    if s.fields.is_empty() {
                        w.line("return {};");
                    } else {
                        w.block("return {", "};", |w| {
                            for f in &s.fields {
                                w.line(format!("{}: {},", f.name, read_expr(model, &f.ty)));
                            }
                        });
                    }
                },
            );
        }
        for e in m.enums.iter().filter(|e| e.rich) {
            let name = &e.name;
            let what = js_string(&format!("unknown {} tag: ", e.name));
            w.block(
                format!("function {}(w, v) {{", helper(model, "w", name)),
                "}",
                |w| {
                    w.block("switch (v.tag) {", "}", |w| {
                        for v in &e.variants {
                            w.line(format!("case {}:", js_string(&v.name)));
                            w.scope(|w| {
                                w.line(format!("w.writeI32({});", v.value));
                                for f in &v.fields {
                                    w.line(format!(
                                        "{};",
                                        write_expr(model, &f.ty, &format!("v.{}", f.name))
                                    ));
                                }
                                w.line("return;");
                            });
                        }
                        w.line("default:");
                        w.line(format!("  throw new TypeError({what} + (v && v.tag));"));
                    });
                },
            );
            w.block(
                format!("function {}(r) {{", helper(model, "r", name)),
                "}",
                |w| {
                    w.line("const tag = r.readI32();");
                    w.block("switch (tag) {", "}", |w| {
                        for v in &e.variants {
                            let fields: String = v
                                .fields
                                .iter()
                                .map(|f| format!(", {}: {}", f.name, read_expr(model, &f.ty)))
                                .collect();
                            w.line(format!(
                                "case {}: return {{ tag: {}{fields} }};",
                                v.value,
                                js_string(&v.name)
                            ));
                        }
                        w.line("default:");
                        w.line(format!("  throw new $Error(-3, {what} + tag);"));
                    });
                },
            );
        }
    }
    for name in &shapes.objects {
        let cls = type_decl(model, name);
        w.block(
            format!("function {}(w, v) {{", helper(model, "w", name)),
            "}",
            |w| {
                w.line(format!("w.writeU64(BigInt($clone(v, {cls})));"));
            },
        );
        w.block(
            format!("function {}(r) {{", helper(model, "r", name)),
            "}",
            |w| {
                w.line(format!("return $adopt({cls}, $token(r.readU64()));"));
            },
        );
    }
    for ty in shapes.composites.values() {
        let (write, read) = match ty.wire() {
            WireType::Optional(inner) => (
                format!("w.writeOpt(v, {});", writer_fn(model, inner)),
                format!("return r.readOpt({});", reader_fn(model, inner)),
            ),
            WireType::List(inner) => (
                format!("w.writeList(v, {});", writer_fn(model, inner)),
                format!("return r.readList({});", reader_fn(model, inner)),
            ),
            WireType::Map(k, v) => (
                format!(
                    "w.writeMap(v, {}, {});",
                    key_writer_fn(k),
                    writer_fn(model, v)
                ),
                format!(
                    "return r.readMap({}, {});",
                    reader_fn(model, k),
                    reader_fn(model, v)
                ),
            ),
            other => unreachable!("{other:?} is not a composite"),
        };
        w.line(format!("/** `{ty}` */"));
        w.block(
            format!("function {}(w, v) {{", writer_fn(model, ty)),
            "}",
            |w| {
                w.line(write);
            },
        );
        w.block(
            format!("function {}(r) {{", reader_fn(model, ty)),
            "}",
            |w| {
                w.line(read);
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::js::names::tests::model;
    use weaveffi_model::ty::Prim;

    #[test]
    fn primitives_collapse_onto_pascal_names() {
        let m = model();
        assert_eq!(
            write_expr(&m, &Ty::Prim(Prim::I64), "v.n"),
            "w.writeI64(v.n)"
        );
        assert_eq!(read_expr(&m, &Ty::Prim(Prim::String)), "r.readString()");
        assert_eq!(writer_fn(&m, &Ty::Prim(Prim::Bytes)), "$W.Bytes");
        assert_eq!(reader_fn(&m, &Ty::Enum("Kind".into())), "$R.I32");
    }

    #[test]
    fn composites_name_one_function_per_shape() {
        let m = model();
        let ty = Ty::Optional(Box::new(Ty::List(Box::new(Ty::Record("Entry".into())))));
        assert_eq!(write_expr(&m, &ty, "v.x"), "$w_opt_list_Entry(w, v.x)");
        assert_eq!(read_expr(&m, &ty), "$r_opt_list_Entry(r)");
        let map = Ty::Map(
            Box::new(Ty::Prim(Prim::U64)),
            Box::new(Ty::Interface("Store".into())),
        );
        assert_eq!(writer_fn(&m, &map), "$w_map_u64_Store");
        assert_eq!(reader_fn(&m, &Ty::Interface("Store".into())), "$r$kv$Store");
    }

    #[test]
    fn codecs_cover_every_buffered_shape_once() {
        let yaml = r#"
version: "0.11.0"
modules:
  - name: kv
    structs:
      - name: Entry
        fields:
          - { name: tags, type: "[string]" }
          - { name: by_kind, type: "{Kind:u32}" }
          - { name: owner, type: "Store?" }
    enums: [{ name: Kind, variants: [{ name: A, value: 0 }] }]
    interfaces: [{ name: Store, methods: [{ name: get }] }]
    functions:
      - { name: all, params: [{ name: tags, type: "[string]" }], return: "[Entry?]" }
"#;
        let api = weaveffi_model::parse::parse_api_str(yaml, "yaml").unwrap();
        let m = weaveffi_model::validate::validate(
            &api,
            &weaveffi_model::pkg::Identity::named("t"),
            None,
        )
        .unwrap();
        let mut w = CodeWriter::two_space();
        emit_codecs(&mut w, &m);
        let js = w.finish();
        for needle in [
            "function $w_list_string(w, v) {\n  w.writeList(v, $W.String);\n}",
            "function $r_map_Kind_u32(r) {\n  return r.readMap($R.I32, $R.U32);\n}",
            "function $w_map_Kind_u32(w, v) {\n  w.writeMap(v, $WK.I32, $W.U32);\n}",
            "function $w_list_opt_Entry(w, v) {\n  w.writeList(v, $w_opt_Entry);\n}",
            "function $r_opt_Store(r) {\n  return r.readOpt($r$kv$Store);\n}",
            "function $w$kv$Store(w, v) {\n  w.writeU64(BigInt($clone(v, kv$Store)));\n}",
            "  $w_opt_Store(w, v.owner);\n",
        ] {
            assert!(js.contains(needle), "missing `{needle}` in:\n{js}");
        }
        assert_eq!(js.matches("function $w_list_string(").count(), 1);
    }
}
