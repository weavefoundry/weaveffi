// Conformance consumer: kvstore sample (node and wasm lanes).
//
// The Store class (static factories, methods, statics, close()), the typed
// errors of the `kv` domain, records with bytes, optional, list, and map
// fields, the lazy `listKeys` iterator, the `kv.stats` submodule, the
// `EvictionListener` callback interface, objects in every buffered position
// (`share`, `fork`, `larger`, `describe`, `openMany`, `totalCount`), the
// cancellable async `compact`, and leak-free teardown.

import { expect, finish, isWasm, load, rejects, same, throws } from './harness.mjs';

const api = await load('kvstore');
const { kv, KvstoreError, CancelledError } = api;
const { EntryKind, EvictionReason } = kv;

class Listener {
  constructor(stopAfter = Infinity) {
    this.stopAfter = stopAfter;
    this.seen = [];
  }

  onEvict(entry, reason) {
    this.seen.push({ key: entry.key, reason, entry });
    return this.seen.length < this.stopAfter;
  }
}

const isClosed = (e) => e instanceof KvstoreError && e.code === -3;

async function main() {
  expect(EntryKind.Persistent === 1 && EvictionReason[1] === 'Expired', 'enums');
  throws(
    () => kv.Store.open(''),
    (e) => e instanceof kv.IoError && e instanceof kv.KvError && e instanceof KvstoreError && e.code === 1004 && kv.IoError.CODE === 1004,
    'a typed error from a factory',
  );
  throws(() => new kv.Store(), (e) => e instanceof TypeError, 'Store has no public constructor');

  const store = kv.Store.open('/tmp/conformance-kvstore');
  expect(kv.Store.defaultCapacity() === 1000000n, 'a static method');
  const payload = new Uint8Array([1, 2, 3]);
  expect(store.put('alpha', payload, EntryKind.Persistent, null), 'put without a ttl');
  expect(store.put('beta', payload, EntryKind.Volatile, 3600n), 'put with a bigint ttl');
  expect(store.put('gamma', payload, EntryKind.Encrypted, 7200), 'put with a number ttl');
  expect(store.count() === 3n, 'count');

  same([...store.listKeys(null)], ['alpha', 'beta', 'gamma'], 'listKeys');
  same([...store.listKeys('al')], ['alpha'], 'listKeys with a prefix');
  const keys = store.listKeys(null);
  expect(keys.next().value === 'alpha', 'lazy first key');
  keys.close();
  expect(keys.next().done, 'a closed iterator is done');
  store.listKeys(null).next();

  const alpha = store.get('alpha');
  expect(alpha.id === 1n && alpha.key === 'alpha' && alpha.created_at > 0n, 'record fields');
  expect(alpha.value instanceof Uint8Array, 'bytes are a Uint8Array');
  same(alpha.value, payload, 'bytes round-trip');
  same([alpha.expires_at, alpha.tags, alpha.metadata], [null, [], {}], 'empty optional, list, and map');
  const beta = store.get('beta');
  expect(beta.expires_at === beta.created_at + 3600n, 'an optional bigint');
  throws(
    () => store.get('missing'),
    (e) => e instanceof kv.KeyNotFoundError && e.code === 1001 && e.message === 'key not found',
    'a typed error from a method',
  );
  expect(store.legacyPut('legacy', payload) && store.delete('legacy') && !store.delete('legacy'), 'legacyPut and delete');
  same(kv.stats.getStats(store), { total_entries: 3n, total_bytes: 9n, expired_entries: 0n }, 'the stats submodule');

  // The eviction listener.
  const l1 = new Listener();
  store.setEvictionListener(l1);
  expect(store.delete('gamma') && l1.seen.length === 1, 'the listener fires synchronously');
  expect(l1.seen[0].key === 'gamma' && l1.seen[0].reason === EvictionReason.Deleted, 'listener arguments');
  same(l1.seen[0].entry.value, payload, 'the listener decodes its record');
  expect(store.put('doomed', new Uint8Array([9]), EntryKind.Volatile, -1n), 'put an expired entry');
  throws(() => store.get('doomed'), (e) => e instanceof kv.ExpiredError && e.code === 1002, 'expired');
  expect(l1.seen[1].reason === EvictionReason.Expired, 'eviction on read');
  const l2 = new Listener(1);
  store.setEvictionListener(l2);
  store.put('x1', payload, EntryKind.Volatile, null);
  store.put('x2', payload, EntryKind.Volatile, null);
  store.delete('x1');
  store.delete('x2');
  expect(l2.seen.length === 1 && l1.seen.length === 2, 'a listener returning false detaches');
  store.setEvictionListener({
    onEvict(entry) {
      throw new TypeError('listener refuses ' + entry.key);
    },
  });
  store.put('x3', payload, EntryKind.Volatile, null);
  throws(
    () => store.delete('x3'),
    (e) => e instanceof KvstoreError && !(e instanceof kv.KvError) && e.code === -4 && e.message.includes('listener refuses x3'),
    'a throwing listener fails the call with code -4',
  );
  store.setEvictionListener({ onEvict: () => 'yes' });
  store.put('x4', payload, EntryKind.Volatile, null);
  throws(() => store.delete('x4'), (e) => e.code === -4, 'a wrong return type is code -4');
  store.clearEvictionListener();
  expect(store.count() === 2n, 'alpha and beta remain');

  // Objects in every position.
  const shared = store.share();
  expect(shared instanceof kv.Store && shared !== store, 'share returns a new wrapper');
  shared.put('via-shared', payload, EntryKind.Volatile, null);
  expect(store.count() === 3n, 'the wrappers share the object');
  shared.close();
  store.delete('via-shared');
  const forked = store.fork();
  forked.put('only-in-fork', payload, EntryKind.Volatile, null);
  expect(forked.count() === 3n && store.count() === 2n, 'fork is independent');
  const empty = kv.Store.open('/tmp/empty');
  expect(empty.larger(null) === null && empty.larger(undefined) === null, 'a null object result');
  const bigger = store.larger(forked);
  expect(bigger.count() === 3n, 'an object argument and result');
  bigger.close();
  const info = store.describe('primary', forked);
  expect(info.label === 'primary' && info.count === 2n, 'a record with objects');
  expect(info.store instanceof kv.Store && info.mirror instanceof kv.Store, 'object fields');
  expect(info.mirror.get('only-in-fork').key === 'only-in-fork', 'an object field aliases its object');
  const many = kv.Store.openMany(['/tmp/m1', '/tmp/m2']);
  expect(many.length === 2 && many.every((s) => s.count() === 0n), 'a list of objects');
  throws(() => kv.Store.openMany(['/tmp/ok', '']), (e) => e instanceof kv.IoError, 'openMany fails as a whole');
  expect(kv.Store.totalCount([store, forked], null) === 5n, 'a list of objects as an argument');
  expect(kv.Store.totalCount(many, info) === 2n, 'a record of objects as an argument');
  expect(kv.Store.totalCount([], { label: 'x', store: forked, mirror: null, count: 0n }) === 3n, 'a hand-built record');
  info.store.close();
  info.mirror.close();
  for (const s of many) s.close();
  forked.close();
  throws(() => forked.count(), isClosed, 'use after close');
  throws(() => kv.Store.totalCount([forked], null), isClosed, 'a closed object in a buffer');
  throws(() => kv.stats.getStats(forked), isClosed, 'a closed object argument');
  throws(() => kv.stats.getStats({}), (e) => e instanceof TypeError, 'a non-Store argument');

  // A close() racing a call: the listener closes the store mid-call, and
  // the release waits for the call to return.
  const racing = kv.Store.open('/tmp/racing');
  racing.setEvictionListener({
    onEvict() {
      racing.close();
      return true;
    },
  });
  racing.put('k', payload, EntryKind.Volatile, null);
  expect(racing.delete('k') === true, 'the call completes after a close() from its callback');
  throws(() => racing.count(), isClosed, 'the deferred close took effect');

  // Async compaction, and its cancellation.
  store.put('dead', payload, EntryKind.Volatile, 0n);
  const pending = store.compact();
  expect(pending instanceof Promise, 'compact returns a Promise');
  expect((await pending) === 3n, 'compact reclaims the expired entry');
  const aborted = new AbortController();
  aborted.abort();
  await rejects(
    store.compact({ signal: aborted.signal }),
    (e) => e instanceof CancelledError && e.code === -5,
    'a cancelled compact',
  );
  expect((await store.compact({ signal: new AbortController().signal })) === 0n, 'compact with a live signal');
  if (!isWasm(api)) {
    // The pending call keeps the store alive past close().
    const p = store.compact();
    store.close();
    expect((await p) === 0n, 'a pending call survives close()');
  } else {
    store.close();
  }
  await rejects(store.compact(), isClosed, 'compact after close');
  empty.close();
}

await main();
await finish(api, 'kvstore');
