// Conformance consumer: kvstore sample, .NET target.
//
// Full-surface drive of the generated P/Invoke wrapper (Kvstore.cs, namespace
// Kvstore) at ABI 2: the reference-counted Store object class (static Open
// factory throwing the typed KvException IoError=1004 on an empty path,
// instance methods put/get/delete/list_keys/count/clear, the deprecated
// legacy_put, the Task-returning Compact, the DefaultCapacity static), the
// optional buffered `Entry?` return decoded into a plain value class, direct
// value-class construction of Entry, the IEnumerable-backed ListKeys iterator,
// the cross-module KvStats.GetStats, the `IEvictionListener` callback
// interface implemented in C# (fired synchronously on delete and on an
// expired read, detached when it returns false, replaced and cleared, and a
// throwing listener surfacing to the caller as WeaveFFIException with
// ForeignErrorCode -4), the object graph (Share returning a second wrapper to
// the same native object, Fork, `Store?` both ways through Larger, the
// StoreInfo record carrying Store objects, OpenMany returning a list of
// objects, TotalCount taking a list of objects plus an optional record with
// objects), the typed KvException codes (KeyNotFound=1001, Expired=1002), and
// IDisposable release (safe double Dispose, ObjectDisposedException after).
// The producer cdylib is resolved by absolute path via a DllImportResolver
// reading WEAVEFFI_LIBRARY.
//
// The harness compiles the generated source into this assembly, so the
// wrapper's `internal` Handle property is reachable and used to prove two
// wrappers point at the same native object.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using Kvstore;

internal sealed class RecordingListener : IEvictionListener
{
    private readonly int _keepAfter;
    public readonly List<KeyValuePair<string, EvictionReason>> Evictions =
        new List<KeyValuePair<string, EvictionReason>>();

    public RecordingListener(int keepAfter)
    {
        _keepAfter = keepAfter;
    }

    public bool OnEvict(Entry entry, EvictionReason reason)
    {
        Evictions.Add(new KeyValuePair<string, EvictionReason>(entry.Key, reason));
        return Evictions.Count < _keepAfter;
    }
}

internal sealed class ThrowingListener : IEvictionListener
{
    public int Calls;

    public bool OnEvict(Entry entry, EvictionReason reason)
    {
        Calls++;
        throw new InvalidOperationException("listener refused " + entry.Key);
    }
}

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
            Expect(e.Message == "I/O failure", $"IoError default message (got '{e.Message}')");
        }

        Expect(Store.DefaultCapacity() == 1_000_000, "static default capacity");

        var store = Store.Open("/tmp/conformance-kvstore-dotnet");
        var payload = new byte[] { 1, 2, 3 };

        // The optional TTL crosses as a buffered `i64?` parameter.
        Expect(store.Put("alpha", payload, EntryKind.Persistent, null), "put alpha");
        Expect(store.Put("beta", payload, EntryKind.Volatile, 3600), "put beta with ttl");
        Expect(store.Count() == 2, "count == 2");

        // Iterator-backed list-of-string return drained through IEnumerable;
        // the optional prefix is a buffered `string?`.
        var keys = store.ListKeys(null).ToList();
        Expect(keys.SequenceEqual(new[] { "alpha", "beta" }),
            $"list_keys sorted values (got [{string.Join(", ", keys)}])");
        Expect(store.ListKeys("al").SequenceEqual(new[] { "alpha" }), "list_keys with prefix");
        Expect(!store.ListKeys("zzz").Any(), "list_keys with unmatched prefix is empty");

        // Optional buffered `Entry?` return decoded into a plain value class
        // covering every complex field type.
        var alpha = store.Get("alpha");
        Expect(alpha != null, "get alpha present");
        Expect(alpha.Id > 0, "entry id positive");
        Expect(alpha.Key == "alpha", "entry key");
        Expect(alpha.Value.SequenceEqual(payload), "entry value bytes");
        Expect(alpha.CreatedAt > 0, "entry created_at positive");
        Expect(alpha.ExpiresAt == null, "alpha ExpiresAt null");
        Expect(alpha.Tags.Length == 0, "alpha tags empty");
        Expect(alpha.Metadata.Count == 0, "alpha metadata empty");

        var beta = store.Get("beta");
        Expect(beta != null && beta.ExpiresAt != null && beta.ExpiresAt > beta.CreatedAt,
            "beta ExpiresAt present and after CreatedAt");
        Expect(beta.Id == alpha.Id + 1, "ids are monotonic");

        // Typed method error: a missing key reports KvError::KeyNotFound.
        try
        {
            store.Get("missing");
            Expect(false, "expected KvException for missing key");
        }
        catch (KvException e)
        {
            Expect(e.Code == KvException.KeyNotFound, $"KeyNotFound code == 1001 (got {e.Code})");
            Expect(e.Message == "key not found", $"KeyNotFound message (got '{e.Message}')");
            Expect(e is WeaveFFIException, "typed exception extends the brand exception");
        }

        // Entry is a plain value class: non-empty list and map fields live
        // directly on the instance.
        var built = new Entry(
            7,
            "built",
            payload,
            1000,
            null,
            new[] { "hot", "fast" },
            new Dictionary<string, string> { ["source"] = "test", ["env"] = "prod" });
        Expect(built.Tags.SequenceEqual(new[] { "hot", "fast" }), "built tags");
        Expect(built.Metadata["source"] == "test" && built.Metadata["env"] == "prod", "built metadata");
        Expect(built.ExpiresAt == null, "built ExpiresAt null");

        // Cross-module call: Stats lives in kv.stats, store is a kv.Store.
        var stats = KvStats.GetStats(store);
        Expect(stats.TotalEntries == 2, "stats total entries == 2");
        Expect(stats.TotalBytes == 6, "stats total bytes == 6");
        Expect(stats.ExpiredEntries == 0, "stats expired entries == 0");

        // Eviction listener: a C# implementation of the callback interface.
        // Delete fires it synchronously with the decoded Entry and the reason.
        var listener = new RecordingListener(keepAfter: 2);
        store.SetEvictionListener(listener);
        Expect(store.Delete("beta"), "delete beta");
        Expect(listener.Evictions.Count == 1
               && listener.Evictions[0].Key == "beta"
               && listener.Evictions[0].Value == EvictionReason.Deleted,
            "eviction fired for beta with Deleted");
        Expect(!store.Delete("beta"), "second delete reports missing");
        Expect(listener.Evictions.Count == 1, "no eviction for a missing key");

        // An already-expired entry is evicted on read with Expired, and the
        // typed error still reaches the caller. The listener returned false
        // on this second call, so the store detaches it.
        Expect(store.Put("expiring", new byte[] { 9 }, EntryKind.Volatile, -1), "put expired entry");
        try
        {
            store.Get("expiring");
            Expect(false, "expected KvException for expired key");
        }
        catch (KvException e)
        {
            Expect(e.Code == KvException.Expired, $"Expired code == 1002 (got {e.Code})");
        }
        Expect(listener.Evictions.Count == 2
               && listener.Evictions[1].Key == "expiring"
               && listener.Evictions[1].Value == EvictionReason.Expired,
            "eviction fired for expiring with Expired");
        Expect(store.Put("again", payload, EntryKind.Volatile, null), "put again");
        Expect(store.Delete("again"), "delete again");
        Expect(listener.Evictions.Count == 2, "detached listener is not notified");

        // A throwing listener surfaces to the caller as the foreign error
        // (code -4). Delete has a domain, so the wrapper's typed check falls
        // back to the brand exception for the reserved code.
        var thrower = new ThrowingListener();
        store.SetEvictionListener(thrower);
        Expect(store.Put("doomed", payload, EntryKind.Volatile, null), "put doomed");
        try
        {
            store.Delete("doomed");
            Expect(false, "expected WeaveFFIException from throwing listener");
        }
        catch (KvException)
        {
            Expect(false, "foreign error must not wear the domain exception type");
        }
        catch (WeaveFFIException e)
        {
            Expect(e.Code == WeaveFFIException.ForeignErrorCode, $"foreign error code == -4 (got {e.Code})");
            Expect(e.Message.Contains("listener refused doomed"),
                $"foreign error carries the exception message (got '{e.Message}')");
        }
        Expect(thrower.Calls == 1, "throwing listener was called once");
        Expect(store.Count() == 1, "doomed was removed before the listener ran");

        // Replacing and clearing the listener stops delivery.
        var replacement = new RecordingListener(keepAfter: int.MaxValue);
        store.SetEvictionListener(replacement);
        Expect(store.Put("r1", payload, EntryKind.Volatile, null) && store.Delete("r1"), "delete r1");
        Expect(replacement.Evictions.Count == 1 && thrower.Calls == 1, "replacement receives, old does not");
        store.ClearEvictionListener();
        Expect(store.Put("r2", payload, EntryKind.Volatile, null) && store.Delete("r2"), "delete r2");
        Expect(replacement.Evictions.Count == 1, "cleared listener is not notified");
        store.ClearEvictionListener();

        // Deprecated method still round-trips (volatile put, no TTL).
#pragma warning disable CS0618
        Expect(store.LegacyPut("legacy", payload), "legacy_put inserts");
#pragma warning restore CS0618
        Expect(store.Count() == 2, "count == 2 after legacy_put");

        // Share: a second wrapper to the SAME native object. Mutations through
        // one are visible through the other; disposing one leaves the other
        // usable.
        var twin = store.Share();
        Expect(!ReferenceEquals(twin, store), "Share returns a distinct wrapper");
        Expect(twin.Handle == store.Handle, "Share wraps the same native pointer");
        Expect(twin.Count() == 2, "twin sees the same entries");
        Expect(twin.Put("via-twin", payload, EntryKind.Persistent, null), "put via twin");
        Expect(store.Count() == 3 && store.Get("via-twin") != null, "store sees the twin's put");
        twin.Dispose();
        twin.Dispose();
        try
        {
            twin.Count();
            Expect(false, "expected ObjectDisposedException on disposed twin");
        }
        catch (ObjectDisposedException)
        {
        }
        Expect(store.Count() == 3, "store survives disposing its twin");

        // Fork: an independent copy.
        var forked = store.Fork();
        Expect(forked.Handle != store.Handle, "Fork is a different native object");
        Expect(forked.Count() == 3, "fork copied live entries");
        Expect(forked.Put("only-in-fork", payload, EntryKind.Volatile, null), "put into fork");
        Expect(forked.Count() == 4 && store.Count() == 3, "fork and original diverge");

        // `Store?` as parameter and return.
        var empty = Store.Open("/tmp/conformance-kvstore-dotnet-empty");
        Expect(empty.Larger(null) == null, "empty.Larger(null) is null");
        var bigger = empty.Larger(store);
        Expect(bigger != null && bigger.Handle == store.Handle, "empty.Larger(store) is store");
        Expect(bigger.Count() == 3, "returned larger store is usable");
        bigger.Dispose();
        var own = store.Larger(null);
        Expect(own != null && own.Handle == store.Handle, "store.Larger(null) is store itself");
        own.Dispose();
        var forkWins = store.Larger(forked);
        Expect(forkWins != null && forkWins.Handle == forked.Handle, "store.Larger(fork) is the fork");
        forkWins.Dispose();
        Expect(store.Count() == 3 && forked.Count() == 4, "both stores alive after Larger releases");

        // A record carrying objects: `store` is required, `mirror` optional.
        var info = store.Describe("primary", null);
        Expect(info.Label == "primary", "describe label");
        Expect(info.Count == 3, $"describe count == 3 (got {info.Count})");
        Expect(info.Store.Handle == store.Handle, "describe().Store is the same native object");
        Expect(info.Mirror == null, "describe().Mirror absent");
        Expect(info.Store.Count() == 3, "describe().Store is usable");
        var mirrored = store.Describe("mirrored", forked);
        Expect(mirrored.Mirror != null && mirrored.Mirror.Handle == forked.Handle,
            "describe().Mirror is the fork");
        Expect(mirrored.Mirror.Count() == 4, "describe().Mirror is usable");

        // A list of objects as a return, and objects inside a list and an
        // optional record as parameters (each encoded token is a fresh clone).
        var many = Store.OpenMany(new[] { "/tmp/many-a", "/tmp/many-b" });
        Expect(many.Length == 2, "open_many returns two stores");
        Expect(many[0].Handle != many[1].Handle, "open_many stores are distinct");
        Expect(many[0].Count() == 0 && many[1].Count() == 0, "open_many stores start empty");
        Expect(many[0].Put("m", payload, EntryKind.Volatile, null), "put into many[0]");
        Expect(Store.TotalCount(many, null) == 1, "total_count over the list");
        Expect(Store.TotalCount(many, info) == 1 + 3, "total_count adds the record's store");
        Expect(Store.TotalCount(many, mirrored) == 1 + 3, "total_count ignores the mirror");
        Expect(Store.TotalCount(new Store[0], null) == 0, "total_count of nothing");
        var handBuilt = new StoreInfo("hand", forked, store, 0);
        Expect(Store.TotalCount(new[] { store, forked }, handBuilt) == 3 + 4 + 4,
            "total_count with a consumer-built record");
        Expect(store.Count() == 3 && forked.Count() == 4 && many[0].Count() == 1,
            "every store survives being encoded into buffers");
        try
        {
            Store.OpenMany(new[] { "/tmp/ok", "" });
            Expect(false, "expected KvException from open_many");
        }
        catch (KvException e)
        {
            Expect(e.Code == KvException.IoError, "open_many reports IoError");
        }

        // Async: an immediately-expired entry gives compact 3 bytes to
        // reclaim; the Task completes from the producer's worker thread.
        Expect(store.Put("doomed", payload, EntryKind.Volatile, 0), "put doomed");
        long reclaimed = await store.Compact();
        Expect(reclaimed == 3, $"compact reclaimed 3 bytes (got {reclaimed})");
        Expect(store.Count() == 3, "live entries survive compact");
        Expect((await forked.Compact()) == 0, "nothing to compact in the fork");

        store.Clear();
        Expect(store.Count() == 0, "store empty after clear");
        Expect(info.Store.Count() == 0, "describe().Store observes the clear");

        // Release every wrapper. Records holding objects expose IDisposable
        // wrappers the consumer disposes; double Dispose is a no-op.
        foreach (var s in many)
        {
            s.Dispose();
            s.Dispose();
        }
        info.Store.Dispose();
        mirrored.Store.Dispose();
        mirrored.Mirror.Dispose();
        empty.Dispose();
        forked.Dispose();
        store.Dispose();
        store.Dispose();
        try
        {
            KvStats.GetStats(store);
            Expect(false, "expected ObjectDisposedException for a disposed store argument");
        }
        catch (ObjectDisposedException)
        {
        }
        try
        {
            await store.Compact();
            Expect(false, "expected ObjectDisposedException from async on a disposed store");
        }
        catch (ObjectDisposedException)
        {
        }

        using (var scoped = Store.Open("/tmp/conformance-kvstore-dotnet-scoped"))
        {
            Expect(scoped.Count() == 0, "scoped store opens empty");
        }

        Console.WriteLine("dotnet/kvstore: OK");
        return 0;
    }
}
