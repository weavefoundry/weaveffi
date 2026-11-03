// Shared by every .NET conformance consumer: reads the producer's leak
// counters and, at the end, forces the garbage collector and finalizers,
// then asserts that the counters (live objects, foreign callbacks,
// iterators, cancel tokens, and byte runs) are all back to zero. The
// counter export is reached through raw FFI on the same library the
// generated bindings load ({PREFIX}_LIBRARY).

using System;
using System.Runtime.InteropServices;
using System.Threading;

internal static unsafe class LeakCheck
{
    /// <summary>The counter kinds `debug_live` reports.</summary>
    public const int Objects = 0, Callbacks = 1, Iterators = 2, CancelTokens = 3, Allocations = 4;

    private static readonly string[] Kinds =
        { "objects", "callbacks", "iterators", "cancel tokens", "allocations" };

    private static delegate* unmanaged[Cdecl]<int, ulong> Counter(string prefix)
    {
        var path = Environment.GetEnvironmentVariable(prefix.ToUpperInvariant() + "_LIBRARY");
        var lib = NativeLibrary.Load(path!);
        return (delegate* unmanaged[Cdecl]<int, ulong>)NativeLibrary.GetExport(lib, prefix + "_debug_live");
    }

    /// <summary>The producer's live count of one kind, after letting the
    /// garbage collector release whatever wrappers are unreachable.</summary>
    public static ulong Live(string prefix, int kind)
    {
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();
        return Counter(prefix)(kind);
    }

    public static void AssertNoLeaks(string prefix)
    {
        var live = Counter(prefix);
        if (live(-1) != 1)
        {
            Console.Error.WriteLine("leak: the producer doesn't count live resources (build it with leak-check)");
            Environment.Exit(1);
        }
        var counts = new ulong[Kinds.Length];
        // Releasing one object can release others (a store drops its
        // listeners and policy), and async workers may still be dropping a
        // finished future, so settle for up to ~2 s before failing.
        for (var attempt = 0; attempt < 100; attempt++)
        {
            GC.Collect();
            GC.WaitForPendingFinalizers();
            GC.Collect();
            var clean = true;
            for (var kind = 0; kind < Kinds.Length; kind++)
            {
                counts[kind] = live(kind);
                clean &= counts[kind] == 0;
            }
            if (clean)
            {
                return;
            }
            Thread.Sleep(20);
        }
        for (var kind = 0; kind < Kinds.Length; kind++)
        {
            if (counts[kind] != 0)
            {
                Console.Error.WriteLine($"leak: {counts[kind]} live {Kinds[kind]}");
            }
        }
        Environment.Exit(1);
    }
}
