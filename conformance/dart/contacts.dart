// Conformance consumer: contacts sample, Dart target.
//
// Binds through the generated `package:weaveffi` wrapper and drives the
// ContactBook interface surface: the unnamed factory constructor, instance
// methods passing the object pointer (add/get/list/remove/count), enum
// marshalling, opaque-handle structs with getters, optional strings (null
// email), list-of-struct returns, boolean returns, and the typed
// ContactsException hierarchy (InvalidNameException = 1, NotFoundException
// = 2) raised by throwing methods.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

void main() {
  final book = wv.ContactBook();

  final alice =
      book.add('Alice', 'Smith', 'alice@example.com', wv.ContactType.work);
  expect(alice.id > 0, 'alice id positive');
  expect(alice.firstName == 'Alice', 'firstName');
  expect(alice.lastName == 'Smith', 'lastName');
  expect(alice.email == 'alice@example.com', 'email');
  expect(alice.contactType == wv.ContactType.work, 'contactType');

  final fetched = book.get(alice.id);
  expect(fetched.firstName == 'Alice', 'get returns the stored record');

  // Optional string: a missing email round-trips as null.
  final bob = book.add('Bob', 'Jones', null, wv.ContactType.personal);
  expect(bob.email == null, 'bob email null');
  expect(bob.contactType == wv.ContactType.personal, 'bob contactType');

  expect(book.count() == 2, 'count == 2');
  final everyone = book.list();
  expect(everyone.length == 2, 'list length == 2');
  final names = everyone.map((p) => p.firstName).toList()..sort();
  expect(names.join(',') == 'Alice,Bob', 'list names');

  // Typed error: an empty name raises the InvalidNameException class of the
  // ContactsException domain, carrying its stable code.
  try {
    book.add('', 'Smith', null, wv.ContactType.personal);
    throw StateError('expected InvalidNameException for empty name');
  } on wv.ContactsException catch (e) {
    expect(e is wv.InvalidNameException, 'InvalidName subclass (got $e)');
    expect(e.code == 1, 'InvalidName code == 1 (got ${e.code})');
  }

  expect(book.remove(alice.id) == true, 'remove returns true');
  expect(book.count() == 1, 'count == 1 after remove');
  expect(book.remove(alice.id) == false, 'second remove returns false');

  // Typed error: a missing id raises NotFoundException, which is also
  // catchable as the domain and the generic brand exception.
  try {
    book.get(9999);
    throw StateError('expected NotFoundException for missing contact');
  } on wv.NotFoundException catch (e) {
    expect(e.code == 2, 'NotFound code == 2 (got ${e.code})');
    expect(e is wv.ContactsException, 'NotFound extends ContactsException');
    expect(e is wv.WeaveFFIException, 'NotFound extends the generic brand');
  }

  book.dispose();
  print('dart/contacts: OK');
}
