// The Node.js transport: loads the native library with koffi and binds each
// C symbol from its signature in `index.js` (`$sigs`). A bound function
// takes and returns one JavaScript value per C value (see the slot kinds
// below), checks the error slot after the call, and throws the reported
// failure as a `$Fault`. JavaScript functions the library calls (async
// completions and callback-interface vtable methods) are registered with
// koffi once per signature or interface and never unregistered, so the
// number of registrations is bounded however many calls or implementations
// are live. koffi runs a callback the library makes from one of its own
// threads on the JavaScript thread, blocking that thread until it returns.
//
// Slot kinds (the strings in `$sigs`), and the JavaScript value of each:
//
//   bool, i8 ... f64   a boolean or number (`bigint` for i64 and u64)
//   opt_<prim>         the value, or `null` for none
//   run                a `Uint8Array`: lent for a parameter, a copy for a
//                      return (whose run is then released), a copy of a
//                      borrowed callback argument, and copied into a run
//                      from {prefix}_alloc for a callback return
//   ptr                an object, iterator, or cancel token handle (`bigint`),
//                      or `null`
//   cb                 a callback-interface argument, `{ ctx, vtable }` or
//                      `null`
//   ctx                a callback's context (a number); filled in for an
//                      async launcher's
//   err                the error slot, filled in
//   out:<kind>         an iterator's item slot, filled in; the function then
//                      returns the item, or `undefined` once exhausted
//   done:<kind>        an async completion, filled in; the function then
//                      returns a promise of the result

import koffi from 'koffi';
import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { $Fault, $messageOf, $release } from './runtime.js';

const $SCALARS = Object.freeze({
  bool: 'bool',
  i8: 'int8_t',
  i16: 'int16_t',
  i32: 'int32_t',
  i64: 'int64_t',
  u8: 'uint8_t',
  u16: 'uint16_t',
  u32: 'uint32_t',
  u64: 'uint64_t',
  f32: 'float',
  f64: 'double',
});

const $WIDE = new Set(['i64', 'u64']);
const $EMPTY_BYTES = new Uint8Array(0);
const $decoder = new TextDecoder();

/** The library's file name on this platform. */
function $fileName(library) {
  switch (process.platform) {
    case 'darwin':
      return `lib${library}.dylib`;
    case 'win32':
      return `${library}.dll`;
    default:
      return `lib${library}.so`;
  }
}

/**
 * Load the native library. The first of these that exists wins: the file
 * (or the directory holding it) named by the `env` environment variable,
 * the copy in the `{pkg}-{platform}-{arch}` package npm installed for this
 * platform, a copy next to `index.js` (where `weaveffi generate --dev` puts it), and
 * finally the bare file name, which the system's library search resolves.
 */
export function $loadNative({ name, library, prefix, env, pkg, base }) {
  const file = $fileName(library);
  const here = dirname(fileURLToPath(base));
  const tried = [];
  const candidates = [];
  const fromEnv = process.env[env];
  if (fromEnv) {
    candidates.push(existsSync(join(fromEnv, file)) ? join(fromEnv, file) : fromEnv);
  } else {
    try {
      const require = createRequire(base);
      const dir = dirname(require.resolve(`${pkg}-${process.platform}-${process.arch}/package.json`));
      candidates.push(join(dir, file));
    } catch {
      // No per-platform package: a development or system install.
    }
    candidates.push(join(here, file));
    candidates.push(file);
  }
  for (const candidate of candidates) {
    if (candidate !== file && !existsSync(candidate)) {
      tried.push(candidate);
      continue;
    }
    try {
      return new $Native(name, koffi.load(candidate), prefix);
    } catch (e) {
      tried.push(`${candidate} (${e.message})`);
    }
  }
  throw new Error(
    `${name}: the native library ${file} was not found (tried ${tried.join(', ')}); ` +
      `install the package for this platform, or set ${env} to the library's path`,
  );
}

/** One loaded library and its bound functions. */
export class $Native {
  constructor(name, lib, prefix) {
    this.name = name;
    this.lib = lib;
    this.prefix = prefix;
    this.ptrSize = koffi.sizeof('void *');
    this.Run = koffi.struct({ ptr: 'void *', len: 'size_t' });
    this.Callback = koffi.struct({ ctx: 'void *', vtable: 'void *' });
    this.Error = koffi.struct({
      code: 'int32_t',
      message_ptr: 'void *',
      message_len: 'size_t',
      payload_ptr: 'void *',
      payload_len: 'size_t',
    });
    this.opts = new Map();
    const rt = (symbol, ret, params) => {
      try {
        return lib.func(`${prefix}_${symbol}`, ret, params);
      } catch {
        throw new Error(`${name}: the native library does not export ${prefix}_${symbol}`);
      }
    };
    this.abiVersionFn = rt('abi_version', 'uint32_t', []);
    this.allocFn = rt('alloc', 'void *', ['size_t']);
    this.runFreeFn = rt('run_free', 'void', [this.Run]);
    this.errorSetFn = rt('error_set', 'void', ['void *', 'int32_t', 'void *', 'size_t']);
    this.errorSetPayloadFn = rt('error_set_payload', 'void', ['void *', 'void *', 'size_t']);
    this.errorClearFn = rt('error_clear', 'void', ['void *']);
    this.errorFreeFn = rt('error_free', 'void', ['void *']);
    this.debugLiveFn = rt('debug_live', 'uint64_t', ['int32_t']);
    // The error slot every synchronous call writes, and the item slot every
    // iterator's `next` writes. A callback can call back into the library in
    // the middle of a call: the library writes both only as a call returns,
    // so the nested call reusing them is safe.
    this.err = koffi.alloc(this.Error, 1);
    this.item = koffi.alloc('uint64_t', 2);
    this.contracts = new Map();
    this.completions = new Map();
    this.pending = new Map();
    this.nextPending = 1;
    this.keepAlive = null;
    this.vtables = new Map();
  }

  /** The koffi type of slot kind `kind` (every special slot is a pointer). */
  type(kind) {
    if (kind in $SCALARS) return $SCALARS[kind];
    if (kind.startsWith('opt_')) return this.opt(kind.slice(4));
    switch (kind) {
      case 'void':
        return 'void';
      case 'run':
        return this.Run;
      case 'cb':
        return this.Callback;
      default:
        return 'void *';
    }
  }

  opt(prim) {
    let t = this.opts.get(prim);
    if (t === undefined) {
      t = koffi.struct({ has: 'bool', value: $SCALARS[prim] });
      this.opts.set(prim, t);
    }
    return t;
  }

  /** The C value of a parameter's JavaScript value. */
  lower(kind) {
    if (kind.startsWith('opt_')) {
      const none = this.zero(kind);
      return (v) => (v === null ? none : { has: true, value: v });
    }
    switch (kind) {
      case 'run':
        return (b) => (b.length === 0 ? { ptr: null, len: 0 } : { ptr: b, len: b.length });
      case 'cb':
        return (c) => (c === null ? { ctx: null, vtable: null } : { ctx: BigInt(c.ctx), vtable: c.vtable });
      default:
        return null;
    }
  }

  /**
   * The JavaScript value of a C value the library hands over (`owned`: a
   * return, async result, or iterator item, whose run is released) or
   * lends (a callback argument).
   */
  lift(kind, owned) {
    if ($WIDE.has(kind)) return (v) => BigInt(v);
    if (kind.startsWith('opt_')) {
      const wide = $WIDE.has(kind.slice(4));
      return (o) => (o.has ? (wide ? BigInt(o.value) : o.value) : null);
    }
    switch (kind) {
      case 'run':
        return owned ? (r) => this.take(r) : (r) => this.peek(r);
      case 'ptr':
        return (p) => (p === 0n || p === undefined ? null : p);
      case 'ctx':
        return (p) => Number(p);
      default:
        return null;
    }
  }

  /** A copy of a lent run's bytes. */
  peek(run) {
    const len = Number(run.len);
    if (len === 0 || run.ptr === null) return $EMPTY_BYTES;
    return new Uint8Array(koffi.view(run.ptr, len)).slice();
  }

  /** A copy of an owned run's bytes, releasing the run. */
  take(run) {
    if (run.ptr === null) return $EMPTY_BYTES;
    const bytes = this.peek(run);
    this.runFreeFn(run);
    return bytes;
  }

  /** A run from {prefix}_alloc holding `bytes`, which the library adopts. */
  give(bytes) {
    if (bytes.length === 0) return { ptr: null, len: 0 };
    const ptr = this.allocFn(bytes.length);
    if (ptr === null) throw new RangeError(`${this.name}: could not allocate ${bytes.length} bytes`);
    new Uint8Array(koffi.view(ptr, bytes.length)).set(bytes);
    return { ptr, len: bytes.length };
  }

  /** The fault an error struct holds, or `null` for success. */
  faultOf(e) {
    if (e.code === 0) return null;
    const messageLen = Number(e.message_len);
    const payloadLen = Number(e.payload_len);
    const message =
      messageLen === 0 || e.message_ptr === null
        ? ''
        : $decoder.decode(new Uint8Array(koffi.view(e.message_ptr, messageLen)));
    const payload =
      e.payload_ptr === null ? null : new Uint8Array(koffi.view(e.payload_ptr, payloadLen)).slice();
    return new $Fault(e.code, message, payload);
  }

  /** Throw the failure the last call wrote to the error slot, if any. */
  check() {
    if (koffi.decode.int32(this.err) === 0) return;
    const fault = this.faultOf(koffi.decode(this.err, this.Error));
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
    const message = new TextEncoder().encode(fault.message);
    this.errorSetFn(errPtr, fault.code, message.length === 0 ? null : message, message.length);
    if (fault.payload !== null && fault.payload.length > 0) {
      this.errorSetPayloadFn(errPtr, fault.payload, fault.payload.length);
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
    let fn;
    try {
      fn = this.lib.func(symbol, this.type(ret), params.map((k) => this.type(k)));
    } catch {
      throw new Error(`${this.name}: the native library does not export ${symbol}`);
    }
    const n = params.length;
    const user = [];
    const fixed = new Array(n);
    let checked = false;
    let item = null;
    let done = null;
    let ctxAt = -1;
    params.forEach((kind, i) => {
      if (kind === 'err') {
        fixed[i] = this.err;
        checked = true;
      } else if (kind.startsWith('out:')) {
        fixed[i] = this.item;
        const k = kind.slice(4);
        item = [this.type(k), this.lift(k, true)];
      } else if (kind.startsWith('done:')) {
        done = kind.slice(5);
        fixed[i] = this.completion(done);
      } else if (kind === 'ctx') {
        ctxAt = i;
      } else {
        user.push([i, this.lower(kind)]);
      }
    });
    const lift = ret === 'void' ? null : this.lift(ret, true);
    const args = (values) => {
      const c = fixed.slice();
      for (let j = 0; j < user.length; j++) {
        const [i, lower] = user[j];
        c[i] = lower === null ? values[j] : lower(values[j]);
      }
      return c;
    };
    if (done !== null) {
      return (...values) => {
        const c = args(values);
        return this.launch((id) => {
          c[ctxAt] = BigInt(id);
          fn(...c);
        });
      };
    }
    if (item !== null) {
      const [type, liftItem] = item;
      return (...values) => {
        const has = fn(...args(values));
        this.check();
        if (has === 0) return undefined;
        const v = koffi.decode(this.item, type);
        return liftItem === null ? v : liftItem(v);
      };
    }
    return (...values) => {
      const r = fn(...args(values));
      const v = lift === null ? r : lift(r);
      if (checked) this.check();
      return ret === 'void' ? undefined : v;
    };
  }

  /** The registered completion of an async call whose result is `kind`. */
  completion(kind) {
    let ptr = this.completions.get(kind);
    if (ptr !== undefined) return ptr;
    const slots = ['void *', 'void *'];
    if (kind !== '') slots.push(this.type(kind));
    const proto = koffi.proto('void', slots);
    const lift = kind === '' ? null : this.lift(kind, true);
    ptr = koffi.register((ctx, err, result) => {
      let value;
      let failure = null;
      try {
        value = lift === null ? result : lift(result);
      } catch (e) {
        failure = e;
      }
      if (err !== null) {
        failure = this.faultOf(koffi.decode(err, this.Error));
        this.errorFreeFn(err);
      }
      this.settle(Number(ctx), failure, kind === '' ? undefined : value);
    }, koffi.pointer(proto));
    this.completions.set(kind, ptr);
    return ptr;
  }

  /**
   * Launch an async call: `call(id)` invokes the launcher with context
   * `id`, and the completion settles the returned promise. The event loop
   * is kept alive while any call is pending, since the completion may
   * arrive from a library thread.
   */
  launch(call) {
    const id = this.nextPending++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      if (this.keepAlive === null) this.keepAlive = setInterval(() => {}, 2 ** 31 - 1);
      try {
        call(id);
      } catch (e) {
        this.settle(id, e, undefined);
      }
    });
  }

  settle(id, failure, value) {
    const pending = this.pending.get(id);
    if (pending === undefined) return;
    this.pending.delete(id);
    if (this.pending.size === 0 && this.keepAlive !== null) {
      clearInterval(this.keepAlive);
      this.keepAlive = null;
    }
    if (failure !== null) {
      pending.reject(failure);
    } else {
      pending.resolve(value);
    }
  }

  /**
   * A C function pointer to `fn`, a callback method (`ret`, `params` as in
   * `$sigs`): arguments arrive as their JavaScript values, the return is
   * lowered, and an exception is reported through the `err` slot, the
   * method then returning a zero value.
   */
  fn(ret, params, fn) {
    const proto = koffi.proto(this.type(ret), params.map((k) => this.type(k)));
    const errAt = params.indexOf('err');
    const lifts = params.map((k) => (k === 'err' ? null : this.lift(k, false)));
    const lower = ret === 'void' ? null : this.callbackReturn(ret);
    const zero = this.zero(ret);
    const wrapper = (...c) => {
      try {
        const args = [];
        for (let i = 0; i < c.length; i++) {
          if (i === errAt) continue;
          args.push(lifts[i] === null ? c[i] : lifts[i](c[i]));
        }
        const r = fn(...args);
        return lower === null ? r : lower(r);
      } catch (e) {
        if (errAt >= 0) this.report(c[errAt], e);
        return zero;
      }
    };
    return koffi.register(wrapper, koffi.pointer(proto));
  }

  callbackReturn(kind) {
    if (kind === 'run') return (b) => this.give(b);
    const lower = this.lower(kind);
    return lower;
  }

  zero(kind) {
    if (kind === 'void') return undefined;
    if (kind === 'bool') return false;
    if ($WIDE.has(kind)) return 0n;
    if (kind in $SCALARS) return 0;
    if (kind.startsWith('opt_')) return { has: false, value: kind === 'opt_bool' ? false : 0 };
    if (kind === 'run') return { ptr: null, len: 0 };
    return null;
  }

  /**
   * The static vtable of callback interface `name`: the header (`size`,
   * `flags` 0, and `free`, which releases the implementation), then one
   * entry per method (`methods` lists each as `[ret, params, fn]`). Built
   * once and kept for the life of the process.
   */
  vtable(name, methods) {
    let ptr = this.vtables.get(name);
    if (ptr !== undefined) return ptr;
    const entries = [this.fn('void', ['ctx'], (ctx) => $release(ctx))];
    for (const [ret, params, fn] of methods) entries.push(this.fn(ret, params, fn));
    const size = 8 + entries.length * this.ptrSize;
    ptr = koffi.alloc('void *', Math.ceil(size / this.ptrSize));
    koffi.encode(ptr, 0, 'uint32_t', size);
    koffi.encode(ptr, 4, 'uint32_t', 0);
    entries.forEach((entry, i) => koffi.encode(ptr, 8 + i * this.ptrSize, 'void *', entry));
    this.vtables.set(name, ptr);
    return ptr;
  }

  /** The handle of an object token read from a value buffer. */
  token(t) {
    return t;
  }

  /** The object token of a handle, for a value buffer. */
  untoken(h) {
    return BigInt(h);
  }

  abiVersion() {
    return this.abiVersionFn();
  }

  /** Whether the library exports `symbol` (a contract function). */
  has(symbol) {
    if (this.contracts.has(symbol)) return true;
    try {
      this.contracts.set(symbol, this.lib.func(symbol, 'uint64_t', []));
      return true;
    } catch {
      return false;
    }
  }

  /** The hash contract function `symbol` returns. */
  contract(symbol) {
    return this.has(symbol) ? BigInt(this.contracts.get(symbol)()) : null;
  }

  /** The library's live-resource counter of `kind`. */
  live(kind) {
    return BigInt(this.debugLiveFn(kind));
  }
}
