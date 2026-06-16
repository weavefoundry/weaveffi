// Conformance consumer: shapes sample, .NET target.
//
// Drives the generated P/Invoke surface for rich (algebraic) enums (Shapes.cs,
// namespace Shapes): the IDisposable opaque-object `Shape` class, its nested
// `Tag` enum + `GetTag()` reader, the per-variant static factories
// (`Shape.Circle(...)`, `Shape.Empty()`, ...) and per-variant field accessors
// (`CircleRadius`, `RectangleWidth`, `LabeledLabel`, ...), plus the free
// functions that take and return `Shape` by value. Also covers the expanded
// numerics (f32 fields, u8 field, list<u8> in / u64 out) and the plain C-style
// `Channel` enum. The producer cdylib is resolved by absolute path via a
// DllImportResolver reading WEAVEFFI_LIBRARY, mirroring the other backends.
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
        using (var empty = Shape.Empty())
        {
            Expect(empty.GetTag() == Shape.Tag.Empty, "empty tag");
        }

        // f64 payload.
        using (var circle = Shape.Circle(2.5))
        {
            Expect(circle.GetTag() == Shape.Tag.Circle, "circle tag");
            Expect(Math.Abs(circle.CircleRadius - 2.5) < 1e-9, "circle radius");

            // Free functions: Shape in, string/Shape out.
            Expect(ShapesDescribe(circle) == "circle(r=2.5)", "describe(circle)");

            using (var big = ShapesScale(circle, 4.0))
            {
                Expect(big.GetTag() == Shape.Tag.Circle, "scaled tag");
                Expect(Math.Abs(big.CircleRadius - 10.0) < 1e-9, "scaled radius");
            }
        }

        // Two f32 payloads.
        using (var rect = Shape.Rectangle(3.0f, 4.0f))
        {
            Expect(rect.GetTag() == Shape.Tag.Rectangle, "rect tag");
            Expect(Math.Abs(rect.RectangleWidth - 3.0f) < 1e-6f, "rect width");
            Expect(Math.Abs(rect.RectangleHeight - 4.0f) < 1e-6f, "rect height");
        }

        // string + u8 payload.
        using (var labeled = Shape.Labeled("hex", 6))
        {
            Expect(labeled.GetTag() == Shape.Tag.Labeled, "labeled tag");
            Expect(labeled.LabeledLabel == "hex", "labeled label");
            Expect(labeled.LabeledCount == 6, "labeled count");
        }

        // Numerics: list<u8> in, u64 out.
        ulong total = ShapesSumBytes(new byte[] { 250, 250, 250, 250 });
        Expect(total == 1000UL, $"sum_bytes == 1000 (got {total})");

        // Plain C-style enum lowers by value.
        Channel ch = Channel.Green;
        Expect((int)ch == 1, "plain enum value");

        Console.WriteLine("dotnet/shapes: OK");
        return 0;
    }
}
