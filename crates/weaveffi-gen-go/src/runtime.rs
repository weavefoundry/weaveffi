//! The emitted Go runtime prelude: bool conversion helpers, the shared
//! error plumbing, the value-buffer reader/writer pair, and the callback
//! registry.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;

/// The `boolToC`/`cToBool` pair converting between Go `bool` and cgo's
/// `C._Bool`.
pub(crate) fn render_bool_helpers(out: &mut String) {
    // cgo models C `_Bool` as a distinct Go type whose underlying kind is
    // bool, so convert with the type itself rather than integer literals.
    out.push_str("func boolToC(b bool) C._Bool {\n");
    out.push_str("\treturn C._Bool(b)\n");
    out.push_str("}\n\n");
    out.push_str("func cToBool(b C._Bool) bool {\n");
    out.push_str("\treturn bool(b)\n");
    out.push_str("}\n\n");
}

/// The shared error plumbing: the generic [`ERROR_BRAND`] struct implementing
/// `error` (unknown codes, marshalling failures), plus the `wvTakeError` slot
/// reader (returning code, message, and a copy of the structured payload
/// buffer), the `wvBrandError` constructor, and the `wvTrap` panic helper
/// non-throwing wrappers check their slot with.
pub(crate) fn render_error_infra(out: &mut String) {
    let mut w = CodeWriter::tabs();
    w.line(format!(
        "// {ERROR_BRAND} reports a failure crossing the C boundary that no typed"
    ));
    w.line("// error domain claims: an unknown code, a marshalling failure, or a");
    w.line("// producer panic.");
    w.block(format!("type {ERROR_BRAND} struct {{"), "}", |w| {
        w.line("// Code is the numeric ABI error code.");
        w.line("Code int32");
        w.line("// Message is the human-readable error message.");
        w.line("Message string");
    });
    w.blank();
    w.block(
        format!("func (e *{ERROR_BRAND}) Error() string {{"),
        "}",
        |w| {
            w.line("return fmt.Sprintf(\"weaveffi: %s (code %d)\", e.Message, e.Code)");
        },
    );
    w.blank();

    w.line("// wvTakeError reads and clears a non-zero C error slot, returning its");
    w.line("// code, message, and a copy of its structured payload buffer (nil when");
    w.line("// the code declares no payload fields).");
    w.block(
        "func wvTakeError(cErr *C.weaveffi_error) (int32, string, []byte) {",
        "}",
        |w| {
            w.line("code := int32(cErr.code)");
            w.line("msg := \"\"");
            w.block("if cErr.message != nil {", "}", |w| {
                w.line("msg = C.GoString(cErr.message)");
            });
            w.line("var payload []byte");
            w.block("if cErr.payload_ptr != nil {", "}", |w| {
                w.line(
                    "payload = C.GoBytes(unsafe.Pointer(cErr.payload_ptr), C.int(cErr.payload_len))",
                );
            });
            w.line("C.weaveffi_error_clear(cErr)");
            w.line("return code, msg, payload");
        },
    );
    w.blank();

    w.block(
        "func wvBrandError(code int32, message string, _ []byte) error {",
        "}",
        |w| {
            w.line(format!(
                "return &{ERROR_BRAND}{{Code: code, Message: message}}"
            ));
        },
    );
    w.blank();

    w.line("// wvTrap panics when the C error slot reports a failure. Non-throwing");
    w.line("// wrappers check their slot with it: a non-zero code there can only be");
    w.line("// a producer panic or a marshalling failure.");
    w.block("func wvTrap(cErr *C.weaveffi_error) {", "}", |w| {
        w.block("if cErr.code != 0 {", "}", |w| {
            w.line("code, msg, _ := wvTakeError(cErr)");
            w.line("panic(fmt.Sprintf(\"weaveffi: %s (code %d)\", msg, code))");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The private writer/reader pair implementing the WeaveFFI value-buffer
/// wire format (little-endian, packed, `u32` length prefixes), plus the two
/// buffer copy helpers (`wvCopyBuffer` for owned returns released with
/// `weaveffi_free_bytes`, `wvBorrowBuffer` for borrowed callback/async
/// buffers the producer frees).
///
/// The reader panics on malformed input: a bad buffer is a producer bug (a
/// contract violation), not a recoverable domain error, so it surfaces
/// through the same panic channel a trapped producer error does.
pub(crate) fn render_buffer_runtime(out: &mut String) {
    let mut w = CodeWriter::tabs();
    w.line("// wvWriter serializes values into the WeaveFFI value-buffer format:");
    w.line("// little-endian, packed, u32 length prefixes.");
    w.block("type wvWriter struct {", "}", |w| {
        w.line("buf []byte");
    });
    w.blank();
    w.block("func (w *wvWriter) writeBool(v bool) {", "}", |w| {
        w.line("if v {");
        w.indent();
        w.line("w.buf = append(w.buf, 1)");
        w.dedent();
        w.line("} else {");
        w.indent();
        w.line("w.buf = append(w.buf, 0)");
        w.dedent();
        w.line("}");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI8(v int8) {", "}", |w| {
        w.line("w.buf = append(w.buf, byte(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU8(v uint8) {", "}", |w| {
        w.line("w.buf = append(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI16(v int16) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint16(w.buf, uint16(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU16(v uint16) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint16(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI32(v int32) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint32(w.buf, uint32(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU32(v uint32) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint32(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeI64(v int64) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint64(w.buf, uint64(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeU64(v uint64) {", "}", |w| {
        w.line("w.buf = binary.LittleEndian.AppendUint64(w.buf, v)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeF32(v float32) {", "}", |w| {
        w.line("w.writeU32(math.Float32bits(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeF64(v float64) {", "}", |w| {
        w.line("w.writeU64(math.Float64bits(v))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeLen(n int) {", "}", |w| {
        w.block("if n < 0 || uint64(n) > uint64(^uint32(0)) {", "}", |w| {
            w.line("panic(\"weaveffi: value-buffer length exceeds u32 range\")");
        });
        w.line("w.writeU32(uint32(n))");
    });
    w.blank();
    w.block("func (w *wvWriter) writeString(v string) {", "}", |w| {
        w.line("w.writeLen(len(v))");
        w.line("w.buf = append(w.buf, v...)");
    });
    w.blank();
    w.block("func (w *wvWriter) writeBytes(v []byte) {", "}", |w| {
        w.line("w.writeLen(len(v))");
        w.line("w.buf = append(w.buf, v...)");
    });
    w.blank();
    w.block(
        "func (w *wvWriter) writeOptionFlag(present bool) {",
        "}",
        |w| {
            w.line("w.writeBool(present)");
        },
    );
    w.blank();

    w.line("// wvReader decodes values from the WeaveFFI value-buffer format. A");
    w.line("// malformed buffer is a producer/consumer contract violation, so every");
    w.line("// read panics (the same channel a trapped producer error uses) instead");
    w.line("// of returning a typed domain error.");
    w.block("type wvReader struct {", "}", |w| {
        w.line("buf []byte");
        w.line("pos int");
    });
    w.blank();
    w.block("func wvMalformed(context string) {", "}", |w| {
        w.line("panic(\"weaveffi: malformed value buffer: \" + context)");
    });
    w.blank();
    w.block(
        "func (r *wvReader) take(n int, context string) []byte {",
        "}",
        |w| {
            w.block("if n < 0 || len(r.buf)-r.pos < n {", "}", |w| {
                w.line("wvMalformed(context)");
            });
            w.line("b := r.buf[r.pos : r.pos+n]");
            w.line("r.pos += n");
            w.line("return b");
        },
    );
    w.blank();
    w.block("func (r *wvReader) readBool() bool {", "}", |w| {
        w.line("switch r.take(1, \"bool\")[0] {");
        w.line("case 0:");
        w.indent();
        w.line("return false");
        w.dedent();
        w.line("case 1:");
        w.indent();
        w.line("return true");
        w.dedent();
        w.line("}");
        w.line("wvMalformed(\"bool byte out of range\")");
        w.line("return false");
    });
    w.blank();
    w.block("func (r *wvReader) readI8() int8 {", "}", |w| {
        w.line("return int8(r.take(1, \"i8\")[0])");
    });
    w.blank();
    w.block("func (r *wvReader) readU8() uint8 {", "}", |w| {
        w.line("return r.take(1, \"u8\")[0]");
    });
    w.blank();
    w.block("func (r *wvReader) readI16() int16 {", "}", |w| {
        w.line("return int16(binary.LittleEndian.Uint16(r.take(2, \"i16\")))");
    });
    w.blank();
    w.block("func (r *wvReader) readU16() uint16 {", "}", |w| {
        w.line("return binary.LittleEndian.Uint16(r.take(2, \"u16\"))");
    });
    w.blank();
    w.block("func (r *wvReader) readI32() int32 {", "}", |w| {
        w.line("return int32(binary.LittleEndian.Uint32(r.take(4, \"i32\")))");
    });
    w.blank();
    w.block("func (r *wvReader) readU32() uint32 {", "}", |w| {
        w.line("return binary.LittleEndian.Uint32(r.take(4, \"u32\"))");
    });
    w.blank();
    w.block("func (r *wvReader) readI64() int64 {", "}", |w| {
        w.line("return int64(binary.LittleEndian.Uint64(r.take(8, \"i64\")))");
    });
    w.blank();
    w.block("func (r *wvReader) readU64() uint64 {", "}", |w| {
        w.line("return binary.LittleEndian.Uint64(r.take(8, \"u64\"))");
    });
    w.blank();
    w.block("func (r *wvReader) readF32() float32 {", "}", |w| {
        w.line("return math.Float32frombits(r.readU32())");
    });
    w.blank();
    w.block("func (r *wvReader) readF64() float64 {", "}", |w| {
        w.line("return math.Float64frombits(r.readU64())");
    });
    w.blank();
    w.block("func (r *wvReader) readLen() int {", "}", |w| {
        w.line("n := int(r.readU32())");
        w.block("if n > len(r.buf)-r.pos {", "}", |w| {
            w.line("wvMalformed(\"length prefix exceeds remaining buffer\")");
        });
        w.line("return n");
    });
    w.blank();
    w.block("func (r *wvReader) readString() string {", "}", |w| {
        w.line("b := r.take(r.readLen(), \"string bytes\")");
        w.block("if !utf8.Valid(b) {", "}", |w| {
            w.line("wvMalformed(\"string is not valid UTF-8\")");
        });
        w.line("return string(b)");
    });
    w.blank();
    w.block("func (r *wvReader) readBytes() []byte {", "}", |w| {
        w.line("b := r.take(r.readLen(), \"byte buffer\")");
        w.line("out := make([]byte, len(b))");
        w.line("copy(out, b)");
        w.line("return out");
    });
    w.blank();
    w.block("func (r *wvReader) readOptionFlag() bool {", "}", |w| {
        w.line("switch r.take(1, \"option flag\")[0] {");
        w.line("case 0:");
        w.indent();
        w.line("return false");
        w.dedent();
        w.line("case 1:");
        w.indent();
        w.line("return true");
        w.dedent();
        w.line("}");
        w.line("wvMalformed(\"option flag byte out of range\")");
        w.line("return false");
    });
    w.blank();
    w.block("func (r *wvReader) expectEnd() {", "}", |w| {
        w.block("if r.pos != len(r.buf) {", "}", |w| {
            w.line("wvMalformed(\"trailing bytes after value\")");
        });
    });
    w.blank();

    w.line("// wvCopyBuffer copies an owned, producer-allocated value buffer into Go");
    w.line("// memory and releases it with weaveffi_free_bytes.");
    w.block(
        "func wvCopyBuffer(ptr *C.uint8_t, length C.size_t) []byte {",
        "}",
        |w| {
            w.block("if ptr == nil {", "}", |w| {
                w.line("return nil");
            });
            w.line("out := C.GoBytes(unsafe.Pointer(ptr), C.int(length))");
            w.line("C.weaveffi_free_bytes(ptr, length)");
            w.line("return out");
        },
    );
    w.blank();
    w.line("// wvBorrowBuffer copies a borrowed value buffer into Go memory. The");
    w.line("// producer keeps ownership and frees it after the borrowing call returns.");
    w.block(
        "func wvBorrowBuffer(ptr *C.uint8_t, length C.size_t) []byte {",
        "}",
        |w| {
            w.block("if ptr == nil {", "}", |w| {
                w.line("return nil");
            });
            w.line("return C.GoBytes(unsafe.Pointer(ptr), C.int(length))");
        },
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The registry mapping opaque context ids to Go callbacks/channels. Only the
/// integer id (never a Go pointer) crosses the C boundary as `void*`, so the
/// GC stays unaware of C-held references and trampolines recover the Go value
/// from the map.
pub(crate) fn render_callback_registry(out: &mut String, has_listeners: bool) {
    let mut w = CodeWriter::tabs();
    w.block("var (", ")", |w| {
        w.line("wvCallbackMu  sync.Mutex");
        w.line("wvCallbackSeq uint64");
        w.line("wvCallbacks   = map[uint64]interface{}{}");
        if has_listeners {
            w.line("// Subscription id -> registry id, so unregister can release the Go callback.");
            w.line("wvListenerCtx = map[uint64]uint64{}");
        }
    });
    w.blank();

    w.block("func wvCallbackStore(v interface{}) uint64 {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("wvCallbackSeq++");
        w.line("wvCallbacks[wvCallbackSeq] = v");
        w.line("return wvCallbackSeq");
    });
    w.blank();

    w.block("func wvCallbackLoad(id uint64) interface{} {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("return wvCallbacks[id]");
    });
    w.blank();

    w.block("func wvCallbackTake(id uint64) interface{} {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("v := wvCallbacks[id]");
        w.line("delete(wvCallbacks, id)");
        w.line("return v");
    });
    w.blank();

    w.block("func wvCallbackDelete(id uint64) {", "}", |w| {
        w.line("wvCallbackMu.Lock()");
        w.line("defer wvCallbackMu.Unlock()");
        w.line("delete(wvCallbacks, id)");
    });
    w.blank();
    out.push_str(&w.finish());
}
