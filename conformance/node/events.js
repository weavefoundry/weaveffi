// Conformance consumer: events sample, Node (N-API) target.
//
// Exercises the napi_threadsafe_function listener path (register -> the
// producer fires synchronously on send, the TSFN queues onto the JS thread ->
// unregister releases the TSFN and stops delivery) and the iterator-drained
// getMessages, through the generated wrapper layer (index.js): function names
// are lowerCamelCase with the module prefix stripped by default. Listener
// delivery is asynchronous (event-loop tick), so assertions run after a
// setImmediate boundary. The harness passes the built addon via WV_ADDON; the
// generated loader honors WEAVEFFI_ADDON.

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

const tick = () => new Promise((resolve) => setImmediate(resolve));

(async () => {
  const received = [];
  const sub = wv.registerMessageListener((message) => received.push(message));
  expect(typeof sub === 'number' && sub > 0, 'listener id positive');

  wv.sendMessage('alpha');
  wv.sendMessage('beta');
  await tick();
  expect(
    received.length === 2 && received[0] === 'alpha' && received[1] === 'beta',
    `listener received sends (got ${JSON.stringify(received)})`
  );

  const msgs = wv.getMessages();
  expect(
    Array.isArray(msgs) && msgs.length === 2 && msgs[0] === 'alpha' && msgs[1] === 'beta',
    `iterator yields messages in order (got ${JSON.stringify(msgs)})`
  );

  // Unregister stops delivery; the producer still records the message.
  wv.unregisterMessageListener(sub);
  wv.sendMessage('gamma');
  await tick();
  expect(received.length === 2, `no delivery after unregister (got ${JSON.stringify(received)})`);
  expect(wv.getMessages().length === 3, 'producer kept recording');

  if (failures > 0) process.exit(1);
  console.log('node/events: OK');
})();
