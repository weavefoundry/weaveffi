// Conformance consumer: contacts sample (node and wasm lanes).
//
// The ContactBook class (canonical constructor, methods, per-object state),
// records as plain objects, an optional string, a C-style enum, a list of
// records, the typed error classes (NotFoundError and InvalidNameError
// extending the `contacts.ContactsError` domain and the package root), the
// nested `contacts.groups` module taking and returning the parent's enum and
// interface and reporting the parent's error domain, the sibling
// `directory` root taking a `contacts` record, and `close()`.

import { expect, finish, isWasm, load, same, throws } from './harness.mjs';

const api = await load('contacts');
const { contacts, directory, ContactsError } = api;
const { ContactType } = contacts;

function main() {
  expect(ContactType.Personal === 0 && ContactType.Work === 1 && ContactType.Other === 2, 'ContactType values');
  expect(ContactType[1] === 'Work', 'ContactType reverse mapping');
  expect(Object.isFrozen(contacts) && Object.isFrozen(contacts.groups), 'namespaces are frozen');

  const book = new contacts.ContactBook();
  expect(book instanceof contacts.ContactBook, 'book is a ContactBook');
  expect(api.__debugLive(0) === 1n, 'the leak counters see the live object');

  const alice = book.add('Alice', 'Smith', 'alice@example.com', ContactType.Work);
  expect(typeof alice.id === 'bigint' && alice.id > 0n, 'add returns the stored record with a bigint id');
  same(
    book.get(alice.id),
    { id: alice.id, first_name: 'Alice', last_name: 'Smith', email: 'alice@example.com', contact_type: ContactType.Work },
    'get returns every field',
  );
  const bob = book.add('Bob', 'Jones', null, ContactType.Personal);
  expect(book.get(bob.id).email === null, 'a missing email round-trips as null');
  const carol = book.add('Carol', 'Adams', null, ContactType.Work);
  expect(book.count() === 3, 'count is a number');
  same(book.list().map((c) => c.first_name).sort(), ['Alice', 'Bob', 'Carol'], 'list returns every record');

  // The nested module uses the parent's enum and interface.
  expect(contacts.groups.countOfType(book, ContactType.Work) === 2, 'countOfType(Work)');
  expect(contacts.groups.dominantType(book) === ContactType.Work, 'dominantType returns the parent enum');
  const work = contacts.groups.splitByType(book, ContactType.Work);
  expect(work instanceof contacts.ContactBook, 'splitByType returns the parent interface');
  expect(work.count() === 2 && book.count() === 3, 'the split book is a new object');
  expect(contacts.groups.firstOfType(book, ContactType.Personal).first_name === 'Bob', 'firstOfType');
  throws(
    () => contacts.groups.firstOfType(work, ContactType.Other),
    (e) => e instanceof contacts.NotFoundError && e instanceof contacts.ContactsError && e.code === 2,
    'the nested module reports the parent domain',
  );
  work.close();

  // The sibling root takes a record declared in `contacts`.
  same(
    directory.card(book.get(alice.id)),
    { display_name: 'Smith, Alice', initials: 'AS', has_email: true },
    'directory.card takes a contacts record',
  );
  same(
    directory.sorted(book.list()).map((c) => c.last_name),
    ['Adams', 'Jones', 'Smith'],
    'directory.sorted round-trips a list of contacts records',
  );
  throws(
    () => directory.sorted([]),
    (e) => e instanceof directory.EmptyError && e instanceof directory.DirectoryError && e instanceof ContactsError && e.code === 1,
    'directory reports its own domain',
  );
  expect(!(new directory.EmptyError() instanceof contacts.ContactsError), 'the two domains are unrelated');
  expect(directory.EmptyError.CODE === 1 && new directory.EmptyError().message === 'no contacts to list', 'default code message');

  expect(book.remove(carol.id) === true && book.remove(carol.id) === false, 'remove');

  throws(
    () => book.get(9999n),
    (e) => e instanceof contacts.NotFoundError && e instanceof ContactsError && e.code === 2 && e.message === 'contact not found',
    'missing contact',
  );
  throws(
    () => book.add('', 'Nameless', null, ContactType.Other),
    (e) => e instanceof contacts.InvalidNameError && e.code === 1,
    'empty name',
  );
  throws(() => book.add('X', 'Y', null, 'Work'), (e) => e instanceof TypeError, 'an enum must be a number');

  const other = new contacts.ContactBook();
  expect(other.count() === 0 && book.count() === 2, 'each book owns its state');
  other.close();
  other.close();
  book.close();
  throws(() => book.count(), (e) => e instanceof ContactsError && e.code === -3, 'use after close');
}

// On Node.js the addon keeps its state per environment, so a worker thread
// can load and use the bindings alongside the main thread.
async function worker() {
  const { Worker } = await import('node:worker_threads');
  const url = new URL('./node_modules/contacts/index.js', import.meta.url).href;
  const thread = new Worker(
    `const { parentPort } = require('node:worker_threads');
     import(${JSON.stringify(url)}).then(({ contacts }) => {
       const book = new contacts.ContactBook();
       book.add('Wanda', 'Worker', null, contacts.ContactType.Work);
       parentPort.postMessage(contacts.groups.countOfType(book, contacts.ContactType.Work));
       book.close();
     });`,
    { eval: true },
  );
  const count = await new Promise((resolve, reject) => {
    thread.once('message', resolve);
    thread.once('error', reject);
  });
  expect(count === 1, `a worker thread uses the bindings (got ${count})`);
  await thread.terminate();
}

main();
if (!isWasm(api)) await worker();
await finish(api, 'contacts');
