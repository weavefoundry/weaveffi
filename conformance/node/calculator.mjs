// Conformance consumer: calculator sample (node and wasm lanes).
//
// The minimal sample: the load-time checks, i32 arithmetic that wraps, a
// throwing function whose typed error has no payload, and strings that keep
// non-ASCII text, astral characters, and interior NULs. Ends with every leak
// counter at zero.

import { expect, finish, load, throws } from './harness.mjs';

const api = await load('calculator');
const { calculator, CalculatorError } = api;

const I32_MAX = 2147483647;
const I32_MIN = -2147483648;

expect(api.__debugLive(-1) === 1n, 'the sample counts live resources');

expect(calculator.add(2, 3) === 5, 'add');
expect(calculator.add(I32_MAX, 1) === I32_MIN, 'add wraps');

expect(calculator.divide(10, 2) === 5, 'divide');
expect(calculator.divide(-7, 2) === -3, 'divide rounds toward zero');
expect(calculator.divide(I32_MIN, -1) === I32_MIN, 'divide wraps');
throws(
  () => calculator.divide(1, 0),
  (e) =>
    e instanceof calculator.DivisionByZeroError &&
    e instanceof calculator.CalcError &&
    e instanceof CalculatorError &&
    e.code === 1 &&
    calculator.DivisionByZeroError.CODE === 1 &&
    e.message === 'division by zero' &&
    Object.keys(e).every((k) => k === 'name' || k === 'code'),
  'divide by zero is the typed error, with no payload fields',
);

expect(calculator.greet('World') === 'Hello, World!', 'greet');
expect(calculator.greet('') === 'Hello, !', 'greet an empty name');
expect(calculator.greet('Wörld 🦀') === 'Hello, Wörld 🦀!', 'non-ASCII and astral text');
expect(calculator.greet('a\0b') === 'Hello, a\0b!', 'an interior NUL');
throws(() => calculator.greet(42), (e) => e instanceof TypeError, 'a non-string is a TypeError');

await finish(api, 'calculator');
