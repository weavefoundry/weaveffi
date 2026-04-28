// Async stress test for the WASM async lifecycle.
//
// Loads the async-demo .wasm file (path in ASYNC_DEMO_WASM) and uses
// the same `_registerTrampoline` + `_asyncContexts` pattern that the
// WASM generator emits to spawn 1000 concurrent calls to
// weaveffi_tasks_run_n_tasks_async.
//
// Verifies:
//   * every spawned worker fires its callback exactly once
//   * each callback returns the n value passed in
//   * after awaiting all calls, weaveffi_tasks_active_callbacks returns 0
//
// If ASYNC_DEMO_WASM is not set (the wasm32 target is not built in
// non-WASM CI matrix slots), the test prints a SKIP message and exits 0.

import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';

const N_TASKS = 1000;
const TIMEOUT_MS = 30_000;

const wasmPath = process.env.ASYNC_DEMO_WASM;
if (!wasmPath || !existsSync(wasmPath)) {
    console.log('SKIP wasm async_stress: ASYNC_DEMO_WASM not set or missing');
    process.exit(0);
}

const bytes = await readFile(wasmPath);
const { instance } = await WebAssembly.instantiate(bytes, {});
const wasm = instance.exports;

const table = wasm.__indirect_function_table;
if (!table) {
    console.error('async_stress: wasm has no indirect function table');
    process.exit(1);
}

const asyncContexts = new Map();
let nextCtxId = 1;

function asyncHandler(ctxId, _errPtr, result) {
    const ctx = asyncContexts.get(ctxId);
    if (!ctx) return;
    asyncContexts.delete(ctxId);
    ctx.resolve(result);
}

const cbIdx = table.grow(1);
table.set(
    cbIdx,
    new WebAssembly.Function(
        { parameters: ['i32', 'i32', 'i32'], results: [] },
        asyncHandler,
    ),
);

function runOne(n) {
    return new Promise((resolve, reject) => {
        const ctxId = nextCtxId++;
        asyncContexts.set(ctxId, { resolve, reject });
        wasm.weaveffi_tasks_run_n_tasks_async(n, cbIdx, ctxId);
    });
}

const start = Date.now();
const promises = [];
for (let i = 0; i < N_TASKS; i++) {
    promises.push(runOne(i));
}
const timeout = new Promise((_, reject) =>
    setTimeout(() => reject(new Error('timeout waiting for callbacks')), TIMEOUT_MS),
);

try {
    const results = await Promise.race([Promise.all(promises), timeout]);
    for (let i = 0; i < N_TASKS; i++) {
        if (results[i] !== i) {
            console.error(`results[${i}] = ${results[i]}, expected ${i}`);
            process.exit(1);
        }
    }
} catch (e) {
    console.error(String(e));
    process.exit(1);
}

await new Promise((r) => setTimeout(r, 50));
const active = wasm.weaveffi_tasks_active_callbacks(0);
if (active !== 0n && active !== 0) {
    console.error(`active_callbacks = ${active} (expected 0)`);
    process.exit(1);
}

if (asyncContexts.size !== 0) {
    console.error(`async contexts leaked: ${asyncContexts.size} entries`);
    process.exit(1);
}

console.log(`OK (${N_TASKS} tasks in ${Date.now() - start}ms)`);
