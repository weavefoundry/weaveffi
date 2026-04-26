//! HTTP-fetch sample.
//!
//! Demonstrates an async + cancellable WeaveFFI C ABI function backed by
//! `reqwest`. A single shared multi-thread Tokio runtime drives the request
//! future and a cooperative cancel-poll future races against it, so the
//! foreign caller can bail out promptly via a `weaveffi_cancel_token` while
//! long-running network I/O is in flight.
#![allow(unsafe_code)]
#![allow(non_camel_case_types)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::raw::c_char;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::runtime::Runtime;
use weaveffi_abi::{self as abi, weaveffi_cancel_token, weaveffi_error};

// ── IR-mirrored Rust types ────────────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get = 0,
    Post = 1,
    Put = 2,
    Delete = 3,
}

impl HttpMethod {
    fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Get),
            1 => Some(Self::Post),
            2 => Some(Self::Put),
            3 => Some(Self::Delete),
            _ => None,
        }
    }

    fn as_reqwest(self) -> reqwest::Method {
        match self {
            Self::Get => reqwest::Method::GET,
            Self::Post => reqwest::Method::POST,
            Self::Put => reqwest::Method::PUT,
            Self::Delete => reqwest::Method::DELETE,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: i32,
    pub body: Vec<u8>,
    pub headers: HashMap<String, String>,
}

// ── Shared runtime ────────────────────────────────────────────────────────

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────

const ERR_CODE_GENERIC: i32 = 1;
const ERR_CODE_CANCELLED: i32 = 2;

/// Resolve the `User-Agent` header to send on outgoing requests. Consumers
/// can override the default at runtime via the `WEAVEFFI_HTTP_USER_AGENT`
/// environment variable; an unset or empty value falls back to a compiled-in
/// default that carries the crate version.
fn resolve_user_agent() -> String {
    match std::env::var("WEAVEFFI_HTTP_USER_AGENT") {
        Ok(v) if !v.is_empty() => v,
        _ => format!("weaveffi-http-fetch/{}", env!("CARGO_PKG_VERSION")),
    }
}

fn slice_to_string(ptr: *const u8, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(slice).ok().map(str::to_owned)
}

fn slice_to_bytes(ptr: *const u8, len: usize) -> Option<Vec<u8>> {
    if ptr.is_null() {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    Some(slice.to_vec())
}

/// Invoke a callback with a transient `weaveffi_error`, ensuring the heap
/// message allocated by `error_set` is released after the callback returns.
fn invoke_err_cb<F>(code: i32, message: &str, invoke: F)
where
    F: FnOnce(*mut weaveffi_error),
{
    let mut err = weaveffi_error::default();
    abi::error_set(&mut err, code, message);
    invoke(&mut err);
    abi::error_clear(&mut err);
}

// ── Async callback type ───────────────────────────────────────────────────

pub type weaveffi_http_fetch_callback =
    extern "C" fn(context: *mut c_void, err: *mut weaveffi_error, result: *mut HttpResponse);

// ── Module functions: async ───────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_http_fetch_async(
    url_ptr: *const u8,
    url_len: usize,
    method: i32,
    body_ptr: *const u8,
    body_len: usize,
    timeout_ms: i32,
    cancel_token: *mut weaveffi_cancel_token,
    callback: weaveffi_http_fetch_callback,
    context: *mut c_void,
) {
    let ctx_addr = context as usize;
    let token_addr = cancel_token as usize;

    let url = match slice_to_string(url_ptr, url_len) {
        Some(s) if !s.is_empty() => s,
        _ => {
            invoke_err_cb(ERR_CODE_GENERIC, "url is null or empty", |err| {
                callback(ctx_addr as *mut c_void, err, std::ptr::null_mut());
            });
            return;
        }
    };
    let http_method = match HttpMethod::from_i32(method) {
        Some(m) => m,
        None => {
            invoke_err_cb(ERR_CODE_GENERIC, "invalid method value", |err| {
                callback(ctx_addr as *mut c_void, err, std::ptr::null_mut());
            });
            return;
        }
    };
    let body = slice_to_bytes(body_ptr, body_len);
    let timeout = if timeout_ms > 0 {
        Some(Duration::from_millis(timeout_ms as u64))
    } else {
        None
    };

    // Raw pointers are `!Send`, so we ferry them across the spawned task as
    // `usize` addresses and recast them at each use site inside the async
    // block and its sub-futures.
    let user_agent = resolve_user_agent();

    runtime().spawn(async move {
        let mut builder = reqwest::Client::builder().user_agent(user_agent);
        if let Some(t) = timeout {
            builder = builder.timeout(t);
        }
        let client = match builder.build() {
            Ok(c) => c,
            Err(e) => {
                invoke_err_cb(ERR_CODE_GENERIC, &e.to_string(), |err| {
                    callback(ctx_addr as *mut c_void, err, std::ptr::null_mut());
                });
                return;
            }
        };

        let mut req = client.request(http_method.as_reqwest(), &url);
        if let Some(b) = body {
            req = req.body(b);
        }

        // Poll the cancel token on a short ladder. Short ticks keep
        // cancellation responsive without materially slowing the happy path,
        // where the request future almost always wins the race.
        let cancel_fut = async move {
            loop {
                let t = token_addr as *const weaveffi_cancel_token;
                if !t.is_null() && abi::cancel_token_is_cancelled(t) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };

        let send_fut = req.send();
        tokio::pin!(send_fut);

        let response = tokio::select! {
            r = &mut send_fut => r,
            _ = cancel_fut => {
                invoke_err_cb(ERR_CODE_CANCELLED, "cancelled", |err| {
                    callback(ctx_addr as *mut c_void, err, std::ptr::null_mut());
                });
                return;
            }
        };

        match response {
            Err(e) => invoke_err_cb(ERR_CODE_GENERIC, &e.to_string(), |err| {
                callback(ctx_addr as *mut c_void, err, std::ptr::null_mut());
            }),
            Ok(resp) => {
                let status = resp.status().as_u16() as i32;
                let mut headers: HashMap<String, String> = HashMap::new();
                for (k, v) in resp.headers().iter() {
                    if let Ok(value) = v.to_str() {
                        headers.insert(k.as_str().to_owned(), value.to_owned());
                    }
                }
                match resp.bytes().await {
                    Ok(bytes) => {
                        let result = Box::into_raw(Box::new(HttpResponse {
                            status,
                            body: bytes.to_vec(),
                            headers,
                        }));
                        callback(ctx_addr as *mut c_void, std::ptr::null_mut(), result);
                    }
                    Err(e) => invoke_err_cb(ERR_CODE_GENERIC, &e.to_string(), |err| {
                        callback(ctx_addr as *mut c_void, err, std::ptr::null_mut());
                    }),
                }
            }
        }
    });
}

// ── HttpResponse struct getters ───────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_http_HttpResponse_get_status(resp: *const HttpResponse) -> i32 {
    assert!(!resp.is_null());
    unsafe { (*resp).status }
}

#[no_mangle]
pub extern "C" fn weaveffi_http_HttpResponse_get_body(
    resp: *const HttpResponse,
    out_len: *mut usize,
) -> *mut u8 {
    assert!(!resp.is_null());
    let body = &unsafe { &*resp }.body;
    let len = body.len();
    if !out_len.is_null() {
        unsafe { *out_len = len };
    }
    if len == 0 {
        return std::ptr::null_mut();
    }
    let boxed: Box<[u8]> = body.clone().into_boxed_slice();
    Box::into_raw(boxed) as *mut u8
}

#[no_mangle]
pub extern "C" fn weaveffi_http_HttpResponse_get_headers(
    resp: *const HttpResponse,
    out_keys: *mut *const c_char,
    out_values: *mut *const c_char,
    out_len: *mut usize,
) {
    assert!(!resp.is_null());
    let headers = &unsafe { &*resp }.headers;
    let len = headers.len();
    if !out_len.is_null() {
        unsafe { *out_len = len };
    }
    if len == 0 {
        if !out_keys.is_null() {
            unsafe { *out_keys = std::ptr::null() };
        }
        if !out_values.is_null() {
            unsafe { *out_values = std::ptr::null() };
        }
        return;
    }

    let mut keys: Vec<*const c_char> = Vec::with_capacity(len);
    let mut vals: Vec<*const c_char> = Vec::with_capacity(len);
    for (k, v) in headers.iter() {
        keys.push(abi::string_to_c_ptr(k));
        vals.push(abi::string_to_c_ptr(v));
    }
    let keys_ptr = keys.as_mut_ptr();
    let vals_ptr = vals.as_mut_ptr();
    std::mem::forget(keys);
    std::mem::forget(vals);

    // The generated C header types these out-parameters as `const char**`
    // (K* out_keys where K = `const char*`). The caller receives the array
    // base via the single slot and indexes it as `out_keys[i]`, freeing
    // each string with `weaveffi_free_string` and the array itself with
    // `weaveffi_http_HttpResponse_headers_list_free`.
    if !out_keys.is_null() {
        unsafe { *out_keys = keys_ptr as *const c_char };
    }
    if !out_values.is_null() {
        unsafe { *out_values = vals_ptr as *const c_char };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_http_HttpResponse_headers_list_free(
    keys: *mut *const c_char,
    values: *mut *const c_char,
    len: usize,
) {
    if !keys.is_null() {
        let v = unsafe { Vec::from_raw_parts(keys, len, len) };
        for p in v {
            abi::free_string(p);
        }
    }
    if !values.is_null() {
        let v = unsafe { Vec::from_raw_parts(values, len, len) };
        for p in v {
            abi::free_string(p);
        }
    }
}

// ── HttpResponse lifecycle ────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_http_HttpResponse_destroy(resp: *mut HttpResponse) {
    if resp.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(resp)) };
}

// ── Enum helpers ──────────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_http_HttpMethod_from_i32(
    value: i32,
    out_err: *mut weaveffi_error,
) -> i32 {
    match HttpMethod::from_i32(value) {
        Some(m) => {
            abi::error_set_ok(out_err);
            m as i32
        }
        None => {
            abi::error_set(out_err, ERR_CODE_GENERIC, "invalid HttpMethod value");
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_http_HttpMethod_to_i32(m: i32) -> i32 {
    m
}

// ── Runtime re-exports ────────────────────────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr)
}

#[no_mangle]
pub extern "C" fn weaveffi_free_bytes(ptr: *mut u8, len: usize) {
    abi::free_bytes(ptr, len)
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err)
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_create() -> *mut weaveffi_cancel_token {
    abi::cancel_token_create()
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_cancel(token: *mut weaveffi_cancel_token) {
    abi::cancel_token_cancel(token)
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_is_cancelled(token: *const weaveffi_cancel_token) -> bool {
    abi::cancel_token_is_cancelled(token)
}

#[no_mangle]
pub extern "C" fn weaveffi_cancel_token_destroy(token: *mut weaveffi_cancel_token) {
    abi::cancel_token_destroy(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    type FetchMsg = (i32, Option<String>, *mut HttpResponse);

    extern "C" fn fetch_callback(
        context: *mut c_void,
        err: *mut weaveffi_error,
        result: *mut HttpResponse,
    ) {
        let tx = unsafe { &*(context as *const mpsc::Sender<FetchMsg>) };
        let (code, msg) = if err.is_null() {
            (0, None)
        } else {
            let e = unsafe { &*err };
            (e.code, abi::c_ptr_to_string(e.message))
        };
        tx.send((code, msg, result)).unwrap();
    }

    fn fetch_sync(url: &str, method_val: HttpMethod, body: Option<&[u8]>) -> FetchMsg {
        let (tx, rx) = mpsc::channel::<FetchMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let (body_ptr, body_len) = body.map_or((std::ptr::null(), 0), |b| (b.as_ptr(), b.len()));
        weaveffi_http_fetch_async(
            url.as_ptr(),
            url.len(),
            method_val as i32,
            body_ptr,
            body_len,
            5_000,
            std::ptr::null_mut(),
            fetch_callback,
            tx_ptr as *mut c_void,
        );
        let msg = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        msg
    }

    #[test]
    fn fetch_get_works_against_local_server() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let (uri, _guard) = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/hello"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("x-weave-test", "ok")
                        .set_body_string("hello, weave"),
                )
                .mount(&server)
                .await;
            let uri = format!("{}/hello", server.uri());
            (uri, server)
        });

        let (code, msg, ptr) = fetch_sync(&uri, HttpMethod::Get, None);
        assert_eq!(code, 0, "fetch errored: {msg:?}");
        assert!(!ptr.is_null(), "fetch returned null result");

        let resp = unsafe { *Box::from_raw(ptr) };
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, b"hello, weave");
        assert_eq!(
            resp.headers.get("x-weave-test").map(String::as_str),
            Some("ok")
        );
    }

    #[test]
    fn fetch_post_sends_body() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (uri, _guard) = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/echo"))
                .and(wiremock::matchers::body_bytes(b"ping".to_vec()))
                .respond_with(ResponseTemplate::new(201).set_body_string("pong"))
                .mount(&server)
                .await;
            let uri = format!("{}/echo", server.uri());
            (uri, server)
        });

        let (code, msg, ptr) = fetch_sync(&uri, HttpMethod::Post, Some(b"ping"));
        assert_eq!(code, 0, "post errored: {msg:?}");
        assert!(!ptr.is_null());
        let resp = unsafe { *Box::from_raw(ptr) };
        assert_eq!(resp.status, 201);
        assert_eq!(resp.body, b"pong");
    }

    #[test]
    fn fetch_reports_404_status() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (uri, _guard) = rt.block_on(async {
            let server = MockServer::start().await;
            let uri = format!("{}/missing", server.uri());
            (uri, server)
        });

        let (code, msg, ptr) = fetch_sync(&uri, HttpMethod::Get, None);
        assert_eq!(code, 0, "fetch errored: {msg:?}");
        assert!(!ptr.is_null());
        let resp = unsafe { *Box::from_raw(ptr) };
        assert_eq!(resp.status, 404);
    }

    #[test]
    fn fetch_rejects_empty_url() {
        let (tx, rx) = mpsc::channel::<FetchMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        weaveffi_http_fetch_async(
            std::ptr::null(),
            0,
            HttpMethod::Get as i32,
            std::ptr::null(),
            0,
            1_000,
            std::ptr::null_mut(),
            fetch_callback,
            tx_ptr as *mut c_void,
        );
        let (code, msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, ERR_CODE_GENERIC);
        assert!(msg.as_deref().unwrap_or("").contains("url"));
        assert!(ptr.is_null());
    }

    #[test]
    fn fetch_rejects_invalid_method() {
        let (tx, rx) = mpsc::channel::<FetchMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));
        let url = "http://127.0.0.1:1/x";
        weaveffi_http_fetch_async(
            url.as_ptr(),
            url.len(),
            99,
            std::ptr::null(),
            0,
            1_000,
            std::ptr::null_mut(),
            fetch_callback,
            tx_ptr as *mut c_void,
        );
        let (code, msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, ERR_CODE_GENERIC);
        assert!(msg.as_deref().unwrap_or("").contains("method"));
        assert!(ptr.is_null());
    }

    #[test]
    fn fetch_cancellation_returns_cancelled() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (uri, _guard) = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/slow"))
                .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
                .mount(&server)
                .await;
            let uri = format!("{}/slow", server.uri());
            (uri, server)
        });

        let token = abi::cancel_token_create();
        let (tx, rx) = mpsc::channel::<FetchMsg>();
        let tx_ptr = Box::into_raw(Box::new(tx));

        weaveffi_http_fetch_async(
            uri.as_ptr(),
            uri.len(),
            HttpMethod::Get as i32,
            std::ptr::null(),
            0,
            30_000,
            token,
            fetch_callback,
            tx_ptr as *mut c_void,
        );

        // Give the worker time to start before we cancel.
        std::thread::sleep(Duration::from_millis(50));
        abi::cancel_token_cancel(token);

        let (code, msg, ptr) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        unsafe { drop(Box::from_raw(tx_ptr)) };
        assert_eq!(code, ERR_CODE_CANCELLED);
        assert_eq!(msg.as_deref(), Some("cancelled"));
        assert!(ptr.is_null());
        abi::cancel_token_destroy(token);
    }

    #[test]
    fn http_response_getters_round_trip() {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());
        let resp = Box::into_raw(Box::new(HttpResponse {
            status: 200,
            body: b"body-bytes".to_vec(),
            headers,
        }));

        assert_eq!(weaveffi_http_HttpResponse_get_status(resp), 200);

        let mut body_len: usize = 0;
        let body_ptr = weaveffi_http_HttpResponse_get_body(resp, &mut body_len);
        assert_eq!(body_len, 10);
        let body_slice = unsafe { std::slice::from_raw_parts(body_ptr, body_len) };
        assert_eq!(body_slice, b"body-bytes");
        weaveffi_free_bytes(body_ptr, body_len);

        let mut keys_slot: *const c_char = std::ptr::null();
        let mut vals_slot: *const c_char = std::ptr::null();
        let mut hlen: usize = 0;
        weaveffi_http_HttpResponse_get_headers(resp, &mut keys_slot, &mut vals_slot, &mut hlen);
        assert_eq!(hlen, 1);
        let keys_arr = keys_slot as *mut *const c_char;
        let vals_arr = vals_slot as *mut *const c_char;
        let k0 = unsafe { *keys_arr };
        let v0 = unsafe { *vals_arr };
        assert_eq!(abi::c_ptr_to_string(k0).unwrap(), "content-type");
        assert_eq!(abi::c_ptr_to_string(v0).unwrap(), "text/plain");
        weaveffi_http_HttpResponse_headers_list_free(keys_arr, vals_arr, hlen);

        weaveffi_http_HttpResponse_destroy(resp);
    }

    #[test]
    fn http_response_getters_empty_headers() {
        let resp = Box::into_raw(Box::new(HttpResponse {
            status: 204,
            body: Vec::new(),
            headers: HashMap::new(),
        }));

        let mut body_len: usize = 123;
        let body_ptr = weaveffi_http_HttpResponse_get_body(resp, &mut body_len);
        assert!(body_ptr.is_null());
        assert_eq!(body_len, 0);

        let mut keys_slot: *const c_char = std::ptr::null();
        let mut vals_slot: *const c_char = std::ptr::null();
        let mut hlen: usize = 7;
        weaveffi_http_HttpResponse_get_headers(resp, &mut keys_slot, &mut vals_slot, &mut hlen);
        assert_eq!(hlen, 0);
        assert!(keys_slot.is_null());
        assert!(vals_slot.is_null());

        weaveffi_http_HttpResponse_destroy(resp);
    }

    #[test]
    fn http_method_from_i32_rejects_invalid() {
        let mut err = weaveffi_error::default();
        let v = weaveffi_http_HttpMethod_from_i32(99, &mut err);
        assert_eq!(v, -1);
        assert_eq!(err.code, ERR_CODE_GENERIC);
        abi::error_clear(&mut err);

        let v = weaveffi_http_HttpMethod_from_i32(HttpMethod::Put as i32, &mut err);
        assert_eq!(v, HttpMethod::Put as i32);
        assert_eq!(err.code, 0);
    }

    #[test]
    fn destroy_null_http_response_is_safe() {
        weaveffi_http_HttpResponse_destroy(std::ptr::null_mut());
    }

    // `std::env` is process-global, so the two user-agent tests serialise
    // through this mutex to avoid racing on `WEAVEFFI_HTTP_USER_AGENT`.
    static UA_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn resolve_user_agent_default_and_override() {
        let _g = UA_TEST_LOCK.lock().unwrap();
        std::env::remove_var("WEAVEFFI_HTTP_USER_AGENT");
        assert_eq!(
            resolve_user_agent(),
            format!("weaveffi-http-fetch/{}", env!("CARGO_PKG_VERSION"))
        );

        std::env::set_var("WEAVEFFI_HTTP_USER_AGENT", "my-weave/1.2.3");
        assert_eq!(resolve_user_agent(), "my-weave/1.2.3");

        std::env::set_var("WEAVEFFI_HTTP_USER_AGENT", "");
        assert_eq!(
            resolve_user_agent(),
            format!("weaveffi-http-fetch/{}", env!("CARGO_PKG_VERSION"))
        );

        std::env::remove_var("WEAVEFFI_HTTP_USER_AGENT");
    }

    #[test]
    fn fetch_sends_overridden_user_agent() {
        let _g = UA_TEST_LOCK.lock().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (uri, _guard) = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/ua"))
                .and(wiremock::matchers::header(
                    "user-agent",
                    "custom-weave-agent/9.9",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
                .mount(&server)
                .await;
            let uri = format!("{}/ua", server.uri());
            (uri, server)
        });

        std::env::set_var("WEAVEFFI_HTTP_USER_AGENT", "custom-weave-agent/9.9");
        let (code, msg, ptr) = fetch_sync(&uri, HttpMethod::Get, None);
        std::env::remove_var("WEAVEFFI_HTTP_USER_AGENT");

        assert_eq!(code, 0, "fetch errored: {msg:?}");
        assert!(!ptr.is_null());
        let resp = unsafe { *Box::from_raw(ptr) };
        assert_eq!(resp.status, 200);
    }
}
