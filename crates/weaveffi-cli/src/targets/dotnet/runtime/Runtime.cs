#nullable enable

using System;
using System.Buffers;
using System.Buffers.Binary;
using System.Collections;
using System.Collections.Generic;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

// Every native signature is blittable (`bool` slots are marshalled as one
// byte by the source generator), so the runtime never marshals on its own.
[assembly: DisableRuntimeMarshalling]

namespace {{NAMESPACE}};

/// <summary>The root of every error a throwing call reports. Each typed domain
/// exception derives from it, and a runtime failure of a throwing call (an
/// untyped producer error, a panic, a marshalling failure, or a failed
/// callback-interface implementation) surfaces as this type itself, with its
/// negative <see cref="Code"/>.</summary>
public class {{EXCEPTION}} : Exception
{
    /// <summary>An untyped producer error (a <c>throws: any</c> call's
    /// failure).</summary>
    public const int GenericErrorCode = -1;
    /// <summary>The producer panicked; the message carries the panic text.</summary>
    public const int PanicErrorCode = -2;
    /// <summary>A value couldn't be converted at the boundary (an argument the
    /// producer rejected, or a callback result it couldn't accept).</summary>
    public const int MarshalErrorCode = -3;
    /// <summary>A callback-interface implementation failed; the message carries
    /// the implementation's message.</summary>
    public const int ForeignErrorCode = -4;
    /// <summary>The call was cancelled. Surfaces as
    /// <see cref="OperationCanceledException"/>, never as this type.</summary>
    public const int CancelledErrorCode = -5;

    /// <summary>The error code the native library reported: positive for a
    /// domain error, negative for a runtime failure.</summary>
    public int Code { get; }

    /// <summary>Creates an exception carrying a native error code.</summary>
    /// <param name="code">The native error code.</param>
    /// <param name="message">The error message.</param>
    public {{EXCEPTION}}(int code, string message) : base(message)
    {
        Code = code;
    }

    /// <summary>Writes the fields of a domain error, the payload a callback
    /// reports with its code. A code without fields writes nothing.</summary>
    internal virtual void WritePayload(FfiBufferWriter writer)
    {
    }

    /// <summary>Maps a code outside every declared domain to its exception:
    /// cancellation to <see cref="OperationCanceledException"/>, anything else
    /// to this type.</summary>
    internal static Exception FromError(int code, string message, FfiBufferReader payload)
    {
        if (code == CancelledErrorCode)
        {
            return new OperationCanceledException(message);
        }
        return new {{EXCEPTION}}(code, message);
    }
}

/// <summary>A call that can't fail failed anyway: a bug in the native library,
/// or a callback failure the call couldn't report, rather than an error to
/// handle. <see cref="Code"/> is the runtime code (-2 a producer panic, -3 a
/// marshalling failure, -4 a failed callback, and so on), and the message
/// names it along with the producer's message.</summary>
public sealed class {{BUG_EXCEPTION}} : InvalidOperationException
{
    /// <summary>The error code the native library reported.</summary>
    public int Code { get; }

    /// <summary>Creates the exception for a failed call that can't
    /// fail.</summary>
    /// <param name="code">The native error code.</param>
    /// <param name="message">The producer's message.</param>
    public {{BUG_EXCEPTION}}(int code, string message)
        : base($"native call failed with code {code}: {message}")
    {
        Code = code;
    }

    /// <summary>Maps a failure of a call that isn't declared to throw:
    /// cancellation to <see cref="OperationCanceledException"/>, anything else
    /// to this type.</summary>
    internal static Exception FromError(int code, string message, FfiBufferReader payload)
    {
        if (code == {{EXCEPTION}}.CancelledErrorCode)
        {
            return new OperationCanceledException(message);
        }
        return new {{BUG_EXCEPTION}}(code, message);
    }
}

/// <summary>The native library couldn't be loaded, or the loaded library
/// doesn't match these bindings (another ABI revision, or a declaration that
/// is missing or changed). Every later call into the library throws the same
/// exception.</summary>
public sealed class {{LOAD_EXCEPTION}} : Exception
{
    /// <summary>Creates the exception with a message and the failure that
    /// caused it, if any.</summary>
    /// <param name="message">What went wrong.</param>
    /// <param name="inner">The underlying failure, if any.</param>
    public {{LOAD_EXCEPTION}}(string message, Exception? inner = null) : base(message, inner)
    {
    }
}

/// <summary>The native <c>{{LIBRARY}}</c> library behind these
/// bindings.</summary>
public static class {{LIBRARY_CLASS}}
{
    /// <summary>The C ABI revision these bindings were generated
    /// against.</summary>
    public const uint AbiVersion = {{ABI_VERSION}};

    /// <summary>Loads the native library and checks that it matches these
    /// bindings: its ABI revision and the contract entry of every declaration
    /// they use. Calling it is optional: the first call into the library runs
    /// the same check and throws the same exception. Calling it again after
    /// a success does nothing.</summary>
    /// <exception cref="{{LOAD_EXCEPTION}}">The library can't be found or
    /// loaded, or it doesn't match these bindings.</exception>
    public static void Check()
    {
        NativeMethods.Load(null);
    }
}

/// <summary>The native <c>{{PREFIX}}_error</c> slot.</summary>
[StructLayout(LayoutKind.Sequential)]
internal unsafe struct FfiError
{
    public int Code;
    public byte* MessagePtr;
    public nuint MessageLen;
    public byte* PayloadPtr;
    public nuint PayloadLen;
}

/// <summary>The native <c>{{PREFIX}}_contract_entry</c>: one declaration's
/// fingerprint in a module's contract table.</summary>
[StructLayout(LayoutKind.Sequential)]
internal struct FfiContractEntry
{
    public ulong Id;
    public ulong Hash;
}

/// <summary>The result type of an async call that returns nothing.</summary>
internal readonly struct FfiVoid
{
}

/// <summary>The UTF-8 bytes of a string argument, in a pooled buffer the
/// native side borrows for the call as a (pointer, length) pair, so interior
/// NULs survive. Pin it with <c>fixed</c>; dispose it after the call.</summary>
internal readonly ref struct FfiUtf8
{
    private readonly byte[] _bytes;

    /// <summary>The number of bytes.</summary>
    internal readonly nuint Length;

    internal FfiUtf8(string value)
    {
        var count = Encoding.UTF8.GetByteCount(value);
        _bytes = count == 0 ? Array.Empty<byte>() : ArrayPool<byte>.Shared.Rent(count);
        Length = (nuint)Encoding.UTF8.GetBytes(value, _bytes);
    }

    public ref byte GetPinnableReference()
    {
        return ref MemoryMarshal.GetArrayDataReference(_bytes);
    }

    public void Dispose()
    {
        if (_bytes.Length > 0)
        {
            ArrayPool<byte>.Shared.Return(_bytes);
        }
    }
}

/// <summary>Marshalling helpers shared by every generated call.</summary>
internal static unsafe class Ffi
{
    /// <summary>Decodes a borrowed UTF-8 run; the native side keeps
    /// ownership.</summary>
    internal static string ReadString(byte* ptr, nuint len)
    {
        return ptr == null || len == 0 ? "" : Encoding.UTF8.GetString(ptr, checked((int)len));
    }

    /// <summary>Copies a borrowed byte run; the native side keeps
    /// ownership.</summary>
    internal static byte[] ReadBytes(byte* ptr, nuint len)
    {
        return ptr == null || len == 0 ? Array.Empty<byte>() : new ReadOnlySpan<byte>(ptr, checked((int)len)).ToArray();
    }

    /// <summary>A borrowed typed array (or byte run) as a span, valid for the
    /// duration of a callback. A zero-length run may have a dangling
    /// pointer, which an empty span never reads.</summary>
    internal static ReadOnlySpan<T> Borrow<T>(T* ptr, nuint len) where T : unmanaged
    {
        return len == 0 ? ReadOnlySpan<T>.Empty : new ReadOnlySpan<T>(ptr, checked((int)len));
    }

    /// <summary>Decodes a borrowed value buffer, which must hold exactly one
    /// value; the native side keeps ownership.</summary>
    internal static T ReadBuffer<T>(byte* ptr, nuint len, Func<FfiBufferReader, T> read)
    {
        var reader = new FfiBufferReader(ptr, len);
        var value = read(reader);
        reader.ExpectEnd();
        return value;
    }

    /// <summary>Decodes a returned UTF-8 string and releases it.</summary>
    internal static string TakeString(byte* ptr, nuint len)
    {
        try
        {
            return ReadString(ptr, len);
        }
        finally
        {
            NativeMethods.FreeBytes(ptr, len);
        }
    }

    /// <summary>Copies returned bytes and releases them.</summary>
    internal static byte[] TakeBytes(byte* ptr, nuint len)
    {
        try
        {
            return ReadBytes(ptr, len);
        }
        finally
        {
            NativeMethods.FreeBytes(ptr, len);
        }
    }

    /// <summary>Copies a returned typed array of <paramref name="len"/>
    /// elements and releases its <c>len * sizeof(T)</c> bytes.</summary>
    internal static T[] TakeArray<T>(T* ptr, nuint len) where T : unmanaged
    {
        if (ptr == null || len == 0)
        {
            return Array.Empty<T>();
        }
        try
        {
            return new ReadOnlySpan<T>(ptr, checked((int)len)).ToArray();
        }
        finally
        {
            NativeMethods.FreeBytes((byte*)ptr, len * (nuint)sizeof(T));
        }
    }

    /// <summary>Decodes a returned value buffer and releases it.</summary>
    internal static T TakeBuffer<T>(byte* ptr, nuint len, Func<FfiBufferReader, T> read)
    {
        try
        {
            return ReadBuffer(ptr, len, read);
        }
        finally
        {
            NativeMethods.FreeBytes(ptr, len);
        }
    }

    /// <summary>Releases a caller-owned error slot and returns the exception
    /// <paramref name="map"/> makes of it.</summary>
    internal static Exception TakeError(FfiError* err, delegate*<int, string, FfiBufferReader, Exception> map)
    {
        try
        {
            return Describe(err, map);
        }
        finally
        {
            NativeMethods.ErrorClear(err);
        }
    }

    /// <summary>Releases a heap-boxed async error and returns the exception
    /// <paramref name="map"/> makes of it.</summary>
    internal static Exception TakeBoxedError(FfiError* err, delegate*<int, string, FfiBufferReader, Exception> map)
    {
        try
        {
            return Describe(err, map);
        }
        finally
        {
            NativeMethods.ErrorFree(err);
        }
    }

    private static Exception Describe(FfiError* err, delegate*<int, string, FfiBufferReader, Exception> map)
    {
        var message = ReadString(err->MessagePtr, err->MessageLen);
        return map(err->Code, message, new FfiBufferReader(err->PayloadPtr, err->PayloadLen));
    }

    /// <summary>Reports a failed callback-interface method to the producer
    /// through <c>out_err</c>: <paramref name="code"/> with the exception's
    /// message. The producer copies it.</summary>
    internal static void Report(FfiError* err, Exception e, int code)
    {
        using var message = new FfiUtf8(e.Message);
        fixed (byte* ptr = message)
        {
            NativeMethods.ErrorSet(err, code, ptr, message.Length);
        }
    }

    /// <summary>Reports an exception of the method's own error domain: its
    /// code, its message, and its fields as the payload, so the producer sees
    /// exactly that domain error. A payload that can't be encoded is reported
    /// as a generic failure instead.</summary>
    internal static void ReportDomain(FfiError* err, {{EXCEPTION}} e)
    {
        using var payload = new FfiBufferWriter();
        try
        {
            e.WritePayload(payload);
        }
        catch (Exception inner)
        {
            Report(err, inner, {{EXCEPTION}}.GenericErrorCode);
            return;
        }
        Report(err, e, e.Code);
        if (payload.Length > 0)
        {
            fixed (byte* ptr = payload.Written)
            {
                NativeMethods.ErrorSetPayload(err, ptr, (nuint)payload.Length);
            }
        }
    }

    /// <summary>Hands <paramref name="bytes"/> to the producer through a
    /// callback method's out slots, as a run allocated with
    /// <c>{{PREFIX}}_alloc</c> that the producer adopts.</summary>
    private static void Return(ReadOnlySpan<byte> bytes, byte** outPtr, nuint* outLen)
    {
        var run = bytes.Length == 0 ? null : NativeMethods.Alloc((nuint)bytes.Length);
        if (run == null && bytes.Length > 0)
        {
            throw new OutOfMemoryException("the native allocator returned null");
        }
        bytes.CopyTo(new Span<byte>(run, bytes.Length));
        *outPtr = run;
        *outLen = (nuint)bytes.Length;
    }

    /// <summary>Returns a string from a callback method.</summary>
    internal static void ReturnString(string value, byte** outPtr, nuint* outLen)
    {
        using var utf8 = new FfiUtf8(value);
        fixed (byte* ptr = utf8)
        {
            Return(new ReadOnlySpan<byte>(ptr, (int)utf8.Length), outPtr, outLen);
        }
    }

    /// <summary>Returns bytes from a callback method.</summary>
    internal static void ReturnBytes(byte[] value, byte** outPtr, nuint* outLen)
    {
        Return(value, outPtr, outLen);
    }

    /// <summary>Returns a typed array from a callback method: a run of
    /// <c>Length * sizeof(T)</c> bytes and the element count.</summary>
    internal static void ReturnArray<T>(T[] value, T** outPtr, nuint* outLen) where T : unmanaged
    {
        nuint count;
        Return(MemoryMarshal.AsBytes(value.AsSpan()), (byte**)outPtr, &count);
        *outLen = (nuint)value.Length;
    }

    /// <summary>Returns an encoded value from a callback method.</summary>
    internal static void ReturnBuffer<T>(T value, Action<FfiBufferWriter, T> write, byte** outPtr, nuint* outLen)
    {
        using var writer = new FfiBufferWriter();
        write(writer, value);
        Return(writer.Written, outPtr, outLen);
    }

    /// <summary>Returns an optional scalar from a callback method: the
    /// presence as the C return, the value through
    /// <paramref name="outValue"/>.</summary>
    internal static bool ReturnOptional<T>(T? value, T* outValue) where T : unmanaged
    {
        if (value is { } present)
        {
            *outValue = present;
            return true;
        }
        return false;
    }

    /// <summary>Pins a callback-interface implementation for the producer and
    /// returns the <c>ctx</c> it hands back to every trampoline. The
    /// producer releases it through the vtable's <c>free</c>. An absent
    /// optional implementation is a null <c>ctx</c>.</summary>
    internal static IntPtr Register(object? implementation)
    {
        return implementation == null ? IntPtr.Zero : GCHandle.ToIntPtr(GCHandle.Alloc(implementation));
    }

    /// <summary>Releases a <c>ctx</c> the producer never received (the call
    /// failed before reaching it) or no longer holds.</summary>
    internal static void Unregister(IntPtr ctx)
    {
        if (ctx != IntPtr.Zero)
        {
            GCHandle.FromIntPtr(ctx).Free();
        }
    }

    /// <summary>The implementation behind a trampoline's <c>ctx</c>.</summary>
    internal static T Target<T>(IntPtr ctx) where T : class
    {
        return (T)GCHandle.FromIntPtr(ctx).Target!;
    }
}

/// <summary>Value equality for record fields whose C# type compares by
/// reference: byte arrays, lists, and maps compare their contents.</summary>
internal static class FfiEquality
{
    /// <summary>Default equality (what a record compares its fields
    /// with).</summary>
    internal static bool Equal<T>(T a, T b)
    {
        return EqualityComparer<T>.Default.Equals(a, b);
    }

    internal static bool Bytes(byte[]? a, byte[]? b)
    {
        return ReferenceEquals(a, b) || a is not null && b is not null && a.AsSpan().SequenceEqual(b);
    }

    internal static bool Lists<T>(IReadOnlyList<T>? a, IReadOnlyList<T>? b)
    {
        return Lists(a, b, EqualityComparer<T>.Default);
    }

    internal static bool Lists<T>(IReadOnlyList<T>? a, IReadOnlyList<T>? b, Func<T, T, bool> equal)
    {
        if (ReferenceEquals(a, b))
        {
            return true;
        }
        if (a is null || b is null || a.Count != b.Count)
        {
            return false;
        }
        for (var i = 0; i < a.Count; i++)
        {
            if (!equal(a[i], b[i]))
            {
                return false;
            }
        }
        return true;
    }

    private static bool Lists<T>(IReadOnlyList<T>? a, IReadOnlyList<T>? b, EqualityComparer<T> comparer)
    {
        if (ReferenceEquals(a, b))
        {
            return true;
        }
        if (a is null || b is null || a.Count != b.Count)
        {
            return false;
        }
        for (var i = 0; i < a.Count; i++)
        {
            if (!comparer.Equals(a[i], b[i]))
            {
                return false;
            }
        }
        return true;
    }

    internal static bool Maps<K, V>(IReadOnlyDictionary<K, V>? a, IReadOnlyDictionary<K, V>? b)
    {
        var comparer = EqualityComparer<V>.Default;
        return Maps(a, b, comparer.Equals);
    }

    internal static bool Maps<K, V>(IReadOnlyDictionary<K, V>? a, IReadOnlyDictionary<K, V>? b, Func<V, V, bool> equal)
    {
        if (ReferenceEquals(a, b))
        {
            return true;
        }
        if (a is null || b is null || a.Count != b.Count)
        {
            return false;
        }
        foreach (var entry in a)
        {
            if (!b.TryGetValue(entry.Key, out var other) || !equal(entry.Value, other))
            {
                return false;
            }
        }
        return true;
    }

    internal static int BytesHash(byte[]? value)
    {
        if (value is null)
        {
            return 0;
        }
        var hash = new HashCode();
        hash.AddBytes(value);
        return hash.ToHashCode();
    }

    /// <summary>A list's hash: its count, which equal lists share.</summary>
    internal static int ListHash<T>(IReadOnlyList<T>? value)
    {
        return value?.Count ?? -1;
    }

    /// <summary>A map's hash: its count, which equal maps share.</summary>
    internal static int MapHash<K, V>(IReadOnlyDictionary<K, V>? value)
    {
        return value?.Count ?? -1;
    }
}

/// <summary>The native library handle, the load-time checks, and the imports
/// of the native runtime surface. The generated API declares its own
/// imports, and the contract it was generated with, in the other half of this
/// partial class.</summary>
internal static unsafe partial class NativeMethods
{
    /// <summary>The native library's base name.</summary>
    internal const string LibName = "{{LIBRARY}}";

    /// <summary>The environment variable naming an explicit library
    /// path.</summary>
    internal const string LibraryEnvVar = "{{LIBRARY_ENV}}";

    private static readonly object Gate = new object();
    private static IntPtr s_library;
    private static {{LOAD_EXCEPTION}}? s_failure;

    // Every import resolves through `Resolve`, which loads the library and
    // checks it once, so a library built for another ABI revision or missing
    // a declaration these bindings use fails before any call can misread
    // memory, with an exception the caller can catch.
    static NativeMethods()
    {
        try
        {
            NativeLibrary.SetDllImportResolver(typeof(NativeMethods).Assembly, Resolve);
        }
        catch (InvalidOperationException)
        {
            // The assembly already has a resolver; only an explicit Check()
            // runs the load-time checks then.
        }
    }

    private static IntPtr Resolve(string name, Assembly assembly, DllImportSearchPath? searchPath)
    {
        return name == LibName ? Load(searchPath) : IntPtr.Zero;
    }

    /// <summary>The checked library handle, loading it on first use. A
    /// failure is remembered and thrown again by every later call.</summary>
    internal static IntPtr Load(DllImportSearchPath? searchPath)
    {
        lock (Gate)
        {
            if (s_failure != null)
            {
                throw s_failure;
            }
            if (s_library != IntPtr.Zero)
            {
                return s_library;
            }
            var library = IntPtr.Zero;
            try
            {
                library = Open(searchPath);
                Verify(library);
                s_library = library;
                return library;
            }
            catch ({{LOAD_EXCEPTION}} e)
            {
                if (library != IntPtr.Zero)
                {
                    NativeLibrary.Free(library);
                }
                s_failure = e;
                throw;
            }
        }
    }

    private static IntPtr Open(DllImportSearchPath? searchPath)
    {
        var path = Environment.GetEnvironmentVariable(LibraryEnvVar);
        if (!string.IsNullOrEmpty(path))
        {
            try
            {
                return NativeLibrary.Load(path);
            }
            catch (Exception e) when (e is DllNotFoundException or BadImageFormatException)
            {
                throw new {{LOAD_EXCEPTION}}($"couldn't load {path} (named by {LibraryEnvVar}): {e.Message}", e);
            }
        }
        if (NativeLibrary.TryLoad(LibName, typeof(NativeMethods).Assembly, searchPath, out var library))
        {
            return library;
        }
        throw new {{LOAD_EXCEPTION}}(
            $"couldn't find the {LibName} native library; put it next to the app or on the library search path, or set {LibraryEnvVar} to its full path");
    }

    private static void Verify(IntPtr library)
    {
        if (!NativeLibrary.TryGetExport(library, "{{PREFIX}}_abi_version", out var abiVersion))
        {
            throw new {{LOAD_EXCEPTION}}(
                $"the loaded {LibName} library doesn't export {{PREFIX}}_abi_version; these bindings expect ABI revision {{{LIBRARY_CLASS}}.AbiVersion}");
        }
        var found = ((delegate* unmanaged[Cdecl]<uint>)abiVersion)();
        if (found != {{LIBRARY_CLASS}}.AbiVersion)
        {
            throw new {{LOAD_EXCEPTION}}(
                $"ABI mismatch: these bindings expect revision {{{LIBRARY_CLASS}}.AbiVersion} but the loaded {LibName} library reports revision {found}");
        }
        VerifyContracts(library);
    }

    static partial void VerifyContracts(IntPtr library);

    /// <summary>Fails the load unless the library's contract table
    /// <paramref name="symbol"/> carries every <paramref name="expected"/>
    /// declaration with an unchanged signature hash. Declarations the library
    /// has and these bindings don't are fine.</summary>
    private static void VerifyContract(IntPtr library, string symbol, (ulong Id, ulong Hash, string Path)[] expected)
    {
        if (!NativeLibrary.TryGetExport(library, symbol, out var table))
        {
            throw new {{LOAD_EXCEPTION}}(
                $"the loaded {LibName} library doesn't export {symbol}; regenerate the bindings or rebuild the library");
        }
        nuint len = 0;
        var entries = ((delegate* unmanaged[Cdecl]<nuint*, FfiContractEntry*>)table)(&len);
        var hashes = new Dictionary<ulong, ulong>(checked((int)len));
        for (nuint i = 0; i < len; i++)
        {
            hashes[entries[i].Id] = entries[i].Hash;
        }
        foreach (var (id, hash, path) in expected)
        {
            if (!hashes.TryGetValue(id, out var actual))
            {
                throw new {{LOAD_EXCEPTION}}(
                    $"{path} is missing from the library {LibName}; regenerate the bindings or rebuild the library");
            }
            if (actual != hash)
            {
                throw new {{LOAD_EXCEPTION}}(
                    $"{path} changed since these bindings were generated; regenerate the bindings or rebuild the library {LibName}");
            }
        }
    }

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_error_set")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial void ErrorSet(FfiError* err, int code, byte* messagePtr, nuint messageLen);

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_error_set_payload")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial void ErrorSetPayload(FfiError* err, byte* ptr, nuint len);

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_error_clear")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial void ErrorClear(FfiError* err);

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_error_free")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial void ErrorFree(FfiError* err);

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_alloc")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial byte* Alloc(nuint len);

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_free_bytes")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial void FreeBytes(byte* ptr, nuint len);

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_cancel_token_create")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial IntPtr CancelTokenCreate();

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_cancel_token_cancel")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial void CancelTokenCancel(IntPtr token);

    [LibraryImport(LibName, EntryPoint = "{{PREFIX}}_cancel_token_destroy")]
    [UnmanagedCallConv(CallConvs = new[] { typeof(CallConvCdecl) })]
    internal static partial void CancelTokenDestroy(IntPtr token);
}

/// <summary>One pending async call: the task it completes, the native
/// <c>context</c> that finds it again, and the caller's
/// <see cref="CancellationToken"/>. A cancellable function links the token to
/// a native cancel token; for any other function, cancelling abandons the
/// wait (the task completes as canceled at once, and the native call
/// finishes in the background and releases its result).</summary>
internal sealed class FfiCall<T>
{
    private readonly TaskCompletionSource<T> _tcs =
        new TaskCompletionSource<T>(TaskCreationOptions.RunContinuationsAsynchronously);
    private readonly CancellationToken _cancellation;
    private readonly object _gate = new object();
    private GCHandle _self;
    private IntPtr _token;
    private CancellationTokenRegistration _registration;
    private bool _completed;
    private bool _cancelling;

    internal FfiCall(CancellationToken cancellation)
    {
        _cancellation = cancellation;
        _self = GCHandle.Alloc(this);
    }

    /// <summary>The <c>context</c> argument for the launcher.</summary>
    internal IntPtr Context => GCHandle.ToIntPtr(_self);

    /// <summary>Creates the native cancel token for a cancellable launcher
    /// and cancels it when the caller's <see cref="CancellationToken"/>
    /// fires.</summary>
    internal IntPtr CancelToken()
    {
        _token = NativeMethods.CancelTokenCreate();
        if (_cancellation.CanBeCanceled)
        {
            _registration = _cancellation.Register(static state => ((FfiCall<T>)state!).Cancel(), this);
        }
        return _token;
    }

    /// <summary>The task, once the launcher returned. A call without a native
    /// cancel token stops waiting when the caller's token fires.</summary>
    internal Task<T> Launched()
    {
        if (_token == IntPtr.Zero && _cancellation.CanBeCanceled)
        {
            lock (_gate)
            {
                if (!_completed)
                {
                    _registration = _cancellation.Register(
                        static state => ((FfiCall<T>)state!).StopWaiting(), this);
                }
            }
        }
        return _tcs.Task;
    }

    /// <summary>Recovers the call behind a completion's <c>context</c>. Runs
    /// exactly once per launch, on a producer thread.</summary>
    internal static FfiCall<T> Complete(IntPtr context)
    {
        var handle = GCHandle.FromIntPtr(context);
        var call = (FfiCall<T>)handle.Target!;
        handle.Free();
        call.Release();
        return call;
    }

    /// <summary>Releases everything when the launcher threw before the
    /// producer took the call, so no completion will arrive.</summary>
    internal void Abandon()
    {
        _self.Free();
        Release();
    }

    /// <summary>Completes the task with the call's result. When the caller
    /// stopped waiting, a disposable result is disposed instead.</summary>
    internal void SetResult(T value)
    {
        if (!_tcs.TrySetResult(value) && value is IDisposable disposable)
        {
            disposable.Dispose();
        }
    }

    internal void SetException(Exception e)
    {
        _tcs.TrySetException(e);
    }

    /// <summary>Faults (or cancels, for the cancelled code) the task from a
    /// heap-boxed error, releasing the box.</summary>
    internal unsafe void SetError(FfiError* err, delegate*<int, string, FfiBufferReader, Exception> map)
    {
        var e = Ffi.TakeBoxedError(err, map);
        if (e is OperationCanceledException)
        {
            _tcs.TrySetCanceled(_cancellation);
        }
        else
        {
            _tcs.TrySetException(e);
        }
    }

    private void StopWaiting()
    {
        _tcs.TrySetCanceled(_cancellation);
    }

    // The registration callback and the completion race: whichever runs
    // last destroys the consumer's token reference, so a cancel never
    // touches a destroyed token.
    private void Cancel()
    {
        lock (_gate)
        {
            if (_completed)
            {
                return;
            }
            _cancelling = true;
        }
        NativeMethods.CancelTokenCancel(_token);
        bool destroy;
        lock (_gate)
        {
            _cancelling = false;
            destroy = _completed;
        }
        if (destroy)
        {
            NativeMethods.CancelTokenDestroy(_token);
        }
    }

    private void Release()
    {
        bool destroy;
        lock (_gate)
        {
            _completed = true;
            destroy = !_cancelling;
        }
        if (destroy)
        {
            _registration.Dispose();
            if (_token != IntPtr.Zero)
            {
                NativeMethods.CancelTokenDestroy(_token);
            }
        }
    }
}

/// <summary>Owns one native iterator and destroys it exactly once, when
/// enumeration ends, the enumerator is disposed, or the garbage collector
/// finalizes an abandoned one.</summary>
internal sealed unsafe class FfiIteratorHandle : SafeHandle
{
    private readonly delegate*<IntPtr, void> _destroy;

    internal FfiIteratorHandle(IntPtr iterator, delegate*<IntPtr, void> destroy)
        : base(IntPtr.Zero, true)
    {
        _destroy = destroy;
        SetHandle(iterator);
    }

    public override bool IsInvalid => handle == IntPtr.Zero;

    protected override bool ReleaseHandle()
    {
        _destroy(handle);
        return true;
    }
}

/// <summary>A lazily streamed sequence backed by native iterators. Each
/// enumeration launches a new native iterator (so a launch failure throws
/// from <c>GetEnumerator</c>), and each <c>MoveNext</c> pulls one
/// item.</summary>
internal sealed unsafe class FfiSequence<T> : IEnumerable<T>
{
    private readonly Func<FfiIteratorHandle> _launch;
    private readonly delegate*<FfiIteratorHandle, out T, bool> _next;

    internal FfiSequence(Func<FfiIteratorHandle> launch, delegate*<FfiIteratorHandle, out T, bool> next)
    {
        _launch = launch;
        _next = next;
    }

    public IEnumerator<T> GetEnumerator()
    {
        return new FfiEnumerator<T>(_launch(), _next);
    }

    IEnumerator IEnumerable.GetEnumerator()
    {
        return GetEnumerator();
    }
}

/// <summary>One enumeration of an <see cref="FfiSequence{T}"/>: owns its
/// native iterator, released on exhaustion or disposal.</summary>
internal sealed unsafe class FfiEnumerator<T> : IEnumerator<T>
{
    private readonly FfiIteratorHandle _iterator;
    private readonly delegate*<FfiIteratorHandle, out T, bool> _next;
    private T _current = default!;

    internal FfiEnumerator(FfiIteratorHandle iterator, delegate*<FfiIteratorHandle, out T, bool> next)
    {
        _iterator = iterator;
        _next = next;
    }

    public T Current => _current;

    object? IEnumerator.Current => _current;

    public bool MoveNext()
    {
        if (_iterator.IsClosed)
        {
            return false;
        }
        if (_next(_iterator, out _current))
        {
            return true;
        }
        _current = default!;
        _iterator.Dispose();
        return false;
    }

    public void Reset()
    {
        throw new NotSupportedException("a native iterator can't be reset; enumerate the sequence again");
    }

    public void Dispose()
    {
        _iterator.Dispose();
    }
}

/// <summary>Serializes values into the value-buffer wire format
/// (little-endian, packed) in a pooled buffer, returned on
/// <see cref="Dispose"/>.</summary>
internal sealed class FfiBufferWriter : IDisposable
{
    private byte[] _buf = ArrayPool<byte>.Shared.Rent(256);
    private int _len;

    /// <summary>The bytes written so far.</summary>
    internal ReadOnlySpan<byte> Written => _buf.AsSpan(0, _len);

    /// <summary>The number of bytes written so far.</summary>
    internal int Length => _len;

    public void Dispose()
    {
        var buf = _buf;
        _buf = Array.Empty<byte>();
        _len = 0;
        if (buf.Length > 0)
        {
            ArrayPool<byte>.Shared.Return(buf);
        }
    }

    private Span<byte> Grow(int extra)
    {
        if (_len + extra > _buf.Length)
        {
            var grown = ArrayPool<byte>.Shared.Rent(Math.Max(_buf.Length * 2, _len + extra));
            _buf.AsSpan(0, _len).CopyTo(grown);
            if (_buf.Length > 0)
            {
                ArrayPool<byte>.Shared.Return(_buf);
            }
            _buf = grown;
        }
        var span = _buf.AsSpan(_len, extra);
        _len += extra;
        return span;
    }

    internal void WriteBool(bool v) => WriteU8(v ? (byte)1 : (byte)0);

    internal void WriteI8(sbyte v) => WriteU8((byte)v);

    internal void WriteU8(byte v) => Grow(1)[0] = v;

    internal void WriteI16(short v) => BinaryPrimitives.WriteInt16LittleEndian(Grow(2), v);

    internal void WriteU16(ushort v) => BinaryPrimitives.WriteUInt16LittleEndian(Grow(2), v);

    internal void WriteI32(int v) => BinaryPrimitives.WriteInt32LittleEndian(Grow(4), v);

    internal void WriteU32(uint v) => BinaryPrimitives.WriteUInt32LittleEndian(Grow(4), v);

    internal void WriteI64(long v) => BinaryPrimitives.WriteInt64LittleEndian(Grow(8), v);

    internal void WriteU64(ulong v) => BinaryPrimitives.WriteUInt64LittleEndian(Grow(8), v);

    internal void WriteF32(float v) => BinaryPrimitives.WriteSingleLittleEndian(Grow(4), v);

    internal void WriteF64(double v) => BinaryPrimitives.WriteDoubleLittleEndian(Grow(8), v);

    private void WriteLen(int len) => WriteU32((uint)len);

    internal void WriteString(string v)
    {
        var count = Encoding.UTF8.GetByteCount(v);
        WriteLen(count);
        Encoding.UTF8.GetBytes(v, Grow(count));
    }

    internal void WriteBytes(byte[] v)
    {
        WriteLen(v.Length);
        v.CopyTo(Grow(v.Length));
    }

    /// <summary>Writes an object token. The token must carry its own strong
    /// reference (a fresh clone), which the reader adopts.</summary>
    internal void WriteObject(IntPtr token) => WriteU64((ulong)(nuint)token);

    internal void WriteList<T>(IReadOnlyList<T> value, Action<FfiBufferWriter, T> write)
    {
        WriteLen(value.Count);
        for (var i = 0; i < value.Count; i++)
        {
            write(this, value[i]);
        }
    }

    internal void WriteMap<K, V>(IReadOnlyDictionary<K, V> value, Action<FfiBufferWriter, K> writeKey, Action<FfiBufferWriter, V> writeValue)
    {
        WriteLen(value.Count);
        foreach (var entry in value)
        {
            writeKey(this, entry.Key);
            writeValue(this, entry.Value);
        }
    }

    internal void WriteOptional<T>(T? value, Action<FfiBufferWriter, T> write) where T : class
    {
        WriteBool(value is not null);
        if (value is not null)
        {
            write(this, value);
        }
    }

    internal void WriteOptionalValue<T>(T? value, Action<FfiBufferWriter, T> write) where T : struct
    {
        WriteBool(value.HasValue);
        if (value is { } present)
        {
            write(this, present);
        }
    }
}

/// <summary>Decodes values from a native value buffer, in place. The buffer
/// must stay alive while it's read. A malformed buffer is a bug on the side
/// that encoded it and throws <see cref="{{BUG_EXCEPTION}}"/> with the
/// marshalling code.</summary>
internal sealed unsafe class FfiBufferReader
{
    private static readonly Encoding Utf8Strict = new UTF8Encoding(false, true);

    private readonly byte* _data;
    private readonly int _len;
    private int _pos;

    internal FfiBufferReader(byte* data, nuint len)
    {
        _data = len == 0 ? null : data;
        _len = checked((int)len);
    }

    /// <summary>The exception for a buffer that breaks the wire
    /// format.</summary>
    internal static {{BUG_EXCEPTION}} Malformed(string what)
    {
        return new {{BUG_EXCEPTION}}({{EXCEPTION}}.MarshalErrorCode, "malformed value buffer: " + what);
    }

    private ReadOnlySpan<byte> Take(int n)
    {
        if (_len - _pos < n)
        {
            throw Malformed("buffer exhausted");
        }
        var span = new ReadOnlySpan<byte>(_data + _pos, n);
        _pos += n;
        return span;
    }

    internal bool ReadBool()
    {
        var b = ReadU8();
        if (b > 1)
        {
            throw Malformed("invalid bool byte");
        }
        return b == 1;
    }

    internal sbyte ReadI8() => (sbyte)ReadU8();

    internal byte ReadU8() => Take(1)[0];

    internal short ReadI16() => BinaryPrimitives.ReadInt16LittleEndian(Take(2));

    internal ushort ReadU16() => BinaryPrimitives.ReadUInt16LittleEndian(Take(2));

    internal int ReadI32() => BinaryPrimitives.ReadInt32LittleEndian(Take(4));

    internal uint ReadU32() => BinaryPrimitives.ReadUInt32LittleEndian(Take(4));

    internal long ReadI64() => BinaryPrimitives.ReadInt64LittleEndian(Take(8));

    internal ulong ReadU64() => BinaryPrimitives.ReadUInt64LittleEndian(Take(8));

    internal float ReadF32() => BinaryPrimitives.ReadSingleLittleEndian(Take(4));

    internal double ReadF64() => BinaryPrimitives.ReadDoubleLittleEndian(Take(8));

    /// <summary>A collection count. Elements can be zero-sized, so the count
    /// isn't checked against the remaining bytes; collections cap their
    /// preallocation instead.</summary>
    private int ReadLen()
    {
        var len = ReadU32();
        if (len > int.MaxValue)
        {
            throw Malformed("length prefix out of range");
        }
        return (int)len;
    }

    internal string ReadString()
    {
        var bytes = Take(ReadLen());
        try
        {
            return Utf8Strict.GetString(bytes);
        }
        catch (DecoderFallbackException)
        {
            throw Malformed("string is not valid UTF-8");
        }
    }

    internal byte[] ReadBytes() => Take(ReadLen()).ToArray();

    /// <summary>Reads an object token carrying one strong reference.</summary>
    internal IntPtr ReadObject()
    {
        var token = ReadU64();
        if (token == 0)
        {
            throw Malformed("null object token");
        }
        return (IntPtr)(nint)token;
    }

    internal T[] ReadList<T>(Func<FfiBufferReader, T> read)
    {
        var count = ReadLen();
        if (count == 0)
        {
            return Array.Empty<T>();
        }
        // A malformed count can't inflate the allocation past the input.
        var items = new List<T>(Math.Min(count, _len - _pos));
        for (var i = 0; i < count; i++)
        {
            items.Add(read(this));
        }
        return items.ToArray();
    }

    internal Dictionary<K, V> ReadMap<K, V>(Func<FfiBufferReader, K> readKey, Func<FfiBufferReader, V> readValue) where K : notnull
    {
        var count = ReadLen();
        var map = new Dictionary<K, V>(Math.Min(count, _len - _pos));
        for (var i = 0; i < count; i++)
        {
            var key = readKey(this);
            if (!map.TryAdd(key, readValue(this)))
            {
                throw Malformed("repeated map key");
            }
        }
        return map;
    }

    internal T? ReadOptional<T>(Func<FfiBufferReader, T> read) where T : class
    {
        return ReadBool() ? read(this) : null;
    }

    internal T? ReadOptionalValue<T>(Func<FfiBufferReader, T> read) where T : struct
    {
        return ReadBool() ? read(this) : null;
    }

    internal void ExpectEnd()
    {
        if (_pos != _len)
        {
            throw Malformed("trailing bytes");
        }
    }

    /// <summary>Returns <paramref name="value"/>, read from this buffer,
    /// after checking that nothing trails it.</summary>
    internal T End<T>(T value)
    {
        ExpectEnd();
        return value;
    }
}
