//! The fixed JS runtime the loader embeds: string, byte, and error-slot
//! helpers, the error class hierarchy with its per-domain factories and
//! checkers, the lazy iterator wrapper, and the trampoline registrar.
//!
//! Everything here is emitted conditionally by the loader assembler in
//! [`crate::entities`], keyed off which features the API actually uses.

use heck::ToShoutySnakeCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{BindingModel, ErrorBinding};

use crate::codec::buf_read_expr;
use crate::types::{
    js_code_class_name, js_error_checker_name, js_error_factory_name, js_str_literal,
};

/// Emit the module-scope error classes: the generic `WeaveFFIError` base
/// (unknown codes, marshalling failures, panics), then one domain class per
/// declaring module (`class KvError extends WeaveFFIError`) with one subclass
/// per code carrying its stable `CODE` and default message. Each code class
/// is also aliased onto its domain class (`KvError.KeyNotFound`), and each
/// domain gets a factory that builds the matching subclass and decodes any
/// declared payload fields from the error's value buffer into properties on
/// the thrown error.
///
/// Domain codes are validated positive-only, so the frozen code table never
/// contains a negative key: the reserved runtime codes (`-1` generic, `-2`
/// panic, `-3` marshalling) always miss the table and fall through to the
/// generic brand error.
pub(crate) fn emit_js_error_classes(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    w.line("/** Base error for WeaveFFI failures: domain errors extend it, and it is");
    w.line(" * thrown directly for unknown codes, marshalling failures, and producer");
    w.line(" * panics. Carries the stable ABI `code`. */");
    w.block(format!("export class {ERROR_BRAND} extends Error {{"), "}", |w| {
        w.block("constructor(code, message) {", "}", |w| {
            w.line("super(message ? `WeaveFFI error ${code}: ${message}` : `WeaveFFI error ${code}`);");
            w.line("this.name = new.target.name;");
            w.line("this.code = code;");
        });
    });
    w.blank();

    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|eb| eb.declared_here) else {
            continue;
        };
        let domain = &eb.type_name;
        w.line(format!(
            "/** Base error for the `{}` module's error domain. */",
            m.path
        ));
        w.line(format!("export class {domain} extends {ERROR_BRAND} {{}}"));
        w.blank();
        for c in &eb.codes {
            let class = js_code_class_name(&c.name);
            let message = js_str_literal(&c.message);
            let doc = c
                .doc
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .unwrap_or(&c.message);
            for line in doc.lines() {
                w.line(format!("// {line}"));
            }
            w.block(
                format!("export class {class} extends {domain} {{"),
                "}",
                |w| {
                    w.block(
                        format!("constructor(message = \"{message}\") {{"),
                        "}",
                        |w| {
                            w.line(format!("super({}, message);", c.value));
                        },
                    );
                },
            );
            w.line(format!("{class}.CODE = {};", c.value));
            w.line(format!("{domain}.{class} = {class};"));
            w.blank();
        }

        let table = js_error_code_table_name(eb);
        let factory = js_error_factory_name(eb);
        let has_payload = eb.codes.iter().any(|c| !c.fields.is_empty());
        w.block(format!("const {table} = Object.freeze({{"), "});", |w| {
            for c in &eb.codes {
                w.line(format!("{}: {},", c.value, js_code_class_name(&c.name)));
            }
        });
        w.blank();
        w.line(format!(
            "// Build the {domain} subclass matching `code`, or a generic"
        ));
        w.line(format!(
            "// {ERROR_BRAND} for codes outside the domain (panics, marshalling)."
        ));
        if has_payload {
            w.line("// Codes that declare payload fields decode them from the error's");
            w.line("// borrowed value buffer into properties on the thrown error.");
        }
        w.block(
            format!("function {factory}(wasm, code, message, payloadPtr, payloadLen) {{"),
            "}",
            |w| {
                w.line(format!("const _cls = {table}[code];"));
                w.line(format!(
                    "const _e = _cls ? (message ? new _cls(message) : new _cls()) : new {ERROR_BRAND}(code, message);"
                ));
                if has_payload {
                    w.block("switch (code) {", "}", |w| {
                        for c in eb.codes.iter().filter(|c| !c.fields.is_empty()) {
                            w.block(format!("case {}: {{", c.value), "}", |w| {
                                w.line(
                                    "const _b = payloadPtr === 0 || payloadLen === 0 ? new Uint8Array(0) : new Uint8Array(wasm.memory.buffer, payloadPtr, payloadLen).slice();",
                                );
                                w.line("const _rd = new _BufReader(_b);");
                                for f in &c.fields {
                                    w.line(format!(
                                        "_e.{} = {};",
                                        f.name,
                                        buf_read_expr(&f.ty, &eb.owner_path, "_rd")
                                    ));
                                }
                                w.line("_rd.end();");
                                w.line("break;");
                            });
                        }
                    });
                }
                w.line("return _e;");
            },
        );
        w.blank();
    }
    out.push_str(&w.finish());
}

/// `_{TYPE_NAME}_CODES`: the frozen code-to-class table for one domain.
fn js_error_code_table_name(eb: &ErrorBinding) -> String {
    format!("_{}_CODES", eb.type_name.to_shouty_snake_case())
}

/// Emit one `_check{Domain}(wasm, errPtr)` helper per declaring module:
/// identical to the generic `_checkErr` except the thrown error is built by
/// the domain's factory, so domain codes surface as their typed subclasses
/// with any declared payload fields decoded and attached. The payload is
/// decoded before `weaveffi_error_clear` releases it.
pub(crate) fn emit_js_error_checkers(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|eb| eb.declared_here) else {
            continue;
        };
        let checker = js_error_checker_name(eb);
        let factory = js_error_factory_name(eb);
        w.line(format!(
            "// Throw the `{}` domain error (and free the slot) if the error slot",
            eb.type_name
        ));
        w.line("// carries a non-zero code.");
        w.block(format!("function {checker}(wasm, errPtr) {{"), "}", |w| {
            w.line("const dv = new DataView(wasm.memory.buffer);");
            w.line("const code = dv.getInt32(errPtr, true);");
            w.block("if (code !== 0) {", "}", |w| {
                w.line("const msg = _readCStr(wasm, dv.getUint32(errPtr + 4, true)) || '';");
                w.line(format!(
                    "const _e = {factory}(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));"
                ));
                w.line("wasm.weaveffi_error_clear(errPtr);");
                w.line("wasm.weaveffi_dealloc(errPtr, 16);");
                w.line("throw _e;");
            });
        });
        w.blank();
    }
    out.push_str(&w.finish());
}

/// Emit the text encoder/decoder pair and the C-string helpers: `_cstr`
/// stages a JS string as a NUL-terminated C string, `_readCStr` reads one
/// without freeing, and `_takeCStr` reads then frees a producer-owned one.
pub(crate) fn emit_string_helpers(out: &mut String) {
    out.push_str("const _enc = new TextEncoder();\n");
    out.push_str("const _dec = new TextDecoder();\n\n");
    out.push_str("// Stage a JS string as a NUL-terminated C string in linear memory.\n");
    out.push_str("// Returns [ptr, size] (size includes the NUL); release with _free.\n");
    out.push_str("function _cstr(wasm, str) {\n");
    out.push_str("  const bytes = _enc.encode(str);\n");
    out.push_str("  const size = bytes.length + 1;\n");
    out.push_str("  const ptr = wasm.weaveffi_alloc(size);\n");
    out.push_str("  const mem = new Uint8Array(wasm.memory.buffer, ptr, size);\n");
    out.push_str("  mem.set(bytes);\n");
    out.push_str("  mem[bytes.length] = 0;\n");
    out.push_str("  return [ptr, size];\n");
    out.push_str("}\n\n");
    out.push_str("// Read a NUL-terminated C string (0 => null). Does not free.\n");
    out.push_str("function _readCStr(wasm, ptr) {\n");
    out.push_str("  if (ptr === 0) return null;\n");
    out.push_str("  const mem = new Uint8Array(wasm.memory.buffer);\n");
    out.push_str("  let end = ptr;\n");
    out.push_str("  while (mem[end] !== 0) end++;\n");
    out.push_str("  return _dec.decode(mem.subarray(ptr, end));\n");
    out.push_str("}\n\n");
    out.push_str("// Read then free a producer-owned C string.\n");
    out.push_str("function _takeCStr(wasm, ptr) {\n");
    out.push_str("  const s = _readCStr(wasm, ptr);\n");
    out.push_str("  if (ptr !== 0) wasm.weaveffi_free_string(ptr);\n");
    out.push_str("  return s;\n");
    out.push_str("}\n\n");
}

/// Emit the byte-buffer helpers: `_bytes` stages a byte (or encoded value)
/// buffer into linear memory, `_takeBytes` copies then frees a
/// producer-owned one.
pub(crate) fn emit_bytes_helpers(out: &mut String) {
    out.push_str("// Stage a byte buffer (or an encoded value buffer); returns [ptr, len];\n");
    out.push_str("// release with weaveffi_dealloc(ptr, len).\n");
    out.push_str("function _bytes(wasm, data) {\n");
    out.push_str("  const u8 = data instanceof Uint8Array ? data : new Uint8Array(data);\n");
    out.push_str("  const ptr = wasm.weaveffi_alloc(u8.length);\n");
    out.push_str("  if (u8.length) new Uint8Array(wasm.memory.buffer, ptr, u8.length).set(u8);\n");
    out.push_str("  return [ptr, u8.length];\n");
    out.push_str("}\n\n");
    out.push_str("// Copy then free a producer-owned byte (or value) buffer.\n");
    out.push_str("function _takeBytes(wasm, ptr, len) {\n");
    out.push_str("  if (ptr === 0 || len === 0) return new Uint8Array(0);\n");
    out.push_str("  const copy = new Uint8Array(wasm.memory.buffer, ptr, len).slice();\n");
    out.push_str("  wasm.weaveffi_free_bytes(ptr, len);\n");
    out.push_str("  return copy;\n");
    out.push_str("}\n\n");
}

/// Emit the error-slot helpers: `_allocErr` allocates a zeroed 16-byte error
/// struct, `_checkErr` throws the generic brand error (and frees the slot)
/// for a non-zero code on the trap path, and `_freeErr` releases the slot on
/// the success path.
pub(crate) fn emit_error_slot_helpers(out: &mut String) {
    out.push_str("// Allocate a zeroed 16-byte error slot:\n");
    out.push_str("// { i32 code, char* message, uint8_t* payload_ptr, size_t payload_len }.\n");
    out.push_str("function _allocErr(wasm) {\n");
    out.push_str("  const ptr = wasm.weaveffi_alloc(16);\n");
    out.push_str("  new Uint8Array(wasm.memory.buffer, ptr, 16).fill(0);\n");
    out.push_str("  return ptr;\n");
    out.push_str("}\n\n");
    out.push_str("// Throw (and free the slot) if the error slot carries a non-zero code.\n");
    out.push_str("// Non-throwing wrappers route here: a non-zero code can only be a\n");
    out.push_str("// producer panic or a marshalling failure, surfaced as the generic\n");
    out.push_str(&format!("// {ERROR_BRAND}.\n"));
    out.push_str("function _checkErr(wasm, errPtr) {\n");
    out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
    out.push_str("  const code = dv.getInt32(errPtr, true);\n");
    out.push_str("  if (code !== 0) {\n");
    out.push_str("    const msgPtr = dv.getUint32(errPtr + 4, true);\n");
    out.push_str("    const msg = _readCStr(wasm, msgPtr) || '';\n");
    out.push_str("    wasm.weaveffi_error_clear(errPtr);\n");
    out.push_str("    wasm.weaveffi_dealloc(errPtr, 16);\n");
    out.push_str(&format!("    throw new {ERROR_BRAND}(code, msg);\n"));
    out.push_str("  }\n");
    out.push_str("}\n\n");
    out.push_str("// Release an error slot on the success path.\n");
    out.push_str("function _freeErr(wasm, errPtr) {\n");
    out.push_str("  wasm.weaveffi_dealloc(errPtr, 16);\n");
    out.push_str("}\n\n");
}

/// Emit `_checkErrRef`, the borrowed-error checker the async completion
/// callbacks route through: the producer owns and frees the error struct, so
/// the slot is read but never deallocated here.
pub(crate) fn emit_check_err_ref(out: &mut String) {
    out.push_str("// Throw if a borrowed (producer-owned) error carries a non-zero\n");
    out.push_str("// code. Used by async callbacks: the producer owns and frees the\n");
    out.push_str("// error struct, so the slot is read but never deallocated here.\n");
    out.push_str("// `mkErr` maps domain codes (and decodes payload fields) for\n");
    out.push_str(&format!(
        "// throwing callables; without it the generic {ERROR_BRAND} is thrown.\n"
    ));
    out.push_str("function _checkErrRef(wasm, errPtr, mkErr) {\n");
    out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
    out.push_str("  const code = dv.getInt32(errPtr, true);\n");
    out.push_str("  if (code === 0) return;\n");
    out.push_str("  const msg = _readCStr(wasm, dv.getUint32(errPtr + 4, true)) || '';\n");
    out.push_str(
        "  if (mkErr) throw mkErr(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true));\n",
    );
    out.push_str(&format!("  throw new {ERROR_BRAND}(code, msg);\n"));
    out.push_str("}\n\n");
}

/// Emit the shared `_WeaveFFIIterator` class: a lazy wrapper over a producer
/// iterator handle implementing the JS iterator protocol, destroying the
/// handle exactly once (on exhaustion, on a `next` error, or from
/// `return()`).
pub(crate) fn emit_iterator_class(out: &mut String) {
    out.push_str("// Lazy wrapper over a producer iterator handle, implementing the JS\n");
    out.push_str("// iterator protocol: each next() issues exactly one producer next call\n");
    out.push_str("// and yields one converted element, so iteration streams in constant\n");
    out.push_str("// memory. The handle is destroyed exactly once: eagerly on exhaustion,\n");
    out.push_str("// on a next error, or from return() when iteration stops early (a\n");
    out.push_str("// for...of loop calls return() automatically on break or throw).\n");
    out.push_str("// Abandoning an iterator without exhausting or closing it leaks the\n");
    out.push_str("// producer handle: JS has no finalization hook that is reliable across\n");
    out.push_str("// every target this loader supports.\n");
    out.push_str("class _WeaveFFIIterator {\n");
    out.push_str("  constructor(wasm, handle, slotSize, callNext, destroy, check, decode) {\n");
    out.push_str("    this._wasm = wasm;\n");
    out.push_str("    this._handle = handle;\n");
    out.push_str("    this._slotSize = slotSize;\n");
    out.push_str("    this._callNext = callNext;\n");
    out.push_str("    this._destroyFn = destroy;\n");
    out.push_str("    this._check = check;\n");
    out.push_str("    this._decode = decode;\n");
    out.push_str("    this._slot = wasm.weaveffi_alloc(slotSize);\n");
    out.push_str("  }\n");
    out.push_str("  // Destroy the handle and release the element slot exactly once.\n");
    out.push_str("  _close() {\n");
    out.push_str("    if (this._handle === 0) return;\n");
    out.push_str("    this._destroyFn(this._handle);\n");
    out.push_str("    this._handle = 0;\n");
    out.push_str("    this._wasm.weaveffi_dealloc(this._slot, this._slotSize);\n");
    out.push_str("    this._slot = 0;\n");
    out.push_str("  }\n");
    out.push_str("  next() {\n");
    out.push_str("    if (this._handle === 0) return { done: true, value: undefined };\n");
    out.push_str("    const wasm = this._wasm;\n");
    out.push_str("    const _err = _allocErr(wasm);\n");
    out.push_str("    let _has;\n");
    out.push_str("    try {\n");
    out.push_str("      _has = this._callNext(this._handle, this._slot, _err);\n");
    out.push_str("      // Throws (and releases the slot) on a non-zero code.\n");
    out.push_str("      this._check(wasm, _err);\n");
    out.push_str("    } catch (e) {\n");
    out.push_str("      this._close();\n");
    out.push_str("      throw e;\n");
    out.push_str("    }\n");
    out.push_str("    _freeErr(wasm, _err);\n");
    out.push_str("    if (_has === 0) {\n");
    out.push_str("      this._close();\n");
    out.push_str("      return { done: true, value: undefined };\n");
    out.push_str("    }\n");
    out.push_str("    return { done: false, value: this._decode(wasm, this._slot) };\n");
    out.push_str("  }\n");
    out.push_str("  // Early-exit cleanup; for...of calls this on break/throw.\n");
    out.push_str("  return(value) {\n");
    out.push_str("    this._close();\n");
    out.push_str("    return { done: true, value };\n");
    out.push_str("  }\n");
    out.push_str("  [Symbol.iterator]() {\n");
    out.push_str("    return this;\n");
    out.push_str("  }\n");
    out.push_str("}\n\n");
}

/// Emit `_registerTrampoline`, which grows the wasm function table by one
/// slot and installs a `WebAssembly.Function` wrapping `handler`, returning
/// the new index. Shared by the async completion and listener trampolines.
pub(crate) fn emit_trampoline_helper(out: &mut String) {
    out.push_str("function _registerTrampoline(table, paramTypes, handler) {\n");
    out.push_str("  const idx = table.grow(1);\n");
    out.push_str("  table.set(idx, new WebAssembly.Function(\n");
    out.push_str("    { parameters: paramTypes, results: [] },\n");
    out.push_str("    handler\n");
    out.push_str("  ));\n");
    out.push_str("  return idx;\n");
    out.push_str("}\n\n");
}
