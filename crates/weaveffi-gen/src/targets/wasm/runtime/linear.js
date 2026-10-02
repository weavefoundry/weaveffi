// The WebAssembly transport: loading the module, staging arguments in its
// linear memory, reading results and errors back, and installing JavaScript
// functions in its function table (async completions and callback
// interface vtables). The generated `index.js` builds one native entry point
// per C symbol on top of this.

import { $Fault } from './runtime.js';

const $enc = new TextEncoder();
const $dec = new TextDecoder();

function $expected(what, v) {
  return new TypeError(`expected ${what}, got ${v === null ? 'null' : typeof v}`);
}

function $bigint(v, bits, what) {
  let b;
  if (typeof v === 'bigint') {
    b = v;
  } else if (typeof v === 'number' && Number.isInteger(v)) {
    b = BigInt(v);
  } else {
    throw $expected('a bigint', v);
  }
  if (bits(64, b) !== b) throw new RangeError(`bigint does not fit in ${what} 64-bit integer`);
  return b;
}

/**
 * One loaded module's linear memory and runtime symbols. `x` holds the
 * module's exports by C symbol name; `memory()` returns the current
 * `ArrayBuffer`; `fnptr(params, results, sig, fn)` installs `fn` in the
 * function table and returns its index.
 */
export class $Linear {
  constructor(x, prefix, memory, fnptr) {
    this.x = x;
    this.memory = memory;
    this.fnptr = fnptr;
    this.allocFn = x[`${prefix}_alloc`];
    this.deallocFn = x[`${prefix}_dealloc`];
    this.freeBytesFn = x[`${prefix}_free_bytes`];
    this.errorClearFn = x[`${prefix}_error_clear`];
    this.errorFreeFn = x[`${prefix}_error_free`];
    this.errorSetFn = x[`${prefix}_error_set`];
    if (typeof this.allocFn !== 'function' || typeof this.deallocFn !== 'function') {
      throw new Error(`the WebAssembly module does not export ${prefix}_alloc and ${prefix}_dealloc`);
    }
    // One scratch block reused by every call: the error slot (16 bytes),
    // the out_len slot (4), and the iterator out_item slot (8). The producer
    // only writes them as a call returns, so a call made from a callback in
    // the middle of another call can reuse them safely.
    const scratch = this.allocFn(32);
    this.bytes().fill(0, scratch, scratch + 32);
    this.err = scratch;
    this.len = scratch + 16;
    this.item = scratch + 24;
    this.callbacks = new Map();
    this.nextCallback = 1;
    this.pending = new Map();
    this.nextPending = 1;
    this.completions = new Map();
    this.vtables = new Map();
  }

  bytes() {
    const buffer = this.memory();
    if (buffer !== this.buffer) {
      this.buffer = buffer;
      this.u8 = new Uint8Array(buffer);
      this.dv = new DataView(buffer);
    }
    return this.u8;
  }

  view() {
    this.bytes();
    return this.dv;
  }

  // Argument checks and conversions for by-value slots.
  num(v) {
    if (typeof v !== 'number') throw $expected('a number', v);
    return v;
  }

  i64(v) {
    return $bigint(v, BigInt.asIntN, 'a signed');
  }

  u64(v) {
    return $bigint(v, BigInt.asUintN, 'an unsigned');
  }

  bool(v) {
    if (typeof v !== 'boolean') throw $expected('a boolean', v);
    return v ? 1 : 0;
  }

  handle(v) {
    if (typeof v !== 'number' || v === 0) throw $expected('an object handle', v);
    return v;
  }

  handleOpt(v) {
    return v === null || v === undefined ? 0 : this.handle(v);
  }

  // Staging: copy a string or bytes into linear memory for one call. A
  // staged value is [ptr, len, size]; release it with unstage().
  str(s) {
    if (typeof s !== 'string') throw $expected('a string', s);
    if (s.length === 0) return [0, 0, 0];
    const size = s.length * 3;
    const ptr = this.allocFn(size);
    const { written } = $enc.encodeInto(s, this.bytes().subarray(ptr, ptr + size));
    return [ptr, written, size];
  }

  data(b) {
    if (!(b instanceof Uint8Array)) throw $expected('a Uint8Array', b);
    if (b.length === 0) return [0, 0, 0];
    const ptr = this.allocFn(b.length);
    this.bytes().set(b, ptr);
    return [ptr, b.length, b.length];
  }

  unstage(staged) {
    if (staged !== null && staged[2] !== 0) this.deallocFn(staged[0], staged[2]);
  }

  // Results.
  outLen() {
    return this.view().getUint32(this.len, true);
  }

  readStr(ptr, len) {
    return len === 0 ? '' : $dec.decode(this.bytes().subarray(ptr, ptr + len));
  }

  readData(ptr, len) {
    return len === 0 ? new Uint8Array(0) : this.bytes().slice(ptr, ptr + len);
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

  cstr(ptr) {
    if (ptr === 0) return '';
    const u8 = this.bytes();
    let end = ptr;
    while (u8[end] !== 0) end++;
    return $dec.decode(u8.subarray(ptr, end));
  }

  fault(errPtr) {
    const dv = this.view();
    const code = dv.getInt32(errPtr, true);
    if (code === 0) return null;
    const payloadPtr = dv.getUint32(errPtr + 8, true);
    const payloadLen = dv.getUint32(errPtr + 12, true);
    return new $Fault(
      code,
      this.cstr(dv.getUint32(errPtr + 4, true)),
      payloadPtr === 0 ? null : this.bytes().slice(payloadPtr, payloadPtr + payloadLen),
    );
  }

  /** Throw the error the last call wrote to the scratch error slot. */
  check() {
    if (this.view().getInt32(this.err, true) === 0) return;
    const fault = this.fault(this.err);
    this.errorClearFn(this.err);
    throw fault;
  }

  // Callback interfaces: implementations live in a table keyed by the
  // integer passed as `ctx`, until the producer calls the vtable's `free`.
  register(adapter) {
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

  /** Report a callback implementation's exception through `errPtr` (code -4). */
  foreign(errPtr, e) {
    const message = e instanceof Error ? e.message || e.name : String(e);
    const bytes = $enc.encode(message);
    const ptr = this.allocFn(bytes.length + 1);
    const u8 = this.bytes();
    u8.set(bytes, ptr);
    u8[ptr + bytes.length] = 0;
    this.errorSetFn(errPtr, -4, ptr);
    this.deallocFn(ptr, bytes.length + 1);
  }

  /**
   * The static vtable of one callback interface: one table entry per
   * method (`methods()` returns each as `[params, results, sig, fn]`), then
   * `free`. Built on first use.
   */
  vtable(name, methods) {
    let ptr = this.vtables.get(name);
    if (ptr !== undefined) return ptr;
    const entries = [...methods(), [['i32'], [], 'vi', (ctx) => this.unregister(ctx)]];
    ptr = this.allocFn(entries.length * 4);
    const dv = this.view();
    entries.forEach(([params, results, sig, fn], i) => {
      dv.setUint32(ptr + i * 4, this.fnptr(params, results, sig, fn), true);
    });
    this.vtables.set(name, ptr);
    return ptr;
  }

  /**
   * Launch an async call. `call(callback, context)` invokes the launcher;
   * the completion (one table entry per signature, shared by every call)
   * settles the returned promise, converting the result slots with
   * `convert`. On this single-threaded target the completion normally
   * fires before the launcher returns.
   */
  launch(params, results, sig, call, convert) {
    let callback = this.completions.get(sig);
    if (callback === undefined) {
      const complete = (ctx, errPtr, ...result) => this.complete(ctx, errPtr, result);
      callback = this.fnptr(params, results, sig, complete);
      this.completions.set(sig, callback);
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
      const fault = errPtr === 0 ? null : this.fault(errPtr);
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
export async function $loadWasm(source, prefix, envVar, fallback) {
  const bytes = await $bytesOf(source, envVar, fallback);
  const module = bytes instanceof WebAssembly.Module ? bytes : await WebAssembly.compile(bytes);
  const { exports } = await WebAssembly.instantiate(module, {});
  const table = exports.__indirect_function_table;
  const fnptr = (params, results, sig, fn) => {
    if (!(table instanceof WebAssembly.Table)) {
      throw new Error('the WebAssembly module does not export __indirect_function_table (link with --export-table --growable-table)');
    }
    const index = table.grow(1);
    table.set(index, $wasmFunction(params, results, fn));
    return index;
  };
  return new $Linear(exports, prefix, () => exports.memory.buffer, fnptr);
}

/**
 * Adopt an initialized Emscripten module (or the promise its `MODULARIZE`
 * factory returns). Its exports carry a leading underscore, its memory is
 * read through `HEAPU8`, and table entries are installed with
 * `addFunction`, so it must be linked with `-sALLOW_TABLE_GROWTH`,
 * `-sWASM_BIGINT`, and `-sEXPORTED_RUNTIME_METHODS=addFunction,HEAPU8`.
 */
export async function $loadEmscripten(module, prefix, symbols) {
  const m = await module;
  const x = {};
  for (const symbol of symbols) x[symbol] = m[`_${symbol}`];
  const fnptr = (params, results, sig, fn) => {
    if (typeof m.addFunction !== 'function') {
      throw new Error('the Emscripten module does not export addFunction (link with -sALLOW_TABLE_GROWTH and -sEXPORTED_RUNTIME_METHODS=addFunction,HEAPU8)');
    }
    return m.addFunction(fn, sig);
  };
  return new $Linear(x, prefix, () => m.HEAPU8.buffer, fnptr);
}
