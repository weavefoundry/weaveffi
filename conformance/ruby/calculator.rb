# frozen_string_literal: true

# Conformance consumer: calculator sample, Ruby target.
#
# The minimal surface: the load-time checks (requiring the gem checked the
# ABI revision and the contract table; here a stale entry is shown to fail
# with the declaration's path), i32 arithmetic that wraps, a throwing
# function whose typed error has no payload, and strings that keep
# non-ASCII text, astral characters, and interior NULs. The gem is installed
# from the generated tree; the cdylib is selected through CALCULATOR_LIBRARY.

require_relative 'support'
require 'calculator'

I32_MAX = (2**31) - 1
I32_MIN = -(2**31)

run_and_check_leaks(Calculator) do
  expect(Calculator::ABI_VERSION == 4, 'bindings target ABI revision 4')
  missing = expect_raise(LoadError, 'an entry the library lacks') do
    Calculator._wv_check_contract!(calculator_calculator_contract: [[1, 2, 'calculator.gone']])
  end
  expect(missing.message.end_with?('calculator.gone is missing from the library'), missing.message)
  stale = expect_raise(LoadError, 'an entry whose signature changed') do
    Calculator._wv_check_contract!(calculator_calculator_contract: [[fnv1a64('calculator.add'), 0, 'calculator.add']])
  end
  expect(stale.message.end_with?('calculator.add changed since these bindings were generated'), stale.message)

  expect(Calculator.add(2, 3) == 5, 'add')
  expect(Calculator.add(I32_MAX, 1) == I32_MIN, 'add wraps')
  expect_raise(RangeError, 'an i32 out of range') { Calculator.add(I32_MAX + 1, 0) }

  expect(Calculator.divide(10, 2) == 5, 'divide')
  expect(Calculator.divide(-7, 2) == -3, 'divide rounds toward zero')
  expect(Calculator.divide(I32_MIN, -1) == I32_MIN, 'divide wraps')
  e = expect_raise(Calculator::CalcError::DivisionByZero, 'divide by zero') { Calculator.divide(1, 0) }
  expect(e.is_a?(Calculator::CalcError) && e.is_a?(Calculator::Error), 'domain errors subclass the root error')
  expect(e.code == 1 && Calculator::CalcError::DivisionByZero::CODE == 1, "DivisionByZero code (got #{e.code})")
  expect(e.message == 'division by zero', "message (got #{e.message.inspect})")
  expect(e.instance_variables == [:@code], 'DivisionByZero carries no payload fields')

  expect(Calculator.greet('World') == 'Hello, World!', 'greet')
  expect(Calculator.greet('') == 'Hello, !', 'greet an empty name')
  crab = Calculator.greet('Wörld 🦀')
  expect(crab == 'Hello, Wörld 🦀!', "non-ASCII and astral text (got #{crab.inspect})")
  expect(crab.encoding == Encoding::UTF_8, 'greet returns UTF-8')
  expect(Calculator.greet("a\0b") == "Hello, a\0b!", 'an interior NUL')
  expect_raise(TypeError, 'a non-string') { Calculator.greet(42) }
  expect_raise(ArgumentError, 'invalid UTF-8') { Calculator.greet((+"\xC3\x28").force_encoding(Encoding::UTF_8)) }
end

puts 'ruby/calculator: OK'
