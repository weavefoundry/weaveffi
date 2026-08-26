// Conformance consumer: shapes sample, .NET target.
//
// Drives the generated P/Invoke surface for rich (algebraic) enums (Shapes.cs,
// namespace Shapes): the abstract `Shape` class with one sealed nested class
// per variant (`Shape.Empty`, `Shape.Circle`, ...), variant construction via
// the nested constructors, discrimination via C# pattern matching, and the
// per-variant properties (`Radius`, `Width`, `Label`, ...). Shapes cross the
// ABI as serialized value buffers behind the free functions that take and
// return `Shape` by value. Also covers the expanded numerics (f32 fields, u8
// field, list<u8> in / u64 out) and the plain C-style `Channel` enum. The
// producer cdylib is resolved by absolute path via a DllImportResolver reading
// WEAVEFFI_LIBRARY, mirroring the other backends.
//
// The IDL sets the .NET namespace to `Shapes` and the module is also `shapes`,
// so the generated free-function class is `Shapes.Shapes`; `using static`
// imports those statics (a bare `Shapes.X` would bind `Shapes` to the
// namespace). Returns non-zero on any failed assertion.

using System;
using System.Runtime.InteropServices;
using Shapes;
using static Shapes.Shapes;

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
        var lib = Environment.GetEnvironmentVariable("WEAVEFFI_LIBRARY");
        NativeLibrary.SetDllImportResolver(typeof(Program).Assembly, (name, asm, search) =>
        {
            if (name == "weaveffi" && !string.IsNullOrEmpty(lib))
                return NativeLibrary.Load(lib);
            return IntPtr.Zero;
        });

        // Unit variant.
        Shape empty = new Shape.Empty();
        Expect(empty is Shape.Empty, "empty variant");

        // f64 payload.
        var circle = new Shape.Circle(2.5);
        Expect(Math.Abs(circle.Radius - 2.5) < 1e-9, "circle radius");

        // Free functions: Shape in, string/Shape out.
        Expect(Describe(circle) == "circle(r=2.5)", "describe(circle)");

        Shape scaled = Scale(circle, 4.0);
        Expect(scaled is Shape.Circle, "scaled variant");
        var big = (Shape.Circle)scaled;
        Expect(Math.Abs(big.Radius - 10.0) < 1e-9, "scaled radius");

        // Two f32 payloads.
        var rect = new Shape.Rectangle(3.0f, 4.0f);
        Expect(Math.Abs(rect.Width - 3.0f) < 1e-6f, "rect width");
        Expect(Math.Abs(rect.Height - 4.0f) < 1e-6f, "rect height");

        // string + u8 payload, round-tripped through the producer to prove
        // the variant encodes and decodes intact.
        Shape labeledBack = Scale(new Shape.Labeled("hex", 6), 2.0);
        Expect(labeledBack is Shape.Labeled, "labeled variant survives round trip");
        var labeled = (Shape.Labeled)labeledBack;
        Expect(labeled.Label == "hex", "labeled label");
        Expect(labeled.Count == 6, "labeled count");

        // Pattern matching discriminates variants like any C# sum type.
        string kind = scaled switch
        {
            Shape.Empty _ => "empty",
            Shape.Circle _ => "circle",
            Shape.Rectangle _ => "rectangle",
            Shape.Labeled _ => "labeled",
            _ => "unknown",
        };
        Expect(kind == "circle", "switch pattern match");

        // Numerics: list<u8> in, u64 out.
        ulong total = SumBytes(new byte[] { 250, 250, 250, 250 });
        Expect(total == 1000UL, $"sum_bytes == 1000 (got {total})");

        // Plain C-style enum lowers by value.
        Channel ch = Channel.Green;
        Expect((int)ch == 1, "plain enum value");

        Console.WriteLine("dotnet/shapes: OK");
        return 0;
    }
}
