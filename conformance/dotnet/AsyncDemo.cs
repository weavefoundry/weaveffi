// Conformance consumer: async-demo sample, .NET target.
//
// Drives the Task-based async surface of the generated AsyncDemo project:
// RunTask completing from a producer thread with a decoded TaskResult, the
// typed TaskException for an empty name, the buffered list round trip
// through RunBatch, the direct scalar RunNTasks, and real cancellation of
// Wait through a CancellationToken (cancelled mid-flight, cancelled before
// launch, and left to time out). Ends by asserting the producer's leak
// counters are zero.

using System;
using System.Diagnostics;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using AsyncDemo;

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

    static async Task Run()
    {
        // Async record return.
        var result = await Tasks.RunTask("alpha");
        Expect(result.Id > 0, "RunTask assigns an id");
        Expect(result.Value == "completed: alpha", $"RunTask value (got {result.Value})");
        Expect(result.Success, "RunTask success flag");

        // Typed async error.
        try
        {
            await Tasks.RunTask("");
            Expect(false, "expected TaskException for empty name");
        }
        catch (TaskException e)
        {
            Expect(e.Code == TaskException.InvalidName, $"InvalidName code == 1 (got {e.Code})");
            Expect(e is NativeException, "typed exception extends NativeException");
        }

        // Buffered list-of-records both ways.
        var batch = await Tasks.RunBatch(new[] { "a", "b", "c" });
        Expect(
            batch.Select(r => r.Value).SequenceEqual(
                new[] { "completed: a", "completed: b", "completed: c" }),
            $"RunBatch values (got [{string.Join(", ", batch.Select(r => r.Value))}])");
        Expect((await Tasks.RunBatch(new string[0])).Length == 0, "RunBatch of nothing");

        // Direct scalar, many in flight at once.
        var many = await Task.WhenAll(Enumerable.Range(0, 64).Select(i => Tasks.RunNTasks(i)));
        Expect(many.SequenceEqual(Enumerable.Range(0, 64)), "RunNTasks echoes n under load");

        // A cancellable call left alone completes after its timeout.
        Expect(await Tasks.Wait(20) == 20, "Wait(20) completes with 20");
        using (var unused = new CancellationTokenSource())
        {
            Expect(await Tasks.Wait(10, unused.Token) == 10, "Wait with an untouched token");
        }

        // Cancelling mid-flight cancels the native call: the task completes
        // as canceled long before the 60 s timeout.
        var clock = Stopwatch.StartNew();
        using (var cts = new CancellationTokenSource())
        {
            var pending = Tasks.Wait(60_000, cts.Token);
            await Task.Delay(50);
            Expect(!pending.IsCompleted, "Wait(60 s) is still pending");
            cts.Cancel();
            try
            {
                await pending;
                Expect(false, "expected OperationCanceledException");
            }
            catch (OperationCanceledException)
            {
            }
            Expect(pending.IsCanceled, "the task is canceled");
        }
        Expect(clock.Elapsed < TimeSpan.FromSeconds(10), $"cancel didn't wait for the timeout ({clock.Elapsed})");

        // A token cancelled before the call never launches it.
        using (var cts = new CancellationTokenSource())
        {
            cts.Cancel();
            var task = Tasks.Wait(60_000, cts.Token);
            Expect(task.IsCanceled, "pre-cancelled Wait is canceled immediately");
        }

        // Cancelling after a timer through CancelAfter, several at once.
        using (var cts = new CancellationTokenSource(TimeSpan.FromMilliseconds(30)))
        {
            var waits = Enumerable.Range(0, 8).Select(_ => Tasks.Wait(60_000, cts.Token)).ToArray();
            foreach (var w in waits)
            {
                try
                {
                    await w;
                    Expect(false, "expected cancellation");
                }
                catch (OperationCanceledException)
                {
                }
            }
        }

        // Every spawned task body has finished.
        for (var i = 0; i < 100 && Tasks.ActiveCallbacks() != 0; i++)
        {
            await Task.Delay(20);
        }
        Expect(Tasks.ActiveCallbacks() == 0, "ActiveCallbacks settles to zero");
    }

    static int Main()
    {
        Run().GetAwaiter().GetResult();
        LeakCheck.AssertNoLeaks("async_demo");
        Console.WriteLine("dotnet/async-demo: OK");
        return 0;
    }
}
