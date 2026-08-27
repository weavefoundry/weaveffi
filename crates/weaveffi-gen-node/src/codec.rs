//! Value-buffer codec emitters for the JS loader.
//!
//! Buffered values (records, rich enums, optionals, lists, maps, and error
//! payloads) cross the addon boundary as `Buffer`s holding the WeaveFFI value
//! buffer encoding. This module emits the JS expressions that read and write
//! that encoding: generic combinators for optionals, lists, and maps, and one
//! generated pack/unpack function per record and rich enum. Every dispatch
//! goes through [`wire::classify`], so the wire-shape folds (handles as `u64`
//! tokens, borrowed views as their owned forms, records and rich enums as one
//! user-codec shape) live in `weaveffi-core`, not here. The composition is
//! fixed at generation time from the IR; no runtime type dispatch happens.

use weaveffi_core::abi::is_buffered;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{BindingModel, ModuleBinding};
use weaveffi_core::utils::local_type_name;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

/// The reader/writer method of the private buffer runtime for a scalar wire
/// shape (including strings and bytes, whose length-prefixed reads live in
/// the runtime too).
///
/// # Panics
///
/// Panics on a composite wire shape; callers dispatch enums, handles, user
/// codecs, and containers before falling through to a scalar method.
fn scalar_method(wt: WireType<'_>) -> &'static str {
    match wt {
        WireType::Bool => "bool",
        WireType::I8 => "i8",
        WireType::U8 => "u8",
        WireType::I16 => "i16",
        WireType::U16 => "u16",
        WireType::I32 => "i32",
        WireType::U32 => "u32",
        WireType::I64 => "i64",
        WireType::U64 => "u64",
        WireType::F32 => "f32",
        WireType::F64 => "f64",
        WireType::String => "str",
        WireType::Bytes => "bytes",
        other => unreachable!("composite wire shape dispatched by the caller: {other:?}"),
    }
}

/// A JS function expression `(w, v) => void` writing one value of `ty` in the
/// wire format. Records and rich enums name their generated pack function;
/// optionals, lists, and maps compose the generic combinators.
pub(crate) fn js_writer_fn(ty: &TypeRef) -> String {
    match wire::classify(ty) {
        WireType::Enum(_) => "(w, v) => w.i32(v)".into(),
        WireType::Handle => "(w, v) => w.u64(v)".into(),
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
fn js_map_key_writer_fn(ty: &TypeRef) -> String {
    match wire::classify(ty) {
        WireType::String => "(w, k) => w.str(k)".into(),
        WireType::Bool => "(w, k) => w.bool(k === true || k === 'true')".into(),
        WireType::I64 => "(w, k) => w.i64(k)".into(),
        WireType::U64 | WireType::Handle => "(w, k) => w.u64(k)".into(),
        WireType::Enum(_) => "(w, k) => w.i32(Number(k))".into(),
        leaf => format!("(w, k) => w.{}(Number(k))", scalar_method(leaf)),
    }
}

/// A JS function expression `(r) => value` reading one value of `ty` from the
/// wire format. 64-bit integers surface as numbers (matching the TS surface);
/// handles surface as `BigInt`s except typed handles, which keep the numeric
/// handle spelling the addon uses.
pub(crate) fn js_reader_fn(ty: &TypeRef) -> String {
    match wire::classify(ty) {
        WireType::I64 => "(r) => Number(r.i64())".into(),
        WireType::U64 => "(r) => Number(r.u64())".into(),
        // Both handle kinds share the u64 token; only the JS surface differs.
        WireType::Handle if matches!(ty, TypeRef::TypedHandle(_)) => {
            "(r) => Number(r.u64())".into()
        }
        WireType::Handle => "(r) => r.u64()".into(),
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
fn js_write_expr(ty: &TypeRef, val: &str) -> String {
    match wire::classify(ty) {
        WireType::Enum(_) => format!("w.i32({val})"),
        WireType::Handle => format!("w.u64({val})"),
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
pub(crate) fn js_read_expr(ty: &TypeRef) -> String {
    match wire::classify(ty) {
        WireType::I64 => "Number(r.i64())".into(),
        WireType::U64 => "Number(r.u64())".into(),
        // As in [`js_reader_fn`]: one wire token, two JS spellings.
        WireType::Handle if matches!(ty, TypeRef::TypedHandle(_)) => "Number(r.u64())".into(),
        WireType::Handle => "r.u64()".into(),
        WireType::Enum(_) => "r.i32()".into(),
        WireType::User(n) => format!("__unpack{}(r)", local_type_name(n)),
        WireType::Optional(inner) => format!("__rOpt(r, {})", js_reader_fn(inner)),
        WireType::List(inner) => format!("__rList(r, {})", js_reader_fn(inner)),
        WireType::Map(k, v) => format!("__rMap(r, {}, {})", js_reader_fn(k), js_reader_fn(v)),
        leaf => format!("r.{}()", scalar_method(leaf)),
    }
}

/// True when the loader must embed the buffer runtime: any record or rich
/// enum is declared, any signature position carries a buffered type, or any
/// error code declares payload fields.
pub(crate) fn model_uses_buffers(model: &BindingModel) -> bool {
    model.modules.iter().any(|m| {
        !m.structs.is_empty()
            || m.enums.iter().any(|e| e.is_rich())
            || m.error
                .as_ref()
                .is_some_and(|e| e.declared_here && e.codes.iter().any(|c| !c.fields.is_empty()))
            || m.callbacks
                .iter()
                .any(|cb| cb.params.iter().any(|p| is_buffered(&p.ty)))
            || m.callables().any(|f| {
                f.params.iter().any(|p| is_buffered(&p.ty))
                    || f.ret.as_ref().is_some_and(|t| {
                        is_buffered(t)
                            || matches!(t, TypeRef::Iterator(inner) if is_buffered(inner))
                    })
            })
    })
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
