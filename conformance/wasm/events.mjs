// Conformance consumer: events sample, Wasm (wasm32-unknown-unknown) target.
//
// The wasm target declares listeners/callbacks unsupported; the sample opts in
// via `generators.wasm.allow_unsupported`, so the supported surface (send +
// the iterator-drained get_messages) must work and the listener register/
// unregister entry points must exist as explicit stubs that throw.
//
// Inputs come from the harness:
//   WV_WASM: path to the compiled events.wasm
//   WV_JS:   path to the generated weaveffi_wasm.js (ESM)

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

// Functions surface in lowerCamelCase (module-prefix-free names under the
// module object).
api.events.sendMessage('alpha');
api.events.sendMessage('beta');

const msgs = api.events.getMessages();
expect(
  Array.isArray(msgs) && msgs.length === 2 && msgs[0] === 'alpha' && msgs[1] === 'beta',
  `iterator yields messages in order (got ${JSON.stringify(msgs)})`
);

// This module declares no error domain, so the generic brand error class is
// still exported for panic/marshalling failures.
expect(typeof mod.WeaveFFIError === 'function', 'WeaveFFIError exported');

// The unsupported listener surface throws with a clear message instead of
// silently not existing.
expect(typeof api.events.registerMessageListener === 'function', 'register stub exists');
let threw = false;
try {
  api.events.registerMessageListener(() => {});
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
  api.events.unregisterMessageListener(1);
} catch (e) {
  threwUnregister = true;
}
expect(threwUnregister, 'unregister stub throws');

if (failures > 0) process.exit(1);
console.log('wasm/events: OK');
