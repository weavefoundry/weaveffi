// Conformance consumer: kvstore sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the generated ESM bindings (loadWeaveffiWasm) against the real
// producer compiled to wasm. Exercises the ABI 2 surface end to end: the
// reference-counted `Store` interface class (static `open` factory, instance
// methods, idempotent `close()`, use-after-close trap, `Symbol.dispose`), the
// typed `KvError` domain (subclasses with stable codes thrown by `throws`
// wrappers, including through a rejected Promise), objects travelling every
// way the schema allows (`share()` returning a wrapper to the same object,
// `fork()` returning a new one, `Store?` in and out of `larger`, a record that
// carries an object and a nullable object from `describe`, a list of objects
// from `openMany`, a list of objects and an optional record holding an object
// into `totalCount`), records decoded from value buffers into plain objects
// (`Entry` with its bytes, optional-scalar, list, and map fields; `Stats`
// from the kv.stats submodule), buffered optional parameters (`i64?` TTL,
// `string?` prefix), a lazy iterator-backed string stream (listKeys), the
// Promise-backed `compact` (the wasm32 default spawner drives the future
// inline), and the `EvictionListener` callback interface implemented as a
// JS object reached through function-table trampolines: `Deleted` and
// `Expired` evictions, detaching by returning false, replacement, and a
// listener that throws, whose abort reaches JS as a translated -4 error while
// the store stays usable.
//
// Inputs come from the harness:
//   WV_WASM: path to the compiled kvstore.wasm
//   WV_JS:   path to the generated weaveffi_wasm.js (ESM)
// Run with: node --experimental-wasm-type-reflection (for WebAssembly.Function).

import fs from 'fs';

const WASM = process.env.WV_WASM;
const JS = process.env.WV_JS;
if (!WASM || !JS) {
  console.error('WV_WASM and WV_JS must be set');
  process.exit(2);
}

// Node has no file:// fetch; shim it so the generated loader can read the .wasm.
globalThis.fetch = async (url) => ({ arrayBuffer: async () => fs.readFileSync(url) });

const mod = await import(JS);
const api = await mod.loadWeaveffiWasm(WASM);

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}

const { WeaveFFIError, KvError, KeyNotFound, Expired, StoreFull, IoError, EntryKind, EvictionReason } = mod;
const { Store } = api.kv;
const { getStats } = api.kv.stats;
expect(typeof Store === 'function', 'Store class exposed on api.kv');
expect(typeof getStats === 'function', 'kv.stats.getStats exposed as a nested namespace');
expect(typeof WeaveFFIError === 'function' && typeof KvError === 'function', 'error classes exported');

// The error domain: a hierarchy with stable codes, reachable both as named
// exports and as statics on the domain class.
expect(Object.getPrototypeOf(KvError) === WeaveFFIError, 'KvError extends WeaveFFIError');
expect(KeyNotFound.CODE === 1001 && KvError.KeyNotFound === KeyNotFound, 'KeyNotFound code 1001');
expect(Expired.CODE === 1002 && KvError.Expired === Expired, 'Expired code 1002');
expect(StoreFull.CODE === 1003 && KvError.StoreFull === StoreFull, 'StoreFull code 1003');
expect(IoError.CODE === 1004 && KvError.IoError === IoError, 'IoError code 1004');
const probe = new KeyNotFound();
expect(probe instanceof KvError && probe instanceof WeaveFFIError && probe instanceof Error, 'error prototype chain');
expect(probe.code === 1001 && probe.message === 'WeaveFFI error 1001: key not found', `default message from the schema doc (got ${JSON.stringify(probe.message)})`);
expect(EntryKind.Volatile === 0 && EntryKind.Persistent === 1 && EntryKind.Encrypted === 2, 'EntryKind values');
expect(EvictionReason.Deleted === 0 && EvictionReason.Expired === 1, 'EvictionReason values');
expect(Object.isFrozen(EntryKind) && Object.isFrozen(EvictionReason), 'enums are frozen');

// Fallible factory: an empty path is the typed IoError.
let openErr = null;
try {
  Store.open('');
} catch (e) {
  openErr = e;
}
expect(openErr instanceof IoError, `Store.open('') throws IoError (got ${openErr && openErr.constructor.name})`);
expect(openErr && openErr.code === 1004 && openErr instanceof KvError, 'IoError has code 1004 and is a KvError');
expect(openErr && openErr.message === 'WeaveFFI error 1004: I/O failure', `IoError carries the code and schema message (got ${JSON.stringify(openErr && openErr.message)})`);

const store = Store.open('/tmp/conformance.db');
expect(store instanceof Store, 'open returns a Store');
expect(store._handle > 0, 'open adopts a non-null handle');
expect(Store.defaultCapacity() === 1000000n, `defaultCapacity is 1000000n (got ${Store.defaultCapacity()})`);
expect(store.count() === 0n, 'fresh store is empty');

// put / get with every Entry field decoded.
const enc = new TextEncoder();
expect(store.put('alpha', enc.encode('one'), EntryKind.Persistent, null) === true, 'put alpha');
expect(store.put('beta', enc.encode('two'), EntryKind.Volatile, 3600n) === true, 'put beta with TTL');
expect(store.put('alpha', enc.encode('uno'), EntryKind.Persistent, undefined) === true, 'replace alpha');
expect(store.count() === 2n, `count is 2n (got ${store.count()})`);
const alpha = store.get('alpha');
expect(alpha !== null && typeof alpha === 'object', 'get returns a plain record');
expect(typeof alpha.id === 'bigint' && alpha.id === 3n, `entry id is the third generated id (got ${alpha.id})`);
expect(alpha.key === 'alpha', 'entry key');
expect(alpha.value instanceof Uint8Array && new TextDecoder().decode(alpha.value) === 'uno', 'entry value bytes');
expect(typeof alpha.created_at === 'bigint' && alpha.created_at > 0n, 'created_at populated');
expect(alpha.expires_at === null, 'no TTL decodes as null expires_at');
expect(Array.isArray(alpha.tags) && alpha.tags.length === 0, 'tags is an empty list');
expect(alpha.metadata !== null && typeof alpha.metadata === 'object' && Object.keys(alpha.metadata).length === 0, 'metadata is an empty map');
const beta = store.get('beta');
expect(beta.expires_at !== null && beta.expires_at === beta.created_at + 3600n, `TTL decodes into expires_at (got ${beta.expires_at})`);
expect(beta.id === 2n, 'beta kept its id');

// Typed errors from a method: missing key, then the same via delete's bool.
let notFound = null;
try {
  store.get('missing');
} catch (e) {
  notFound = e;
}
expect(notFound instanceof KeyNotFound && notFound.code === 1001, `get(missing) throws KeyNotFound (got ${notFound && notFound.constructor.name})`);
expect(notFound && notFound.message.endsWith('key not found'), `KeyNotFound carries the schema message (got ${JSON.stringify(notFound && notFound.message)})`);
expect(store.delete('missing') === false, 'delete(missing) is false, not an error');

// Iterators: sorted, prefix-filtered, lazy, with early exit.
expect(store.put('alpine', enc.encode('x'), EntryKind.Persistent, null), 'put alpine');
expect([...store.listKeys(null)].join(',') === 'alpha,alpine,beta', `listKeys(null) sorted (got ${[...store.listKeys(null)]})`);
expect([...store.listKeys(undefined)].join(',') === 'alpha,alpine,beta', 'listKeys(undefined) is the same as null');
expect([...store.listKeys('alp')].join(',') === 'alpha,alpine', `listKeys('alp') (got ${[...store.listKeys('alp')]})`);
expect([...store.listKeys('zzz')].length === 0, 'listKeys with no match is empty');
const keysIt = store.listKeys(null)[Symbol.iterator]();
expect(keysIt.next().value === 'alpha', 'lazy first key');
keysIt.return();
expect(keysIt.next().done === true, 'iterator done after return()');
let stepped = 0;
for (const _k of store.listKeys(null)) {
  stepped++;
  if (stepped === 2) break;
}
expect(stepped === 2, 'for...of break');

// Stats from the nested submodule take the object as a borrowed parameter.
const stats = getStats(store);
expect(stats.total_entries === 3n, `total_entries 3n (got ${stats.total_entries})`);
expect(stats.total_bytes === 7n, `total_bytes 7n (got ${stats.total_bytes})`);
expect(stats.expired_entries === 0n, 'no expired entries');

// share(): a second wrapper to the same producer object. fork(): a copy.
const shared = store.share();
expect(shared instanceof Store && shared !== store, 'share returns a distinct wrapper');
expect(shared._handle === store._handle, 'share points at the same object');
expect(shared.count() === 3n, 'shared sees the same entries');
expect(shared.put('gamma', enc.encode('g'), EntryKind.Volatile, null), 'put through shared');
expect(store.count() === 4n, 'original observes the put through shared');
const forked = store.fork();
expect(forked instanceof Store && forked._handle !== store._handle, 'fork returns a new object');
expect(forked.count() === 4n, 'fork copies live entries');
expect(forked.put('delta', enc.encode('d'), EntryKind.Volatile, null), 'put into fork');
expect(forked.count() === 5n && store.count() === 4n, 'fork is independent');
expect(forked.get('gamma').id === store.get('gamma').id, 'fork preserved ids');
shared.close();
shared.close();
expect(shared._handle === 0, 'closed shared wrapper is zeroed');
expect(store.count() === 4n, 'original alive after the shared wrapper is closed');

// Nullable objects both ways.
const empty = Store.open('/tmp/empty.db');
expect(empty.larger(null) === null, 'larger(null) on an empty store is null');
expect(empty.larger(undefined) === null, 'larger(undefined) same as null');
const bigger = store.larger(forked);
expect(bigger instanceof Store && bigger._handle === forked._handle, 'larger returns the other when it has more');
const own = store.larger(empty);
expect(own instanceof Store && own._handle === store._handle, 'larger returns self when it has more');
const alone = store.larger(null);
expect(alone instanceof Store && alone._handle === store._handle, 'larger(null) returns self when non-empty');
expect(bigger.count() === 5n && own.count() === 4n, 'returned wrappers are usable');
bigger.close();
own.close();
alone.close();
expect(store.count() === 4n && forked.count() === 5n, 'closing returned wrappers keeps the originals alive');

// A record carrying an object and a nullable object.
const info = store.describe('primary', forked);
expect(info.label === 'primary', 'describe label');
expect(info.store instanceof Store && info.store._handle === store._handle, 'record.store is a wrapper to self');
expect(info.mirror instanceof Store && info.mirror._handle === forked._handle, 'record.mirror is the passed object');
expect(info.count === 4n, `record.count (got ${info.count})`);
const infoNoMirror = empty.describe('secondary', null);
expect(infoNoMirror.mirror === null, 'absent mirror decodes as null');
expect(infoNoMirror.store._handle === empty._handle && infoNoMirror.count === 0n, 'secondary record');

// A list of objects out, a list of objects and an optional record in.
const many = Store.openMany(['/tmp/a.db', '/tmp/b.db', '/tmp/c.db']);
expect(Array.isArray(many) && many.length === 3, `openMany returns three stores (got ${many && many.length})`);
expect(many.every((s) => s instanceof Store && s._handle > 0), 'each opened store is a live wrapper');
expect(new Set(many.map((s) => s._handle)).size === 3, 'opened stores are distinct objects');
expect(many[0].put('k', enc.encode('v'), EntryKind.Persistent, null), 'put into many[0]');
expect(many[1].put('k', enc.encode('v'), EntryKind.Persistent, null), 'put into many[1]');
expect(Store.totalCount(many, null) === 2n, `totalCount(many, null) is 2n (got ${Store.totalCount(many, null)})`);
expect(Store.totalCount([], null) === 0n, 'totalCount of nothing');
expect(Store.totalCount([store, forked], info) === 13n, `totalCount with an extra record (got ${Store.totalCount([store, forked], info)})`);
expect(Store.totalCount([store, forked], { label: 'x', store: empty, mirror: null, count: 0 }) === 9n, 'extra record with a null mirror and a number count');
let openManyErr = null;
try {
  Store.openMany(['/tmp/ok.db', '']);
} catch (e) {
  openManyErr = e;
}
expect(openManyErr instanceof IoError, 'openMany propagates the typed error');
expect(store.count() === 4n && forked.count() === 5n && empty.count() === 0n, 'stores alive after being marshalled as parameters');
for (const s of many) s.close();
info.store.close();
info.mirror.close();
infoNoMirror.store.close();
expect(store.count() === 4n && forked.count() === 5n && empty.count() === 0n, 'stores alive after closing every borrowed wrapper');

// Eviction listener: a JS object with `onEvict(entry, reason) -> bool`.
class Listener {
  constructor(name) {
    this.name = name;
    this.evictions = [];
    this.keepGoing = true;
    this.throwOn = null;
  }
  onEvict(entry, reason) {
    this.evictions.push({ key: entry.key, reason, id: entry.id });
    if (entry.key === this.throwOn) {
      throw new Error('listener ' + this.name + ' refuses ' + entry.key);
    }
    return this.keepGoing;
  }
}
const listener = new Listener('first');
store.setEvictionListener(listener);
expect(store.delete('alpine') === true, 'delete alpine');
expect(listener.evictions.length === 1, 'delete notified the listener');
expect(listener.evictions[0].key === 'alpine' && listener.evictions[0].reason === EvictionReason.Deleted, 'Deleted eviction carries the entry');
expect(typeof listener.evictions[0].id === 'bigint', 'entry inside the callback is fully decoded');
// An already-expired entry is evicted on read: Expired error plus notification.
expect(store.put('stale', enc.encode('old'), EntryKind.Volatile, -1n), 'put stale with negative TTL');
expect(getStats(store).expired_entries === 1n, 'stats count the expired entry before eviction');
let expired = null;
try {
  store.get('stale');
} catch (e) {
  expired = e;
}
expect(expired instanceof Expired && expired.code === 1002, `get(stale) throws Expired (got ${expired && expired.constructor.name})`);
expect(listener.evictions.length === 2 && listener.evictions[1].key === 'stale' && listener.evictions[1].reason === EvictionReason.Expired, 'Expired eviction notified');
expect(store.count() === 3n, 'expired entry evicted');
// Returning false detaches the listener (the producer frees it).
listener.keepGoing = false;
expect(store.delete('gamma') === true, 'delete gamma');
expect(listener.evictions.length === 3, 'listener saw the eviction that detached it');
expect(store.put('tmp', enc.encode('t'), EntryKind.Volatile, null) && store.delete('tmp'), 'delete after detach');
expect(listener.evictions.length === 3, 'detached listener no longer notified');
// Replacement: the second listener takes over.
const second = new Listener('second');
const third = new Listener('third');
store.setEvictionListener(second);
store.setEvictionListener(third);
expect(store.put('tmp2', enc.encode('t'), EntryKind.Volatile, null) && store.delete('tmp2'), 'delete after replacement');
expect(second.evictions.length === 0 && third.evictions.length === 1, 'only the current listener is notified');
// A plain object literal works too, and clearEvictionListener detaches it.
let literalHits = 0;
store.setEvictionListener({ onEvict: () => { literalHits++; return true; } });
expect(store.put('tmp3', enc.encode('t'), EntryKind.Volatile, null) && store.delete('tmp3'), 'delete with literal listener');
expect(literalHits === 1, 'object-literal listener notified');
store.clearEvictionListener();
expect(store.put('tmp4', enc.encode('t'), EntryKind.Volatile, null) && store.delete('tmp4'), 'delete after clear');
expect(literalHits === 1, 'cleared listener no longer notified');
store.clearEvictionListener();

// A throwing listener: no unwinding on wasm32, so the runtime records the
// failure, `delete` runs to completion, and the thunk reports the foreign
// error (-4) carrying the JS message instead of the result. The entry was
// already removed and the store keeps working.
const thrower = new Listener('thrower');
thrower.throwOn = 'doomed';
store.setEvictionListener(thrower);
expect(store.put('doomed', enc.encode('x'), EntryKind.Volatile, null), 'put doomed');
let foreign = null;
try {
  store.delete('doomed');
} catch (e) {
  foreign = e;
}
expect(foreign instanceof WeaveFFIError, `throwing listener surfaces a WeaveFFIError (got ${foreign && foreign.constructor.name})`);
expect(!(foreign instanceof WebAssembly.RuntimeError) && !(foreign instanceof KvError), 'foreign error is neither a trap nor a domain error');
expect(foreign && foreign.code === -4, `foreign error code is -4 (got ${foreign && foreign.code})`);
expect(foreign && foreign.message.includes('listener thrower refuses doomed'), `foreign error carries the JS message (got ${JSON.stringify(foreign && foreign.message)})`);
expect(thrower.evictions.length === 1, 'listener ran once');
expect(store.count() === 2n, `store intact after foreign error (got ${store.count()})`);
let afterNotFound = null;
try {
  store.get('doomed');
} catch (e) {
  afterNotFound = e;
}
expect(afterNotFound instanceof KeyNotFound, 'entry was removed before the listener threw');
expect(store.put('again', enc.encode('y'), EntryKind.Persistent, null) && store.get('again').key === 'again', 'store usable after foreign error');
// Without unwinding the producer observed `onEvict`'s default return (false,
// "detach me") after the throw, so unlike a native build the listener is no
// longer attached. This is the one place the abort-build semantics differ.
expect(store.delete('again') === true && thrower.evictions.length === 1, `throwing listener was detached by its default return on wasm32 (evictions ${thrower.evictions.length})`);
store.setEvictionListener(thrower);
expect(store.put('again2', enc.encode('y'), EntryKind.Persistent, null) && store.delete('again2') === true && thrower.evictions.length === 2, 'listener re-attached and delivering after the foreign error');
// Expired path through the same throwing listener.
thrower.throwOn = 'stale2';
expect(store.put('stale2', enc.encode('z'), EntryKind.Volatile, -5n), 'put stale2');
let foreignExpired = null;
try {
  store.get('stale2');
} catch (e) {
  foreignExpired = e;
}
expect(foreignExpired instanceof WeaveFFIError && foreignExpired.code === -4, `foreign error on the expiry path is -4 (got ${foreignExpired && foreignExpired.code})`);
expect([...store.listKeys(null)].join(',') === 'alpha,beta', `keys after the foreign errors (got ${[...store.listKeys(null)]})`);
store.clearEvictionListener();

// Async compact: the wasm32 default spawner completes inline, so the Promise
// is already settled when returned; `await` just observes it.
expect(store.put('exp1', enc.encode('12345'), EntryKind.Volatile, -10n), 'put exp1');
expect(store.put('exp2', enc.encode('678'), EntryKind.Volatile, -10n), 'put exp2');
expect(getStats(store).expired_entries === 2n, 'two expired entries before compact');
const compacting = store.compact();
expect(compacting instanceof Promise, 'compact returns a Promise');
expect(getStats(store).expired_entries === 0n, 'inline completion: compacted before await');
const reclaimed = await compacting;
expect(reclaimed === 8n, `compact reclaims 8n bytes (got ${reclaimed})`);
expect((await store.compact()) === 0n, 'second compact reclaims nothing');
expect(store.count() === 2n, 'live entries untouched by compact');

// Deprecated method still works (documented as @deprecated in the glue).
expect(store.legacyPut('legacy', enc.encode('l')) === true, 'legacyPut');
expect(store.get('legacy').expires_at === null, 'legacyPut stores a non-expiring entry');
store.clear();
expect(store.count() === 0n && [...store.listKeys(null)].length === 0, 'clear empties the store');

// Idempotent close; use after close is a marshalling error (-3), including
// through the Promise path.
forked.close();
forked.close();
empty.close();
store.close();
expect(store._handle === 0, 'close() zeroes the handle');
store.close();
let closedErr = null;
try {
  store.count();
} catch (e) {
  closedErr = e;
}
expect(closedErr instanceof WeaveFFIError && closedErr.code === -3, `use after close is code -3 (got ${closedErr && closedErr.code})`);
let closedAsync = null;
try {
  await store.compact();
} catch (e) {
  closedAsync = e;
}
expect(closedAsync instanceof WeaveFFIError && closedAsync.code === -3, `async use after close is code -3 (got ${closedAsync && closedAsync.code})`);
let closedStats = null;
try {
  getStats(store);
} catch (e) {
  closedStats = e;
}
expect(closedStats instanceof WeaveFFIError && closedStats.code === -3, 'borrowing a closed object as a parameter is code -3');

// Symbol.dispose releases the same way (twice is safe); a store closed while
// a listener is attached frees that listener.
const disposeSym = typeof Symbol.dispose === 'symbol' ? Symbol.dispose : Symbol.for('Symbol.dispose');
const disposable = Store.open('/tmp/disposable.db');
disposable.setEvictionListener(new Listener('dangling'));
expect(typeof disposable[disposeSym] === 'function', 'wrapper implements Symbol.dispose');
disposable[disposeSym]();
expect(disposable._handle === 0, 'Symbol.dispose zeroes the handle');
disposable[disposeSym]();

if (failures > 0) {
  console.error(`wasm/kvstore: ${failures} failure(s)`);
  process.exit(1);
}
console.log('wasm/kvstore: OK');
