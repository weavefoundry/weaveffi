// Conformance consumer: kvstore sample, .NET target.
//
// Full-surface drive of the generated P/Invoke wrapper (Kvstore.cs, namespace
// Kvstore): the Store interface class (static Open factory throwing the typed
// KvException IoError=1004 on an empty path, instance methods put/get/delete/
// list_keys/count/clear, the deprecated legacy_put, the Task-returning
// Compact settled from the producer's worker thread, and the DefaultCapacity
// static), optional struct returns (Entry) with bytes / nullable-scalar /
// array / dictionary getters, the fluent EntryBuilder (list + map *input*
// marshalling), the IEnumerable-backed ListKeys iterator, the cross-module
// KvStats.GetStats(store), the delegate + GCHandle eviction listener
// (register -> fire synchronously on delete -> unregister), and the typed
// KvException codes (KeyNotFound=1001). The producer cdylib is resolved by
// absolute path via a DllImportResolver reading WEAVEFFI_LIBRARY.

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

        // Typed constructor error: an empty path reports KvError::IoError
        // through the domain exception.
        try
        {
            Store.Open("");
            Expect(false, "expected KvException for empty path");
        }
        catch (KvException e)
        {
            Expect(e.Code == KvException.IoError, $"IoError code == 1004 (got {e.Code})");
        }

        Expect(Store.DefaultCapacity() == 1_000_000, "static default capacity");

        using (var store = Store.Open("/tmp/conformance-kvstore-dotnet"))
        {
            var payload = new byte[] { 1, 2, 3 };

            Expect(store.Put("alpha", payload, EntryKind.Persistent, null), "put alpha");
            Expect(store.Put("beta", payload, EntryKind.Volatile, 3600), "put beta with ttl");
            Expect(store.Count() == 2, "count == 2");

            // Iterator-backed list-of-string return drained through IEnumerable.
            var keys = store.ListKeys(null).OrderBy(k => k).ToList();
            Expect(keys.SequenceEqual(new[] { "alpha", "beta" }),
                $"list_keys values (got [{string.Join(", ", keys)}])");

            // Optional struct return + getters over every complex field type.
            using (var alpha = store.Get("alpha"))
            {
                Expect(alpha != null, "get alpha present");
                Expect(alpha.Id > 0, "entry id positive");
                Expect(alpha.Key == "alpha", "entry key");
                Expect(alpha.Value.SequenceEqual(payload), "entry value bytes");
                Expect(alpha.ExpiresAt == null, "alpha ExpiresAt null");
                Expect(alpha.Tags.Length == 0, "alpha tags empty");
                Expect(alpha.Metadata.Count == 0, "alpha metadata empty");
            }
            using (var beta = store.Get("beta"))
            {
                Expect(beta != null && beta.ExpiresAt != null && beta.ExpiresAt > 0,
                    "beta ExpiresAt present");
            }

            // Typed method error: a missing key reports KvError::KeyNotFound.
            try
            {
                store.Get("missing");
                Expect(false, "expected KvException for missing key");
            }
            catch (KvException e)
            {
                Expect(e.Code == KvException.KeyNotFound,
                    $"KeyNotFound code == 1001 (got {e.Code})");
                Expect(e is WeaveFFIException, "typed exception extends the brand exception");
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
            using (var stats = KvStats.GetStats(store))
            {
                Expect(stats.TotalEntries == 2, "stats total entries == 2");
                Expect(stats.ExpiredEntries == 0, "stats expired entries == 0");
            }

            // Eviction listener: delete fires the pinned delegate synchronously.
            var evicted = new List<string>();
            ulong sub = Kv.RegisterEvictionListener(evicted.Add);
            Expect(sub > 0, "listener id positive");
            Expect(store.Delete("beta"), "delete beta");
            Expect(evicted.SequenceEqual(new[] { "beta" }),
                $"eviction fired for beta (got [{string.Join(", ", evicted)}])");

            // Unregister stops delivery.
            Kv.UnregisterEvictionListener(sub);
            Expect(store.Delete("alpha"), "delete alpha");
            Expect(evicted.Count == 1, "no eviction after unregister");

            // Deprecated method still round-trips (volatile put, no TTL).
#pragma warning disable CS0618
            Expect(store.LegacyPut("legacy", payload), "legacy_put inserts");
#pragma warning restore CS0618
            Expect(store.Count() == 1, "count == 1 after legacy_put");

            // Async: an immediately-expired entry gives compact 3 bytes to
            // reclaim; the Task completes from the producer's worker thread.
            Expect(store.Put("doomed", payload, EntryKind.Volatile, 0), "put doomed");
            long reclaimed = await store.Compact();
            Expect(reclaimed == 3, $"compact reclaimed 3 bytes (got {reclaimed})");
            Expect(store.Count() == 1, "live entry survives compact");

            store.Clear();
            Expect(store.Count() == 0, "store empty after clear");

            // Store.Dispose lowers to the destroy symbol when the using block
            // closes; no explicit close call exists (or is needed).
        }

        Console.WriteLine("dotnet/kvstore: OK");
        return 0;
    }
}
