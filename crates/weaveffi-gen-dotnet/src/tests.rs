//! Rendering tests over a small hand-built API exercising every ABI
//! revision 2 shape: reference-counted objects, nullable objects, objects in
//! records and lists, iterators of objects, and a callback interface.

use camino::Utf8Path;
use weaveffi_core::backend::LanguageBackend;
use weaveffi_core::model::BindingModel;
use weaveffi_core::package::PackageContext;
use weaveffi_core::platform::{BinarySet, Platform};
use weaveffi_core::resolved::ResolvedApi;
use weaveffi_core::validate::validate_api;
use weaveffi_ir::ir::{
    Api, CallbackInterfaceDef, Function, InterfaceDef, Module, Param, StructDef, StructField,
    TypeRef,
};

use crate::{render_csharp, DotnetConfig, DotnetGenerator};

fn param(name: &str, ty: TypeRef) -> Param {
    Param {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn func(name: &str, params: Vec<Param>, returns: Option<TypeRef>) -> Function {
    Function {
        name: name.into(),
        params,
        returns,
        doc: None,
        throws: false,
        r#async: false,
        cancellable: false,
        deprecated: None,
    }
}

fn field(name: &str, ty: TypeRef) -> StructField {
    StructField {
        name: name.into(),
        ty,
        doc: None,
    }
}

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.into())
}

/// The fixture: a `bus` module with a `Ticker` interface, a `Bundle` record
/// holding a `Ticker` and a `[Ticker]`, a `Subscriber` callback interface,
/// and free functions covering nullable objects, iterators of objects, an
/// async object result, and a callback-interface parameter.
fn fixture() -> ResolvedApi {
    let ticker = InterfaceDef {
        name: "Ticker".into(),
        doc: Some("A counter the producer owns.".into()),
        deprecated: None,
        constructors: vec![func("new", vec![param("start", TypeRef::I32)], None)],
        methods: vec![func("value", vec![], Some(TypeRef::I32))],
        statics: vec![],
    };
    let bundle = StructDef {
        name: "Bundle".into(),
        doc: None,
        deprecated: None,
        fields: vec![
            field("primary", named("Ticker")),
            field("others", TypeRef::List(Box::new(named("Ticker")))),
            field("label", TypeRef::StringUtf8),
        ],
    };
    let subscriber = CallbackInterfaceDef {
        name: "Subscriber".into(),
        doc: Some("Receives bus events.".into()),
        deprecated: None,
        methods: vec![
            func(
                "on_message",
                vec![
                    param("text", TypeRef::StringUtf8),
                    param("weight", TypeRef::I32),
                    param("bundle", named("Bundle")),
                    param("ticker", named("Ticker")),
                ],
                Some(TypeRef::Bool),
            ),
            func("on_close", vec![param("code", TypeRef::I64)], None),
        ],
    };
    let module = Module {
        name: "bus".into(),
        doc: None,
        functions: vec![
            func(
                "maybe_ticker",
                vec![param("input", TypeRef::Optional(Box::new(named("Ticker"))))],
                Some(TypeRef::Optional(Box::new(named("Ticker")))),
            ),
            func(
                "tickers",
                vec![],
                Some(TypeRef::Iterator(Box::new(named("Ticker")))),
            ),
            func(
                "subscribe",
                vec![param("listener", named("Subscriber"))],
                None,
            ),
            Function {
                r#async: true,
                ..func("spawn", vec![], Some(named("Ticker")))
            },
            func(
                "roundtrip_bundle",
                vec![param("b", named("Bundle"))],
                Some(named("Bundle")),
            ),
        ],
        interfaces: vec![ticker],
        callback_interfaces: vec![subscriber],
        structs: vec![bundle],
        enums: vec![],
        errors: None,
        modules: vec![],
    };
    validate_api(
        Api {
            version: weaveffi_ir::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module],
        },
        None,
    )
    .expect("fixture validates")
}

fn render() -> String {
    let api = fixture();
    let model = BindingModel::build(&api, "weaveffi");
    render_csharp(&model, "Bus", true, "bus.yml", "Bus.cs")
}

fn assert_has(out: &str, needle: &str) {
    assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
}

#[test]
fn interface_wrapper_is_reference_counted_and_disposes_once() {
    let out = render();
    assert_has(&out, "public class Ticker : IDisposable");
    assert_has(
        &out,
        "internal static extern IntPtr weaveffi_bus_Ticker_clone(IntPtr self);",
    );
    assert_has(
        &out,
        "internal static extern void weaveffi_bus_Ticker_destroy(IntPtr self);",
    );
    // A second strong reference for the codec.
    assert_has(
        &out,
        "return NativeMethods.weaveffi_bus_Ticker_clone(Handle);",
    );
    // Exactly one destroy across Dispose and the finalizer.
    assert_has(
        &out,
        "if (System.Threading.Interlocked.Exchange(ref _released, 1) != 0)",
    );
    assert_has(&out, "NativeMethods.weaveffi_bus_Ticker_destroy(h);");
    assert_has(&out, "~Ticker()");
    assert_has(&out, "throw new ObjectDisposedException(nameof(Ticker));");
    // The constructor adopts the returned reference; the method borrows
    // through the checked property so a disposed wrapper throws.
    assert_has(&out, "public Ticker(int start)");
    assert_has(
        &out,
        "var result = NativeMethods.weaveffi_bus_Ticker_new(start, ref err);",
    );
    assert_has(&out, "_handle = result;");
    assert_has(&out, "public int Value()");
    assert_has(
        &out,
        "var result = NativeMethods.weaveffi_bus_Ticker_value(Handle, ref err);",
    );
}

#[test]
fn adopting_constructor_takes_the_handle_marker_not_a_bare_pointer() {
    let out = render();
    // Compiled into the consumer's assembly, an `internal Ticker(IntPtr)`
    // would win overload resolution over the public `Ticker(int start)` for
    // an integer literal (int -> nint is implicit and preferred over long),
    // silently adopting a bogus pointer. The marker struct rules that out.
    assert_has(&out, "internal readonly struct WeaveFFIHandle");
    assert_has(&out, "internal WeaveFFIHandle(IntPtr value)");
    assert_has(&out, "internal Ticker(WeaveFFIHandle handle)");
    assert_has(&out, "_handle = handle.Value;");
    assert!(
        !out.contains("internal Ticker(IntPtr handle)"),
        "adopting constructor must not take a bare IntPtr"
    );
    assert!(
        !out.contains("new Ticker(result)"),
        "every adoption must go through the WeaveFFIHandle marker"
    );
}

#[test]
fn nullable_objects_are_single_nullable_slots() {
    let out = render();
    assert_has(&out, "public static Ticker? MaybeTicker(Ticker? input)");
    assert_has(
        &out,
        "NativeMethods.weaveffi_bus_maybe_ticker(input?.Handle ?? IntPtr.Zero, ref err);",
    );
    assert_has(
        &out,
        "return result == IntPtr.Zero ? null : new Ticker(new WeaveFFIHandle(result));",
    );
    assert_has(
        &out,
        "internal static extern IntPtr weaveffi_bus_maybe_ticker(IntPtr input, ref WeaveFFIError out_err);",
    );
}

#[test]
fn objects_in_records_use_cloned_tokens_and_adopt_on_read() {
    let out = render();
    assert_has(&out, "public sealed class Bundle");
    assert_has(&out, "public Ticker Primary { get; }");
    assert_has(&out, "public Ticker[] Others { get; }");
    // Writing clones through the interface's clone symbol.
    assert_has(&out, "writer.WriteObject(Primary.CloneHandle());");
    assert_has(&out, "writer.WriteObject(item0.CloneHandle());");
    // Reading adopts the token into a fresh wrapper.
    assert_has(
        &out,
        "var fPrimary = new Ticker(new WeaveFFIHandle(reader.ReadObject()));",
    );
    assert_has(
        &out,
        "var fOthersItem = new Ticker(new WeaveFFIHandle(reader.ReadObject()));",
    );
    // The runtime codec helpers exist and reject a null token.
    assert_has(&out, "internal void WriteObject(IntPtr token)");
    assert_has(&out, "internal IntPtr ReadObject()");
    assert_has(&out, "null object token");
}

#[test]
fn iterator_of_objects_adopts_each_element() {
    let out = render();
    assert_has(&out, "public static IEnumerable<Ticker> Tickers()");
    assert_has(
        &out,
        "if (NativeMethods.weaveffi_bus_TickersIterator_next(iter, out var out_item, ref iterErr) == 0)",
    );
    assert_has(
        &out,
        "yield return new Ticker(new WeaveFFIHandle(out_item));",
    );
    assert_has(
        &out,
        "NativeMethods.weaveffi_bus_TickersIterator_destroy(iter);",
    );
}

#[test]
fn async_object_result_is_adopted() {
    let out = render();
    assert_has(&out, "public static async Task<Ticker> Spawn()");
    assert_has(
        &out,
        "tcs.SetResult(new Ticker(new WeaveFFIHandle(result)));",
    );
    assert_has(
        &out,
        "NativeMethods.weaveffi_bus_spawn_async(callback, ctx);",
    );
}

#[test]
fn callback_interface_renders_interface_vtable_and_trampolines() {
    let out = render();
    assert_has(&out, "using System.Runtime.CompilerServices;");
    // The consumer-facing interface.
    assert_has(&out, "public interface ISubscriber");
    assert_has(
        &out,
        "bool OnMessage(string text, int weight, Bundle bundle, Ticker ticker);",
    );
    assert_has(&out, "void OnClose(long code);");

    // One static vtable, method order then `free`, allocated once.
    assert_has(
        &out,
        "internal static unsafe class WeaveFFIVtable_bus_Subscriber",
    );
    assert_has(&out, "[StructLayout(LayoutKind.Sequential)]");
    let layout_start = out.find("private struct Layout").expect("layout struct");
    let layout = &out[layout_start..];
    let on_message = layout.find("public IntPtr on_message;").unwrap();
    let on_close = layout.find("public IntPtr on_close;").unwrap();
    let free = layout.find("public IntPtr free;").unwrap();
    assert!(on_message < on_close && on_close < free);
    assert_has(
        &out,
        "internal static readonly IntPtr Pointer = Allocate();",
    );
    assert_has(
        &out,
        "on_message = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, IntPtr, int, IntPtr, UIntPtr, IntPtr, IntPtr, byte>)&OnMessageTrampoline,",
    );
    assert_has(
        &out,
        "on_close = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, long, IntPtr, void>)&OnCloseTrampoline,",
    );
    assert_has(
        &out,
        "free = (IntPtr)(delegate* unmanaged[Cdecl]<IntPtr, void>)&FreeTrampoline,",
    );
    assert_has(
        &out,
        "var mem = Marshal.AllocHGlobal(Marshal.SizeOf<Layout>());",
    );

    // Trampolines: borrowed string and buffer decoded, object adopted,
    // direct return written through, failures reported via error_set.
    assert_has(
        &out,
        "[UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]",
    );
    assert_has(
        &out,
        "private static byte OnMessageTrampoline(IntPtr ctx, IntPtr text, int weight, IntPtr bundle_ptr, UIntPtr bundle_len, IntPtr ticker, IntPtr out_err)",
    );
    assert_has(&out, "var impl = Target(ctx);");
    assert_has(&out, "var arg2Buf = new byte[(int)bundle_len];");
    assert_has(&out, "var arg2Reader = new WeaveFFIBufferReader(arg2Buf);");
    assert_has(
        &out,
        "return (byte)(impl.OnMessage(Marshal.PtrToStringUTF8(text) ?? \"\", weight, arg2, new Ticker(new WeaveFFIHandle(ticker))) ? 1 : 0);",
    );
    assert_has(
        &out,
        "private static void OnCloseTrampoline(IntPtr ctx, long code, IntPtr out_err)",
    );
    assert_has(&out, "impl.OnClose(code);");
    assert_has(
        &out,
        "NativeMethods.weaveffi_error_set(out_err, WeaveFFIException.ForeignErrorCode, ex.Message);",
    );
    assert_has(&out, "return default;");
    assert_has(&out, "public const int ForeignErrorCode = -4;");
    // No string or buffer is ever freed inside a trampoline.
    let tramp_start = out.find("private static byte OnMessageTrampoline").unwrap();
    let tramp_end = out[tramp_start..].find("FreeTrampoline").unwrap() + tramp_start;
    let tramp = &out[tramp_start..tramp_end];
    assert!(!tramp.contains("weaveffi_free_"), "{tramp}");

    // `free` releases the GCHandle.
    assert_has(&out, "private static void FreeTrampoline(IntPtr ctx)");
    assert_has(&out, "GCHandle.FromIntPtr(ctx).Free();");

    // The error_set runtime symbol is bound.
    assert_has(
        &out,
        "internal static extern void weaveffi_error_set(IntPtr err, int code, [MarshalAs(UnmanagedType.LPUTF8Str)] string message);",
    );
}

#[test]
fn passing_a_callback_interface_registers_a_gchandle_and_the_static_vtable() {
    let out = render();
    assert_has(&out, "public static void Subscribe(ISubscriber listener)");
    assert_has(
        &out,
        "var listenerCtx = GCHandle.ToIntPtr(GCHandle.Alloc(listener));",
    );
    assert_has(
        &out,
        "NativeMethods.weaveffi_bus_subscribe(listenerCtx, WeaveFFIVtable_bus_Subscriber.Pointer, ref err);",
    );
    assert_has(
        &out,
        "internal static extern void weaveffi_bus_subscribe(IntPtr listener_ctx, IntPtr listener_vtable, ref WeaveFFIError out_err);",
    );
}

#[test]
fn legacy_runtime_surface_is_gone() {
    let out = render();
    for legacy in [
        "arena",
        "weaveffi_handle_t",
        "Handle(IntPtr raw)",
        "_listenerRefs",
    ] {
        assert!(!out.contains(legacy), "found legacy `{legacy}` in:\n{out}");
    }
    assert_has(&out, "internal const uint AbiVersion = 2;");
}

#[test]
fn output_is_deterministic() {
    assert_eq!(render(), render());
}

#[test]
fn package_skips_platforms_without_a_nuget_rid() {
    let api = fixture();
    let model = BindingModel::build(&api, "weaveffi");
    let mut binaries = BinarySet::new("bus");
    for p in Platform::ALL {
        binaries.insert(p, format!("/tmp/{}/lib", p.id()));
    }
    let ctx = PackageContext {
        binaries: &binaries,
        input_basename: Some("bus"),
    };
    let files = DotnetGenerator
        .package(
            &api,
            &model,
            &ctx,
            Utf8Path::new("out"),
            &DotnetConfig::default(),
        )
        .expect("dotnet packages");
    let paths: Vec<String> = files.iter().map(|f| f.path.to_string()).collect();
    let native: Vec<&String> = paths.iter().filter(|p| p.contains("/runtimes/")).collect();
    assert_eq!(native.len(), Platform::DESKTOP.len(), "{paths:?}");
    for rid in [
        "osx-arm64",
        "osx-x64",
        "linux-x64",
        "linux-arm64",
        "win-x64",
    ] {
        assert!(
            paths
                .iter()
                .any(|p| p.contains(&format!("/runtimes/{rid}/native/"))),
            "{paths:?}"
        );
    }
    assert!(!paths
        .iter()
        .any(|p| p.contains("android") || p.contains("wasm")));
}
