// Dart consumer example for the SQLite-backed contacts sample.
//
// The generated Dart bindings expose SQLite CRUD calls as `Future<T>` values,
// so consumers can use normal async/await syntax while the Rust sample runs
// the database work on its Tokio runtime.

import 'package:weaveffi/weaveffi.dart';

String _statusLabel(Status status) {
  switch (status) {
    case Status.active:
      return 'Active';
    case Status.archived:
      return 'Archived';
  }
}

void _printContact(String label, Contact contact) {
  final email = contact.email ?? '-';
  print(
    '$label #${contact.id}: ${contact.name} <$email> '
    '(${_statusLabel(contact.status)})',
  );
}

Future<void> main() async {
  print('=== Dart SQLite Contacts Example ===\n');

  final ownedContacts = <Contact>[];
  try {
    final alice = await createContact('Alice', 'alice@example.com');
    ownedContacts.add(alice);
    print('Created #${alice.id} ${alice.name}');

    final bob = await createContact('Bob', null);
    ownedContacts.add(bob);
    print('Created #${bob.id} ${bob.name}');

    final found = await findContact(alice.id);
    try {
      if (found == null) {
        throw StateError('expected to find contact #${alice.id}');
      }
      _printContact('\nFound', found);
    } finally {
      found?.dispose();
    }

    final updated = await updateContact(alice.id, 'alice@new.com');
    print("Updated Alice's email: $updated");

    final refetched = await findContact(alice.id);
    try {
      if (refetched == null) {
        throw StateError('expected to refetch contact #${alice.id}');
      }
      _printContact('Refetched', refetched);
    } finally {
      refetched?.dispose();
    }

    final total = await countContacts(null);
    final active = await countContacts(Status.active);
    print('\nTotals: all=$total active=$active');

    final deleted = await deleteContact(bob.id);
    print('Deleted Bob: $deleted');
    print('Remaining: ${await countContacts(null)}');
  } on WeaveffiException catch (e) {
    print('weaveffi error: $e');
    rethrow;
  } finally {
    for (final contact in ownedContacts) {
      contact.dispose();
    }
  }
}
