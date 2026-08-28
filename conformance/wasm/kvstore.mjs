// Conformance consumer: kvstore sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the generated ESM bindings (loadWeaveffiWasm) against the real producer
// compiled to wasm. Exercises the 0.7.0 surface end to end: the `Store`
// interface class (static `open` factory, instance methods passing the handle
// as the implicit self argument, the `defaultCapacity` static, and `free()`
// through the destroy symbol), the per-module typed error domain (`KvError`
// subclasses with stable codes, thrown by `throws` wrappers), plus every
// marshalling path the generator emits: NUL-terminated string args, records
// decoded from value buffers into plain objects (`Entry` with its bytes,
// optional-scalar, list, and map fields; `Stats` from the kv.stats
// submodule), buffered optional parameters in (`i64?` TTL, `string?`
// prefix), a lazy iterator-backed string stream (listKeys), the async,
// i64-returning compact via the callback trampoline, and the eviction
// listener via the long-lived function-table trampoline (synchronous,
// same-thread delivery). Records are value types now: there is no Entry
// wrapper class, builder, or create factory.
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

// Typed error surface: module-scope classes with the domain hierarchy.
const { WeaveFFIError, KvError, KeyNotFound, IoError } = mod;
expect(typeof WeaveFFIError === 'function', 'WeaveFFIError exported');
expect(typeof KvError === 'function', 'KvError exported');
expect(KeyNotFound.CODE === 1001, 'KeyNotFound.CODE === 1001');
expect(KvError.KeyNotFound === KeyNotFound, 'per-code class aliased on the domain');

// The interface class hangs off the module object.
const Store = api.kv.Store;
expect(typeof Store === 'function', 'Store class exposed on api.kv');

// Constructor `open` throws the typed domain error on an empty path.
let openErr = null;
try { Store.open(''); } catch (e) { openErr = e; }
expect(openErr instanceof IoError, 'open("") -> instanceof IoError');
expect(openErr instanceof KvError, 'open("") -> instanceof KvError (domain)');
expect(openErr instanceof WeaveFFIError, 'open("") -> instanceof WeaveFFIError (base)');
expect(openErr && openErr.code === 1004, 'open("") -> code 1004');
expect(openErr && /I\/O failure/.test(openErr.message), 'open("") -> default message');

// Static factory returns a wrapped owned handle.
const store = Store.open('/tmp/conformance-kvstore-wasm');
expect(store instanceof Store, 'open -> instanceof Store');
expect(store._handle > 0, 'open -> non-null handle');

// The optional TTL parameter is a buffered `i64?`: null omits, a value
// encodes the flag byte plus payload.
const payload = new Uint8Array([1, 2, 3]);
expect(store.put('alpha', payload, EntryKind.Persistent, null) === true, 'put alpha (no ttl)');
expect(store.put('beta', payload, EntryKind.Volatile, 3600) === true, 'put beta (with ttl)');

// i64 return -> BigInt, via the implicit self argument.
expect(store.count() === 2n, 'count == 2');

// Static method on the interface class.
expect(Store.defaultCapacity() === 1000000n, 'Store.defaultCapacity == 1_000_000');

// Iterator-backed string stream, drained eagerly into an array. The
// optional prefix is a buffered `string?`.
const keys = [...store.listKeys(null)].sort();
expect(keys.length === 2 && keys[0] === 'alpha' && keys[1] === 'beta', 'listKeys values');
const filtered = [...store.listKeys('al')];
expect(filtered.length === 1 && filtered[0] === 'alpha', 'listKeys prefix filter');

// get -> buffered `Entry?` decoded into a plain object covering every
// complex field type: bytes, optional scalar, list, and map.
const alpha = store.get('alpha');
expect(alpha && alpha.key === 'alpha', 'get alpha.key');
expect(alpha.id > 0n, 'alpha.id positive (i64)');
expect(alpha.value instanceof Uint8Array && alpha.value.length === 3 && alpha.value[0] === 1 && alpha.value[2] === 3, 'alpha.value bytes');
expect(alpha.created_at > 0n, 'alpha.created_at positive (i64)');
expect(alpha.expires_at === null, 'alpha.expires_at null (optional scalar absent)');
expect(Array.isArray(alpha.tags) && alpha.tags.length === 0, 'alpha.tags empty list');
expect(alpha.metadata && typeof alpha.metadata === 'object' && Object.keys(alpha.metadata).length === 0, 'alpha.metadata empty map');

const beta = store.get('beta');
expect(typeof beta.expires_at === 'bigint' && beta.expires_at > 0n, 'beta.expires_at present (optional scalar)');

// Missing key: the throwing wrapper raises the typed per-code subclass.
let getErr = null;
try { store.get('nope'); } catch (e) { getErr = e; }
expect(getErr instanceof KeyNotFound, 'get missing -> instanceof KeyNotFound');
expect(getErr instanceof KvError, 'get missing -> instanceof KvError');
expect(getErr && getErr.code === 1001, 'get missing -> code 1001');
expect(getErr && /key not found/.test(getErr.message), 'get missing -> message');

// Deprecated method still works.
expect(store.legacyPut('legacy', new Uint8Array([7])) === true, 'legacyPut inserts');
expect(store.delete('legacy') === true, 'delete existing -> true');
expect(store.delete('legacy') === false, 'delete missing -> false');

// Submodule function taking the interface as a borrowed parameter; the
// Stats record decodes into a plain object of BigInt fields.
const st = api.kv.stats.getStats(store);
expect(st.total_entries === 2n, 'stats.total_entries == 2');
expect(st.total_bytes === 6n, 'stats.total_bytes == 6');
expect(st.expired_entries === 0n, 'stats.expired_entries == 0');

// Async i64 return via the registered callback trampoline. The wasm build
// pins the clock, so a zero TTL is expired immediately and compact
// deterministically reclaims that entry's 3 value bytes.
expect(store.put('expired', payload, EntryKind.Volatile, 0) === true, 'put expired (ttl 0)');
const reclaimed = await store.compact();
expect(reclaimed === 3n, `compact reclaimed 3 bytes (got ${reclaimed})`);
expect(store.count() === 2n, 'count == 2 after compact');

// clear drops every entry.
store.clear();
expect(store.count() === 0n, 'count == 0 after clear');

// Eviction listener: delete fires emit_eviction_listener synchronously (wasm
// delivery is same-thread; the callback runs inside the delete call).
const evicted = [];
const evictionSub = api.kv.registerEvictionListener((key) => evicted.push(key));
expect(typeof evictionSub === 'number' && evictionSub > 0, 'eviction listener id positive');
expect(store.put('doomed', new Uint8Array([1]), EntryKind.Volatile, null) === true, 'put doomed');
expect(store.delete('doomed') === true, 'delete doomed -> true');
expect(
  evicted.length === 1 && evicted[0] === 'doomed',
  `eviction listener fired synchronously (got ${JSON.stringify(evicted)})`
);
api.kv.unregisterEvictionListener(evictionSub);
expect(store.put('doomed2', new Uint8Array([1]), EntryKind.Volatile, null) === true, 'put doomed2');
expect(store.delete('doomed2') === true, 'delete doomed2 -> true');
expect(evicted.length === 1, `no delivery after unregister (got ${JSON.stringify(evicted)})`);

// Disposal: free() releases the handle via the destroy symbol exactly once.
store.free();
expect(store._handle === 0, 'free() zeroes the handle');
store.free(); // second call is a no-op

if (failures === 0) {
  console.log('wasm/kvstore: OK');
} else {
  console.error(`wasm/kvstore: ${failures} failure(s)`);
  process.exit(1);
}
