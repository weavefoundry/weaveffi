//! The fixed JS runtime the loader embeds: string, byte, and error-slot
//! helpers, the error class hierarchy with its per-domain factories and
//! checkers, the object-wrapper lifecycle helpers, the lazy iterator wrapper,
//! the trampoline registrar, and the foreign-error reporter callback
//! trampolines use.
//!
//! Everything here is emitted conditionally by the loader assembler in
//! [`crate::entities`], keyed off which features the API actually uses. The
//! error classes are exported at module scope; the per-domain factories and
//! checkers decode error payloads through the value-buffer codecs, which live
//! inside the loader (they reference the loader-scoped interface classes for
//! object tokens), so they're emitted there too.

use heck::ToShoutySnakeCase;
use weaveffi_core::cabi::ABI_VERSION;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;
use weaveffi_core::model::{BindingModel, ErrorBinding};

use crate::codec::buf_read_expr;
use crate::types::{
    js_code_class_name, js_error_checker_name, js_error_factory_name, js_str_literal,
};

/// Emit the module-scope error classes: the generic `WeaveFFIError` base
/// (unknown codes, marshalling failures, panics, foreign callback failures),
/// then one domain class per declaring module (`class KvError extends
/// WeaveFFIError`) with one subclass per code carrying its stable `CODE` and
/// default message. Each code class is also aliased onto its domain class
/// (`KvError.KeyNotFound`).
///
/// Domain codes are validated positive-only, so the reserved runtime codes
/// (`-1` generic, `-2` panic, `-3` marshalling, `-4` a callback-interface
/// implementation raised) always fall through to the generic brand error in
/// the factories emitted by [`emit_js_error_factories`].
pub(crate) fn emit_js_error_classes(out: &mut String, model: &BindingModel) {
    let mut w = CodeWriter::two_space();
    w.line("/** Base error for WeaveFFI failures: domain errors extend it, and it is");
    w.line(" * thrown directly for unknown codes, marshalling failures, producer");
    w.line(" * panics, and callback-interface implementations that raised (code -4).");
    w.line(" * Carries the stable ABI `code`. */");
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
        w.block(format!("const {table} = Object.freeze({{"), "});", |w| {
            for c in &eb.codes {
                w.line(format!("{}: {},", c.value, js_code_class_name(&c.name)));
            }
        });
        w.blank();
    }
    out.push_str(&w.finish());
}

/// `_{TYPE_NAME}_CODES`: the frozen code-to-class table for one domain.
fn js_error_code_table_name(eb: &ErrorBinding) -> String {
    format!("_{}_CODES", eb.type_name.to_shouty_snake_case())
}

/// Emit one `_{domain}From(wasm, code, message, payloadPtr, payloadLen)`
/// factory per declaring module at `indent` (loader scope): it builds the
/// matching code subclass (or the generic brand error for codes outside the
/// domain) and decodes any declared payload fields from the error's value
/// buffer into properties on the thrown error.
pub(crate) fn emit_js_error_factories(out: &mut String, model: &BindingModel, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
    for m in &model.modules {
        let Some(eb) = m.error.as_ref().filter(|eb| eb.declared_here) else {
            continue;
        };
        let domain = &eb.type_name;
        let table = js_error_code_table_name(eb);
        let factory = js_error_factory_name(eb);
        let has_payload = eb.codes.iter().any(|c| !c.fields.is_empty());
        w.line(format!(
            "// Build the {domain} subclass matching `code`, or a generic"
        ));
        w.line(format!(
            "// {ERROR_BRAND} for codes outside the domain (panics, marshalling,"
        ));
        w.line("// foreign callback failures).");
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

/// Emit one `_check{Domain}(wasm, errPtr)` helper per declaring module at
/// `indent` (loader scope): identical to the generic `_checkErr` except the
/// thrown error is built by the domain's factory, so domain codes surface as
/// their typed subclasses with any declared payload fields decoded and
/// attached. The payload is decoded before `weaveffi_error_clear` releases
/// it.
pub(crate) fn emit_js_error_checkers(out: &mut String, model: &BindingModel, indent: &str) {
    let mut w = CodeWriter::two_space().with_depth(indent.len() / 2);
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
            w.line("_pendingForeign = null;");
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
    out.push_str("// producer panic (-2), a marshalling failure (-3), or a callback\n");
    out.push_str("// interface implementation that raised (-4), surfaced as the generic\n");
    out.push_str(&format!("// {ERROR_BRAND}.\n"));
    out.push_str("function _checkErr(wasm, errPtr) {\n");
    out.push_str("  _pendingForeign = null;\n");
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
    out.push_str("  _pendingForeign = null;\n");
    out.push_str("  wasm.weaveffi_dealloc(errPtr, 16);\n");
    out.push_str("}\n\n");
    emit_trap_helpers(out);
}

/// Emit `_pendingForeign`, `_trapError`, and `_trap`: the trap translation
/// every producer call routes its exceptions through.
///
/// `wasm32-unknown-unknown` has no unwinding runtime, so a producer panic
/// can't be caught and reported through `out_err` the way native targets do:
/// it executes `unreachable` and the engine throws a `WebAssembly.RuntimeError`
/// out of the call. The glue restores the ABI's contract on the JS side: a
/// trap that follows a callback-interface failure recorded in
/// `_pendingForeign` becomes the `-4` brand error with the consumer's message
/// (the runtime normally reports that failure through `out_err` without
/// trapping; this is the fallback), and any other trap is reported as a
/// producer panic (`-2`). Exceptions that aren't traps (the `-3` from
/// `_borrow`, for example) pass through unchanged.
fn emit_trap_helpers(out: &mut String) {
    let mut w = CodeWriter::two_space();
    w.line("// The message of a callback-interface failure a trampoline reported through");
    w.line("// out_err, held until the producer call on the stack returns (every error");
    w.line("// checker and _freeErr clear it) so later callbacks in that call are refused.");
    w.line("let _pendingForeign = null;");
    w.blank();
    w.line("// Translate an exception raised while a producer call was on the stack.");
    w.line("// wasm32-unknown-unknown has no unwinding runtime: a producer panic executes");
    w.line("// `unreachable`, so the engine throws a WebAssembly.RuntimeError instead of");
    w.line("// the producer writing out_err. A trap that follows a callback-interface");
    w.line("// failure is labelled with that failure (-4); any other trap is a producer");
    w.line("// panic (-2). Non-trap exceptions pass through unchanged. The producer");
    w.line("// frames between the trap and the call are not unwound, so a lock held");
    w.line("// across a callback stays locked; a producer that snapshots its state");
    w.line("// before calling out stays usable.");
    w.block("function _trapError(e) {", "}", |w| {
        w.line("const foreign = _pendingForeign;");
        w.line("_pendingForeign = null;");
        w.line("if (!(e instanceof WebAssembly.RuntimeError)) return e;");
        w.line(format!(
            "if (foreign !== null) return new {ERROR_BRAND}(-4, foreign);"
        ));
        w.line(format!(
            "return new {ERROR_BRAND}(-2, 'producer panicked: ' + e.message);"
        ));
    });
    w.blank();
    w.line("// Release the error slot a failed call never filled, then translate.");
    w.block("function _trap(wasm, errPtr, e) {", "}", |w| {
        w.line("_freeErr(wasm, errPtr);");
        w.line("return _trapError(e);");
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit `_checkAbiVersion`, which the loader calls on the bound export table
/// before returning the API object. A module that does not export
/// `weaveffi_abi_version` predates versioning; one reporting a different
/// revision was built against an incompatible runtime. Both throw so the
/// mismatch surfaces at load time rather than as a garbled value buffer.
pub(crate) fn emit_abi_version_check(out: &mut String) {
    out.push_str("// The ABI revision this glue was generated against.\n");
    out.push_str(&format!("const _ABI_VERSION = {ABI_VERSION};\n\n"));
    out.push_str("function _checkAbiVersion(wasm) {\n");
    out.push_str("  if (typeof wasm.weaveffi_abi_version !== 'function') {\n");
    out.push_str("    throw new Error(`the loaded WeaveFFI module predates ABI versioning (this glue expects ABI revision ${_ABI_VERSION})`);\n");
    out.push_str("  }\n");
    out.push_str("  const found = wasm.weaveffi_abi_version() >>> 0;\n");
    out.push_str("  if (found !== _ABI_VERSION) {\n");
    out.push_str("    throw new Error(`WeaveFFI ABI mismatch: this glue expects revision ${_ABI_VERSION} but the loaded module reports revision ${found}`);\n");
    out.push_str("  }\n");
    out.push_str("}\n\n");
}

/// Emit `_checkErrRef`, the boxed-error checker the async completion
/// callbacks route through: the consumer owns the heap-boxed error struct,
/// so its fields are read (and any typed error built) before the box is
/// released with `weaveffi_error_free`.
pub(crate) fn emit_check_err_ref(out: &mut String) {
    out.push_str("// Throw if a heap-boxed (consumer-owned) error carries a non-zero\n");
    out.push_str("// code. Used by async callbacks: the fields are read, then the box\n");
    out.push_str("// is released with weaveffi_error_free before throwing.\n");
    out.push_str("// `mkErr` maps domain codes (and decodes payload fields) for\n");
    out.push_str(&format!(
        "// throwing callables; without it the generic {ERROR_BRAND} is thrown.\n"
    ));
    out.push_str("function _checkErrRef(wasm, errPtr, mkErr) {\n");
    out.push_str("  _pendingForeign = null;\n");
    out.push_str("  const dv = new DataView(wasm.memory.buffer);\n");
    out.push_str("  const code = dv.getInt32(errPtr, true);\n");
    out.push_str("  if (code === 0) { wasm.weaveffi_error_free(errPtr); return; }\n");
    out.push_str("  const msg = _readCStr(wasm, dv.getUint32(errPtr + 4, true)) || '';\n");
    out.push_str(&format!(
        "  const err = mkErr ? mkErr(wasm, code, msg, dv.getUint32(errPtr + 8, true), dv.getUint32(errPtr + 12, true)) : new {ERROR_BRAND}(code, msg);\n"
    ));
    out.push_str("  wasm.weaveffi_error_free(errPtr);\n");
    out.push_str("  throw err;\n");
    out.push_str("}\n\n");
}

/// Emit the object-wrapper lifecycle helpers shared by every interface class:
/// `_dispose` (the `Symbol.dispose` well-known symbol, or its registered
/// stand-in on runtimes that predate explicit resource management), the
/// `FinalizationRegistry` backstop, `_adopt` (bind one strong reference to a
/// wrapper and arm the backstop), `_release` (destroy exactly once, disarming
/// the backstop), and `_borrow` (the live pointer of a wrapper passed as an
/// argument, rejecting a closed one).
pub(crate) fn emit_object_helpers(out: &mut String) {
    let mut w = CodeWriter::two_space();
    w.line("// `using` declarations (explicit resource management) call this symbol;");
    w.line("// runtimes that predate it get the registered stand-in so the method is");
    w.line("// still reachable by name.");
    w.line("const _dispose = typeof Symbol.dispose === 'symbol' ? Symbol.dispose : Symbol.for('Symbol.dispose');");
    w.blank();
    w.line("// Finalization backstop: a wrapper that is garbage collected without");
    w.line("// close() still releases its reference. The held value is the");
    w.line("// [destroy, handle] pair (never the wrapper, which would keep it alive);");
    w.line("// the wrapper is the unregister token so close() disarms the backstop and");
    w.line("// destroy runs exactly once either way. Absent on runtimes without");
    w.line("// FinalizationRegistry, where close() is the only release path.");
    w.line("const _finalizer = typeof FinalizationRegistry === 'function'");
    w.line("  ? new FinalizationRegistry(([destroy, handle]) => destroy(handle))");
    w.line("  : null;");
    w.blank();
    w.line("// Bind one strong reference to a wrapper and arm the backstop.");
    w.block("function _adopt(obj, handle, destroy) {", "}", |w| {
        w.line("obj._handle = handle;");
        w.line("obj._destroy = destroy;");
        w.line("if (_finalizer !== null) _finalizer.register(obj, [destroy, handle], obj);");
        w.line("return obj;");
    });
    w.blank();
    w.line("// Release a wrapper's reference exactly once; later calls are no-ops.");
    w.block("function _release(obj) {", "}", |w| {
        w.line("if (!obj._handle) return;");
        w.line("if (_finalizer !== null) _finalizer.unregister(obj);");
        w.line("obj._destroy(obj._handle);");
        w.line("obj._handle = 0;");
    });
    w.blank();
    w.line("// The pointer of a live wrapper, lent to the producer for one call (the");
    w.line("// wrapper keeps its own reference). A closed or non-object argument is a");
    w.line("// consumer programming error, reported with the marshalling code.");
    w.block("function _borrow(obj) {", "}", |w| {
        w.block(
            "if (obj === null || obj === undefined || !obj._handle) {",
            "}",
            |w| {
                w.line(format!(
                    "throw new {ERROR_BRAND}(-3, 'expected a live object wrapper');"
                ));
            },
        );
        w.line("return obj._handle;");
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Emit `_reportForeign` and `_setForeignError`, which callback-interface
/// trampolines call when the consumer's implementation throws: the exception
/// message is staged in linear memory only for the duration of the
/// `weaveffi_error_set` call (the producer copies it with its own allocator),
/// then released, and the producer sees `FOREIGN_ERROR_CODE` (`-4`).
///
/// `wasm32-unknown-unknown` builds with `panic = "abort"`, so the producer
/// can't unwind out of the failed call the way native targets do; the
/// runtime records the failure instead and the producer's code runs to
/// completion on the trampoline's default return value before the thunk
/// reports `-4`. The message is parked in `_pendingForeign` for two reasons:
/// so that every further callback invocation during the same producer call
/// is refused with the same error rather than reaching the consumer's
/// implementation again (matching what unwinding would have done), and so
/// that `_trapError` can still label a trap, should one follow.
pub(crate) fn emit_foreign_error_helper(out: &mut String) {
    let mut w = CodeWriter::two_space();
    w.line("// Report a callback-interface failure through the trampoline's out_err");
    w.line("// slot as FOREIGN_ERROR_CODE (-4). weaveffi_error_set copies the message");
    w.line("// with the producer's allocator, so the staged C string is released as");
    w.line("// soon as it returns. The message is parked in _pendingForeign until the");
    w.line("// producer call on the stack returns: the producer can't unwind on wasm32,");
    w.line("// so it keeps running on the trampoline's default return value, and any");
    w.line("// further callback it makes during that call is refused with this error");
    w.line("// instead of reaching the implementation again.");
    w.block("function _reportForeign(wasm, errPtr, msg) {", "}", |w| {
        w.line("if (_pendingForeign === null) _pendingForeign = msg;");
        w.line("const [p, s] = _cstr(wasm, msg);");
        w.line("wasm.weaveffi_error_set(errPtr, -4, p);");
        w.line("wasm.weaveffi_dealloc(p, s);");
    });
    w.blank();
    w.line("// Report an exception thrown by a callback-interface implementation.");
    w.block("function _setForeignError(wasm, errPtr, e) {", "}", |w| {
        w.line(
            "_reportForeign(wasm, errPtr, e instanceof Error ? (e.message || e.name) : String(e));",
        );
    });
    w.blank();
    out.push_str(&w.finish());
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
    out.push_str("    } catch (e) {\n");
    out.push_str("      // A trap: the slot was never filled, so release it here.\n");
    out.push_str("      this._close();\n");
    out.push_str("      throw _trap(wasm, _err, e);\n");
    out.push_str("    }\n");
    out.push_str("    try {\n");
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
/// slot and installs a `WebAssembly.Function` wrapping `handler` with the
/// given wasm parameter and result types, returning the new index. Shared by
/// the async completion trampolines and the callback-interface vtable
/// entries.
pub(crate) fn emit_trampoline_helper(out: &mut String) {
    out.push_str("function _registerTrampoline(table, paramTypes, resultTypes, handler) {\n");
    out.push_str("  const idx = table.grow(1);\n");
    out.push_str("  table.set(idx, new WebAssembly.Function(\n");
    out.push_str("    { parameters: paramTypes, results: resultTypes },\n");
    out.push_str("    handler\n");
    out.push_str("  ));\n");
    out.push_str("  return idx;\n");
    out.push_str("}\n\n");
}
