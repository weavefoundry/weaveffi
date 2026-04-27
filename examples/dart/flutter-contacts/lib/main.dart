import 'package:flutter/material.dart';
import 'package:weaveffi/weaveffi.dart' as weaveffi;

void main() {
  runApp(const ContactsApp());
}

class ContactRow {
  const ContactRow({
    required this.name,
    required this.email,
    required this.type,
  });

  final String name;
  final String? email;
  final String type;
}

class ContactsApp extends StatelessWidget {
  const ContactsApp({super.key, this.loader = loadContacts});

  final Future<List<ContactRow>> Function() loader;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'WeaveFFI Contacts',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.indigo),
        useMaterial3: true,
      ),
      home: ContactsPage(loader: loader),
    );
  }
}

class ContactsPage extends StatefulWidget {
  const ContactsPage({super.key, required this.loader});

  final Future<List<ContactRow>> Function() loader;

  @override
  State<ContactsPage> createState() => _ContactsPageState();
}

class _ContactsPageState extends State<ContactsPage> {
  late final Future<List<ContactRow>> _contacts;

  @override
  void initState() {
    super.initState();
    _contacts = widget.loader();
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Contacts')),
      body: FutureBuilder<List<ContactRow>>(
        future: _contacts,
        builder: (context, snapshot) {
          if (snapshot.connectionState != ConnectionState.done) {
            return const Center(child: CircularProgressIndicator());
          }
          if (snapshot.hasError) {
            return Center(child: Text('Unable to load contacts: ${snapshot.error}'));
          }

          final contacts = snapshot.data ?? const <ContactRow>[];
          if (contacts.isEmpty) {
            return const Center(child: Text('No contacts yet'));
          }

          return ListView.separated(
            itemCount: contacts.length,
            separatorBuilder: (_, __) => const Divider(height: 1),
            itemBuilder: (context, index) {
              final contact = contacts[index];
              return ListTile(
                leading: CircleAvatar(child: Text(contact.name[0])),
                title: Text(contact.name),
                subtitle: Text(contact.email ?? 'No email'),
                trailing: Text(contact.type),
              );
            },
          );
        },
      ),
    );
  }
}

Future<List<ContactRow>> loadContacts() async {
  if (weaveffi.countContacts() == 0) {
    weaveffi.createContact(
      'Alice',
      'Smith',
      'alice@example.com',
      weaveffi.ContactType.personal,
    );
    weaveffi.createContact('Bob', 'Jones', null, weaveffi.ContactType.work);
  }

  final nativeContacts = weaveffi.listContacts();
  try {
    return [
      for (final contact in nativeContacts)
        ContactRow(
          name: '${contact.firstName} ${contact.lastName}',
          email: contact.email,
          type: _typeLabel(contact.contactType),
        ),
    ];
  } finally {
    for (final contact in nativeContacts) {
      contact.dispose();
    }
  }
}

String _typeLabel(weaveffi.ContactType type) {
  switch (type) {
    case weaveffi.ContactType.personal:
      return 'Personal';
    case weaveffi.ContactType.work:
      return 'Work';
    case weaveffi.ContactType.other:
      return 'Other';
  }
}
