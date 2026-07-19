// Conformance consumer: events sample, Wasm (wasm32-unknown-unknown) target.
//
// Exercises the function-table trampoline listener path (register -> the
// producer's emit fires synchronously back into JS during sendMessage ->
// unregister stops delivery) and the lazy iterable getMessages (one producer
// next per step, drained here via spread). Unlike the native targets there
// is no event-loop hop: wasm delivery is same-thread and synchronous, so
// assertions run immediately after each send.
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

// Listener registration returns a plain numeric subscription id.
const received = [];
const sub = api.events.registerMessageListener((message) => received.push(message));
expect(typeof sub === 'number' && sub > 0, 'listener id positive');

// Functions surface in lowerCamelCase (module-prefix-free names under the
// module object). Delivery is synchronous: the callback runs inside each
// sendMessage call.
api.events.sendMessage('alpha');
expect(
  received.length === 1 && received[0] === 'alpha',
  `listener received first send synchronously (got ${JSON.stringify(received)})`
);
api.events.sendMessage('beta');
expect(
  received.length === 2 && received[1] === 'beta',
  `listener received sends in order (got ${JSON.stringify(received)})`
);

// getMessages returns a lazy iterable (one producer next per step); spread
// drains it here.
const msgs = [...api.events.getMessages()];
expect(
  msgs.length === 2 && msgs[0] === 'alpha' && msgs[1] === 'beta',
  `iterator yields messages in order (got ${JSON.stringify(msgs)})`
);

// This module declares no error domain, so the generic brand error class is
// still exported for panic/marshalling failures.
expect(typeof mod.WeaveFFIError === 'function', 'WeaveFFIError exported');

// Unregister stops delivery; the producer still records the message.
api.events.unregisterMessageListener(sub);
api.events.sendMessage('gamma');
expect(received.length === 2, `no delivery after unregister (got ${JSON.stringify(received)})`);
expect([...api.events.getMessages()].length === 3, 'producer kept recording');

// Unregistering an unknown id is a harmless no-op.
api.events.unregisterMessageListener(sub);
api.events.unregisterMessageListener(999999);

if (failures > 0) process.exit(1);
console.log('wasm/events: OK');
