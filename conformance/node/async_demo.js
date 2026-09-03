// Conformance consumer: async-demo sample, Node (N-API) target.
//
// Drives the Promise-backed async surface end to end: `runTask` resolving
// with a TaskResult object decoded from the completion callback's value
// buffer, the typed rejection (InvalidNameError extending TaskError extending
// WeaveFFIError) for an empty name, the buffered list-of-records round trip
// through `runBatch`, the direct-scalar `runNTasks`, the sync `cancelTask`,
// and `activeCallbacks` settling to zero once every task body has completed.
// The harness passes the built addon via WV_ADDON; the generated loader
// honors WEAVEFFI_ADDON, and the generated index.js lives in the sibling
// conformance-gen tree.

'use strict';

const path = require('path');

const ADDON = path.resolve(process.env.WV_ADDON);
process.env.WEAVEFFI_ADDON = ADDON;
// WV_ADDON = <target>/conformance-build/node-async-demo/build/Release/index.node;
// the generated files sit at <target>/conformance-gen/async-demo/node/.
const wv = require(
  path.resolve(ADDON, '../../../../..', 'conformance-gen', 'async-demo', 'node', 'index.js')
);

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

(async () => {
  // Async record return: the Promise resolves with a plain TaskResult object.
  const result = await wv.runTask('alpha');
  expect(typeof result.id === 'bigint' && result.id > 0n, 'runTask assigns a BigInt id');
  expect(result.value === 'completed: alpha', `runTask value (got ${result.value})`);
  expect(result.success === true, 'runTask success flag');

  // Typed async rejection: the empty name settles with InvalidNameError.
  try {
    await wv.runTask('');
    expect(false, 'expected InvalidNameError for empty name');
  } catch (e) {
    expect(e instanceof wv.InvalidNameError, `typed subclass (got ${e.constructor.name})`);
    expect(e instanceof wv.TaskError, 'subclass of TaskError');
    expect(e instanceof wv.WeaveFFIError, 'subclass of the brand error');
    expect(e.code === 1, `InvalidName carries code 1 (got ${e.code})`);
  }

  // Buffered list-of-records both ways.
  const batch = await wv.runBatch(['a', 'b', 'c']);
  expect(
    batch.map((r) => r.value).join('|') === 'completed: a|completed: b|completed: c',
    'runBatch values'
  );
  expect(batch.every((r) => r.success), 'runBatch success flags');

  // Direct scalar through the async callback.
  expect((await wv.runNTasks(7)) === 7, 'runNTasks echoes n');

  // Sync functions beside the async ones.
  expect(wv.cancelTask(1) === false, 'cancelTask reports not cancelled');

  // Every spawned task body has completed by the time its callback fires.
  // i64 returns are BigInt.
  const active = wv.activeCallbacks();
  expect(typeof active === 'bigint', `activeCallbacks is a BigInt (got ${typeof active})`);
  expect(active === 0n, `activeCallbacks settles to zero (got ${active})`);

  console.log('node async-demo conformance: OK');
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
