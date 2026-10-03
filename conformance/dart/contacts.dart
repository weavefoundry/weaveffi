// Conformance consumer: contacts sample, Dart target.
//
// The ContactBook interface (unnamed factory, instance methods, dispose and
// use after dispose), the Contact record with an optional string and a
// C-style enum field, the typed ContactsException domain, the nested
// `contacts.groups` module taking and returning the parent's ContactType
// enum and ContactBook interface and reporting the parent's domain, and the
// sibling `directory` root sharing the Contact record and declaring its own
// DirectoryException domain.

import 'package:contacts/contacts.dart' as ct;

import 'support.dart';

void run() {
  final book = ct.ContactBook();

  final alice =
      book.add('Alice', 'Smith', 'alice@example.com', ct.ContactType.work);
  expect(alice.id > 0, 'alice id positive');
  expect(alice.firstName == 'Alice' && alice.lastName == 'Smith', 'names');
  expect(alice.email == 'alice@example.com', 'email');
  expect(alice.contactType == ct.ContactType.work, 'contactType');
  expect(book.get(alice.id).firstName == 'Alice', 'get');

  final bob = book.add('Bob', 'Jones', null, ct.ContactType.personal);
  expect(bob.email == null, 'absent email');
  book.add('Carol', 'Adams', 'carol@example.com', ct.ContactType.work);
  book.add('Dan', 'Brown', null, ct.ContactType.other);
  expect(book.count() == 4, 'count');
  final names = book.list().map((c) => c.firstName).toList()..sort();
  expect(names.join(',') == 'Alice,Bob,Carol,Dan', 'list (got $names)');

  final invalid = expectThrows<ct.ContactsException>(
      () => book.add('', 'Smith', null, ct.ContactType.personal),
      'empty name');
  expect(invalid is ct.InvalidNameException && invalid.code == 1,
      'InvalidName (got $invalid)');
  final missing =
      expectThrows<ct.NotFoundException>(() => book.get(9999), 'missing id');
  expect(missing.code == 2 && missing is ct.NativeException, 'NotFound');

  // Nested module: the parent's enum and interface cross the module
  // boundary, and the parent's error domain applies.
  expect(ct.countOfType(book, ct.ContactType.work) == 2, 'countOfType work');
  expect(ct.countOfType(book, ct.ContactType.other) == 1, 'countOfType other');
  expect(ct.dominantType(book) == ct.ContactType.work, 'dominantType');
  final work = ct.splitByType(book, ct.ContactType.work);
  expect(work.count() == 2, 'splitByType returns a new book');
  expect(work.list().every((c) => c.contactType == ct.ContactType.work),
      'split book holds only work contacts');
  expect(ct.firstOfType(book, ct.ContactType.personal).firstName == 'Bob',
      'firstOfType');
  work.dispose();
  final empty = ct.ContactBook();
  expectThrows<ct.NotFoundException>(
      () => ct.firstOfType(empty, ct.ContactType.work),
      'nested module raises the parent domain');
  expect(ct.dominantType(empty) == ct.ContactType.personal, 'empty book');
  empty.dispose();

  // Sibling root: the shared record crosses into another module tree.
  final card = ct.card(alice);
  expect(card.displayName == 'Smith, Alice', 'card name');
  expect(card.initials == 'AS' && card.hasEmail, 'card initials and email');
  expect(!ct.card(bob).hasEmail, 'card without email');
  final sorted = ct.sorted(book.list());
  expect(sorted.map((c) => c.lastName).join(',') == 'Adams,Brown,Jones,Smith',
      'sorted (got ${sorted.map((c) => c.lastName)})');
  final none = expectThrows<ct.DirectoryException>(
      () => ct.sorted(<ct.Contact>[]), 'sorted empty');
  expect(none is ct.EmptyException && none.code == 1, 'EmptyException');

  expect(book.remove(alice.id), 'remove');
  expect(!book.remove(alice.id), 'second remove');
  expect(book.count() == 3, 'count after remove');

  book.dispose();
  book.dispose();
  final gone = expectThrows<StateError>(() => book.count(), 'use after dispose');
  expect(gone.message.contains('dispose'), 'use after dispose message');
}

Future<void> main() async {
  run();
  await expectNoLeaks('contacts');
  print('dart/contacts: OK');
}
