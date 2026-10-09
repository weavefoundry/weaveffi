// Conformance consumer: calculator sample, Dart target.
//
// The getting-started surface: a direct-value call (wrapping at the i32
// edges), a throwing call that raises the typed DivisionByZeroException, and
// a string in and out (non-ASCII and astral text and an interior NUL
// survive). Then a second error domain in the same module (`ParseError`,
// whose code 1 must not be mistaken for `CalcError`'s), an untyped
// `throws any` error, an optional scalar return (OptDirect), and numeric
// lists in and out (typed arrays). Loading the bindings checks the ABI
// revision and the `calculator` contract. Ends by asserting the producer's
// leak counters are zero.

import 'dart:typed_data';

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

  // A second domain: its code 1 is `NotANumber`, not `DivisionByZero`.
  expect(calc.parse('42') == 42, 'parse(42)');
  expect(calc.parse(' 42 ') == 42, 'parse trims');
  final nan = expectThrows<calc.NotANumberException>(
    () => calc.parse('4x'),
    'parse(4x) raises NotANumber',
  );
  expect(nan is calc.ParseException, 'NotANumber is a ParseException');
  expect(
    nan.code == 1 && nan.text == '4x' && nan.message == 'not a number: 4x',
    'NotANumber payload and message (got ${nan.code}, ${nan.text}, '
    '${nan.message})',
  );
  final empty = expectThrows<calc.NotANumberException>(
    () => calc.parse(''),
    'parse("") raises NotANumber',
  );
  expect(
    empty.text == '' && empty.message == 'not a number: ',
    'NotANumber of the empty string',
  );

  // `throws any`: a plain NativeException with the generic code.
  expect(calc.sqrt(9.0) == 3.0, 'sqrt(9)');
  final untyped = expectThrows<calc.NativeException>(
    () => calc.sqrt(-4.0),
    'sqrt(-4) raises',
  );
  expect(
    untyped.runtimeType == calc.NativeException &&
        untyped.code == calc.NativeException.genericCode &&
        untyped.message == 'cannot take the square root of -4',
    'untyped error (got ${untyped.runtimeType} ${untyped.code}: '
    '${untyped.message})',
  );

  // A numeric list in (a typed array), an optional scalar out.
  expect(calc.mean([1.0, 2.0, 6.0]) == 3.0, 'mean([1, 2, 6])');
  expect(calc.mean(Float64List.fromList([2.5])) == 2.5, 'mean of a Float64List');
  expect(calc.mean([]) == null, 'mean([]) is absent');

  // Typed arrays both ways, wrapping at the i32 edge.
  expect(listEquals(calc.runningTotal([1, 2, 3]), [1, 3, 6]), 'runningTotal');
  expect(calc.runningTotal([]).isEmpty, 'runningTotal([])');
  final wrapped = calc.runningTotal([1, 2, 3, i32Max]);
  expect(
    listEquals(wrapped, [1, 3, 6, -2147483643]),
    'runningTotal wraps (got $wrapped)',
  );
  expect(
    calc.runningTotal(Int32List.fromList([5, 5])) is Int32List,
    'a returned typed array is an Int32List',
  );
}

bool listEquals<T>(List<T> a, List<T> b) {
  if (a.length != b.length) return false;
  for (var i = 0; i < a.length; i++) {
    if (a[i] != b[i]) return false;
  }
  return true;
}

Future<void> main() async {
  run();
  await expectNoLeaks('calculator');
  print('dart/calculator: OK');
}
