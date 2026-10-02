// Conformance consumer: events sample (node and wasm lanes).
//
// A `Subscriber` callback interface implemented by a class and by object
// literals: `route` (whose producer-side trait method returns
// `Result<Delivery, ForeignError>`) steering delivery, `onMessage` returning
// a bigint, and `onAttached` receiving the EventBus object. Also the
// EventBus class, `publishLater` (whose callbacks run on a producer thread
// on Node.js and hop to the JavaScript thread), the lazy `messages()`
// iterator, a nullable record, `routeOnce`, failures thrown by an
// implementation surfacing as code -4, and leak-free teardown.

import { expect, finish, isWasm, load, rejects, same, throws } from './harness.mjs';

const api = await load('events');
const { events, EventsError } = api;
const { Delivery } = events;

class Recorder {
  constructor(name, opts = {}) {
    this.name = name;
    this.opts = opts;
    this.routed = [];
    this.received = [];
    this.attached = [];
  }

  route(topic) {
    this.routed.push(topic);
    if (topic === this.opts.throwOn) throw new Error(`subscriber ${this.name} rejects ${topic}`);
    if (topic === this.opts.skip) return Delivery.Skip;
    if (topic === this.opts.stop) return Delivery.AcceptAndStop;
    return Delivery.Accept;
  }

  onMessage(message) {
    this.received.push(message);
    return BigInt(this.received.length);
  }

  onAttached(bus) {
    expect(bus instanceof events.EventBus, `${this.name}: onAttached receives an EventBus`);
    this.attached.push(bus.subscriberCount());
    // The adopted wrapper is this subscriber's to release.
    bus.close();
  }
}

async function main() {
  expect(Delivery.Accept === 0 && Delivery.Skip === 1 && Delivery.AcceptAndStop === 2, 'Delivery values');

  const bus = new events.EventBus();
  expect(bus.subscriberCount() === 0n && bus.lastMessage() === null, 'fresh bus');
  same([...bus.messages()], [], 'no messages yet');

  const a = new Recorder('a', { skip: 'quiet', stop: 'stop' });
  const b = new Recorder('b');
  expect(bus.subscribe(a) === 1n && bus.subscribe(b) === 2n, 'subscribe returns the count');
  same(a.attached, [0n], 'a attached before it was added');
  same(b.attached, [1n], 'b attached after a');

  expect(bus.publish('news', 'hello', ['x', 'y']) === 2n, 'both accept');
  same(a.received[0], { seq: 1n, topic: 'news', text: 'hello', tags: ['x', 'y'] }, 'the message record');
  expect(bus.publish('quiet', 'psst', []) === 1n && a.received.length === 1, 'Skip');
  expect(bus.publish('stop', 'last', ['z']) === 1n && b.routed.length === 2, 'AcceptAndStop');

  const later = bus.publishLater('news', 'later');
  expect(later instanceof Promise, 'publishLater returns a Promise');
  expect((await later) === 2n, 'publishLater resolves with the accepted count');
  expect(a.received[2].text === 'later' && b.received[2].seq === 4n, 'async delivery');

  same([...bus.messages()], ['hello', 'psst', 'last', 'later'], 'messages() in order');
  const it = bus.messages();
  expect(it.next().value === 'hello', 'lazy first element');
  it.return();
  expect(it.next().done === true, 'done after return()');
  for (const text of bus.messages()) {
    if (text === 'psst') break;
  }
  same(bus.lastMessage(), { seq: 4n, topic: 'news', text: 'later', tags: [] }, 'lastMessage');

  // `route` returns a Result on the producer side: a thrown exception comes
  // back to the bus as an Err, which it propagates as code -4.
  const bad = new Recorder('bad', { throwOn: 'boom' });
  expect(bus.subscribe(bad) === 3n, 'subscribe bad');
  throws(
    () => bus.publish('boom', 'x', []),
    (e) => e instanceof EventsError && e.code === -4 && e.message.includes('rejects boom'),
    'a Result-returning callback failure',
  );
  expect(a.received.length === 4 && b.received.length === 4, 'earlier subscribers still delivered');
  expect(bus.publish('ok', 'y', []) === 3n, 'the bus works after a callback failure');
  await rejects(
    bus.publishLater('boom', 'z'),
    (e) => e instanceof EventsError && e.code === -4 && e.message.includes('rejects boom'),
    'an async callback failure',
  );

  // `onMessage` returns a plain value: a failure there aborts the call.
  const failing = { route: () => Delivery.Accept, onMessage() { throw new TypeError('no thanks'); }, onAttached: (bus) => bus.close() };
  const solo = new events.EventBus();
  solo.subscribe(failing);
  throws(
    () => solo.publish('t', 'x', []),
    (e) => e instanceof EventsError && e.code === -4 && e.message.includes('no thanks'),
    'a plain-return callback failure',
  );
  // A wrong return type is a failure of the implementation too.
  const weird = { route: () => 'not a number', onMessage: () => 0n, onAttached: () => {} };
  throws(() => events.routeOnce(weird, 'x'), (e) => e instanceof EventsError && e.code === -4, 'a bad return type');
  throws(() => events.routeOnce(null, 'x'), (e) => e instanceof TypeError, 'a missing implementation');
  expect(events.routeOnce(new Recorder('r', { skip: 'quiet' }), 'quiet') === Delivery.Skip, 'routeOnce');
  expect(events.routeOnce({ route: () => Delivery.AcceptAndStop }, 'x') === Delivery.AcceptAndStop, 'an object literal');

  if (!isWasm(api)) {
    // A callback made on a producer thread runs on this thread while the
    // caller awaits, so it may use the bindings freely.
    const nested = { route: () => Delivery.Accept, onMessage: (m) => BigInt(bus.subscriberCount()) + m.seq, onAttached: (b) => b.close() };
    const busy = new events.EventBus();
    busy.subscribe(nested);
    expect((await busy.publishLater('t', 'x')) === 1n, 'a producer-thread callback can call back in');
    busy.close();
  }

  // Release everything. The subscribers are freed when their bus is.
  solo.clearSubscribers();
  solo.close();
  bus.clearSubscribers();
  expect(bus.subscriberCount() === 0n && bus.publish('news', 'nobody', []) === 0n, 'clearSubscribers');
  bus.close();
  bus.close();
  throws(() => bus.subscriberCount(), (e) => e instanceof EventsError && e.code === -3, 'use after close');
  const disposed = new events.EventBus();
  disposed[Symbol.dispose]();
  disposed[Symbol.dispose]();
  throws(() => disposed.subscriberCount(), (e) => e.code === -3, 'Symbol.dispose releases');
}

await main();
await finish(api, 'events');
