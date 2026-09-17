// Conformance consumer: events sample, Dart target.
//
// Drives the ABI 2 surface end to end: a consumer-implemented `Subscriber`
// callback interface (a Dart class extending the generated abstract class,
// bound through isolate-local NativeCallable vtable trampolines), the
// reference-counted `EventBus` object (constructor, methods, dispose, double
// dispose, use after dispose), an object handed *to* the consumer through a
// callback method (`onAttached` adopts a second `EventBus` wrapper to the
// same native object and keeps it alive past the original's dispose), the
// `Delivery` enum steering `publish`'s accepted count, the buffered
// `Message` record decoded inside a callback, the lazy `Iterable` iterator
// behind `messages()`, the optional `lastMessage()`, the Future-backed
// `publishLater`, and a Dart exception thrown inside a callback surfacing to
// the original caller as `WeaveFFIException` with the foreign code (-4)
// without unwinding through the native frame. Throws (non-zero exit) on any
// mismatch.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

/// A subscriber that records every callback it receives. `skipTopic` is
/// routed as [wv.Delivery.skip], `stop` as [wv.Delivery.acceptAndStop], and
/// `failTopic` throws from `route`; `failOnMessage` throws from `onMessage`.
class RecordingSubscriber extends wv.Subscriber {
  RecordingSubscriber(this.name,
      {this.skipTopic = '', this.failTopic = '', this.failOnMessage = false});

  final String name;
  final String skipTopic;
  final String failTopic;
  final bool failOnMessage;

  final List<String> routed = <String>[];
  final List<wv.Message> received = <wv.Message>[];
  int attachedCalls = 0;
  int? subscribersSeenOnAttach;
  wv.EventBus? adoptedBus;

  @override
  wv.Delivery route(String topic) {
    routed.add(topic);
    if (topic == failTopic) throw StateError('$name rejected topic $topic');
    if (topic == skipTopic) return wv.Delivery.skip;
    if (topic == 'stop') return wv.Delivery.acceptAndStop;
    return wv.Delivery.accept;
  }

  @override
  int onMessage(wv.Message message) {
    if (failOnMessage) throw ArgumentError('$name cannot take ${message.text}');
    received.add(message);
    return received.length;
  }

  @override
  void onAttached(wv.EventBus bus) {
    attachedCalls++;
    // The handed-in object is live and usable right here: the producer calls
    // onAttached before it appends this subscriber, so the count excludes us.
    subscribersSeenOnAttach = bus.subscriberCount();
    // The reference is ours. One subscriber keeps it (released at the end of
    // main); the others drop theirs right away.
    if (name == 'keeper') {
      adoptedBus = bus;
    } else {
      bus.dispose();
    }
  }
}

Future<void> main() async {
  // routeOnce: a free function taking the callback interface, whose Delivery
  // return crosses back as the C enum discriminant.
  final probe = RecordingSubscriber('probe', skipTopic: 'quiet');
  expect(wv.routeOnce(probe, 'quiet') == wv.Delivery.skip, 'routeOnce skip');
  expect(wv.routeOnce(probe, 'news') == wv.Delivery.accept, 'routeOnce accept');
  expect(wv.routeOnce(probe, 'stop') == wv.Delivery.acceptAndStop,
      'routeOnce acceptAndStop');
  expect(probe.routed.join(',') == 'quiet,news,stop',
      'routeOnce delivered each topic (got ${probe.routed})');
  expect(probe.attachedCalls == 0, 'routeOnce never attaches');

  // Enum value mapping matches the producer's discriminants.
  expect(wv.Delivery.accept.value == 0 &&
          wv.Delivery.skip.value == 1 &&
          wv.Delivery.acceptAndStop.value == 2,
      'Delivery discriminants');
  expect(wv.Delivery.fromValue(2) == wv.Delivery.acceptAndStop,
      'Delivery.fromValue');

  // A fresh bus: no subscribers, no messages, no last message.
  final bus = wv.EventBus();
  expect(bus.subscriberCount() == 0, 'fresh bus has no subscribers');
  expect(bus.messages().isEmpty, 'fresh bus has no messages');
  expect(bus.lastMessage() == null, 'fresh bus lastMessage is null');

  // subscribe: onAttached fires synchronously with the bus object, and the
  // return value is the new subscriber count.
  final keeper = RecordingSubscriber('keeper', skipTopic: 'quiet');
  final second = RecordingSubscriber('second');
  expect(bus.subscribe(keeper) == 1, 'first subscribe returns 1');
  expect(keeper.attachedCalls == 1, 'keeper attached once');
  expect(keeper.subscribersSeenOnAttach == 0,
      'keeper saw 0 subscribers inside onAttached '
      '(got ${keeper.subscribersSeenOnAttach})');
  expect(keeper.adoptedBus != null, 'keeper adopted the bus reference');
  expect(bus.subscribe(second) == 2, 'second subscribe returns 2');
  expect(second.attachedCalls == 1, 'second attached once');
  expect(second.subscribersSeenOnAttach == 1,
      'second saw 1 subscriber inside onAttached');
  expect(bus.subscriberCount() == 2, 'subscriberCount == 2');

  // The adopted wrapper is a second reference to the same object.
  expect(keeper.adoptedBus!.subscriberCount() == 2,
      'adopted bus sees the same subscribers');

  // publish: both accept -> 2; the Message record decodes inside onMessage
  // with every field (seq, topic, text, tags list).
  expect(bus.publish('news', 'hello', <String>['x', 'y']) == 2,
      'publish news accepted by 2');
  expect(keeper.received.length == 1 && second.received.length == 1,
      'both received news');
  final m = keeper.received.first;
  expect(m.seq == 1, 'first message seq == 1 (got ${m.seq})');
  expect(m.topic == 'news', 'message topic');
  expect(m.text == 'hello', 'message text');
  expect(m.tags.length == 2 && m.tags[0] == 'x' && m.tags[1] == 'y',
      'message tags (got ${m.tags})');
  expect(second.received.first.seq == 1 && second.received.first.text == 'hello',
      'second decoded the same message');

  // Skip: the keeper routes `quiet` as skip, so only `second` accepts.
  expect(bus.publish('quiet', 'psst', <String>[]) == 1, 'publish quiet -> 1');
  expect(keeper.received.length == 1, 'keeper skipped quiet');
  expect(second.received.length == 2 && second.received.last.seq == 2,
      'second received quiet with seq 2');
  expect(second.received.last.tags.isEmpty, 'empty tags list decodes empty');

  // AcceptAndStop: the keeper (first in line) accepts and stops delivery, so
  // `second` never sees it.
  expect(bus.publish('stop', 'last', <String>[]) == 1, 'publish stop -> 1');
  expect(keeper.received.length == 2 && keeper.received.last.text == 'last',
      'keeper received stop');
  expect(second.received.length == 2, 'second not reached after stop');
  expect(keeper.routed.join(',') == 'news,quiet,stop', 'keeper routing log');
  expect(second.routed.join(',') == 'news,quiet', 'second routing log');

  // Iterator: lazy Iterable over the native iterator, in publish order; a
  // second iteration launches a fresh native iterator, and an abandoned
  // partial iteration is harmless.
  final texts = bus.messages().toList();
  expect(texts.join('|') == 'hello|psst|last',
      'messages in order (got $texts)');
  expect(bus.messages().length == 3, 'second iteration yields 3 again');
  expect(bus.messages().first == 'hello', 'partial iteration');
  expect(bus.messages().skip(1).first == 'psst', 'skip then first');

  // Optional record return.
  final last = bus.lastMessage();
  expect(last != null, 'lastMessage present');
  expect(last!.seq == 3 && last.topic == 'stop' && last.text == 'last',
      'lastMessage is the stop message');

  // Foreign error from route: a Dart exception inside the callback aborts the
  // publish and surfaces to the caller as the generic exception carrying the
  // foreign code and the exception text; the VM does not crash, and the bus
  // stays usable. Earlier subscribers already accepted the message.
  final rejecting = RecordingSubscriber('rejecting', failTopic: 'boom');
  expect(bus.subscribe(rejecting) == 3, 'third subscribe returns 3');
  try {
    bus.publish('boom', 'x', <String>[]);
    throw StateError('expected WeaveFFIException for rejected topic');
  } on wv.WeaveFFIException catch (e) {
    expect(e.code == wv.WeaveFFIException.foreignCode,
        'foreign code -4 (got ${e.code})');
    expect(e.code == -4, 'foreignCode constant is -4');
    expect(e.message.contains('rejecting rejected topic boom'),
        'foreign message carries the Dart exception text (got ${e.message})');
  }
  expect(keeper.received.length == 3 && second.received.length == 3,
      'subscribers ahead of the failing one still accepted boom');
  expect(rejecting.received.isEmpty, 'rejecting never received');
  expect(bus.messages().length == 4, 'the aborted publish was still logged');
  expect(bus.lastMessage()!.text == 'x', 'lastMessage is the aborted one');
  expect(bus.publish('ok', 'y', <String>['t']) == 3,
      'bus usable after a foreign error');
  expect(rejecting.received.length == 1 && rejecting.received.first.seq == 5,
      'rejecting received ok with seq 5');

  // Foreign error from onMessage (a different vtable slot and exception type).
  final fragile = RecordingSubscriber('fragile', failOnMessage: true);
  expect(bus.subscribe(fragile) == 4, 'fourth subscribe returns 4');
  try {
    bus.publish('any', 'payload', <String>[]);
    throw StateError('expected WeaveFFIException for failing onMessage');
  } on wv.WeaveFFIException catch (e) {
    expect(e.code == wv.WeaveFFIException.foreignCode,
        'onMessage foreign code -4 (got ${e.code})');
    expect(e.message.contains('fragile cannot take payload'),
        'onMessage foreign message (got ${e.message})');
  }
  expect(fragile.routed.length == 1, 'fragile was routed once');
  expect(bus.subscriberCount() == 4, 'a failing subscriber stays subscribed');

  // Foreign error from onAttached: subscribe itself fails and the subscriber
  // is not retained.
  final unattachable = FailingAttachSubscriber();
  try {
    bus.subscribe(unattachable);
    throw StateError('expected WeaveFFIException for failing onAttached');
  } on wv.WeaveFFIException catch (e) {
    expect(e.code == -4, 'onAttached foreign code -4 (got ${e.code})');
    expect(e.message.contains('refuses to attach'),
        'onAttached foreign message (got ${e.message})');
  }
  expect(bus.subscriberCount() == 4, 'failed subscribe did not retain');

  // clearSubscribers drops every retained subscriber; publishing then
  // reaches nobody but is still logged.
  bus.clearSubscribers();
  expect(bus.subscriberCount() == 0, 'cleared subscribers');
  expect(bus.publish('news', 'nobody', <String>[]) == 0,
      'publish with no subscribers -> 0');
  expect(keeper.received.length == 5,
      'cleared keeper receives nothing (got ${keeper.received.length})');

  // Async: publishLater runs on a producer thread and settles a Future. The
  // generated Dart callback trampolines are isolate-local, so subscribers
  // can't be driven from that thread; with none attached the call resolves
  // with 0 accepted and the message is logged.
  final laterCount = await bus.publishLater('later', 'zzz');
  expect(laterCount == 0, 'publishLater resolves with 0 (got $laterCount)');
  expect(bus.lastMessage()!.text == 'zzz' && bus.lastMessage()!.seq == 8,
      'publishLater logged its message (got ${bus.lastMessage()!.text})');
  expect(bus.messages().length == 8, 'eight messages logged');
  final second1 = await bus.publishLater('later', 'again');
  expect(second1 == 0 && bus.messages().length == 9, 'second publishLater');

  // Object lifetime: dispose the original wrapper; the adopted reference
  // keeps the native bus alive and fully usable. Double dispose is safe and
  // use after dispose is a StateError rather than a native fault.
  bus.dispose();
  bus.dispose();
  try {
    bus.subscriberCount();
    throw StateError('expected StateError after dispose');
  } on StateError catch (e) {
    expect(e.message.contains('dispose'), 'use after dispose message');
  }
  final adopted = keeper.adoptedBus!;
  expect(adopted.messages().length == 9,
      'adopted reference outlives the original wrapper');
  expect(adopted.publish('final', 'bye', <String>[]) == 0,
      'publish through the adopted reference');
  expect(adopted.lastMessage()!.text == 'bye', 'adopted lastMessage');
  adopted.dispose();
  adopted.dispose();

  // A bus that is dropped while still holding subscribers releases them on
  // its last reference (the consumer's `free` entries run through a listener
  // NativeCallable once the event loop turns).
  final short = wv.EventBus();
  final tenant = RecordingSubscriber('tenant');
  short.subscribe(tenant);
  tenant.adoptedBus?.dispose();
  short.dispose();
  await Future<void>.delayed(Duration.zero);

  print('dart/events: OK');
}

/// A subscriber whose `onAttached` throws, so `subscribe` itself fails.
class FailingAttachSubscriber extends wv.Subscriber {
  @override
  wv.Delivery route(String topic) => wv.Delivery.accept;

  @override
  int onMessage(wv.Message message) => 0;

  @override
  void onAttached(wv.EventBus bus) {
    bus.dispose();
    throw StateError('refuses to attach');
  }
}
