// Conformance consumer: async-demo sample (node and wasm lanes).
//
// Promise-returning async functions: a record result, a typed rejection, a
// list of records, a direct result, and the cancellable `wait`, which only
// completes early when cancelled: aborting its `AbortSignal` cancels the
// native token and the promise rejects with `CancelledError` (code -5). On
// WebAssembly every async call completes before its launcher returns, so
// only an already-aborted signal can cancel one there.

import { expect, finish, isWasm, load, rejects, same } from './harness.mjs';

const api = await load('async-demo');
const { tasks, AsyncDemoError, CancelledError } = api;

async function main() {
  same(await tasks.runTask('alpha'), { id: 1n, value: 'completed: alpha', success: true }, 'runTask');
  await rejects(
    tasks.runTask(''),
    (e) => e instanceof tasks.InvalidNameError && e instanceof tasks.TaskError && e instanceof AsyncDemoError && e.code === 1,
    'a typed async rejection',
  );
  same(
    (await tasks.runBatch(['a', 'b', 'c'])).map((r) => r.value),
    ['completed: a', 'completed: b', 'completed: c'],
    'runBatch',
  );
  expect((await tasks.runNTasks(7)) === 7, 'runNTasks');
  const many = await Promise.all(Array.from({ length: 50 }, (_, i) => tasks.runNTasks(i)));
  same(many, Array.from({ length: 50 }, (_, i) => i), 'concurrent calls settle independently');
  await rejects(tasks.runNTasks('7'), (e) => e instanceof TypeError, 'async arguments are type-checked');

  // Cancellation. The sample's timer reads the clock, which traps on
  // wasm32-unknown-unknown, so there only a call cancelled before it starts
  // can run.
  if (!isWasm(api)) {
    expect((await tasks.wait(5n)) === 5n, 'wait completes after its timeout');
    expect((await tasks.wait(5n, {})) === 5n, 'wait without a signal');
  }
  const aborted = new AbortController();
  aborted.abort();
  await rejects(
    tasks.wait(60_000n, { signal: aborted.signal }),
    (e) => e instanceof CancelledError && e instanceof AsyncDemoError && e.code === -5,
    'an already-aborted signal cancels the call',
  );
  if (!isWasm(api)) {
    const controller = new AbortController();
    const started = Date.now();
    const pending = tasks.wait(60_000n, { signal: controller.signal });
    setTimeout(() => controller.abort(), 20);
    await rejects(pending, (e) => e instanceof CancelledError && e.code === -5, 'aborting mid-call cancels it');
    expect(Date.now() - started < 10_000, 'cancellation completes promptly');
    // A signal aborted after the call completed is harmless.
    const late = new AbortController();
    expect((await tasks.wait(1n, { signal: late.signal })) === 1n, 'completes before the abort');
    late.abort();
  }
  expect(tasks.activeCallbacks() === 0n, 'every task body has finished');
}

await main();
await finish(api, 'async-demo');
