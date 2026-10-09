//! The C ABI revision 5 families end to end: optional scalars (OptDirect)
//! and numeric lists (Slice) in every position, `usize`/`isize`/`char`,
//! custom types, typed callback errors, and thread-affine vtables. This
//! test crate is `abi5`, so that's the prefix of every symbol.

#![allow(unsafe_code)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::time::Duration;

use weaveffi::abi::{self, FfiError};

const WAIT: Duration = Duration::from_secs(30);

/// A hex-encoded `u32`: crosses as a `string`.
fn parse_hex(text: String) -> Result<u32, std::num::ParseIntError> {
    u32::from_str_radix(&text, 16)
}

fn to_hex(value: &u32) -> String {
    format!("{value:x}")
}

#[weaveffi::module]
pub mod fam {
    use std::sync::Arc;

    use weaveffi::ForeignError;

    /// A C-style enum, optional at the boundary.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Mode {
        /// Off.
        Off = 0,
        /// On.
        On = 1,
    }

    /// A number written in hex.
    #[weaveffi::custom(repr = String, lift = super::parse_hex, lower = super::to_hex)]
    pub type Hex = u32;

    /// A record with custom-typed fields.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Tagged {
        /// The id.
        pub id: Hex,
        /// More ids.
        pub others: Vec<Hex>,
        /// A plain field.
        pub label: String,
    }

    /// The probe's errors.
    #[weaveffi::error]
    #[derive(Debug, PartialEq)]
    #[repr(i32)]
    pub enum ProbeError {
        /// a bad reading
        #[weaveffi(message = "bad reading {n}")]
        Bad {
            /// The reading.
            n: i32,
        } = 1,
        /// the probe failed
        #[weaveffi(message = "probe failed ({code}): {message}")]
        Failed {
            /// The runtime code.
            code: i32,
            /// The message.
            message: String,
        } = 2,
    }

    impl From<ForeignError> for ProbeError {
        fn from(e: ForeignError) -> Self {
            Self::Failed {
                code: e.code,
                message: e.message,
            }
        }
    }

    /// A consumer-implemented probe.
    #[weaveffi::callback_interface]
    pub trait Probe: Send + Sync {
        /// The next reading after `last`, if any.
        fn reading(&self, last: Option<f64>) -> Result<Option<f64>, ProbeError>;
        /// `n` samples, scaled by the elements of `scale`.
        fn samples(&self, scale: &[f32]) -> Result<Vec<f32>, ForeignError>;
        /// A one-character tag.
        fn tag(&self) -> Result<char, ForeignError>;
        /// A notification with no return.
        fn ping(&self, count: usize) -> Result<(), ForeignError>;
    }

    /// Half of `x`, if any.
    #[weaveffi::export]
    pub fn half(x: Option<i32>) -> Option<f64> {
        x.map(|v| f64::from(v) / 2.0)
    }

    /// The other mode, if any.
    #[weaveffi::export]
    pub fn flip(mode: Option<Mode>) -> Option<Mode> {
        mode.map(|m| match m {
            Mode::Off => Mode::On,
            Mode::On => Mode::Off,
        })
    }

    /// Negate a flag.
    #[weaveffi::export]
    pub fn not(flag: Option<bool>) -> Option<bool> {
        flag.map(|f| !f)
    }

    /// Scale every element (a borrowed array in, an array out).
    #[weaveffi::export]
    pub fn scale(xs: &[f64], by: f64) -> Vec<f64> {
        xs.iter().map(|x| x * by).collect()
    }

    /// The address of a borrowed array, proving it wasn't copied.
    #[weaveffi::export]
    pub fn addr(xs: &[i64]) -> u64 {
        xs.as_ptr() as u64
    }

    /// Sizes doubled (`usize` crosses as `u64`).
    #[weaveffi::export]
    pub fn doubled(sizes: Vec<usize>) -> Vec<usize> {
        sizes.iter().map(|s| s * 2).collect()
    }

    /// A count (`usize` in and out).
    #[weaveffi::export]
    pub fn count(n: usize) -> usize {
        n + 1
    }

    /// An offset (`isize` in and out).
    #[weaveffi::export]
    pub fn offset(n: isize) -> isize {
        n - 1
    }

    /// The first character of `name`.
    #[weaveffi::export]
    pub fn initial(name: &str) -> char {
        name.chars().next().unwrap_or('?')
    }

    /// A character, repeated.
    #[weaveffi::export]
    pub fn repeat(c: char, n: u8) -> String {
        std::iter::repeat_n(c, usize::from(n)).collect()
    }

    /// Add one to a hex number.
    #[weaveffi::export]
    pub fn bump(h: Hex) -> Hex {
        h + 1
    }

    /// The largest of several hex numbers.
    #[weaveffi::export]
    pub fn largest(hs: Vec<Hex>) -> Option<Hex> {
        hs.into_iter().max()
    }

    /// Tag an id.
    #[weaveffi::export]
    pub fn tagged(id: Hex, label: String) -> Tagged {
        Tagged {
            id,
            others: vec![id + 1, id + 2],
            label,
        }
    }

    /// The sum of a tagged record's ids.
    #[weaveffi::export]
    pub fn total(t: &Tagged) -> u32 {
        t.id + t.others.iter().sum::<u32>()
    }

    /// Complete later with `x` doubled, if any.
    #[weaveffi::export]
    pub async fn double_later(x: Option<u32>) -> Option<u32> {
        x.map(|v| v * 2)
    }

    /// Complete later with `0..n`.
    #[weaveffi::export]
    pub async fn range_later(n: u32) -> Vec<i64> {
        (0..i64::from(n)).collect()
    }

    /// `n` arrays of `0..i`, pulled lazily.
    #[weaveffi::export]
    pub fn ramps(n: u16) -> weaveffi::Iter<Vec<u16>> {
        weaveffi::Iter::new((0..n).map(|i| (0..i).collect()))
    }

    /// Alternating flags with a gap, pulled lazily.
    #[weaveffi::export]
    pub fn flags(n: u8) -> weaveffi::Iter<Option<bool>> {
        weaveffi::Iter::new((0..n).map(|i| (i % 3 != 2).then_some(i % 2 == 0)))
    }

    /// Exercise every method of `probe` and describe the results.
    #[weaveffi::export]
    pub fn survey(probe: Arc<dyn Probe>) -> Result<String, ProbeError> {
        let first = probe.reading(None)?;
        let second = probe.reading(first)?;
        let samples = probe.samples(&[1.0, 2.0]).map_err(ProbeError::from)?;
        let tag = probe.tag().map_err(ProbeError::from)?;
        probe.ping(3).map_err(ProbeError::from)?;
        Ok(format!("{first:?} {second:?} {samples:?} {tag}"))
    }

    /// Call `reading` from another thread and report the outcome.
    #[weaveffi::export]
    pub fn read_elsewhere(probe: Arc<dyn Probe>) -> String {
        std::thread::spawn(move || {
            probe
                .ping(1)
                .map_err(ProbeError::from)
                .and(probe.reading(None))
        })
        .join()
        .map_or_else(|_| "panicked".to_string(), |r| format!("{r:?}"))
    }
}

weaveffi::export_runtime!();

fn message(err: &FfiError) -> String {
    unsafe { err.message_str() }.unwrap_or_default().to_string()
}

fn take_string(ptr: *const u8, len: usize) -> String {
    let s = unsafe { abi::lift_string(ptr, len) }.expect("valid UTF-8");
    unsafe { abi::free_bytes(ptr.cast_mut(), len) };
    s
}

/// Copy a returned typed array and release it, as a binding does.
fn take_slice<T: Copy>(ptr: *const T, len: usize) -> Vec<T> {
    if len == 0 {
        assert!(ptr.is_null());
        return Vec::new();
    }
    assert_eq!(ptr as usize % abi::RUN_ALIGN, 0, "every run is 8-aligned");
    let out = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
    unsafe { abi::free_bytes(ptr.cast_mut().cast(), len * std::mem::size_of::<T>()) };
    out
}

#[test]
fn optional_scalars_cross_directly() {
    let mut err = FfiError::default();
    let mut out = 0.0f64;
    assert!(unsafe { fam::abi5_fam_half(true, 7, &mut out, &mut err) });
    assert_eq!(out, 3.5);
    assert!(!unsafe { fam::abi5_fam_half(false, 99, &mut out, &mut err) });
    assert_eq!(err.code, 0);

    let mut mode = -1i32;
    assert!(unsafe { fam::abi5_fam_flip(true, 1, &mut mode, &mut err) });
    assert_eq!(mode, 0);
    assert!(!unsafe { fam::abi5_fam_flip(false, 0, &mut mode, &mut err) });
    // A present value is still checked; an absent one is ignored.
    assert!(!unsafe { fam::abi5_fam_flip(true, 7, &mut mode, &mut err) });
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    assert_eq!(message(&err), "mode: 7 is not a valid Mode");
    assert!(!unsafe { fam::abi5_fam_flip(false, 7, &mut mode, &mut err) });
    assert_eq!(err.code, 0);

    let mut flag = true;
    assert!(unsafe { fam::abi5_fam_not(true, true, &mut flag, &mut err) });
    assert!(!flag);
}

#[test]
fn numeric_lists_cross_as_typed_arrays() {
    let mut err = FfiError::default();
    let xs = [1.0f64, -2.5, 4.0];
    let mut len = 0usize;
    let ptr = unsafe { fam::abi5_fam_scale(xs.as_ptr(), xs.len(), 2.0, &mut len, &mut err) };
    assert_eq!(take_slice(ptr, len), [2.0, -5.0, 8.0]);
    let ptr = unsafe { fam::abi5_fam_scale(std::ptr::null(), 0, 2.0, &mut len, &mut err) };
    assert_eq!(take_slice(ptr, len), Vec::<f64>::new());

    let words = [1i64, 2, 3];
    assert_eq!(
        unsafe { fam::abi5_fam_addr(words.as_ptr(), 3, &mut err) },
        words.as_ptr() as u64,
        "a borrowed slice is lent, not copied"
    );
    let misaligned = unsafe { words.as_ptr().cast::<u8>().add(4) }.cast::<i64>();
    assert_eq!(unsafe { fam::abi5_fam_addr(misaligned, 1, &mut err) }, 0);
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);

    let sizes = [1u64, 20, 300];
    let ptr = unsafe { fam::abi5_fam_doubled(sizes.as_ptr(), 3, &mut len, &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(take_slice(ptr, len), [2u64, 40, 600]);
}

#[test]
fn sizes_and_chars_have_idl_types() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { fam::abi5_fam_count(41, &mut err) }, 42u64);
    assert_eq!(unsafe { fam::abi5_fam_offset(-41, &mut err) }, -42i64);
    if usize::BITS < 64 {
        assert_eq!(unsafe { fam::abi5_fam_count(u64::MAX, &mut err) }, 0);
        assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    }

    let name = "\u{1F980}rab";
    let mut len = 0usize;
    let ptr = unsafe { fam::abi5_fam_initial(name.as_ptr(), name.len(), &mut len, &mut err) };
    assert_eq!(take_string(ptr, len), "\u{1F980}");
    let c = "é";
    let ptr = unsafe { fam::abi5_fam_repeat(c.as_ptr(), c.len(), 3, &mut len, &mut err) };
    assert_eq!(take_string(ptr, len), "ééé");
    let two = "ab";
    let ptr = unsafe { fam::abi5_fam_repeat(two.as_ptr(), two.len(), 3, &mut len, &mut err) };
    assert!(ptr.is_null());
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    assert_eq!(message(&err), "c: \"ab\" is not a valid char");
}

#[test]
fn custom_types_cross_as_their_repr() {
    let mut err = FfiError::default();
    let mut len = 0usize;
    let h = "ff";
    let ptr = unsafe { fam::abi5_fam_bump(h.as_ptr(), h.len(), &mut len, &mut err) };
    assert_eq!(take_string(ptr, len), "100");
    let bad = "zz";
    let ptr = unsafe { fam::abi5_fam_bump(bad.as_ptr(), bad.len(), &mut len, &mut err) };
    assert!(ptr.is_null());
    assert_eq!(err.code, abi::MARSHAL_ERROR_CODE);
    assert_eq!(message(&err), "h: invalid digit found in string");

    let hs = abi::encode_value(&vec!["a".to_string(), "1f".to_string()]);
    let ptr = unsafe { fam::abi5_fam_largest(hs.as_ptr(), hs.len(), &mut len, &mut err) };
    let bytes = take_slice(ptr, len);
    assert_eq!(
        unsafe { abi::decode_value::<Option<String>>(&bytes) }.unwrap(),
        Some("1f".to_string())
    );

    let label = "x";
    let ptr = unsafe {
        fam::abi5_fam_tagged(
            h.as_ptr(),
            h.len(),
            label.as_ptr(),
            label.len(),
            &mut len,
            &mut err,
        )
    };
    let bytes = take_slice(ptr, len);
    // The record's custom fields encode as their repr...
    let mut r = abi::BufferReader::new(&bytes);
    assert_eq!(r.read_string().unwrap(), "ff");
    assert_eq!(r.read_count().unwrap(), 2);
    assert_eq!(r.read_string().unwrap(), "100");
    // ...and decode back through `lift`.
    let tagged: fam::Tagged = unsafe { abi::decode_value(&bytes) }.unwrap();
    assert_eq!(tagged.id, 255);
    assert_eq!(tagged.others, [256, 257]);
    assert_eq!(
        unsafe { fam::abi5_fam_total(bytes.as_ptr(), bytes.len(), &mut err) },
        255 + 256 + 257
    );
}

type Done<T> = mpsc::Sender<(i32, T)>;

#[test]
fn async_results_of_the_new_families() {
    extern "C" fn on_opt(ctx: *mut c_void, err: *mut FfiError, has: bool, value: u32) {
        assert!(err.is_null());
        let tx = unsafe { &*ctx.cast::<Done<Option<u32>>>() }.clone();
        tx.send((0, has.then_some(value))).unwrap();
    }
    extern "C" fn on_slice(ctx: *mut c_void, err: *mut FfiError, ptr: *const i64, len: usize) {
        assert!(err.is_null());
        let tx = unsafe { &*ctx.cast::<Done<Vec<i64>>>() }.clone();
        tx.send((0, take_slice(ptr, len))).unwrap();
    }

    let (tx, rx) = mpsc::channel::<(i32, Option<u32>)>();
    let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
    unsafe { fam::abi5_fam_double_later(true, 21, on_opt, ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, Some(42)));
    unsafe { fam::abi5_fam_double_later(false, 21, on_opt, ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, None));
    drop(unsafe { Box::from_raw(ctx.cast::<Done<Option<u32>>>()) });

    let (tx, rx) = mpsc::channel::<(i32, Vec<i64>)>();
    let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
    unsafe { fam::abi5_fam_range_later(4, on_slice, ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, vec![0, 1, 2, 3]));
    unsafe { fam::abi5_fam_range_later(0, on_slice, ctx) };
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), (0, vec![]));
    drop(unsafe { Box::from_raw(ctx.cast::<Done<Vec<i64>>>()) });
}

#[test]
fn iterator_items_of_the_new_families() {
    let mut err = FfiError::default();
    let it = unsafe { fam::abi5_fam_ramps(3, &mut err) };
    let mut got = Vec::new();
    loop {
        let mut item: *mut u16 = std::ptr::null_mut();
        let mut len = 0usize;
        if unsafe { fam::abi5_fam_RampsIterator_next(it, &mut item, &mut len, &mut err) } == 0 {
            break;
        }
        got.push(take_slice(item, len));
    }
    unsafe { fam::abi5_fam_RampsIterator_destroy(it) };
    assert_eq!(got, vec![vec![], vec![0], vec![0, 1]]);

    let it = unsafe { fam::abi5_fam_flags(4, &mut err) };
    let mut got = Vec::new();
    loop {
        let (mut has, mut item) = (false, false);
        if unsafe { fam::abi5_fam_FlagsIterator_next(it, &mut has, &mut item, &mut err) } == 0 {
            break;
        }
        got.push(has.then_some(item));
    }
    // A null out slot fails before an element is pulled.
    let mut item = false;
    let r =
        unsafe { fam::abi5_fam_FlagsIterator_next(it, std::ptr::null_mut(), &mut item, &mut err) };
    assert_eq!((r, err.code), (0, abi::MARSHAL_ERROR_CODE));
    unsafe { fam::abi5_fam_FlagsIterator_destroy(it) };
    assert_eq!(got, vec![Some(true), Some(false), None, Some(false)]);
}

/// A consumer-side `Probe`.
mod consumer_probe {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub struct State {
        pub fail_reading: bool,
        pub pings: AtomicUsize,
    }

    unsafe extern "C" fn reading(
        ctx: *mut c_void,
        has_last: bool,
        last: f64,
        out_value: *mut f64,
        out_err: *mut FfiError,
    ) -> bool {
        let state = unsafe { &*ctx.cast::<State>() };
        if state.fail_reading {
            // A typed failure of the method's own domain, with its payload.
            let msg = "bad";
            let fields = abi::encode_value(&7i32);
            unsafe {
                super::abi5_error_set(out_err, 1, msg.as_ptr(), msg.len());
                super::abi5_error_set_payload(out_err, fields.as_ptr(), fields.len());
            }
            return false;
        }
        if has_last {
            if last > 1.0 {
                return false;
            }
            unsafe { *out_value = last + 1.0 };
        } else {
            unsafe { *out_value = 0.5 };
        }
        true
    }

    unsafe extern "C" fn samples(
        _ctx: *mut c_void,
        scale_ptr: *const f32,
        scale_len: usize,
        out_ptr: *mut *mut f32,
        out_len: *mut usize,
        _err: *mut FfiError,
    ) {
        let scale = unsafe { std::slice::from_raw_parts(scale_ptr, scale_len) };
        let run = super::abi5_alloc(scale.len() * 4).cast::<f32>();
        for (i, s) in scale.iter().enumerate() {
            unsafe { run.add(i).write(s * 10.0) };
        }
        unsafe {
            *out_ptr = run;
            *out_len = scale.len();
        }
    }

    unsafe extern "C" fn tag(
        _ctx: *mut c_void,
        out_ptr: *mut *mut u8,
        out_len: *mut usize,
        _err: *mut FfiError,
    ) {
        let text = "λ";
        let run = super::abi5_alloc(text.len());
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), run, text.len());
            *out_ptr = run;
            *out_len = text.len();
        }
    }

    unsafe extern "C" fn ping(ctx: *mut c_void, count: u64, _err: *mut FfiError) {
        let state = unsafe { &*ctx.cast::<State>() };
        state.pings.fetch_add(count as usize, Ordering::SeqCst);
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        drop(unsafe { Box::from_raw(ctx.cast::<State>()) });
    }

    pub fn vtable(flags: u32) -> fam::abi5_fam_Probe_vtable {
        fam::abi5_fam_Probe_vtable {
            header: abi::VtableHeader {
                size: std::mem::size_of::<fam::abi5_fam_Probe_vtable>() as u32,
                flags,
                free,
            },
            reading,
            samples,
            tag,
            ping,
        }
    }

    pub fn ctx(fail_reading: bool) -> *mut c_void {
        Box::into_raw(Box::new(State {
            fail_reading,
            pings: AtomicUsize::new(0),
        }))
        .cast()
    }
}

#[test]
fn callback_methods_with_the_new_families() {
    let vtable = Box::leak(Box::new(consumer_probe::vtable(0)));
    let mut err = FfiError::default();
    let mut len = 0usize;
    let ptr =
        unsafe { fam::abi5_fam_survey(consumer_probe::ctx(false), vtable, &mut len, &mut err) };
    assert_eq!(err.code, 0, "{}", message(&err));
    assert_eq!(take_string(ptr, len), "Some(0.5) Some(1.5) [10.0, 20.0] λ");

    // The consumer's typed failure arrives typed and propagates with its
    // code, message, and payload.
    let ptr =
        unsafe { fam::abi5_fam_survey(consumer_probe::ctx(true), vtable, &mut len, &mut err) };
    assert!(ptr.is_null());
    assert_eq!(err.code, 1);
    assert_eq!(message(&err), "bad reading 7");
    assert_eq!(
        unsafe { abi::decode_value::<i32>(err.payload()) }.unwrap(),
        7
    );
    unsafe { abi::error_clear(&mut err) };
}

#[test]
fn thread_affine_vtables_reject_value_methods_off_thread() {
    let mut err = FfiError::default();
    let mut len = 0usize;
    let affine = Box::leak(Box::new(consumer_probe::vtable(abi::VTABLE_THREAD_AFFINE)));
    let ptr = unsafe {
        fam::abi5_fam_read_elsewhere(consumer_probe::ctx(false), affine, &mut len, &mut err)
    };
    // `ping` (no return value) still ran on the other thread; `reading`
    // failed without being called, reaching the producer as `-4`.
    assert_eq!(
        take_string(ptr, len),
        "Err(Failed { code: -4, message: \"callback called off its thread\" })"
    );
    let free_threaded = Box::leak(Box::new(consumer_probe::vtable(0)));
    let ptr = unsafe {
        fam::abi5_fam_read_elsewhere(
            consumer_probe::ctx(false),
            free_threaded,
            &mut len,
            &mut err,
        )
    };
    assert_eq!(take_string(ptr, len), "Ok(Some(0.5))");
    // On the adopting thread, an affine vtable works normally.
    let ptr =
        unsafe { fam::abi5_fam_survey(consumer_probe::ctx(false), affine, &mut len, &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(take_string(ptr, len), "Some(0.5) Some(1.5) [10.0, 20.0] λ");
}

#[test]
fn contract_tables_carry_per_method_and_per_code_entries() {
    fn fnv(data: &str) -> u64 {
        data.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
    let mut len = 0usize;
    let ptr = unsafe { fam::abi5_fam_contract(&mut len) };
    let table = unsafe { std::slice::from_raw_parts(ptr, len) };
    let hash = |path: &str| table.iter().find(|e| e.id == fnv(path)).map(|e| e.hash);
    assert_eq!(hash("fam.Probe"), Some(fnv("callback Probe")));
    assert_eq!(
        hash("fam.Probe.reading"),
        Some(fnv(
            "callback_method reading(f64?) -> f64? throws ProbeError"
        ))
    );
    assert_eq!(
        hash("fam.Probe.samples"),
        Some(fnv("callback_method samples([f32]) -> [f32] throws any"))
    );
    assert_eq!(
        hash("fam.Probe.ping"),
        Some(fnv("callback_method ping(u64) -> void throws any"))
    );
    assert_eq!(hash("fam.ProbeError"), Some(fnv("errors ProbeError")));
    assert_eq!(
        hash("fam.ProbeError.Failed"),
        Some(fnv("code Failed = 2 {i32, string}"))
    );
    assert_eq!(
        hash("fam.tagged"),
        Some(fnv("function tagged(string, string) -> Tagged"))
    );
    assert_eq!(
        hash("fam.Tagged"),
        Some(fnv("record Tagged {string, [string], string}"))
    );
}
