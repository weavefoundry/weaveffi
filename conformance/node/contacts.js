// Conformance consumer: contacts sample, Node (N-API) target.
//
// Drives the compiled addon end to end: enum-as-int params, UTF-8 string params,
// optional strings (null email), the struct-return materialization (a real JS
// object with id/first_name/last_name/email/contact_type, matching types.d.ts;
// previously the addon leaked a raw handle number), list-of-struct returns,
// the bool return, and the thrown-error path. The built addon path comes in via
// WV_ADDON; its dependent cdylib is resolved by absolute install name.

'use strict';

const addon = require(process.env.WV_ADDON);

function expect(cond, msg) {
  if (!cond) {
    console.error('assertion failed: ' + msg);
    process.exit(1);
  }
}

const ContactType = { Personal: 0, Work: 1, Other: 2 };

const alice = addon.contacts_create_contact('Alice', 'Smith', 'alice@example.com', ContactType.Work);
expect(Number(alice) > 0, 'alice handle positive');

const c = addon.contacts_get_contact(alice);
expect(typeof c === 'object' && c !== null, 'get_contact returns an object');
expect(c.first_name === 'Alice', 'first_name (got ' + c.first_name + ')');
expect(c.last_name === 'Smith', 'last_name');
expect(c.email === 'alice@example.com', 'email');
expect(c.contact_type === ContactType.Work, 'contact_type');

// Optional string: a missing email round-trips as null.
const bob = addon.contacts_create_contact('Bob', 'Jones', null, ContactType.Personal);
const cb = addon.contacts_get_contact(bob);
expect(cb.email === null, 'bob email null (got ' + cb.email + ')');
expect(cb.contact_type === ContactType.Personal, 'bob contact_type');

expect(addon.contacts_count_contacts() === 2, 'count == 2');

const everyone = addon.contacts_list_contacts();
expect(Array.isArray(everyone) && everyone.length === 2, 'list length == 2');
const names = everyone.map((p) => p.first_name).sort();
expect(names[0] === 'Alice' && names[1] === 'Bob', 'list names');

expect(addon.contacts_delete_contact(alice) === true, 'delete returns true');
expect(addon.contacts_count_contacts() === 1, 'count == 1 after delete');

// Error path throws an Error carrying the producer's message.
try {
  addon.contacts_get_contact(9999);
  expect(false, 'expected throw for missing contact');
} catch (e) {
  expect(typeof e.message === 'string' && e.message.length > 0, 'error has message');
}

console.log('node/contacts: OK');
