// Conformance consumer: events sample, .NET target.
//
// Exercises the delegate + GCHandle listener trampoline (register pins the
// Action<string>, the producer fires it synchronously on send, unregister
// frees the handle and stops delivery) and the opaque-iterator ABI behind
// Events.GetMessages. The producer cdylib is resolved by absolute path via a
// DllImportResolver reading WEAVEFFI_LIBRARY.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;
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

    static int Main()
    {
        var lib = Environment.GetEnvironmentVariable("WEAVEFFI_LIBRARY");
        NativeLibrary.SetDllImportResolver(typeof(Program).Assembly, (name, asm, search) =>
        {
            if (name == "weaveffi" && !string.IsNullOrEmpty(lib))
                return NativeLibrary.Load(lib);
            return IntPtr.Zero;
        });

        var received = new List<string>();
        ulong sub = Events.RegisterMessageListener(received.Add);
        Expect(sub > 0, "listener id positive");

        Events.SendMessage("alpha");
        Events.SendMessage("beta");
        Expect(received.SequenceEqual(new[] { "alpha", "beta" }),
            $"listener received sends (got [{string.Join(", ", received)}])");

        var msgs = Events.GetMessages().ToList();
        Expect(msgs.SequenceEqual(new[] { "alpha", "beta" }),
            $"iterator yields messages in order (got [{string.Join(", ", msgs)}])");

        // Unregister stops delivery; the producer still records the message.
        Events.UnregisterMessageListener(sub);
        Events.SendMessage("gamma");
        Expect(received.Count == 2, "no delivery after unregister");
        Expect(Events.GetMessages().Count() == 3, "producer kept recording");

        Console.WriteLine("dotnet/events: OK");
        return 0;
    }
}
