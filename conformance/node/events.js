// Conformance consumer: events sample, Node (N-API) target.
//
// Exercises the napi_threadsafe_function listener path (register -> the
// producer fires synchronously on send, the TSFN queues onto the JS thread ->
// unregister releases the TSFN and stops delivery) and the iterator-drained
// events_get_messages. Listener delivery is asynchronous (event-loop tick), so
// assertions run after a setImmediate boundary.

'use strict';

const addon = require(process.env.WV_ADDON);

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}

const tick = () => new Promise((resolve) => setImmediate(resolve));

(async () => {
  const received = [];
  const sub = addon.events_register_message_listener((message) => received.push(message));
  expect(typeof sub === 'number' && sub > 0, 'listener id positive');

  addon.events_send_message('alpha');
  addon.events_send_message('beta');
  await tick();
  expect(
    received.length === 2 && received[0] === 'alpha' && received[1] === 'beta',
    `listener received sends (got ${JSON.stringify(received)})`
  );

  const msgs = addon.events_get_messages();
  expect(
    Array.isArray(msgs) && msgs.length === 2 && msgs[0] === 'alpha' && msgs[1] === 'beta',
    `iterator yields messages in order (got ${JSON.stringify(msgs)})`
  );

  // Unregister stops delivery; the producer still records the message.
  addon.events_unregister_message_listener(sub);
  addon.events_send_message('gamma');
  await tick();
  expect(received.length === 2, `no delivery after unregister (got ${JSON.stringify(received)})`);
  expect(addon.events_get_messages().length === 3, 'producer kept recording');

  if (failures > 0) process.exit(1);
  console.log('node/events: OK');
})();
