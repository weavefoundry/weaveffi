package {{PACKAGE}}

import (
	"encoding/binary"
	"math"
	"unicode/utf8"
)

// wvWriter serializes values into the value-buffer format: little-endian,
// packed, u32 length prefixes.
type wvWriter struct {
	buf []byte
}

func (w *wvWriter) writeBool(v bool) {
	if v {
		w.buf = append(w.buf, 1)
	} else {
		w.buf = append(w.buf, 0)
	}
}

func (w *wvWriter) writeI8(v int8) {
	w.buf = append(w.buf, byte(v))
}

func (w *wvWriter) writeU8(v uint8) {
	w.buf = append(w.buf, v)
}

func (w *wvWriter) writeI16(v int16) {
	w.buf = binary.LittleEndian.AppendUint16(w.buf, uint16(v))
}

func (w *wvWriter) writeU16(v uint16) {
	w.buf = binary.LittleEndian.AppendUint16(w.buf, v)
}

func (w *wvWriter) writeI32(v int32) {
	w.buf = binary.LittleEndian.AppendUint32(w.buf, uint32(v))
}

func (w *wvWriter) writeU32(v uint32) {
	w.buf = binary.LittleEndian.AppendUint32(w.buf, v)
}

func (w *wvWriter) writeI64(v int64) {
	w.buf = binary.LittleEndian.AppendUint64(w.buf, uint64(v))
}

func (w *wvWriter) writeU64(v uint64) {
	w.buf = binary.LittleEndian.AppendUint64(w.buf, v)
}

func (w *wvWriter) writeF32(v float32) {
	w.writeU32(math.Float32bits(v))
}

func (w *wvWriter) writeF64(v float64) {
	w.writeU64(math.Float64bits(v))
}

func (w *wvWriter) writeLen(n int) {
	if n < 0 || uint64(n) > math.MaxUint32 {
		panic("{{PACKAGE}}: value-buffer length exceeds u32 range")
	}
	w.writeU32(uint32(n))
}

func (w *wvWriter) writeString(v string) {
	w.writeLen(len(v))
	w.buf = append(w.buf, v...)
}

func (w *wvWriter) writeBytes(v []byte) {
	w.writeLen(len(v))
	w.buf = append(w.buf, v...)
}

func (w *wvWriter) writeOptionFlag(present bool) {
	w.writeBool(present)
}

// wvReader decodes values from the value-buffer format. A malformed buffer
// is a producer/consumer contract violation, so every read panics instead of
// returning a typed domain error.
type wvReader struct {
	buf []byte
	pos int
}

func wvMalformed(context string) {
	panic("{{PACKAGE}}: malformed value buffer: " + context)
}

func (r *wvReader) take(n int, context string) []byte {
	if n < 0 || len(r.buf)-r.pos < n {
		wvMalformed(context)
	}
	b := r.buf[r.pos : r.pos+n]
	r.pos += n
	return b
}

func (r *wvReader) readBool() bool {
	switch r.take(1, "bool")[0] {
	case 0:
		return false
	case 1:
		return true
	}
	wvMalformed("bool byte out of range")
	return false
}

func (r *wvReader) readI8() int8 {
	return int8(r.take(1, "i8")[0])
}

func (r *wvReader) readU8() uint8 {
	return r.take(1, "u8")[0]
}

func (r *wvReader) readI16() int16 {
	return int16(binary.LittleEndian.Uint16(r.take(2, "i16")))
}

func (r *wvReader) readU16() uint16 {
	return binary.LittleEndian.Uint16(r.take(2, "u16"))
}

func (r *wvReader) readI32() int32 {
	return int32(binary.LittleEndian.Uint32(r.take(4, "i32")))
}

func (r *wvReader) readU32() uint32 {
	return binary.LittleEndian.Uint32(r.take(4, "u32"))
}

func (r *wvReader) readI64() int64 {
	return int64(binary.LittleEndian.Uint64(r.take(8, "i64")))
}

func (r *wvReader) readU64() uint64 {
	return binary.LittleEndian.Uint64(r.take(8, "u64"))
}

func (r *wvReader) readF32() float32 {
	return math.Float32frombits(r.readU32())
}

func (r *wvReader) readF64() float64 {
	return math.Float64frombits(r.readU64())
}

// readLen reads a u32 element count. Elements may encode to zero bytes, so
// the count isn't checked against the remaining buffer; callers cap their
// preallocation with capHint instead.
func (r *wvReader) readLen() int {
	return int(r.readU32())
}

// capHint bounds a preallocation for n decoded elements by the bytes left in
// the buffer, so a corrupt count can't force a huge allocation.
func (r *wvReader) capHint(n int) int {
	return min(n, len(r.buf)-r.pos)
}

func (r *wvReader) readString() string {
	b := r.take(r.readLen(), "string bytes")
	if !utf8.Valid(b) {
		wvMalformed("string is not valid UTF-8")
	}
	return string(b)
}

func (r *wvReader) readBytes() []byte {
	return append([]byte{}, r.take(r.readLen(), "byte buffer")...)
}

func (r *wvReader) readOptionFlag() bool {
	switch r.take(1, "option flag")[0] {
	case 0:
		return false
	case 1:
		return true
	}
	wvMalformed("option flag byte out of range")
	return false
}

func (r *wvReader) expectEnd() {
	if r.pos != len(r.buf) {
		wvMalformed("trailing bytes after value")
	}
}
