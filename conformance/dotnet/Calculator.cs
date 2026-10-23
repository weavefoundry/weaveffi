// Conformance consumer: calculator sample, .NET target.
//
// CalculatorLibrary.Check() loads the producer and checks the ABI revision
// and the `calculator` contract table up front. Covers wrapping addition,
// division rounding toward zero (including i32::MIN / -1), the typed
// CalcException.DivisionByZero (code 1, message "division by zero", no
// fields), a second domain in the same module (ParseException.NotANumber,
// also code 1, with its `text` field), a `throws: any` function (the root
// NativeException with code -1), an optional scalar return (`double?`),
// typed arrays in and out (spans in, arrays out), and strings crossing as
// UTF-8 (pointer, length) pairs: empty, non-ASCII, astral, and an interior
// NUL. Ends by asserting the producer's leak counters are zero.
//
// Run with EXPECT_LOAD_FAILURE=1 and CALCULATOR_LIBRARY naming a file that
// doesn't exist, it checks instead that the load failure is a catchable
// NativeLoadException, from Check() and from every later call.
//
// The namespace is `Calculator` and the module is also `calculator`, so the
// free-function class is `Calculator.Calculator`; `using static` imports
// its statics.

using System;
using System.Linq;
using Calculator;
using static Calculator.Calculator;

internal static class Program
{
    static void Expect(bool cond, string msg)
    {
        if (!cond)
        {
            Console.Error.WriteLine($"assertion failed: {msg}");
            Environment.Exit(1);
        }
    }

    static T Throws<T>(Action action, string what) where T : Exception
    {
        try
        {
            action();
        }
        catch (T e)
        {
            return e;
        }
        catch (Exception e)
        {
            Expect(false, $"{what}: expected {typeof(T).Name}, got {e.GetType().Name}: {e.Message}");
        }
        Expect(false, $"{what}: expected {typeof(T).Name}, nothing thrown");
        return null;
    }

    static int LoadFailure()
    {
        var first = Throws<NativeLoadException>(CalculatorLibrary.Check, "Check() with a missing library");
        Expect(first.Message.Contains("CALCULATOR_LIBRARY"), $"the message names the variable (got '{first.Message}')");
        var again = Throws<NativeLoadException>(() => Add(1, 2), "a call after a failed load");
        Expect(ReferenceEquals(first, again), "every call throws the same failure");
        Console.WriteLine("dotnet/calculator (load failure): OK");
        return 0;
    }

    static void Arithmetic()
    {
        Expect(Add(2, 3) == 5, "add(2, 3)");
        Expect(Add(int.MaxValue, 1) == int.MinValue, "add wraps");

        Expect(Divide(10, 2) == 5, "divide(10, 2)");
        Expect(Divide(-7, 2) == -3, "divide rounds toward zero");
        Expect(Divide(int.MinValue, -1) == int.MinValue, "divide(MIN, -1) wraps");
        var e = Throws<CalcException.DivisionByZero>(() => Divide(1, 0), "divide(1, 0)");
        Expect(e.Code == 1 && e.Code == CalcException.DivisionByZero.ErrorCode, $"code (got {e.Code})");
        Expect(e.Message == "division by zero", $"message (got '{e.Message}')");
        Expect(e is CalcException && e is NativeException, "hierarchy");
    }

    static void SecondDomain()
    {
        Expect(Parse("42") == 42 && Parse(" 42 ") == 42, "parse");
        // Both domains use code 1; the call's domain decides the type.
        var bad = Throws<ParseException.NotANumber>(() => Parse("4x"), "parse(4x)");
        Expect(bad.Code == 1 && bad.Text == "4x", $"NotANumber fields (got {bad.Code}, '{bad.Text}')");
        Expect(bad.Message == "not a number: 4x", $"NotANumber message (got '{bad.Message}')");
        Expect(!((NativeException)bad is CalcException), "not the other domain");
        var empty = Throws<ParseException.NotANumber>(() => Parse(""), "parse(\"\")");
        Expect(empty.Text == "" && empty.Message == "not a number: ", $"empty text (got '{empty.Message}')");
    }

    static void Untyped()
    {
        Expect(Sqrt(9.0) == 3.0, "sqrt(9)");
        var e = Throws<NativeException>(() => Sqrt(-4.0), "sqrt(-4)");
        Expect(e.Code == NativeException.GenericErrorCode && e.Code == -1, $"code -1 (got {e.Code})");
        Expect(e.Message == "cannot take the square root of -4", $"message (got '{e.Message}')");
        Expect(e.GetType() == typeof(NativeException), "the root type itself");
    }

    static void Shapes()
    {
        Expect(Mean(new[] { 1.0, 2.0, 6.0 }) == 3.0, "mean present");
        Expect(Mean(ReadOnlySpan<double>.Empty) == null, "mean absent");
        // A span over part of an array, pinned in place.
        var values = new[] { 100.0, 4.0, 8.0, 100.0 };
        Expect(Mean(values.AsSpan(1, 2)) == 6.0, "mean of a slice");

        Expect(RunningTotal(new[] { 1, 2, 3 }).SequenceEqual(new[] { 1, 3, 6 }), "running_total");
        var none = RunningTotal(Array.Empty<int>());
        Expect(none.Length == 0, "running_total([])");
        Expect(RunningTotal(new[] { 1, 2, 3, int.MaxValue }).SequenceEqual(new[] { 1, 3, 6, -2147483643 }),
            "running_total wraps");
        Span<int> stack = stackalloc int[] { 5, 5 };
        Expect(RunningTotal(stack).SequenceEqual(new[] { 5, 10 }), "running_total of stack memory");
    }

    static void Strings()
    {
        Expect(Greet("World") == "Hello, World!", "greet");
        Expect(Greet("") == "Hello, !", "greet empty");
        Expect(Greet("Wörld 🦀") == "Hello, Wörld 🦀!", "greet non-ASCII and astral");
        Expect(Greet("a\0b") == "Hello, a\0b!", "greet interior NUL");
        var big = new string('x', 100_000);
        Expect(Greet(big) == "Hello, " + big + "!", "greet a large string");
    }

    static int Main()
    {
        if (Environment.GetEnvironmentVariable("EXPECT_LOAD_FAILURE") == "1")
        {
            return LoadFailure();
        }
        CalculatorLibrary.Check();
        CalculatorLibrary.Check();
        Expect(CalculatorLibrary.AbiVersion == 5, "ABI revision 5");

        Arithmetic();
        SecondDomain();
        Untyped();
        Shapes();
        Strings();

        LeakCheck.AssertNoLeaks("calculator");
        Console.WriteLine("dotnet/calculator: OK");
        return 0;
    }
}
