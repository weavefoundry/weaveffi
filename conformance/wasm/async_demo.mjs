// Conformance consumer: async-demo sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the Promise-backed async surface of the generated ESM bindings end
// to end: `tasks.runTask` settled through the function-table callback
// trampoline and decoded from a value buffer into a plain object (i64 fields
// as BigInt), the typed rejection (InvalidName extending TaskError extending
// WeaveFFIError) for an empty name, the buffered list-of-records round trip
// through `runBatch`, the direct-scalar `runNTasks`, the sync `cancelTask`,
// and `activeCallbacks` settling to zero once every task body has completed.
//
// A wasm32-unknown-unknown build has no threads, so the runtime's default
// spawner drives each future to completion inline, inside the `_async`
// export: the completion trampoline has already fired, and the Promise is
// already settled, by the time the wrapper returns it. Nothing needs to be
// pumped; `await` only hops the microtask queue. This consumer pins that
// down (completion observed before `await`, ordering preserved, many
// concurrent Promises) while staying correct under a spawner that defers.
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

let failures = 0;
function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}

globalThis.fetch = async (url) => ({ arrayBuffer: async () => fs.readFileSync(url) });

const mod = await import(JS);
const api = await mod.loadWeaveffiWasm(WASM);
const { WeaveFFIError, TaskError, InvalidName } = mod;
expect(typeof TaskError === 'function' && InvalidName.CODE === 1 && TaskError.InvalidName === InvalidName, 'TaskError domain exported');

// Async record return: the Promise resolves with a plain object whose i64
// field decodes as BigInt.
const pending = api.tasks.runTask('alpha');
expect(pending instanceof Promise, 'runTask returns a Promise');
// Inline completion: the task body already ran before the wrapper returned.
expect(api.tasks.activeCallbacks() === 0n, 'inline completion: no callback in flight after launch');
const result = await pending;
expect(typeof result.id === 'bigint' && result.id > 0n, 'runTask assigns an id (BigInt)');
expect(result.value === 'completed: alpha', `runTask value (got ${result.value})`);
expect(result.success === true, 'runTask success flag');

// Typed async rejection: the empty name settles with the InvalidName subclass.
let rejected = null;
try {
  await api.tasks.runTask('');
} catch (e) {
  rejected = e;
}
expect(rejected !== null, 'expected InvalidName for empty name');
expect(rejected instanceof InvalidName, `typed subclass (got ${rejected && rejected.constructor.name})`);
expect(rejected instanceof TaskError && rejected instanceof WeaveFFIError, 'subclass of TaskError and the brand error');
expect(rejected && rejected.code === 1, `InvalidName carries code 1 (got ${rejected && rejected.code})`);
// A rejection through `.catch` too (no await involved).
let caught = null;
await api.tasks.runTask('').catch((e) => { caught = e; });
expect(caught instanceof InvalidName, 'rejection observable through .catch');

// The module keeps working after a rejection.
const beta = await api.tasks.runTask('beta');
expect(beta.value === 'completed: beta' && beta.id > result.id, 'ids keep increasing after a rejection');

// Buffered list-of-records both ways.
const batch = await api.tasks.runBatch(['a', 'b', 'c']);
expect(Array.isArray(batch) && batch.length === 3, 'runBatch returns three records');
expect(
  batch.map((r) => r.value).join('|') === 'completed: a|completed: b|completed: c',
  'runBatch values'
);
expect(batch.every((r) => r.success && typeof r.id === 'bigint'), 'runBatch success flags and BigInt ids');
expect((await api.tasks.runBatch([])).length === 0, 'runBatch of nothing');
const unicodeBatch = await api.tasks.runBatch(['héllo ✓', '']);
expect(unicodeBatch[0].value === 'completed: héllo ✓', 'unicode names survive the async buffer');
expect(unicodeBatch[1].value === 'completed: ' && unicodeBatch[1].success === true, 'runBatch does not validate names');

// Direct scalar through the async callback.
expect((await api.tasks.runNTasks(7)) === 7, 'runNTasks echoes n');
expect((await api.tasks.runNTasks(0)) === 0, 'runNTasks(0)');
expect((await api.tasks.runNTasks(-3)) === -3, 'runNTasks keeps the sign of an i32');

// Many launches at once: every Promise settles independently and in order.
const many = await Promise.all(Array.from({ length: 50 }, (_, i) => api.tasks.runTask('t' + i)));
expect(many.length === 50 && many.every((r, i) => r.value === 'completed: t' + i), 'fifty concurrent runTask calls settle with their own results');
expect(many.every((r, i) => i === 0 || r.id > many[i - 1].id), 'ids assigned in launch order');
const mixed = await Promise.allSettled([api.tasks.runTask('ok'), api.tasks.runTask(''), api.tasks.runNTasks(2)]);
expect(mixed[0].status === 'fulfilled' && mixed[1].status === 'rejected' && mixed[2].status === 'fulfilled', 'interleaved fulfilments and rejections');
expect(mixed[1].reason instanceof InvalidName, 'interleaved rejection keeps its type');

// Sync functions beside the async ones.
expect(api.tasks.cancelTask(1) === false, 'cancelTask reports not cancelled');
expect(api.tasks.cancelTask(1n) === false, 'cancelTask accepts a BigInt id');
expect(typeof api.tasks.cancelTask(99) === 'boolean', 'cancelTask returns a boolean');

// Every spawned task body has completed by the time its callback fires.
expect(api.tasks.activeCallbacks() === 0n, 'activeCallbacks settles to zero');

if (failures > 0) {
  console.error(`wasm/async-demo: ${failures} failure(s)`);
  process.exit(1);
}
console.log('wasm/async-demo: OK');
