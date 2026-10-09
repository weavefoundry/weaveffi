// Conformance consumer: kvstore sample, the feature-complete producer (node
// and wasm lanes).
//
// The `Store` class (a throwing factory, the `new` constructor, methods,
// statics, the deprecated `size`, close()), typed `KvError`s with their
// payload fields, records with bytes, optional, list, and map fields, lazy
// iterators of strings, records, and objects, three callback interfaces
// implemented in JavaScript (a `Listener` that is retained, filtered, and
// detached when it throws; a `Policy` with a record return, a typed error
// raised back through `put`, and object parameters and returns; a `Loader`
// passed as an optional callback with string, bytes, and optional-object
// returns), objects in every buffered position, async calls with `AbortSignal`
// cancellation, the nested `kv.stats` module, the sibling `report` root, and
// leak-free teardown. `__debugLive(1)` (live callbacks) shows when the
// producer released an implementation.
//
// On wasm32 the producer has no threads: compaction pauses for no time and
// notifies inline, so the mid-pause cancellation and the off-thread delivery
// checks run on Node.js only.

import { expect, finish, isWasm, load, rejects, same, throws } from './harness.mjs';

const api = await load('kvstore');
const { kv, report, KvstoreError, CancelledError } = api;
const { EntryKind, Store } = kv;
const wasm = isWasm(api);

const enc = (s) => new TextEncoder().encode(s);
const dec = (b) => new TextDecoder().decode(b);
const callbacks = () => api.__debugLive(1);

function put(store, key, value, kind = EntryKind.Persistent, ttl = null) {
  return store.put(key, enc(value), kind, ttl);
}

const keyNotFound = (key) => (e) =>
  e instanceof kv.KeyNotFoundError && e instanceof kv.KvError && e.code === 1001 && e.key === key;

// 1. Load: the import checked the ABI revision and both contract tables.
expect(api.__debugLive(-1) === 1n, 'the sample counts live resources');

async function constructors() {
  throws(
    () => Store.open(''),
    (e) =>
      e instanceof kv.InvalidPathError &&
      e instanceof KvstoreError &&
      e.code === 1004 &&
      kv.InvalidPathError.CODE === 1004 &&
      e.message === 'invalid path' &&
      Object.keys(e).every((k) => k === 'name' || k === 'code'),
    'open("") is InvalidPath, with no payload',
  );
  const s = new Store();
  expect(s.path() === 'memory', 'new Store() is in memory');
  expect(s.capacity() === Store.defaultCapacity() && Store.defaultCapacity() === 1000000, 'default capacity');
  s.close();
  const opened = await kv.openStore('/async');
  expect(opened instanceof Store && opened.path() === '/async', 'openStore resolves to a Store');
  opened.close();
  await rejects(kv.openStore(''), (e) => e instanceof kv.InvalidPathError, 'openStore("") rejects');
}

function basics() {
  const s = Store.open('/basics');
  same(
    put(s, 'alpha', 'one'),
    { key: 'alpha', value: enc('one'), kind: EntryKind.Persistent, version: 1, expires_at: null, tags: [], metadata: {} },
    'put returns the stored entry',
  );
  const second = put(s, 'alpha', 'two', EntryKind.Volatile);
  expect(second.version === 2 && second.kind === EntryKind.Volatile, 'a second put bumps the version');
  expect(dec(s.get('alpha').value) === 'two', 'get');
  throws(
    () => s.get('nope'),
    (e) => keyNotFound('nope')(e) && e.message === 'key not found: nope',
    'get a missing key',
  );
  expect(s.find('alpha')?.version === 2 && s.find('nope') === null, 'find');

  expect(put(s, 'ttl', 'x', EntryKind.Volatile, 10n).expires_at === 10n, 'a ttl sets expires_at');
  expect(s.now() === 0n, 'the clock starts at zero');
  expect(s.tick(9n) === 9n && s.count() === 2, 'tick(9)');
  expect(s.tick(1) === 10n && s.count() === 1, 'tick(1) expires the entry');
  throws(
    () => s.get('ttl'),
    (e) =>
      e instanceof kv.ExpiredError && e.key === 'ttl' && e.expired_at === 10n && e.message === 'entry ttl expired at 10',
    'get an expired key',
  );
  throws(() => s.get('ttl'), keyNotFound('ttl'), 'the expired read removed it');

  s.setCapacity(1);
  expect(s.capacity() === 1, 'setCapacity');
  put(s, 'alpha', 'three');
  throws(
    () => put(s, 'beta', 'b'),
    (e) => e instanceof kv.StoreFullError && e.capacity === 1,
    'a new key past the capacity',
  );
  s.setCapacity(100);
  throws(
    () => put(s, 'k', 'v', 9),
    (e) => e instanceof KvstoreError && !(e instanceof kv.KvError) && e.code === -3,
    'an undeclared enum value is a marshalling failure',
  );

  put(s, 'beta', 'b');
  expect(s.delete('beta') === true && s.delete('beta') === false, 'delete');
  expect(s.size() === s.count() && s.count() === 1, 'the deprecated size');
  expect(s.clear() === 1 && s.count() === 0, 'clear');
  s.close();
  throws(() => s.count(), (e) => e instanceof KvstoreError && e.code === -3, 'a closed store');
}

function iterators() {
  const s = Store.open('/iter');
  put(s, 'user.bob', 'b');
  put(s, 'user.alice', 'a');
  put(s, 'sys.x', 'xx');

  same([...s.keys(null)], ['sys.x', 'user.alice', 'user.bob'], 'keys in order');
  throws(() => s.keys('zzz'), keyNotFound('zzz'), 'a prefix that matches nothing');
  const partial = s.keys('user.');
  expect(partial.next().value === 'user.alice', 'a lazy first key');
  partial.return();
  expect(partial.next().done && api.__debugLive(2) === 0n, 'abandoning an iterator releases it');

  const entries = [...s.entries('sys.')];
  expect(entries.length === 1 && entries[0].key === 'sys.x' && dec(entries[0].value) === 'xx', 'entries');

  const prefixes = ['user.', 'sys.', 'none.'];
  const parts = [...s.partition(prefixes)];
  expect(parts.length === 3, 'one store per prefix');
  parts.forEach((p, i) => {
    expect(p instanceof Store && p.path() === prefixes[i], `partition ${i} path`);
    expect(p.count() === [2, 1, 0][i], `partition ${i} count`);
    p.close();
  });
  s.close();
}

class Listener {
  constructor({ skip = null, failOn = null } = {}) {
    this.skip = skip;
    this.failOn = failOn;
    this.changes = [];
  }

  accepts(key) {
    if (key === this.failOn) throw new Error('listener refused');
    return key !== this.skip;
  }

  onChange(change) {
    this.changes.push(change);
  }

  count(tag, pred = () => true) {
    return this.changes.filter((c) => c.tag === tag && pred(c)).length;
  }
}

function listeners() {
  const base = callbacks();
  const s = Store.open('/listen');
  const l = new Listener({ skip: 'quiet' });
  const id = s.subscribe(l);
  expect(id > 0 && s.listenerCount() === 1 && callbacks() === base + 1n, 'subscribe');

  // Synchronous calls notify before they return.
  const last = () => l.changes.at(-1);
  put(s, 'a', '1');
  expect(last().tag === 'Put' && last().entry.version === 1 && last().replaced === false, 'a new key');
  put(s, 'a', '2');
  expect(last().tag === 'Put' && last().entry.version === 2 && last().replaced === true, 'a replaced key');
  put(s, 'quiet', 'x');
  expect(l.count('Put') === 2, 'accepts filters a key');
  s.delete('a');
  same(l.changes.at(-1), { tag: 'Removed', key: 'a', expired: false }, 'a delete');
  put(s, 'short', 'x', EntryKind.Volatile, 1n);
  s.tick(1n);
  throws(() => s.get('short'), (e) => e instanceof kv.ExpiredError, 'an expired read');
  same(l.changes.at(-1), { tag: 'Removed', key: 'short', expired: true }, 'an expired read notifies');
  expect(s.clear() === 1, 'clear leaves quiet');
  same(l.changes.at(-1), { tag: 'Cleared', count: 1 }, 'a clear');

  expect(s.unsubscribe(id) === true && callbacks() === base, 'unsubscribe releases the listener');
  expect(s.unsubscribe(id) === false && s.listenerCount() === 0, 'a second unsubscribe');

  // A listener that throws is detached (and released); the put succeeds.
  const failing = new Listener({ failOn: 'boom' });
  s.subscribe(failing);
  put(s, 'fine', '1');
  expect(failing.count('Put') === 1, 'the failing listener sees fine');
  put(s, 'boom', '1');
  expect(s.count() === 2 && s.listenerCount() === 0 && callbacks() === base, 'a throwing listener is detached');

  // Releasing the store releases the listeners it still holds.
  s.subscribe(new Listener());
  s.subscribe(new Listener());
  expect(s.listenerCount() === 2 && callbacks() === base + 2n, 'two more listeners');
  s.close();
  expect(callbacks() === base, 'closing the store releases its listeners');
}

class Policy {
  constructor(other) {
    this.other = other;
    this.admitted = 0;
  }

  admit(entry) {
    this.admitted++;
    expect(entry.version === 0, 'admit sees version 0');
    if (entry.key.startsWith('secret')) {
      throw new kv.RejectedError({ key: entry.key, reason: 'no secrets' }, 'secrets are not stored');
    }
    if (entry.key.startsWith('boom')) throw new Error('policy exploded');
    if (entry.key.startsWith('garbage')) return { key: entry.key };
    return { ...entry, key: 'renamed', kind: EntryKind.Encrypted, tags: ['admitted'] };
  }

  route(key, home) {
    if (key.startsWith('b/')) {
      home.close();
      return this.other;
    }
    if (key.startsWith('null/')) {
      home.close();
      return null;
    }
    return home;
  }
}

function policies() {
  const base = callbacks();
  const s = Store.open('/policy');
  const other = Store.open('/other');
  const p = new Policy(other);
  s.setPolicy(p);
  expect(s.hasPolicy() && callbacks() === base + 1n, 'setPolicy');

  const e = put(s, 'a', '1', EntryKind.Volatile);
  expect(e.key === 'a' && e.version === 1 && e.kind === EntryKind.Encrypted, "admit's rewrite is stored");
  same(e.tags, ['admitted'], "admit's tags");
  put(s, 'b/x', '2', EntryKind.Volatile);
  expect(s.count() === 1 && other.count() === 1, 'route redirects a write');

  throws(
    () => put(s, 'secret', '3'),
    (err) =>
      err instanceof kv.RejectedError &&
      err.code === 1005 &&
      err.key === 'secret' &&
      err.reason === 'no secrets' &&
      err.message === 'secrets are not stored',
    'a typed error from admit reaches the caller with its payload',
  );
  const foreign = (message) => (err) =>
    err instanceof KvstoreError && !(err instanceof kv.KvError) && err.code === -4 && err.message.includes(message);
  throws(() => put(s, 'boom', '4'), foreign('policy exploded'), 'any other exception is -4 with its message');
  // The binding checks returns, so a malformed record or a null object
  // never reaches the producer: each fails the callback (-4) instead.
  throws(() => put(s, 'garbage', '5'), foreign('expected'), 'a malformed admit return');
  throws(() => put(s, 'null/x', '6'), foreign('expected a Store'), 'a null route return');
  expect(s.count() === 1 && other.count() === 1 && p.admitted === 6, 'failed puts change nothing');

  s.setPolicy(new Policy(other));
  expect(callbacks() === base + 1n, 'replacing the policy releases the old one');
  s.setPolicy(null);
  expect(!s.hasPolicy() && callbacks() === base, 'setPolicy(null) releases it');
  put(s, 'secret', 'now allowed');
  expect(s.count() === 2, 'no policy, no veto');
  other.close();
  s.close();
}

class Loader {
  constructor(fallback = null) {
    this.store = fallback;
  }

  name() {
    return 'js-loader';
  }

  fallback(key) {
    return key === 'fb' ? this.store : null;
  }

  load(key) {
    if (key === 'missing') throw new kv.KeyNotFoundError({ key: 'missing' }, 'not in the loader');
    if (key === 'elsewhere') throw new kv.KeyNotFoundError({ key: 'other' }, 'not in the loader');
    if (key === 'broken') throw new Error('loader is broken');
    return enc(`loaded:${key}`);
  }
}

function loaders() {
  const base = callbacks();
  const s = Store.open('/load');
  expect(s.getOrLoad('k', null) === null, 'no loader, no entry');
  const loaded = s.getOrLoad('k', new Loader());
  expect(dec(loaded.value) === 'loaded:k' && loaded.kind === EntryKind.Volatile, 'a loaded entry');
  same(loaded.metadata, { source: 'js-loader' }, 'the loader name is recorded');
  expect(callbacks() === base, 'the loader is released after the call');
  expect(s.getOrLoad('k', new Loader())?.version === 1, 'a hit skips the loader');

  const backup = Store.open('/backup');
  put(backup, 'fb', 'from backup');
  const copied = s.getOrLoad('fb', new Loader(backup));
  expect(dec(copied.value) === 'from backup' && copied.kind === EntryKind.Persistent, 'the fallback store');
  backup.close();

  expect(s.getOrLoad('missing', new Loader()) === null, 'KeyNotFound for this key is none');
  throws(
    () => s.getOrLoad('elsewhere', new Loader()),
    (e) => keyNotFound('other')(e) && e.message === 'not in the loader',
    'KeyNotFound for another key passes through',
  );
  throws(
    () => s.getOrLoad('broken', new Loader()),
    (e) => e instanceof KvstoreError && e.code === -4 && e.message === 'loader is broken',
    'any other loader failure is -4',
  );
  expect(callbacks() === base, 'every loader is released');
  s.close();
}

async function asyncCalls() {
  const s = Store.open('/async-calls');
  const l = new Listener();
  s.subscribe(l);
  put(s, 'old1', 'x', EntryKind.Volatile, 1n);
  put(s, 'old2', 'x', EntryKind.Volatile, 1n);
  put(s, 'keep', 'x');
  s.tick(5n);

  // compact notifies from a producer thread; Node.js runs the listener on
  // the JavaScript thread, so nothing arrives before control returns to the
  // event loop. On wasm32 it notifies inline.
  const expired = () => l.count('Removed', (c) => c.expired);
  const pending = s.compact(0, { signal: new AbortController().signal });
  if (!wasm) expect(expired() === 0, 'off-thread notifications wait for the event loop');
  expect((await pending) === 2, 'compact removes the expired entries');
  expect(expired() === 2 && s.count() === 1, 'the listener saw both removals');
  expect((await s.compact(5)) === 0, 'compact without a signal');
  await rejects(s.compact(0, { signal: AbortSignal.abort() }), (e) => e instanceof CancelledError, 'an aborted signal');

  if (!wasm) {
    const controller = new AbortController();
    const started = Date.now();
    const long = s.compact(60000, { signal: controller.signal });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(Store.activeJobs() >= 1, 'the pause is running');
    controller.abort();
    await rejects(long, (e) => e instanceof CancelledError && e.code === -5, 'cancelled mid-pause');
    expect(Date.now() - started < 5000, 'cancellation is prompt');
    let stopped = false;
    for (let i = 0; i < 200 && !stopped; i++) {
      stopped = Store.activeJobs() === 0;
      if (!stopped) await new Promise((resolve) => setTimeout(resolve, 10));
    }
    expect(stopped, 'the cancelled pause stopped cooperatively');
  }

  const many = await Promise.all(Array.from({ length: 32 }, () => s.getMany(['keep', 'gone', 'keep'])));
  expect(
    many.every((r) => r.length === 3 && r[0]?.key === 'keep' && r[1] === null && r[2]?.key === 'keep'),
    '32 concurrent getMany calls',
  );

  const other = Store.open('/other');
  put(other, 'a', '123');
  same(
    await kv.stats.summarizeAll([s, other]),
    { entries: 2, bytes: 4n, by_kind: { [EntryKind.Persistent]: 2 } },
    'summarizeAll',
  );
  other.close();
  s.close();
}

function objectGraph() {
  const s = Store.open('/graph');
  put(s, 'k', 'v');

  const shared = s.share();
  put(s, 'via-original', 'v');
  expect(shared.find('via-original') !== null, 'share() is the same object');
  s.delete('via-original');
  s.close();
  expect(shared.count() === 1, 'still alive through the shared reference');

  const fork = shared.fork();
  expect(fork.count() === 1 && fork.path() === '/graph', 'fork copies the entries');
  put(fork, 'k2', 'v');
  expect(fork.count() === 2 && shared.count() === 1, 'fork is distinct');

  const empty = Store.open('/empty');
  expect(empty.larger(null) === null, 'larger(null) on an empty store');
  const bigger = empty.larger(fork);
  expect(bigger.count() === 2, 'larger picks the fork');
  bigger.close();
  const self = shared.larger(null);
  put(self, 'via-larger', 'v');
  expect(shared.count() === 2, 'larger(null) on a non-empty store is itself');
  self.delete('via-larger');
  self.close();

  const info = shared.describe('main', fork);
  expect(info.label === 'main' && info.count === 1, 'describe');
  expect(info.store.count() === 1 && info.mirror.count() === 2, 'describe carries both stores');
  put(info.store, 'via-info', 'v');
  expect(shared.count() === 2, 'describe carries this store itself');
  shared.delete('via-info');

  const opened = Store.openMany(['/a', '/b']);
  same(opened.map((o) => o.path()), ['/a', '/b'], 'openMany');
  throws(() => Store.openMany(['/a', '']), (e) => e instanceof kv.InvalidPathError, 'openMany with a bad path');

  const named = Store.byLabel([info, { label: 'first', store: opened[0], mirror: null, count: 0 }]);
  same(Object.keys(named).sort(), ['first', 'main'], 'byLabel keys');
  expect(named.main.count() === 1 && named.first.path() === '/a', 'byLabel values');

  put(opened[0], 'm', '1');
  expect(Store.totalCount([opened[0], opened[1], fork], named, info) === 6, 'totalCount with extra');
  expect(Store.totalCount([opened[0], opened[1], fork], named, null) === 5, 'totalCount without extra');
  expect(shared.count() === 1 && fork.count() === 2 && opened[0].count() === 1, 'still usable after buffers');

  for (const o of [info.store, info.mirror, named.main, named.first, ...opened, empty, fork, shared]) o.close();
}

function statsAndReport() {
  const s = Store.open('/stats');
  put(s, 'b', '12');
  put(s, 'a', '1');
  put(s, 'a', '123');
  put(s, 'c', 'x', EntryKind.Encrypted);
  same(
    kv.stats.summarize(s, null),
    { entries: 3, bytes: 6n, by_kind: { [EntryKind.Persistent]: 2, [EntryKind.Encrypted]: 1 } },
    'summarize',
  );
  throws(() => kv.stats.summarize(s, 'q'), keyNotFound('q'), 'summarize a prefix that matches nothing');
  same(
    report.renderReport([...s.entries(null)]),
    ['a: 3 bytes, Persistent, v2', 'b: 2 bytes, Persistent', 'c: 1 bytes, Encrypted'],
    'renderReport',
  );
  throws(
    () => report.renderReport([]),
    (e) =>
      e instanceof report.NothingToReportError &&
      e instanceof report.ReportError &&
      e.code === 2001 &&
      e.message === 'nothing to report',
    'renderReport([])',
  );
  s.close();
}

await constructors();
basics();
iterators();
listeners();
policies();
loaders();
await asyncCalls();
objectGraph();
statsAndReport();
expect(callbacks() === 0n, 'every callback implementation was released');

await finish(api, 'kvstore');
