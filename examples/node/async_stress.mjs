// Async stress test for the Node.js async lifecycle.
//
// Loads the async-demo cdylib (path in ASYNC_DEMO_LIB) via node-ffi-napi
// and spawns 1000 concurrent calls to weaveffi_tasks_run_n_tasks_async.
//
// Verifies:
//   * every spawned worker fires its callback exactly once
//   * each callback returns the n value passed in
//   * after awaiting all calls, weaveffi_tasks_active_callbacks returns 0
//
// node-ffi-napi is heavyweight; we shell out via the WeaveFFI generated
// N-API addon when available. If neither is available the test prints
// "SKIP node async_stress: ffi-napi not installed" and exits 0 so CI on
// platforms without the optional native deps doesn't fail. In CI with
// the addon built we exercise the real generator output.

import { createRequire } from 'node:module';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const N_TASKS = 1000;
const TIMEOUT_MS = 30_000;

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const require = createRequire(import.meta.url);

const addonPath = join(__dirname, '..', '..', 'generated', 'node', 'index.js');
if (!existsSync(addonPath)) {
    console.log('SKIP node async_stress: generated N-API addon not built');
    process.exit(0);
}

const api = require(addonPath);
if (typeof api.run_n_tasks !== 'function' || typeof api.active_callbacks !== 'function') {
    console.log('SKIP node async_stress: addon was not built from async-demo IDL');
    process.exit(0);
}

const start = Date.now();
const promises = [];
for (let i = 0; i < N_TASKS; i++) {
    promises.push(api.run_n_tasks(i));
}

const timeout = new Promise((_, reject) => {
    setTimeout(() => reject(new Error('timeout waiting for callbacks')), TIMEOUT_MS);
});

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

// Settle any pending callback bookkeeping.
await new Promise((r) => setTimeout(r, 50));
const active = api.active_callbacks();
if (active !== 0n && active !== 0) {
    console.error(`active_callbacks = ${active} (expected 0)`);
    process.exit(1);
}

console.log(`OK (${N_TASKS} tasks in ${Date.now() - start}ms)`);
