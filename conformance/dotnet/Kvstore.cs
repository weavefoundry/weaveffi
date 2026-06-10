// Conformance consumer: kvstore sample, .NET target.
//
// Full-surface drive of the generated P/Invoke wrapper (Kvstore.cs, namespace
// Kvstore): typed-handle returns (Store), optional struct returns (Entry, null
// when missing) with bytes / nullable-scalar / array / dictionary getters, the
// fluent EntryBuilder (list + map *input* marshalling), the IEnumerable-backed
// KvListKeys iterator, the cross-module KvStats.KvStatsGetStats, the delegate +
// GCHandle eviction listener (register -> fire synchronously on delete ->
// unregister), and the Task-returning KvCompactAsync settled from the
// producer's worker thread. The producer cdylib is resolved by absolute path
// via a DllImportResolver reading WEAVEFFI_LIBRARY.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using Kvstore;

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

    static async Task<int> Main()
    {
        var lib = Environment.GetEnvironmentVariable("WEAVEFFI_LIBRARY");
        NativeLibrary.SetDllImportResolver(typeof(Program).Assembly, (name, asm, search) =>
        {
            if (name == "weaveffi" && !string.IsNullOrEmpty(lib))
                return NativeLibrary.Load(lib);
            return IntPtr.Zero;
        });

        var store = Kv.KvOpenStore("/tmp/conformance-kvstore-dotnet");
        var payload = new byte[] { 1, 2, 3 };

        Expect(Kv.KvPut(store, "alpha", payload, EntryKind.Persistent, null), "put alpha");
        Expect(Kv.KvPut(store, "beta", payload, EntryKind.Volatile, 3600), "put beta with ttl");
        Expect(Kv.KvCount(store) == 2, "count == 2");

        // Iterator-backed list-of-string return drained through IEnumerable.
        var keys = Kv.KvListKeys(store, null).OrderBy(k => k).ToList();
        Expect(keys.SequenceEqual(new[] { "alpha", "beta" }),
            $"list_keys values (got [{string.Join(", ", keys)}])");

        // Optional struct return + getters over every complex field type.
        using (var alpha = Kv.KvGet(store, "alpha"))
        {
            Expect(alpha != null, "get alpha present");
            Expect(alpha.Id > 0, "entry id positive");
            Expect(alpha.Key == "alpha", "entry key");
            Expect(alpha.Value.SequenceEqual(payload), "entry value bytes");
            Expect(alpha.ExpiresAt == null, "alpha ExpiresAt null");
            Expect(alpha.Tags.Length == 0, "alpha tags empty");
            Expect(alpha.Metadata.Count == 0, "alpha metadata empty");
        }
        using (var beta = Kv.KvGet(store, "beta"))
        {
            Expect(beta != null && beta.ExpiresAt != null && beta.ExpiresAt > 0,
                "beta ExpiresAt present");
        }

        // Builder round-trips non-empty list/map inputs through the C `create`.
        using (var built = new EntryBuilder()
            .WithId(7)
            .WithKey("built")
            .WithValue(payload)
            .WithCreatedAt(1000)
            .WithExpiresAt(null)
            .WithTags(new[] { "hot", "fast" })
            .WithMetadata(new Dictionary<string, string> { ["source"] = "test", ["env"] = "prod" })
            .Build())
        {
            Expect(built.Tags.OrderBy(t => t).SequenceEqual(new[] { "fast", "hot" }), "built tags");
            Expect(built.Metadata["source"] == "test" && built.Metadata["env"] == "prod",
                "built metadata");
            Expect(built.ExpiresAt == null, "built ExpiresAt null");
        }

        // Cross-module call: Stats lives in kv.stats, store is a kv.Store.
        using (var stats = KvStats.KvStatsGetStats(store))
        {
            Expect(stats.TotalEntries == 2, "stats total entries == 2");
            Expect(stats.ExpiredEntries == 0, "stats expired entries == 0");
        }

        // Eviction listener: delete fires the pinned delegate synchronously.
        var evicted = new List<string>();
        ulong sub = Kv.KvRegisterEvictionListener(evicted.Add);
        Expect(sub > 0, "listener id positive");
        Expect(Kv.KvDelete(store, "beta"), "delete beta");
        Expect(evicted.SequenceEqual(new[] { "beta" }),
            $"eviction fired for beta (got [{string.Join(", ", evicted)}])");

        // Unregister stops delivery.
        Kv.KvUnregisterEvictionListener(sub);
        Expect(Kv.KvDelete(store, "alpha"), "delete alpha");
        Expect(evicted.Count == 1, "no eviction after unregister");

        // Async: an immediately-expired entry gives compact 3 bytes to
        // reclaim; the Task completes from the producer's worker thread.
        Expect(Kv.KvPut(store, "doomed", payload, EntryKind.Volatile, 0), "put doomed");
        long reclaimed = await Kv.KvCompactAsync(store);
        Expect(reclaimed == 3, $"compact reclaimed 3 bytes (got {reclaimed})");
        Expect(Kv.KvCount(store) == 0, "store empty after deletes + compact");

        // No explicit KvCloseStore: Store.Dispose lowers to the same destroy,
        // so closing here as well would double-free.
        Console.WriteLine("dotnet/kvstore: OK");
        return 0;
    }
}
