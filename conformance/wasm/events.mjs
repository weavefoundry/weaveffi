// Conformance consumer: events sample, WASM (wasm32-unknown-unknown) target.
//
// The wasm target declares listeners/callbacks unsupported; the sample opts in
// via `generators.wasm.allow_unsupported`, so the supported surface (send +
// the iterator-drained get_messages) must work and the listener register/
// unregister entry points must exist as explicit stubs that throw.
//
// Inputs come from the harness:
//   WV_WASM — path to the compiled events.wasm
//   WV_JS   — path to the generated weaveffi_wasm.js (ESM)

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

api.events.send_message('alpha');
api.events.send_message('beta');

const msgs = api.events.get_messages();
expect(
  Array.isArray(msgs) && msgs.length === 2 && msgs[0] === 'alpha' && msgs[1] === 'beta',
  `iterator yields messages in order (got ${JSON.stringify(msgs)})`
);

// The unsupported listener surface throws with a clear message instead of
// silently not existing.
expect(typeof api.events.register_message_listener === 'function', 'register stub exists');
let threw = false;
try {
  api.events.register_message_listener(() => {});
} catch (e) {
  threw = true;
  expect(
    String(e.message).includes('not supported by the wasm target'),
    `stub error names the wasm target (got: ${e.message})`
  );
}
expect(threw, 'register stub throws');

let threwUnregister = false;
try {
  api.events.unregister_message_listener(1);
} catch (e) {
  threwUnregister = true;
}
expect(threwUnregister, 'unregister stub throws');

if (failures > 0) process.exit(1);
console.log('wasm/events: OK');
