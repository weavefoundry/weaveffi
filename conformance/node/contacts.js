// Conformance consumer: contacts sample, Node (N-API) target.
//
// Drives the generated wrapper layer (index.js) end to end: the ContactBook
// interface class (canonical `new` constructor, instance methods, per-object
// state), struct returns materialized as real JS objects, optional strings
// (null email), enum-as-int params, list-of-struct returns, the bool return,
// and the typed error surface (NotFoundError / InvalidNameError extending
// ContactsError extending WeaveFFIError, each carrying its stable code). The
// harness passes the built addon via WV_ADDON; the generated loader honors
// WEAVEFFI_ADDON, and the generated index.js lives in the sibling
// conformance-gen tree.

'use strict';

const path = require('path');

const ADDON = path.resolve(process.env.WV_ADDON);
process.env.WEAVEFFI_ADDON = ADDON;
// WV_ADDON = <target>/conformance-build/node-contacts/build/Release/index.node;
// the generated files sit at <target>/conformance-gen/contacts/node/.
const wv = require(
  path.resolve(ADDON, '../../../../..', 'conformance-gen', 'contacts', 'node', 'index.js')
);

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

const ContactType = { Personal: 0, Work: 1, Other: 2 };

const book = new wv.ContactBook();
expect(book instanceof wv.ContactBook, 'book is a ContactBook instance');

// add() returns the stored record with its assigned id (struct return).
const alice = book.add('Alice', 'Smith', 'alice@example.com', ContactType.Work);
expect(typeof alice === 'object' && alice !== null, 'add returns an object');
expect(alice.id > 0, 'alice id positive');

const c = book.get(alice.id);
expect(typeof c === 'object' && c !== null, 'get returns an object');
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
expect(book.count() === 1, 'count == 1 after remove');
expect(book.remove(alice.id) === false, 'second remove returns false');

// Each ContactBook object owns its own state.
const other = new wv.ContactBook();
expect(other.count() === 0, 'fresh book empty');
expect(book.count() === 1, 'first book unaffected');

// Typed errors: a missing id throws the NotFound class (code 2), an empty
// name the InvalidName class (code 1); both are instances of the domain
// class and the generic brand.
try {
  book.get(9999);
  expect(false, 'expected throw for missing contact');
} catch (e) {
  expect(e instanceof wv.NotFoundError, 'NotFoundError instance (got ' + e.name + ')');
  expect(e instanceof wv.ContactsError, 'NotFoundError extends ContactsError');
  expect(e instanceof wv.WeaveFFIError, 'NotFoundError extends WeaveFFIError');
  expect(e.code === 2, 'NotFound code == 2 (got ' + e.code + ')');
  expect(wv.NotFoundError.CODE === 2, 'NotFoundError.CODE == 2');
  expect(typeof e.errorMessage === 'string' && e.errorMessage.length > 0, 'error has message');
}

try {
  book.add('', 'Nameless', null, ContactType.Other);
  expect(false, 'expected throw for empty name');
} catch (e) {
  expect(e instanceof wv.InvalidNameError, 'InvalidNameError instance (got ' + e.name + ')');
  expect(e instanceof wv.ContactsError, 'InvalidNameError extends ContactsError');
  expect(e instanceof wv.WeaveFFIError, 'InvalidNameError extends WeaveFFIError');
  expect(e.code === 1, 'InvalidName code == 1 (got ' + e.code + ')');
}

other.destroy();
book.destroy();

console.log('node/contacts: OK');
