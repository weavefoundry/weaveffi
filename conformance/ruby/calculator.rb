# frozen_string_literal: true

# Conformance consumer: calculator sample, Ruby target.
#
# The minimal surface: the load-time checks (requiring the gem checked the
# ABI revision and the contract table; here a stale entry is shown to fail
# with the declaration's path), the private boundary (no raw C function on
# the public module), i32 arithmetic that wraps, two error domains in one
# module whose codes share a value (dispatched by the callable's domain),
# a `throws: any` function, an optional scalar return, typed arrays in and
# out, and strings that keep non-ASCII text, astral characters, and
# interior NULs. The gem is installed from the generated tree; the cdylib
# is selected through CALCULATOR_LIBRARY.

require_relative 'support'
require 'calculator'

I32_MAX = (2**31) - 1
I32_MIN = -(2**31)

run_and_check_leaks(Calculator) do
  expect(Calculator::ABI_VERSION == 5, 'bindings target ABI revision 5')
  expect(Calculator::LoadError < ::LoadError, 'load failures are LoadErrors')
  missing = expect_raise(Calculator::LoadError, 'an entry the library lacks') do
    bridge(Calculator).check_contract!(calculator_calculator_contract: [[1, 2, 'calculator.gone']])
  end
  expect(missing.message.end_with?('calculator.gone is missing from the library'), missing.message)
  stale = expect_raise(Calculator::LoadError, 'an entry whose signature changed') do
    bridge(Calculator).check_contract!(calculator_calculator_contract: [[fnv1a64('calculator.add'), 0, 'calculator.add']])
  end
  expect(stale.message.end_with?('calculator.add changed since these bindings were generated'), stale.message)
  expect(!Calculator.respond_to?(:calculator_calculator_add), 'raw C functions are private')
  expect_raise(NameError, 'the native module is private') { Calculator::Native }
  expect_raise(NameError, 'the runtime module is private') { Calculator::Bridge }

  expect(Calculator.add(2, 3) == 5, 'add')
  expect(Calculator.add(I32_MAX, 1) == I32_MIN, 'add wraps')
  expect_raise(RangeError, 'an i32 out of range') { Calculator.add(I32_MAX + 1, 0) }
  expect_raise(TypeError, 'a Float for an i32') { Calculator.add(1.5, 0) }

  expect(Calculator.divide(10, 2) == 5, 'divide')
  expect(Calculator.divide(-7, 2) == -3, 'divide rounds toward zero')
  expect(Calculator.divide(I32_MIN, -1) == I32_MIN, 'divide wraps')
  e = expect_raise(Calculator::CalcError::DivisionByZero, 'divide by zero') { Calculator.divide(1, 0) }
  expect(e.is_a?(Calculator::CalcError) && e.is_a?(Calculator::Error), 'domain errors subclass the root error')
  expect(e.code == 1 && Calculator::CalcError::DivisionByZero::CODE == 1, "DivisionByZero code (got #{e.code})")
  expect(e.message == 'division by zero', "message (got #{e.message.inspect})")
  expect(e.instance_variables == [:@code], 'DivisionByZero carries no payload fields')

  # A second domain whose code 1 is a different error.
  expect(Calculator.parse('42') == 42 && Calculator.parse(' 42 ') == 42, 'parse')
  e = expect_raise(Calculator::ParseError::NotANumber, 'parse("4x")') { Calculator.parse('4x') }
  expect(e.is_a?(Calculator::ParseError) && !e.is_a?(Calculator::CalcError), 'NotANumber is a ParseError')
  expect(e.code == 1 && e.text == '4x', "NotANumber payload (got #{e.code}, #{e.text.inspect})")
  expect(e.message == 'not a number: 4x', "NotANumber message (got #{e.message.inspect})")
  e = expect_raise(Calculator::ParseError::NotANumber, 'parse("")') { Calculator.parse('') }
  expect(e.text == '' && e.message == 'not a number: ', "an empty text (got #{e.message.inspect})")

  # throws: any.
  expect(Calculator.sqrt(9.0) == 3.0, 'sqrt')
  e = expect_raise(Calculator::Error, 'sqrt(-4)') { Calculator.sqrt(-4.0) }
  expect(e.instance_of?(Calculator::Error) && e.code == -1, "an untyped error (got #{e.class}, #{e.code})")
  expect(e.message == 'cannot take the square root of -4', "untyped message (got #{e.message.inspect})")

  # An optional scalar return and typed arrays.
  expect(Calculator.mean([1.0, 2.0, 6.0]) == 3.0, 'mean')
  expect(Calculator.mean([1, 2]) == 1.5, 'mean of Integers')
  expect(Calculator.mean([]).nil?, 'mean([]) is nil')
  expect_raise(TypeError, 'a non-array') { Calculator.mean(1.0) }
  expect_raise(TypeError, 'a non-numeric element') { Calculator.mean(['1']) }
  expect(Calculator.running_total([1, 2, 3]) == [1, 3, 6], 'running_total')
  expect(Calculator.running_total([]) == [], 'running_total([])')
  expect(Calculator.running_total([1, 2, 3, I32_MAX]) == [1, 3, 6, -2_147_483_643], 'running_total wraps')
  expect_raise(RangeError, 'an element out of range') { Calculator.running_total([I32_MAX + 1]) }

  expect(Calculator.greet('World') == 'Hello, World!', 'greet')
  expect(Calculator.greet('') == 'Hello, !', 'greet an empty name')
  crab = Calculator.greet('Wörld 🦀')
  expect(crab == 'Hello, Wörld 🦀!', "non-ASCII and astral text (got #{crab.inspect})")
  expect(crab.encoding == Encoding::UTF_8, 'greet returns UTF-8')
  expect(Calculator.greet("a\0b") == "Hello, a\0b!", 'an interior NUL')
  expect(Calculator.greet('Wörld'.encode(Encoding::ISO_8859_1)) == 'Hello, Wörld!', 'another encoding converts')
  expect_raise(TypeError, 'a non-string') { Calculator.greet(42) }
  expect_raise(ArgumentError, 'invalid UTF-8') { Calculator.greet((+"\xC3\x28").force_encoding(Encoding::UTF_8)) }
end

puts 'ruby/calculator: OK'
