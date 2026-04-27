// End-to-end consumer test for the WASM binding consumer.
//
// Loads the calculator .wasm file in Node via the generated
// loader (generated/wasm/weaveffi_wasm.js) and asserts that the
// raw exports are reachable and a basic call returns the expected
// result. Prints "OK" on success; any assertion failure prints a
// diagnostic and exits 1.
//
// The generated loader uses fetch(), so we polyfill fetch to
// resolve file:// URLs from disk for Node compatibility.

import { readFile } from 'node:fs/promises'
import { fileURLToPath, pathToFileURL } from 'node:url'
import { dirname, resolve } from 'node:path'

const __dirname = dirname(fileURLToPath(import.meta.url))
const repoRoot = resolve(__dirname, '..', '..')

const wasmPath =
  process.env.CALCULATOR_WASM ||
  resolve(repoRoot, 'target/wasm32-unknown-unknown/release/calculator.wasm')

const realFetch = globalThis.fetch
globalThis.fetch = async (url, init) => {
  const s = String(url)
  if (s.startsWith('file://')) {
    const bytes = await readFile(fileURLToPath(s))
    return new Response(bytes)
  }
  return realFetch(url, init)
}

function check(cond, msg) {
  if (!cond) {
    console.error(`assertion failed: ${msg}`)
    process.exit(1)
  }
}

const loader = await import(pathToFileURL(resolve(repoRoot, 'generated/wasm/weaveffi_wasm.js')))
const api = await loader.loadWeaveffiWasm(pathToFileURL(wasmPath).href)

check(typeof api === 'object' && api !== null, 'loader returned no api')
check(typeof api.calculator === 'object', 'calculator namespace missing')
check(typeof api.calculator.add === 'function', 'calculator.add wrapper missing')
check(typeof api._raw.weaveffi_calculator_add === 'function', 'raw add export missing')

// Call the raw export directly (the generated wrapper depends on a
// weaveffi_alloc helper that the abi-only cdylib does not export).
const errPtr = (api._raw.__heap_base?.value ?? 1024) >>> 0
new Uint8Array(api._raw.memory.buffer, errPtr, 8).fill(0)
const sum = api._raw.weaveffi_calculator_add(2, 3, errPtr)
check(sum === 5, 'raw weaveffi_calculator_add(2,3) != 5')

console.log('OK')
