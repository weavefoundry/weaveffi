//! Value-buffer codec emitters.
//!
//! Records, rich enums, optionals, lists, and maps that cross inside a value
//! buffer, and error payloads, use the wire format. The runtime's `$Writer`
//! and `$Reader` implement the wire primitives (`writeI32`, `readString`,
//! ...) and the optional, list, and map combinators. This module emits one
//! writer and one reader function per type a buffer can hold beyond the
//! primitives:
//!
//! * each record and rich enum (`$w$Entry`, `$r$Entry`);
//! * each interface carried as an object token (`$w$Store`, `$r$Store`):
//!   writing one clones the wrapper's native object (`$clone`), and reading
//!   one adopts the reference into a new wrapper (`index.js` defines
//!   `$token` and `$untoken`, which convert between the `u64` token and the
//!   transport's handle);
//! * each composite (`$w_list_opt_Entry` writes `[Entry?]`), from the shared
//!   [`codecs::composites`] and named by its [`codecs::stem`].
//!
//! Call sites then name these functions instead of building closures. Every
//! dispatch goes through [`Ty::wire`], and primitives collapse onto one arm
//! through [`Prim::pascal`](weaveffi_model::ty::Prim::pascal).

use std::collections::BTreeSet;

use weaveffi_model::model::Model;
use weaveffi_model::ty::{Ty, WireType};

use crate::codegen::{codecs, CodeWriter};
use crate::targets::js::names::{decl, helper, js_string};

/// A function expression `(w, v) => ...` writing one value of `ty`.
pub(crate) fn writer_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("$W.{}", p.pascal()),
        WireType::Enum(_) => "$W.I32".into(),
        WireType::User(n) | WireType::Object(n) => helper("w", n),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("$w_{}", codecs::stem(ty))
        }
    }
}

/// A function expression `(r) => value` reading one value of `ty`.
pub(crate) fn reader_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("$R.{}", p.pascal()),
        WireType::Enum(_) => "$R.I32".into(),
        WireType::User(n) | WireType::Object(n) => helper("r", n),
        WireType::Optional(_) | WireType::List(_) | WireType::Map(..) => {
            format!("$r_{}", codecs::stem(ty))
        }
    }
}

/// The statement expression writing `val` of type `ty` to the writer `w`.
pub(crate) fn write_expr(ty: &Ty, val: &str) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("w.write{}({val})", p.pascal()),
        WireType::Enum(_) => format!("w.writeI32({val})"),
        _ => format!("{}(w, {val})", writer_fn(ty)),
    }
}

/// The expression reading one value of type `ty` from the reader `r`.
pub(crate) fn read_expr(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("r.read{}()", p.pascal()),
        WireType::Enum(_) => "r.readI32()".into(),
        _ => format!("{}(r)", reader_fn(ty)),
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

/// Every interface a value buffer in `model` can carry as an object token:
/// those named by a record, variant, or payload field, or inside a
/// composite.
fn tokens(model: &Model, composites: &[Ty]) -> BTreeSet<String> {
    let fields = model
        .records
        .iter()
        .flat_map(|s| &s.fields)
        .chain(
            model
                .enums
                .iter()
                .flat_map(|e| &e.variants)
                .flat_map(|v| &v.fields),
        )
        .chain(
            model
                .errors
                .iter()
                .flat_map(|e| &e.codes)
                .flat_map(|c| &c.fields),
        )
        .map(|f| &f.ty);
    let mut out = BTreeSet::new();
    for ty in fields.chain(composites) {
        collect_interfaces(ty, &mut out);
    }
    out
}

/// Add every interface `ty` names, at any depth, to `out`.
fn collect_interfaces(ty: &Ty, out: &mut BTreeSet<String>) {
    match ty {
        Ty::Interface(n) => {
            out.insert(n.clone());
        }
        Ty::Optional(inner) | Ty::List(inner) => collect_interfaces(inner, out),
        Ty::Map(k, v) => {
            collect_interfaces(k, out);
            collect_interfaces(v, out);
        }
        Ty::Prim(_) | Ty::Record(_) | Ty::RichEnum(_) | Ty::Enum(_) => {}
    }
}

/// Emit the writer and reader of every record, rich enum, object token, and
/// composite type in the model.
pub(crate) fn emit_codecs(w: &mut CodeWriter, model: &Model) {
    let composites = codecs::composites(model);
    for s in &model.records {
        let name = &s.name;
        w.block(
            format!("function {}(w, v) {{", helper("w", name)),
            "}",
            |w| {
                for f in &s.fields {
                    w.line(format!("{};", write_expr(&f.ty, &format!("v.{}", f.name))));
                }
            },
        );
        w.block(format!("function {}(r) {{", helper("r", name)), "}", |w| {
            if s.fields.is_empty() {
                w.line("return {};");
            } else {
                w.block("return {", "};", |w| {
                    for f in &s.fields {
                        w.line(format!("{}: {},", f.name, read_expr(&f.ty)));
                    }
                });
            }
        });
    }
    for e in model.enums.iter().filter(|e| e.is_rich()) {
        let name = &e.name;
        let what = js_string(&format!("unknown {} tag: ", e.name));
        w.block(
            format!("function {}(w, v) {{", helper("w", name)),
            "}",
            |w| {
                w.block("switch (v.tag) {", "}", |w| {
                    for v in &e.variants {
                        w.line(format!("case {}:", js_string(&v.name)));
                        w.scope(|w| {
                            w.line(format!("w.writeI32({});", v.value));
                            for f in &v.fields {
                                w.line(format!("{};", write_expr(&f.ty, &format!("v.{}", f.name))));
                            }
                            w.line("return;");
                        });
                    }
                    w.line("default:");
                    w.line(format!("  throw new TypeError({what} + (v && v.tag));"));
                });
            },
        );
        w.block(format!("function {}(r) {{", helper("r", name)), "}", |w| {
            w.line("const tag = r.readI32();");
            w.block("switch (tag) {", "}", |w| {
                for v in &e.variants {
                    let fields: String = v
                        .fields
                        .iter()
                        .map(|f| format!(", {}: {}", f.name, read_expr(&f.ty)))
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
        });
    }
    for name in tokens(model, &composites) {
        let cls = decl(&name);
        w.block(
            format!("function {}(w, v) {{", helper("w", &name)),
            "}",
            |w| {
                w.line(format!("w.writeU64($untoken($clone(v, {cls})));"));
            },
        );
        w.block(format!("function {}(r) {{", helper("r", &name)), "}", |w| {
            w.line(format!("return $adopt({cls}, $token(r.readU64()));"));
        });
    }
    for ty in &composites {
        let (write, read) = match ty.wire() {
            WireType::Optional(inner) => (
                format!("w.writeOpt(v, {});", writer_fn(inner)),
                format!("return r.readOpt({});", reader_fn(inner)),
            ),
            WireType::List(inner) => (
                format!("w.writeList(v, {});", writer_fn(inner)),
                format!("return r.readList({});", reader_fn(inner)),
            ),
            WireType::Map(k, v) => (
                format!("w.writeMap(v, {}, {});", key_writer_fn(k), writer_fn(v)),
                format!("return r.readMap({}, {});", reader_fn(k), reader_fn(v)),
            ),
            other => unreachable!("{other:?} is not a composite"),
        };
        w.line(format!("/** `{ty}` */"));
        w.block(format!("function {}(w, v) {{", writer_fn(ty)), "}", |w| {
            w.line(write);
        });
        w.block(format!("function {}(r) {{", reader_fn(ty)), "}", |w| {
            w.line(read);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use weaveffi_model::ty::Prim;

    #[test]
    fn primitives_collapse_onto_pascal_names() {
        assert_eq!(write_expr(&Ty::Prim(Prim::I64), "v.n"), "w.writeI64(v.n)");
        assert_eq!(read_expr(&Ty::Prim(Prim::String)), "r.readString()");
        assert_eq!(writer_fn(&Ty::Prim(Prim::Bytes)), "$W.Bytes");
        assert_eq!(reader_fn(&Ty::Enum("Kind".into())), "$R.I32");
    }

    #[test]
    fn composites_are_named_by_the_shared_stem() {
        let ty = Ty::Optional(Box::new(Ty::List(Box::new(Ty::Record("Entry".into())))));
        assert_eq!(write_expr(&ty, "v.x"), "$w_opt_list_Entry(w, v.x)");
        assert_eq!(read_expr(&ty), "$r_opt_list_Entry(r)");
        let map = Ty::Map(
            Box::new(Ty::Prim(Prim::U64)),
            Box::new(Ty::Interface("Store".into())),
        );
        assert_eq!(writer_fn(&map), "$w_map_u64_Store");
        assert_eq!(reader_fn(&Ty::Interface("Store".into())), "$r$Store");
    }

    #[test]
    fn object_tokens_clone_and_adopt() {
        let m = crate::codegen::test_model(
            r#"
version: "0.13.0"
interfaces: [{ name: Store, methods: [{ name: get }] }]
records: [{ name: Holder, fields: [{ name: stores, type: "[Store]" }] }]
"#,
        );
        let mut w = CodeWriter::two_space();
        emit_codecs(&mut w, &m);
        let js = w.finish();
        assert!(
            js.contains("w.writeU64($untoken($clone(v, Store$)));"),
            "{js}"
        );
        assert!(
            js.contains("return $adopt(Store$, $token(r.readU64()));"),
            "{js}"
        );
        assert!(js.contains("function $w_list_Store(w, v) {"), "{js}");
    }
}
