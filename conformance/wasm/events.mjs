// Conformance consumer: events sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the ABI 2 callback-interface surface of the generated ESM glue
// (loadWeaveffiWasm): a `Subscriber` implemented as a plain JS object whose
// methods the producer reaches through function-table trampolines (route ->
// Delivery, onMessage -> running count as BigInt, onAttached receiving the
// EventBus object the producer hands over), the reference-counted `EventBus`
// class (constructor, methods, idempotent `close()`, `Symbol.dispose`,
// use-after-close trap), the `Delivery` return steering `publish`'s accepted
// count, the Promise-backed `publishLater` (the default wasm32 spawner drives
// the future inline, so the Promise is settled before `await`), the lazy
// iterable `messages()`, the nullable `lastMessage()` record, the free
// function `routeOnce`, and subscribers that throw: wasm32 has no unwinding,
// so the runtime records the failure, the producer runs to completion on the
// callback's default return, the glue refuses any further callback during that
// call, and the wrapper throws a WeaveFFIError with code -4 carrying the JS
// message, leaving the bus usable.
//
// Inputs come from the harness:
//   WV_WASM: path to the compiled events.wasm
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

const { WeaveFFIError, Delivery } = mod;
const { EventBus } = api.events;
expect(typeof WeaveFFIError === 'function', 'WeaveFFIError exported');
expect(typeof EventBus === 'function', 'EventBus class exposed on api.events');
expect(Delivery.Accept === 0 && Delivery.Skip === 1 && Delivery.AcceptAndStop === 2, 'Delivery enum values');

// A subscriber as a class: `route` steers delivery per topic, `onMessage`
// records what it saw and returns its running count (an i64, so a BigInt),
// and `onAttached` adopts the bus object the producer hands over.
class RecordingSubscriber {
  constructor(name, opts) {
    this.name = name;
    this.skipTopic = (opts && opts.skip) || null;
    this.stopTopic = (opts && opts.stop) || null;
    this.throwTopic = (opts && opts.throwOn) || null;
    this.routed = [];
    this.received = [];
    this.attachedBuses = [];
    this.attachedCounts = [];
  }
  route(topic) {
    this.routed.push(topic);
    if (topic === this.throwTopic) {
      throw new Error('subscriber ' + this.name + ' rejects ' + topic);
    }
    if (topic === this.skipTopic) return Delivery.Skip;
    if (topic === this.stopTopic) return Delivery.AcceptAndStop;
    return Delivery.Accept;
  }
  onMessage(message) {
    this.received.push(message);
    return BigInt(this.received.length);
  }
  onAttached(bus) {
    // The object is usable right away (a re-entrant call into the module
    // while `subscribe` is on the stack): `subscribe` attaches before it
    // appends, so the count seen here is the pre-subscribe count.
    expect(bus instanceof EventBus, this.name + ': onAttached receives an EventBus');
    expect(bus._handle > 0, this.name + ': adopted bus has a live handle');
    this.attachedCounts.push(bus.subscriberCount());
    this.attachedBuses.push(bus);
  }
}

const bus = new EventBus();
expect(bus instanceof EventBus, 'constructor returns an EventBus');
expect(bus._handle > 0, 'constructor adopts a non-null handle');
expect(bus.subscriberCount() === 0n, 'fresh bus has no subscribers');
expect(bus.lastMessage() === null, 'lastMessage is null before any publish');
expect([...bus.messages()].length === 0, 'messages() empty before any publish');

// subscribe() returns the new subscriber count and calls onAttached with
// the bus object (one strong reference transferred to the consumer).
const a = new RecordingSubscriber('a', { skip: 'quiet', stop: 'stop' });
const b = new RecordingSubscriber('b');
expect(bus.subscribe(a) === 1n, 'subscribe a returns 1n');
expect(bus.subscribe(b) === 2n, 'subscribe b returns 2n');
expect(bus.subscriberCount() === 2n, 'subscriberCount == 2n');
expect(a.attachedBuses.length === 1, 'a attached once');
expect(a.attachedCounts[0] === 0n, `a saw 0 subscribers at attach (got ${a.attachedCounts[0]})`);
expect(b.attachedCounts[0] === 1n, `b saw 1 subscriber at attach (got ${b.attachedCounts[0]})`);
// The adopted wrapper is a distinct reference to the same producer object.
expect(a.attachedBuses[0] !== bus, 'adopted wrapper is a distinct JS object');
expect(a.attachedBuses[0]._handle === bus._handle, 'adopted wrapper points at the same object');

// Both accept: two deliveries, one Message each with every field decoded.
const n1 = bus.publish('news', 'hello', ['x', 'y']);
expect(typeof n1 === 'bigint', 'publish returns a BigInt');
expect(n1 === 2n, `publish(news) accepted by 2 (got ${n1})`);
expect(a.received.length === 1 && b.received.length === 1, 'both received news');
const m = a.received[0];
expect(m.seq === 1n, `message seq is 1n (got ${m.seq})`);
expect(m.topic === 'news', 'message topic');
expect(m.text === 'hello', 'message text');
expect(
  Array.isArray(m.tags) && m.tags.length === 2 && m.tags[0] === 'x' && m.tags[1] === 'y',
  `message tags (got ${JSON.stringify(m.tags)})`
);
expect(a.routed.length === 1 && a.routed[0] === 'news', 'route called with topic');

// Skip steers the count: a skips "quiet", only b accepts.
const n2 = bus.publish('quiet', 'psst', []);
expect(n2 === 1n, `publish(quiet) accepted by 1 (got ${n2})`);
expect(a.received.length === 1, 'a skipped quiet');
expect(b.received.length === 2 && b.received[1].seq === 2n, 'b received quiet with seq 2n');
expect(b.received[1].tags.length === 0, 'empty tags list decodes as []');

// AcceptAndStop: a accepts and the bus stops before reaching b.
const n3 = bus.publish('stop', 'last', ['z']);
expect(n3 === 1n, `publish(stop) accepted by 1 (got ${n3})`);
expect(a.received.length === 2 && a.received[1].text === 'last', 'a received stop');
expect(b.received.length === 2, 'b not reached after AcceptAndStop');
expect(b.routed.length === 2, 'b.route not consulted after stop');

// The adopted bus is the same underlying object: it observes the same log.
const viaA = a.attachedBuses[0];
expect(viaA.subscriberCount() === 2n, 'adopted bus sees both subscribers');
const viaAMsgs = [...viaA.messages()];
expect(
  viaAMsgs.join('|') === 'hello|psst|last',
  `adopted bus shares the log (got ${JSON.stringify(viaAMsgs)})`
);
const lastViaA = viaA.lastMessage();
expect(lastViaA !== null && lastViaA.seq === 3n && lastViaA.text === 'last', 'adopted bus lastMessage');
// Releasing the adopted references (twice each) leaves the original alive.
for (const adopted of a.attachedBuses.concat(b.attachedBuses)) {
  adopted.close();
  expect(adopted._handle === 0, 'close() zeroes the adopted handle');
  adopted.close();
}
expect(bus.subscriberCount() === 2n, 'bus alive after adopted wrappers closed');
expect(bus.publish('news', 'still', []) === 2n, 'bus still delivers after adopted wrappers closed');

// Async publish: the wasm32 default spawner drives the future inline, so the
// subscribers have already run when the Promise is handed back; `await`
// only hops the microtask queue.
const later = bus.publishLater('news', 'later');
expect(later instanceof Promise, 'publishLater returns a Promise');
expect(a.received.length === 4 && a.received[3].text === 'later', 'inline completion: a received before await');
const n4 = await later;
expect(n4 === 2n, `publishLater accepted by 2 (got ${n4})`);
expect(b.received.length === 4 && b.received[3].seq === 5n, 'b received the async message');
expect(a.received[3].tags.length === 0, 'publishLater sends no tags');

// Iterator and nullable record.
const msgs = [...bus.messages()];
expect(
  msgs.join('|') === 'hello|psst|last|still|later',
  `messages() in order (got ${JSON.stringify(msgs)})`
);
const last = bus.lastMessage();
expect(last !== null && typeof last === 'object', 'lastMessage present');
expect(last.seq === 5n && last.topic === 'news' && last.text === 'later', 'lastMessage fields');
expect(Array.isArray(last.tags) && last.tags.length === 0, 'lastMessage tags empty');

// Lazy iteration: one producer step per next(); early return() closes the
// producer iterator without draining, and for...of break does the same.
const it = bus.messages()[Symbol.iterator]();
const first = it.next();
expect(!first.done && first.value === 'hello', 'lazy first element');
expect(typeof it.return === 'function', 'iterator has return()');
it.return();
expect(it.next().done === true, 'iterator done after return()');
let seen = 0;
for (const text of bus.messages()) {
  seen++;
  if (text === 'psst') break;
}
expect(seen === 2, `for...of break after two steps (got ${seen})`);

// A subscriber whose route throws. There is no unwinding on wasm32, so the
// producer's `publish` keeps running on route's default return (0 = Accept)
// and would call `onMessage` next; the glue refuses that call with the same
// foreign error, and the thunk reports code -4 with the thrown message in
// place of the result.
const bad = new RecordingSubscriber('bad', { throwOn: 'boom' });
expect(bus.subscribe(bad) === 3n, 'subscribe bad returns 3n');
let foreign = null;
try {
  bus.publish('boom', 'x', []);
} catch (e) {
  foreign = e;
}
expect(foreign !== null, 'publish throws when a subscriber throws');
expect(foreign instanceof WeaveFFIError, `foreign error is a WeaveFFIError (got ${foreign && foreign.constructor.name})`);
expect(!(foreign instanceof WebAssembly.RuntimeError), 'no trap leaks out of the call');
expect(foreign && foreign.code === -4, `foreign error code is -4 (got ${foreign && foreign.code})`);
expect(
  foreign && typeof foreign.message === 'string' && foreign.message.includes('subscriber bad rejects boom'),
  `foreign error carries the JS message (got ${JSON.stringify(foreign && foreign.message)})`
);
// a and b ran before bad and still accepted; the bus is intact and keeps
// working (no lock is held across the callback, so nothing stays locked).
expect(a.received.length === 5 && b.received.length === 5, 'earlier subscribers still delivered');
expect(bus.subscriberCount() === 3n, 'bus intact after foreign error');
expect(bus.publish('ok', 'y', []) === 3n, 'publish works again after foreign error');
expect(bad.received.length === 1 && bad.received[0].text === 'y', `throwing subscriber still subscribed and was not consulted again after failing (received ${bad.received.map((m) => m.text).join(',')})`);
expect(bus.lastMessage().text === 'y', 'lastMessage works after foreign error');

// The same through the async path: the failure is recorded during the inline
// launch, and the Promise rejects with code -4.
let asyncForeign = null;
try {
  await bus.publishLater('boom', 'z');
} catch (e) {
  asyncForeign = e;
}
expect(asyncForeign instanceof WeaveFFIError && asyncForeign.code === -4, `async foreign error code -4 (got ${asyncForeign && asyncForeign.code})`);
expect(asyncForeign && asyncForeign.message.includes('rejects boom'), 'async foreign error carries the JS message');
expect((await bus.publishLater('ok', 'again')) === 3n, 'publishLater works again after the rejection');

// A non-BigInt return from onMessage (an i64 slot) fails the coercion inside
// the trampoline, which is a foreign error too.
const weird = { route: () => Delivery.Accept, onMessage: () => undefined, onAttached: () => {} };
expect(bus.subscribe(weird) === 4n, 'subscribe weird returns 4n');
let badReturn = null;
try {
  bus.publish('any', 'w', []);
} catch (e) {
  badReturn = e;
}
expect(badReturn instanceof WeaveFFIError && badReturn.code === -4, `bad return type is code -4 (got ${badReturn && badReturn.code})`);
expect(bus.subscriberCount() === 4n, 'bus intact after bad return');

// routeOnce: a free function taking the callback interface; no bus involved.
const solo = new RecordingSubscriber('solo', { skip: 'quiet' });
expect(api.events.routeOnce(solo, 'quiet') === Delivery.Skip, 'routeOnce(quiet) == Skip');
expect(api.events.routeOnce(solo, 'news') === Delivery.Accept, 'routeOnce(news) == Accept');
expect(solo.routed.length === 2 && solo.attachedBuses.length === 0, 'routeOnce calls route only');
// A plain object literal works as an implementation too.
const literal = { route: () => Delivery.AcceptAndStop, onMessage: () => 1n, onAttached: () => {} };
expect(api.events.routeOnce(literal, 'x') === Delivery.AcceptAndStop, 'object literal subscriber');
// A throwing route through the free function is the same -4.
let onceErr = null;
try {
  api.events.routeOnce(new RecordingSubscriber('once', { throwOn: 't' }), 't');
} catch (e) {
  onceErr = e;
}
expect(onceErr instanceof WeaveFFIError && onceErr.code === -4, `routeOnce foreign error is -4 (got ${onceErr && onceErr.code})`);

// clearSubscribers drops every retained subscriber (each `free(ctx)` entry
// runs synchronously); publishing then accepts nothing.
bus.clearSubscribers();
expect(bus.subscriberCount() === 0n, 'subscriberCount == 0n after clear');
expect(bus.publish('news', 'nobody', []) === 0n, 'publish with no subscribers accepts 0');
expect(
  a.received.length === 9 && b.received.length === 9 && bad.received.length === 3,
  `cleared subscribers no longer receive (got ${a.received.length}/${b.received.length}/${bad.received.length})`
);
const all = [...bus.messages()];
expect(all.length === 11, `log records every publish, aborted ones included (got ${all.length})`);
expect(all[5] === 'x' && all[7] === 'z' && all[9] === 'w', 'aborted publishes were logged before delivery');

// Idempotent close; use after close is a marshalling error (-3), including
// through the async path.
bus.close();
expect(bus._handle === 0, 'close() zeroes the handle');
bus.close();
let closedErr = null;
try {
  bus.subscriberCount();
} catch (e) {
  closedErr = e;
}
expect(closedErr instanceof WeaveFFIError && closedErr.code === -3, `use after close is code -3 (got ${closedErr && closedErr.code})`);
let closedAsync = null;
try {
  await bus.publishLater('x', 'y');
} catch (e) {
  closedAsync = e;
}
expect(closedAsync instanceof WeaveFFIError && closedAsync.code === -3, `async use after close is code -3 (got ${closedAsync && closedAsync.code})`);

// Symbol.dispose releases the same way (twice is safe).
const disposeSym = typeof Symbol.dispose === 'symbol' ? Symbol.dispose : Symbol.for('Symbol.dispose');
const disposable = new EventBus();
expect(typeof disposable[disposeSym] === 'function', 'wrapper implements Symbol.dispose');
disposable[disposeSym]();
expect(disposable._handle === 0, 'Symbol.dispose zeroes the handle');
disposable[disposeSym]();

// A bus that still holds subscribers when its last reference is released
// frees them without calling back into JS in a broken way.
const shortLived = new EventBus();
shortLived.subscribe(new RecordingSubscriber('short'));
shortLived.close();

if (failures > 0) {
  console.error(`wasm/events: ${failures} failure(s)`);
  process.exit(1);
}
console.log('wasm/events: OK');
