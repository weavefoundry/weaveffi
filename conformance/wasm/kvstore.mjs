// Conformance consumer: kvstore sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the generated ESM bindings (loadWeaveffiWasm) against the real producer
// compiled to wasm. Exercises every path the generator marshals across linear
// memory: NUL-terminated string args, the bytes getter (Entry.value), the
// optional-scalar getter (Entry.expires_at), the list getter (Entry.tags), the
// map getter (Entry.metadata over parallel key/value arrays), an iterator-backed
// list-of-string return (list_keys), the fluent builder + static create factory
// (which round-trip list/map/bytes/optional inputs), the kv.stats submodule, and
// the async, i64-returning compact_async via the callback trampoline.
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

const EntryKind = mod.EntryKind;
expect(EntryKind && EntryKind.Persistent === 1, 'enum EntryKind exported');

const store = api.kv.open_store('/tmp/conformance-kvstore-wasm');
expect(store && store._handle > 0, 'open store -> handle');

const payload = new Uint8Array([1, 2, 3]);
expect(api.kv.put(store, 'alpha', payload, EntryKind.Persistent, null) === true, 'put alpha (no ttl)');
expect(api.kv.put(store, 'beta', payload, EntryKind.Volatile, 3600) === true, 'put beta (with ttl)');

// i64 return -> BigInt.
expect(api.kv.count(store) === 2n, 'count == 2');

// Iterator-backed list-of-string return, drained eagerly into an array.
const keys = api.kv.list_keys(store, null).sort();
expect(keys.length === 2 && keys[0] === 'alpha' && keys[1] === 'beta', 'list_keys values');

// Struct return + getters over every complex field type.
const alpha = api.kv.get(store, 'alpha');
expect(alpha && alpha.key === 'alpha', 'get alpha.key');
expect(alpha.id > 0n, 'alpha.id positive (i64)');
expect(alpha.value instanceof Uint8Array && alpha.value.length === 3 && alpha.value[0] === 1 && alpha.value[2] === 3, 'alpha.value bytes');
expect(alpha.expires_at === null, 'alpha.expires_at null (optional scalar absent)');
expect(Array.isArray(alpha.tags) && alpha.tags.length === 0, 'alpha.tags empty list');
expect(alpha.metadata && typeof alpha.metadata === 'object' && Object.keys(alpha.metadata).length === 0, 'alpha.metadata empty map');

const beta = api.kv.get(store, 'beta');
expect(typeof beta.expires_at === 'bigint' && beta.expires_at > 0n, 'beta.expires_at present (optional scalar)');

// Missing key: the producer signals via an error code, so the binding throws.
let threw = false;
try { api.kv.get(store, 'nope'); } catch (e) { threw = /key not found/.test(String(e)); }
expect(threw, 'get missing -> throws key-not-found');

// Submodule + struct return.
const st = api.kv.stats.get_stats(store);
expect(st.total_entries === 2n, 'stats.total_entries == 2');

// Builder: round-trips list + map + bytes + optional scalar through C arrays.
const built = api.kv.Entry.builder()
  .id(7).key('built').value(new Uint8Array([9, 9])).created_at(123)
  .expires_at(456).tags(['x', 'y']).metadata({ a: '1', b: '2' }).build();
expect(built.key === 'built', 'built.key');
expect(built.id === 7n, 'built.id (i64)');
expect(built.value.length === 2 && built.value[1] === 9, 'built.value bytes');
expect(built.expires_at === 456n, 'built.expires_at present');
expect(JSON.stringify(built.tags) === JSON.stringify(['x', 'y']), 'built.tags list round-trip');
expect(built.metadata.a === '1' && built.metadata.b === '2', 'built.metadata map round-trip');

// Static create factory with the same complex inputs; optional omitted via null.
const created = api.kv.Entry.create(11, 'made', new Uint8Array([5]), 100, null, ['t'], { k: 'v' });
expect(created.key === 'made', 'created.key');
expect(created.expires_at === null, 'created.expires_at null');
expect(created.tags.length === 1 && created.tags[0] === 't', 'created.tags');
expect(created.metadata.k === 'v', 'created.metadata');

// Async i64 return via the registered callback trampoline.
const reclaimed = await api.kv.compact_async(store);
expect(typeof reclaimed === 'bigint', 'compact_async -> BigInt');

api.kv.close_store(store);

if (failures === 0) {
  console.log('wasm/kvstore: OK');
} else {
  console.error(`wasm/kvstore: ${failures} failure(s)`);
  process.exit(1);
}
