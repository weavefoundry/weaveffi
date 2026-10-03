// Shared helpers of the JavaScript conformance consumers. The node and wasm
// targets generate the same ES module (only the transport differs), so one
// consumer per sample runs in both lanes: each lane installs the generated
// package into a scratch project and runs the consumer there with
// `node --expose-gc`.

import assert from 'node:assert';

let failures = 0;

export function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    failures++;
  }
}

/** Structural equality with SameValue semantics (BigInt, NaN, -0, bytes). */
export function same(actual, expected, msg) {
  try {
    assert.deepStrictEqual(actual, expected);
  } catch (e) {
    console.error('assertion failed: ' + msg);
    console.error(e.message);
    failures++;
  }
}

/** Expect `fn()` to throw an error satisfying `check`. */
export function throws(fn, check, msg) {
  try {
    fn();
    expect(false, `${msg}: expected a throw`);
  } catch (e) {
    expect(check(e), `${msg} (got ${e && e.constructor && e.constructor.name}: ${e && e.message})`);
  }
}

/** Expect `promise` to reject with an error satisfying `check`. */
export async function rejects(promise, check, msg) {
  try {
    await promise;
    expect(false, `${msg}: expected a rejection`);
  } catch (e) {
    expect(check(e), `${msg} (got ${e && e.constructor && e.constructor.name}: ${e && e.message})`);
  }
}

/**
 * Import the package installed next to the consumer (by path, since a
 * sample may share its name with a Node.js builtin such as `events`) and,
 * on WebAssembly, initialize it.
 */
export async function load(pkg) {
  const api = await import(new URL(`./node_modules/${pkg}/index.js`, import.meta.url));
  if (typeof api.init === 'function') await api.init();
  return api;
}

/** `true` when running against the WebAssembly bindings. */
export function isWasm(api) {
  return typeof api.init === 'function';
}

const KINDS = ['objects', 'callbacks', 'iterators', 'cancel tokens', 'returned buffers'];

/**
 * Force garbage collection until every wrapper the consumer dropped has been
 * finalized, require every native leak counter back at zero, and exit.
 */
export async function finish(api, label) {
  let live = [];
  for (let round = 0; round < 100; round++) {
    globalThis.gc();
    await new Promise((resolve) => setTimeout(resolve, 5));
    live = KINDS.map((_, kind) => api.__debugLive(kind));
    if (live.every((n) => n === 0n)) break;
  }
  live.forEach((n, kind) => expect(n === 0n, `${n} ${KINDS[kind]} still live at exit`));
  if (failures > 0) {
    console.error(`${label}: ${failures} failure(s)`);
    process.exit(1);
  }
  console.log(`${label}: OK`);
}
