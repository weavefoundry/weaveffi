import 'package:flutter_test/flutter_test.dart';
import 'package:weaveffi_flutter_contacts/main.dart';

void main() {
  testWidgets('renders contacts returned by the loader', (tester) async {
    await tester.pumpWidget(
      ContactsApp(
        loader: () async => const [
          ContactRow(
            name: 'Alice Smith',
            email: 'alice@example.com',
            type: 'Personal',
          ),
          ContactRow(name: 'Bob Jones', email: null, type: 'Work'),
        ],
      ),
    );
    await tester.pumpAndSettle();

    expect(find.text('Alice Smith'), findsOneWidget);
    expect(find.text('alice@example.com'), findsOneWidget);
    expect(find.text('Bob Jones'), findsOneWidget);
    expect(find.text('No email'), findsOneWidget);
  });
}
