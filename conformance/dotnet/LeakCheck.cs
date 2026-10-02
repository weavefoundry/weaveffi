// Shared by every .NET conformance consumer: after the consumer has dropped
// its wrappers, force the garbage collector and finalizers, then assert that
// the producer's leak counters (live objects, foreign callbacks, iterators,
// cancel tokens, and returned allocations) are all back to zero. The
// counter export is reached through raw FFI on the same library the
// generated bindings load ({PREFIX}_LIBRARY).

using System;
using System.Runtime.InteropServices;
using System.Threading;

internal static unsafe class LeakCheck
{
    private static readonly string[] Kinds =
        { "objects", "callbacks", "iterators", "cancel tokens", "allocations" };

    public static void AssertNoLeaks(string prefix)
    {
        var path = Environment.GetEnvironmentVariable(prefix.ToUpperInvariant() + "_LIBRARY");
        var lib = NativeLibrary.Load(path!);
        var live = (delegate* unmanaged[Cdecl]<int, ulong>)NativeLibrary.GetExport(lib, prefix + "_debug_live");
        var counts = new ulong[Kinds.Length];
        // Releasing one object can release others (a bus drops its
        // subscribers), and async completions finish on producer threads, so
        // settle for a moment before failing.
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
