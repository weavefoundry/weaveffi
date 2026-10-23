// Conformance consumer: calculator sample (node and wasm lanes).
//
// The minimal sample: the load-time checks, i32 arithmetic that wraps,
// range-checked integer arguments, two error domains whose codes share a
// value (dispatched by the callable's domain), a `throws: any` function,
// an optional scalar return, typed arrays in both directions, and strings
// that keep non-ASCII text, astral characters, and interior NULs. Ends with
// every leak counter at zero.

import { expect, finish, live, load, same, throws } from './harness.mjs';

const api = await load('calculator');
const { calculator, CalculatorError } = api;

const I32_MAX = 2147483647;
const I32_MIN = -2147483648;

expect(live(-1) === 1n, 'the sample counts live resources');

expect(calculator.add(2, 3) === 5, 'add');
expect(calculator.add(I32_MAX, 1) === I32_MIN, 'add wraps');
throws(() => calculator.add(I32_MAX + 1, 0), (e) => e instanceof RangeError, 'an i32 past the range');
throws(() => calculator.add(I32_MIN - 1, 0), (e) => e instanceof RangeError, 'an i32 below the range');
throws(() => calculator.add(1.5, 1), (e) => e instanceof RangeError, 'a fractional i32');
throws(() => calculator.add(NaN, 1), (e) => e instanceof RangeError, 'NaN as an i32');
throws(() => calculator.add('1', 2), (e) => e instanceof TypeError, 'a string as an i32');

expect(calculator.divide(10, 2) === 5, 'divide');
expect(calculator.divide(-7, 2) === -3, 'divide rounds toward zero');
expect(calculator.divide(I32_MIN, -1) === I32_MIN, 'divide wraps');
throws(
  () => calculator.divide(1, 0),
  (e) =>
    e instanceof calculator.DivisionByZeroError &&
    e instanceof calculator.CalcError &&
    e instanceof CalculatorError &&
    !(e instanceof calculator.ParseError) &&
    e.code === 1 &&
    calculator.DivisionByZeroError.CODE === 1 &&
    e.message === 'division by zero' &&
    Object.keys(e).every((k) => k === 'name' || k === 'code'),
  'divide by zero is the typed error, with no payload fields',
);

// A second domain whose code 1 is a different error.
expect(calculator.parse('42') === 42 && calculator.parse(' 42 ') === 42, 'parse');
throws(
  () => calculator.parse('4x'),
  (e) =>
    e instanceof calculator.NotANumberError &&
    e instanceof calculator.ParseError &&
    !(e instanceof calculator.CalcError) &&
    e.code === 1 &&
    e.text === '4x' &&
    e.message === 'not a number: 4x',
  'parse("4x") is ParseError.NotANumber with its field',
);
throws(
  () => calculator.parse(''),
  (e) => e instanceof calculator.NotANumberError && e.text === '' && e.message === 'not a number: ',
  'parse("")',
);

// throws: any is the root error with code -1.
expect(calculator.sqrt(9) === 3, 'sqrt');
throws(
  () => calculator.sqrt(-4),
  (e) =>
    e instanceof CalculatorError &&
    !(e instanceof calculator.CalcError) &&
    !(e instanceof calculator.ParseError) &&
    e.code === -1 &&
    e.message === 'cannot take the square root of -4',
  'sqrt(-4) is an untyped error',
);

// An optional scalar return and typed arrays.
expect(calculator.mean([1, 2, 6]) === 3, 'mean');
expect(calculator.mean(Float64Array.of(1, 2, 6)) === 3, 'mean of a Float64Array');
expect(calculator.mean([]) === null, 'the mean of nothing is null');
same(calculator.runningTotal([1, 2, 3]), [1, 3, 6], 'runningTotal');
same(calculator.runningTotal(Int32Array.of(1, 2, 3)), [1, 3, 6], 'runningTotal of an Int32Array');
same(calculator.runningTotal([]), [], 'runningTotal of nothing');
same(calculator.runningTotal([1, 2, 3, I32_MAX]), [1, 3, 6, -2147483643], 'runningTotal wraps');
expect(Array.isArray(calculator.runningTotal([1])), 'a typed-array return is a plain array');
throws(() => calculator.runningTotal([1, 2 ** 31]), (e) => e instanceof RangeError && e.message.includes('values[1]'), 'an element past the range');
throws(() => calculator.runningTotal([0.5]), (e) => e instanceof RangeError, 'a fractional element');
throws(() => calculator.runningTotal(new Float64Array(1)), (e) => e instanceof TypeError, 'the wrong typed array');
throws(() => calculator.runningTotal('123'), (e) => e instanceof TypeError, 'a string as a list');

expect(calculator.greet('World') === 'Hello, World!', 'greet');
expect(calculator.greet('') === 'Hello, !', 'greet an empty name');
expect(calculator.greet('Wörld 🦀') === 'Hello, Wörld 🦀!', 'non-ASCII and astral text');
expect(calculator.greet('a\0b') === 'Hello, a\0b!', 'an interior NUL');
throws(() => calculator.greet(42), (e) => e instanceof TypeError, 'a non-string is a TypeError');

await finish('calculator');
