// Conformance consumer: kvstore sample, Node (N-API) target.
//
// Drives the generated wrapper layer (index.js) end to end: the
// reference-counted Store class (static `open` factory, instance methods,
// the static `defaultCapacity`, idempotent `close()`, use-after-close trap),
// the typed error surface (KeyNotFoundError / ExpiredError / IoError
// extending KvError extending WeaveFFIError, each carrying its stable code),
// record materialization with complex fields (`Buffer` bytes, nullable
// BigInt, list, map), the iterator-backed `listKeys`, the deprecated
// `legacyPut`, the `kv.stats` submodule taking the interface by reference,
// the synchronous `EvictionListener` callback interface implemented as a JS
// class (return value detaches, replace and clear, a throwing listener
// surfacing to the caller as code -4), objects in every buffered position
// (`share()` aliasing the same object, `fork()`, `larger(null)`,
// `describe().store`, `openMany`, `totalCount` with a record that carries
// objects), and the Promise-returning `compact`. i64 values cross as BigInt.
// The harness passes the built addon via WV_ADDON; the generated loader
// honors WEAVEFFI_ADDON.

'use strict';

const path = require('path');

const ADDON = path.resolve(process.env.WV_ADDON);
process.env.WEAVEFFI_ADDON = ADDON;
// WV_ADDON = <target>/conformance-build/node-kvstore/build/Release/index.node;
// the generated files sit at <target>/conformance-gen/kvstore/node/.
const wv = require(
  path.resolve(ADDON, '../../../../..', 'conformance-gen', 'kvstore', 'node', 'index.js')
);

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}

// C-style enums are exported as frozen objects (forward and reverse mappings).
const EntryKind = wv.EntryKind;
const EvictionReason = wv.EvictionReason;
expect(EntryKind.Volatile === 0 && EntryKind.Persistent === 1 && EntryKind.Encrypted === 2, 'EntryKind values');
expect(EvictionReason.Deleted === 0 && EvictionReason.Expired === 1, 'EvictionReason values');
expect(EvictionReason[1] === 'Expired', 'EvictionReason reverse mapping');

// An eviction listener as a JS class: records every notification and keeps
// receiving until `stopAfter` notifications have arrived.
class Listener {
  constructor(name, stopAfter) {
    this.name = name;
    this.stopAfter = stopAfter === undefined ? Infinity : stopAfter;
    this.seen = [];
  }
  onEvict(entry, reason) {
    this.seen.push({ key: entry.key, reason, entry });
    return this.seen.length < this.stopAfter;
  }
}

// Typed error on the factory: an empty path is rejected with the IoError
// class (code 1004; `IoError` already ends in `Error`, so no suffix stacking).
try {
  wv.Store.open('');
  expect(false, 'expected throw for empty path');
} catch (e) {
  expect(e instanceof wv.IoError, 'IoError instance (got ' + e.name + ')');
  expect(e instanceof wv.KvError, 'IoError extends KvError');
  expect(e instanceof wv.WeaveFFIError, 'IoError extends WeaveFFIError');
  expect(e.code === 1004, 'IoError code == 1004 (got ' + e.code + ')');
  expect(wv.IoError.CODE === 1004, 'IoError.CODE == 1004');
}

const store = wv.Store.open('/tmp/conformance-kvstore-node');
expect(store instanceof wv.Store, 'open returns a Store instance');

// Static method; i64 returns are BigInt.
const cap = wv.Store.defaultCapacity();
expect(typeof cap === 'bigint', 'defaultCapacity is a BigInt');
expect(cap === 1000000n, `defaultCapacity == 1000000n (got ${cap})`);

const payload = Buffer.from([1, 2, 3]);
expect(store.put('alpha', payload, EntryKind.Persistent, null) === true, 'put alpha');
expect(store.put('beta', payload, EntryKind.Volatile, 3600n) === true, 'put beta with BigInt ttl');
expect(store.put('gamma', payload, EntryKind.Encrypted, 7200) === true, 'put gamma with number ttl');

expect(store.count() === 3n, `count == 3n (got ${store.count()})`);

// Iterator-backed method return: a lazy iterable streaming keys in sorted
// order (one producer next per step), optionally filtered by prefix.
const keys = [...store.listKeys(null)];
expect(
  keys.join(',') === 'alpha,beta,gamma',
  `listKeys yields sorted keys (got ${JSON.stringify(keys)})`
);
const filtered = [...store.listKeys('al')];
expect(
  filtered.length === 1 && filtered[0] === 'alpha',
  `listKeys honors the prefix (got ${JSON.stringify(filtered)})`
);
expect([...store.listKeys('zzz')].length === 0, 'listKeys with no match is empty');
const keyIt = store.listKeys(null)[Symbol.iterator]();
expect(keyIt.next().value === 'alpha', 'lazy first key');
keyIt.return();
expect(keyIt.next().done === true, 'iterator done after return()');

// Record materialization with complex fields.
const alpha = store.get('alpha');
expect(typeof alpha === 'object' && alpha !== null, 'get alpha object');
expect(typeof alpha.id === 'bigint' && alpha.id === 1n, `entry id is 1n (got ${alpha.id})`);
expect(alpha.key === 'alpha', 'entry key');
expect(typeof alpha.created_at === 'bigint' && alpha.created_at > 0n, 'created_at is a positive BigInt');

// Bytes getter -> Buffer.
expect(Buffer.isBuffer(alpha.value), 'entry value is Buffer');
expect(alpha.value.equals(payload), 'entry value bytes');

// Optional-scalar getter: alpha had no TTL -> null.
expect(alpha.expires_at === null, 'alpha expires_at null');

// List getter (empty) -> array; map getter (empty) -> object.
expect(Array.isArray(alpha.tags) && alpha.tags.length === 0, 'alpha tags empty array');
expect(
  typeof alpha.metadata === 'object' &&
    alpha.metadata !== null &&
    Object.keys(alpha.metadata).length === 0,
  'alpha metadata empty object'
);

// beta had a TTL, so the nullable-scalar getter yields a BigInt.
const beta = store.get('beta');
expect(typeof beta.expires_at === 'bigint', 'beta expires_at BigInt');
expect(beta.expires_at === beta.created_at + 3600n, 'beta expires_at == created_at + 3600');
expect(beta.id === 2n, 'beta id 2n');

// Typed error on a method: a missing key throws the KeyNotFound class.
try {
  store.get('missing');
  expect(false, 'expected throw for missing key');
} catch (e) {
  expect(e instanceof wv.KeyNotFoundError, 'KeyNotFoundError instance (got ' + e.name + ')');
  expect(e instanceof wv.KvError, 'KeyNotFoundError extends KvError');
  expect(e instanceof wv.WeaveFFIError, 'KeyNotFoundError extends WeaveFFIError');
  expect(e.code === 1001, 'KeyNotFound code == 1001 (got ' + e.code + ')');
  expect(wv.KeyNotFoundError.CODE === 1001, 'KeyNotFoundError.CODE == 1001');
  expect(e.errorMessage === 'key not found', `default message (got ${JSON.stringify(e.errorMessage)})`);
}

// Deprecated method still works.
expect(store.legacyPut('legacy', payload) === true, 'legacyPut inserts');
expect(store.count() === 4n, 'count == 4n after legacyPut');
expect(store.delete('legacy') === true, 'delete legacy');
expect(store.delete('legacy') === false, 'second delete returns false');

// kv.stats submodule: takes the Store instance by reference.
const st = wv.getStats(store);
expect(st.total_entries === 3n, `stats total entries == 3n (got ${st.total_entries})`);
expect(st.total_bytes === 9n, `stats total bytes == 9n (got ${st.total_bytes})`);
expect(st.expired_entries === 0n, 'stats expired entries == 0n');

// --- Eviction listener (synchronous callback interface) ---------------------
const l1 = new Listener('l1');
store.setEvictionListener(l1);
expect(store.delete('gamma') === true, 'delete gamma');
expect(l1.seen.length === 1, `listener fired synchronously (got ${l1.seen.length})`);
expect(l1.seen[0].key === 'gamma', 'evicted key gamma');
expect(l1.seen[0].reason === EvictionReason.Deleted, `reason Deleted (got ${l1.seen[0].reason})`);
expect(l1.seen[0].entry.id === 3n && l1.seen[0].entry.value.equals(payload), 'evicted entry decoded');

// An expired entry is evicted on read: get() throws ExpiredError and the
// listener sees reason Expired.
expect(store.put('doomed', Buffer.from([9]), EntryKind.Volatile, -1n) === true, 'put doomed');
try {
  store.get('doomed');
  expect(false, 'expected ExpiredError');
} catch (e) {
  expect(e instanceof wv.ExpiredError && e.code === 1002, `ExpiredError code 1002 (got ${e && e.code})`);
  expect(e instanceof wv.KvError, 'ExpiredError extends KvError');
}
expect(l1.seen.length === 2 && l1.seen[1].key === 'doomed', 'listener saw the expiry');
expect(l1.seen[1].reason === EvictionReason.Expired, `reason Expired (got ${l1.seen[1].reason})`);
expect(l1.seen[1].entry.expires_at !== null && l1.seen[1].entry.expires_at < l1.seen[1].entry.created_at, 'expired entry carries its past expiry');

// Replacing the listener: the new one receives, the old one doesn't.
const l2 = new Listener('l2', 1);
store.setEvictionListener(l2);
expect(store.put('x1', payload, EntryKind.Volatile, null) && store.put('x2', payload, EntryKind.Volatile, null), 'put x1 x2');
expect(store.delete('x1') === true, 'delete x1');
expect(l2.seen.length === 1 && l2.seen[0].key === 'x1', 'replacement listener fired');
expect(l1.seen.length === 2, 'replaced listener no longer fires');
// l2 returned false: it detached itself, so the next eviction is unobserved.
expect(store.delete('x2') === true, 'delete x2');
expect(l2.seen.length === 1, 'listener detached after returning false');

// A listener that throws: delete() aborts with the foreign error (code -4)
// and the process keeps running; the entry is gone regardless.
const thrower = {
  onEvict(entry) {
    throw new TypeError('listener refuses ' + entry.key);
  },
};
store.setEvictionListener(thrower);
expect(store.put('x3', payload, EntryKind.Volatile, null) === true, 'put x3');
try {
  store.delete('x3');
  expect(false, 'expected delete to throw when the listener throws');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError, `foreign error is a WeaveFFIError (got ${e && e.constructor.name})`);
  expect(!(e instanceof wv.KvError), 'foreign error is not a domain error');
  expect(e.code === -4, `foreign error code -4 (got ${e.code})`);
  expect(e.errorMessage.includes('listener refuses x3'), `carries the JS message (got ${JSON.stringify(e.errorMessage)})`);
}
expect(store.count() === 2n, 'x3 removed despite the listener failure');
// A wrong return type is a foreign error too.
store.setEvictionListener({ onEvict: () => 'yes' });
expect(store.put('x4', payload, EntryKind.Volatile, null) === true, 'put x4');
try {
  store.delete('x4');
  expect(false, 'expected delete to throw for a bad listener return');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError && e.code === -4, `bad return type is code -4 (got ${e && e.code})`);
}
store.clearEvictionListener();
expect(store.put('x5', payload, EntryKind.Volatile, null) === true, 'put x5');
expect(store.delete('x5') === true, 'delete after clearEvictionListener');
expect(store.count() === 2n, 'count back to 2n');

// --- Objects in every position ----------------------------------------------
// share(): a second wrapper over the same object. Writes through one are
// visible through the other, and closing one leaves the other alive.
const shared = store.share();
expect(shared instanceof wv.Store, 'share returns a Store');
expect(shared !== store, 'share returns a distinct wrapper');
expect(shared.count() === 2n, 'shared sees the same entries');
expect(shared.put('via-shared', payload, EntryKind.Volatile, null) === true, 'put through shared');
expect(store.count() === 3n, 'write through share visible through the original');
expect(store.get('via-shared').key === 'via-shared', 'entry readable through the original');
const l3 = new Listener('l3');
shared.setEvictionListener(l3);
expect(store.delete('via-shared') === true, 'delete through the original');
expect(l3.seen.length === 1 && l3.seen[0].key === 'via-shared', 'listener set through share fires for the original');
store.clearEvictionListener();
shared.close();
shared.close();
expect(store.count() === 2n, 'original alive after shared wrapper closed');

// fork(): an independent copy.
const forked = store.fork();
expect(forked.count() === 2n, 'fork copies live entries');
expect(forked.put('only-in-fork', payload, EntryKind.Volatile, null) === true, 'put in fork');
expect(forked.count() === 3n && store.count() === 2n, 'fork is independent');
expect(forked.get('alpha').id === 1n, 'fork preserves entry ids');

// larger(): Store? both ways.
const empty = wv.Store.open('/tmp/empty');
expect(empty.larger(null) === null, 'larger(null) on an empty store is null');
expect(empty.larger(undefined) === null, 'larger(undefined) on an empty store is null');
const own = store.larger(null);
expect(own instanceof wv.Store && own.count() === 2n, 'larger(null) on a non-empty store is itself');
const bigger = store.larger(forked);
expect(bigger instanceof wv.Store && bigger.count() === 3n, 'larger(forked) picks the fork');
const stillOwn = store.larger(empty);
expect(stillOwn.count() === 2n, 'larger(empty) picks self');
own.close();
bigger.close();
stillOwn.close();
expect(store.count() === 2n && forked.count() === 3n, 'sources alive after result wrappers closed');

// describe(): a record carrying an object field and an optional object.
const info = store.describe('primary', null);
expect(info.label === 'primary', 'describe label');
expect(info.count === 2n, `describe count 2n (got ${info.count})`);
expect(info.store instanceof wv.Store, 'describe().store is a Store');
expect(info.mirror === null, 'describe().mirror is null when absent');
expect(info.store.count() === 2n, 'describe().store is usable');
expect(info.store.put('via-info', payload, EntryKind.Volatile, null) === true, 'put through describe().store');
expect(store.count() === 3n, 'describe().store aliases the original');
const infoWithMirror = store.describe('mirrored', forked);
expect(infoWithMirror.mirror instanceof wv.Store, 'describe().mirror is a Store when present');
expect(infoWithMirror.mirror.count() === 3n, 'mirror is usable');
expect(infoWithMirror.mirror.get('only-in-fork').key === 'only-in-fork', 'mirror aliases the fork');

// openMany(): a list of objects as a return; a bad path fails the whole call.
const many = wv.Store.openMany(['/tmp/m1', '/tmp/m2', '/tmp/m3']);
expect(Array.isArray(many) && many.length === 3, `openMany returns 3 stores (got ${many && many.length})`);
expect(many.every((s) => s instanceof wv.Store && s.count() === 0n), 'openMany stores are fresh');
expect(many[0].put('m', payload, EntryKind.Volatile, null) === true, 'put in many[0]');
expect(many[1].count() === 0n, 'openMany stores are distinct');
try {
  wv.Store.openMany(['/tmp/ok', '']);
  expect(false, 'expected openMany to throw for an empty path');
} catch (e) {
  expect(e instanceof wv.IoError && e.code === 1004, `openMany IoError (got ${e && e.code})`);
}
expect(wv.Store.openMany([]).length === 0, 'openMany([]) is empty');

// totalCount(): a list of objects as a parameter plus an optional record
// that carries objects (the encoder clones each handle for the buffer).
expect(wv.Store.totalCount(many, null) === 1n, 'totalCount(many) == 1n');
expect(wv.Store.totalCount([], null) === 0n, 'totalCount([]) == 0n');
expect(wv.Store.totalCount([store, forked], null) === 6n, `totalCount([store, forked]) == 6n`);
const total = wv.Store.totalCount(many, info);
expect(total === 4n, `totalCount(many, info) == 4n (got ${total})`);
const total2 = wv.Store.totalCount([], infoWithMirror);
expect(total2 === 3n, `totalCount([], infoWithMirror) == 3n (got ${total2})`);
// Hand-built record with the mirror present.
const total3 = wv.Store.totalCount([empty], { label: 'x', store: forked, mirror: store, count: 0n });
expect(total3 === 3n, `totalCount with hand-built StoreInfo == 3n (got ${total3})`);
// Every source is still alive: encoding borrowed clones, not the wrappers.
expect(store.count() === 3n && forked.count() === 3n && many[0].count() === 1n, 'sources alive after totalCount');

// Release the object graph; every wrapper closes independently.
info.store.close();
infoWithMirror.store.close();
infoWithMirror.mirror.close();
for (const s of many) s.close();
for (const s of many) s.close();
empty.close();
forked.close();
expect(store.count() === 3n, 'original alive after releasing every alias');
try {
  forked.count();
  expect(false, 'expected throw for use after close');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError && e.code === -3, `use after close is code -3 (got ${e && e.code})`);
}
try {
  wv.Store.totalCount([forked], null);
  expect(false, 'expected throw for a closed object inside a list');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError && e.code === -3, `closed object in a buffer is code -3 (got ${e && e.code})`);
}
try {
  wv.getStats(forked);
  expect(false, 'expected throw for a closed object passed by reference');
} catch (e) {
  expect(e instanceof wv.WeaveFFIError && e.code === -3, `closed object by reference is code -3 (got ${e && e.code})`);
}

(async () => {
  // Async: an immediately-expired entry gives compact 3 bytes to reclaim; the
  // promise settles via a TSFN from the producer's worker thread.
  expect(store.delete('via-info') === true, 'delete via-info');
  expect(store.put('dead', payload, EntryKind.Volatile, 0n) === true, 'put dead');
  const st2 = wv.getStats(store);
  expect(st2.expired_entries === 1n, `one expired entry pending (got ${st2.expired_entries})`);
  const pending = store.compact();
  expect(pending instanceof Promise, 'compact returns a Promise');
  const reclaimed = await pending;
  expect(typeof reclaimed === 'bigint', 'compact resolves with a BigInt');
  expect(reclaimed === 3n, `compact reclaimed 3 bytes (got ${reclaimed})`);
  expect((await store.compact()) === 0n, 'second compact reclaims nothing');
  expect(store.count() === 2n, 'alpha and beta remain');
  store.clear();
  expect(store.count() === 0n, 'store empty after clear');

  store.close();
  store.close();
  try {
    await store.compact();
    expect(false, 'expected compact on a closed store to throw');
  } catch (e) {
    expect(e instanceof wv.WeaveFFIError && e.code === -3, `compact after close is code -3 (got ${e && e.code})`);
  }

  if (failures > 0) process.exit(1);
  console.log('node/kvstore: OK');
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
