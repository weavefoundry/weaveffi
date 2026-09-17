// Conformance consumer: codec sample, .NET target.
//
// Round-trips every value-buffer wire shape through the generated P/Invoke
// wrapper (Codec.cs, namespace Codec) against the producer's oracle: the
// canonical Scalars and Composite fixtures are decoded and checked field by
// field (producer encodes, consumer decodes), handed back through Verify*
// (consumer encodes, producer decodes) and Roundtrip* (both), then rebuilt
// from scratch with edge values (empty strings, lists, and maps; unicode;
// long/ulong extremes; NaN, the infinities, and negative zero) and
// round-tripped again. Also covers every Shape variant of the rich enum, the
// typed CodecException (Mismatch=1), and the Holder record carrying Token
// objects in a required field, an optional, and a list: each encoded token is
// a fresh clone, PrimaryOf returns a wrapper to the same native object as the
// holder's Primary, and every wrapper is disposed (double Dispose safe,
// ObjectDisposedException after). The producer cdylib is resolved by absolute
// path via a DllImportResolver reading WEAVEFFI_LIBRARY.
//
// The harness compiles the generated source into this assembly, so the
// wrapper's `internal` Handle property is reachable and used to prove two
// wrappers point at the same native object. The IDL sets the .NET namespace
// to `Codec` and the module is also `codec`, so the free-function class is
// `Codec.Codec`; `using static` imports those statics.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using Codec;
using static Codec.Codec;

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

    static Scalars CanonicalScalars()
    {
        return new Scalars(-8, 200, -16_000, 60_000, -2_000_000_000, 4_000_000_000U,
            -9_007_199_254_740_993L, ulong.MaxValue, 1.5f, -2.25e100, true, Color.Blue);
    }

    static void CheckScalars(Scalars s, string what)
    {
        Expect(s.I8Value == -8, $"{what}: i8 (got {s.I8Value})");
        Expect(s.U8Value == 200, $"{what}: u8 (got {s.U8Value})");
        Expect(s.I16Value == -16_000, $"{what}: i16 (got {s.I16Value})");
        Expect(s.U16Value == 60_000, $"{what}: u16 (got {s.U16Value})");
        Expect(s.I32Value == -2_000_000_000, $"{what}: i32 (got {s.I32Value})");
        Expect(s.U32Value == 4_000_000_000U, $"{what}: u32 (got {s.U32Value})");
        Expect(s.I64Value == -9_007_199_254_740_993L, $"{what}: i64 (got {s.I64Value})");
        Expect(s.U64Value == ulong.MaxValue, $"{what}: u64 (got {s.U64Value})");
        Expect(s.F32Value == 1.5f, $"{what}: f32 (got {s.F32Value})");
        Expect(s.F64Value == -2.25e100, $"{what}: f64 (got {s.F64Value})");
        Expect(s.Flag, $"{what}: flag");
        Expect(s.Color == Color.Blue, $"{what}: color (got {s.Color})");
    }

    static void ExpectScalarsEqual(Scalars a, Scalars b, string what)
    {
        Expect(a.I8Value == b.I8Value && a.U8Value == b.U8Value
               && a.I16Value == b.I16Value && a.U16Value == b.U16Value
               && a.I32Value == b.I32Value && a.U32Value == b.U32Value
               && a.I64Value == b.I64Value && a.U64Value == b.U64Value
               && SameBits(a.F32Value, b.F32Value) && SameBits(a.F64Value, b.F64Value)
               && a.Flag == b.Flag && a.Color == b.Color,
            $"{what}: scalars equal");
    }

    static void ExpectShapesEqual(Shape a, Shape b, string what)
    {
        Expect(a.GetType() == b.GetType(), $"{what}: same variant ({a.GetType().Name} vs {b.GetType().Name})");
        switch (a)
        {
            case Shape.Empty _:
                break;
            case Shape.Circle ca:
                Expect(SameBits(ca.Radius, ((Shape.Circle)b).Radius), $"{what}: circle radius");
                break;
            case Shape.Rect ra:
                var rb = (Shape.Rect)b;
                Expect(SameBits(ra.Width, rb.Width) && SameBits(ra.Height, rb.Height), $"{what}: rect dims");
                break;
            case Shape.Labeled la:
                var lb = (Shape.Labeled)b;
                Expect(la.Label == lb.Label && la.Count == lb.Count, $"{what}: labeled payload");
                break;
            case Shape.Nested na:
                var nb = (Shape.Nested)b;
                ExpectScalarsEqual(na.Inner, nb.Inner, what + ": nested inner");
                Expect(na.Note == nb.Note, $"{what}: nested note");
                break;
            default:
                Expect(false, $"{what}: unknown variant");
                break;
        }
    }

    static void ExpectCompositesEqual(Composite a, Composite b, string what)
    {
        Expect(a.Name == b.Name, $"{what}: name");
        Expect(a.Blob.SequenceEqual(b.Blob), $"{what}: blob");
        Expect(a.SomeI64 == b.SomeI64, $"{what}: some_i64");
        Expect(a.NoneI64 == b.NoneI64, $"{what}: none_i64");
        Expect(a.SomeText == b.SomeText, $"{what}: some_text");
        Expect(a.Names.SequenceEqual(b.Names), $"{what}: names");
        Expect(a.Matrix.Length == b.Matrix.Length
               && a.Matrix.Zip(b.Matrix, (x, y) => x.SequenceEqual(y)).All(eq => eq),
            $"{what}: matrix");
        Expect(a.Empty.Length == b.Empty.Length
               && a.Empty.Zip(b.Empty, (x, y) => SameBits(x, y)).All(eq => eq),
            $"{what}: empty list");
        Expect(a.ByName.Count == b.ByName.Count
               && a.ByName.All(kv => b.ByName.TryGetValue(kv.Key, out var v) && v == kv.Value),
            $"{what}: by_name");
        Expect(a.ById.Count == b.ById.Count && a.ById.Keys.All(k => b.ById.ContainsKey(k)),
            $"{what}: by_id keys");
        foreach (var kv in a.ById)
        {
            ExpectScalarsEqual(kv.Value, b.ById[kv.Key], $"{what}: by_id[{kv.Key}]");
        }
        ExpectScalarsEqual(a.Scalars, b.Scalars, what + ": scalars");
        ExpectShapesEqual(a.Shape, b.Shape, what + ": shape");
        Expect(a.Shapes.Length == b.Shapes.Length, $"{what}: shapes length");
        for (int i = 0; i < a.Shapes.Length; i++)
        {
            ExpectShapesEqual(a.Shapes[i], b.Shapes[i], $"{what}: shapes[{i}]");
        }
        Expect((a.MaybeShape == null) == (b.MaybeShape == null), $"{what}: maybe_shape presence");
        if (a.MaybeShape != null)
        {
            ExpectShapesEqual(a.MaybeShape, b.MaybeShape, what + ": maybe_shape");
        }
        Expect((a.MaybeList == null) == (b.MaybeList == null)
               && (a.MaybeList == null || a.MaybeList.SequenceEqual(b.MaybeList)),
            $"{what}: maybe_list");
        Expect(a.Sparse.SequenceEqual(b.Sparse), $"{what}: sparse");
        Expect(a.Colors.SequenceEqual(b.Colors), $"{what}: colors");
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

        // --- Scalars: producer encodes, consumer decodes; then back. ---
        var scalars = SampleScalars();
        CheckScalars(scalars, "sample_scalars");
        Expect(VerifyScalars(scalars), "verify_scalars accepts the decoded sample");
        CheckScalars(RoundtripScalars(scalars), "roundtrip_scalars");
        var mine = CanonicalScalars();
        Expect(VerifyScalars(mine), "verify_scalars accepts a consumer-built canonical value");

        // A one-field mismatch reports the typed domain error.
        var wrong = new Scalars(-8, 200, -16_000, 60_000, -2_000_000_000, 4_000_000_000U,
            -9_007_199_254_740_993L, ulong.MaxValue, 1.5f, -2.25e100, false, Color.Blue);
        try
        {
            VerifyScalars(wrong);
            Expect(false, "expected CodecException for a mismatched Scalars");
        }
        catch (CodecException e)
        {
            Expect(e.Code == CodecException.Mismatch, $"Mismatch code == 1 (got {e.Code})");
            Expect(e.Code == 1, "Mismatch constant is 1");
            Expect(e is WeaveFFIException, "typed exception extends the brand exception");
        }

        // Scalars at the extremes, including float edge values whose bit
        // patterns must survive both directions.
        var extremes = new Scalars(sbyte.MinValue, byte.MaxValue, short.MinValue, ushort.MaxValue,
            int.MinValue, uint.MaxValue, long.MinValue, ulong.MaxValue,
            float.NegativeInfinity, -0.0, false, Color.Red);
        var extremesBack = RoundtripScalars(extremes);
        ExpectScalarsEqual(extremes, extremesBack, "extremes");
        Expect(BitConverter.DoubleToInt64Bits(extremesBack.F64Value) == long.MinValue, "negative zero keeps its sign bit");
        var nans = new Scalars(sbyte.MaxValue, 0, short.MaxValue, 0, int.MaxValue, 0, long.MaxValue, 0,
            float.NaN, double.NaN, true, Color.Green);
        var nansBack = RoundtripScalars(nans);
        Expect(float.IsNaN(nansBack.F32Value) && double.IsNaN(nansBack.F64Value), "NaN survives both floats");
        Expect(nansBack.I64Value == long.MaxValue && nansBack.I8Value == sbyte.MaxValue, "max extremes");
        var infs = new Scalars(0, 0, 0, 0, 0, 0, 0, 0, float.PositiveInfinity, double.PositiveInfinity, false, Color.Red);
        var infsBack = RoundtripScalars(infs);
        Expect(float.IsPositiveInfinity(infsBack.F32Value) && double.IsPositiveInfinity(infsBack.F64Value),
            "+inf survives both floats");
        Expect(double.IsNegativeInfinity(RoundtripScalars(new Scalars(0, 0, 0, 0, 0, 0, 0, 0, 0f,
            double.NegativeInfinity, false, Color.Red)).F64Value), "-inf survives f64");

        // --- Composite: every nested wire shape. ---
        var composite = SampleComposite();
        Expect(composite.Name == "héllo wörld ✓", $"name (got '{composite.Name}')");
        Expect(composite.Blob.SequenceEqual(new byte[] { 0, 1, 2, 253, 254, 255 }), "blob");
        Expect(composite.SomeI64 == long.MinValue, $"some_i64 == i64::MIN (got {composite.SomeI64})");
        Expect(composite.NoneI64 == null, "none_i64 absent");
        Expect(composite.SomeText != null && composite.SomeText == "", "some_text is a present empty string");
        Expect(composite.Names.SequenceEqual(new[] { "a", "", "ccc" }), "names");
        Expect(composite.Matrix.Length == 3
               && composite.Matrix[0].SequenceEqual(new[] { 1, 2, 3 })
               && composite.Matrix[1].Length == 0
               && composite.Matrix[2].SequenceEqual(new[] { -4 }),
            "matrix");
        Expect(composite.Empty.Length == 0, "empty list");
        Expect(composite.ByName.Count == 3
               && composite.ByName["one"] == 1 && composite.ByName["two"] == 2 && composite.ByName["neg"] == -3,
            "by_name");
        Expect(composite.ById.Count == 2 && composite.ById.ContainsKey(-1) && composite.ById.ContainsKey(42), "by_id keys");
        CheckScalars(composite.ById[-1], "by_id[-1]");
        Expect(!composite.ById[42].Flag && composite.ById[42].U64Value == ulong.MaxValue, "by_id[42] differs only in flag");
        CheckScalars(composite.Scalars, "composite.scalars");
        Expect(composite.Shape is Shape.Labeled sl && sl.Label == "tag" && sl.Count == 3, "shape is Labeled(tag, 3)");
        Expect(composite.Shapes.Length == 5, "five shapes");
        Expect(composite.Shapes[0] is Shape.Empty, "shapes[0] Empty");
        Expect(composite.Shapes[1] is Shape.Circle c1 && c1.Radius == 2.5, "shapes[1] Circle(2.5)");
        Expect(composite.Shapes[2] is Shape.Rect r2 && r2.Width == 1.0f && r2.Height == 0.5f, "shapes[2] Rect(1, 0.5)");
        Expect(composite.Shapes[3] is Shape.Labeled l3 && l3.Label == "" && l3.Count == -1, "shapes[3] Labeled('', -1)");
        Expect(composite.Shapes[4] is Shape.Nested n4 && n4.Note == "n", "shapes[4] Nested(note n)");
        CheckScalars(((Shape.Nested)composite.Shapes[4]).Inner, "shapes[4].inner");
        Expect(composite.MaybeShape is Shape.Nested mn && mn.Note == null, "maybe_shape Nested(note absent)");
        CheckScalars(((Shape.Nested)composite.MaybeShape).Inner, "maybe_shape.inner");
        Expect(composite.MaybeList != null && composite.MaybeList.SequenceEqual(new byte[] { 9, 8 }), "maybe_list");
        Expect(composite.Sparse.SequenceEqual(new bool?[] { true, null, false }), "sparse");
        Expect(composite.Colors.SequenceEqual(new[] { Color.Red, Color.Green, Color.Blue }), "colors");

        Expect(VerifyComposite(composite), "verify_composite accepts the decoded sample");
        ExpectCompositesEqual(composite, RoundtripComposite(composite), "roundtrip_composite");
        var described = DescribeComposite(composite);
        Expect(described.Contains("héllo wörld ✓") && described.Contains("Labeled"),
            $"describe_composite renders the value (got '{described}')");

        // The same composite with one nested change fails verification.
        var tweaked = new Composite(composite.Name, composite.Blob, composite.SomeI64, composite.NoneI64,
            composite.SomeText, composite.Names, composite.Matrix, composite.Empty, composite.ByName,
            composite.ById, composite.Scalars, composite.Shape, composite.Shapes, composite.MaybeShape,
            composite.MaybeList, new bool?[] { true, true, false }, composite.Colors);
        try
        {
            VerifyComposite(tweaked);
            Expect(false, "expected CodecException for a tweaked Composite");
        }
        catch (CodecException e)
        {
            Expect(e.Code == CodecException.Mismatch, "tweaked composite reports Mismatch");
        }

        // A consumer-built composite with edge values: empty everything,
        // unicode, extremes, NaN, the infinities, and negative zero.
        var edge = new Composite(
            "",
            new byte[0],
            long.MaxValue,
            null,
            "日本語 🎉 \u0000 tail",
            new string[0],
            new int[][] { new int[0], new[] { int.MinValue, int.MaxValue } },
            new[] { double.NaN, double.PositiveInfinity, double.NegativeInfinity, -0.0, double.Epsilon, double.MaxValue },
            new Dictionary<string, long>(),
            new Dictionary<int, Scalars> { [int.MinValue] = extremes, [0] = nans, [int.MaxValue] = infs },
            extremes,
            new Shape.Empty(),
            new Shape[0],
            null,
            null,
            new bool?[0],
            new Color[0]);
        var edgeBack = RoundtripComposite(edge);
        ExpectCompositesEqual(edge, edgeBack, "edge composite");
        Expect(edgeBack.SomeText == "日本語 🎉 \u0000 tail", "embedded NUL and astral characters survive inside a buffer");
        Expect(double.IsNaN(edgeBack.Empty[0]) && double.IsPositiveInfinity(edgeBack.Empty[1])
               && double.IsNegativeInfinity(edgeBack.Empty[2])
               && BitConverter.DoubleToInt64Bits(edgeBack.Empty[3]) == long.MinValue,
            "float edge values in a list");
        Expect(edgeBack.MaybeShape == null && edgeBack.MaybeList == null && edgeBack.NoneI64 == null,
            "absent optionals stay absent");

        // Rich enum variants on their own and in a list.
        Shape[] variants =
        {
            new Shape.Empty(),
            new Shape.Circle(double.NaN),
            new Shape.Circle(-0.0),
            new Shape.Rect(float.MaxValue, float.Epsilon),
            new Shape.Labeled("", int.MinValue),
            new Shape.Labeled("ünïcödé", int.MaxValue),
            new Shape.Nested(extremes, null),
            new Shape.Nested(nans, ""),
        };
        foreach (var v in variants)
        {
            ExpectShapesEqual(v, RoundtripShape(v), "roundtrip_shape " + v.GetType().Name);
        }
        var listBack = RoundtripShapes(variants);
        Expect(listBack.Length == variants.Length, "roundtrip_shapes length");
        for (int i = 0; i < variants.Length; i++)
        {
            ExpectShapesEqual(variants[i], listBack[i], $"roundtrip_shapes[{i}]");
        }
        Expect(RoundtripShapes(new Shape[0]).Length == 0, "roundtrip_shapes empty");
        Expect(DescribeShape(new Shape.Empty()) == "Empty", "describe_shape Empty");
        Expect(DescribeShape(new Shape.Circle(2.5)) == "Circle { radius: 2.5 }",
            $"describe_shape Circle (got '{DescribeShape(new Shape.Circle(2.5))}')");
        Expect(DescribeShape(new Shape.Labeled("x", 7)) == "Labeled { label: \"x\", count: 7 }",
            "describe_shape Labeled");

        // Top-level optionals, maps, strings, bytes, and direct scalars.
        Expect(RoundtripOptI64(null) == null, "opt_i64 absent");
        Expect(RoundtripOptI64(long.MinValue) == long.MinValue, "opt_i64 i64::MIN");
        Expect(RoundtripOptI64(0) == 0, "opt_i64 zero is present");
        Expect(RoundtripMap(new Dictionary<string, long>()).Count == 0, "empty map");
        var map = RoundtripMap(new Dictionary<string, long> { [""] = long.MinValue, ["k"] = 0, ["ключ"] = long.MaxValue });
        Expect(map.Count == 3 && map[""] == long.MinValue && map["k"] == 0 && map["ключ"] == long.MaxValue, "map contents");
        Expect(RoundtripString("") == "", "empty string");
        Expect(RoundtripString("héllo wörld ✓ 🎉") == "héllo wörld ✓ 🎉", "unicode string");
        Expect(RoundtripBytes(new byte[0]).Length == 0, "empty bytes");
        Expect(RoundtripBytes(new byte[] { 0, 127, 128, 255 }).SequenceEqual(new byte[] { 0, 127, 128, 255 }), "bytes");
        Expect(RoundtripI64(long.MinValue) == long.MinValue, "i64::MIN direct");
        Expect(RoundtripI64(long.MaxValue) == long.MaxValue, "i64::MAX direct");
        Expect(RoundtripU64(ulong.MaxValue) == ulong.MaxValue, "u64::MAX direct");
        Expect(RoundtripU64(1UL << 63) == (1UL << 63), "2^63 direct");
        Expect(double.IsNaN(RoundtripF64(double.NaN)), "f64 NaN direct");
        Expect(double.IsPositiveInfinity(RoundtripF64(double.PositiveInfinity)), "f64 +inf direct");
        Expect(double.IsNegativeInfinity(RoundtripF64(double.NegativeInfinity)), "f64 -inf direct");
        Expect(BitConverter.DoubleToInt64Bits(RoundtripF64(-0.0)) == long.MinValue, "f64 -0.0 direct");
        Expect(RoundtripF64(double.MaxValue) == double.MaxValue, "f64 max direct");
        Expect(RoundtripBool(true) && !RoundtripBool(false), "bool direct");
        Expect(RoundtripColor(Color.Blue) == Color.Blue && (int)RoundtripColor(Color.Blue) == 7, "enum direct");
        Expect(RoundtripColor(Color.Red) == Color.Red, "enum zero direct");

        // --- Objects inside buffers. ---
        var holder = MakeHolder(10, true);
        Expect(holder.Primary.Value() == 10, "holder.primary");
        Expect(holder.Spare != null && holder.Spare.Value() == 11, "holder.spare");
        Expect(holder.Many.Select(t => t.Value()).SequenceEqual(new long[] { 12, 13, 14 }), "holder.many");
        Expect(holder.Many.Select(t => t.Handle).Distinct().Count() == 3, "holder.many are distinct objects");
        Expect(SumHolder(holder) == 10 + 11 + 12 + 13 + 14, "sum_holder");
        // Encoding cloned each token, so the holder is still fully usable.
        Expect(SumHolder(holder) == 60, "sum_holder again after re-encoding");
        Expect(holder.Primary.Value() == 10, "primary alive after encoding twice");

        var primary = PrimaryOf(holder);
        Expect(!ReferenceEquals(primary, holder.Primary), "primary_of is a new wrapper");
        Expect(primary.Handle == holder.Primary.Handle, "primary_of wraps the same native object");
        Expect(primary.Value() == 10, "primary_of value");
        Expect(SamePrimary(holder, holder), "same_primary with itself");
        var other = MakeHolder(10, true);
        Expect(!SamePrimary(holder, other), "same_primary across holders");
        var rebuilt = new Holder(primary, null, new Token[0]);
        Expect(SamePrimary(holder, rebuilt), "same_primary through a consumer-built holder");
        Expect(SumHolder(rebuilt) == 10, "sum_holder of a consumer-built holder");

        var noSpare = MakeHolder(0, false);
        Expect(noSpare.Spare == null, "make_holder without spare");
        Expect(SumHolder(noSpare) == 0 + 2 + 3 + 4, "sum_holder without spare");

        // Consumer-created tokens travel into buffers too.
        var t1 = new Token(1);
        var t2 = new Token(2);
        var t3 = new Token(long.MinValue);
        var own = new Holder(t1, t2, new[] { t3, t1 });
        Expect(SumHolder(own) == 1 + 2 + long.MinValue + 1, "sum_holder over consumer tokens");
        var ownPrimary = PrimaryOf(own);
        Expect(ownPrimary.Handle == t1.Handle && ownPrimary.Value() == 1, "primary_of consumer token");

        // Reference counting: dropping one wrapper leaves the others valid;
        // double Dispose is a no-op; a disposed wrapper throws.
        holder.Primary.Dispose();
        holder.Primary.Dispose();
        Expect(primary.Value() == 10, "primary_of wrapper outlives holder.Primary");
        try
        {
            holder.Primary.Value();
            Expect(false, "expected ObjectDisposedException");
        }
        catch (ObjectDisposedException)
        {
        }
        try
        {
            SumHolder(holder);
            Expect(false, "encoding a disposed token must throw before calling the producer");
        }
        catch (ObjectDisposedException)
        {
        }
        ownPrimary.Dispose();
        Expect(t1.Value() == 1, "t1 alive after disposing its twin");

        foreach (var t in new[] { primary, holder.Spare, other.Primary, other.Spare, noSpare.Primary, t1, t2, t3 }
                     .Concat(holder.Many).Concat(other.Many).Concat(noSpare.Many))
        {
            t.Dispose();
            t.Dispose();
        }
        using (var scoped = new Token(99))
        {
            Expect(scoped.Value() == 99, "scoped token");
        }

        Console.WriteLine("dotnet/codec: OK");
        return 0;
    }
}
