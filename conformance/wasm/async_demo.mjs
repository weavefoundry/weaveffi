// Conformance consumer: async-demo sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the Promise-backed async surface of the generated ESM bindings end
// to end: `tasks.runTask` settled through the function-table callback
// trampoline and decoded from a value buffer into a plain object (i64 fields
// as BigInt), the typed rejection (InvalidName extending TaskError extending
// WeaveFFIError) for an empty name, the buffered list-of-records round trip
// through `runBatch`, the direct-scalar `runNTasks`, the sync `cancelTask`,
// and `activeCallbacks` settling to zero once every task body has completed.
// Unlike the cdylib lanes the .wasm is fully self-contained; the async
// completion fires on the same thread via the exported function table.
//
// Inputs come from the harness:
//   WV_WASM: path to the compiled async_demo.wasm
//   WV_JS:   path to the generated weaveffi_wasm.js (ESM)
// Run with: node --experimental-wasm-type-reflection (for WebAssembly.Function).

import fs from 'fs';

const WASM = process.env.WV_WASM;
const JS = process.env.WV_JS;
if (!WASM || !JS) {
  console.error('WV_WASM and WV_JS must be set');
  process.exit(2);
}

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

globalThis.fetch = async (url) => ({ arrayBuffer: async () => fs.readFileSync(url) });

const mod = await import(JS);
const api = await mod.loadWeaveffiWasm(WASM);

// Async record return: the Promise resolves with a plain object whose i64
// field decodes as BigInt.
const result = await api.tasks.runTask('alpha');
expect(result.id > 0n, 'runTask assigns an id');
expect(result.value === 'completed: alpha', `runTask value (got ${result.value})`);
expect(result.success === true, 'runTask success flag');

// Typed async rejection: the empty name settles with the InvalidName subclass.
try {
  await api.tasks.runTask('');
  expect(false, 'expected InvalidName for empty name');
} catch (e) {
  expect(e instanceof mod.InvalidName, `typed subclass (got ${e.constructor.name})`);
  expect(e instanceof mod.TaskError, 'subclass of TaskError');
  expect(e instanceof mod.WeaveFFIError, 'subclass of the brand error');
  expect(e.code === 1, `InvalidName carries code 1 (got ${e.code})`);
}

// Buffered list-of-records both ways.
const batch = await api.tasks.runBatch(['a', 'b', 'c']);
expect(
  batch.map((r) => r.value).join('|') === 'completed: a|completed: b|completed: c',
  'runBatch values'
);
expect(batch.every((r) => r.success), 'runBatch success flags');

// Direct scalar through the async callback.
expect((await api.tasks.runNTasks(7)) === 7, 'runNTasks echoes n');

// Sync functions beside the async ones.
expect(api.tasks.cancelTask(1) === false, 'cancelTask reports not cancelled');

// Every spawned task body has completed by the time its callback fires.
expect(api.tasks.activeCallbacks() === 0n, 'activeCallbacks settles to zero');

console.log('wasm async-demo conformance: OK');
