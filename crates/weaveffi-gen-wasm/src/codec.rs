//! Value-buffer codegen: the private JS buffer runtime and the per-type
//! codec functions.
//!
//! Buffered values (records, rich enums, optionals, lists, maps, and error
//! payloads) cross the boundary serialized in the WeaveFFI value-buffer wire
//! format. This module emits the JS statements and expressions that encode
//! and decode that format. Every dispatch goes through [`wire::classify`], so
//! the wire-shape folds (handles as `u64` tokens, borrowed views as their
//! owned forms, records and rich enums as one user-codec shape) live in
//! `weaveffi-core`, not here.

use weaveffi_core::abi::lower::split_qualified;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::BindingModel;
use weaveffi_core::wire::{self, WireType};
use weaveffi_ir::ir::TypeRef;

/// The `_write_*`/`_read_*` codec function names for a (possibly
/// `module.Name`-qualified) record or rich enum referenced from
/// `current_module`.
pub(crate) fn buf_codec_names(name: &str, current_module: &str) -> (String, String) {
    let (module, local) = split_qualified(name, current_module);
    (
        format!("_write_{module}_{local}"),
        format!("_read_{module}_{local}"),
    )
}

/// The buffer writer/reader method encoding one leaf wire shape (including
/// strings and bytes, whose length-prefixed forms live in the runtime too).
///
/// # Panics
///
/// Panics on a composite wire shape; callers dispatch enums, user codecs,
/// and containers before falling through to a leaf method.
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
        WireType::U64 | WireType::Handle => "u64",
        WireType::F32 => "f32",
        WireType::F64 => "f64",
        WireType::String => "str",
        WireType::Bytes => "bytes",
        other => unreachable!("composite wire shape dispatched by the caller: {other:?}"),
    }
}

/// Append the statements serializing `val` (a JS expression of IDL type `ty`)
/// into the buffer writer named `wtr`, resolving record and rich-enum
/// references against `module`. `tmp` supplies collision-free local names.
pub(crate) fn emit_buf_write_stmts(
    w: &mut CodeWriter,
    ty: &TypeRef,
    wtr: &str,
    val: &str,
    module: &str,
    tmp: &mut u32,
) {
    match wire::classify(ty) {
        WireType::Enum(_) => {
            w.line(format!("{wtr}.i32({val});"));
        }
        WireType::User(name) => {
            let (write_fn, _) = buf_codec_names(name, module);
            w.line(format!("{write_fn}({wtr}, {val});"));
        }
        WireType::Optional(inner) => {
            w.line(format!("if ({val} === null || {val} === undefined) {{"));
            w.scope(|w| {
                w.line(format!("{wtr}.flag(false);"));
            });
            w.line("} else {");
            w.scope(|w| {
                w.line(format!("{wtr}.flag(true);"));
                emit_buf_write_stmts(w, inner, wtr, val, module, tmp);
            });
            w.line("}");
        }
        WireType::List(inner) => {
            *tmp += 1;
            let arr = format!("_a{tmp}");
            let elem = format!("_e{tmp}");
            w.line(format!("const {arr} = {val} || [];"));
            w.line(format!("{wtr}.len({arr}.length);"));
            w.line(format!("for (const {elem} of {arr}) {{"));
            w.scope(|w| {
                emit_buf_write_stmts(w, inner, wtr, &elem, module, tmp);
            });
            w.line("}");
        }
        WireType::Map(k, v) => {
            *tmp += 1;
            let src = format!("_s{tmp}");
            let ents = format!("_m{tmp}");
            let key = format!("_k{tmp}");
            let value = format!("_v{tmp}");
            w.line(format!("const {src} = {val} || {{}};"));
            w.line(format!(
                "const {ents} = {src} instanceof Map ? [...{src}.entries()] : Object.entries({src});"
            ));
            w.line(format!("{wtr}.len({ents}.length);"));
            w.line(format!("for (const [{key}, {value}] of {ents}) {{"));
            w.scope(|w| {
                emit_buf_write_stmts(w, k, wtr, &key, module, tmp);
                emit_buf_write_stmts(w, v, wtr, &value, module, tmp);
            });
            w.line("}");
        }
        leaf => {
            w.line(format!("{wtr}.{}({val});", scalar_method(leaf)));
        }
    }
}

/// A JS expression decoding one value of IDL type `ty` from the buffer reader
/// named `rdr`, resolving record and rich-enum references against `module`.
/// Composite types recurse; lists and maps expand to inline arrow IIFEs so
/// the whole decode stays a single expression.
pub(crate) fn buf_read_expr(ty: &TypeRef, module: &str, rdr: &str) -> String {
    match wire::classify(ty) {
        // A typed handle is an i32 pointer at the ABI but a u64 on the wire;
        // narrowing back to a JS number keeps the two spellings interchangeable.
        WireType::Handle if matches!(ty, TypeRef::TypedHandle(_)) => {
            format!("Number({rdr}.u64())")
        }
        WireType::Enum(_) => format!("{rdr}.i32()"),
        WireType::User(name) => {
            let (_, read_fn) = buf_codec_names(name, module);
            format!("{read_fn}({rdr})")
        }
        WireType::Optional(inner) => {
            format!(
                "({rdr}.flag() ? {} : null)",
                buf_read_expr(inner, module, rdr)
            )
        }
        WireType::List(inner) => {
            let elem = buf_read_expr(inner, module, rdr);
            format!(
                "(() => {{ const _n = {rdr}.len(); const _arr = []; for (let _i = 0; _i < _n; _i++) _arr.push({elem}); return _arr; }})()"
            )
        }
        WireType::Map(k, v) => {
            let key = buf_read_expr(k, module, rdr);
            let value = buf_read_expr(v, module, rdr);
            format!(
                "(() => {{ const _n = {rdr}.len(); const _obj = {{}}; for (let _i = 0; _i < _n; _i++) {{ const _k = {key}; _obj[_k] = {value}; }} return _obj; }})()"
            )
        }
        leaf => format!("{rdr}.{}()", scalar_method(leaf)),
    }
}

/// Emit the private value-buffer runtime: a growable little-endian writer and
/// a strict reader implementing the WeaveFFI wire format. Malformed input (a
/// producer/consumer contract violation) throws the generic brand error with
/// code `-3`.
pub(crate) fn emit_js_buffer_runtime(out: &mut String) {
    let mut w = CodeWriter::two_space();
    w.line("// Growable little-endian byte writer implementing the WeaveFFI value-buffer");
    w.line("// wire format: values are packed back to back with no alignment; lengths and");
    w.line("// counts are u32; strings are u32 byte length + UTF-8 bytes (no NUL).");
    w.block("class _BufWriter {", "}", |w| {
        w.block("constructor() {", "}", |w| {
            w.line("this._u8 = new Uint8Array(64);");
            w.line("this._dv = new DataView(this._u8.buffer);");
            w.line("this._len = 0;");
        });
        w.block("_need(n) {", "}", |w| {
            w.line("if (this._len + n <= this._u8.length) return;");
            w.line("let cap = this._u8.length * 2;");
            w.line("while (cap < this._len + n) cap *= 2;");
            w.line("const u8 = new Uint8Array(cap);");
            w.line("u8.set(this._u8.subarray(0, this._len));");
            w.line("this._u8 = u8;");
            w.line("this._dv = new DataView(u8.buffer);");
        });
        w.line("bool(v) { this._need(1); this._dv.setUint8(this._len, v ? 1 : 0); this._len += 1; }");
        w.line("i8(v) { this._need(1); this._dv.setInt8(this._len, v); this._len += 1; }");
        w.line("u8(v) { this._need(1); this._dv.setUint8(this._len, v); this._len += 1; }");
        w.line("i16(v) { this._need(2); this._dv.setInt16(this._len, v, true); this._len += 2; }");
        w.line("u16(v) { this._need(2); this._dv.setUint16(this._len, v, true); this._len += 2; }");
        w.line("i32(v) { this._need(4); this._dv.setInt32(this._len, v, true); this._len += 4; }");
        w.line("u32(v) { this._need(4); this._dv.setUint32(this._len, v, true); this._len += 4; }");
        w.line("i64(v) { this._need(8); this._dv.setBigInt64(this._len, BigInt(v), true); this._len += 8; }");
        w.line("u64(v) { this._need(8); this._dv.setBigUint64(this._len, BigInt(v), true); this._len += 8; }");
        w.line("f32(v) { this._need(4); this._dv.setFloat32(this._len, v, true); this._len += 4; }");
        w.line("f64(v) { this._need(8); this._dv.setFloat64(this._len, v, true); this._len += 8; }");
        w.line("len(n) { this.u32(n); }");
        w.line("flag(present) { this.u8(present ? 1 : 0); }");
        w.block("str(v) {", "}", |w| {
            w.line("const b = _enc.encode(v);");
            w.line("this.len(b.length);");
            w.line("this._need(b.length);");
            w.line("this._u8.set(b, this._len);");
            w.line("this._len += b.length;");
        });
        w.block("bytes(v) {", "}", |w| {
            w.line("const b = v instanceof Uint8Array ? v : new Uint8Array(v);");
            w.line("this.len(b.length);");
            w.line("this._need(b.length);");
            w.line("this._u8.set(b, this._len);");
            w.line("this._len += b.length;");
        });
        w.line("finish() { return this._u8.subarray(0, this._len); }");
    });
    w.blank();
    w.line("const _bufDec = new TextDecoder('utf-8', { fatal: true });");
    w.blank();
    w.line("// Strict little-endian reader for the WeaveFFI value-buffer wire format. A");
    w.line("// malformed buffer (truncation, invalid bool or flag bytes, an oversized");
    w.line("// length prefix, invalid UTF-8, trailing bytes) is a producer/consumer");
    w.line("// contract violation and throws the generic brand error; code -3 marks a");
    w.line("// consumer-side marshalling failure.");
    w.block("class _BufReader {", "}", |w| {
        w.block("constructor(bytes) {", "}", |w| {
            w.line("this._u8 = bytes;");
            w.line("this._dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);");
            w.line("this._pos = 0;");
        });
        w.block("_bad(what) {", "}", |w| {
            w.line(format!(
                "throw new {ERROR_BRAND}(-3, 'malformed value buffer: ' + what);"
            ));
        });
        w.block("_take(n, what) {", "}", |w| {
            w.line("if (this._pos + n > this._u8.length) this._bad('truncated ' + what);");
            w.line("const at = this._pos;");
            w.line("this._pos += n;");
            w.line("return at;");
        });
        w.block("bool() {", "}", |w| {
            w.line("const b = this._u8[this._take(1, 'bool')];");
            w.line("if (b > 1) this._bad('bool byte out of range');");
            w.line("return b === 1;");
        });
        w.line("i8() { return this._dv.getInt8(this._take(1, 'i8')); }");
        w.line("u8() { return this._u8[this._take(1, 'u8')]; }");
        w.line("i16() { return this._dv.getInt16(this._take(2, 'i16'), true); }");
        w.line("u16() { return this._dv.getUint16(this._take(2, 'u16'), true); }");
        w.line("i32() { return this._dv.getInt32(this._take(4, 'i32'), true); }");
        w.line("u32() { return this._dv.getUint32(this._take(4, 'u32'), true); }");
        w.line("i64() { return this._dv.getBigInt64(this._take(8, 'i64'), true); }");
        w.line("u64() { return this._dv.getBigUint64(this._take(8, 'u64'), true); }");
        w.line("f32() { return this._dv.getFloat32(this._take(4, 'f32'), true); }");
        w.line("f64() { return this._dv.getFloat64(this._take(8, 'f64'), true); }");
        w.block("len() {", "}", |w| {
            w.line("const n = this.u32();");
            w.line("if (n > this._u8.length - this._pos) this._bad('length prefix exceeds remaining buffer');");
            w.line("return n;");
        });
        w.block("flag() {", "}", |w| {
            w.line("const b = this._u8[this._take(1, 'option flag')];");
            w.line("if (b > 1) this._bad('option flag byte out of range');");
            w.line("return b === 1;");
        });
        w.block("str() {", "}", |w| {
            w.line("const n = this.len();");
            w.line("const at = this._take(n, 'string bytes');");
            w.block("try {", "} catch (e) {", |w| {
                w.line("return _bufDec.decode(this._u8.subarray(at, at + n));");
            });
            w.scope(|w| {
                w.line("this._bad('string is not valid UTF-8');");
            });
            w.line("}");
        });
        w.block("bytes() {", "}", |w| {
            w.line("const n = this.len();");
            w.line("const at = this._take(n, 'byte buffer');");
            w.line("return this._u8.subarray(at, at + n).slice();");
        });
        w.block("end() {", "}", |w| {
            w.line("if (this._pos !== this._u8.length) this._bad('trailing bytes after value');");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit the module-scope `_write_*`/`_read_*` codec pair for every record and
/// rich enum in the model, in model (declaration) order. Field order is fixed
/// at generation time, so the codecs are direct straight-line code with no
/// runtime dispatch.
pub(crate) fn emit_js_buffer_codecs(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    for m in &model.modules {
        for s in &m.structs {
            let (write_fn, read_fn) = buf_codec_names(&s.name, &m.path);
            w.line(format!(
                "// Serialize a `{}.{}` record into the value-buffer wire format.",
                m.path, s.name
            ));
            w.block(format!("function {write_fn}(w, v) {{"), "}", |w| {
                let mut tmp = 0u32;
                for f in &s.fields {
                    emit_buf_write_stmts(
                        w,
                        &f.ty,
                        "w",
                        &format!("v.{}", f.name),
                        &m.path,
                        &mut tmp,
                    );
                }
            });
            w.blank();
            w.line(format!(
                "// Decode a `{}.{}` record from the value-buffer wire format.",
                m.path, s.name
            ));
            w.block(format!("function {read_fn}(r) {{"), "}", |w| {
                w.line("const v = {};");
                for f in &s.fields {
                    w.line(format!(
                        "v.{} = {};",
                        f.name,
                        buf_read_expr(&f.ty, &m.path, "r")
                    ));
                }
                w.line("return v;");
            });
            w.blank();
        }
        for e in m.enums.iter().filter(|e| e.is_rich()) {
            let (write_fn, read_fn) = buf_codec_names(&e.name, &m.path);
            w.line(format!(
                "// Serialize a `{}.{}` rich enum into the value-buffer wire format:",
                m.path, e.name
            ));
            w.line("// an i32 tag, then the active variant's fields in order.");
            w.block(format!("function {write_fn}(w, v) {{"), "}", |w| {
                w.block("switch (v.tag) {", "}", |w| {
                    for v in &e.variants {
                        w.block(format!("case \"{}\": {{", v.name), "}", |w| {
                            w.line(format!("w.i32({});", v.value));
                            let mut tmp = 0u32;
                            for f in &v.fields {
                                emit_buf_write_stmts(
                                    w,
                                    &f.ty,
                                    "w",
                                    &format!("v.{}", f.name),
                                    &m.path,
                                    &mut tmp,
                                );
                            }
                            w.line("break;");
                        });
                    }
                    w.line("default:");
                    w.scope(|w| {
                        w.line(format!(
                            "throw new {ERROR_BRAND}(-3, \"unknown {} variant tag: \" + v.tag);",
                            e.name
                        ));
                    });
                });
            });
            w.blank();
            w.line(format!(
                "// Decode a `{}.{}` rich enum from the value-buffer wire format.",
                m.path, e.name
            ));
            w.block(format!("function {read_fn}(r) {{"), "}", |w| {
                w.line("const _tag = r.i32();");
                w.block("switch (_tag) {", "}", |w| {
                    for v in &e.variants {
                        if v.fields.is_empty() {
                            w.line(format!("case {}:", v.value));
                            w.scope(|w| {
                                w.line(format!("return {{ tag: \"{}\" }};", v.name));
                            });
                        } else {
                            w.block(format!("case {}: {{", v.value), "}", |w| {
                                w.line(format!("const v = {{ tag: \"{}\" }};", v.name));
                                for f in &v.fields {
                                    w.line(format!(
                                        "v.{} = {};",
                                        f.name,
                                        buf_read_expr(&f.ty, &m.path, "r")
                                    ));
                                }
                                w.line("return v;");
                            });
                        }
                    }
                    w.line("default:");
                    w.scope(|w| {
                        w.line(format!(
                            "throw new {ERROR_BRAND}(-3, \"malformed value buffer: unknown {} tag \" + _tag);",
                            e.name
                        ));
                    });
                });
            });
            w.blank();
        }
    }
    out.push_str(&w.finish());
}
