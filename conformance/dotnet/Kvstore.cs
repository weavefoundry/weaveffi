// Conformance consumer: kvstore sample, .NET target (feature-complete).
//
// Loading the generated project checks the ABI revision and the `kv` and
// `report` contract tables. Then it drives the generated Kvstore project:
//
//   * the Store class: the throwing Open factory and the infallible
//     constructor, methods, statics, the deprecated Size, records (Entry,
//     StoreInfo), the EntryKind enum, maps and optionals, the logical clock;
//   * the KvException hierarchy with typed fields (KeyNotFound, Expired,
//     StoreFull, InvalidPath, Rejected) and ReportException.NothingToReport;
//   * lazy IEnumerable iterators of strings (throwing), records, and
//     objects, including one abandoned part-way;
//   * three callback interfaces implemented in C#: an IListener (filtered
//     by Accepts, told about every Change, detached when it throws, and
//     notified from a producer thread during compaction), an IPolicy (a
//     record return, a throwing method whose KvException.Rejected reaches
//     the Put caller with its fields, any other exception reaching it as
//     -4, an object parameter and object return, a null object rejected as
//     -3), and an ILoader passed as an optional parameter (string, bytes,
//     and optional-object returns; typed errors decoded by the producer or
//     passed through);
//   * Store objects in every position: parameter, return, optional, list,
//     map value, record field, iterator element, async result, and callback
//     parameter and return;
//   * Task-returning async calls: an async free function returning an
//     object (and failing with a typed error), a cancellable method
//     cancelled through a CancellationToken mid-pause (an
//     OperationCanceledException at once, its background work stopping
//     cooperatively), 32 concurrent async lists, and the nested module's
//     async function;
//   * the nested KvStats class and the sibling Report root;
//   * the ABI 5 shapes: an optional scalar (`long?`) parameter, return,
//     iterator item, async result, and callback parameter and return; typed
//     arrays as a return, an async result, and a callback parameter (a
//     span) and return (an array); `usize` counts (`ulong`); a `throws:
//     any` method (the root exception with code -1); callback failures the
//     sample turns into KvException.CallbackFailed; records with value
//     equality; re-enumerable iterators; and async calls that can't be
//     cancelled abandoning the wait when their token fires.
//
// Callback releases are observed through the producer's live-callback
// counter. Ends by asserting every leak counter is zero.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Kvstore;

internal sealed class TestListener : IListener
{
    private readonly string _skip;
    private readonly string _failOn;
    public int Puts, Removed, Expired, Cleared, OffMainThread;
    public uint LastVersion, LastCleared;
    public bool LastReplaced;
    public string LastKey;

    public TestListener(string skip = null, string failOn = null)
    {
        _skip = skip;
        _failOn = failOn;
    }

    public bool Accepts(string key)
    {
        if (key == _failOn)
        {
            throw new InvalidOperationException("listener refused");
        }
        return key != _skip;
    }

    public void OnChange(Change change)
    {
        if (Environment.CurrentManagedThreadId != Program.MainThread)
        {
            Interlocked.Increment(ref OffMainThread);
        }
        switch (change)
        {
            case Change.Put put:
                LastVersion = put.Entry.Version;
                LastReplaced = put.Replaced;
                LastKey = put.Entry.Key;
                Interlocked.Increment(ref Puts);
                break;
            case Change.Removed removed:
                LastKey = removed.Key;
                Interlocked.Increment(ref removed.Expired ? ref Expired : ref Removed);
                break;
            case Change.Cleared cleared:
                LastCleared = cleared.Count;
                Interlocked.Increment(ref Cleared);
                break;
        }
    }
}

internal sealed class TestPolicy : IPolicy
{
    private readonly Store _other;
    public int Admitted;

    public TestPolicy(Store other)
    {
        _other = other;
    }

    // An optional scalar in and out: "short" lives one tick, "forever"
    // never expires, "ttl-fail" fails with a typed error, and every other
    // key keeps the requested TTL.
    public long? TtlFor(string key, long? requested)
    {
        switch (key)
        {
            case "short":
                return 1;
            case "forever":
                return null;
            case "ttl-fail":
                throw new KvException.InvalidPath("no ttl for you");
            default:
                return requested;
        }
    }

    public Entry Admit(Entry entry)
    {
        Interlocked.Increment(ref Admitted);
        Program.Expect(entry.Version == 0, "the store assigns the version after admission");
        if (entry.Key.StartsWith("secret"))
        {
            throw new KvException.Rejected(entry.Key, "no secrets", "secrets are not stored");
        }
        if (entry.Key.StartsWith("boom"))
        {
            throw new InvalidOperationException("policy exploded");
        }
        // Tag it and store it encrypted (and try to rename it, which the
        // store ignores).
        return entry with { Key = "renamed", Kind = EntryKind.Encrypted, Tags = new[] { "admitted" } };
    }

    public Store Route(string key, Store home)
    {
        if (key.StartsWith("b/"))
        {
            return _other;
        }
        if (key.StartsWith("null/"))
        {
            return null; // a required object may not be null: -3
        }
        return home;
    }
}

internal sealed class TestLoader : ILoader
{
    private readonly Store _fallback;

    public TestLoader(Store fallback = null)
    {
        _fallback = fallback;
    }

    public string Name() => "dotnet-loader";

    public Store Fallback(string key) => key == "fb" ? _fallback : null;

    public byte[] Load(string key)
    {
        switch (key)
        {
            case "missing":
                throw new KvException.KeyNotFound("missing", "not in the loader"); // this key: none
            case "elsewhere":
                throw new KvException.KeyNotFound("other", "not in the loader"); // passed through
            case "broken":
                throw new InvalidOperationException("loader is broken"); // -4
            default:
                return Encoding.UTF8.GetBytes("loaded:" + key);
        }
    }
}

internal sealed class TestScorer : IScorer
{
    public enum Mode { Sizes, Fail, Short }

    private readonly Mode _mode;
    public ulong[] Seen;

    public TestScorer(Mode mode)
    {
        _mode = mode;
    }

    // `sizes` is a span over the producer's typed array, valid for the call.
    public double[] Scores(ReadOnlySpan<ulong> sizes)
    {
        Seen = sizes.ToArray();
        if (_mode == Mode.Fail)
        {
            throw new InvalidOperationException("scorer is out of order");
        }
        var n = _mode == Mode.Short ? 1 : sizes.Length;
        var scores = new double[n];
        for (var i = 0; i < n; i++)
        {
            scores[i] = sizes[i] * 1.0;
        }
        return scores;
    }
}

internal static class Program
{
    // The thread synchronous calls notify on (async continuations hop threads,
    // so each section records its own).
    public static int MainThread;

    public static void Expect(bool cond, string msg)
    {
        if (!cond)
        {
            Console.Error.WriteLine($"assertion failed: {msg}");
            Environment.Exit(1);
        }
    }

    static byte[] Bytes(string s) => Encoding.UTF8.GetBytes(s);

    static string Text(byte[] b) => Encoding.UTF8.GetString(b);

    static ulong LiveCallbacks() => LeakCheck.Live("kvstore", LeakCheck.Callbacks);

    static Entry Put(Store s, string key, string value, EntryKind kind = EntryKind.Persistent, long? ttl = null)
    {
        return s.Put(key, Bytes(value), kind, ttl);
    }

    // Runs `action`, which must throw `T`, and returns the exception.
    static T Throws<T>(Action action, string what) where T : Exception
    {
        try
        {
            action();
        }
        catch (T e)
        {
            return e;
        }
        catch (Exception e)
        {
            Expect(false, $"{what}: expected {typeof(T).Name}, got {e.GetType().Name}: {e.Message}");
        }
        Expect(false, $"{what}: expected {typeof(T).Name}, nothing thrown");
        return null;
    }

    static async Task<T> ThrowsAsync<T>(Func<Task> action, string what) where T : Exception
    {
        try
        {
            await action();
        }
        catch (T e)
        {
            return e;
        }
        catch (Exception e)
        {
            Expect(false, $"{what}: expected {typeof(T).Name}, got {e.GetType().Name}: {e.Message}");
        }
        Expect(false, $"{what}: expected {typeof(T).Name}, nothing thrown");
        return null;
    }

    static void ExpectKeyNotFound(Action action, string key, string what)
    {
        var e = Throws<KvException.KeyNotFound>(action, what);
        Expect(e.Code == 1001 && e.Key == key, $"{what}: KeyNotFound {{key: {key}}} (got {e.Key})");
    }

    static async Task Constructors()
    {
        var invalid = Throws<KvException.InvalidPath>(() => Store.Open(""), "Open(\"\")");
        Expect(invalid.Code == 1004 && invalid.Message == "invalid path", $"InvalidPath (got {invalid.Code} '{invalid.Message}')");
        using (var memory = new Store())
        {
            Expect(memory.Path() == "memory", "new Store() path");
            Expect(memory.Capacity() == Store.DefaultCapacity() && Store.DefaultCapacity() == 1_000_000, "default capacity");
        }
        using (var opened = await Kv.OpenStore("/async"))
        {
            Expect(opened.Path() == "/async", "OpenStore path");
        }
        var rejected = await ThrowsAsync<KvException.InvalidPath>(() => Kv.OpenStore(""), "OpenStore(\"\")");
        Expect(rejected.Message == "invalid path", "async InvalidPath message");
    }

    static void Basics()
    {
        using var s = Store.Open("/basics");

        // put returns the stored entry; the version counts puts of the key.
        var e = Put(s, "alpha", "one");
        Expect(e.Key == "alpha" && Text(e.Value) == "one" && e.Kind == EntryKind.Persistent && e.Version == 1,
            "first put");
        Expect(e.ExpiresAt == null && e.Tags.Count == 0 && e.Metadata.Count == 0, "first put defaults");
        e = Put(s, "alpha", "two", EntryKind.Volatile);
        Expect(e.Version == 2 && e.Kind == EntryKind.Volatile, "second put");

        // get (throwing record) and find (optional record).
        Expect(Text(s.Get("alpha").Value) == "two", "get");
        // Records compare by value, byte arrays and collections included.
        var first = s.Get("alpha");
        var second = s.Get("alpha");
        Expect(first == second && first.GetHashCode() == second.GetHashCode(), "entries are value-equal");
        Expect(first != (second with { Tags = new[] { "x" } }), "a different list differs");
        var missing = Throws<KvException.KeyNotFound>(() => s.Get("nope"), "get(nope)");
        Expect(missing.Key == "nope" && missing.Message == "key not found: nope", $"KeyNotFound (got '{missing.Message}')");
        Expect(s.Find("alpha")?.Version == 2, "find present");
        Expect(s.Find("nope") == null, "find absent");

        // TTLs follow the logical clock; an expired get reports when.
        Expect(Put(s, "ttl", "x", EntryKind.Volatile, 10).ExpiresAt == 10, "expires_at");
        Expect(s.Now() == 0, "now");
        Expect(s.Tick(9) == 9 && s.Count() == 2, "tick 9");
        Expect(s.Tick(1) == 10 && s.Count() == 1, "tick 1");
        var expired = Throws<KvException.Expired>(() => s.Get("ttl"), "get(ttl)");
        Expect(expired.Key == "ttl" && expired.ExpiredAt == 10, "Expired fields");
        ExpectKeyNotFound(() => s.Get("ttl"), "ttl", "the expired read removed it");

        // Capacity: a new key past it is StoreFull { capacity }.
        s.SetCapacity(1);
        Expect(s.Capacity() == 1, "capacity");
        Put(s, "alpha", "three"); // replacing is fine
        var full = Throws<KvException.StoreFull>(() => Put(s, "beta", "b"), "put past capacity");
        Expect(full.Capacity == 1, "StoreFull capacity");
        s.SetCapacity(100);

        // An undeclared enum value is a marshalling failure, which a
        // throwing call reports as the root exception.
        var marshal = Throws<NativeException>(() => s.Put("k", Bytes("v"), (EntryKind)9, null), "undeclared kind");
        Expect(marshal.Code == NativeException.MarshalErrorCode && !(marshal is KvException), "undeclared kind is -3");

        // delete, clear, and the deprecated size().
        Put(s, "beta", "b");
        Expect(s.Delete("beta") && !s.Delete("beta"), "delete");
#pragma warning disable CS0618
        Expect(s.Size() == 1, "size");
#pragma warning restore CS0618
        Expect(s.Clear() == 1 && s.Count() == 0, "clear");
    }

    static void Iterators()
    {
        using var s = Store.Open("/iter");
        Put(s, "user.bob", "b");
        Put(s, "user.alice", "a");
        Put(s, "sys.x", "xx");

        Expect(s.Keys(null).SequenceEqual(new[] { "sys.x", "user.alice", "user.bob" }), "keys in order");
        // Each enumeration launches its own native iterator, so a sequence
        // can be enumerated again, and a launch failure throws on enumeration.
        var all = s.Keys(null);
        Expect(all.SequenceEqual(all) && all.Count() == 3, "keys re-enumerates");
        var missing = s.Keys("zzz");
        ExpectKeyNotFound(() => missing.GetEnumerator(), "zzz", "keys(zzz)");
        ExpectKeyNotFound(() => missing.ToList(), "zzz", "keys(zzz) again");

        // Abandoning an iterator part-way releases it.
        using (var keys = s.Keys("user.").GetEnumerator())
        {
            Expect(keys.MoveNext() && keys.Current == "user.alice", "first key");
            Expect(LeakCheck.Live("kvstore", LeakCheck.Iterators) == 1, "the iterator is live");
        }
        Expect(LeakCheck.Live("kvstore", LeakCheck.Iterators) == 0, "the abandoned iterator is released");

        var entries = s.Entries("sys.").ToList();
        Expect(entries.Count == 1 && entries[0].Key == "sys.x" && Text(entries[0].Value) == "xx", "entries(sys.)");

        // partition: objects, created as they're pulled.
        var prefixes = new[] { "user.", "sys.", "none." };
        var counts = new uint[] { 2, 1, 0 };
        var n = 0;
        foreach (var part in s.Partition(prefixes))
        {
            using (part)
            {
                Expect(!part.Equals(s) && part.Count() == counts[n] && part.Path() == prefixes[n], $"partition {n}");
            }
            n++;
        }
        Expect(n == 3, "three partitions");
    }

    static void Listeners()
    {
        MainThread = Environment.CurrentManagedThreadId;
        using var s = Store.Open("/listen");
        var baseline = LiveCallbacks();

        var l = new TestListener(skip: "quiet");
        var id = s.Subscribe(l);
        Expect(id > 0 && s.ListenerCount() == 1, "subscribe");

        Put(s, "a", "1");
        Expect(l.Puts == 1 && l.LastVersion == 1 && !l.LastReplaced, "Put v1");
        Put(s, "a", "2");
        Expect(l.Puts == 2 && l.LastVersion == 2 && l.LastReplaced && l.LastKey == "a", "Put v2");
        Put(s, "quiet", "x"); // accepts() said no
        Expect(l.Puts == 2, "filtered");
        Expect(s.Delete("a") && l.Removed == 1 && l.LastKey == "a", "Removed");

        // An expired read removes the entry and says so.
        Put(s, "short", "x", EntryKind.Volatile, 1);
        s.Tick(1);
        Throws<KvException.Expired>(() => s.Get("short"), "get(short)");
        Expect(l.Expired == 1 && l.LastKey == "short", "Removed expired");

        Expect(s.Clear() == 1, "clear"); // "quiet" was left
        Expect(l.Cleared == 1 && l.LastCleared == 1, "Cleared");
        Expect(l.OffMainThread == 0, "synchronous calls notify on the calling thread");

        // Unsubscribing releases the listener once.
        Expect(LiveCallbacks() == baseline + 1, "one listener held");
        Expect(s.Unsubscribe(id), "unsubscribe");
        Expect(LiveCallbacks() == baseline, "the listener is released");
        Expect(!s.Unsubscribe(id) && s.ListenerCount() == 0, "unsubscribe again");

        // A listener that throws is detached (and released); the put succeeds.
        var failing = new TestListener(failOn: "boom");
        s.Subscribe(failing);
        Put(s, "fine", "1");
        Expect(failing.Puts == 1, "fine delivered");
        Put(s, "boom", "1");
        Expect(s.Count() == 2 && s.ListenerCount() == 0, "the failing listener is detached");
        Expect(LiveCallbacks() == baseline, "the failing listener is released");

        // Disposing the store releases the listeners it still holds.
        s.Subscribe(new TestListener());
        s.Subscribe(new TestListener());
        Expect(s.ListenerCount() == 2 && LiveCallbacks() == baseline + 2, "two listeners held");
        s.Dispose();
        Expect(LiveCallbacks() == baseline, "disposing the store releases its listeners");
    }

    static void Policies()
    {
        using var s = Store.Open("/policy");
        using var other = Store.Open("/other");
        var baseline = LiveCallbacks();

        var p = new TestPolicy(other);
        s.SetPolicy(p);
        Expect(s.HasPolicy(), "has policy");

        // admit's record return is what's stored (its key and version aside).
        var e = Put(s, "a", "1", EntryKind.Volatile);
        Expect(e.Key == "a" && e.Version == 1 && e.Kind == EntryKind.Encrypted, "admitted entry");
        Expect(e.Tags.SequenceEqual(new[] { "admitted" }), "admitted tags");

        // route: the object parameter and object return redirect a write.
        Put(s, "b/x", "2", EntryKind.Volatile);
        Expect(s.Count() == 1 && other.Count() == 1, "routed");

        // A typed error from the throwing callback reaches the caller with
        // its code and fields, and the domain's own message for them.
        var rejected = Throws<KvException.Rejected>(() => Put(s, "secret", "3", EntryKind.Volatile), "secret");
        Expect(rejected.Code == 1005 && rejected.Message == "write to secret rejected: no secrets",
            $"Rejected (got '{rejected.Message}')");
        Expect(rejected.Key == "secret" && rejected.Reason == "no secrets", "Rejected fields");

        // Any other exception is a callback failure, which the sample turns
        // into CallbackFailed with the implementation's message.
        var foreign = Throws<KvException.CallbackFailed>(() => Put(s, "boom", "4", EntryKind.Volatile), "boom");
        Expect(foreign.Code == 1006 && foreign.Message == "policy exploded" && foreign.Message_ == "policy exploded",
            $"callback failure (got {foreign.Code} '{foreign.Message}')");
        // So is a null required object, a return the runtime can't accept.
        Throws<KvException.CallbackFailed>(() => Put(s, "null/x", "6", EntryKind.Volatile), "null route");
        Expect(s.Count() == 1 && other.Count() == 1, "failed puts store nothing");
        Expect(p.Admitted == 5, $"admitted 5 times (got {p.Admitted})");

        // ttl_for: an optional scalar in and out, consulted before admit.
        Expect(Put(s, "short", "x", EntryKind.Volatile).ExpiresAt == 1, "ttl_for short");
        Expect(Put(s, "forever", "x", EntryKind.Volatile, 5).ExpiresAt == null, "ttl_for forever");
        Expect(Put(s, "kept", "x", EntryKind.Volatile, 5).ExpiresAt == 5, "ttl_for keeps the request");
        var noTtl = Throws<KvException.InvalidPath>(() => Put(s, "ttl-fail", "x", EntryKind.Volatile), "ttl-fail");
        Expect(noTtl.Code == 1004 && noTtl.Message == "invalid path", $"ttl_for's typed error (got '{noTtl.Message}')");
        Expect(s.Count() == 4 && p.Admitted == 8, $"three more admitted (got {s.Count()}, {p.Admitted})");

        // Replacing the policy releases the old one; null removes it.
        Expect(LiveCallbacks() == baseline + 1, "one policy held");
        s.SetPolicy(new TestPolicy(other));
        Expect(LiveCallbacks() == baseline + 1, "replacing releases the old policy");
        s.SetPolicy(null);
        Expect(LiveCallbacks() == baseline && !s.HasPolicy(), "SetPolicy(null) releases it");
        Put(s, "secret", "now allowed");
        Expect(s.Count() == 5, "no policy");
    }

    static void Loaders()
    {
        using var s = Store.Open("/load");
        var baseline = LiveCallbacks();

        // No loader (a null optional callback): a miss is none.
        Expect(s.GetOrLoad("k", null) == null, "no loader");

        // load's bytes are stored, tagged with the loader's name.
        var e = s.GetOrLoad("k", new TestLoader());
        Expect(e != null && Text(e.Value) == "loaded:k" && e.Kind == EntryKind.Volatile, "loaded");
        Expect(e.Metadata.Count == 1 && e.Metadata["source"] == "dotnet-loader", "loaded metadata");
        Expect(LiveCallbacks() == baseline, "the loader is released after the call");
        Expect(s.Count() == 1, "stored");
        // A hit doesn't consult the loader.
        Expect(s.GetOrLoad("k", new TestLoader())?.Version == 1, "hit");

        // The fallback store (an optional object return) is consulted first.
        using (var backup = Store.Open("/backup"))
        {
            Put(backup, "fb", "from backup");
            var copied = s.GetOrLoad("fb", new TestLoader(backup));
            Expect(copied != null && Text(copied.Value) == "from backup" && copied.Kind == EntryKind.Persistent,
                "copied from the fallback");
        }

        // KeyNotFound for this key: the producer decoded the fields and
        // answers none.
        Expect(s.GetOrLoad("missing", new TestLoader()) == null, "missing is none");
        // KeyNotFound for another key: passed through, fields intact.
        var elsewhere = Throws<KvException.KeyNotFound>(() => s.GetOrLoad("elsewhere", new TestLoader()), "elsewhere");
        Expect(elsewhere.Key == "other" && elsewhere.Message == "key not found: other", "passed through");
        // Any other failure is CallbackFailed with the loader's message.
        var broken = Throws<KvException.CallbackFailed>(() => s.GetOrLoad("broken", new TestLoader()), "broken");
        Expect(broken.Message == "loader is broken", $"broken loader (got {broken.Code} '{broken.Message}')");
        Expect(LiveCallbacks() == baseline, "every loader is released");
    }

    static async Task AsyncCalls()
    {
        MainThread = Environment.CurrentManagedThreadId;
        using var s = Store.Open("/async-calls");
        var l = new TestListener();
        s.Subscribe(l);
        Put(s, "old1", "x", EntryKind.Volatile, 1);
        Put(s, "old2", "x", EntryKind.Volatile, 1);
        Put(s, "keep", "x");
        s.Tick(5);

        // compact runs on a producer thread and notifies listeners there.
        using (var cts = new CancellationTokenSource())
        {
            Expect(await s.Compact(0, cts.Token) == 2, "compact(0) removes two");
        }
        Expect(l.Expired == 2 && l.OffMainThread == 2, $"notified from a producer thread ({l.OffMainThread})");
        Expect(s.Count() == 1, "one left");

        // Without a token the call never cancels.
        Expect(await s.Compact(5) == 0, "compact(5)");

        // Cancel mid-pause: the task cancels at once, and the background
        // pause notices the native token and stops.
        using (var cts = new CancellationTokenSource())
        {
            var pending = s.Compact(60_000, cts.Token);
            await Task.Delay(20);
            Expect(!pending.IsCompleted && Store.ActiveJobs() >= 1, "the pause is running");
            cts.Cancel();
            await ThrowsAsync<OperationCanceledException>(() => pending, "cancelled compact");
            Expect(pending.IsCanceled, "the task is canceled");
        }
        var stopped = false;
        for (var i = 0; i < 200 && !stopped; i++)
        {
            stopped = Store.ActiveJobs() == 0;
            if (!stopped)
            {
                await Task.Delay(10);
            }
        }
        Expect(stopped, "the cancelled pause stopped cooperatively");
        // An already-cancelled token doesn't launch.
        await ThrowsAsync<OperationCanceledException>(() => s.Compact(5, new CancellationToken(true)), "pre-cancelled");

        // get_many: an async list of optional records, launched concurrently.
        var many = Enumerable.Range(0, 32).Select(_ => s.GetMany(new[] { "keep", "gone", "keep" })).ToArray();
        foreach (var got in await Task.WhenAll(many))
        {
            Expect(got.Count == 3 && got[0] != null && got[1] == null && got[2]?.Key == "keep", "get_many");
        }

        // The nested module's async free function: objects in a list in, a
        // record out.
        using var other = Store.Open("/other");
        Put(other, "a", "123");
        var st = await KvStats.SummarizeAll(new[] { s, other });
        Expect(st.Entries == 2 && st.Bytes == 4, "summarize_all totals");
        Expect(st.ByKind.Count == 1 && st.ByKind[EntryKind.Persistent] == 2, "summarize_all by_kind");

        // A call that can't be cancelled stops waiting when its token fires;
        // the native call finishes and its result (here a Store) is released.
        await ThrowsAsync<OperationCanceledException>(() => Kv.OpenStore("/never", new CancellationToken(true)),
            "pre-cancelled open_store");
        var abandoned = 0;
        for (var i = 0; i < 32; i++)
        {
            using var cts = new CancellationTokenSource();
            var pending = Kv.OpenStore("/abandoned", cts.Token);
            cts.Cancel();
            try
            {
                using var opened = await pending;
            }
            catch (OperationCanceledException)
            {
                abandoned++;
            }
            Expect(pending.IsCanceled || pending.IsCompletedSuccessfully, "abandoned or finished");
        }
        Console.WriteLine($"dotnet/kvstore: {abandoned} of 32 open_store calls abandoned");
    }

    static void ObjectGraph()
    {
        var s = Store.Open("/graph");
        Put(s, "k", "v");

        // share(): the same object, alive through either wrapper.
        var shared = s.Share();
        Expect(shared.Equals(s), "share is the same object");
        s.Dispose();
        s.Dispose(); // safe twice
        Throws<ObjectDisposedException>(() => s.Count(), "a disposed wrapper");
        Expect(shared.Count() == 1, "alive through the shared reference");
        s = shared;

        // fork(): a distinct object with a copy of the entries.
        using var fork = s.Fork();
        Expect(!fork.Equals(s) && fork.Count() == 1 && fork.Path() == "/graph", "fork");
        Put(fork, "k2", "v");
        Expect(fork.Count() == 2 && s.Count() == 1, "fork is a copy");

        // larger(): `Store?` in and out.
        using var empty = Store.Open("/empty");
        Expect(empty.Larger(null) == null, "larger(none) on empty");
        using (var bigger = empty.Larger(fork))
        {
            Expect(bigger.Equals(fork), "larger(fork)");
        }
        using (var self = s.Larger(null))
        {
            Expect(self.Equals(s), "larger(none) on non-empty is self");
        }

        // describe(): a record whose fields carry objects.
        var info = s.Describe("main", fork);
        Expect(info.Label == "main" && info.Store.Equals(s) && info.Mirror.Equals(fork) && info.Count == 1, "describe");
        Expect(info.Mirror.Count() == 2, "the mirror is usable");

        // open_many(): a list of objects; one bad path fails the whole call.
        var many = Store.OpenMany(new[] { "/a", "/b" });
        Expect(many.Count == 2 && many[0].Path() == "/a" && many[1].Path() == "/b", "open_many");
        Throws<KvException.InvalidPath>(() => Store.OpenMany(new[] { "/a", "" }), "open_many with a bad path");

        // by_label(): records with objects in, a map with object values out.
        var named = Store.ByLabel(new[] { info, new StoreInfo("first", many[0], null, 0) });
        Expect(named.Count == 2 && named["main"].Equals(s) && named["first"].Equals(many[0]), "by_label");

        // total_count(): a list, a map, and an optional record, all carrying
        // objects (each encoding mints fresh references).
        Put(many[0], "m", "1");
        var three = new[] { many[0], many[1], fork };
        Expect(Store.TotalCount(three, named, info) == 6, "total_count with extra");
        Expect(Store.TotalCount(three, named, null) == 5, "total_count without extra");

        // Everything is still usable after crossing in buffers.
        Expect(s.Count() == 1 && fork.Count() == 2 && many[0].Count() == 1, "still usable");
        foreach (var store in many.Concat(named.Values).Concat(new[] { info.Store, info.Mirror, s }))
        {
            store.Dispose();
        }
    }

    static void StatsAndReport()
    {
        using var s = Store.Open("/stats");
        Put(s, "b", "12");
        Put(s, "a", "1");
        Put(s, "a", "123");
        Put(s, "c", "x", EntryKind.Encrypted);

        // kv.stats.summarize: the parent's Store and error domain.
        var st = KvStats.Summarize(s, null);
        Expect(st.Entries == 3 && st.Bytes == 6 && st.ByKind.Count == 2, "summarize totals");
        Expect(st.ByKind[EntryKind.Persistent] == 2 && st.ByKind[EntryKind.Encrypted] == 1, "summarize by_kind");
        ExpectKeyNotFound(() => KvStats.Summarize(s, "q"), "q", "summarize(q)");

        // report.render_report: the sibling root shares the Entry record.
        var lines = Report.RenderReport(s.Entries(null).ToArray());
        Expect(lines.SequenceEqual(new[] { "a: 3 bytes, Persistent, v2", "b: 2 bytes, Persistent", "c: 1 bytes, Encrypted" }),
            $"render_report (got [{string.Join("; ", lines)}])");
        var nothing = Throws<ReportException.NothingToReport>(() => Report.RenderReport(Array.Empty<Entry>()), "empty report");
        Expect(nothing.Code == 2001 && nothing.Message == "nothing to report", "NothingToReport");
    }

    static async Task Abi5Shapes()
    {
        using var s = Store.Open("/abi5");
        Put(s, "b", "12");
        s.Put("a", new byte[] { 1, 2, 3 }, EntryKind.Volatile, 7);
        Expect(s.Count() == 2UL, "count is a usize");

        // An optional scalar return.
        Expect(s.ExpiresAt("a") == 7 && s.ExpiresAt("b") == null && s.ExpiresAt("zzz") == null, "expires_at");
        // A typed-array return, in key order.
        Expect(s.ValueSizes().SequenceEqual(new ulong[] { 3, 2 }), "value_sizes");
        // Optional scalar iterator items, enumerated twice.
        var expirations = s.Expirations();
        for (var pass = 0; pass < 2; pass++)
        {
            Expect(expirations.SequenceEqual(new long?[] { 7, null }), $"expirations pass {pass}");
        }
        // An optional scalar async result.
        Expect(await s.VersionOf("a") == 1u && await s.VersionOf("q") == null, "version_of");
        // A typed-array async result.
        Put(s, "b", "x");
        Expect((await s.Versions(new[] { "b", "q", "a" })).SequenceEqual(new uint[] { 2, 0, 1 }), "versions");

        // A callback taking and returning typed arrays.
        using var r = Store.Open("/rank");
        Put(r, "a", "1");
        Put(r, "b", "333");
        Put(r, "c", "22");
        var scorer = new TestScorer(TestScorer.Mode.Sizes);
        Expect(r.Rank(scorer).SequenceEqual(new[] { "b", "c", "a" }), "rank");
        Expect(scorer.Seen.SequenceEqual(new ulong[] { 1, 3, 2 }), "the scorer saw the sizes");
        var failed = Throws<KvException.CallbackFailed>(() => r.Rank(new TestScorer(TestScorer.Mode.Fail)), "failing scorer");
        Expect(failed.Message == "scorer is out of order", $"failing scorer message (got '{failed.Message}')");
        var shortScores = Throws<KvException.CallbackFailed>(() => r.Rank(new TestScorer(TestScorer.Mode.Short)), "short scorer");
        Expect(shortScores.Message == "expected 3 scores, got 1", $"short scorer message (got '{shortScores.Message}')");

        // `throws: any` and a `usize` return: the root exception, code -1.
        using var imported = Store.Open("/import");
        Expect(imported.ImportLines("a=1\n\nb=two\n") == 2UL, "import_lines");
        Expect(Text(imported.Get("b").Value) == "two", "imported value");
        var untyped = Throws<NativeException>(() => imported.ImportLines("c=3\nbroken\nd=4"), "import_lines broken");
        Expect(untyped.GetType() == typeof(NativeException) && untyped.Code == NativeException.GenericErrorCode,
            $"untyped failure (got {untyped.GetType().Name} {untyped.Code})");
        Expect(untyped.Message == "line 2: expected key=value", $"untyped message (got '{untyped.Message}')");
        Expect(imported.Count() == 3, "c was stored, d wasn't");
    }

    static async Task Run()
    {
        KvstoreLibrary.Check();
        await Constructors();
        Basics();
        Iterators();
        Listeners();
        Policies();
        Loaders();
        await AsyncCalls();
        ObjectGraph();
        StatsAndReport();
        await Abi5Shapes();
    }

    static int Main()
    {
        Run().GetAwaiter().GetResult();
        LeakCheck.AssertNoLeaks("kvstore");
        Console.WriteLine("dotnet/kvstore: OK");
        return 0;
    }
}
