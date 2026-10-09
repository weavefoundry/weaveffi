// Conformance consumer: codec sample, .NET target (the wire oracle).
//
// Loading the generated project checks the ABI revision and the `codec`
// contract table. Then, against the producer's shared vectors:
//
//   1. every vector decodes (producer encodes, consumer decodes), checks
//      against its own index and not its neighbor's (consumer encodes,
//      producer decodes), and primitive vectors echo back identically;
//   2. vectors built from C# literals check against their named entries
//      (catching symmetric codec bugs), including NaN, signed zero, maps in
//      another insertion order, and a holder of consumer-made tokens;
//   3. spot checks on decoded values (i64 past 2^53, the f32 subnormal's
//      bits, astral text, every scalar minimum, the canonical composite);
//   4. the typed CodecException.OutOfRange with its index and count fields;
//   5. an undeclared Color value is rejected by the producer (-3, which a
//      non-throwing call raises as NativeBugException);
//   6. objects: token values, sum_holder twice over the same holder,
//      primary_of returning the same native object, identity (not value)
//      in same_primary, one wrapper in several slots;
//
// and ends by asserting the producer's leak counters are zero.

using System;
using System.Collections.Generic;
using System.Linq;
using Codec;
using C = Codec.Codec;

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

    static bool SameBits(double a, double b)
    {
        return BitConverter.DoubleToInt64Bits(a) == BitConverter.DoubleToInt64Bits(b);
    }

    static bool SameBits(float a, float b)
    {
        return BitConverter.SingleToInt32Bits(a) == BitConverter.SingleToInt32Bits(b);
    }

    // Look a vector up by name, so the table can grow without breaking us.
    static uint Find(uint n, string name)
    {
        for (uint i = 0; i < n; i++)
        {
            if (C.VectorName(i) == name)
            {
                return i;
            }
        }
        Expect(false, $"no vector named '{name}'");
        return 0;
    }

    static void Release(Vector v)
    {
        if (v is Vector.Objects o)
        {
            Release(o.Value);
        }
    }

    static void Release(Holder h)
    {
        h.Primary.Dispose();
        h.Spare?.Dispose();
        foreach (var t in h.Many)
        {
            t.Dispose();
        }
        foreach (var t in h.ByName.Values)
        {
            t.Dispose();
        }
    }

    // A primitive vector's value comes back unchanged from its echo_*.
    static void Echo(Vector v, string name)
    {
        switch (v)
        {
            case Vector.I8 x: Expect(C.EchoI8(x.Value) == x.Value, name); break;
            case Vector.U8 x: Expect(C.EchoU8(x.Value) == x.Value, name); break;
            case Vector.I16 x: Expect(C.EchoI16(x.Value) == x.Value, name); break;
            case Vector.U16 x: Expect(C.EchoU16(x.Value) == x.Value, name); break;
            case Vector.I32 x: Expect(C.EchoI32(x.Value) == x.Value, name); break;
            case Vector.U32 x: Expect(C.EchoU32(x.Value) == x.Value, name); break;
            case Vector.I64 x: Expect(C.EchoI64(x.Value) == x.Value, name); break;
            case Vector.U64 x: Expect(C.EchoU64(x.Value) == x.Value, name); break;
            case Vector.F32 x: Expect(SameBits(C.EchoF32(x.Value), x.Value), name); break;
            case Vector.F64 x: Expect(SameBits(C.EchoF64(x.Value), x.Value), name); break;
            case Vector.Flag x: Expect(C.EchoBool(x.Value) == x.Value, name); break;
            case Vector.Text x: Expect(C.EchoText(x.Value) == x.Value, name); break;
            case Vector.Blob x: Expect(C.EchoBlob(x.Value).SequenceEqual(x.Value), name); break;
            case Vector.Hue x: Expect(C.EchoColor(x.Value) == x.Value, name); break;
        }
    }

    static void EveryVector(uint n)
    {
        for (uint i = 0; i < n; i++)
        {
            var name = C.VectorName(i);
            var v = C.Vector(i);
            if (!C.CheckVector(i, v))
            {
                Expect(false, $"vector {i} ({name}) did not round-trip; producer saw {C.DescribeVector(v)}");
            }
            Expect(!C.CheckVector((i + 1) % n, v), $"vector {i} ({name}) matches its neighbor");
            Echo(v, $"echo {name}");
            Release(v);
        }
    }

    static Scalars CanonicalScalars(ushort u16 = 60_000)
    {
        return new Scalars(-8, 200, -16_000, u16, -2_000_000_000, 4_000_000_000U,
            -9_007_199_254_740_993L, ulong.MaxValue, 1.5f, -2.25e100, true, Color.Blue);
    }

    static void LiteralVectors(uint n)
    {
        Expect(C.CheckVector(Find(n, "scalars canonical"), new Vector.AllScalars(CanonicalScalars())),
            "literal scalars canonical");
        Expect(!C.CheckVector(Find(n, "scalars canonical"), new Vector.AllScalars(CanonicalScalars(60_001))),
            "changed scalars don't match");
        Expect(C.CheckVector(Find(n, "shape labeled"), new Vector.Figure(new Shape.Labeled("tag", 3))),
            "literal shape labeled");
        Expect(C.CheckVector(Find(n, "string interior nul"), new Vector.Text("nul\0inside\0")),
            "literal interior nul");
        // Any NaN matches the NaN vector; zero keeps its sign.
        var nan = BitConverter.Int64BitsToDouble(0x7ff8000000000001L);
        Expect(C.CheckVector(Find(n, "f64 nan"), new Vector.F64(nan)), "any NaN matches");
        Expect(C.CheckVector(Find(n, "f64 -0"), new Vector.F64(-0.0)), "-0.0 matches -0");
        Expect(!C.CheckVector(Find(n, "f64 -0"), new Vector.F64(0.0)), "+0.0 doesn't match -0");
        Expect(C.CheckVector(Find(n, "u64 max"), new Vector.U64(ulong.MaxValue)), "literal u64 max");
        Expect(C.CheckVector(Find(n, "enum infrared"), new Vector.Hue(Color.Infrared)), "literal infrared");
        Expect(C.CheckVector(Find(n, "optional zero"), new Vector.MaybeI64(0)), "Some(0)");
        Expect(C.CheckVector(Find(n, "optional absent"), new Vector.MaybeI64(null)), "None");
        Expect(!C.CheckVector(Find(n, "optional zero"), new Vector.MaybeI64(null)), "None isn't Some(0)");
        // A map's entry order doesn't matter on the wire.
        var counts = new Dictionary<string, long> { ["x"] = 0, ["héllo"] = -1, [""] = long.MaxValue };
        Expect(C.CheckVector(Find(n, "map of strings"), new Vector.Counts(counts)), "map in another order");
        Expect(C.CheckVector(Find(n, "blank"), new Vector.Blank()), "literal blank");
        using (var lone = new Token(-1))
        {
            var sparse = new Holder(lone, null, Array.Empty<Token>(), new Dictionary<string, Token>());
            Expect(C.CheckVector(Find(n, "objects sparse"), new Vector.Objects(sparse)), "consumer-made sparse holder");
        }
    }

    static T Fetch<T>(uint n, string name) where T : Vector
    {
        var v = C.Vector(Find(n, name));
        Expect(v is T, $"{name} is a {typeof(T).Name} (got {v.GetType().Name})");
        return (T)v;
    }

    static void SpotChecks(uint n)
    {
        Expect(Fetch<Vector.I64>(n, "i64 past 2^53").Value == -9_007_199_254_740_993L, "i64 past 2^53");
        Expect(BitConverter.SingleToInt32Bits(Fetch<Vector.F32>(n, "f32 min subnormal").Value) == 1,
            "f32 min subnormal bits");
        Expect(Fetch<Vector.Text>(n, "string astral").Value == "🦀 crab 😀", "string astral");

        var m = Fetch<Vector.AllScalars>(n, "scalars minimum").Value;
        Expect(m.I8Value == sbyte.MinValue && m.I16Value == short.MinValue && m.I32Value == int.MinValue,
            "signed minimums");
        Expect(m.I64Value == long.MinValue && m.U64Value == 0 && m.U8Value == 0 && m.U16Value == 0 && m.U32Value == 0,
            "i64 and unsigned minimums");
        Expect(float.IsNegativeInfinity(m.F32Value) && double.IsNaN(m.F64Value), "float minimums");
        Expect(m.Color == Color.Infrared && !m.Flag, "color and flag minimums");

        var c = Fetch<Vector.Deep>(n, "composite canonical").Value;
        Expect(c.Name == "héllo wörld ✓", $"composite name (got {c.Name})");
        Expect(c.Blob.Length == 6 && c.Blob[5] == 255, "composite blob");
        Expect(c.SomeI64 == long.MinValue && c.NoneI64 == null, "composite optionals");
        Expect(c.SomeText == "", "composite some_text is present and empty");
        Expect(c.Names.Length == 3 && c.Names[1] == "", "composite names");
        Expect(c.Matrix.Length == 3 && c.Matrix[1].Length == 0 && c.Matrix[2][0] == -4, "composite matrix");
        Expect(c.Floats.Length == 6 && double.IsNaN(c.Floats[0]) && double.IsNegative(c.Floats[3]),
            "composite floats");
        Expect(c.ByName.Count == 4 && c.ById.Count == 3 && c.ByColor.Count == 2 && c.Flags.Count == 2,
            "composite map sizes");
        Expect(c.Scalars.U32Value == 4_000_000_000U, "composite scalars");
        Expect(c.Shape is Shape.Labeled { Count: 3 }, "composite shape");
        Expect(c.Shapes.Length == 6 && c.Shapes[5] is Shape.Nested { Note: null }, "composite shapes");
        Expect(c.MaybeShape is Shape.Nested, "composite maybe_shape");
        Expect(c.MaybeList != null && c.MaybeList.Length == 2, "composite maybe_list");
        Expect(c.Sparse.Length == 3 && c.Sparse[1] == null && c.Sparse[0] == true, "composite sparse");
        Expect(c.Colors.Length == 4 && c.Colors[3] == Color.Infrared, "composite colors");
    }

    static void OutOfRange(uint n)
    {
        try
        {
            C.Vector(n);
            Expect(false, "vector(n) throws");
        }
        catch (CodecException.OutOfRange e)
        {
            Expect(e.Code == CodecException.OutOfRange.ErrorCode && e.Code == 1, $"code (got {e.Code})");
            Expect(e.Index == n && e.Count == n, $"payload (got {e.Index}, {e.Count})");
            Expect(e.Message == $"vector {n} is out of range (count {n})", $"message (got '{e.Message}')");
        }
        try
        {
            C.VectorName(n + 5);
            Expect(false, "vector_name(n + 5) throws");
        }
        catch (CodecException.OutOfRange e)
        {
            Expect(e.Index == n + 5 && e.Count == n, $"vector_name payload (got {e.Index}, {e.Count})");
        }
        Expect(!C.CheckVector(n, new Vector.Blank()), "check_vector past the end is false");
    }

    static void Malformed()
    {
        // C# can name an undeclared enum value; the producer rejects it, and
        // a non-throwing call traps with the marshalling code.
        try
        {
            C.EchoColor((Color)3);
            Expect(false, "echo_color(3) is rejected");
        }
        catch (NativeBugException e)
        {
            Expect(e.Code == NativeException.MarshalErrorCode, $"code -3 (got {e.Code})");
            Expect(e.Message.Contains("-3"), $"the message names the code (got '{e.Message}')");
        }
    }

    static void Objects(uint n)
    {
        var full = Fetch<Vector.Objects>(n, "objects full").Value;
        Expect(full.Primary.Value() == 10 && full.Spare != null && full.Spare.Value() == 11, "primary and spare");
        Expect(full.Many.Length == 3 && full.Many[2].Value() == long.MinValue, "many");
        Expect(full.ByName.Count == 2 && full.ByName["b"].Value() == 21, "by_name");
        // Each encoding mints fresh references, so the holder can be sent twice.
        var expected = unchecked(10L + 11 + 12 + 13 + 20 + 21 + long.MinValue);
        Expect(C.SumHolder(full) == expected && C.SumHolder(full) == expected, "sum_holder twice");

        // primary_of returns the very same native object.
        using var p = C.PrimaryOf(full);
        Expect(p.Equals(full.Primary), "primary_of is the same object");
        var none = new Dictionary<string, Token>();
        Expect(C.SamePrimary(full, new Holder(p, null, Array.Empty<Token>(), none)), "same_primary by identity");
        using var twin = new Token(10);
        Expect(!C.SamePrimary(full, new Holder(twin, null, Array.Empty<Token>(), none)),
            "an equal value isn't the same object");

        // One consumer-made wrapper in every slot.
        using var minus = new Token(-4);
        var mine = new Holder(twin, twin, new[] { twin, twin, minus }, new Dictionary<string, Token> { ["k"] = twin });
        Expect(C.SumHolder(mine) == 10 * 5 - 4, "holder of consumer tokens");
        Expect(twin.Value() == 10, "the wrapper is still usable");

        Release(full);
        try
        {
            full.Primary.Value();
            Expect(false, "a disposed wrapper throws");
        }
        catch (ObjectDisposedException)
        {
        }
    }

    static int Main()
    {
        var n = C.VectorCount();
        Expect(n >= 60, $"vector_count >= 60 (got {n})");

        EveryVector(n);
        LiteralVectors(n);
        SpotChecks(n);
        OutOfRange(n);
        Malformed();
        Objects(n);

        LeakCheck.AssertNoLeaks("codec");
        Console.WriteLine($"dotnet/codec: OK ({n} vectors)");
        return 0;
    }
}
