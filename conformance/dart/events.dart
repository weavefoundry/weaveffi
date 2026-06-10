// Conformance consumer: events sample, Dart target.
//
// Exercises the NativeCallable listener trampoline (register -> the producer
// fires the Dart closure synchronously on send -> unregister stops delivery)
// and the opaque-iterator ABI behind getMessages. Throws (non-zero exit) on
// any mismatch.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

void main() {
  final received = <String>[];
  final sub = wv.registerMessageListener(received.add);
  expect(sub > 0, 'listener id positive');

  wv.sendMessage('alpha');
  wv.sendMessage('beta');
  expect(received.length == 2 && received[0] == 'alpha' && received[1] == 'beta',
      'listener received sends (got $received)');

  final msgs = wv.getMessages().toList();
  expect(msgs.length == 2 && msgs[0] == 'alpha' && msgs[1] == 'beta',
      'iterator yields messages in order (got $msgs)');

  // Unregister stops delivery; the producer still records the message.
  wv.unregisterMessageListener(sub);
  wv.sendMessage('gamma');
  expect(received.length == 2, 'no delivery after unregister (got $received)');
  expect(wv.getMessages().length == 3, 'producer kept recording');

  print('dart/events: OK');
}
