//! Rendering tests over a small hand-built API exercising every ABI
//! revision 3 shape: objects, nullable objects, objects in records and
//! lists, iterators of objects, a callback interface, async and cancellable
//! functions, and bytes parameters.

use crate::backend::LanguageBackend;
use crate::package::PackageContext;
use crate::platform::{BinarySet, Platform};
use camino::Utf8Path;
use weaveffi_model::ir::{
    Api, CallbackInterfaceDef, Function, InterfaceDef, Module, Param, StructDef, StructField,
    TypeRef,
};
use weaveffi_model::model::BindingModel;
use weaveffi_model::pkg::Identity;
use weaveffi_model::resolved::ResolvedApi;
use weaveffi_model::validate::validate_api;

use crate::targets::dotnet::{DotnetConfig, DotnetGenerator};

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

/// A `bus` module with a `Ticker` interface, a `Bundle` record holding
/// objects, a `Subscriber` callback interface, and free functions covering
/// nullable objects, an iterator of objects, async and cancellable calls, a
/// callback-interface parameter, and bytes.
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
            Function {
                r#async: true,
                cancellable: true,
                ..func("wait", vec![param("ms", TypeRef::I64)], Some(TypeRef::I64))
            },
            func(
                "roundtrip_bundle",
                vec![param("b", named("Bundle"))],
                Some(named("Bundle")),
            ),
            // A method named like the type it returns.
            func("bundle", vec![], Some(named("Bundle"))),
            func(
                "digest",
                vec![
                    param("data", TypeRef::Bytes),
                    param("label", TypeRef::StringUtf8),
                ],
                Some(TypeRef::StringUtf8),
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
            version: weaveffi_model::ir::CURRENT_SCHEMA_VERSION.into(),
            modules: vec![module],
        },
        None,
    )
    .expect("fixture validates")
    .with_identity(Identity::named("bus-lib"))
}

/// Every generated file's text, keyed by file name.
fn render_files(config: &DotnetConfig) -> Vec<(String, String)> {
    let api = fixture();
    let model = BindingModel::build(&api);
    DotnetGenerator
        .files(&api, &model, Utf8Path::new("out"), config)
        .into_iter()
        .map(|f| (f.path.file_name().unwrap().to_string(), f.contents))
        .collect()
}

fn file(files: &[(String, String)], name: &str) -> String {
    files
        .iter()
        .find(|(n, _)| n == name)
        .unwrap_or_else(|| {
            panic!(
                "no {name} in {:?}",
                files.iter().map(|f| &f.0).collect::<Vec<_>>()
            )
        })
        .1
        .clone()
}

fn render() -> String {
    file(&render_files(&DotnetConfig::default()), "BusLib.cs")
}

fn assert_has(out: &str, needle: &str) {
    assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
}

#[test]
fn names_come_from_the_identity() {
    let files = render_files(&DotnetConfig::default());
    let names: Vec<&str> = files.iter().map(|f| f.0.as_str()).collect();
    assert_eq!(
        names,
        ["BusLib.cs", "Runtime.cs", "BusLib.csproj", "README.md"]
    );
    let runtime = file(&files, "Runtime.cs");
    assert_has(&runtime, "namespace BusLib;");
    assert_has(&runtime, "internal const string LibName = \"bus_lib\";");
    assert_has(
        &runtime,
        "internal const string LibraryEnvVar = \"BUS_LIB_LIBRARY\";",
    );
    assert_has(&runtime, "EntryPoint = \"bus_lib_abi_version\"");
    assert_has(&runtime, "internal const uint AbiVersion = 3;");
    assert_has(&runtime, "public class NativeException : Exception");
    assert_has(&runtime, "public const int CancelledErrorCode = -5;");
    assert!(
        !runtime.contains("{{"),
        "unreplaced placeholder:\n{runtime}"
    );
    let csproj = file(&files, "BusLib.csproj");
    assert_has(&csproj, "<AssemblyName>BusLib</AssemblyName>");
    assert_has(&csproj, "<PackageId>BusLib</PackageId>");
    for (name, text) in &files {
        let body: String = text
            .lines()
            .filter(|l| !l.contains("Generated by WeaveFFI") && !l.contains("weaveffi generate"))
            .collect();
        assert!(
            !body.to_lowercase().contains("weaveffi"),
            "{name} is branded:\n{text}"
        );
    }
}

#[test]
fn configured_namespace_wins() {
    let config = DotnetConfig {
        namespace: Some("Acme.Bus".into()),
        ..DotnetConfig::default()
    };
    let files = render_files(&config);
    assert_has(&file(&files, "Acme.Bus.cs"), "namespace Acme.Bus;");
    assert_has(
        &file(&files, "Acme.Bus.csproj"),
        "<PackageId>Acme.Bus</PackageId>",
    );
}

#[test]
fn load_time_check_covers_every_root_checksum() {
    let out = render();
    assert_has(&out, "static partial void VerifyChecksums()");
    assert_has(&out, "VerifyChecksum(\"bus\", &bus_lib_bus_checksum, 0x");
    assert_has(
        &out,
        "internal static partial ulong bus_lib_bus_checksum();",
    );
}

#[test]
fn interfaces_wrap_a_safe_handle() {
    let out = render();
    assert_has(
        &out,
        "public sealed unsafe class Ticker : IDisposable, IEquatable<Ticker>",
    );
    assert_has(&out, "internal sealed class NativeHandle : SafeHandle");
    assert_has(&out, "NativeMethods.bus_lib_bus_Ticker_destroy(handle);");
    assert!(
        !out.contains("~Ticker()"),
        "the SafeHandle is the finalizer"
    );
    // Every import borrows through the handle, never a raw pointer.
    assert_has(
        &out,
        "internal static partial int bus_lib_bus_Ticker_value(Ticker.NativeHandle self, FfiError* out_err);",
    );
    assert_has(
        &out,
        "internal static partial IntPtr bus_lib_bus_Ticker_clone(Ticker.NativeHandle self);",
    );
    assert_has(
        &out,
        "var ffiResult = NativeMethods.bus_lib_bus_Ticker_value(Handle, &ffiErr);",
    );
    assert_has(&out, "public Ticker(int start)");
    assert_has(&out, "Handle = new NativeHandle(ffiResult);");
    assert_has(
        &out,
        "return NativeMethods.bus_lib_bus_Ticker_clone(Handle);",
    );
}

#[test]
fn nullable_objects_pass_the_null_handle() {
    let out = render();
    assert_has(&out, "public static Ticker? MaybeTicker(Ticker? input)");
    assert_has(
        &out,
        "NativeMethods.bus_lib_bus_maybe_ticker(input?.Handle ?? global::BusLib.Ticker.NativeHandle.Null, &ffiErr);",
    );
    assert_has(
        &out,
        "return ffiResult == IntPtr.Zero ? null : global::BusLib.Ticker.Adopt(ffiResult);",
    );
}

#[test]
fn objects_in_records_use_cloned_tokens_and_adopt_on_read() {
    let out = render();
    assert_has(&out, "public sealed class Bundle");
    assert_has(&out, "writer.WriteObject(Primary.CloneHandle());");
    assert_has(&out, "writer.WriteObject(item0.CloneHandle());");
    assert_has(
        &out,
        "var fPrimary = global::BusLib.Ticker.Adopt(reader.ReadObject());",
    );
    assert_has(
        &out,
        "var fOthersItem = global::BusLib.Ticker.Adopt(reader.ReadObject());",
    );
}

#[test]
fn type_references_survive_a_member_of_the_same_name() {
    let out = render();
    // Inside `Bus`, the method `Bundle()` would shadow the type `Bundle` in
    // an expression, so static accesses go through the global namespace.
    assert_has(&out, "public static Bundle Bundle()");
    assert_has(
        &out,
        "var ffiDecoded = global::BusLib.Bundle.ReadFrom(ffiDecodedReader);",
    );
}

#[test]
fn strings_and_bytes_cross_as_pointer_and_length() {
    let out = render();
    assert_has(
        &out,
        "internal static partial byte* bus_lib_bus_digest(byte* data_ptr, nuint data_len, byte* label_ptr, nuint label_len, nuint* out_len, FfiError* out_err);",
    );
    assert_has(
        &out,
        "public static string Digest(ReadOnlySpan<byte> data, string label)",
    );
    assert_has(&out, "var labelBytes = Ffi.Utf8(label);");
    assert_has(&out, "fixed (byte* dataPtr = data, labelPtr = labelBytes)");
    assert_has(&out, "return Ffi.TakeString(ffiResult, ffiOutLen);");
    // The byte[] overload forwards to the span one.
    assert_has(
        &out,
        "public static string Digest(byte[] data, string label) => Digest((ReadOnlySpan<byte>)data, label);",
    );
}

#[test]
fn iterators_stream_through_a_safe_handle() {
    let out = render();
    assert_has(&out, "public static IEnumerable<Ticker> Tickers()");
    assert_has(
        &out,
        "return new FfiSequence<Ticker>(new FfiIteratorHandle(ffiResult, &NativeMethods.bus_lib_bus_TickersIterator_destroy), &NextTickers);",
    );
    assert_has(
        &out,
        "private static bool NextTickers(FfiIteratorHandle iter, out Ticker item)",
    );
    assert_has(
        &out,
        "internal static partial int bus_lib_bus_TickersIterator_next(FfiIteratorHandle iter, IntPtr* out_item, FfiError* out_err);",
    );
    assert_has(&out, "item = global::BusLib.Ticker.Adopt(ffiItem);");
}

#[test]
fn async_calls_complete_through_an_unmanaged_callback() {
    let out = render();
    assert_has(&out, "public static Task<Ticker> Spawn()");
    assert_has(&out, "var ffiCall = new FfiCall<Ticker>();");
    assert_has(
        &out,
        "NativeMethods.bus_lib_bus_spawn(&CompleteSpawn, ffiCall.Context);",
    );
    assert_has(
        &out,
        "internal static partial void bus_lib_bus_spawn(delegate* unmanaged[Cdecl]<IntPtr, FfiError*, IntPtr, void> callback, IntPtr context);",
    );
    assert_has(
        &out,
        "private static void CompleteSpawn(IntPtr context, FfiError* err, IntPtr result)",
    );
    assert_has(
        &out,
        "ffiCall.SetResult(global::BusLib.Ticker.Adopt(result));",
    );
    assert_has(&out, "ffiCall.Abandon();");
}

#[test]
fn cancellable_calls_take_a_cancellation_token() {
    let out = render();
    assert_has(
        &out,
        "public static Task<long> Wait(long ms, CancellationToken cancellationToken = default)",
    );
    assert_has(&out, "return Task.FromCanceled<long>(cancellationToken);");
    assert_has(&out, "var ffiCall = new FfiCall<long>(cancellationToken);");
    assert_has(
        &out,
        "NativeMethods.bus_lib_bus_wait(ms, ffiCall.CancelToken(), &CompleteWait, ffiCall.Context);",
    );
    let runtime = file(&render_files(&DotnetConfig::default()), "Runtime.cs");
    assert_has(&runtime, "_tcs.TrySetCanceled(_cancellation);");
    assert_has(&runtime, "NativeMethods.CancelTokenCancel(_token);");
}

#[test]
fn callback_interfaces_render_a_static_vtable_of_trampolines() {
    let out = render();
    assert_has(&out, "public interface ISubscriber");
    assert_has(
        &out,
        "bool OnMessage(string text, int weight, Bundle bundle, Ticker ticker);",
    );
    assert_has(
        &out,
        "internal static unsafe class FfiVtable_bus_Subscriber",
    );
    assert_has(
        &out,
        "public delegate* unmanaged[Cdecl]<IntPtr, byte*, nuint, int, byte*, nuint, IntPtr, FfiError*, byte> on_message;",
    );
    assert_has(&out, "vtable->on_message = &OnMessageTrampoline;");
    assert_has(&out, "vtable->free = &FreeTrampoline;");
    assert_has(
        &out,
        "private static byte OnMessageTrampoline(IntPtr ctx, byte* text_ptr, nuint text_len, int weight, byte* bundle_ptr, nuint bundle_len, IntPtr ticker, FfiError* out_err)",
    );
    assert_has(
        &out,
        "return (byte)(Ffi.Target<ISubscriber>(ctx).OnMessage(Ffi.ReadString(text_ptr, text_len), weight, arg2, global::BusLib.Ticker.Adopt(ticker)) ? 1 : 0);",
    );
    assert_has(&out, "Ffi.SetForeignError(out_err, e);");
    // Arguments are borrowed: nothing is freed inside a trampoline.
    let start = out.find("OnMessageTrampoline(IntPtr").unwrap();
    let end = out[start..].find("FreeTrampoline(IntPtr").unwrap() + start;
    assert!(!out[start..end].contains("Take"), "{}", &out[start..end]);
}

#[test]
fn passing_a_callback_interface_registers_it_after_encoding() {
    let out = render();
    assert_has(&out, "public static void Subscribe(ISubscriber listener)");
    assert_has(&out, "var listenerCtx = Ffi.Register(listener);");
    assert_has(
        &out,
        "NativeMethods.bus_lib_bus_subscribe(listenerCtx, FfiVtable_bus_Subscriber.Pointer, &ffiErr);",
    );
    // A call that never reaches the producer releases the registration.
    assert_has(&out, "Ffi.Unregister(listenerCtx);");
}

#[test]
fn output_is_deterministic() {
    assert_eq!(
        render_files(&DotnetConfig::default()),
        render_files(&DotnetConfig::default())
    );
}

#[test]
fn package_bundles_desktop_runtimes_only() {
    let api = fixture();
    let model = BindingModel::build(&api);
    let mut binaries = BinarySet::new("bus_lib");
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
