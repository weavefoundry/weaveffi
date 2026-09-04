//! Value-buffer codec emitters for the JS loader.
//!
//! Buffered values (records, rich enums, optionals, lists, maps, and error
//! payloads) cross the addon boundary as `Buffer`s holding the WeaveFFI value
//! buffer encoding. This module emits the JS expressions that read and write
//! that encoding: generic combinators for optionals, lists, and maps, and one
//! generated pack/unpack function per record and rich enum. Every dispatch
//! goes through [`Ty::wire`], so the wire-shape classification (records and
//! rich enums as one user-codec shape, interfaces as object tokens) lives in
//! `weaveffi-core`, not here. The composition is fixed at generation time
//! from the IR; no runtime type dispatch happens.
//!
//! Object tokens follow the reference-counting contract: writing an interface
//! field calls the wrapper's `_cloneHandle()` (which invokes the producer's
//! `_clone` symbol) so the buffer carries a fresh strong reference, and
//! reading one adopts the token into a new wrapper via `_adopt`.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::ModuleBinding;
use weaveffi_core::model::Ty;
use weaveffi_core::model::{Prim, WireType};
use weaveffi_core::utils::local_type_name;

/// The reader/writer method of the private buffer runtime for a scalar wire
/// shape (including strings and bytes, whose length-prefixed reads live in
/// the runtime too). The 64-bit methods read and write JS `bigint`s.
///
/// # Panics
///
/// Panics on a composite wire shape; callers dispatch enums, objects, user
/// codecs, and containers before falling through to a scalar method.
fn scalar_method(wt: WireType<'_>) -> &'static str {
    match wt {
        WireType::Prim(Prim::Bool) => "bool",
        WireType::Prim(Prim::I8) => "i8",
        WireType::Prim(Prim::U8) => "u8",
        WireType::Prim(Prim::I16) => "i16",
        WireType::Prim(Prim::U16) => "u16",
        WireType::Prim(Prim::I32) => "i32",
        WireType::Prim(Prim::U32) => "u32",
        WireType::Prim(Prim::I64) => "i64",
        WireType::Prim(Prim::U64) => "u64",
        WireType::Prim(Prim::F32) => "f32",
        WireType::Prim(Prim::F64) => "f64",
        WireType::Prim(Prim::String) => "str",
        WireType::Prim(Prim::Bytes) => "bytes",
        other => unreachable!("composite wire shape dispatched by the caller: {other:?}"),
    }
}

/// The JS expression writing the object token of wrapper `val` (an instance
/// of the interface's class): a freshly cloned strong reference, never the
/// handle the wrapper keeps for itself.
fn js_object_token_expr(val: &str) -> String {
    format!("w.u64({val}._cloneHandle())")
}

/// The JS expression adopting an object token read from `r` into a new
/// wrapper of interface `name` (possibly dot-qualified; the class is named by
/// its local type name).
fn js_object_adopt_expr(name: &str) -> String {
    format!("{}._adopt(r.u64())", local_type_name(name))
}

/// A JS function expression `(w, v) => void` writing one value of `ty` in the
/// wire format. Records and rich enums name their generated pack function;
/// optionals, lists, and maps compose the generic combinators; interfaces
/// write a cloned object token.
pub(crate) fn js_writer_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Enum(_) => "(w, v) => w.i32(v)".into(),
        WireType::Object(_) => format!("(w, v) => {}", js_object_token_expr("v")),
        WireType::User(n) => format!("__pack{}", local_type_name(n)),
        WireType::Optional(inner) => format!("(w, v) => __wOpt(w, v, {})", js_writer_fn(inner)),
        WireType::List(inner) => format!("(w, v) => __wList(w, v, {})", js_writer_fn(inner)),
        WireType::Map(k, v) => format!(
            "(w, v) => __wMap(w, v, {}, {})",
            js_map_key_writer_fn(k),
            js_writer_fn(v)
        ),
        leaf => format!("(w, v) => w.{}(v)", scalar_method(leaf)),
    }
}

/// A JS function expression `(w, k) => void` writing one *map key*. JS object
/// keys arrive as strings from `Object.keys`, so numeric key types coerce
/// through `Number` (or `BigInt`, inside the 64-bit writer methods) first.
fn js_map_key_writer_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Prim(Prim::String) => "(w, k) => w.str(k)".into(),
        WireType::Prim(Prim::Bool) => "(w, k) => w.bool(k === true || k === 'true')".into(),
        WireType::Prim(Prim::I64) => "(w, k) => w.i64(k)".into(),
        WireType::Prim(Prim::U64) => "(w, k) => w.u64(k)".into(),
        WireType::Enum(_) => "(w, k) => w.i32(Number(k))".into(),
        leaf => format!("(w, k) => w.{}(Number(k))", scalar_method(leaf)),
    }
}

/// A JS function expression `(r) => value` reading one value of `ty` from the
/// wire format. 64-bit integers surface as `bigint`s (matching the TS
/// surface); object tokens are adopted into wrapper instances.
pub(crate) fn js_reader_fn(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Object(n) => format!("(r) => {}", js_object_adopt_expr(n)),
        WireType::Enum(_) => "(r) => r.i32()".into(),
        WireType::User(n) => format!("__unpack{}", local_type_name(n)),
        WireType::Optional(inner) => format!("(r) => __rOpt(r, {})", js_reader_fn(inner)),
        WireType::List(inner) => format!("(r) => __rList(r, {})", js_reader_fn(inner)),
        WireType::Map(k, v) => {
            format!("(r) => __rMap(r, {}, {})", js_reader_fn(k), js_reader_fn(v))
        }
        leaf => format!("(r) => r.{}()", scalar_method(leaf)),
    }
}

/// The JS statement expression writing `val` of type `ty` onto writer `w`.
/// Direct spellings for leaves and generated pack functions; combinator calls
/// for optionals, lists, and maps.
fn js_write_expr(ty: &Ty, val: &str) -> String {
    match ty.wire() {
        WireType::Enum(_) => format!("w.i32({val})"),
        WireType::Object(_) => js_object_token_expr(val),
        WireType::User(n) => format!("__pack{}(w, {val})", local_type_name(n)),
        WireType::Optional(inner) => format!("__wOpt(w, {val}, {})", js_writer_fn(inner)),
        WireType::List(inner) => format!("__wList(w, {val}, {})", js_writer_fn(inner)),
        WireType::Map(k, v) => format!(
            "__wMap(w, {val}, {}, {})",
            js_map_key_writer_fn(k),
            js_writer_fn(v)
        ),
        leaf => format!("w.{}({val})", scalar_method(leaf)),
    }
}

/// The JS expression reading one value of type `ty` from reader `r`.
pub(crate) fn js_read_expr(ty: &Ty) -> String {
    match ty.wire() {
        WireType::Object(n) => js_object_adopt_expr(n),
        WireType::Enum(_) => "r.i32()".into(),
        WireType::User(n) => format!("__unpack{}(r)", local_type_name(n)),
        WireType::Optional(inner) => format!("__rOpt(r, {})", js_reader_fn(inner)),
        WireType::List(inner) => format!("__rList(r, {})", js_reader_fn(inner)),
        WireType::Map(k, v) => format!("__rMap(r, {}, {})", js_reader_fn(k), js_reader_fn(v)),
        leaf => format!("r.{}()", scalar_method(leaf)),
    }
}

/// Emit one module's pack/unpack functions: one pair per record and one pair
/// per rich enum, with fields written and read in declaration (wire) order.
pub(crate) fn render_pack_fns_js(out: &mut String, m: &ModuleBinding) {
    let mut w = CodeWriter::two_space();
    for s in &m.structs {
        w.block(format!("function __pack{}(w, v) {{", s.name), "}", |w| {
            for field in &s.fields {
                w.line(format!(
                    "{};",
                    js_write_expr(&field.ty, &format!("v.{}", field.name))
                ));
            }
        });
        w.block(format!("function __unpack{}(r) {{", s.name), "}", |w| {
            if s.fields.is_empty() {
                w.line("return {};");
            } else {
                w.line("return {");
                for field in &s.fields {
                    w.line(format!("  {}: {},", field.name, js_read_expr(&field.ty)));
                }
                w.line("};");
            }
        });
    }
    for e in &m.enums {
        if !e.is_rich() {
            continue;
        }
        let name = &e.name;
        // Pack: string tag selects the variant; the i32 discriminant plus the
        // variant's fields go on the wire.
        w.block(format!("function __pack{name}(w, v) {{"), "}", |w| {
            w.block("switch (v.tag) {", "}", |w| {
                for v in &e.variants {
                    w.line(format!("case '{}':", v.name));
                    w.line(format!("  w.i32({});", v.value));
                    for field in &v.fields {
                        w.line(format!(
                            "  {};",
                            js_write_expr(&field.ty, &format!("v.{}", field.name))
                        ));
                    }
                    w.line("  break;");
                }
                w.line("default:");
                w.line(format!(
                    "  throw new {ERROR_BRAND}(-2, 'unknown {name} tag: ' + (v && v.tag));"
                ));
            });
        });
        // Unpack: the i32 discriminant selects the variant; fields decode in
        // order and land next to the string tag.
        w.block(format!("function __unpack{name}(r) {{"), "}", |w| {
            w.line("const tag = r.i32();");
            w.block("switch (tag) {", "}", |w| {
                for v in &e.variants {
                    let fields: String = v
                        .fields
                        .iter()
                        .map(|f| format!(", {}: {}", f.name, js_read_expr(&f.ty)))
                        .collect();
                    w.line(format!(
                        "case {}: return {{ tag: '{}'{fields} }};",
                        v.value, v.name
                    ));
                }
                w.line(format!(
                    "default: throw new {ERROR_BRAND}(-2, 'unknown {name} tag: ' + tag);"
                ));
            });
        });
    }
    let text = w.finish();
    if !text.is_empty() {
        out.push_str(&text);
        out.push('\n');
    }
}
