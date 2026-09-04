//! The emitted JS runtime prelude: addon loading, the generic error brand
//! and invoke helpers, the object borrow helper, the private value-buffer
//! reader/writer, and the shared lazy iterator class.
//!
//! Everything here is fixed text (or near-fixed text) embedded at the top of
//! `index.js`; the per-entity wrappers in [`crate::entities`] call into it.

use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors::ERROR_BRAND;

/// The private buffer writer/reader runtime embedded in `index.js` whenever
/// the model uses value buffers. Little-endian, packed, no alignment; decode
/// failures throw the generic error brand (a malformed buffer is a contract
/// violation, not a typed domain error). The 64-bit methods take and return
/// `bigint`s (the writers also accept numbers and decimal strings, which is
/// what map keys arrive as).
pub(crate) const BUFFER_RUNTIME_JS: &str = r#"// --- Private value-buffer runtime (WeaveFFI wire format) --------------------
// Little-endian, packed, no alignment. Decoders reject truncated buffers,
// invalid bool/flag bytes, hostile length prefixes, and trailing bytes.
const __utf8 = new TextDecoder('utf-8', { fatal: true });
function __bufferError(what) {
  return new WeaveFFIError(-2, 'malformed value buffer: ' + what);
}
class __Writer {
  constructor() {
    this._buf = Buffer.alloc(256);
    this._len = 0;
  }
  _reserve(n) {
    if (this._len + n <= this._buf.length) return;
    let cap = this._buf.length;
    while (cap < this._len + n) cap *= 2;
    const grown = Buffer.alloc(cap);
    this._buf.copy(grown, 0, 0, this._len);
    this._buf = grown;
  }
  bool(v) { this._reserve(1); this._buf[this._len++] = v ? 1 : 0; }
  i8(v) { this._reserve(1); this._buf.writeInt8(v, this._len); this._len += 1; }
  u8(v) { this._reserve(1); this._buf.writeUInt8(v, this._len); this._len += 1; }
  i16(v) { this._reserve(2); this._buf.writeInt16LE(v, this._len); this._len += 2; }
  u16(v) { this._reserve(2); this._buf.writeUInt16LE(v, this._len); this._len += 2; }
  i32(v) { this._reserve(4); this._buf.writeInt32LE(v, this._len); this._len += 4; }
  u32(v) { this._reserve(4); this._buf.writeUInt32LE(v, this._len); this._len += 4; }
  i64(v) { this._reserve(8); this._buf.writeBigInt64LE(BigInt(v), this._len); this._len += 8; }
  u64(v) { this._reserve(8); this._buf.writeBigUInt64LE(BigInt(v), this._len); this._len += 8; }
  f32(v) { this._reserve(4); this._buf.writeFloatLE(v, this._len); this._len += 4; }
  f64(v) { this._reserve(8); this._buf.writeDoubleLE(v, this._len); this._len += 8; }
  str(v) {
    const b = Buffer.from(String(v), 'utf8');
    this.u32(b.length);
    this._reserve(b.length);
    b.copy(this._buf, this._len);
    this._len += b.length;
  }
  bytes(v) {
    this.u32(v.length);
    this._reserve(v.length);
    this._buf.set(v, this._len);
    this._len += v.length;
  }
  finish() { return this._buf.subarray(0, this._len); }
}
class __Reader {
  constructor(buf) {
    this._buf = Buffer.isBuffer(buf) ? buf : Buffer.from(buf);
    this._pos = 0;
  }
  _take(n, what) {
    if (this._pos + n > this._buf.length) throw __bufferError(what);
    const at = this._pos;
    this._pos += n;
    return at;
  }
  bool() {
    const b = this._buf[this._take(1, 'bool')];
    if (b > 1) throw __bufferError('bool byte out of range');
    return b === 1;
  }
  i8() { return this._buf.readInt8(this._take(1, 'i8')); }
  u8() { return this._buf.readUInt8(this._take(1, 'u8')); }
  i16() { return this._buf.readInt16LE(this._take(2, 'i16')); }
  u16() { return this._buf.readUInt16LE(this._take(2, 'u16')); }
  i32() { return this._buf.readInt32LE(this._take(4, 'i32')); }
  u32() { return this._buf.readUInt32LE(this._take(4, 'u32')); }
  i64() { return this._buf.readBigInt64LE(this._take(8, 'i64')); }
  u64() { return this._buf.readBigUInt64LE(this._take(8, 'u64')); }
  f32() { return this._buf.readFloatLE(this._take(4, 'f32')); }
  f64() { return this._buf.readDoubleLE(this._take(8, 'f64')); }
  len() {
    const n = this.u32();
    if (n > this._buf.length - this._pos) throw __bufferError('length prefix exceeds remaining bytes');
    return n;
  }
  str() {
    const n = this.len();
    const at = this._take(n, 'string bytes');
    try {
      return __utf8.decode(this._buf.subarray(at, at + n));
    } catch (e) {
      throw __bufferError('string is not valid UTF-8');
    }
  }
  bytes() {
    const n = this.len();
    const at = this._take(n, 'byte buffer');
    return Buffer.from(this._buf.subarray(at, at + n));
  }
  end() {
    if (this._pos !== this._buf.length) throw __bufferError('trailing bytes after value');
  }
}
function __encode(f, v) { const w = new __Writer(); f(w, v); return w.finish(); }
function __decode(f, b) { const r = new __Reader(b); const v = f(r); r.end(); return v; }
function __wOpt(w, v, f) { if (v === null || v === undefined) { w.bool(false); } else { w.bool(true); f(w, v); } }
function __rOpt(r, f) { return r.bool() ? f(r) : null; }
function __wList(w, v, f) { w.u32(v.length); for (const e of v) f(w, e); }
function __rList(r, f) {
  const n = r.len();
  const out = [];
  for (let i = 0; i < n; i++) out.push(f(r));
  return out;
}
function __wMap(w, v, kf, vf) {
  const keys = Object.keys(v);
  w.u32(keys.length);
  for (const k of keys) { kf(w, k); vf(w, v[k]); }
}
function __rMap(r, kf, vf) {
  const n = r.len();
  const out = {};
  for (let i = 0; i < n; i++) {
    const k = kf(r);
    out[k] = vf(r);
  }
  return out;
}

"#;

/// Emit the addon-loading preamble of `index.js`: resolve the built `.node`
/// file (env override, node-gyp output, prebuilt fallback) and re-export
/// every native binding onto the wrapper namespace `wv`.
pub(crate) fn render_loader_js(out: &mut String) {
    out.push_str(
        "// The WEAVEFFI_ADDON environment variable overrides the addon location\n\
         // (an absolute path to the built .node file); otherwise prefer the\n\
         // default node-gyp output path and fall back to a prebuilt index.node\n\
         // placed next to this file.\n\
         let addon;\n\
         if (process.env.WEAVEFFI_ADDON) {\n  addon = require(process.env.WEAVEFFI_ADDON);\n} else {\n  try {\n    addon = require('./build/Release/weaveffi.node');\n  } catch (e) {\n    addon = require('./index.node');\n  }\n}\n",
    );

    // The native bindings are defined as non-enumerable properties, so copy
    // them by explicit own-name lookup before layering the idiomatic wrappers.
    out.push_str(
        "\n// Re-export every native binding, then layer the idiomatic wrappers\n\
         // (error classes, interface classes, buffer pack/unpack, function\n\
         // wrappers) on top.\n\
         const wv = {};\n\
         for (const _name of Object.getOwnPropertyNames(addon)) {\n  wv[_name] = addon[_name];\n}\n\n",
    );
}

/// Emit the generic error brand and the shared invoke helpers. Every wrapper
/// funnels addon failures (JS errors carrying the numeric ABI `code` and, for
/// structured errors, the raw `payload` buffer) through a mapping factory:
/// the module domain's for throwing callables, the generic constructor
/// otherwise.
pub(crate) fn render_error_brand_js(out: &mut String) {
    out.push_str(&format!(
        "class {ERROR_BRAND} extends Error {{\n  \
           constructor(code, message) {{\n    \
             super('(' + code + ') ' + (message || ''));\n    \
             this.name = '{ERROR_BRAND}';\n    \
             this.code = code;\n    \
             this.errorMessage = message || '';\n  \
           }}\n\
         }}\n\
         wv.{ERROR_BRAND} = {ERROR_BRAND};\n\
         function __generic(code, message) {{\n  \
           return new {ERROR_BRAND}(code, message);\n\
         }}\n\
         function __rebrand(e, map) {{\n  \
           return e && typeof e.code === 'number' ? map(e.code, e.message, e.payload) : e;\n\
         }}\n\
         function __invoke(fn, args, map) {{\n  \
           try {{\n    \
             return fn.apply(null, args);\n  \
           }} catch (e) {{\n    \
             throw __rebrand(e, map);\n  \
           }}\n\
         }}\n\
         function __invokeAsync(fn, args, map) {{\n  \
           return fn.apply(null, args).catch((e) => {{\n    \
             throw __rebrand(e, map);\n  \
           }});\n\
         }}\n\n"
    ));
}

/// Emit the shared object helper every interface wrapper and every
/// object-typed argument routes through. `__borrow` unwraps a live wrapper
/// to the handle the callee borrows for the call (the wrapper keeps its own
/// reference); a closed wrapper or a foreign value is a programming error
/// reported through the generic brand with the marshalling code.
pub(crate) fn render_object_helpers_js(out: &mut String) {
    out.push_str(&format!(
        "// Borrow a live object wrapper's native handle for one call. The wrapper\n\
         // keeps its own reference; the producer clones if it retains the object.\n\
         function __borrow(o, cls) {{\n  \
           if (!(o instanceof cls)) {{\n    \
             throw new {ERROR_BRAND}(-3, 'expected an instance of ' + cls.name);\n  \
           }}\n  \
           if (!o._handle) {{\n    \
             throw new {ERROR_BRAND}(-3, cls.name + ' used after close()');\n  \
           }}\n  \
           return o._handle;\n\
         }}\n\n"
    ));
}

/// Emit the shared lazy iterator class the JS loader hands out for every
/// `iter<T>` callable. It implements the iterator protocol over the addon's
/// per-iterator `next`/`destroy` entry points: one native pull per `next()`,
/// eager release on exhaustion (the addon destroys the handle when the
/// producer reports done), and `return()` releases the handle on early exit
/// so `for...of` breaks clean up deterministically. Abandoned iterators are
/// reclaimed by the external's native finalizer.
pub(crate) fn render_iterator_class_js(out: &mut String) {
    let mut w = CodeWriter::two_space();
    w.raw(
        "// Lazy iterator over a native producer: one native `next` per step.\n\
         // The native handle is released on exhaustion, by `return()` on early\n\
         // exit, or by the external's finalizer if the iterator is abandoned.\n",
    );
    w.block("class WeaveFFIIterator {", "}", |w| {
        w.block(
            "constructor(ext, nextFn, destroyFn, map, wrapElem) {",
            "}",
            |w| {
                w.line("this._ext = ext;");
                w.line("this._nextFn = nextFn;");
                w.line("this._destroyFn = destroyFn;");
                w.line("this._map = map;");
                w.line("this._wrapElem = wrapElem;");
                w.line("this._done = false;");
            },
        );
        w.block("next() {", "}", |w| {
            w.block("if (this._done) {", "}", |w| {
                w.line("return { done: true, value: undefined };");
            });
            w.line("const _v = __invoke(this._nextFn, [this._ext], this._map);");
            w.block("if (_v === undefined) {", "}", |w| {
                w.line("this._done = true;");
                w.line("return { done: true, value: undefined };");
            });
            w.line("return { done: false, value: this._wrapElem ? this._wrapElem(_v) : _v };");
        });
        w.block("return(value) {", "}", |w| {
            w.block("if (!this._done) {", "}", |w| {
                w.line("this._done = true;");
                w.line("this._destroyFn(this._ext);");
            });
            w.line("return { done: true, value };");
        });
        w.block("[Symbol.iterator]() {", "}", |w| {
            w.line("return this;");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}
