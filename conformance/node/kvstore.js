// Conformance consumer: kvstore sample, Node (N-API) target.
//
// Drives the generated wrapper layer (index.js) end to end: the Store
// interface class (non-`new` constructor mapped to the static `open` factory,
// instance methods, the static `defaultCapacity`, explicit `destroy`), the
// typed error surface (KeyNotFoundError / IoError extending KvError extending
// WeaveFFIError, each carrying its stable code), struct materialization with
// complex fields (`Buffer` bytes, nullable scalar, list, map), the
// iterator-backed `listKeys` (order + prefix filter), the deprecated
// `legacyPut`, the `kv.stats` submodule taking the interface by reference,
// the TSFN-backed eviction listener (delivery hops to the JS thread, so
// assertions follow a setImmediate boundary), and the promise-returning
// `compact` settled via a threadsafe function from the producer's worker
// thread. The harness passes the built addon via WV_ADDON; the generated
// loader honors WEAVEFFI_ADDON.

'use strict';

const path = require('path');

const ADDON = path.resolve(process.env.WV_ADDON);
process.env.WEAVEFFI_ADDON = ADDON;
// WV_ADDON = <target>/conformance-build/node-kvstore/build/Release/index.node;
// the generated files sit at <target>/conformance-gen/kvstore/node/.
const wv = require(
  path.resolve(ADDON, '../../../../..', 'conformance-gen', 'kvstore', 'node', 'index.js')
);

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

const tick = () => new Promise((resolve) => setImmediate(resolve));

const EntryKind = { Volatile: 0, Persistent: 1, Encrypted: 2 };

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
}

const store = wv.Store.open('/tmp/conformance-kvstore-node');
expect(store instanceof wv.Store, 'open returns a Store instance');

// Static method.
expect(wv.Store.defaultCapacity() === 1000000, 'defaultCapacity == 1000000');

const payload = Buffer.from([1, 2, 3]);
expect(store.put('alpha', payload, EntryKind.Persistent, null) === true, 'put alpha');
expect(store.put('beta', payload, EntryKind.Volatile, 3600) === true, 'put beta with ttl');

expect(store.count() === 2, 'count == 2');

// Iterator-backed method return: keys stream in sorted order, optionally
// filtered by prefix.
const keys = store.listKeys(null);
expect(
  Array.isArray(keys) && keys.length === 2 && keys[0] === 'alpha' && keys[1] === 'beta',
  `listKeys yields sorted keys (got ${JSON.stringify(keys)})`
);
const filtered = store.listKeys('al');
expect(
  filtered.length === 1 && filtered[0] === 'alpha',
  `listKeys honors the prefix (got ${JSON.stringify(filtered)})`
);

// Struct materialization with complex fields.
const alpha = store.get('alpha');
expect(typeof alpha === 'object' && alpha !== null, 'get alpha object');
expect(alpha.id > 0, 'entry id positive');
expect(alpha.key === 'alpha', 'entry key');

// Bytes getter -> Buffer.
expect(Buffer.isBuffer(alpha.value), 'entry value is Buffer');
expect(alpha.value.length === 3 && alpha.value[0] === 1 && alpha.value[2] === 3, 'entry value bytes');

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

// beta had a TTL, so the nullable-scalar getter yields a number.
const beta = store.get('beta');
expect(typeof beta.expires_at === 'number' && beta.expires_at > 0, 'beta expires_at number');

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
}

// Deprecated method still works (delete before the listener registers so the
// eviction counts below stay exact).
expect(store.legacyPut('legacy', payload) === true, 'legacyPut inserts');
expect(store.count() === 3, 'count == 3 after legacyPut');
expect(store.delete('legacy') === true, 'delete legacy');

// kv.stats submodule: takes the Store instance by reference.
const st = wv.getStats(store);
expect(st.total_entries === 2, 'stats total entries == 2');
expect(st.total_bytes === 6, 'stats total bytes == 6');
expect(st.expired_entries === 0, 'stats expired entries == 0');

(async () => {
  // Eviction listener: the producer fires on the deleting thread; the TSFN
  // queues delivery onto the JS thread (visible after an event-loop tick).
  const evicted = [];
  const sub = wv.registerEvictionListener((key) => evicted.push(key));
  expect(typeof sub === 'number' && sub > 0, 'listener id positive');

  expect(store.delete('beta') === true, 'delete beta');
  await tick();
  expect(evicted.length === 1 && evicted[0] === 'beta', `eviction fired for beta (got ${JSON.stringify(evicted)})`);

  // Unregister stops delivery.
  wv.unregisterEvictionListener(sub);
  expect(store.delete('alpha') === true, 'delete alpha');
  await tick();
  expect(evicted.length === 1, `no eviction after unregister (got ${JSON.stringify(evicted)})`);

  // Async: an immediately-expired entry gives compact 3 bytes to reclaim; the
  // promise settles via a TSFN from the producer's worker thread.
  expect(store.put('doomed', payload, EntryKind.Volatile, 0) === true, 'put doomed');
  const reclaimed = await store.compact();
  expect(reclaimed === 3, `compact reclaimed 3 bytes (got ${reclaimed})`);
  expect(store.count() === 0, 'store empty after deletes + compact');

  store.destroy();
  console.log('node/kvstore: OK');
})();
