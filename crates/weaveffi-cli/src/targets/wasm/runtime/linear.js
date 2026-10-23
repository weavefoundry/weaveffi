// The WebAssembly transport: loading the module, staging arguments in its
// linear memory, reading results and errors back, and installing JavaScript
// functions in its function table (async completions and callback
// interface vtables). The generated `index.js` builds one native entry point
// per C symbol on top of this.
//
// Runs from {prefix}_alloc are 8-aligned, but multi-byte reads and writes
// still go through a `DataView` (little-endian, any address). The slots the
// producer writes through typed pointers (the error struct, `out_len`,
// `out_value`, iterator items, vtables) live in 8-byte-aligned blocks carved
// from the transport's own arenas.
//
// A trap (a producer panic aborts on wasm32) leaves the instance's memory in
// an unknown state, so it poisons the instance: every later call throws a
// code -2 fault instead of running, and releases (`_destroy`, cancel
// tokens, freeing runs) do nothing.

import { $Fault } from './runtime.js';

const $enc = new TextEncoder();
const $dec = new TextDecoder();

function $show(v) {
  if (v === null) return 'null';
  if (typeof v === 'bigint') return `${v}n`;
  if (typeof v === 'number') return String(v);
  return typeof v;
}

function $expected(what, v) {
  return new TypeError(`expected ${what}, got ${$show(v)}`);
}

/** Integer argument checks: `[min, max]` per kind. */
function $int(v, lo, hi) {
  if (typeof v !== 'number') throw $expected('a number', v);
  if (!Number.isInteger(v) || v < lo || v > hi) {
    throw new RangeError(`expected an integer in [${lo}, ${hi}], got ${v}`);
  }
  return v;
}

function $int64(v, signed) {
  const shape = signed ? 'a signed 64-bit integer' : 'an unsigned 64-bit integer';
  let b;
  if (typeof v === 'bigint') {
    b = v;
  } else if (typeof v === 'number') {
    if (!Number.isInteger(v)) throw new RangeError(`expected ${shape}, got ${v}`);
    b = BigInt(v);
  } else {
    throw $expected('a bigint', v);
  }
  if ((signed ? BigInt.asIntN(64, b) : BigInt.asUintN(64, b)) !== b) {
    throw new RangeError(`expected ${shape}, got ${b}`);
  }
  return b;
}

/** The size of each arena the transport carves its permanent blocks from. */
const $ARENA = 4096;

/** Offsets in `{prefix}_error` on wasm32: code, message (ptr, len), payload (ptr, len). */
const $ERR_SIZE = 20;

/**
 * One loaded module's linear memory and runtime symbols. `exports` holds
 * the module's exports by C symbol name; `memory()` returns the current
 * `ArrayBuffer`; `fnptr(params, results, fn)` installs `fn` in the function
 * table and returns its index.
 *
 * `x` holds every exported function guarded against a poisoned instance (a
 * call throws), `q` the same functions for releases (a call does nothing).
 */
export class $Linear {
  constructor(library, exports, prefix, memory, fnptr) {
    this.library = library;
    this.memory = memory;
    this.fnptr = fnptr;
    this.trap = null;
    this.x = {};
    this.q = {};
    for (const [name, fn] of Object.entries(exports)) {
      if (typeof fn !== 'function') continue;
      this.x[name] = (...args) => this.enter(fn, args);
      this.q[name] = (...args) => (this.trap === null ? this.enter(fn, args) : undefined);
    }
    for (const symbol of ['alloc', 'free_bytes', 'error_set', 'error_set_payload', 'error_clear', 'error_free']) {
      if (typeof exports[`${prefix}_${symbol}`] !== 'function') {
        throw new Error(`${library}: the WebAssembly module does not export ${prefix}_${symbol}`);
      }
    }
    this.allocFn = this.x[`${prefix}_alloc`];
    this.freeBytesFn = this.q[`${prefix}_free_bytes`];
    this.errorClearFn = this.q[`${prefix}_error_clear`];
    this.errorFreeFn = this.q[`${prefix}_error_free`];
    this.errorSetFn = this.x[`${prefix}_error_set`];
    this.errorSetPayloadFn = this.x[`${prefix}_error_set_payload`];
    // Permanent blocks (the scratch slots and the vtables) come from
    // arenas allocated with {prefix}_alloc and never freed; `arenas` counts
    // them so the leak counter of byte runs can leave them out.
    this.arenas = 0;
    this.arenaNext = 0;
    this.arenaEnd = 0;
    // One scratch block reused by every call: the error slot, `out_len`,
    // `out_value` (and an iterator's item), and an iterator's presence
    // flag. The producer only writes them as a call returns, so a call made
    // from a callback in the middle of another call can reuse them safely.
    const scratch = this.reserve(48);
    this.err = scratch;
    this.len = scratch + 24;
    this.out = scratch + 32;
    this.item = scratch + 32;
    this.has = scratch + 40;
    this.callbacks = new Map();
    this.nextCallback = 1;
    this.pending = new Map();
    this.nextPending = 1;
    this.completions = new Map();
    this.vtables = new Map();
  }

  /**
   * Call the module function `fn`. A trap poisons the instance (see the
   * file comment); a poisoned instance throws instead of calling.
   */
  enter(fn, args) {
    if (this.trap !== null) throw this.poisoned();
    try {
      return fn(...args);
    } catch (e) {
      if (e instanceof WebAssembly.RuntimeError && this.trap === null) this.trap = e;
      throw e;
    }
  }

  poisoned() {
    return new $Fault(
      -2,
      `${this.library}: the WebAssembly instance trapped earlier (${this.trap.message}) and can't be used anymore; load the module again in a new process or page`,
      null,
    );
  }

  bytes() {
    const buffer = this.memory();
    if (buffer !== this.buffer) {
      this.buffer = buffer;
      this.heap = new Uint8Array(buffer);
      this.dv = new DataView(buffer);
    }
    return this.heap;
  }

  view() {
    this.bytes();
    return this.dv;
  }

  /** A run of `size` bytes from {prefix}_alloc (`0` for an empty one). */
  alloc(size) {
    if (size === 0) return 0;
    const ptr = this.allocFn(size) >>> 0;
    if (ptr === 0) throw new RangeError(`the WebAssembly module could not allocate ${size} bytes`);
    return ptr;
  }

  /** A permanent, zero-filled, 8-byte-aligned block of `size` bytes. */
  reserve(size) {
    const need = Math.ceil(size / 8) * 8;
    if (this.arenaNext + need > this.arenaEnd) {
      const length = Math.max($ARENA, need + 8);
      const base = this.alloc(length);
      this.arenas++;
      this.arenaNext = Math.ceil(base / 8) * 8;
      this.arenaEnd = base + length;
    }
    const at = this.arenaNext;
    this.arenaNext += need;
    return at;
  }

  /**
   * A `{prefix}_debug_live(kind)` result as a bigint, leaving out the
   * transport's own arenas from the count of byte runs (kind 4).
   */
  live(raw, kind) {
    const n = BigInt.asUintN(64, raw);
    return kind === 4 && n > 0n ? n - BigInt(this.arenas) : n;
  }

  // Argument checks and conversions for by-value slots: integers are
  // range-checked (a `RangeError` rather than a silent wrap), 64-bit ones
  // take a bigint or an integral number.
  i8(v) { return $int(v, -128, 127); }
  i16(v) { return $int(v, -32768, 32767); }
  i32(v) { return $int(v, -2147483648, 2147483647); }
  u8(v) { return $int(v, 0, 255); }
  u16(v) { return $int(v, 0, 65535); }
  u32(v) { return $int(v, 0, 4294967295); }
  i64(v) { return $int64(v, true); }
  u64(v) { return $int64(v, false); }

  num(v) {
    if (typeof v !== 'number') throw $expected('a number', v);
    return v;
  }

  bool(v) {
    if (typeof v !== 'boolean') throw $expected('a boolean', v);
    return v ? 1 : 0;
  }

  /** Whether an optional argument is present (neither null nor undefined). */
  some(v) {
    return v !== null && v !== undefined;
  }

  handle(v) {
    if (typeof v !== 'number' || v === 0) throw $expected('an object handle', v);
    return v;
  }

  handleOpt(v) {
    return v === null || v === undefined ? 0 : this.handle(v);
  }

  // Staging: copy a string, bytes, or a typed array into a run for one
  // call. A staged value is [ptr, len, size] (`len` counts elements of a
  // typed array); release it with unstage().
  str(s) {
    if (typeof s !== 'string') throw $expected('a string', s);
    if (s.length === 0) return [0, 0, 0];
    const size = s.length * 3;
    const ptr = this.alloc(size);
    const { written } = $enc.encodeInto(s, this.bytes().subarray(ptr, ptr + size));
    return [ptr, written, size];
  }

  data(b) {
    if (!(b instanceof Uint8Array)) throw $expected('a Uint8Array', b);
    if (b.length === 0) return [0, 0, 0];
    const ptr = this.alloc(b.length);
    this.bytes().set(b, ptr);
    return [ptr, b.length, b.length];
  }

  slice(a, Typed) {
    if (!(a instanceof Typed)) throw $expected(`a ${Typed.name}`, a);
    if (a.length === 0) return [0, 0, 0];
    const ptr = this.alloc(a.byteLength);
    this.bytes().set(new Uint8Array(a.buffer, a.byteOffset, a.byteLength), ptr);
    return [ptr, a.length, a.byteLength];
  }

  unstage(staged) {
    if (staged !== null && staged[2] !== 0) this.freeBytesFn(staged[0], staged[2]);
  }

  // Results.
  outLen() {
    return this.view().getUint32(this.len, true);
  }

  /** The pointer stored at `addr`. */
  ptrAt(addr) {
    return this.view().getUint32(addr, true);
  }

  readStr(ptr, len) {
    ptr >>>= 0;
    return len === 0 ? '' : $dec.decode(this.bytes().subarray(ptr, ptr + len));
  }

  readData(ptr, len) {
    ptr >>>= 0;
    return len === 0 ? new Uint8Array(0) : this.bytes().slice(ptr, ptr + len);
  }

  /** A copy of the typed array of `count` elements at `ptr`. */
  readSlice(ptr, count, Typed) {
    ptr >>>= 0;
    if (count === 0) return new Typed(0);
    return new Typed(this.memory().slice(ptr, ptr + count * Typed.BYTES_PER_ELEMENT));
  }

  takeStr(ptr, len) {
    const s = this.readStr(ptr, len);
    if (ptr !== 0) this.freeBytesFn(ptr, len);
    return s;
  }

  takeData(ptr, len) {
    const b = this.readData(ptr, len);
    if (ptr !== 0) this.freeBytesFn(ptr, len);
    return b;
  }

  takeSlice(ptr, count, Typed) {
    const a = this.readSlice(ptr, count, Typed);
    if (ptr !== 0) this.freeBytesFn(ptr, count * Typed.BYTES_PER_ELEMENT);
    return a;
  }

  fault(errPtr) {
    const dv = this.view();
    const code = dv.getInt32(errPtr, true);
    if (code === 0) return null;
    const payloadPtr = dv.getUint32(errPtr + 12, true);
    const payloadLen = dv.getUint32(errPtr + 16, true);
    return new $Fault(
      code,
      this.readStr(dv.getUint32(errPtr + 4, true), dv.getUint32(errPtr + 8, true)),
      payloadPtr === 0 ? null : this.readData(payloadPtr, payloadLen),
    );
  }

  /** Throw the error the last call wrote to the scratch error slot. */
  check() {
    if (this.view().getInt32(this.err, true) === 0) return;
    const fault = this.fault(this.err);
    this.errorClearFn(this.err);
    throw fault;
  }

  /** A module's contract table as a `BigUint64Array` of `id, hash` pairs. */
  contract(table) {
    const ptr = table(this.len) >>> 0;
    const n = this.outLen() * 2;
    const dv = this.view();
    const out = new BigUint64Array(n);
    for (let i = 0; i < n; i++) out[i] = dv.getBigUint64(ptr + i * 8, true);
    return out;
  }

  // Callback interfaces: implementations live in a table keyed by the
  // integer passed as `ctx`, until the producer calls the vtable's `free`.
  // An absent optional implementation is ctx 0 with a null vtable.
  register(adapter) {
    if (adapter === null) return 0;
    const id = this.nextCallback++;
    this.callbacks.set(id, adapter);
    return id;
  }

  adapter(id) {
    const adapter = this.callbacks.get(id);
    if (adapter === undefined) throw new Error('callback implementation was already released');
    return adapter;
  }

  unregister(id) {
    this.callbacks.delete(id);
  }

  /**
   * Report a callback implementation's exception through `errPtr`: a
   * `$Fault` (the adapter's report of a method declared `throws`) with its
   * code, message, and payload, anything else as code -4 with its message.
   */
  foreign(errPtr, e) {
    let code = -4;
    let message;
    let payload = null;
    if (e instanceof $Fault) {
      code = e.code;
      message = e.message;
      payload = e.payload;
    } else {
      message = e instanceof Error ? e.message || e.name : String(e);
    }
    const text = this.str(message);
    try {
      this.errorSetFn(errPtr, code, text[0], text[1]);
    } finally {
      this.unstage(text);
    }
    if (payload !== null && payload.length > 0) {
      const staged = this.data(payload);
      try {
        this.errorSetPayloadFn(errPtr, staged[0], staged[1]);
      } finally {
        this.unstage(staged);
      }
    }
  }

  /**
   * Hand `bytes` (a `Uint8Array`) to the producer through a callback's
   * `out_ptr`/`out_len` slots, as a run from {prefix}_alloc it adopts.
   */
  give(outPtr, outLen, bytes) {
    if (!(bytes instanceof Uint8Array)) throw $expected('a Uint8Array', bytes);
    const run = this.alloc(bytes.length);
    if (bytes.length > 0) this.bytes().set(bytes, run);
    const dv = this.view();
    dv.setUint32(outPtr >>> 0, run, true);
    dv.setUint32(outLen >>> 0, bytes.length, true);
  }

  /** `give` for a string, as UTF-8. */
  giveStr(outPtr, outLen, s) {
    if (typeof s !== 'string') throw $expected('a string', s);
    this.give(outPtr, outLen, $enc.encode(s));
  }

  /** `give` for a typed array of `Typed`; `out_len` is its element count. */
  giveSlice(outPtr, outLen, a, Typed) {
    if (!(a instanceof Typed)) throw $expected(`a ${Typed.name}`, a);
    const run = this.alloc(a.byteLength);
    if (a.length > 0) this.bytes().set(new Uint8Array(a.buffer, a.byteOffset, a.byteLength), run);
    const dv = this.view();
    dv.setUint32(outPtr >>> 0, run, true);
    dv.setUint32(outLen >>> 0, a.length, true);
  }

  /**
   * The static vtable of one callback interface, built on first use: the
   * header (`size`, `flags`, then the `free` entry), then one table entry
   * per method (`methods()` returns each as `[params, results, fn]`).
   * `flags` is 0: methods may be called from any thread (there is only
   * one).
   */
  vtable(name, methods) {
    let ptr = this.vtables.get(name);
    if (ptr !== undefined) return ptr;
    const entries = [[['i32'], [], (ctx) => this.unregister(ctx)], ...methods()];
    const size = 8 + entries.length * 4;
    ptr = this.reserve(size);
    const dv = this.view();
    dv.setUint32(ptr, size, true);
    dv.setUint32(ptr + 4, 0, true);
    entries.forEach(([params, results, fn], i) => {
      dv.setUint32(ptr + 8 + i * 4, this.fnptr(params, results, fn), true);
    });
    this.vtables.set(name, ptr);
    return ptr;
  }

  /** `vtable`, or a null vtable for an absent optional implementation. */
  vtableOpt(adapter, name, methods) {
    return adapter === null ? 0 : this.vtable(name, methods);
  }

  /**
   * Launch an async call. `call(callback, context)` invokes the launcher;
   * the completion (one table entry per signature, shared by every call)
   * settles the returned promise, converting the result slots with
   * `convert`. On this single-threaded target the completion normally
   * fires before the launcher returns.
   */
  launch(params, results, call, convert) {
    const key = `${params.join(',')}:${results.join(',')}`;
    let callback = this.completions.get(key);
    if (callback === undefined) {
      const complete = (ctx, errPtr, ...result) => this.complete(ctx, errPtr, result);
      callback = this.fnptr(params, results, complete);
      this.completions.set(key, callback);
    }
    const id = this.nextPending++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, convert });
      try {
        call(callback, id);
      } catch (e) {
        this.pending.delete(id);
        reject(e);
      }
    });
  }

  complete(id, errPtr, result) {
    const pending = this.pending.get(id);
    if (pending === undefined) return;
    this.pending.delete(id);
    try {
      const fault = errPtr === 0 ? null : this.fault(errPtr >>> 0);
      if (errPtr !== 0) this.errorFreeFn(errPtr);
      if (fault !== null) {
        pending.reject(fault);
      } else {
        pending.resolve(pending.convert(...result));
      }
    } catch (e) {
      pending.reject(e);
    }
  }
}

/** A stand-in for the native entry points until `init()` has finished. */
export function $unloaded(library) {
  return new Proxy(
    {},
    {
      get() {
        throw new Error(`${library}: call init() before using the bindings`);
      },
    },
  );
}

const $valtype = { i32: 0x7f, i64: 0x7e, f32: 0x7d, f64: 0x7c };

function $leb(n) {
  const out = [];
  do {
    let b = n & 0x7f;
    n >>>= 7;
    if (n !== 0) b |= 0x80;
    out.push(b);
  } while (n !== 0);
  return out;
}

function $section(id, body) {
  return [id, ...$leb(body.length), ...body];
}

/**
 * Turn a JavaScript function into a WebAssembly function with the given
 * signature: `WebAssembly.Function` where the engine has it, otherwise a
 * tiny module that imports `fn` and re-exports it.
 */
function $wasmFunction(params, results, fn) {
  if (typeof WebAssembly.Function === 'function') {
    return new WebAssembly.Function({ parameters: params, results }, fn);
  }
  const type = [0x60, ...$leb(params.length), ...params.map((t) => $valtype[t])];
  type.push(...$leb(results.length), ...results.map((t) => $valtype[t]));
  const bytes = new Uint8Array([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...$section(1, [1, ...type]),
    ...$section(2, [1, 1, 0x65, 1, 0x66, 0x00, 0x00]),
    ...$section(7, [1, 1, 0x66, 0x00, 0x00]),
  ]);
  const instance = new WebAssembly.Instance(new WebAssembly.Module(bytes), { e: { f: fn } });
  return instance.exports.f;
}

async function $bytesOf(source, envVar, fallback) {
  if (source === undefined || source === null) {
    const env = typeof process === 'object' && process.env ? process.env[envVar] : undefined;
    if (env) {
      const { readFile } = await import('node:fs/promises');
      return readFile(env);
    }
    source = fallback;
  }
  if (source instanceof WebAssembly.Module || source instanceof ArrayBuffer || ArrayBuffer.isView(source)) {
    return source;
  }
  if (typeof Response === 'function' && source instanceof Response) {
    return source.arrayBuffer();
  }
  const url = source instanceof URL ? source : new URL(String(source), fallback);
  if (url.protocol === 'file:') {
    const { readFile } = await import('node:fs/promises');
    return readFile(url);
  }
  const response = await fetch(url);
  if (!response.ok) throw new Error(`failed to fetch ${url}: ${response.status}`);
  return response.arrayBuffer();
}

/**
 * Instantiate a `wasm32-unknown-unknown` module from `source` (a URL, a
 * path, bytes, a `Response`, or a compiled module). Without a source it
 * reads the file named by the `envVar` environment variable (on Node.js),
 * else `fallback`. The module must export its memory and a growable
 * function table (`__indirect_function_table`).
 */
export async function $loadWasm(library, source, prefix, envVar, fallback) {
  const bytes = await $bytesOf(source, envVar, fallback);
  const module = bytes instanceof WebAssembly.Module ? bytes : await WebAssembly.compile(bytes);
  const { exports } = await WebAssembly.instantiate(module, {});
  const table = exports.__indirect_function_table;
  const fnptr = (params, results, fn) => {
    if (!(table instanceof WebAssembly.Table)) {
      throw new Error(
        `${library}: the WebAssembly module does not export __indirect_function_table (link with --export-table --growable-table)`,
      );
    }
    const index = table.grow(1);
    table.set(index, $wasmFunction(params, results, fn));
    return index;
  };
  return new $Linear(library, exports, prefix, () => exports.memory.buffer, fnptr);
}
