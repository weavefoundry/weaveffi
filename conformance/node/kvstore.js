// Conformance consumer: kvstore sample, Node (N-API) target.
//
// Exercises the struct-materialization paths the Node addon previously stubbed
// to null: the `Buffer` bytes getter (`Entry.value`), the nullable-scalar getter
// (`Entry.expires_at`), the array list getter (`Entry.tags`) and the object map
// getter (`Entry.metadata`) over the triple-pointer ABI. Also covers the
// iterator-backed `kv_list_keys` and the `kv.stats` submodule. Node materializes
// structs by value and exposes no builder, so tags/metadata are read in their
// (empty) producer state; the bytes and optional-scalar paths carry payloads.

'use strict';

const addon = require(process.env.WV_ADDON);

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

const EntryKind = { Volatile: 0, Persistent: 1, Encrypted: 2 };

const store = addon.kv_open_store('/tmp/conformance-kvstore-node');
expect(Number(store) > 0, 'open store');

const payload = Buffer.from([1, 2, 3]);
expect(addon.kv_put(store, 'alpha', payload, EntryKind.Persistent, null) === true, 'put alpha');
expect(addon.kv_put(store, 'beta', payload, EntryKind.Volatile, 3600) === true, 'put beta with ttl');

expect(addon.kv_count(store) === 2, 'count == 2');

// Iterator-backed list-of-string function return.
const keys = addon.kv_list_keys(store, null).sort();
expect(keys.length === 2 && keys[0] === 'alpha' && keys[1] === 'beta', 'list_keys values');

// Struct materialization with complex fields.
const alpha = addon.kv_get(store, 'alpha');
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
const beta = addon.kv_get(store, 'beta');
expect(typeof beta.expires_at === 'number' && beta.expires_at > 0, 'beta expires_at number');

// kv.stats submodule.
const st = addon.kv_stats_get_stats(store);
expect(st.total_entries === 2, 'stats total entries == 2');

addon.kv_close_store(store);
console.log('node/kvstore: OK');
