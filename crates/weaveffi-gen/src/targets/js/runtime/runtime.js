// The runtime shared by every binding in this package: the error classes,
// the value-buffer codec, the object and iterator wrappers, cancellation, and
// the load-time contract check. The generated `index.js` imports it; the
// native transport (an N-API addon or a WebAssembly instance) never sees it,
// except for `$Fault`, the shape every native failure arrives in.

/**
 * The root of every error these bindings throw. `code` is the ABI error
 * code: positive for a declared domain error, negative for a runtime trap
 * (-1 generic, -2 producer panic, -3 marshalling failure, -4 a callback
 * implementation failed, -5 cancelled).
 */
export class {{ERROR_CLASS}} extends Error {
  constructor(code, message) {
    super(message === undefined ? '' : message);
    this.name = new.target.name;
    this.code = code;
  }
}

/** The error a cancelled async call rejects with (code -5). */
export class CancelledError extends {{ERROR_CLASS}} {
  constructor(message = 'cancelled') {
    super(-5, message);
  }
}

/**
 * A failure as the native transport reports it, before the generated
 * wrappers map it onto an error class. `payload` holds the matched error
 * code's fields as a value buffer, or `null`.
 */
export class $Fault {
  constructor(code, message, payload) {
    this.code = code;
    this.message = message;
    this.payload = payload === undefined ? null : payload;
  }
}

/**
 * Map an exception raised by a native call onto the public error classes:
 * a cancelled call becomes `CancelledError`, any other fault the root error,
 * and a WebAssembly trap (a producer panic on a `panic = "abort"` build) a
 * code -2 error. Anything else (a `TypeError` from argument checking, say)
 * passes through unchanged.
 */
export function $fault(e) {
  if (e instanceof $Fault) {
    return e.code === -5
      ? new CancelledError(e.message || undefined)
      : new {{ERROR_CLASS}}(e.code, e.message);
  }
  if (typeof WebAssembly === 'object' && e instanceof WebAssembly.RuntimeError) {
    return new {{ERROR_CLASS}}(-2, 'the native library trapped: ' + e.message);
  }
  return e;
}

/**
 * Map a fault onto an error domain: `codes` maps each declared code to its
 * class, and `payloads` (optional) maps codes that carry fields to the
 * reader that decodes them onto the error. Codes outside the domain fall
 * back to `$fault`.
 */
export function $domain(e, codes, payloads) {
  if (e instanceof $Fault) {
    const cls = codes.get(e.code);
    if (cls !== undefined) {
      const err = new cls(e.message || undefined);
      const read = payloads === undefined ? undefined : payloads.get(e.code);
      if (read !== undefined && e.payload !== null) {
        Object.assign(err, $decode(e.payload, read));
      }
      return err;
    }
  }
  return $fault(e);
}

function $malformed(what) {
  return new {{ERROR_CLASS}}(-3, 'malformed value buffer: ' + what);
}

function $expected(what, v) {
  return new TypeError(`expected ${what}, got ${v === null ? 'null' : typeof v}`);
}

function $bigint(v) {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number' && Number.isInteger(v)) return BigInt(v);
  throw $expected('a bigint', v);
}

const $utf8 = new TextEncoder();
const $utf8Strict = new TextDecoder('utf-8', { fatal: true });

/**
 * Writes the value-buffer wire format: little-endian, packed, no alignment.
 * Strings and bytes are a u32 length then the bytes, optionals a presence
 * byte then the value, lists a u32 count then the elements, and maps a u32
 * count then alternating keys and values.
 */
export class $Writer {
  constructor() {
    this.buf = new Uint8Array(64);
    this.view = new DataView(this.buf.buffer);
    this.len = 0;
  }

  reserve(n) {
    if (this.len + n <= this.buf.length) return;
    let cap = this.buf.length * 2;
    while (cap < this.len + n) cap *= 2;
    const grown = new Uint8Array(cap);
    grown.set(this.buf.subarray(0, this.len));
    this.buf = grown;
    this.view = new DataView(grown.buffer);
  }

  num(v) {
    if (typeof v !== 'number') throw $expected('a number', v);
    return v;
  }

  big(v, bits) {
    const b = $bigint(v);
    if (bits(64, b) !== b) throw new RangeError(`${b} does not fit in a 64-bit integer`);
    return b;
  }

  writeBool(v) {
    if (typeof v !== 'boolean') throw $expected('a boolean', v);
    this.reserve(1);
    this.buf[this.len++] = v ? 1 : 0;
  }

  writeI8(v) { this.reserve(1); this.view.setInt8(this.len, this.num(v)); this.len += 1; }
  writeU8(v) { this.reserve(1); this.view.setUint8(this.len, this.num(v)); this.len += 1; }
  writeI16(v) { this.reserve(2); this.view.setInt16(this.len, this.num(v), true); this.len += 2; }
  writeU16(v) { this.reserve(2); this.view.setUint16(this.len, this.num(v), true); this.len += 2; }
  writeI32(v) { this.reserve(4); this.view.setInt32(this.len, this.num(v), true); this.len += 4; }
  writeU32(v) { this.reserve(4); this.view.setUint32(this.len, this.num(v), true); this.len += 4; }
  writeI64(v) { this.reserve(8); this.view.setBigInt64(this.len, this.big(v, BigInt.asIntN), true); this.len += 8; }
  writeU64(v) { this.reserve(8); this.view.setBigUint64(this.len, this.big(v, BigInt.asUintN), true); this.len += 8; }
  writeF32(v) { this.reserve(4); this.view.setFloat32(this.len, this.num(v), true); this.len += 4; }
  writeF64(v) { this.reserve(8); this.view.setFloat64(this.len, this.num(v), true); this.len += 8; }

  writeString(v) {
    if (typeof v !== 'string') throw $expected('a string', v);
    // UTF-8 never needs more than three bytes per UTF-16 code unit.
    this.reserve(4 + v.length * 3);
    const { written } = $utf8.encodeInto(v, this.buf.subarray(this.len + 4));
    this.view.setUint32(this.len, written, true);
    this.len += 4 + written;
  }

  writeBytes(v) {
    if (!(v instanceof Uint8Array)) throw $expected('a Uint8Array', v);
    this.reserve(4 + v.length);
    this.view.setUint32(this.len, v.length, true);
    this.buf.set(v, this.len + 4);
    this.len += 4 + v.length;
  }

  writeOpt(v, write) {
    if (v === null || v === undefined) {
      this.writeBool(false);
    } else {
      this.writeBool(true);
      write(this, v);
    }
  }

  writeList(v, write) {
    if (!Array.isArray(v)) throw $expected('an array', v);
    this.writeU32(v.length);
    for (const e of v) write(this, e);
  }

  writeMap(v, writeKey, writeValue) {
    if (v === null || typeof v !== 'object') throw $expected('an object or a Map', v);
    const entries = v instanceof Map ? [...v.entries()] : Object.entries(v);
    this.writeU32(entries.length);
    for (const [k, e] of entries) {
      writeKey(this, k);
      writeValue(this, e);
    }
  }

  finish() {
    return this.buf.subarray(0, this.len);
  }
}

/**
 * Reads the value-buffer wire format. Every read is bounds-checked; a
 * truncated buffer, an invalid bool or presence byte, invalid UTF-8, or
 * trailing bytes throw a code -3 error.
 */
export class $Reader {
  constructor(bytes) {
    this.buf = bytes;
    this.view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    this.pos = 0;
  }

  take(n) {
    if (n > this.buf.length - this.pos) throw $malformed('truncated');
    const at = this.pos;
    this.pos += n;
    return at;
  }

  readBool() {
    const b = this.buf[this.take(1)];
    if (b > 1) throw $malformed('invalid bool byte ' + b);
    return b === 1;
  }

  readI8() { return this.view.getInt8(this.take(1)); }
  readU8() { return this.view.getUint8(this.take(1)); }
  readI16() { return this.view.getInt16(this.take(2), true); }
  readU16() { return this.view.getUint16(this.take(2), true); }
  readI32() { return this.view.getInt32(this.take(4), true); }
  readU32() { return this.view.getUint32(this.take(4), true); }
  readI64() { return this.view.getBigInt64(this.take(8), true); }
  readU64() { return this.view.getBigUint64(this.take(8), true); }
  readF32() { return this.view.getFloat32(this.take(4), true); }
  readF64() { return this.view.getFloat64(this.take(8), true); }

  readString() {
    const n = this.readU32();
    const at = this.take(n);
    try {
      return $utf8Strict.decode(this.buf.subarray(at, at + n));
    } catch {
      throw $malformed('invalid UTF-8');
    }
  }

  readBytes() {
    const n = this.readU32();
    const at = this.take(n);
    return this.buf.slice(at, at + n);
  }

  readOpt(read) {
    return this.readBool() ? read(this) : null;
  }

  // A count may exceed the remaining bytes when elements encode to zero
  // bytes (a record without fields), so it is not checked up front; a
  // truncated element still fails on its first read.
  readList(read) {
    const n = this.readU32();
    const out = [];
    for (let i = 0; i < n; i++) out.push(read(this));
    return out;
  }

  readMap(readKey, readValue) {
    const n = this.readU32();
    const out = {};
    for (let i = 0; i < n; i++) {
      const k = readKey(this);
      out[k] = readValue(this);
    }
    return out;
  }

  end() {
    if (this.pos !== this.buf.length) throw $malformed('trailing bytes');
  }
}

/** Encode `v` with `write` into a fresh value buffer. */
export function $encode(v, write) {
  const w = new $Writer();
  write(w, v);
  return w.finish();
}

/** Decode a whole value buffer with `read`, rejecting trailing bytes. */
export function $decode(bytes, read) {
  const r = new $Reader(bytes);
  const v = read(r);
  r.end();
  return v;
}

/** Element writers for the fixed-width primitives, strings, and bytes. */
export const $W = Object.freeze({
  Bool: (w, v) => w.writeBool(v),
  I8: (w, v) => w.writeI8(v),
  I16: (w, v) => w.writeI16(v),
  I32: (w, v) => w.writeI32(v),
  I64: (w, v) => w.writeI64(v),
  U8: (w, v) => w.writeU8(v),
  U16: (w, v) => w.writeU16(v),
  U32: (w, v) => w.writeU32(v),
  U64: (w, v) => w.writeU64(v),
  F32: (w, v) => w.writeF32(v),
  F64: (w, v) => w.writeF64(v),
  String: (w, v) => w.writeString(v),
  Bytes: (w, v) => w.writeBytes(v),
});

/** Element readers for the fixed-width primitives, strings, and bytes. */
export const $R = Object.freeze({
  Bool: (r) => r.readBool(),
  I8: (r) => r.readI8(),
  I16: (r) => r.readI16(),
  I32: (r) => r.readI32(),
  I64: (r) => r.readI64(),
  U8: (r) => r.readU8(),
  U16: (r) => r.readU16(),
  U32: (r) => r.readU32(),
  U64: (r) => r.readU64(),
  F32: (r) => r.readF32(),
  F64: (r) => r.readF64(),
  String: (r) => r.readString(),
  Bytes: (r) => r.readBytes(),
});

/** The key `using` declarations call, or a stand-in where it is missing. */
export const $dispose = typeof Symbol.dispose === 'symbol' ? Symbol.dispose : Symbol.for('Symbol.dispose');

// Releases the native reference of a wrapper collected without close().
// The held value never references the wrapper, so it can't keep it alive.
const $objects = new FinalizationRegistry((held) => held.destroy(held.handle));

function $release(o) {
  const handle = o.$h;
  o.$h = null;
  o.constructor.$destroy(handle);
}

/**
 * The base of every interface class. An instance owns one strong reference
 * to a native object. `close()` (or a `using` declaration) releases it;
 * a wrapper that is garbage collected unclosed is released by a
 * `FinalizationRegistry`. A `close()` that races an in-flight call (from a
 * callback, or while an async method is pending) is deferred until that
 * call returns, so the native object is never freed mid-call.
 */
export class $Object {
  close() {
    if (this.$closed !== false) return;
    this.$closed = true;
    $objects.unregister(this);
    if (this.$calls === 0) $release(this);
  }

  [$dispose]() {
    this.close();
  }
}

/** Bind a native handle (one strong reference) to wrapper `o`. */
export function $own(o, handle) {
  o.$h = handle;
  o.$calls = 0;
  o.$closed = false;
  $objects.register(o, { handle, destroy: o.constructor.$destroy }, o);
  return o;
}

/** Wrap a handle the native side handed over in a new `cls` instance. */
export function $adopt(cls, handle) {
  return $own(Object.create(cls.prototype), handle);
}

/** `$adopt`, with `null` for an absent object. */
export function $adoptOpt(cls, handle) {
  return handle === null ? null : $adopt(cls, handle);
}

/**
 * Lend wrapper `o` (an instance of `cls`) to one native call and return its
 * handle. Every `$lend` is paired with an `$unlend` when the call returns.
 */
export function $lend(o, cls) {
  if (!(o instanceof cls)) throw $expected(`a ${cls.name}`, o);
  if (o.$closed !== false) throw new {{ERROR_CLASS}}(-3, `${cls.name} used after close()`);
  o.$calls++;
  return o.$h;
}

/** `$lend`, with `null` for an absent object. */
export function $lendOpt(o, cls) {
  return o === null || o === undefined ? null : $lend(o, cls);
}

/** End one loan of `o`, completing a `close()` deferred by the loan. */
export function $unlend(o) {
  if (o === null || o === undefined) return;
  if (--o.$calls === 0 && o.$closed) $release(o);
}

/**
 * A second strong reference to `o`'s native object, for an object token
 * written into a value buffer (the native side adopts it).
 */
export function $clone(o, cls) {
  const handle = $lend(o, cls);
  try {
    return cls.$clone(handle);
  } finally {
    $unlend(o);
  }
}

const $iterators = new FinalizationRegistry((held) => held.destroy(held.handle));

/**
 * A lazy iterator over a native `iter<T>`: each `next()` makes exactly one
 * native call. The native iterator is destroyed exactly once: when it is
 * exhausted or fails, by `return()` (which `for...of` calls on `break`), by
 * `close()` or a `using` declaration, or when the iterator is collected.
 * `spec` supplies `next`, `destroy`, `map` (the error mapping), and
 * `convert` (or `null` when elements need no conversion).
 */
export class $Iterator {
  constructor(handle, spec) {
    this.$h = handle;
    this.$spec = spec;
    $iterators.register(this, { handle, destroy: spec.destroy }, this);
  }

  next() {
    if (this.$h === null) return { done: true, value: undefined };
    const spec = this.$spec;
    let v;
    try {
      v = spec.next(this.$h);
    } catch (e) {
      this.close();
      throw spec.map(e);
    }
    if (v === undefined) {
      this.close();
      return { done: true, value: undefined };
    }
    return { done: false, value: spec.convert === null ? v : spec.convert(v) };
  }

  return(value) {
    this.close();
    return { done: true, value };
  }

  close() {
    const handle = this.$h;
    if (handle === null) return;
    this.$h = null;
    $iterators.unregister(this);
    this.$spec.destroy(handle);
  }

  [Symbol.iterator]() {
    return this;
  }

  [$dispose]() {
    this.close();
  }
}

/**
 * Run a cancellable launch. Without a signal, `launch(null)` runs as is.
 * With one, a native cancel token is created and passed to `launch`,
 * aborting the signal cancels it (an already-aborted signal cancels it
 * before the launch, so the call completes with `CancelledError` at once),
 * and the token is destroyed once the call settles. `tokens` supplies the
 * native `create`, `cancel`, and `destroy`.
 */
export function $cancellable(signal, tokens, launch) {
  if (signal === undefined || signal === null) return launch(null);
  if (typeof signal.addEventListener !== 'function') throw $expected('an AbortSignal', signal);
  const token = tokens.create();
  const abort = () => tokens.cancel(token);
  const done = () => {
    signal.removeEventListener('abort', abort);
    tokens.destroy(token);
  };
  if (signal.aborted) {
    abort();
  } else {
    signal.addEventListener('abort', abort, { once: true });
  }
  let pending;
  try {
    pending = launch(token);
  } catch (e) {
    done();
    throw e;
  }
  return pending.finally(done);
}

/** Check a callback interface implementation. */
export function $impl(impl, what) {
  if (impl === null || (typeof impl !== 'object' && typeof impl !== 'function')) {
    throw $expected(`a ${what} implementation`, impl);
  }
  return impl;
}

function $num(v, where) {
  if (typeof v !== 'number') throw new TypeError(`${where} must return a number`);
  return v;
}

function $retBigint(v, where) {
  if (typeof v === 'bigint') return v;
  if (typeof v === 'number' && Number.isInteger(v)) return BigInt(v);
  throw new TypeError(`${where} must return a bigint`);
}

/**
 * Checks for a callback method's return value, by wire primitive. A value
 * of the wrong type throws, which reports the call as a code -4 failure.
 */
export const $ret = Object.freeze({
  Bool(v, where) {
    if (typeof v !== 'boolean') throw new TypeError(`${where} must return a boolean`);
    return v;
  },
  I8: $num,
  I16: $num,
  I32: $num,
  U8: $num,
  U16: $num,
  U32: $num,
  F32: $num,
  F64: $num,
  I64: $retBigint,
  U64: $retBigint,
});

/**
 * The load-time contract check: the native library must implement ABI
 * revision `abi`, and every top-level module's contract checksum must
 * match the one these bindings were generated from.
 */
export function $verify(raw, library, prefix, abi, checksums) {
  const version = raw[`${prefix}_abi_version`];
  if (typeof version !== 'function') {
    throw new Error(`${library}: the native library does not export ${prefix}_abi_version`);
  }
  const found = Number(version());
  if (found !== abi) {
    throw new Error(
      `${library}: the native library implements ABI revision ${found}, but these bindings require revision ${abi}`,
    );
  }
  for (const [module, expected] of checksums) {
    const checksum = raw[`${prefix}_${module}_checksum`];
    const got = typeof checksum === 'function' ? BigInt.asUintN(64, BigInt(checksum())) : null;
    if (got !== expected) {
      const what = got === null ? 'no contract checksum' : `contract checksum 0x${got.toString(16)}`;
      throw new Error(
        `${library}: module '${module}' of the native library does not match these bindings ` +
          `(${what}, expected 0x${expected.toString(16)}); regenerate the bindings for this build`,
      );
    }
  }
}
