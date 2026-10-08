// Conformance consumer: calculator sample, Dart target.
//
// The getting-started surface: a direct-value call (wrapping at the i32
// edges), a throwing call that raises the typed DivisionByZeroException, and
// a string in and out (non-ASCII and astral text and an interior NUL
// survive). Loading the bindings checks the ABI revision and the
// `calculator` contract. Ends by asserting the producer's leak counters are
// zero.

import 'package:calculator/calculator.dart' as calc;

import 'support.dart';

const int i32Max = 0x7fffffff;
const int i32Min = -0x80000000;

void run() {
  expect(calc.add(2, 3) == 5, 'add(2, 3)');
  expect(calc.add(-7, 7) == 0, 'add(-7, 7)');
  expect(calc.add(i32Max, 1) == i32Min, 'add wraps');

  expect(calc.divide(10, 2) == 5, 'divide(10, 2)');
  expect(calc.divide(-7, 2) == -3, 'divide rounds toward zero');
  expect(calc.divide(i32Min, -1) == i32Min, 'divide(MIN, -1)');

  final e = expectThrows<calc.DivisionByZeroException>(
    () => calc.divide(1, 0),
    'divide(1, 0) raises DivisionByZero',
  );
  expect(e.code == 1, 'DivisionByZero code (got ${e.code})');
  expect(e.message == 'division by zero', 'message (got ${e.message})');

  expect(calc.greet('World') == 'Hello, World!', 'greet(World)');
  expect(calc.greet('') == 'Hello, !', 'greet(empty)');
  expect(
    calc.greet('Wörld 🦀') == 'Hello, Wörld 🦀!',
    'greet keeps non-ASCII and astral text',
  );
  final nul = calc.greet('a\u0000b');
  expect(
    nul == 'Hello, a\u0000b!',
    'greet keeps an interior NUL (got ${nul.codeUnits})',
  );
}

Future<void> main() async {
  run();
  await expectNoLeaks('calculator');
  print('dart/calculator: OK');
}
