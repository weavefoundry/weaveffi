# HTTP Fetch sample

A networking-backed WeaveFFI sample that exposes a single async, cancellable
`fetch` entry point built on top of [`reqwest`](https://crates.io/crates/reqwest).
It doubles as the reference for "what does a real async + cancel + struct-return
function look like across every target language" — without any mocking or
in-memory toy storage.

## What this sample demonstrates

- **Async + cancellable C ABI function** — `fetch` is declared `async: true`
  and `cancellable: true` in the IDL, so the generated C entry point threads
  a `weaveffi_cancel_token*` through to the worker alongside the callback +
  context pointer. See [`src/lib.rs`](src/lib.rs)
  (`weaveffi_http_fetch_async`).
- **Shared multi-thread Tokio runtime** — a single `OnceLock<Runtime>` drives
  every request future so foreign callers never have to manage the runtime
  themselves.
- **Struct return with heterogeneous fields** — `HttpResponse` carries `i32`,
  `bytes`, and a `{string:string}` headers map, exercising the
  primitive, buffer, and map accessor paths the generators emit.
- **Cooperative cancellation racing real I/O** — the request future is raced
  against a short-tick cancel-poll future via `tokio::select!`, so foreign
  callers get a prompt `ERR_CODE_CANCELLED` instead of waiting for the
  socket timeout.
- **Overridable `User-Agent`** — the runtime reads
  `WEAVEFFI_HTTP_USER_AGENT` from the environment on every call; see
  [Overriding the User-Agent](#overriding-the-user-agent) below.

## Security notes — rustls vs. system OpenSSL

This sample pins reqwest to **rustls** and disables its default feature set:

```toml
reqwest = { version = "0.12", features = ["rustls-tls", "json"], default-features = false }
```

That choice has real security and portability implications that any
downstream consumer should be aware of before reusing the sample as a
starting point:

- **No libssl / libcrypto dependency.** The TLS stack is pure Rust
  (`rustls` + `ring`/`aws-lc`), so there is no link against the host's
  OpenSSL. CVEs against `openssl` on the host system (e.g. distro
  packaging bugs, stale `libssl.so.1.1`) do **not** apply to this cdylib,
  and cross-compiling to musl / iOS / Android does not need a vendored
  OpenSSL toolchain.
- **Trust anchors are embedded at build time.** `rustls-tls` pulls in
  [`webpki-roots`](https://crates.io/crates/webpki-roots), which ships a
  snapshot of Mozilla's CA bundle. The cdylib therefore **ignores the
  OS keychain / system trust store** — a custom root CA installed in
  macOS Keychain or Windows certmgr will not be trusted by this sample.
  Consumers who need OS-integrated trust (MDM-pushed roots, corporate
  proxies, user-installed CAs) should swap to the
  `rustls-tls-native-roots` or `native-tls` feature instead and
  accept a runtime dependency on the platform TLS stack.
- **TLS 1.2+ only, modern cipher suites.** rustls does not negotiate
  SSLv3 / TLS 1.0 / TLS 1.1 and has no "legacy-compat" knob. This is
  a hard compatibility floor — talking to very old middleboxes will
  fail at the TLS handshake layer.
- **Cert revocation is not checked by default.** Neither rustls nor
  `webpki-roots` performs OCSP / CRL validation; revocation enforcement
  must be layered on top if it matters for your threat model.
- **No proxy auto-configuration.** With `default-features = false` the
  sample does **not** honour `HTTPS_PROXY` / `HTTP_PROXY` environment
  variables that reqwest would otherwise pick up. If you need proxy
  support, add `reqwest` feature `socks` or configure a proxy via
  `reqwest::Proxy::all(...)` inside `weaveffi_http_fetch_async`.
- **No cookie jar, no redirect policy override.** reqwest's defaults
  apply (follow up to 10 redirects, no cookies persisted across calls).
  Anything stricter must be configured on the `Client::builder()` in
  `src/lib.rs`.

If you need any of the above (OS trust store, OpenSSL compatibility, proxy
env, cookie jar) you should fork this sample rather than toggle features at
the consumer level — the cdylib's C ABI does not currently surface those
knobs.

## Overriding the User-Agent

By default every outgoing request advertises itself as
`weaveffi-http-fetch/<crate-version>`. Consumers can override this at
runtime without rebuilding the cdylib by setting the
`WEAVEFFI_HTTP_USER_AGENT` environment variable **before** the first
`fetch` call:

```bash
export WEAVEFFI_HTTP_USER_AGENT="MyApp/1.2.3 (+https://example.com)"
```

The variable is read on every call, so late-binding updates take effect on
the next request. An unset or empty value falls back to the compiled-in
default. Because the Tokio runtime is process-wide, setting and unsetting
the variable affects every concurrent `fetch` issued from the process —
this sample does not expose a per-request User-Agent on the C ABI.

Setting the variable from each consumer language:

```swift
// Swift — set before the first Http.http_fetch call
import Darwin
setenv("WEAVEFFI_HTTP_USER_AGENT", "MyApp/1.2.3", 1)
```

```kotlin
// Kotlin (Android / JVM) — set in the parent process (Gradle, shell,
// or launcher), because JVM's System.setProperty does not mutate the
// POSIX environment that Rust's std::env::var reads. If you control the
// app's entry point you can also call setenv(3) via JNI before
// System.loadLibrary("http_fetch").
ProcessBuilder("myapp")
    .apply { environment()["WEAVEFFI_HTTP_USER_AGENT"] = "MyApp/1.2.3" }
    .start()
```

```python
# Python — must be set before the cdylib is first loaded by ctypes.
import os
os.environ["WEAVEFFI_HTTP_USER_AGENT"] = "MyApp/1.2.3"

import weaveffi  # loads the cdylib; subsequent fetch() calls pick up the UA
```

## IDL highlights

From [`http_fetch.yml`](http_fetch.yml):

```yaml
modules:
  - name: http
    enums:
      - name: HttpMethod
        variants:
          - { name: Get,    value: 0 }
          - { name: Post,   value: 1 }
          - { name: Put,    value: 2 }
          - { name: Delete, value: 3 }
    structs:
      - name: HttpResponse
        fields:
          - { name: status,  type: i32 }
          - { name: body,    type: bytes }
          - { name: headers, type: "{string:string}" }
    functions:
      - name: fetch
        params:
          - { name: url,        type: string }
          - { name: method,     type: HttpMethod }
          - { name: body,       type: "bytes?" }
          - { name: timeout_ms, type: i32 }
        return: HttpResponse
        async: true             # ← lifts to _async C entry point
        cancellable: true       # ← threads a weaveffi_cancel_token*
```

Key IDL features exercised: `async: true`, `cancellable: true`,
`type: bytes` / `"bytes?"`, a `{string:string}` map field, and an
enum-typed parameter.

## Generate bindings

Run the following from the repo root. Omit `--target` to generate bindings
for **all** supported targets.

```bash
# All targets
cargo run -p weaveffi-cli -- generate samples/http-fetch/http_fetch.yml -o generated

# A comma-separated subset
cargo run -p weaveffi-cli -- generate samples/http-fetch/http_fetch.yml \
    -o generated --target swift,android,python
```

Supported `--target` values: `c`, `cpp`, `swift`, `android`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`.

## Swift consumer code

The Swift generator emits a SwiftPM package (`generated/swift/`) with a
C module map that links against the cdylib. The async + cancellable
`fetch` becomes an `async throws` method on a `public enum Http`,
backed by `CheckedContinuation` and
`withTaskCancellationHandler`:

```swift
import Foundation
import WeaveFFI

func demo() async {
    setenv("WEAVEFFI_HTTP_USER_AGENT", "WeaveDemo/1.0", 1)

    do {
        let resp = try await Http.http_fetch(
            "https://httpbin.org/get",
            .get,                   // HttpMethod.get
            nil,                    // no body
            10_000                  // 10s timeout
        )
        print("status:", resp.status)
        if let text = String(data: Data(resp.body), encoding: .utf8) {
            print("body:", text)
        }
        print("content-type:", resp.headers["content-type"] ?? "-")
    } catch {
        print("fetch failed:", error)
    }

    // Cancellation: cancelling the enclosing task flips the C cancel
    // token inside the Rust worker, which returns ERR_CODE_CANCELLED.
    let task = Task {
        try await Http.http_fetch("https://httpbin.org/delay/10", .get, nil, 30_000)
    }
    task.cancel()
    _ = try? await task.value
}
```

`HttpResponse` is a `public class` that owns an `OpaquePointer` and
calls `weaveffi_http_HttpResponse_destroy` in `deinit`, so the response
cleans up automatically when it goes out of scope.

## Kotlin (Android) consumer code

The Android generator emits a Kotlin `object Http` on top of a
`System.loadLibrary("http_fetch")` JNI bridge. The async + cancellable
`fetch` becomes a `suspend fun` that forwards coroutine cancellation to
the C token via `invokeOnCancellation`:

```kotlin
import com.weaveffi.Http
import com.weaveffi.HttpMethod
import kotlinx.coroutines.*

suspend fun demo() {
    // WEAVEFFI_HTTP_USER_AGENT must already be set in the process env
    // (e.g. via the Android launcher or a ProcessBuilder). JVM does not
    // expose a portable way to mutate the POSIX env after startup.

    val resp = Http.fetch(
        url = "https://httpbin.org/get",
        method = HttpMethod.Get,
        body = null,
        timeoutMs = 10_000,
    )
    println("status=${resp.status}")
    println("body=${String(resp.body, Charsets.UTF_8)}")
    println("content-type=${resp.headers["content-type"]}")

    // Cancellation: cancelling the coroutine flips the C cancel token.
    val job = CoroutineScope(Dispatchers.IO).launch {
        try {
            Http.fetch("https://httpbin.org/delay/10", HttpMethod.Get, null, 30_000)
        } catch (_: CancellationException) {
            // Propagated from ERR_CODE_CANCELLED on the Rust side.
        }
    }
    job.cancelAndJoin()
}
```

The generated `HttpResponse` class closes its native handle on
`close()` / `AutoCloseable`, so either `use { }` or letting it go out
of scope keeps native memory bounded.

## Python consumer code

The Python generator emits a `weaveffi` package
(`generated/python/weaveffi/`) that loads the cdylib through `ctypes`.
`fetch` becomes an `async def`; cancellation via `asyncio.CancelledError`
is forwarded to the C cancel token:

```python
import asyncio
import os

# Must be set before the cdylib is first imported.
os.environ["WEAVEFFI_HTTP_USER_AGENT"] = "WeaveDemo/1.0"

from weaveffi import HttpMethod, http_fetch

async def main() -> None:
    resp = await http_fetch(
        "https://httpbin.org/get",
        HttpMethod.Get,
        None,                # no body
        10_000,              # 10s timeout
    )
    print("status:", resp.status)
    print("body:", bytes(resp.body).decode("utf-8", errors="replace"))
    print("content-type:", resp.headers.get("content-type"))

    # Cancellation: cancelling the awaiting task flips the C cancel token.
    task = asyncio.create_task(
        http_fetch("https://httpbin.org/delay/10", HttpMethod.Get, None, 30_000)
    )
    await asyncio.sleep(0.05)
    task.cancel()
    try:
        await task
    except asyncio.CancelledError:
        pass

asyncio.run(main())
```

## Build the cdylib and run the tests

From the repo root:

```bash
cargo build -p http-fetch
cargo test  -p http-fetch
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libhttp_fetch.dylib`
- Linux: `target/debug/libhttp_fetch.so`
- Windows: `target\debug\http_fetch.dll`

The `#[cfg(test)]` block uses [`wiremock`](https://crates.io/crates/wiremock)
to spin up a loopback HTTP server and covers GET / POST round-trips,
404 propagation, empty-URL and invalid-method rejection, cancellation,
the `HttpResponse` getter surface, and both the default and overridden
`User-Agent`. No external network access is performed during tests.
