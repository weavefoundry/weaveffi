// Conformance consumer: events sample, Node (N-API) target.
//
// Drives the ABI 2 callback-interface surface through the generated wrapper
// layer (index.js): a `Subscriber` implemented as a plain JS object (route ->
// Delivery, onMessage -> running count, onAttached receiving the EventBus
// object the producer hands over), the reference-counted `EventBus` class
// (constructor, methods, idempotent `close()`, use-after-close trap), the
// `Delivery` return steering `publish`'s accepted count, the Promise-backed
// `publishLater` whose callbacks hop from the producer thread onto the JS
// thread, the lazy iterable `messages()`, the nullable `lastMessage()`
// record, the free function `routeOnce`, and a subscriber that throws: the
// exception surfaces to the caller as a WeaveFFIError with code -4 (foreign
// error) without aborting the process. i64 values cross as BigInt. The
// harness passes the built addon via WV_ADDON; the generated loader honors
// WEAVEFFI_ADDON.

'use strict';

const path = require('path');

const ADDON = path.resolve(process.env.WV_ADDON);
process.env.WEAVEFFI_ADDON = ADDON;
// WV_ADDON = <target>/conformance-build/node-events/build/Release/index.node;
// the generated files sit at <target>/conformance-gen/events/node/.
const wv = require(
  path.resolve(ADDON, '../../../../..', 'conformance-gen', 'events', 'node', 'index.js')
);

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}

// The C-style enum is exported as a frozen object with forward and reverse
// mappings, the runtime value `types.d.ts` declares as `export enum`.
const Delivery = wv.Delivery;
expect(Delivery.Accept === 0 && Delivery.Skip === 1 && Delivery.AcceptAndStop === 2, 'Delivery values');
expect(Delivery[2] === 'AcceptAndStop', 'Delivery reverse mapping');
expect(Object.isFrozen(Delivery), 'Delivery is frozen');

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
    // The object is usable right away: `subscribe` attaches before it
    // appends, so the count seen here is the pre-subscribe count.
    expect(bus instanceof wv.EventBus, this.name + ': onAttached receives an EventBus');
    this.attachedCounts.push(bus.subscriberCount());
    this.attachedBuses.push(bus);
  }
}

(async () => {
  const bus = new wv.EventBus();
  expect(bus instanceof wv.EventBus, 'constructor returns an EventBus');
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
  // Releasing the adopted references leaves the original alive.
  for (const adopted of a.attachedBuses.concat(b.attachedBuses)) {
    adopted.close();
    adopted.close();
  }
  expect(bus.subscriberCount() === 2n, 'bus alive after adopted wrappers closed');

  // Async publish: the producer thread calls route/onMessage, which hop to
  // the JS thread while this frame awaits.
  const later = bus.publishLater('news', 'later');
  expect(later instanceof Promise, 'publishLater returns a Promise');
  const n4 = await later;
  expect(n4 === 2n, `publishLater accepted by 2 (got ${n4})`);
  expect(a.received.length === 3 && a.received[2].text === 'later', 'a received the async message');
  expect(b.received.length === 3 && b.received[2].seq === 4n, 'b received the async message');
  expect(a.received[2].tags.length === 0, 'publishLater sends no tags');

  // Iterator and nullable record.
  const msgs = [...bus.messages()];
  expect(
    msgs.join('|') === 'hello|psst|last|later',
    `messages() in order (got ${JSON.stringify(msgs)})`
  );
  const last = bus.lastMessage();
  expect(last !== null && typeof last === 'object', 'lastMessage present');
  expect(last.seq === 4n && last.topic === 'news' && last.text === 'later', 'lastMessage fields');
  expect(Array.isArray(last.tags) && last.tags.length === 0, 'lastMessage tags empty');

  // Early return closes the underlying producer iterator without draining.
  const it = bus.messages()[Symbol.iterator]();
  const first = it.next();
  expect(!first.done && first.value === 'hello', 'lazy first element');
  expect(typeof it.return === 'function', 'iterator has return()');
  it.return();
  expect(it.next().done === true, 'iterator done after return()');

  // A subscriber whose route throws: the producer aborts publish and the
  // caller sees the foreign error (code -4) carrying the thrown message.
  const bad = new RecordingSubscriber('bad', { throwOn: 'boom' });
  expect(bus.subscribe(bad) === 3n, 'subscribe bad returns 3n');
  try {
    bus.publish('boom', 'x', []);
    expect(false, 'expected publish to throw when a subscriber throws');
  } catch (e) {
    expect(e instanceof wv.WeaveFFIError, `foreign error is a WeaveFFIError (got ${e && e.constructor.name})`);
    expect(e.code === -4, `foreign error code is -4 (got ${e.code})`);
    expect(
      typeof e.errorMessage === 'string' && e.errorMessage.includes('rejects boom'),
      `foreign error carries the JS message (got ${JSON.stringify(e.errorMessage)})`
    );
  }
  // a and b ran before bad and still accepted; the bus is intact.
  expect(a.received.length === 4 && b.received.length === 4, 'earlier subscribers still delivered');
  expect(bus.subscriberCount() === 3n, 'bus intact after foreign error');
  expect(bus.publish('ok', 'y', []) === 3n, 'publish works again after foreign error');

  // The same through the async path: the Promise rejects with code -4.
  try {
    await bus.publishLater('boom', 'z');
    expect(false, 'expected publishLater to reject when a subscriber throws');
  } catch (e) {
    expect(e instanceof wv.WeaveFFIError && e.code === -4, `async foreign error code -4 (got ${e && e.code})`);
    expect(e.errorMessage.includes('rejects boom'), 'async foreign error carries the JS message');
  }

  // A wrong return type from the callback is also a foreign error.
  const weird = { route() { return 'not a number'; }, onMessage() { return 0n; }, onAttached() {} };
  try {
    wv.routeOnce(weird, 'any');
    expect(false, 'expected routeOnce to throw for a bad return type');
  } catch (e) {
    expect(e instanceof wv.WeaveFFIError && e.code === -4, `bad return type is code -4 (got ${e && e.code})`);
  }

  // routeOnce: a free function taking the callback interface; no bus involved.
  const solo = new RecordingSubscriber('solo', { skip: 'quiet' });
  expect(wv.routeOnce(solo, 'quiet') === Delivery.Skip, 'routeOnce(quiet) == Skip');
  expect(wv.routeOnce(solo, 'news') === Delivery.Accept, 'routeOnce(news) == Accept');
  expect(solo.routed.length === 2 && solo.attachedBuses.length === 0, 'routeOnce calls route only');
  // A plain object literal works as an implementation too.
  const literal = { route: () => Delivery.AcceptAndStop, onMessage: () => 1n, onAttached: () => {} };
  expect(wv.routeOnce(literal, 'x') === Delivery.AcceptAndStop, 'object literal subscriber');

  // clearSubscribers drops every retained subscriber.
  bus.clearSubscribers();
  expect(bus.subscriberCount() === 0n, 'subscriberCount == 0n after clear');
  expect(bus.publish('news', 'nobody', []) === 0n, 'publish with no subscribers accepts 0');
  const all = [...bus.messages()];
  expect(all.length === 8, `log records every publish, aborted ones included (got ${all.length})`);

  // Idempotent close; use after close is a marshalling error (-3).
  bus.close();
  bus.close();
  try {
    bus.subscriberCount();
    expect(false, 'expected throw for use after close');
  } catch (e) {
    expect(e instanceof wv.WeaveFFIError && e.code === -3, `use after close is code -3 (got ${e && e.code})`);
  }
  if (typeof Symbol.dispose === 'symbol') {
    const disposable = new wv.EventBus();
    disposable[Symbol.dispose]();
    disposable[Symbol.dispose]();
  }

  if (failures > 0) process.exit(1);
  console.log('node/events: OK');
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
