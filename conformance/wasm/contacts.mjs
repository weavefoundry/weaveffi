// Conformance consumer: contacts sample, Wasm (wasm32-unknown-unknown) target.
//
// Drives the generated ESM bindings (loadWeaveffiWasm) against the real
// producer compiled to wasm. Exercises the original struct/enum/optional
// surface: the `ContactBook` interface class (canonical `new` constructor,
// instance methods passing the handle as the implicit self argument, and
// `free()` through the destroy symbol), records decoded from value buffers
// into plain objects (`Contact` with its i64 id as BigInt, optional email,
// and enum-as-int contact_type), NUL-terminated string args, the buffered
// `string?` parameter (null email), list-of-record returns, the bool return,
// and the typed error domain (`InvalidName` / `NotFound` subclasses of
// `ContactsError` with stable codes, thrown by `throws` wrappers).
//
// Inputs come from the harness:
//   WV_WASM: path to the compiled contacts.wasm
//   WV_JS:   path to the generated weaveffi_wasm.js (ESM)
// Run with: node --experimental-wasm-type-reflection (for WebAssembly.Function).

import fs from 'fs';

const WASM = process.env.WV_WASM;
const JS = process.env.WV_JS;
if (!WASM || !JS) {
  console.error('WV_WASM and WV_JS must be set');
  process.exit(2);
}

// Node has no file:// fetch; shim it so the generated loader can read the .wasm.
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

const ContactType = mod.ContactType;
expect(ContactType && ContactType.Work === 1, 'enum ContactType exported');

// Typed error surface: module-scope classes with the domain hierarchy.
const { WeaveFFIError, ContactsError, InvalidName, NotFound } = mod;
expect(typeof WeaveFFIError === 'function', 'WeaveFFIError exported');
expect(typeof ContactsError === 'function', 'ContactsError exported');
expect(InvalidName.CODE === 1, 'InvalidName.CODE === 1');
expect(NotFound.CODE === 2, 'NotFound.CODE === 2');
expect(ContactsError.NotFound === NotFound, 'per-code class aliased on the domain');

// The interface class hangs off the module object.
const ContactBook = api.contacts.ContactBook;
expect(typeof ContactBook === 'function', 'ContactBook class exposed on api.contacts');

// Canonical `new` constructor returns a wrapped owned handle.
const book = new ContactBook();
expect(book instanceof ContactBook, 'new -> instanceof ContactBook');
expect(book._handle > 0, 'new -> non-null handle');
expect(book.count() === 0, 'fresh book empty');

// add() returns the stored record decoded into a plain object; the i64 id
// crosses as BigInt and the enum as its integer discriminant.
const alice = book.add('Alice', 'Smith', 'alice@example.com', ContactType.Work);
expect(typeof alice === 'object' && alice !== null, 'add returns an object');
expect(typeof alice.id === 'bigint' && alice.id > 0n, 'alice.id positive (i64)');

const c = book.get(alice.id);
expect(c.first_name === 'Alice', 'first_name (got ' + c.first_name + ')');
expect(c.last_name === 'Smith', 'last_name');
expect(c.email === 'alice@example.com', 'email');
expect(c.contact_type === ContactType.Work, 'contact_type');

// Optional string: a missing email round-trips as null.
const bob = book.add('Bob', 'Jones', null, ContactType.Personal);
const cb = book.get(bob.id);
expect(cb.email === null, 'bob email null (got ' + cb.email + ')');
expect(cb.contact_type === ContactType.Personal, 'bob contact_type');

expect(book.count() === 2, 'count == 2');

const everyone = book.list();
expect(Array.isArray(everyone) && everyone.length === 2, 'list length == 2');
const names = everyone.map((p) => p.first_name).sort();
expect(names[0] === 'Alice' && names[1] === 'Bob', 'list names');

expect(book.remove(alice.id) === true, 'remove returns true');
expect(book.remove(alice.id) === false, 'second remove returns false');
expect(book.count() === 1, 'count == 1 after remove');

// Each ContactBook object owns its own state.
const other = new ContactBook();
expect(other.count() === 0, 'fresh second book empty');
expect(book.count() === 1, 'first book unaffected');

// Typed errors: a missing id throws the NotFound class (code 2), an empty
// name the InvalidName class (code 1); both are instances of the domain
// class and the generic brand.
let getErr = null;
try { book.get(9999); } catch (e) { getErr = e; }
expect(getErr instanceof NotFound, 'get missing -> instanceof NotFound');
expect(getErr instanceof ContactsError, 'get missing -> instanceof ContactsError');
expect(getErr instanceof WeaveFFIError, 'get missing -> instanceof WeaveFFIError');
expect(getErr && getErr.code === 2, 'get missing -> code 2');
expect(getErr && typeof getErr.message === 'string' && getErr.message.length > 0, 'get missing -> has message');

let addErr = null;
try { book.add('', 'Nameless', null, ContactType.Other); } catch (e) { addErr = e; }
expect(addErr instanceof InvalidName, 'empty name -> instanceof InvalidName');
expect(addErr instanceof ContactsError, 'empty name -> instanceof ContactsError');
expect(addErr && addErr.code === 1, 'empty name -> code 1');
expect(book.count() === 1, 'failed add leaves count == 1');

// Disposal: free() releases the handle via the destroy symbol exactly once.
other.free();
book.free();
expect(book._handle === 0, 'free() zeroes the handle');
book.free(); // second call is a no-op

if (failures === 0) {
  console.log('wasm/contacts: OK');
} else {
  console.error(`wasm/contacts: ${failures} failure(s)`);
  process.exit(1);
}
