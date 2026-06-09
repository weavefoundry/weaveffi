// Conformance consumer: contacts sample, Dart target.
//
// Binds through the generated `package:weaveffi` wrapper and asserts the full
// contacts surface: enum marshalling, opaque-handle structs with getters,
// optional strings (null email), list-of-struct returns (the `out_len` +
// `T**` lowering), boolean returns, and the thrown-exception error path.

import 'package:__PKG__/__LIB__.dart' as wv;

void expect(bool cond, String msg) {
  if (!cond) throw StateError('assertion failed: $msg');
}

void main() {
  final alice =
      wv.createContact('Alice', 'Smith', 'alice@example.com', wv.ContactType.work);
  expect(alice > 0, 'alice handle positive');

  final c = wv.getContact(alice);
  expect(c.firstName == 'Alice', 'firstName');
  expect(c.lastName == 'Smith', 'lastName');
  expect(c.email == 'alice@example.com', 'email');
  expect(c.contactType == wv.ContactType.work, 'contactType');

  // Optional string: a missing email round-trips as null.
  final bob = wv.createContact('Bob', 'Jones', null, wv.ContactType.personal);
  final cb = wv.getContact(bob);
  expect(cb.email == null, 'bob email null');
  expect(cb.contactType == wv.ContactType.personal, 'bob contactType');

  expect(wv.countContacts() == 2, 'count == 2');
  final everyone = wv.listContacts();
  expect(everyone.length == 2, 'list length == 2');
  final names = everyone.map((p) => p.firstName).toList()..sort();
  expect(names.join(',') == 'Alice,Bob', 'list names');

  expect(wv.deleteContact(alice) == true, 'delete returns true');
  expect(wv.countContacts() == 1, 'count == 1 after delete');

  try {
    wv.getContact(9999);
    throw StateError('expected WeaveFFIException for missing contact');
  } on wv.WeaveFFIException catch (e) {
    expect(e.code != 0, 'error code non-zero');
  }

  print('dart/contacts: OK');
}
