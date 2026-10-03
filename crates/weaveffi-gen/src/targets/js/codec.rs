//! Value-buffer codec emitters.
//!
//! Records, rich enums, optionals, lists, maps, and error payloads cross the
//! ABI as value buffers. The runtime's `$Writer` and `$Reader` implement the
//! wire primitives (`writeI32`, `readString`, ...) and the optional, list,
//! and map combinators; this module emits the expressions that compose them
//! for a given type, plus one writer and one reader function per record and
//! rich enum (`$w$kv$Entry`, `$r$kv$Entry`). Every dispatch goes through
//! [`Ty::wire`], and primitives collapse onto one arm through
//! [`Prim::pascal`].
//!
//! Object tokens carry a strong reference: writing one clones the wrapper's
//! native object (`$clone`), and reading one adopts the reference into a new
//! wrapper. The transport defines `$token`, which turns the `u64` token into
//! its handle representation.

use weaveffi_model::model::{BindingModel, Prim, Ty, WireType};

use crate::codegen::CodeWriter;
use crate::targets::js::names::{helper, js_string, type_decl};

/// A function expression `(w, v) => ...` writing one value of `ty`.
pub(crate) fn writer_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("$W.{}", p.pascal()),
        WireType::Enum(_) => "$W.I32".into(),
        WireType::User(n) => helper("w", n),
        _ => format!("(w, v) => {}", write_expr(ty, "v")),
    }
}

/// A function expression `(r) => value` reading one value of `ty`.
pub(crate) fn reader_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("$R.{}", p.pascal()),
        WireType::Enum(_) => "$R.I32".into(),
        WireType::User(n) => helper("r", n),
        _ => format!("(r) => {}", read_expr(ty)),
    }
}

/// The expression writing `val` of type `ty` to the writer `w`.
pub(crate) fn write_expr(ty: &Ty, val: &str) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("w.write{}({val})", p.pascal()),
        WireType::Enum(_) => format!("w.writeI32({val})"),
        WireType::Object(n) => format!("w.writeU64(BigInt($clone({val}, {})))", type_decl(n)),
        WireType::User(n) => format!("{}(w, {val})", helper("w", n)),
        WireType::Optional(inner) => format!("w.writeOpt({val}, {})", writer_fn(inner)),
        WireType::List(inner) => format!("w.writeList({val}, {})", writer_fn(inner)),
        WireType::Map(k, v) => format!("w.writeMap({val}, {}, {})", key_writer_fn(k), writer_fn(v)),
    }
}

/// The expression reading one value of type `ty` from the reader `r`.
pub(crate) fn read_expr(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(p) => format!("r.read{}()", p.pascal()),
        WireType::Enum(_) => "r.readI32()".into(),
        WireType::Object(n) => format!("$adopt({}, $token(r.readU64()))", type_decl(n)),
        WireType::User(n) => format!("{}(r)", helper("r", n)),
        WireType::Optional(inner) => format!("r.readOpt({})", reader_fn(inner)),
        WireType::List(inner) => format!("r.readList({})", reader_fn(inner)),
        WireType::Map(k, v) => format!("r.readMap({}, {})", reader_fn(k), reader_fn(v)),
    }
}

/// A writer for one map key. Plain-object keys are always strings, so
/// numeric, `bigint`, `bool`, and enum keys convert first (a `Map`'s typed
/// keys pass through the same conversions unchanged).
fn key_writer_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(Prim::String) => "$W.String".into(),
        WireType::Prim(Prim::Bool) => "(w, k) => w.writeBool(k === true || k === 'true')".into(),
        WireType::Prim(p @ (Prim::I64 | Prim::U64)) => {
            format!("(w, k) => w.write{}(BigInt(k))", p.pascal())
        }
        WireType::Prim(p) => format!("(w, k) => w.write{}(Number(k))", p.pascal()),
        WireType::Enum(_) => "(w, k) => w.writeI32(Number(k))".into(),
        other => unreachable!("validation admits no {other:?} map keys"),
    }
}

/// Emit the writer and reader of every record and rich enum in the model.
pub(crate) fn emit_codecs(w: &mut CodeWriter, model: &BindingModel) {
    for m in &model.modules {
        for s in &m.structs {
            let dotted = format!("{}.{}", m.dot_path, s.name);
            w.block(
                format!("function {}(w, v) {{", helper("w", &dotted)),
                "}",
                |w| {
                    for f in &s.fields {
                        w.line(format!("{};", write_expr(&f.ty, &format!("v.{}", f.name))));
                    }
                },
            );
            w.block(
                format!("function {}(r) {{", helper("r", &dotted)),
                "}",
                |w| {
                    if s.fields.is_empty() {
                        w.line("return {};");
                    } else {
                        w.block("return {", "};", |w| {
                            for f in &s.fields {
                                w.line(format!("{}: {},", f.name, read_expr(&f.ty)));
                            }
                        });
                    }
                },
            );
        }
        for e in m.enums.iter().filter(|e| e.rich) {
            let dotted = format!("{}.{}", m.dot_path, e.name);
            let what = js_string(&format!("unknown {} tag: ", e.name));
            w.block(
                format!("function {}(w, v) {{", helper("w", &dotted)),
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
                                        write_expr(&f.ty, &format!("v.{}", f.name))
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
                format!("function {}(r) {{", helper("r", &dotted)),
                "}",
                |w| {
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
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitives_collapse_onto_pascal_names() {
        assert_eq!(write_expr(&Ty::I64, "v.n"), "w.writeI64(v.n)");
        assert_eq!(read_expr(&Ty::StringUtf8), "r.readString()");
        assert_eq!(writer_fn(&Ty::Bytes), "$W.Bytes");
        assert_eq!(reader_fn(&Ty::Enum("kv.Kind".into())), "$R.I32");
    }

    #[test]
    fn composites_use_the_runtime_combinators() {
        let ty = Ty::Optional(Box::new(Ty::List(Box::new(Ty::Record("kv.Entry".into())))));
        assert_eq!(
            write_expr(&ty, "v.x"),
            "w.writeOpt(v.x, (w, v) => w.writeList(v, $w$kv$Entry))"
        );
        assert_eq!(read_expr(&ty), "r.readOpt((r) => r.readList($r$kv$Entry))");
        let map = Ty::Map(
            Box::new(Ty::U64),
            Box::new(Ty::Interface("kv.Store".into())),
        );
        assert_eq!(
            write_expr(&map, "v.m"),
            "w.writeMap(v.m, (w, k) => w.writeU64(BigInt(k)), (w, v) => w.writeU64(BigInt($clone(v, kv$Store))))"
        );
        assert_eq!(
            read_expr(&map),
            "r.readMap($R.U64, (r) => $adopt(kv$Store, $token(r.readU64())))"
        );
    }
}
