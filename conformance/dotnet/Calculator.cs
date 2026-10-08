// Conformance consumer: calculator sample, .NET target.
//
// Loading the generated project checks the ABI revision and the
// `calculator` contract table (the first call runs NativeMethods' static
// constructor). Covers wrapping addition, division rounding toward zero
// (including i32::MIN / -1), the typed CalcException.DivisionByZero (code 1,
// message "division by zero", no fields), and strings crossing as UTF-8
// (pointer, length) pairs: empty, non-ASCII, astral, and an interior NUL.
// Ends by asserting the producer's leak counters are zero.
//
// The namespace is `Calculator` and the module is also `calculator`, so the
// free-function class is `Calculator.Calculator`; `using static` imports
// its statics.

using System;
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

    static int Main()
    {
        Expect(Add(2, 3) == 5, "add(2, 3)");
        Expect(Add(int.MaxValue, 1) == int.MinValue, "add wraps");

        Expect(Divide(10, 2) == 5, "divide(10, 2)");
        Expect(Divide(-7, 2) == -3, "divide rounds toward zero");
        Expect(Divide(int.MinValue, -1) == int.MinValue, "divide(MIN, -1) wraps");
        try
        {
            Divide(1, 0);
            Expect(false, "divide(1, 0) throws");
        }
        catch (CalcException.DivisionByZero e)
        {
            Expect(e.Code == 1 && e.Code == CalcException.DivisionByZero.ErrorCode, $"code (got {e.Code})");
            Expect(e.Message == "division by zero", $"message (got '{e.Message}')");
            Expect(e is CalcException && e is NativeException, "hierarchy");
        }

        Expect(Greet("World") == "Hello, World!", "greet");
        Expect(Greet("") == "Hello, !", "greet empty");
        Expect(Greet("Wörld 🦀") == "Hello, Wörld 🦀!", "greet non-ASCII and astral");
        Expect(Greet("a\0b") == "Hello, a\0b!", "greet interior NUL");

        LeakCheck.AssertNoLeaks("calculator");
        Console.WriteLine("dotnet/calculator: OK");
        return 0;
    }
}
