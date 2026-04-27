// Kvstore consumer smoke test (.NET / P/Invoke).
//
// Loads KVSTORE_LIB at runtime via NativeLibrary.SetDllImportResolver
// and exercises the minimum lifecycle every language binding must
// support: open store, put a value, get it back, delete it, close
// the store. Prints "OK" and exits 0 on success; any assertion
// failure exits 1.

using System;
using System.Reflection;
using System.Runtime.InteropServices;

[StructLayout(LayoutKind.Sequential)]
internal struct WeaveffiError
{
    public int Code;
    public IntPtr Message;
}

internal static class Kv
{
    private const string Lib = "kvstore";

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr weaveffi_kv_open_store(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path, ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_kv_close_store(IntPtr store, ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern bool weaveffi_kv_put(
        IntPtr store,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string key,
        byte[] value,
        UIntPtr valueLen,
        int kind,
        IntPtr ttlSeconds,
        ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr weaveffi_kv_get(
        IntPtr store, [MarshalAs(UnmanagedType.LPUTF8Str)] string key, ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern IntPtr weaveffi_kv_Entry_get_value(IntPtr entry, out UIntPtr len);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_kv_Entry_destroy(IntPtr entry);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern bool weaveffi_kv_delete(
        IntPtr store, [MarshalAs(UnmanagedType.LPUTF8Str)] string key, ref WeaveffiError err);

    [DllImport(Lib, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void weaveffi_free_bytes(IntPtr ptr, UIntPtr len);
}

internal static class Program
{
    private static IntPtr Resolver(string name, Assembly asm, DllImportSearchPath? path)
    {
        if (name != "kvstore") return IntPtr.Zero;
        var libPath = Environment.GetEnvironmentVariable("KVSTORE_LIB");
        if (string.IsNullOrEmpty(libPath))
        {
            Console.Error.WriteLine("KVSTORE_LIB not set");
            Environment.Exit(1);
        }
        return NativeLibrary.Load(libPath);
    }

    private static void Check(bool cond, string msg)
    {
        if (!cond)
        {
            Console.Error.WriteLine($"assertion failed: {msg}");
            Environment.Exit(1);
        }
    }

    private static int Main()
    {
        NativeLibrary.SetDllImportResolver(typeof(Kv).Assembly, Resolver);

        var err = new WeaveffiError();
        var store = Kv.weaveffi_kv_open_store("/tmp/kvstore-dotnet-smoke", ref err);
        Check(err.Code == 0, "open_store error");
        Check(store != IntPtr.Zero, "open_store returned null");

        err = new WeaveffiError();
        var value = new byte[] { 0x68, 0x65, 0x6c, 0x6c, 0x6f };
        var ok = Kv.weaveffi_kv_put(store, "greeting", value, (UIntPtr)5, 1, IntPtr.Zero, ref err);
        Check(err.Code == 0, "put error");
        Check(ok, "put returned false");

        err = new WeaveffiError();
        var entry = Kv.weaveffi_kv_get(store, "greeting", ref err);
        Check(err.Code == 0, "get error");
        Check(entry != IntPtr.Zero, "get returned null");

        var ptr = Kv.weaveffi_kv_Entry_get_value(entry, out var len);
        Check((ulong)len == 5, "value length mismatch");
        var got = new byte[(int)len];
        Marshal.Copy(ptr, got, 0, (int)len);
        for (var i = 0; i < value.Length; i++)
            Check(got[i] == value[i], $"value byte {i} mismatch");
        Kv.weaveffi_free_bytes(ptr, len);
        Kv.weaveffi_kv_Entry_destroy(entry);

        err = new WeaveffiError();
        var deleted = Kv.weaveffi_kv_delete(store, "greeting", ref err);
        Check(err.Code == 0, "delete error");
        Check(deleted, "delete did not return true");

        err = new WeaveffiError();
        Kv.weaveffi_kv_close_store(store, ref err);
        Check(err.Code == 0, "close_store error");

        Console.WriteLine("OK");
        return 0;
    }
}
