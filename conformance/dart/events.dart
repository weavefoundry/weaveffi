// Conformance consumer: events sample, Dart target.
//
// A consumer-implemented `Subscriber`: `route` (a `Result`-returning method
// on the producer side) and `onMessage` return values synchronously through
// isolate-local trampolines, while the void `onAttached` is forwarded to the
// event loop and receives an `EventBus` it owns. Also covers the
// reference-counted `EventBus` (dispose, double dispose, use after dispose,
// an adopted second reference outliving the original), the `Delivery` enum,
// the buffered `Message` record decoded inside a callback, the lazy
// `messages()` iterator (including an abandoned iteration), the optional
// `lastMessage()`, the Future-backed `publishLater`, Dart exceptions in
// value methods surfacing to the caller as the foreign code (-4), and an
// exception in a void method reaching the zone that passed the subscriber.

import 'dart:async';

import 'package:events/events.dart' as ev;

import 'support.dart';

/// Records every callback. `skipTopic` routes as skip, `stop` as
/// acceptAndStop, `failTopic` throws from `route`, and `failOnMessage`
/// throws from `onMessage`.
class RecordingSubscriber extends ev.Subscriber {
  RecordingSubscriber(this.name,
      {this.skipTopic = '', this.failTopic = '', this.failOnMessage = false});

  final String name;
  final String skipTopic;
  final String failTopic;
  final bool failOnMessage;

  final List<String> routed = <String>[];
  final List<ev.Message> received = <ev.Message>[];
  int attachedCalls = 0;
  int? subscribersSeenOnAttach;
  ev.EventBus? adoptedBus;

  @override
  ev.Delivery route(String topic) {
    routed.add(topic);
    if (topic == failTopic) throw StateError('$name rejected topic $topic');
    if (topic == skipTopic) return ev.Delivery.skip;
    if (topic == 'stop') return ev.Delivery.acceptAndStop;
    return ev.Delivery.accept;
  }

  @override
  int onMessage(ev.Message message) {
    if (failOnMessage) throw ArgumentError('$name cannot take ${message.text}');
    received.add(message);
    return received.length;
  }

  @override
  void onAttached(ev.EventBus bus) {
    attachedCalls++;
    subscribersSeenOnAttach = bus.subscriberCount();
    // The reference is ours: the keeper holds on to it, the others drop it.
    if (name == 'keeper') {
      adoptedBus = bus;
    } else {
      bus.dispose();
    }
  }
}

/// A subscriber whose void `onAttached` throws.
class FailingAttachSubscriber extends ev.Subscriber {
  @override
  ev.Delivery route(String topic) => ev.Delivery.skip;

  @override
  int onMessage(ev.Message message) => 0;

  @override
  void onAttached(ev.EventBus bus) {
    bus.dispose();
    throw StateError('refuses to attach');
  }
}

Future<void> run() async {
  // A free function taking the callback interface; the Delivery return
  // crosses back as its discriminant.
  final probe = RecordingSubscriber('probe', skipTopic: 'quiet', failTopic: 'boom');
  expect(ev.routeOnce(probe, 'quiet') == ev.Delivery.skip, 'routeOnce skip');
  expect(ev.routeOnce(probe, 'news') == ev.Delivery.accept, 'routeOnce accept');
  expect(ev.routeOnce(probe, 'stop') == ev.Delivery.acceptAndStop,
      'routeOnce acceptAndStop');
  final routeFailure = expectThrows<ev.NativeException>(
      () => ev.routeOnce(probe, 'boom'), 'routeOnce failure');
  expect(routeFailure.code == ev.NativeException.foreignCode,
      'Result-returning callback failure is -4 (got ${routeFailure.code})');
  expect(routeFailure.message.contains('probe rejected topic boom'),
      'foreign message (got ${routeFailure.message})');
  expect(probe.routed.join(',') == 'quiet,news,stop,boom', 'routeOnce log');
  expect(ev.Delivery.fromValue(2) == ev.Delivery.acceptAndStop, 'fromValue');

  final bus = ev.EventBus();
  expect(bus.subscriberCount() == 0, 'fresh bus');
  expect(bus.messages().isEmpty, 'no messages');
  expect(bus.lastMessage() == null, 'no last message');

  // subscribe returns the new count; the void onAttached arrives on the
  // event loop with an EventBus reference the subscriber owns.
  final keeper = RecordingSubscriber('keeper', skipTopic: 'quiet');
  final second = RecordingSubscriber('second');
  expect(bus.subscribe(keeper) == 1, 'first subscribe');
  expect(bus.subscribe(second) == 2, 'second subscribe');
  await settle(() => keeper.attachedCalls == 1 && second.attachedCalls == 1,
      'onAttached delivered');
  expect(keeper.subscribersSeenOnAttach == 2 && keeper.adoptedBus != null,
      'keeper adopted a live bus (saw ${keeper.subscribersSeenOnAttach})');
  expect(keeper.adoptedBus!.subscriberCount() == 2, 'adopted bus is the same');

  // publish: route and onMessage run synchronously during the call.
  expect(bus.publish('news', 'hello', <String>['x', 'y']) == 2, 'publish news');
  final m = keeper.received.single;
  expect(m.seq == 1 && m.topic == 'news' && m.text == 'hello', 'message');
  expect(m.tags.join(',') == 'x,y', 'message tags');
  expect(bus.publish('quiet', 'psst', <String>[]) == 1, 'publish quiet');
  expect(second.received.last.seq == 2 && second.received.last.tags.isEmpty,
      'second received quiet');
  expect(bus.publish('stop', 'last', <String>[]) == 1, 'publish stop');
  expect(keeper.received.last.text == 'last' && second.received.length == 2,
      'acceptAndStop');

  // A lazy iterator; each iteration is a fresh native iterator, and an
  // abandoned one is released by its finalizer.
  expect(bus.messages().join('|') == 'hello|psst|last', 'messages');
  expect(bus.messages().first == 'hello', 'abandoned iteration');
  expect(bus.messages().skip(1).first == 'psst', 'skip then first');
  final last = bus.lastMessage()!;
  expect(last.seq == 3 && last.text == 'last', 'lastMessage');

  // A Dart exception in route (a Result-returning producer method) aborts
  // publish with the foreign code; earlier subscribers already accepted.
  final rejecting = RecordingSubscriber('rejecting', failTopic: 'boom');
  expect(bus.subscribe(rejecting) == 3, 'third subscribe');
  final rejected = expectThrows<ev.NativeException>(
      () => bus.publish('boom', 'x', <String>[]), 'route throws');
  expect(rejected.code == -4 && rejected.message.contains('rejected topic boom'),
      'route failure (got $rejected)');
  expect(keeper.received.length == 3 && second.received.length == 3,
      'earlier subscribers accepted boom');
  expect(bus.publish('ok', 'y', <String>['t']) == 3, 'bus usable afterwards');

  // A Dart exception in onMessage (a plain-return producer method).
  final fragile = RecordingSubscriber('fragile', failOnMessage: true);
  expect(bus.subscribe(fragile) == 4, 'fourth subscribe');
  final failed = expectThrows<ev.NativeException>(
      () => bus.publish('any', 'payload', <String>[]), 'onMessage throws');
  expect(failed.code == ev.NativeException.foreignCode &&
          failed.message.contains('fragile cannot take payload'),
      'onMessage failure (got $failed)');

  // An exception in a void method is an uncaught error of the zone that
  // passed the subscriber; the call itself succeeded.
  final zoneErrors = <Object>[];
  runZonedGuarded(() {
    expect(bus.subscribe(FailingAttachSubscriber()) == 5, 'fifth subscribe');
  }, (e, _) => zoneErrors.add(e));
  await settle(() => zoneErrors.isNotEmpty, 'void method error reaches zone');
  expect(zoneErrors.single.toString().contains('refuses to attach'),
      'zone error (got $zoneErrors)');

  bus.clearSubscribers();
  expect(bus.subscriberCount() == 0, 'cleared');
  expect(bus.publish('news', 'nobody', <String>[]) == 0, 'no subscribers');

  // publishLater runs on a producer thread. Value-returning callback
  // methods only run on the isolate's thread, so it is driven with no
  // subscribers attached.
  expect(await bus.publishLater('later', 'zzz') == 0, 'publishLater');
  expect(bus.lastMessage()!.text == 'zzz', 'publishLater logged');

  // Lifetime: the adopted reference outlives the original wrapper.
  bus.dispose();
  bus.dispose();
  final gone = expectThrows<StateError>(() => bus.subscriberCount(), 'disposed');
  expect(gone.message.contains('dispose'), 'use after dispose message');
  final adopted = keeper.adoptedBus!;
  expect(adopted.lastMessage()!.text == 'zzz', 'adopted bus alive');
  expect(adopted.publish('final', 'bye', <String>[]) == 0, 'adopted publish');
  adopted.dispose();

  // A bus dropped while holding subscribers releases them.
  final short = ev.EventBus();
  final tenant = RecordingSubscriber('tenant');
  short.subscribe(tenant);
  await settle(() => tenant.attachedCalls == 1, 'tenant attached');
  short.dispose();
}

Future<void> main() async {
  await run();
  await expectNoLeaks('events');
  print('dart/events: OK');
}
