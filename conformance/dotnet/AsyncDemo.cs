// Conformance consumer: async-demo sample, .NET target.
//
// Drives the Task-backed async surface end to end: RunTask settled from the
// producer's worker thread and decoded from a value buffer into the plain
// TaskResult class, the typed TaskException (Code == TaskException.InvalidName)
// thrown for an empty name, the buffered list-of-records round trip through
// RunBatch, the direct-scalar RunNTasks, the sync CancelTask, and
// ActiveCallbacks settling to zero once every task body has completed. The
// producer cdylib is resolved by absolute path via a DllImportResolver
// reading WEAVEFFI_LIBRARY.

using System;
using System.Linq;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using WeaveFFI;

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

        // Async record return: the Task resolves with a TaskResult decoded
        // from the completion callback's value buffer.
        var result = await Tasks.RunTask("alpha");
        Expect(result.Id > 0, "RunTask assigns an id");
        Expect(result.Value == "completed: alpha", $"RunTask value (got {result.Value})");
        Expect(result.Success, "RunTask success flag");

        // Typed async error: the empty name faults the Task with the domain
        // exception carrying its stable code.
        try
        {
            await Tasks.RunTask("");
            Expect(false, "expected TaskException for empty name");
        }
        catch (TaskException e)
        {
            Expect(e.Code == TaskException.InvalidName, $"InvalidName code == 1 (got {e.Code})");
        }

        // Buffered list-of-records both ways.
        var batch = await Tasks.RunBatch(new[] { "a", "b", "c" });
        Expect(
            batch.Select(r => r.Value).SequenceEqual(
                new[] { "completed: a", "completed: b", "completed: c" }),
            $"RunBatch values (got [{string.Join(", ", batch.Select(r => r.Value))}])");
        Expect(batch.All(r => r.Success), "RunBatch success flags");

        // Direct scalar through the async callback.
        Expect(await Tasks.RunNTasks(7) == 7, "RunNTasks echoes n");

        // Sync functions beside the async ones.
        Expect(!Tasks.CancelTask(1), "CancelTask reports not cancelled");

        // Every spawned task body has completed by the time its callback fires.
        Expect(Tasks.ActiveCallbacks() == 0, "ActiveCallbacks settles to zero");

        Console.WriteLine("dotnet async-demo conformance: OK");
        return 0;
    }
}
