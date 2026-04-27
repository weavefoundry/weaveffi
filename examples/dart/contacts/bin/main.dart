// Dart consumer example for the contacts sample.
//
// Exercises the auto-generated `package:weaveffi/weaveffi.dart` bindings to
// drive the contacts CRUD API end to end:
//   * createContact / countContacts
//   * listContacts  (returns List<Contact>, each owning a native handle)
//   * getContact    (returns a single Contact)
//   * deleteContact
// and demonstrates Dart's equivalent of RAII: each `Contact` exposes a
// `dispose()` method that calls `weaveffi_contacts_Contact_destroy` on the
// underlying handle. A `Finalizer` is also attached as a safety net in case
// the caller forgets to dispose.

import 'package:weaveffi/weaveffi.dart';

String _typeLabel(ContactType t) {
  switch (t) {
    case ContactType.personal:
      return 'Personal';
    case ContactType.work:
      return 'Work';
    case ContactType.other:
      return 'Other';
  }
}

void _printContact(Contact c) {
  final email = c.email != null ? ' <${c.email}>' : '';
  print('  [${c.id}] ${c.firstName} ${c.lastName}$email '
      '(${_typeLabel(c.contactType)})');
}

void main() {
  print('=== Dart Contacts Example ===\n');

  try {
    final aliceId = createContact(
      'Alice',
      'Smith',
      'alice@example.com',
      ContactType.personal,
    );
    print('Created contact #$aliceId');

    final bobId = createContact('Bob', 'Jones', null, ContactType.work);
    print('Created contact #$bobId');

    print('\nTotal: ${countContacts()} contacts\n');

    // `listContacts` hands back a `List<Contact>`. Each element owns a
    // native handle and must be disposed — do it in a `finally` so even a
    // thrown exception still releases the handles (the classic RAII pattern
    // adapted to Dart's explicit `dispose()`).
    final contacts = listContacts();
    try {
      print('All contacts:');
      for (final c in contacts) {
        _printContact(c);
      }
    } finally {
      for (final c in contacts) {
        c.dispose();
      }
    }

    // Fetch a single contact by handle. Same dispose-in-finally pattern.
    print('\nGet contact #$aliceId:');
    final fetched = getContact(aliceId);
    try {
      _printContact(fetched);
    } finally {
      fetched.dispose();
    }

    final deleted = deleteContact(bobId);
    print('\nDeleted contact #$bobId: $deleted');
    print('Total: ${countContacts()} contacts');
  } on WeaveffiException catch (e) {
    print('weaveffi error: $e');
    rethrow;
  }
}
