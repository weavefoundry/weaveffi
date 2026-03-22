#![allow(unsafe_code)]

use libloading::{Library, Symbol};
use napi::bindgen_prelude::*;
use napi_derive::napi;
use once_cell::sync::OnceCell;
use std::ffi::CStr;
use std::os::raw::c_char;
use weaveffi_abi::weaveffi_error;

type AddFn = unsafe extern "C" fn(i32, i32, *mut weaveffi_error) -> i32;
type MulFn = unsafe extern "C" fn(i32, i32, *mut weaveffi_error) -> i32;
type DivFn = unsafe extern "C" fn(i32, i32, *mut weaveffi_error) -> i32;
type EchoFn = unsafe extern "C" fn(*const c_char, *mut weaveffi_error) -> *const c_char;
type FreeStringFn = unsafe extern "C" fn(*const c_char);
type ErrorClearFn = unsafe extern "C" fn(*mut weaveffi_error);

struct FfiApi {
    add: AddFn,
    mul: MulFn,
    div: DivFn,
    echo: EchoFn,
    free_string: FreeStringFn,
    error_clear: ErrorClearFn,
}

static API: OnceCell<(Library, FfiApi)> = OnceCell::new();

fn load_api() -> napi::Result<&'static (Library, FfiApi)> {
    API.get_or_try_init(|| {
        let path = std::env::var("WEAVEFFI_LIB").ok().unwrap_or_else(|| {
            let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            p.pop(); // up to samples/
            p.pop(); // up to workspace root
            p.push("target");
            p.push("debug");
            let lib_name = if cfg!(target_os = "macos") {
                "libcalculator.dylib"
            } else if cfg!(target_os = "windows") {
                "calculator.dll"
            } else {
                "libcalculator.so"
            };
            p.push(lib_name);
            p.display().to_string()
        });
        let lib = unsafe { Library::new(&path) }
            .map_err(|e| Error::new(Status::GenericFailure, format!("load {}: {}", path, e)))?;
        let api: FfiApi = unsafe {
            let add: Symbol<AddFn> = lib.get(b"weaveffi_calculator_add").map_err(map_err)?;
            let mul: Symbol<MulFn> = lib.get(b"weaveffi_calculator_mul").map_err(map_err)?;
            let div: Symbol<DivFn> = lib.get(b"weaveffi_calculator_div").map_err(map_err)?;
            let echo: Symbol<EchoFn> = lib.get(b"weaveffi_calculator_echo").map_err(map_err)?;
            let free_string: Symbol<FreeStringFn> =
                lib.get(b"weaveffi_free_string").map_err(map_err)?;
            let error_clear: Symbol<ErrorClearFn> =
                lib.get(b"weaveffi_error_clear").map_err(map_err)?;
            FfiApi {
                add: *add,
                mul: *mul,
                div: *div,
                echo: *echo,
                free_string: *free_string,
                error_clear: *error_clear,
            }
        };
        Ok((lib, api))
    })
}

fn map_err(e: libloading::Error) -> Error {
    Error::new(Status::GenericFailure, e.to_string())
}

fn take_error(api: &FfiApi, err: &mut weaveffi_error) -> Option<(i32, String)> {
    if err.code == 0 {
        return None;
    }
    let code = err.code;
    let msg = if err.message.is_null() {
        String::new()
    } else {
        // SAFETY: message is NUL-terminated string from Rust side
        unsafe { CStr::from_ptr(err.message) }
            .to_string_lossy()
            .to_string()
    };
    // SAFETY: clear frees message buffer
    unsafe { (api.error_clear)(err as *mut weaveffi_error) };
    Some((code, msg))
}

#[napi]
pub fn add(a: i32, b: i32) -> napi::Result<i32> {
    let mut err = weaveffi_error::default();
    let (_, api) = load_api()?;
    let rv = unsafe { (api.add)(a, b, &mut err) };
    if let Some((code, msg)) = take_error(api, &mut err) {
        return Err(Error::new(
            Status::GenericFailure,
            format!("({}) {}", code, msg),
        ));
    }
    Ok(rv)
}

#[napi]
pub fn mul(a: i32, b: i32) -> napi::Result<i32> {
    let mut err = weaveffi_error::default();
    let (_, api) = load_api()?;
    let rv = unsafe { (api.mul)(a, b, &mut err) };
    if let Some((code, msg)) = take_error(api, &mut err) {
        return Err(Error::new(
            Status::GenericFailure,
            format!("({}) {}", code, msg),
        ));
    }
    Ok(rv)
}

#[napi]
pub fn div(a: i32, b: i32) -> napi::Result<i32> {
    let mut err = weaveffi_error::default();
    let (_, api) = load_api()?;
    let rv = unsafe { (api.div)(a, b, &mut err) };
    if let Some((code, msg)) = take_error(api, &mut err) {
        return Err(Error::new(
            Status::GenericFailure,
            format!("({}) {}", code, msg),
        ));
    }
    Ok(rv)
}

#[napi]
pub fn echo(s: String) -> napi::Result<String> {
    let mut err = weaveffi_error::default();
    let (_, api) = load_api()?;
    let c_str =
        std::ffi::CString::new(s).map_err(|e| Error::new(Status::GenericFailure, e.to_string()))?;
    let c_ptr = unsafe { (api.echo)(c_str.as_ptr(), &mut err) };
    if let Some((code, msg)) = take_error(api, &mut err) {
        return Err(Error::new(
            Status::GenericFailure,
            format!("({}) {}", code, msg),
        ));
    }
    if c_ptr.is_null() {
        return Err(Error::new(
            Status::GenericFailure,
            "null string".to_string(),
        ));
    }
    let out = unsafe { CStr::from_ptr(c_ptr) }
        .to_string_lossy()
        .to_string();
    unsafe { (api.free_string)(c_ptr) };
    Ok(out)
}
