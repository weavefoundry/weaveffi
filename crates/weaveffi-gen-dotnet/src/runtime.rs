//! Shared runtime types emitted once per generated file: the branded and
//! typed exceptions, the raw error struct with its check helpers, the memory
//! helpers, the value-buffer writer and reader, and the single-use
//! enumerable wrapping iterator returns.

use heck::ToUpperCamelCase;
use weaveffi_core::codegen::CodeWriter;
use weaveffi_core::errors;
use weaveffi_core::manifest::xml_escape;
use weaveffi_core::model::ErrorBinding;

use crate::codec::emit_buffer_read;
use crate::docs::writer_doc;
use crate::types::cs_str;

/// The C# exception class name for one error domain: the domain stem with
/// exactly one `Exception` suffix, so `KvError` becomes `KvException` rather
/// than `KvErrorException`.
pub(crate) fn dotnet_exception_name(eb: &ErrorBinding) -> String {
    errors::exception_type_name(&eb.type_name)
}

/// The per-domain error-check helper name on `WeaveFFIError`; `KvException`
/// is checked by `CheckKv`.
pub(crate) fn check_method_name(eb: &ErrorBinding) -> String {
    let exc = dotnet_exception_name(eb);
    let stem = exc.strip_suffix("Exception").unwrap_or(&exc).to_string();
    format!("Check{stem}")
}

/// Render the generic branded exception every failure surfaces through when
/// no typed domain applies. The runtime trap codes are exposed as constants
/// so a caller can tell a producer panic (`-2`) from a callback-interface
/// implementation that threw (`-4`); both arrive through this class rather
/// than a typed domain exception.
pub(crate) fn render_exception_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("/// <summary>Raised for any WeaveFFI failure that isn't a typed domain");
    w.line("/// error: producer bugs, panics, marshalling failures, and exceptions");
    w.line("/// thrown by a callback-interface implementation.</summary>");
    w.line("public class WeaveFFIException : Exception");
    w.block("{", "}", |w| {
        w.line("/// <summary>An untyped producer error.</summary>");
        w.line("public const int GenericErrorCode = -1;");
        w.line("/// <summary>The producer panicked; the message carries the panic text.</summary>");
        w.line("public const int PanicErrorCode = -2;");
        w.line("/// <summary>An argument couldn't be lifted by the producer.</summary>");
        w.line("public const int MarshalErrorCode = -3;");
        w.line("/// <summary>A callback-interface implementation threw; the message");
        w.line("/// carries the original exception's message.</summary>");
        w.line("public const int ForeignErrorCode = -4;");
        w.blank();
        w.line("public int Code { get; }");
        w.blank();
        w.line("public WeaveFFIException(int code, string message) : base(message)");
        w.block("{", "}", |w| {
            w.line("Code = code;");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// One typed exception class per declared error domain, extending the generic
/// brand exception. Each code surfaces as a `public const int` (PascalCase),
/// and `FromCode` maps a raw error slot to the typed exception, falling back
/// to the generic `WeaveFFIException` for unknown codes. Declared codes are
/// validated positive, so every negative runtime code (generic failures,
/// producer panics, marshalling failures, foreign callback failures) takes
/// the fallback. When the
/// matched code declares payload fields, `FromCode` decodes them from the
/// serialized payload buffer and exposes each field in the exception's
/// `Data` dictionary, keyed by the IDL field name.
pub(crate) fn render_domain_exception(out: &mut String, eb: &ErrorBinding) {
    let exc = dotnet_exception_name(eb);
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line(format!(
        "/// <summary>Typed exception for the {} error domain (module {}).</summary>",
        eb.type_name,
        eb.owner_path.replace('_', ".")
    ));
    w.line(format!("public class {exc} : WeaveFFIException"));
    w.block("{", "}", |w| {
        for c in &eb.codes {
            if c.doc.is_some() {
                writer_doc(w, &c.doc);
            } else {
                w.line(format!("/// <summary>{}</summary>", xml_escape(&c.message)));
            }
            w.line(format!(
                "public const int {} = {};",
                errors::pascal(&c.name),
                c.value
            ));
        }
        w.blank();
        w.line(format!(
            "public {exc}(int code, string message) : base(code, message)"
        ));
        w.line("{");
        w.line("}");
        w.blank();
        w.line("/// <summary>Wraps a raw error slot in the typed exception, falling");
        w.line("/// back to <see cref=\"WeaveFFIException\"/> for unknown codes. Codes");
        w.line("/// declaring payload fields decode them into Data.</summary>");
        w.line(
            "internal static WeaveFFIException FromCode(int code, string message, byte[]? payload)",
        );
        w.block("{", "}", |w| {
            w.line("switch (code)");
            w.block("{", "}", |w| {
                for c in &eb.codes {
                    w.line(format!("case {}:", errors::pascal(&c.name)));
                    if c.fields.is_empty() {
                        w.indent();
                        w.line(format!(
                            "return new {exc}(code, string.IsNullOrEmpty(message) ? \"{}\" : message);",
                            cs_str(&c.message)
                        ));
                        w.dedent();
                    } else {
                        w.block("{", "}", |w| {
                            w.line(format!(
                                "var ex = new {exc}(code, string.IsNullOrEmpty(message) ? \"{}\" : message);",
                                cs_str(&c.message)
                            ));
                            w.line("if (payload != null)");
                            w.block("{", "}", |w| {
                                w.line("var reader = new WeaveFFIBufferReader(payload);");
                                for f in &c.fields {
                                    let var = format!("f{}", f.name.to_upper_camel_case());
                                    emit_buffer_read(w, &f.ty, &var, "reader", 0);
                                    w.line(format!("ex.Data[\"{}\"] = {var};", cs_str(&f.name)));
                                }
                                w.line("reader.ExpectEnd();");
                            });
                            w.line("return ex;");
                        });
                    }
                }
                w.line("default:");
                w.indent();
                w.line("return new WeaveFFIException(code, message);");
                w.dedent();
            });
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The raw error slot plus its check helpers: the generic `Check` (throws
/// `WeaveFFIException` on any non-zero code) and one `Check{Domain}` variant
/// per declared domain (throws the typed exception via `FromCode`). Every
/// check copies the message (and, for domains, the serialized payload) and
/// then calls `weaveffi_error_clear`, which frees both producer allocations,
/// before throwing.
pub(crate) fn render_error_struct(out: &mut String, domains: &[&ErrorBinding]) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("[StructLayout(LayoutKind.Sequential)]");
    w.line("internal struct WeaveFFIError");
    w.block("{", "}", |w| {
        w.line("public int Code;");
        w.line("public IntPtr Message;");
        w.line("public IntPtr PayloadPtr;");
        w.line("public UIntPtr PayloadLen;");
        w.blank();
        w.line("internal static byte[]? CopyPayload(WeaveFFIError err)");
        w.block("{", "}", |w| {
            w.line("if (err.PayloadPtr == IntPtr.Zero || (int)err.PayloadLen == 0)");
            w.block("{", "}", |w| {
                w.line("return null;");
            });
            w.line("var payload = new byte[(int)err.PayloadLen];");
            w.line("Marshal.Copy(err.PayloadPtr, payload, 0, (int)err.PayloadLen);");
            w.line("return payload;");
        });
        w.blank();
        w.line("internal static void Check(WeaveFFIError err)");
        w.block("{", "}", |w| {
            w.line("if (err.Code != 0)");
            w.block("{", "}", |w| {
                // The clear zeroes the slot, so capture code and message
                // before releasing the producer allocations.
                w.line("var code = err.Code;");
                w.line("var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";");
                w.line("NativeMethods.weaveffi_error_clear(ref err);");
                w.line("throw new WeaveFFIException(code, msg);");
            });
        });
        for eb in domains {
            let exc = dotnet_exception_name(eb);
            let check = check_method_name(eb);
            w.blank();
            w.line(format!("internal static void {check}(WeaveFFIError err)"));
            w.block("{", "}", |w| {
                w.line("if (err.Code != 0)");
                w.block("{", "}", |w| {
                    w.line("var code = err.Code;");
                    w.line("var msg = Marshal.PtrToStringUTF8(err.Message) ?? \"\";");
                    w.line("var payload = CopyPayload(err);");
                    w.line("NativeMethods.weaveffi_error_clear(ref err);");
                    w.line(format!("throw {exc}.FromCode(code, msg, payload);"));
                });
            });
        }
    });
    w.blank();
    out.push_str(&w.finish());
}

/// Render the internal string and pointer helpers shared by every wrapper,
/// plus the `WeaveFFIHandle` marker every object wrapper's adopting
/// constructor takes. The marker exists because the generated file is often
/// compiled into the consumer's own assembly, where an `internal` constructor
/// taking a bare `IntPtr` would join overload resolution with the public IDL
/// constructors: an `int` literal converts implicitly to `IntPtr` (`nint`) and
/// C# prefers that over `long`, so `new Token(1)` would silently adopt the
/// pointer `0x1`. No IDL type lowers to `WeaveFFIHandle`, so the adopting
/// constructor can never be chosen by accident.
pub(crate) fn render_helpers_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("/// <summary>One strong reference to a native object, on its way into");
    w.line("/// a wrapper that adopts it. A distinct type (rather than a bare");
    w.line("/// <c>IntPtr</c>) so the adopting constructor never competes with a");
    w.line("/// public constructor taking an integer.</summary>");
    w.line("internal readonly struct WeaveFFIHandle");
    w.block("{", "}", |w| {
        w.line("internal readonly IntPtr Value;");
        w.blank();
        w.line("internal WeaveFFIHandle(IntPtr value)");
        w.block("{", "}", |w| {
            w.line("Value = value;");
        });
    });
    w.blank();
    w.line("internal static class WeaveFFIHelpers");
    w.block("{", "}", |w| {
        w.line("internal static IntPtr StringToPtr(string? s)");
        w.block("{", "}", |w| {
            w.line("return s == null ? IntPtr.Zero : Marshal.StringToCoTaskMemUTF8(s);");
        });
        w.blank();
        w.line("internal static string? PtrToString(IntPtr ptr)");
        w.block("{", "}", |w| {
            w.line("return ptr == IntPtr.Zero ? null : Marshal.PtrToStringUTF8(ptr);");
        });
        w.blank();
        w.line("internal static void FreePtr(IntPtr ptr)");
        w.block("{", "}", |w| {
            w.line("Marshal.FreeCoTaskMem(ptr);");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}

/// The private buffer writer and reader implementing the WeaveFFI value-buffer
/// wire format (little-endian, packed, no alignment) over managed byte arrays.
/// The reader rejects malformed input (truncation, invalid bool or option
/// flags, oversized length prefixes, a zero object token, trailing bytes) by
/// throwing `InvalidOperationException`; a malformed buffer is always a
/// producer or consumer bug, never a recoverable domain error. Object tokens
/// are the pointer zero-extended to `u64`, so the conversions go through the
/// native-sized unsigned integer rather than `long`.
pub(crate) fn render_buffer_classes(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.block_raw(
        r#"/// <summary>Serializes values into the WeaveFFI value-buffer wire
/// format (little-endian, packed).</summary>
internal sealed class WeaveFFIBufferWriter
{
    private byte[] _buf = new byte[64];
    private int _len;

    private void Ensure(int extra)
    {
        if (_len + extra <= _buf.Length)
        {
            return;
        }
        var size = _buf.Length * 2;
        while (size < _len + extra)
        {
            size *= 2;
        }
        Array.Resize(ref _buf, size);
    }

    internal void WriteBool(bool v)
    {
        Ensure(1);
        _buf[_len++] = v ? (byte)1 : (byte)0;
    }

    internal void WriteI8(sbyte v)
    {
        Ensure(1);
        _buf[_len++] = (byte)v;
    }

    internal void WriteU8(byte v)
    {
        Ensure(1);
        _buf[_len++] = v;
    }

    internal void WriteU16(ushort v)
    {
        Ensure(2);
        _buf[_len++] = (byte)v;
        _buf[_len++] = (byte)(v >> 8);
    }

    internal void WriteI16(short v)
    {
        WriteU16((ushort)v);
    }

    internal void WriteU32(uint v)
    {
        Ensure(4);
        _buf[_len++] = (byte)v;
        _buf[_len++] = (byte)(v >> 8);
        _buf[_len++] = (byte)(v >> 16);
        _buf[_len++] = (byte)(v >> 24);
    }

    internal void WriteI32(int v)
    {
        WriteU32((uint)v);
    }

    internal void WriteU64(ulong v)
    {
        WriteU32((uint)v);
        WriteU32((uint)(v >> 32));
    }

    internal void WriteI64(long v)
    {
        WriteU64((ulong)v);
    }

    internal void WriteF32(float v)
    {
        WriteU32((uint)BitConverter.SingleToInt32Bits(v));
    }

    internal void WriteF64(double v)
    {
        WriteU64((ulong)BitConverter.DoubleToInt64Bits(v));
    }

    internal void WriteLen(int len)
    {
        WriteU32((uint)len);
    }

    internal void WriteOptionFlag(bool present)
    {
        WriteBool(present);
    }

    internal void WriteString(string v)
    {
        var bytes = System.Text.Encoding.UTF8.GetBytes(v);
        WriteLen(bytes.Length);
        Ensure(bytes.Length);
        Array.Copy(bytes, 0, _buf, _len, bytes.Length);
        _len += bytes.Length;
    }

    internal void WriteBytes(byte[] v)
    {
        WriteLen(v.Length);
        Ensure(v.Length);
        Array.Copy(v, 0, _buf, _len, v.Length);
        _len += v.Length;
    }

    internal void WriteObject(IntPtr token)
    {
        WriteU64((ulong)(nuint)(nint)token);
    }

    internal byte[] ToArray()
    {
        var outBuf = new byte[_len];
        Array.Copy(_buf, outBuf, _len);
        return outBuf;
    }
}

/// <summary>Decodes values from the WeaveFFI value-buffer wire format.
/// A malformed buffer indicates a producer/consumer contract violation and
/// throws <see cref="InvalidOperationException"/>.</summary>
internal sealed class WeaveFFIBufferReader
{
    private static readonly System.Text.Encoding Utf8Strict =
        new System.Text.UTF8Encoding(false, true);

    private readonly byte[] _data;
    private int _pos;

    internal WeaveFFIBufferReader(byte[] data)
    {
        _data = data;
    }

    private void Require(int n)
    {
        if (_data.Length - _pos < n)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: buffer exhausted");
        }
    }

    internal bool ReadBool()
    {
        Require(1);
        var b = _data[_pos++];
        if (b > 1)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: invalid bool byte");
        }
        return b == 1;
    }

    internal sbyte ReadI8()
    {
        Require(1);
        return (sbyte)_data[_pos++];
    }

    internal byte ReadU8()
    {
        Require(1);
        return _data[_pos++];
    }

    internal ushort ReadU16()
    {
        Require(2);
        var v = (ushort)(_data[_pos] | (_data[_pos + 1] << 8));
        _pos += 2;
        return v;
    }

    internal short ReadI16()
    {
        return (short)ReadU16();
    }

    internal uint ReadU32()
    {
        Require(4);
        var v = (uint)_data[_pos]
            | ((uint)_data[_pos + 1] << 8)
            | ((uint)_data[_pos + 2] << 16)
            | ((uint)_data[_pos + 3] << 24);
        _pos += 4;
        return v;
    }

    internal int ReadI32()
    {
        return (int)ReadU32();
    }

    internal ulong ReadU64()
    {
        var lo = (ulong)ReadU32();
        var hi = (ulong)ReadU32();
        return lo | (hi << 32);
    }

    internal long ReadI64()
    {
        return (long)ReadU64();
    }

    internal float ReadF32()
    {
        return BitConverter.Int32BitsToSingle(ReadI32());
    }

    internal double ReadF64()
    {
        return BitConverter.Int64BitsToDouble(ReadI64());
    }

    internal int ReadLen()
    {
        var len = ReadU32();
        if (len > (uint)(_data.Length - _pos))
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: length prefix exceeds remaining bytes");
        }
        return (int)len;
    }

    internal bool ReadOptionFlag()
    {
        return ReadBool();
    }

    internal string ReadString()
    {
        var len = ReadLen();
        string s;
        try
        {
            s = Utf8Strict.GetString(_data, _pos, len);
        }
        catch (ArgumentException)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: string is not valid UTF-8");
        }
        _pos += len;
        return s;
    }

    internal byte[] ReadBytes()
    {
        var len = ReadLen();
        var outBuf = new byte[len];
        Array.Copy(_data, _pos, outBuf, 0, len);
        _pos += len;
        return outBuf;
    }

    internal IntPtr ReadObject()
    {
        var token = ReadU64();
        if (token == 0)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: null object token");
        }
        return (nint)(nuint)token;
    }

    internal void ExpectEnd()
    {
        if (_pos != _data.Length)
        {
            throw new InvalidOperationException("malformed WeaveFFI value buffer: trailing bytes");
        }
    }
}
"#,
    );
    w.blank();
    out.push_str(&w.finish());
}

/// The single-use `IEnumerable<T>` wrapping every iterator return. The
/// native iterator is consumed (and destroyed) by its one enumerator, so a
/// second `GetEnumerator()` cannot yield anything; surfacing it as an
/// `InvalidOperationException` beats silently returning an empty or
/// double-destroyed sequence.
pub(crate) fn render_once_enumerable_class(out: &mut String) {
    let mut w = CodeWriter::four_space().with_depth(1);
    w.line("/// <summary>A lazily streamed sequence backed by a native iterator.");
    w.line("/// It can be enumerated exactly once; enumerate it promptly (or call");
    w.line("/// a materializing operator such as ToList) and let the enumerator be");
    w.line("/// disposed to release the native iterator.</summary>");
    w.line("internal sealed class WeaveFFIOnceEnumerable<T> : IEnumerable<T>");
    w.block("{", "}", |w| {
        w.line("private IEnumerator<T>? _enumerator;");
        w.blank();
        w.line("internal WeaveFFIOnceEnumerable(IEnumerator<T> enumerator)");
        w.block("{", "}", |w| {
            w.line("_enumerator = enumerator;");
        });
        w.blank();
        w.line("public IEnumerator<T> GetEnumerator()");
        w.block("{", "}", |w| {
            w.line("var e = System.Threading.Interlocked.Exchange(ref _enumerator, null);");
            w.line("if (e == null)");
            w.block("{", "}", |w| {
                w.line(
                    "throw new InvalidOperationException(\"this sequence can be enumerated only once\");",
                );
            });
            w.line("return e;");
        });
        w.blank();
        w.line("System.Collections.IEnumerator System.Collections.IEnumerable.GetEnumerator()");
        w.block("{", "}", |w| {
            w.line("return GetEnumerator();");
        });
    });
    w.blank();
    out.push_str(&w.finish());
}
