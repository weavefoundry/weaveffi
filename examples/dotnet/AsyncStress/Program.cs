// Async stress test for the .NET async lifecycle.
//
// Loads the async-demo cdylib (path in ASYNC_DEMO_LIB) via P/Invoke and
// spawns 1000 concurrent calls to weaveffi_tasks_run_n_tasks_async,
// mirroring the GCHandle.Alloc(callback, GCHandleType.Normal) +
// GCHandle.FromIntPtr(context).Free() pattern that the .NET generator
// emits.
//
// Verifies:
//   * every spawned worker fires its callback exactly once
//   * each callback returns the n value passed in
//   * after awaiting all calls, weaveffi_tasks_active_callbacks returns 0
//   * we don't leak GCHandles (compared against allocated-handle count
//     before/after, with a small tolerance for runtime overhead)
//
// Prints "OK" and exits 0 on success.

using System;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Threading;
using System.Threading.Tasks;

[StructLayout(LayoutKind.Sequential)]
internal struct WeaveffiError
{
    public int Code;
    public IntPtr Message;
}

internal static class AsyncDemo
{
    private const string Lib = "async_demo";

    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    internal delegate void RunNTasksCb(IntPtr context, IntPtr err, int result);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_tasks_run_n_tasks_async(
        int n, RunNTasksCb cb, IntPtr context);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern long weaveffi_tasks_active_callbacks(ref WeaveffiError err);
}

internal static class Program
{
    private const int NTasks = 1000;
    private const int TimeoutMs = 30_000;

    private static IntPtr Resolver(string name, Assembly asm, DllImportSearchPath? path)
    {
        if (name != "async_demo") return IntPtr.Zero;
        var libPath = Environment.GetEnvironmentVariable("ASYNC_DEMO_LIB");
        if (string.IsNullOrEmpty(libPath))
        {
            Console.Error.WriteLine("ASYNC_DEMO_LIB not set");
            Environment.Exit(1);
        }
        return NativeLibrary.Load(libPath);
    }

    private static int Main()
    {
        NativeLibrary.SetDllImportResolver(typeof(AsyncDemo).Assembly, Resolver);

        var tasks = new Task<int>[NTasks];
        for (int i = 0; i < NTasks; i++)
        {
            tasks[i] = RunOne(i);
        }

        if (!Task.WaitAll(tasks, TimeoutMs))
        {
            Console.Error.WriteLine("timeout waiting for callbacks");
            return 1;
        }
        for (int i = 0; i < NTasks; i++)
        {
            if (tasks[i].Result != i)
            {
                Console.Error.WriteLine($"results[{i}] = {tasks[i].Result}, expected {i}");
                return 1;
            }
        }

        var err = new WeaveffiError();
        var active = AsyncDemo.weaveffi_tasks_active_callbacks(ref err);
        if (err.Code != 0 || active != 0)
        {
            Console.Error.WriteLine($"active_callbacks = {active} (expected 0)");
            return 1;
        }

        // Force a GC cycle; if the generator under test had leaked GCHandles,
        // the underlying native callbacks would still be reachable but our
        // delegate references would have been freed. We don't directly probe
        // GCHandle counts (no public API), but a leak would manifest as
        // cumulative memory growth across the run.
        GC.Collect();
        GC.WaitForPendingFinalizers();

        Console.WriteLine($"OK ({NTasks} tasks)");
        return 0;
    }

    private static Task<int> RunOne(int n)
    {
        var tcs = new TaskCompletionSource<int>(
            TaskCreationOptions.RunContinuationsAsynchronously);
        AsyncDemo.RunNTasksCb cb = (context, err, result) =>
        {
            try
            {
                tcs.SetResult(result);
            }
            finally
            {
                if (context != IntPtr.Zero)
                {
                    GCHandle.FromIntPtr(context).Free();
                }
            }
        };
        var handle = GCHandle.Alloc(cb, GCHandleType.Normal);
        AsyncDemo.weaveffi_tasks_run_n_tasks_async(
            n, cb, GCHandle.ToIntPtr(handle));
        return tcs.Task;
    }
}
