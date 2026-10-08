//! Rendering tests over a small API exercising every C ABI revision 4 shape
//! the .NET wrapper distinguishes: the contract check, objects, nullable
//! objects, objects in records and lists, composite codecs, iterators of
//! objects, typed errors with fields, a callback interface returning every
//! family (one method `throws`) passed both required and optional, async
//! and cancellable functions, and bytes parameters.

use crate::backend::LanguageBackend;
use crate::package::PackageContext;
use crate::platform::{BinarySet, NativeBinary, Platform};
use camino::Utf8Path;
use weaveffi_model::contract::entries;
use weaveffi_model::ir::Api;
use weaveffi_model::model::Model;
use weaveffi_model::pkg::Identity;
use weaveffi_model::validate::validate;

use crate::targets::dotnet::{DotnetConfig, DotnetGenerator};

/// A `bus` module with an error domain (one code carrying a field), a
/// `Ticker` interface, a `Bundle` record holding objects, a `Subscriber`
/// callback interface, and free functions covering nullable objects, an
/// iterator of objects, async and cancellable calls, required and optional
/// callback parameters, bytes, and a nested composite; plus a nested
/// `bus.deep_inner` module inheriting the domain.
const FIXTURE: &str = r#"
version: "0.11.0"
modules:
  - name: bus
    errors:
      name: BusError
      codes:
        - { name: Closed, code: 1, message: "bus closed" }
        - { name: Full, code: 2, message: "bus full", fields: [{ name: capacity, type: u32 }, { name: topic, type: string }] }
    enums:
      - name: Level
        variants: [{ name: Low, value: 0 }, { name: High, value: 1 }]
    structs:
      - name: Bundle
        fields:
          - { name: primary, type: Ticker }
          - { name: others, type: "[Ticker]" }
          - { name: label, type: string }
    interfaces:
      - name: Ticker
        doc: A counter the producer owns.
        constructors:
          - { name: new, params: [{ name: start, type: i32 }] }
        methods:
          - { name: value, return: i32 }
    callback_interfaces:
      - name: Subscriber
        doc: Receives bus events.
        methods:
          - name: on_message
            params:
              - { name: text, type: string }
              - { name: weight, type: i32 }
              - { name: bundle, type: Bundle }
              - { name: ticker, type: Ticker }
            return: bool
          - { name: on_close, params: [{ name: code, type: i64 }] }
          - { name: level, return: Level }
          - { name: label, return: string, throws: true }
          - { name: payload, return: bytes }
          - { name: latest, return: "Bundle?" }
          - { name: favorite, return: Ticker }
          - { name: spare, return: "Ticker?" }
    functions:
      - { name: maybe_ticker, params: [{ name: input, type: "Ticker?" }], return: "Ticker?" }
      - { name: tickers, return: "iter<Ticker>" }
      - { name: subscribe, params: [{ name: listener, type: Subscriber }] }
      - { name: maybe_subscribe, params: [{ name: listener, type: "Subscriber?" }], throws: true }
      - { name: spawn, return: Ticker, async: true }
      - { name: wait, params: [{ name: ms, type: i64 }], return: i64, async: true, cancellable: true }
      - { name: roundtrip_bundle, params: [{ name: b, type: Bundle }], return: Bundle }
      - { name: bundle, return: Bundle }
      - { name: digest, params: [{ name: data, type: bytes }, { name: label, type: string }], return: string }
      - { name: tally, params: [{ name: counts, type: "{string:[i32?]}" }], return: "[Bundle]", throws: true }
    modules:
      - name: deep_inner
        functions:
          - { name: drain, return: u32, throws: true }
"#;

fn fixture() -> Model {
    let api: Api = serde_yaml::from_str(FIXTURE).expect("fixture parses");
    validate(&api, &Identity::named("bus-lib"), None).expect("fixture validates")
}

/// Every generated file's text, keyed by file name.
fn render_files(config: &DotnetConfig) -> Vec<(String, String)> {
    let model = fixture();
    DotnetGenerator
        .files(&model, Utf8Path::new("out"), config)
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

fn runtime() -> String {
    file(&render_files(&DotnetConfig::default()), "Runtime.cs")
}

#[track_caller]
fn assert_has(out: &str, needle: &str) {
    assert!(out.contains(needle), "missing `{needle}` in:\n{out}");
}

/// The text of the C# member starting at `start`, up to the next blank
/// line at its indentation.
fn member<'a>(out: &'a str, start: &str) -> &'a str {
    let at = out
        .find(start)
        .unwrap_or_else(|| panic!("no `{start}` in:\n{out}"));
    let rest = &out[at..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    &rest[..end]
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
    assert_has(&runtime, "internal const uint AbiVersion = 4;");
    assert_has(&runtime, "public class NativeException : Exception");
    assert_has(
        &runtime,
        "public sealed class NativeBugException : InvalidOperationException",
    );
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
        name: Some("Acme.Bus".into()),
    };
    let files = render_files(&config);
    assert_has(&file(&files, "Acme.Bus.cs"), "namespace Acme.Bus;");
    assert_has(
        &file(&files, "Acme.Bus.csproj"),
        "<PackageId>Acme.Bus</PackageId>",
    );
}

#[test]
fn the_runtime_surface_is_revision_4() {
    let runtime = runtime();
    assert_has(&runtime, "EntryPoint = \"bus_lib_alloc\"");
    assert_has(&runtime, "internal static partial byte* Alloc(nuint len);");
    assert_has(&runtime, "EntryPoint = \"bus_lib_error_set_payload\"");
    assert!(!runtime.contains("_dealloc"), "{runtime}");
    assert!(!runtime.to_lowercase().contains("checksum"), "{runtime}");
}

#[test]
fn load_checks_the_abi_revision_and_every_root_contract() {
    let model = fixture();
    let out = render();
    assert_has(&out, "static partial void VerifyContracts()");
    assert_has(
        &out,
        "VerifyContract(\"bus_lib_bus_contract\", &bus_lib_bus_contract, new (ulong, ulong, string)[]",
    );
    assert_has(
        &out,
        "internal static partial FfiContractEntry* bus_lib_bus_contract(nuint* out_len);",
    );
    let root = model.roots().next().unwrap();
    let expected = entries(&model, root);
    assert!(expected.iter().any(|e| e.path == "bus.deep_inner.drain"));
    for e in &expected {
        assert_has(
            &out,
            &format!("(0x{:016x}UL, 0x{:016x}UL, \"{}\"),", e.id, e.hash, e.path),
        );
    }
    let runtime = runtime();
    assert_has(&runtime, "VerifyContracts();");
    assert_has(&runtime, "is missing from the library");
    assert_has(&runtime, "changed since these bindings were generated");
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
    assert_has(&out, "FfiCodecs.WriteListOfTicker(writer, Others);");
    assert_has(&out, "writer.WriteObject(item.CloneHandle());");
    assert_has(&out, "global::BusLib.Ticker.Adopt(reader.ReadObject()),");
    assert_has(
        &out,
        "list.Add(global::BusLib.Ticker.Adopt(reader.ReadObject()));",
    );
}

#[test]
fn composites_get_one_codec_pair_each() {
    let out = render();
    for name in [
        "ListOfTicker",
        "ListOfBundle",
        "ListOfOptionalOfI32",
        "MapOfStringToListOfOptionalOfI32",
        "OptionalOfBundle",
    ] {
        assert_eq!(
            out.matches(&format!("internal static void Write{name}("))
                .count(),
            1,
            "{name}"
        );
        assert_eq!(
            out.matches(&format!(" Read{name}(FfiBufferReader reader)"))
                .count(),
            1,
            "{name}"
        );
    }
    // The call site writes through the pair instead of inlining a loop.
    let tally = member(&out, "public static Bundle[] Tally(");
    assert_has(
        tally,
        "FfiCodecs.WriteMapOfStringToListOfOptionalOfI32(countsWriter, counts);",
    );
    assert!(!tally.contains("foreach"), "{tally}");
    // A repeated map key is malformed.
    assert_has(
        &out,
        "throw FfiBufferReader.Malformed(\"repeated map key\");",
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
        "return FfiCodecs.Decode(Ffi.TakeBuffer(ffiResult, ffiOutLen), static r => global::BusLib.Bundle.ReadFrom(r));",
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
fn errors_follow_the_throws_flag() {
    let out = render();
    // A domain is an abstract class with one nested class per code; fields
    // are typed properties and the payload decodes into them.
    assert_has(&out, "public abstract class BusException : NativeException");
    assert_has(&out, "public sealed class Full : BusException");
    assert_has(&out, "public const int ErrorCode = 2;");
    assert_has(&out, "public uint Capacity { get; }");
    assert_has(&out, "public string Topic { get; }");
    assert_has(
        &out,
        "public Full(uint capacity, string topic, string? message = null) : base(ErrorCode, message ?? \"bus full\")",
    );
    assert_has(&out, "case Full.ErrorCode:");
    assert_has(&out, "return new Closed(given);");
    // A throwing call maps through its domain, a non-throwing one traps.
    let tally = member(&out, "public static Bundle[] Tally(");
    assert_has(
        tally,
        "throw Ffi.TakeError(&ffiErr, global::BusLib.BusException.FromError);",
    );
    let digest = member(&out, "public static string Digest(ReadOnlySpan<byte>");
    assert_has(
        digest,
        "throw Ffi.TakeError(&ffiErr, global::BusLib.NativeBugException.FromError);",
    );
    // A nested module inherits the domain, and the doc names the
    // declaring module's dotted path.
    let drain = member(&out, "public static uint Drain()");
    assert_has(drain, "global::BusLib.BusException.FromError");
    assert_has(&out, "declared by module <c>bus</c>.");
    let runtime = runtime();
    assert_has(&runtime, "native call failed with code {code}: {message}");
    assert_has(&runtime, "return new OperationCanceledException(message);");
}

#[test]
fn the_domain_doc_names_the_dotted_module_path() {
    let yaml = r#"
version: "0.11.0"
modules:
  - name: edge_cases
    errors:
      name: EdgeError
      codes: [{ name: Busy, code: 7, message: "busy" }]
    functions:
      - { name: poke, throws: true }
"#;
    let api: Api = serde_yaml::from_str(yaml).unwrap();
    let model = validate(&api, &Identity::named("edge"), None).unwrap();
    let out = super::render_csharp(
        &model,
        "Edge",
        ("NativeException", "NativeBugException"),
        "Edge.cs",
    );
    assert_has(&out, "declared by module <c>edge_cases</c>.");
    assert!(!out.contains("edge.cases"), "{out}");
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
    let runtime = runtime();
    assert_has(&runtime, "_tcs.TrySetCanceled(_cancellation);");
    assert_has(&runtime, "NativeMethods.CancelTokenCancel(_token);");
}

#[test]
fn callback_vtables_start_with_the_header() {
    let out = render();
    assert_has(
        &out,
        "internal static unsafe class FfiVtable_bus_Subscriber",
    );
    let layout = member(&out, "private struct Layout");
    let header = [
        "public uint Size;",
        "public uint Flags;",
        "public delegate* unmanaged[Cdecl]<IntPtr, void> Free;",
        "public delegate* unmanaged[Cdecl]<IntPtr, byte*, nuint, int, byte*, nuint, IntPtr, FfiError*, byte> on_message;",
    ];
    let mut at = 0;
    for field in header {
        let found = layout[at..]
            .find(field)
            .unwrap_or_else(|| panic!("`{field}` out of order in:\n{layout}"));
        at += found + field.len();
    }
    assert_has(&out, "vtable->Size = (uint)sizeof(Layout);");
    assert_has(&out, "vtable->Flags = 0;");
    assert_has(&out, "vtable->Free = &FreeTrampoline;");
    assert_has(&out, "vtable->on_message = &OnMessageTrampoline;");
}

#[test]
fn callback_methods_receive_borrowed_arguments_and_adopt_objects() {
    let out = render();
    assert_has(&out, "public interface ISubscriber");
    assert_has(
        &out,
        "bool OnMessage(string text, int weight, Bundle bundle, Ticker ticker);",
    );
    let trampoline = member(&out, "private static byte OnMessageTrampoline(");
    assert_has(
        trampoline,
        "private static byte OnMessageTrampoline(IntPtr ctx, byte* text_ptr, nuint text_len, int weight, byte* bundle_ptr, nuint bundle_len, IntPtr ticker, FfiError* out_err)",
    );
    assert_has(trampoline, "Ffi.ReadString(text_ptr, text_len)");
    assert_has(
        trampoline,
        "FfiCodecs.Decode(Ffi.ReadBuffer(bundle_ptr, bundle_len), static r => global::BusLib.Bundle.ReadFrom(r))",
    );
    assert_has(trampoline, "global::BusLib.Ticker.Adopt(ticker)");
    assert_has(trampoline, "Ffi.SetForeignError(out_err, e);");
    // Arguments are borrowed: nothing is freed inside a trampoline.
    assert!(!trampoline.contains("Take"), "{trampoline}");
}

#[test]
fn callback_methods_return_every_family() {
    let out = render();
    assert_has(&out, "Level Level();");
    assert_has(&out, "string Label();");
    assert_has(&out, "byte[] Payload();");
    assert_has(&out, "Bundle? Latest();");
    assert_has(&out, "Ticker Favorite();");
    assert_has(&out, "Ticker? Spare();");
    // Direct values by value.
    assert_has(
        member(&out, "private static int LevelTrampoline("),
        "return (int)Ffi.Target<ISubscriber>(ctx).Level();",
    );
    // Strings, bytes, and buffers as `{prefix}_alloc` runs in the out slots.
    let label = member(&out, "private static void LabelTrampoline(");
    assert_has(
        label,
        "LabelTrampoline(IntPtr ctx, byte** out_ptr, nuint* out_len, FfiError* out_err)",
    );
    assert_has(
        label,
        "Ffi.ReturnString(Ffi.Target<ISubscriber>(ctx).Label(), out_ptr, out_len);",
    );
    assert_has(
        member(&out, "private static void PayloadTrampoline("),
        "Ffi.ReturnBytes(Ffi.Target<ISubscriber>(ctx).Payload(), out_ptr, out_len);",
    );
    let latest = member(&out, "private static void LatestTrampoline(");
    assert_has(
        latest,
        "FfiCodecs.WriteOptionalOfBundle(ffiWriter, ffiResult);",
    );
    assert_has(latest, "Ffi.ReturnBuffer(ffiWriter, out_ptr, out_len);");
    // Objects as a fresh reference; a null passes through for the producer
    // to judge.
    assert_has(
        member(&out, "private static IntPtr FavoriteTrampoline("),
        "return Ffi.Target<ISubscriber>(ctx).Favorite()?.CloneHandle() ?? IntPtr.Zero;",
    );
    assert_has(
        member(&out, "private static IntPtr SpareTrampoline("),
        "return Ffi.Target<ISubscriber>(ctx).Spare()?.CloneHandle() ?? IntPtr.Zero;",
    );
    let runtime = runtime();
    assert_has(
        &runtime,
        "var run = NativeMethods.Alloc((nuint)bytes.Length);",
    );
}

#[test]
fn throwing_callback_methods_report_the_domain_error() {
    let out = render();
    let label = member(&out, "private static void LabelTrampoline(");
    assert_has(label, "catch (global::BusLib.BusException e)");
    assert_has(
        label,
        "Ffi.SetDomainError(out_err, e.Code, e, e.WritePayload);",
    );
    assert_has(label, "catch (Exception e)");
    // Only `throws` methods report the domain; the rest are foreign errors.
    let payload = member(&out, "private static void PayloadTrampoline(");
    assert!(!payload.contains("BusException"), "{payload}");
    // Each code with fields encodes them as the payload.
    assert_has(
        &out,
        "internal override void WritePayload(FfiBufferWriter writer)\n        {\n            writer.WriteU32(Capacity);\n            writer.WriteString(Topic);",
    );
    let runtime = runtime();
    assert_has(
        &runtime,
        "NativeMethods.ErrorSetPayload(err, ptr, (nuint)payload.Length);",
    );
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
fn an_absent_optional_callback_passes_a_null_vtable() {
    let out = render();
    assert_has(
        &out,
        "public static void MaybeSubscribe(ISubscriber? listener)",
    );
    assert_has(
        &out,
        "NativeMethods.bus_lib_bus_maybe_subscribe(listenerCtx, listener == null ? IntPtr.Zero : FfiVtable_bus_Subscriber.Pointer, &ffiErr);",
    );
    let runtime = runtime();
    assert_has(
        &runtime,
        "return implementation == null ? IntPtr.Zero : GCHandle.ToIntPtr(GCHandle.Alloc(implementation));",
    );
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
    let model = fixture();
    let mut binaries = BinarySet::new("bus_lib");
    for p in Platform::ALL {
        binaries.insert(NativeBinary::new(p, format!("/tmp/{}/lib", p.id())));
    }
    let ctx = PackageContext::new(&binaries);
    let artifacts = DotnetGenerator
        .package(&model, &ctx, &DotnetConfig::default())
        .expect("dotnet packages");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].path, "dotnet/BusLib");
    assert!(artifacts[0].file("BusLib.csproj").is_some());
    let paths: Vec<String> = artifacts[0]
        .files
        .iter()
        .map(|f| f.path.to_string())
        .collect();
    let native: Vec<&String> = paths
        .iter()
        .filter(|p| p.starts_with("runtimes/"))
        .collect();
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
                .any(|p| p.starts_with(&format!("runtimes/{rid}/native/"))),
            "{paths:?}"
        );
    }
    assert!(!paths
        .iter()
        .any(|p| p.contains("android") || p.contains("wasm")));
}
