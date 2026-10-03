// Conformance consumer: calculator sample, Dart target.
//
// Direct scalars, the typed CalcException domain, and strings crossing as
// (ptr, len) UTF-8 runs, including interior NUL and non-ASCII text.

import 'package:calculator/calculator.dart' as calc;

import 'support.dart';

void run() {
  expect(calc.add(2, 3) == 5, 'add');
  expect(calc.mul(-4, 6) == -24, 'mul');
  expect(calc.div(17, 5) == 3, 'div');

  final e = expectThrows<calc.DivisionByZeroException>(
      () => calc.div(1, 0), 'div by zero');
  expect(e.code == 1, 'DivisionByZero code (got ${e.code})');
  expect(e.message == 'division by zero', 'message (got ${e.message})');
  expect(e is calc.CalcException && e is calc.NativeException,
      'domain hierarchy');

  for (final s in <String>['', 'hello', 'héllo wörld ✓ \u{1F600}', 'a\u0000b']) {
    final back = calc.echo(s);
    expect(back == s, 'echo round trip (got ${back.codeUnits})');
  }
  final nul = calc.echo('nul\u0000inside');
  expect(nul.length == 10 && nul.codeUnitAt(3) == 0, 'interior NUL survives');
}

Future<void> main() async {
  run();
  await expectNoLeaks('calculator');
  print('dart/calculator: OK');
}
