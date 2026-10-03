# frozen_string_literal: true
# Conformance consumer: calculator sample, Ruby target.
#
# The minimal surface: direct scalars, the typed CalcError domain, and a
# string round trip (including non-ASCII text and an interior NUL, which
# cross the ABI as a pointer plus a length). The gem is installed from the
# generated tree; the cdylib is selected through CALCULATOR_LIBRARY.

require_relative 'support'
require 'calculator'

run_and_check_leaks(Calculator) do
  expect(Calculator::ABI_VERSION == 3, 'bindings target ABI revision 3')
  expect(Calculator.add(2, 40) == 42, 'add')
  expect(Calculator.mul(6, 7) == 42, 'mul')
  expect(Calculator.div(10, 2) == 5, 'div')

  begin
    Calculator.div(1, 0)
    raise 'expected CalcError::DivisionByZero'
  rescue Calculator::CalcError::DivisionByZero => e
    expect(e.code == 1, "DivisionByZero code (got #{e.code})")
    expect(e.message == 'division by zero', "message (got #{e.message.inspect})")
    expect(e.is_a?(Calculator::Error), 'domain errors subclass Calculator::Error')
  end

  expect(Calculator.echo('hello') == 'hello', 'echo')
  expect(Calculator.echo('') == '', 'echo empty')
  unicode = Calculator.echo('héllo ✓ 日本')
  expect(unicode == 'héllo ✓ 日本', "echo unicode (got #{unicode.inspect})")
  expect(unicode.encoding == Encoding::UTF_8, 'echo returns UTF-8')
  expect(Calculator.echo("a\0b") == "a\0b", 'echo keeps an interior NUL')
end

puts 'ruby/calculator: OK'
