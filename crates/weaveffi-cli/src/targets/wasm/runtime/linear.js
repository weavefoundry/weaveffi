// The WebAssembly transport: loads a `wasm32-unknown-unknown` build of the
// library and binds each C symbol from its signature in `index.js`
// (`$sigs`), staging arguments in the module's linear memory and reading
// results and errors back. A bound function takes and returns one
// JavaScript value per C value, with the slot kinds of the Node.js
// transport: scalars as booleans and numbers (`bigint` for i64 and u64),
// `opt_<prim>` as the value or `null`, `run` as a `Uint8Array`, `ptr` as a
// handle (an address) or `null`, `cb` as `{ ctx, vtable }` or `null`, and
// the `err`, `ctx`, `out:<kind>`, and `done:<kind>` slots filled in.
// JavaScript functions the module calls (async completions and
// callback-interface vtable methods) are installed in its function table,
// once per signature or interface.
//
// The C ABI of wasm32 (which Rust's `extern "C"` follows): integers of 32
// bits or fewer, `bool`, `size_t`, and pointers are `i32`; `int64_t` and
// `uint64_t` are `i64`; and a struct (a `{prefix}_run`, `{prefix}_opt_*`, or
// callback-interface argument) is passed indirectly, as the `i32` address
// of a copy the caller owns, and returned through a hidden first `i32`
// parameter, the address the callee writes it to (the function then returns
// nothing). The copies live on a small stack the transport carves from
// linear memory; runs from {prefix}_alloc are 8-aligned, but multi-byte
// reads and writes still go through a `DataView` (little-endian, any
// address).
//
// A trap (a producer panic aborts on wasm32) leaves the instance's memory in
// an unknown state, so it poisons the instance: every later call throws a
// code -2 fault instead of running, and releases (`_destroy`, cancel
// tokens, freeing runs) do nothing.

import { $Fault, $messageOf, $release } from './runtime.js';

const $enc = new TextEncoder();
const $dec = new TextDecoder();
const $EMPTY_BYTES = new Uint8Array(0);

/** The size of each arena the transport carves its permanent blocks from. */
const $ARENA = 4096;

/** The size of the stack the struct copies of calls are pushed on. */
const $STACK = 16384;

/** An optional scalar's layout by primitive: [value offset, size]. */
const $OPT = Object.freeze({
  bool: [1, 2],
  i8: [1, 2],
  u8: [1, 2],
  i16: [2, 4],
  u16: [2, 4],
  i32: [4, 8],
  u32: [4, 8],
  f32: [4, 8],
  i64: [8, 16],
  u64: [8, 16],
  f64: [8, 16],
});

/** Whether slot kind `kind` is a struct (passed indirectly). */
function $isStruct(kind) {
  return kind === 'run' || kind === 'cb' || kind.startsWith('opt_');
}

/** The size of struct kind `kind` on wasm32. */
function $structSize(kind) {
  return kind.startsWith('opt_') ? $OPT[kind.slice(4)][1] : 8;
}

/** The wasm value type of a by-value slot kind. */
function $valtype(kind) {
  switch (kind) {
    case 'i64':
    case 'u64':
      return 'i64';
    case 'f32':
      return 'f32';
    case 'f64':
      return 'f64';
    default:
      return 'i32';
  }
}

/** The JavaScript value of the wasm value `r` of a by-value slot kind. */
function $carrier(kind, r) {
  switch (kind) {
    case 'bool':
      return r !== 0;
    case 'i8':
      return (r << 24) >> 24;
    case 'i16':
      return (r << 16) >> 16;
    case 'u8':
      return r & 0xff;
    case 'u16':
      return r & 0xffff;
    case 'u32':
    case 'ctx':
      return r >>> 0;
    case 'u64':
      return BigInt.asUintN(64, r);
    case 'ptr':
      return r >>> 0 || null;
    default:
      return r;
  }
}

/** The wasm value of the JavaScript value `v` of a by-value slot kind. */
function $wasmValue(kind, v) {
  switch (kind) {
    case 'bool':
      return v ? 1 : 0;
    case 'i64':
    case 'u64':
      return BigInt(v);
    case 'ptr':
      return v === null || v === undefined ? 0 : v;
    default:
      return v;
  }
}

/** The zero wasm value a failed callback returns for `kind`. */
function $zero(kind) {
  return kind === 'i64' || kind === 'u64' ? 0n : 0;
}

/**
 * One loaded module: its linear memory, its runtime symbols, and its bound
 * functions. `exports` holds the module's exports by C symbol name;
 * `memory()` returns the current `ArrayBuffer`; `fnptr(params, results,
 * fn)` installs `fn` in the function table and returns its index.
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
    const runtime = ['abi_version', 'alloc', 'run_free', 'error_set', 'error_set_payload', 'error_clear', 'error_free', 'debug_live'];
    for (const symbol of runtime) {
      if (typeof exports[`${prefix}_${symbol}`] !== 'function') {
        throw new Error(`${library}: the WebAssembly module does not export ${prefix}_${symbol}`);
      }
    }
    this.abiVersionFn = this.x[`${prefix}_abi_version`];
    this.allocFn = this.x[`${prefix}_alloc`];
    this.runFreeFn = this.q[`${prefix}_run_free`];
    this.errorClearFn = this.q[`${prefix}_error_clear`];
    this.errorFreeFn = this.q[`${prefix}_error_free`];
    this.errorSetFn = this.x[`${prefix}_error_set`];
    this.errorSetPayloadFn = this.x[`${prefix}_error_set_payload`];
    this.debugLiveFn = this.x[`${prefix}_debug_live`];
    // Permanent blocks (the scratch slots, the stack, and the vtables) come
    // from arenas allocated with {prefix}_alloc and never freed; `arenas`
    // counts them so the leak counter of runs can leave them out.
    this.arenas = 0;
    this.arenaNext = 0;
    this.arenaEnd = 0;
    // The error slot every synchronous call writes, and the item slot every
    // iterator's `next` writes. The library writes them only as a call
    // returns, so a call made from a callback in the middle of another call
    // can reuse them safely.
    const scratch = this.reserve(40);
    this.err = scratch;
    this.item = scratch + 24;
    this.stack = this.reserve($STACK);
    this.sp = this.stack;
    this.completions = new Map();
    this.pending = new Map();
    this.nextPending = 1;
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

  /** Push `size` zeroed bytes (8-aligned) on the call stack. */
  push(size) {
    const at = Math.ceil(this.sp / 8) * 8;
    if (at + size > this.stack + $STACK) {
      throw new RangeError(`${this.library}: the WebAssembly call stack is exhausted (calls nested too deeply)`);
    }
    this.sp = at + size;
    this.bytes().fill(0, at, at + size);
    return at;
  }

  store(prim, at, v) {
    const dv = this.view();
    switch (prim) {
      case 'bool': dv.setUint8(at, v ? 1 : 0); break;
      case 'i8': dv.setInt8(at, v); break;
      case 'u8': dv.setUint8(at, v); break;
      case 'i16': dv.setInt16(at, v, true); break;
      case 'u16': dv.setUint16(at, v, true); break;
      case 'i32': dv.setInt32(at, v, true); break;
      case 'u32': dv.setUint32(at, v, true); break;
      case 'i64': dv.setBigInt64(at, BigInt(v), true); break;
      case 'u64': dv.setBigUint64(at, BigInt(v), true); break;
      case 'f32': dv.setFloat32(at, v, true); break;
      case 'f64': dv.setFloat64(at, v, true); break;
      default: dv.setUint32(at, v === null || v === undefined ? 0 : v, true);
    }
  }

  load(prim, at) {
    const dv = this.view();
    switch (prim) {
      case 'bool': return dv.getUint8(at) !== 0;
      case 'i8': return dv.getInt8(at);
      case 'u8': return dv.getUint8(at);
      case 'i16': return dv.getInt16(at, true);
      case 'u16': return dv.getUint16(at, true);
      case 'i32': return dv.getInt32(at, true);
      case 'u32': return dv.getUint32(at, true);
      case 'i64': return dv.getBigInt64(at, true);
      case 'u64': return dv.getBigUint64(at, true);
      case 'f32': return dv.getFloat32(at, true);
      case 'f64': return dv.getFloat64(at, true);
      default: return dv.getUint32(at, true) || null;
    }
  }

  /** Copy `bytes` into a run from {prefix}_alloc: `[ptr, len]`. */
  stage(bytes) {
    if (bytes.length === 0) return [0, 0];
    const ptr = this.alloc(bytes.length);
    this.bytes().set(bytes, ptr);
    return [ptr, bytes.length];
  }

  /** Release a staged run. */
  freeRun([ptr, len]) {
    if (ptr === 0) return;
    const at = this.push(8);
    this.store('u32', at, ptr);
    this.store('u32', at + 4, len);
    this.runFreeFn(at);
  }

  /** Write struct `kind`'s C value of `v` at `at`, staging a run in `staged`. */
  writeStruct(kind, at, v, staged) {
    if (kind === 'run') {
      const run = this.stage(v);
      if (staged !== null) staged.push(run);
      this.store('u32', at, run[0]);
      this.store('u32', at + 4, run[1]);
    } else if (kind === 'cb') {
      this.store('u32', at, v === null ? 0 : v.ctx);
      this.store('u32', at + 4, v === null ? 0 : v.vtable);
    } else {
      const prim = kind.slice(4);
      this.store('bool', at, v !== null);
      if (v !== null) this.store(prim, at + $OPT[prim][0], v);
    }
  }

  /**
   * The JavaScript value of the struct `kind` at `at`; an `owned` run is
   * released after it is copied.
   */
  readStruct(kind, at, owned) {
    if (kind === 'run') {
      const ptr = this.load('u32', at);
      const len = this.load('u32', at + 4);
      if (ptr === 0 || len === 0) {
        if (owned && ptr !== 0) this.runFreeFn(at);
        return $EMPTY_BYTES;
      }
      const bytes = this.bytes().slice(ptr, ptr + len);
      if (owned) this.runFreeFn(at);
      return bytes;
    }
    const prim = kind.slice(4);
    return this.load('bool', at) ? this.load(prim, at + $OPT[prim][0]) : null;
  }

  /** The value of a C value of `kind` the module handed over at `at`. */
  readOwned(kind, at) {
    return $isStruct(kind) ? this.readStruct(kind, at, true) : this.load(kind, at);
  }

  fault(errPtr) {
    const dv = this.view();
    const code = dv.getInt32(errPtr, true);
    if (code === 0) return null;
    const messagePtr = dv.getUint32(errPtr + 4, true);
    const messageLen = dv.getUint32(errPtr + 8, true);
    const payloadPtr = dv.getUint32(errPtr + 12, true);
    const payloadLen = dv.getUint32(errPtr + 16, true);
    const heap = this.bytes();
    return new $Fault(
      code,
      messageLen === 0 ? '' : $dec.decode(heap.subarray(messagePtr, messagePtr + messageLen)),
      payloadPtr === 0 ? null : heap.slice(payloadPtr, payloadPtr + payloadLen),
    );
  }

  /** Throw the failure the last call wrote to the error slot, if any. */
  check() {
    if (this.view().getInt32(this.err, true) === 0) return;
    const fault = this.fault(this.err);
    this.errorClearFn(this.err);
    throw fault;
  }

  /**
   * Report a callback implementation's exception through `errPtr`: a
   * `$Fault` (the adapter's report of a method declared `throws`) with its
   * code, message, and payload, anything else as code -4 with its message.
   */
  report(errPtr, e) {
    const fault = e instanceof $Fault ? e : new $Fault(-4, $messageOf(e), null);
    const sp = this.sp;
    const text = this.stage($enc.encode(fault.message));
    try {
      this.errorSetFn(errPtr, fault.code, text[0], text[1]);
      if (fault.payload !== null && fault.payload.length > 0) {
        const payload = this.stage(fault.payload);
        try {
          this.errorSetPayloadFn(errPtr, payload[0], payload[1]);
        } finally {
          this.freeRun(payload);
        }
      }
    } finally {
      this.freeRun(text);
      this.sp = sp;
    }
  }

  /** The functions of `sigs` (`{ symbol: [ret, [slot, ...]] }`), bound. */
  bind(sigs) {
    const out = {};
    for (const [symbol, [ret, params]] of Object.entries(sigs)) {
      out[symbol] = this.wrap(symbol, ret, params);
    }
    return out;
  }

  wrap(symbol, ret, params) {
    if (typeof this.x[symbol] !== 'function') {
      throw new Error(`${this.library}: the WebAssembly module does not export ${symbol}`);
    }
    const release = ret === 'void' && !params.some((k) => k === 'err' || k.startsWith('done:'));
    const fn = release ? this.q[symbol] : this.x[symbol];
    const sret = $isStruct(ret);
    const checked = params.includes('err');
    const itemKind = params.find((k) => k.startsWith('out:'))?.slice(4) ?? null;
    const doneKind = params.find((k) => k.startsWith('done:'))?.slice(5) ?? null;
    return (...values) => {
      const sp = this.sp;
      const staged = [];
      try {
        const c = [];
        const retAt = sret ? this.push($structSize(ret)) : 0;
        if (sret) c.push(retAt);
        let doneAt = -1;
        let ctxAt = -1;
        let j = 0;
        for (const kind of params) {
          if (kind === 'err') {
            c.push(this.err);
          } else if (kind.startsWith('out:')) {
            c.push(this.item);
          } else if (kind.startsWith('done:')) {
            doneAt = c.length;
            c.push(0);
          } else if (kind === 'ctx') {
            ctxAt = c.length;
            c.push(0);
          } else if ($isStruct(kind)) {
            const at = this.push($structSize(kind));
            this.writeStruct(kind, at, values[j++], staged);
            c.push(at);
          } else {
            c.push($wasmValue(kind, values[j++]));
          }
        }
        if (doneKind !== null) {
          return this.launch(doneKind, (callback, id) => {
            c[doneAt] = callback;
            c[ctxAt] = id;
            fn(...c);
          });
        }
        const r = fn(...c);
        const v = sret ? this.readStruct(ret, retAt, true) : ret === 'void' ? undefined : $carrier(ret, r);
        if (checked) this.check();
        if (itemKind !== null) return r === 0 ? undefined : this.readOwned(itemKind, this.item);
        return v;
      } finally {
        for (const run of staged) this.freeRun(run);
        this.sp = sp;
      }
    };
  }

  /** The value of a lent callback argument of `kind` (its wasm value `raw`). */
  liftArg(kind, raw) {
    return $isStruct(kind) ? this.readStruct(kind, raw >>> 0, false) : $carrier(kind, raw);
  }

  /**
   * A table function calling `fn`, a callback method (`ret`, `params` as in
   * `$sigs`): arguments arrive as their JavaScript values, the return is
   * lowered (a run is copied into one from {prefix}_alloc, which the module
   * adopts), and an exception is reported through the `err` slot, the method
   * then returning a zero value.
   */
  fn(ret, params, fn) {
    const sret = $isStruct(ret);
    const wasmParams = params.map((k) => ($isStruct(k) ? 'i32' : $valtype(k)));
    if (sret) wasmParams.unshift('i32');
    const results = ret === 'void' || sret ? [] : [$valtype(ret)];
    const errAt = params.indexOf('err');
    const off = sret ? 1 : 0;
    const wrapper = (...raw) => {
      const retAt = sret ? raw[0] >>> 0 : 0;
      try {
        const args = [];
        for (let i = 0; i < params.length; i++) {
          if (i !== errAt) args.push(this.liftArg(params[i], raw[i + off]));
        }
        const r = fn(...args);
        if (sret) {
          this.writeStruct(ret, retAt, r, null);
          return undefined;
        }
        return ret === 'void' ? undefined : $wasmValue(ret, r);
      } catch (e) {
        if (errAt >= 0) this.report(raw[errAt + off] >>> 0, e);
        if (sret) {
          this.bytes().fill(0, retAt, retAt + $structSize(ret));
          return undefined;
        }
        return ret === 'void' ? undefined : $zero(ret);
      }
    };
    return this.fnptr(wasmParams, results, wrapper);
  }

  /**
   * The static vtable of callback interface `name`: the header (`size`,
   * `flags` 0, and `free`, which releases the implementation), then one
   * table entry per method (`methods` lists each as `[ret, params, fn]`).
   * Built on first use. `flags` is 0: methods may be called from any thread
   * (there is only one).
   */
  vtable(name, methods) {
    let ptr = this.vtables.get(name);
    if (ptr !== undefined) return ptr;
    const entries = [this.fn('void', ['ctx'], (ctx) => $release(ctx))];
    for (const [ret, params, fn] of methods) entries.push(this.fn(ret, params, fn));
    const size = 8 + entries.length * 4;
    ptr = this.reserve(size);
    this.store('u32', ptr, size);
    this.store('u32', ptr + 4, 0);
    entries.forEach((entry, i) => this.store('u32', ptr + 8 + i * 4, entry));
    this.vtables.set(name, ptr);
    return ptr;
  }

  /**
   * Launch an async call. `call(callback, id)` invokes the launcher; the
   * completion (one table entry per result kind, shared by every call)
   * settles the returned promise. On this single-threaded target the
   * completion normally fires before the launcher returns.
   */
  launch(kind, call) {
    let callback = this.completions.get(kind);
    if (callback === undefined) {
      const params = ['i32', 'i32'];
      if (kind !== '') params.push($isStruct(kind) ? 'i32' : $valtype(kind));
      callback = this.fnptr(params, [], (id, errPtr, result) => this.complete(kind, id, errPtr, result));
      this.completions.set(kind, callback);
    }
    const id = this.nextPending++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      try {
        call(callback, id);
      } catch (e) {
        this.pending.delete(id);
        reject(e);
      }
    });
  }

  complete(kind, id, errPtr, result) {
    const pending = this.pending.get(id >>> 0);
    this.pending.delete(id >>> 0);
    let value;
    let failure = null;
    try {
      if (kind !== '') value = $isStruct(kind) ? this.readStruct(kind, result >>> 0, true) : $carrier(kind, result);
    } catch (e) {
      failure = e;
    }
    if (errPtr !== 0) {
      failure = this.fault(errPtr >>> 0);
      this.errorFreeFn(errPtr);
    }
    if (pending === undefined) return;
    if (failure !== null) {
      pending.reject(failure);
    } else {
      pending.resolve(value);
    }
  }

  /** The handle of an object token read from a value buffer. */
  token(t) {
    return Number(t);
  }

  /** The object token of a handle, for a value buffer. */
  untoken(h) {
    return BigInt(h);
  }

  abiVersion() {
    return this.abiVersionFn() >>> 0;
  }

  /** Whether the module exports `symbol` (a contract function). */
  has(symbol) {
    return typeof this.x[symbol] === 'function';
  }

  /** The hash contract function `symbol` returns. */
  contract(symbol) {
    return BigInt.asUintN(64, this.x[symbol]());
  }

  /**
   * The module's live-resource counter of `kind`, leaving out the
   * transport's own arenas from the count of runs (kind 4).
   */
  live(kind) {
    const n = BigInt.asUintN(64, this.debugLiveFn(kind));
    return kind === 4 && n > 0n ? n - BigInt(this.arenas) : n;
  }
}

/** A stand-in for the loaded module until `init()` has finished. */
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

const $valtypes = { i32: 0x7f, i64: 0x7e, f32: 0x7d, f64: 0x7c };

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
  const type = [0x60, ...$leb(params.length), ...params.map((t) => $valtypes[t])];
  type.push(...$leb(results.length), ...results.map((t) => $valtypes[t]));
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
