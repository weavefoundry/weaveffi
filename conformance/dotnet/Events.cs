// Conformance consumer: events sample, .NET target.
//
// Drives the generated Events project: the `ISubscriber` callback interface
// implemented in C# and adapted to the producer's vtable by the generated
// trampolines (Route, whose producer side returns `Result<_, ForeignError>`,
// steering Publish through the `Delivery` enum; OnMessage decoding the
// buffered `Message` record; OnAttached adopting an `EventBus` handed through
// the callback), the `EventBus` class (constructor, Task-based PublishLater,
// single-use Messages iterator, optional LastMessage, Dispose), the free
// function RouteOnce, subscribers that throw (reported to the caller as
// NativeException with ForeignErrorCode -4 for both the Result-returning
// and the plain callback methods), callbacks fired from producer threads,
// and wrapper equality as native identity. Ends by asserting the producer's
// leak counters are zero.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.CompilerServices;
using System.Threading;
using System.Threading.Tasks;
using Events;
using EventsApi = Events.Events;

internal sealed class TestSubscriber : ISubscriber
{
    private readonly string _skipTopic;
    private readonly string _failTopic;
    private readonly bool _keepBus;

    public readonly List<string> Routed = new List<string>();
    public readonly List<Message> Received = new List<Message>();
    public int Attached;
    public long AttachedCount = -1;
    public EventBus KeptBus;

    public TestSubscriber(string skipTopic, string failTopic, bool keepBus = false)
    {
        _skipTopic = skipTopic;
        _failTopic = failTopic;
        _keepBus = keepBus;
    }

    public Delivery Route(string topic)
    {
        Routed.Add(topic);
        if (topic == _failTopic)
        {
            throw new InvalidOperationException("subscriber rejected topic " + topic);
        }
        if (topic == _skipTopic)
        {
            return Delivery.Skip;
        }
        return topic == "stop" ? Delivery.AcceptAndStop : Delivery.Accept;
    }

    public long OnMessage(Message message)
    {
        if (message.Text == "explode")
        {
            throw new ApplicationException("subscriber exploded on " + message.Text);
        }
        Received.Add(message);
        return Received.Count;
    }

    public void OnAttached(EventBus bus)
    {
        Attached++;
        // The reference is ours: the bus is usable here, and releasing it
        // must not tear down the caller's live bus.
        AttachedCount = bus.SubscriberCount();
        if (_keepBus)
        {
            KeptBus = bus;
        }
        else
        {
            bus.Dispose();
        }
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

    // Subscribes a subscriber nothing else references, so once the producer's
    // `free(ctx)` releases the GCHandle the object becomes collectible.
    [MethodImpl(MethodImplOptions.NoInlining)]
    static WeakReference SubscribeTransient(EventBus bus)
    {
        var sub = new TestSubscriber("", "");
        bus.Subscribe(sub);
        return new WeakReference(sub);
    }

    static int Main()
    {
        Run().GetAwaiter().GetResult();
        LeakCheck.AssertNoLeaks("events");
        Console.WriteLine("dotnet/events: OK");
        return 0;
    }

    static async Task Run()
    {
        var bus = new EventBus();
        Expect(bus.SubscriberCount() == 0, "fresh bus has no subscribers");
        Expect(bus.LastMessage() == null, "fresh bus has no last message");
        Expect(!bus.Messages().Any(), "fresh bus has no messages");

        // Subscribe: the producer calls OnAttached with a reference-counted
        // bus object before retaining the subscriber.
        var a = new TestSubscriber("quiet", "", keepBus: true);
        var b = new TestSubscriber("", "");
        Expect(bus.Subscribe(a) == 1, "first subscribe returns 1");
        Expect(bus.Subscribe(b) == 2, "second subscribe returns 2");
        Expect(a.Attached == 1 && b.Attached == 1, "OnAttached fired once per subscriber");
        Expect(a.AttachedCount == 0, $"a saw 0 subscribers at attach (got {a.AttachedCount})");
        Expect(b.AttachedCount == 1, $"b saw 1 subscriber at attach (got {b.AttachedCount})");
        Expect(a.KeptBus != null && a.KeptBus.Equals(bus),
            "bus handed to OnAttached is the same native object");
        Expect(bus.SubscriberCount() == 2, "subscriber count == 2");

        // Delivery steers the accepted count: Accept delivers, Skip does not,
        // AcceptAndStop delivers and halts later subscribers.
        Expect(bus.Publish("news", "hello", new[] { "a", "b" }) == 2, "news accepted by both");
        Expect(bus.Publish("quiet", "psst", new string[0]) == 1, "quiet skipped by a");
        Expect(bus.Publish("stop", "last", new[] { "z" }) == 1, "stop halts after a");

        Expect(a.Routed.SequenceEqual(new[] { "news", "quiet", "stop" }),
            $"a routed every topic (got [{string.Join(", ", a.Routed)}])");
        Expect(b.Routed.SequenceEqual(new[] { "news", "quiet" }),
            $"b never routed the stopped topic (got [{string.Join(", ", b.Routed)}])");
        Expect(a.Received.Select(m => m.Text).SequenceEqual(new[] { "hello", "last" }),
            "a received hello and last");
        Expect(b.Received.Select(m => m.Text).SequenceEqual(new[] { "hello", "psst" }),
            "b received hello and psst");

        // The buffered Message record decodes intact inside the callback.
        var first = a.Received[0];
        Expect(first.Seq == 1, $"first seq == 1 (got {first.Seq})");
        Expect(first.Topic == "news", "first topic");
        Expect(first.Text == "hello", "first text");
        Expect(first.Tags.SequenceEqual(new[] { "a", "b" }),
            $"first tags (got [{string.Join(", ", first.Tags)}])");
        Expect(a.Received[1].Seq == 3 && a.Received[1].Tags.SequenceEqual(new[] { "z" }),
            "third message decoded for a");
        Expect(b.Received[1].Seq == 2 && b.Received[1].Tags.Length == 0,
            "second message decoded for b with empty tags");

        // Async publish settles the Task from the producer's worker thread.
        long later = await bus.PublishLater("async", "later");
        Expect(later == 2, $"PublishLater accepted by both (got {later})");
        Expect(a.Received.Last().Text == "later" && b.Received.Last().Text == "later",
            "async publish reached both subscribers");

        // Iterator and optional record return.
        var texts = bus.Messages().ToList();
        Expect(texts.SequenceEqual(new[] { "hello", "psst", "last", "later" }),
            $"messages in order (got [{string.Join(", ", texts)}])");
        var last = bus.LastMessage();
        Expect(last != null && last.Seq == 4 && last.Topic == "async" && last.Text == "later",
            "LastMessage is the async publish");

        // Early exit disposes the enumerator (and the native iterator).
        foreach (var t in bus.Messages())
        {
            Expect(t == "hello", "first streamed message");
            break;
        }
        var once = bus.Messages();
        once.ToList();
        try
        {
            once.ToList();
            Expect(false, "second enumeration should throw");
        }
        catch (InvalidOperationException)
        {
        }

        // Free function taking the callback interface without a bus.
        var probe = new TestSubscriber("quiet", "");
        Expect(EventsApi.RouteOnce(probe, "quiet") == Delivery.Skip, "RouteOnce skip");
        Expect(EventsApi.RouteOnce(probe, "stop") == Delivery.AcceptAndStop, "RouteOnce accept-and-stop");
        Expect(EventsApi.RouteOnce(probe, "x") == Delivery.Accept, "RouteOnce accept");
        Expect(probe.Routed.SequenceEqual(new[] { "quiet", "stop", "x" }), "RouteOnce routed topics");
        Expect(probe.Attached == 0, "RouteOnce never attaches");

        // A throwing subscriber surfaces to the caller as the foreign error
        // (code -4) with the exception's message, and the bus keeps working.
        var c = new TestSubscriber("", "boom");
        Expect(bus.Subscribe(c) == 3, "third subscribe returns 3");
        try
        {
            bus.Publish("boom", "x", new string[0]);
            Expect(false, "expected NativeException from throwing Route");
        }
        catch (NativeException e)
        {
            Expect(e.Code == NativeException.ForeignErrorCode,
                $"foreign error code == -4 (got {e.Code})");
            Expect(e.Code == -4, "ForeignErrorCode constant is -4");
            Expect(e.Message.Contains("subscriber rejected topic boom"),
                $"foreign error carries the exception message (got '{e.Message}')");
        }
        try
        {
            bus.Publish("ok", "explode", new string[0]);
            Expect(false, "expected NativeException from throwing OnMessage");
        }
        catch (NativeException e)
        {
            Expect(e.Code == NativeException.ForeignErrorCode,
                $"OnMessage foreign error code == -4 (got {e.Code})");
            Expect(e.Message.Contains("subscriber exploded"),
                $"OnMessage foreign error message (got '{e.Message}')");
        }
        Expect(bus.Publish("ok", "y", new string[0]) == 3, "bus still delivers after a foreign error");
        try
        {
            EventsApi.RouteOnce(c, "boom");
            Expect(false, "expected NativeException from RouteOnce");
        }
        catch (NativeException e)
        {
            Expect(e.Code == NativeException.ForeignErrorCode, "RouteOnce foreign error code");
        }

        // ClearSubscribers drops the producer's references; the consumer's
        // `free(ctx)` releases the GCHandle so the subscriber is collectible.
        var weak = SubscribeTransient(bus);
        Expect(bus.SubscriberCount() == 4, "subscriber count == 4");
        bus.ClearSubscribers();
        Expect(bus.SubscriberCount() == 0, "subscriber count == 0 after clear");
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.Collect();
        Expect(!weak.IsAlive, "freed subscriber was collected");
        Expect(bus.Publish("nobody", "home", new string[0]) == 0, "no subscribers accept");

        // Reference counting: the wrapper adopted in OnAttached outlives the
        // original wrapper; disposing either releases only its own reference.
        var kept = a.KeptBus;
        bus.Dispose();
        bus.Dispose();
        try
        {
            bus.SubscriberCount();
            Expect(false, "expected ObjectDisposedException");
        }
        catch (ObjectDisposedException)
        {
        }
        // Every publish is logged, including the two aborted by foreign errors.
        var keptCount = kept.Messages().Count();
        Expect(keptCount == 8, $"kept bus still alive with 8 messages (got {keptCount})");
        var keptLast = kept.LastMessage();
        Expect(keptLast != null && keptLast.Text == "home", "kept bus sees the last publish");
        kept.Dispose();
        kept.Dispose();

        using (var scoped = new EventBus())
        {
            Expect(scoped.Publish("t", "u", new string[0]) == 0, "scoped bus publishes");
        }

        // Callbacks arrive on whichever thread calls in, several at once, and
        // from the producer's own threads for async publishes.
        var counter = new CountingSubscriber();
        using (var busy = new EventBus())
        {
            busy.Subscribe(counter);
            await Task.WhenAll(Enumerable.Range(0, 8).Select(_ => Task.Run(() =>
            {
                for (var j = 0; j < 50; j++)
                {
                    busy.Publish("t", "m", new string[0]);
                }
            })));
            Expect(counter.Count == 400, $"400 concurrent deliveries (got {counter.Count})");
            var laters = await Task.WhenAll(Enumerable.Range(0, 16).Select(_ => busy.PublishLater("t", "later")));
            Expect(laters.All(n => n == 1), "every async publish delivered once");
            Expect(counter.Count == 416, $"416 deliveries in all (got {counter.Count})");
        }
    }
}

internal sealed class CountingSubscriber : ISubscriber
{
    private long _count;

    public long Count => Interlocked.Read(ref _count);

    public Delivery Route(string topic) => Delivery.Accept;

    public long OnMessage(Message message) => Interlocked.Increment(ref _count);

    public void OnAttached(EventBus bus) => bus.Dispose();
}
