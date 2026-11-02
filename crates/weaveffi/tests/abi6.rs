//! The C ABI revision 6 value shapes end to end: every family (a scalar, a
//! C-style enum, an optional scalar and enum, a string, bytes, a typed
//! array, a value buffer, and an object) in every position (a parameter, a
//! return, an async result, an iterator item, a callback argument, and a
//! callback return), each one C value. Then the families' edges: `bool`,
//! `usize`, `isize`, and `char`, borrowed parameters, custom types, typed
//! callback errors, and thread-affine vtables. This test crate is `abi6`,
//! so that's the prefix of every symbol.

#![allow(unsafe_code)]
// The `extern` blocks name the runtime's opaque types (objects, iterators)
// behind pointers, as the C header names incomplete structs.
#![allow(improper_ctypes)]

use std::os::raw::c_void;
use std::sync::mpsc;
use std::time::Duration;

use weaveffi::__rt::{self as rt, CallbackArg, FfiError, IterHandle, Opt, Run, VtableHeader};

weaveffi::export_runtime!();

const WAIT: Duration = Duration::from_secs(30);

/// The value of every family the matrix exercises.
pub mod family {
    use std::sync::Arc;

    /// A C-style enum with a gap in its discriminants.
    #[weaveffi::enumeration]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Mode {
        /// Off.
        Off = 0,
        /// On.
        On = 1,
        /// Chosen by the device.
        Auto = 5,
    }

    /// A record: a value buffer.
    #[weaveffi::record]
    #[derive(Clone, Debug, PartialEq)]
    pub struct Reading {
        /// The label.
        pub label: String,
        /// The value, if any.
        pub value: Option<i64>,
    }

    /// An object.
    #[weaveffi::interface]
    pub struct Cell {
        value: i64,
    }

    #[weaveffi::export]
    impl Cell {
        /// A cell holding `value`.
        pub fn new(value: i64) -> Self {
            Self { value }
        }
        /// The value.
        pub fn get(&self) -> i64 {
            self.value
        }
    }

    /// Echo every family back through the consumer: one method per family,
    /// each taking and returning it.
    #[weaveffi::callback_interface]
    pub trait Mirror: Send + Sync {
        /// A scalar.
        fn scalar(&self, x: i64) -> Result<i64, weaveffi::ForeignError>;
        /// A C-style enum.
        fn enumeration(&self, x: Mode) -> Result<Mode, weaveffi::ForeignError>;
        /// An optional scalar.
        fn opt_scalar(&self, x: Option<f64>) -> Result<Option<f64>, weaveffi::ForeignError>;
        /// An optional enum.
        fn opt_enum(&self, x: Option<Mode>) -> Result<Option<Mode>, weaveffi::ForeignError>;
        /// A string.
        fn text(&self, x: String) -> Result<String, weaveffi::ForeignError>;
        /// Bytes.
        fn bytes(&self, x: Vec<u8>) -> Result<Vec<u8>, weaveffi::ForeignError>;
        /// A typed array.
        fn typed(&self, x: Vec<f32>) -> Result<Vec<f32>, weaveffi::ForeignError>;
        /// A value buffer.
        fn buffer(&self, x: Reading) -> Result<Reading, weaveffi::ForeignError>;
        /// An object.
        fn object(&self, x: Arc<Cell>) -> Result<Arc<Cell>, weaveffi::ForeignError>;
        /// An optional object.
        fn opt_object(
            &self,
            x: Option<Arc<Cell>>,
        ) -> Result<Option<Arc<Cell>>, weaveffi::ForeignError>;
    }
}

/// For each family `F`: `echo_F` (a parameter and a return), `later_F` (an
/// async result), `iter_F` (two iterator items), and `bounce_F` (a callback
/// argument and return).
pub mod matrix {
    use std::sync::Arc;

    use weaveffi::{ForeignError, Iter};

    use super::family::{Cell, Mirror, Mode, Reading};

    /// Echo a scalar.
    #[weaveffi::export]
    pub fn echo_scalar(x: i64) -> i64 {
        x
    }
    /// Echo a scalar later.
    #[weaveffi::export]
    pub async fn later_scalar(x: i64) -> i64 {
        x
    }
    /// Yield a scalar twice.
    #[weaveffi::export]
    pub fn iter_scalar(x: i64) -> Iter<i64> {
        Iter::new([x, x])
    }
    /// Bounce a scalar off the consumer.
    #[weaveffi::export]
    pub fn bounce_scalar(m: Arc<dyn Mirror>, x: i64) -> Result<i64, ForeignError> {
        m.scalar(x)
    }

    /// Echo an enum.
    #[weaveffi::export]
    pub fn echo_enum(x: Mode) -> Mode {
        x
    }
    /// Echo an enum later.
    #[weaveffi::export]
    pub async fn later_enum(x: Mode) -> Mode {
        x
    }
    /// Yield an enum twice.
    #[weaveffi::export]
    pub fn iter_enum(x: Mode) -> Iter<Mode> {
        Iter::new([x, x])
    }
    /// Bounce an enum off the consumer.
    #[weaveffi::export]
    pub fn bounce_enum(m: Arc<dyn Mirror>, x: Mode) -> Result<Mode, ForeignError> {
        m.enumeration(x)
    }

    /// Echo an optional scalar.
    #[weaveffi::export]
    pub fn echo_opt_scalar(x: Option<f64>) -> Option<f64> {
        x
    }
    /// Echo an optional scalar later.
    #[weaveffi::export]
    pub async fn later_opt_scalar(x: Option<f64>) -> Option<f64> {
        x
    }
    /// Yield an optional scalar twice.
    #[weaveffi::export]
    pub fn iter_opt_scalar(x: Option<f64>) -> Iter<Option<f64>> {
        Iter::new([x, x])
    }
    /// Bounce an optional scalar off the consumer.
    #[weaveffi::export]
    pub fn bounce_opt_scalar(
        m: Arc<dyn Mirror>,
        x: Option<f64>,
    ) -> Result<Option<f64>, ForeignError> {
        m.opt_scalar(x)
    }

    /// Echo an optional enum.
    #[weaveffi::export]
    pub fn echo_opt_enum(x: Option<Mode>) -> Option<Mode> {
        x
    }
    /// Echo an optional enum later.
    #[weaveffi::export]
    pub async fn later_opt_enum(x: Option<Mode>) -> Option<Mode> {
        x
    }
    /// Yield an optional enum twice.
    #[weaveffi::export]
    pub fn iter_opt_enum(x: Option<Mode>) -> Iter<Option<Mode>> {
        Iter::new([x, x])
    }
    /// Bounce an optional enum off the consumer.
    #[weaveffi::export]
    pub fn bounce_opt_enum(
        m: Arc<dyn Mirror>,
        x: Option<Mode>,
    ) -> Result<Option<Mode>, ForeignError> {
        m.opt_enum(x)
    }

    /// Echo a string.
    #[weaveffi::export]
    pub fn echo_text(x: String) -> String {
        x
    }
    /// Echo a string later.
    #[weaveffi::export]
    pub async fn later_text(x: String) -> String {
        x
    }
    /// Yield a string twice.
    #[weaveffi::export]
    pub fn iter_text(x: String) -> Iter<String> {
        Iter::new([x.clone(), x])
    }
    /// Bounce a string off the consumer.
    #[weaveffi::export]
    pub fn bounce_text(m: Arc<dyn Mirror>, x: String) -> Result<String, ForeignError> {
        m.text(x)
    }

    /// Echo bytes.
    #[weaveffi::export]
    pub fn echo_bytes(x: Vec<u8>) -> Vec<u8> {
        x
    }
    /// Echo bytes later.
    #[weaveffi::export]
    pub async fn later_bytes(x: Vec<u8>) -> Vec<u8> {
        x
    }
    /// Yield bytes twice.
    #[weaveffi::export]
    pub fn iter_bytes(x: Vec<u8>) -> Iter<Vec<u8>> {
        Iter::new([x.clone(), x])
    }
    /// Bounce bytes off the consumer.
    #[weaveffi::export]
    pub fn bounce_bytes(m: Arc<dyn Mirror>, x: Vec<u8>) -> Result<Vec<u8>, ForeignError> {
        m.bytes(x)
    }

    /// Echo a typed array.
    #[weaveffi::export]
    pub fn echo_typed(x: Vec<f32>) -> Vec<f32> {
        x
    }
    /// Echo a typed array later.
    #[weaveffi::export]
    pub async fn later_typed(x: Vec<f32>) -> Vec<f32> {
        x
    }
    /// Yield a typed array twice.
    #[weaveffi::export]
    pub fn iter_typed(x: Vec<f32>) -> Iter<Vec<f32>> {
        Iter::new([x.clone(), x])
    }
    /// Bounce a typed array off the consumer.
    #[weaveffi::export]
    pub fn bounce_typed(m: Arc<dyn Mirror>, x: Vec<f32>) -> Result<Vec<f32>, ForeignError> {
        m.typed(x)
    }

    /// Echo a value buffer.
    #[weaveffi::export]
    pub fn echo_buffer(x: Reading) -> Reading {
        x
    }
    /// Echo a value buffer later.
    #[weaveffi::export]
    pub async fn later_buffer(x: Reading) -> Reading {
        x
    }
    /// Yield a value buffer twice.
    #[weaveffi::export]
    pub fn iter_buffer(x: Reading) -> Iter<Reading> {
        Iter::new([x.clone(), x])
    }
    /// Bounce a value buffer off the consumer.
    #[weaveffi::export]
    pub fn bounce_buffer(m: Arc<dyn Mirror>, x: Reading) -> Result<Reading, ForeignError> {
        m.buffer(x)
    }

    /// Echo an object.
    #[weaveffi::export]
    pub fn echo_object(x: Arc<Cell>) -> Arc<Cell> {
        x
    }
    /// Echo an object later.
    #[weaveffi::export]
    pub async fn later_object(x: Arc<Cell>) -> Arc<Cell> {
        x
    }
    /// Yield an object twice.
    #[weaveffi::export]
    pub fn iter_object(x: Arc<Cell>) -> Iter<Arc<Cell>> {
        Iter::new([x.clone(), x])
    }
    /// Bounce an object off the consumer.
    #[weaveffi::export]
    pub fn bounce_object(m: Arc<dyn Mirror>, x: Arc<Cell>) -> Result<Arc<Cell>, ForeignError> {
        m.object(x)
    }

    /// Echo an optional object.
    #[weaveffi::export]
    pub fn echo_opt_object(x: Option<Arc<Cell>>) -> Option<Arc<Cell>> {
        x
    }
    /// Echo an optional object later.
    #[weaveffi::export]
    pub async fn later_opt_object(x: Option<Arc<Cell>>) -> Option<Arc<Cell>> {
        x
    }
    /// Yield an optional object twice.
    #[weaveffi::export]
    pub fn iter_opt_object(x: Option<Arc<Cell>>) -> Iter<Option<Arc<Cell>>> {
        Iter::new([x.clone(), x])
    }
    /// Bounce an optional object off the consumer.
    #[weaveffi::export]
    pub fn bounce_opt_object(
        m: Arc<dyn Mirror>,
        x: Option<Arc<Cell>>,
    ) -> Result<Option<Arc<Cell>>, ForeignError> {
        m.opt_object(x)
    }
}

/// A hex-encoded `u32`, a newtype crossing as a `string`.
#[weaveffi::custom(repr = String, lift = parse_hex, lower = to_hex)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Hex(pub u32);

fn parse_hex(text: String) -> Result<Hex, std::num::ParseIntError> {
    u32::from_str_radix(&text, 16).map(Hex)
}

fn to_hex(value: &Hex) -> String {
    format!("{:x}", value.0)
}

/// A foreign type crossing as a `string`: a custom type may be an alias of
/// another crate's type.
#[weaveffi::custom(repr = String, lift = parse_addr, lower = ToString::to_string)]
pub type Addr = std::net::Ipv4Addr;

fn parse_addr(text: String) -> Result<Addr, std::net::AddrParseError> {
    text.parse()
}

/// The families' edges.
pub mod edges {
    use std::sync::Arc;

    use weaveffi::{ForeignError, Iter};

    use super::family::{Cell, Reading};
    use super::{Addr, Hex};

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
        /// Samples, scaled by the elements of `scale` (a borrowed array).
        fn samples(&self, scale: &[f32]) -> Result<Vec<f32>, ForeignError>;
        /// A one-character tag.
        fn tag(&self) -> Result<char, ForeignError>;
        /// A notification with no return.
        fn ping(&self, count: usize) -> Result<(), ForeignError>;
        /// Describe an optional borrowed label and a borrowed record.
        fn note(&self, label: Option<&str>, reading: &Reading) -> Result<String, ForeignError>;
    }

    /// Sizes doubled (`usize` crosses as `u64`, in a typed array too).
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
        Hex(h.0 + 1)
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
            others: vec![Hex(id.0 + 1), Hex(id.0 + 2)],
            label,
        }
    }

    /// The sum of a tagged record's ids.
    #[weaveffi::export]
    pub fn total(t: &Tagged) -> u32 {
        t.id.0 + t.others.iter().map(|h| h.0).sum::<u32>()
    }

    /// The address after `a`.
    #[weaveffi::export]
    pub fn next_addr(a: Addr) -> Addr {
        Addr::from(u32::from(a) + 1)
    }

    /// `n` arrays of `0..i`, pulled lazily.
    #[weaveffi::export]
    pub fn ramps(n: u16) -> Iter<Vec<u16>> {
        Iter::new((0..n).map(|i| (0..i).collect()))
    }

    /// Alternating flags with a gap, pulled lazily.
    #[weaveffi::export]
    pub fn flags(n: u8) -> Iter<Option<bool>> {
        Iter::new((0..n).map(|i| (i % 3 != 2).then_some(i % 2 == 0)))
    }

    /// Describe every borrowed parameter shape.
    #[weaveffi::export]
    #[allow(clippy::too_many_arguments)]
    pub fn borrowed(
        a: &str,
        b: &[u8],
        c: &[f32],
        d: &Reading,
        e: Option<&str>,
        f: &Cell,
        g: Option<&Cell>,
        h: Option<&Reading>,
    ) -> String {
        format!(
            "{a} {b:?} {c:?} {}:{:?} {e:?} {} {:?} {:?}",
            d.label,
            d.value,
            f.get(),
            g.map(Cell::get),
            h.map(|r| &r.label)
        )
    }

    /// The borrowed shapes an async function owns copies of.
    #[weaveffi::export]
    pub async fn borrowed_later(a: &str, c: &[f32], d: &Reading, f: &Cell) -> String {
        format!("{a} {c:?} {} {}", d.label, f.get())
    }

    /// Exercise every method of `probe` and describe the results.
    #[weaveffi::export]
    pub fn survey(probe: Arc<dyn Probe>) -> Result<String, ProbeError> {
        let first = probe.reading(None)?;
        let second = probe.reading(first)?;
        let samples = probe.samples(&[1.0, 2.0])?;
        let tag = probe.tag()?;
        probe.ping(3)?;
        let reading = Reading {
            label: "r".to_string(),
            value: Some(1),
        };
        let noted = probe.note(Some("lbl"), &reading)?;
        let unnoted = probe.note(None, &reading)?;
        Ok(format!(
            "{first:?} {second:?} {samples:?} {tag} {noted} {unnoted}"
        ))
    }

    /// Call `ping`, then `reading`, from another thread and report the
    /// outcome.
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

/// Exports generated by `macro_rules!`, whose `$t:ty` fragments reach the
/// attribute as invisible groups around the type.
pub mod generated {
    macro_rules! echo {
        ($name:ident, $param:ty, $ret:ty, $body:expr) => {
            /// A generated export.
            #[weaveffi::export]
            pub fn $name(x: $param) -> $ret {
                ($body)(x)
            }
        };
    }

    echo!(gen_len, &str, usize, |x: &str| x.len());
    echo!(gen_first, Option<&[u8]>, Option<u8>, |x: Option<&[u8]>| x
        .and_then(|b| b.first().copied()));
    echo!(gen_iter, u8, weaveffi::Iter<u8>, |x: u8| {
        weaveffi::Iter::new(0..x)
    });

    macro_rules! later {
        ($name:ident, $token:ty) => {
            /// A generated cancellable export.
            #[weaveffi::export]
            pub async fn $name(x: i32, cancel: $token) -> i32 {
                let _ = cancel;
                x
            }
        };
    }

    later!(gen_later, weaveffi::CancelToken);
}

use family::Cell;

type Done<R> = unsafe extern "C" fn(*mut c_void, *mut FfiError, R);

unsafe extern "C" {
    fn abi6_alloc(len: usize) -> *mut u8;
    fn abi6_run_free(run: Run);
    fn abi6_error_set(err: *mut FfiError, code: i32, ptr: *const u8, len: usize);
    fn abi6_error_set_payload(err: *mut FfiError, ptr: *const u8, len: usize);
    fn abi6_error_clear(err: *mut FfiError);
    fn abi6_error_free(err: *mut FfiError);
    fn abi6_debug_live(kind: i32) -> u64;

    fn abi6_Cell_new(value: i64, err: *mut FfiError) -> *const Cell;
    fn abi6_Cell_get(this: *const Cell, err: *mut FfiError) -> i64;
    fn abi6_Cell_destroy(this: *mut Cell);

    fn abi6_echo_scalar(x: i64, err: *mut FfiError) -> i64;
    fn abi6_later_scalar(x: i64, callback: Option<Done<i64>>, context: *mut c_void);
    fn abi6_iter_scalar(x: i64, err: *mut FfiError) -> *mut IterHandle<i64>;
    fn abi6_IterScalarIterator_next(
        it: *mut IterHandle<i64>,
        out: *mut i64,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterScalarIterator_destroy(it: *mut IterHandle<i64>);
    fn abi6_bounce_scalar(m: CallbackArg<MirrorVtable>, x: i64, err: *mut FfiError) -> i64;

    fn abi6_echo_enum(x: i32, err: *mut FfiError) -> i32;
    fn abi6_later_enum(x: i32, callback: Option<Done<i32>>, context: *mut c_void);
    fn abi6_iter_enum(x: i32, err: *mut FfiError) -> *mut IterHandle<i32>;
    fn abi6_IterEnumIterator_next(
        it: *mut IterHandle<i32>,
        out: *mut i32,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterEnumIterator_destroy(it: *mut IterHandle<i32>);
    fn abi6_bounce_enum(m: CallbackArg<MirrorVtable>, x: i32, err: *mut FfiError) -> i32;

    fn abi6_echo_opt_scalar(x: Opt<f64>, err: *mut FfiError) -> Opt<f64>;
    fn abi6_later_opt_scalar(x: Opt<f64>, callback: Option<Done<Opt<f64>>>, context: *mut c_void);
    fn abi6_iter_opt_scalar(x: Opt<f64>, err: *mut FfiError) -> *mut IterHandle<Opt<f64>>;
    fn abi6_IterOptScalarIterator_next(
        it: *mut IterHandle<Opt<f64>>,
        out: *mut Opt<f64>,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterOptScalarIterator_destroy(it: *mut IterHandle<Opt<f64>>);
    fn abi6_bounce_opt_scalar(
        m: CallbackArg<MirrorVtable>,
        x: Opt<f64>,
        err: *mut FfiError,
    ) -> Opt<f64>;

    fn abi6_echo_opt_enum(x: Opt<i32>, err: *mut FfiError) -> Opt<i32>;
    fn abi6_later_opt_enum(x: Opt<i32>, callback: Option<Done<Opt<i32>>>, context: *mut c_void);
    fn abi6_iter_opt_enum(x: Opt<i32>, err: *mut FfiError) -> *mut IterHandle<Opt<i32>>;
    fn abi6_IterOptEnumIterator_next(
        it: *mut IterHandle<Opt<i32>>,
        out: *mut Opt<i32>,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterOptEnumIterator_destroy(it: *mut IterHandle<Opt<i32>>);
    fn abi6_bounce_opt_enum(
        m: CallbackArg<MirrorVtable>,
        x: Opt<i32>,
        err: *mut FfiError,
    ) -> Opt<i32>;

    fn abi6_echo_text(x: Run, err: *mut FfiError) -> Run;
    fn abi6_later_text(x: Run, callback: Option<Done<Run>>, context: *mut c_void);
    fn abi6_iter_text(x: Run, err: *mut FfiError) -> *mut IterHandle<Run>;
    fn abi6_IterTextIterator_next(
        it: *mut IterHandle<Run>,
        out: *mut Run,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterTextIterator_destroy(it: *mut IterHandle<Run>);
    fn abi6_bounce_text(m: CallbackArg<MirrorVtable>, x: Run, err: *mut FfiError) -> Run;

    fn abi6_echo_bytes(x: Run, err: *mut FfiError) -> Run;
    fn abi6_later_bytes(x: Run, callback: Option<Done<Run>>, context: *mut c_void);
    fn abi6_iter_bytes(x: Run, err: *mut FfiError) -> *mut IterHandle<Run>;
    fn abi6_IterBytesIterator_next(
        it: *mut IterHandle<Run>,
        out: *mut Run,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterBytesIterator_destroy(it: *mut IterHandle<Run>);
    fn abi6_bounce_bytes(m: CallbackArg<MirrorVtable>, x: Run, err: *mut FfiError) -> Run;

    fn abi6_echo_typed(x: Run, err: *mut FfiError) -> Run;
    fn abi6_later_typed(x: Run, callback: Option<Done<Run>>, context: *mut c_void);
    fn abi6_iter_typed(x: Run, err: *mut FfiError) -> *mut IterHandle<Run>;
    fn abi6_IterTypedIterator_next(
        it: *mut IterHandle<Run>,
        out: *mut Run,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterTypedIterator_destroy(it: *mut IterHandle<Run>);
    fn abi6_bounce_typed(m: CallbackArg<MirrorVtable>, x: Run, err: *mut FfiError) -> Run;

    fn abi6_echo_buffer(x: Run, err: *mut FfiError) -> Run;
    fn abi6_later_buffer(x: Run, callback: Option<Done<Run>>, context: *mut c_void);
    fn abi6_iter_buffer(x: Run, err: *mut FfiError) -> *mut IterHandle<Run>;
    fn abi6_IterBufferIterator_next(
        it: *mut IterHandle<Run>,
        out: *mut Run,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterBufferIterator_destroy(it: *mut IterHandle<Run>);
    fn abi6_bounce_buffer(m: CallbackArg<MirrorVtable>, x: Run, err: *mut FfiError) -> Run;

    fn abi6_echo_object(x: *const Cell, err: *mut FfiError) -> *const Cell;
    fn abi6_later_object(x: *const Cell, callback: Option<Done<*const Cell>>, context: *mut c_void);
    fn abi6_iter_object(x: *const Cell, err: *mut FfiError) -> *mut IterHandle<*const Cell>;
    fn abi6_IterObjectIterator_next(
        it: *mut IterHandle<*const Cell>,
        out: *mut *const Cell,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterObjectIterator_destroy(it: *mut IterHandle<*const Cell>);
    fn abi6_bounce_object(
        m: CallbackArg<MirrorVtable>,
        x: *const Cell,
        err: *mut FfiError,
    ) -> *const Cell;

    fn abi6_echo_opt_object(x: *const Cell, err: *mut FfiError) -> *const Cell;
    fn abi6_later_opt_object(
        x: *const Cell,
        callback: Option<Done<*const Cell>>,
        context: *mut c_void,
    );
    fn abi6_iter_opt_object(x: *const Cell, err: *mut FfiError) -> *mut IterHandle<*const Cell>;
    fn abi6_IterOptObjectIterator_next(
        it: *mut IterHandle<*const Cell>,
        out: *mut *const Cell,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_IterOptObjectIterator_destroy(it: *mut IterHandle<*const Cell>);
    fn abi6_bounce_opt_object(
        m: CallbackArg<MirrorVtable>,
        x: *const Cell,
        err: *mut FfiError,
    ) -> *const Cell;
}

// ── Consumer helpers ────────────────────────────────────────────────────

fn message(err: &FfiError) -> String {
    unsafe { err.message_str() }.unwrap_or_default().to_string()
}

fn take_failure(err: &mut FfiError) -> (i32, String) {
    let out = (err.code, message(err));
    unsafe { abi6_error_clear(err) };
    out
}

fn text(s: &str) -> Run {
    Run::borrowing(s.as_bytes())
}

fn array<T: Copy>(values: &[T]) -> Run {
    Run::borrowing(unsafe {
        std::slice::from_raw_parts(values.as_ptr().cast(), std::mem::size_of_val(values))
    })
}

/// Copy a returned run's bytes and release it, checking that it's 8-aligned.
fn take_bytes(run: Run) -> Vec<u8> {
    if run.len == 0 {
        assert!(run.ptr.is_null(), "an empty run is null");
        return Vec::new();
    }
    assert_eq!(run.ptr as usize % 8, 0, "every run is 8-aligned");
    let out = unsafe { std::slice::from_raw_parts(run.ptr, run.len) }.to_vec();
    unsafe { abi6_run_free(run) };
    out
}

fn take_string(run: Run) -> String {
    String::from_utf8(take_bytes(run)).unwrap()
}

/// Copy a returned typed array, read in place, and release it.
fn take_array<T: Copy>(run: Run) -> Vec<T> {
    let out = unsafe { run.slice::<T>() }
        .expect("an aligned typed array")
        .to_vec();
    if !run.ptr.is_null() {
        unsafe { abi6_run_free(run) };
    }
    out
}

/// A run the consumer allocates with `{prefix}_alloc` for the producer to
/// adopt.
fn give(bytes: &[u8]) -> Run {
    if bytes.is_empty() {
        return Run::EMPTY;
    }
    let ptr = unsafe { abi6_alloc(bytes.len()) };
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len()) };
    Run {
        ptr,
        len: bytes.len(),
    }
}

/// A `Reading` encoded by hand, as the value-buffer spec lays it out: the
/// label as a `u32` byte length and UTF-8, then the optional value as a
/// presence byte and a little-endian `i64`.
fn encode_reading(label: &str, value: Option<i64>) -> Vec<u8> {
    let mut out = (label.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(label.as_bytes());
    match value {
        Some(v) => {
            out.push(1);
            out.extend_from_slice(&v.to_le_bytes());
        }
        None => out.push(0),
    }
    out
}

fn decode_reading(bytes: &[u8]) -> (String, Option<i64>) {
    let len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    let label = String::from_utf8(bytes[4..4 + len].to_vec()).unwrap();
    let rest = &bytes[4 + len..];
    let value = match rest[0] {
        0 => {
            assert_eq!(rest.len(), 1);
            None
        }
        1 => {
            assert_eq!(rest.len(), 9);
            Some(i64::from_le_bytes(rest[1..9].try_into().unwrap()))
        }
        other => panic!("bad presence byte {other}"),
    };
    (label, value)
}

fn cell(value: i64) -> *const Cell {
    let mut err = FfiError::default();
    let c = unsafe { abi6_Cell_new(value, &mut err) };
    assert_eq!(err.code, 0);
    c
}

/// Read a cell's value and release the reference.
fn take_cell(c: *const Cell) -> i64 {
    assert!(!c.is_null());
    let mut err = FfiError::default();
    let v = unsafe { abi6_Cell_get(c, &mut err) };
    unsafe { abi6_Cell_destroy(c.cast_mut()) };
    v
}

fn assert_no_leaks() {
    // Objects, callbacks, iterators, and runs: everything this binary hands
    // out is released by the end of each test that checks this. The async
    // tests' tasks may still be finishing, so poll briefly.
    for _ in 0..1000 {
        if (0..5).all(|k| unsafe { abi6_debug_live(k) } == 0) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let live: Vec<u64> = (0..5).map(|k| unsafe { abi6_debug_live(k) }).collect();
    panic!("still live: {live:?}");
}

/// A value moved to the completion's thread.
struct Sent<R>(i32, R);

// SAFETY: the C value is owned by the consumer from the moment the
// completion receives it.
unsafe impl<R> Send for Sent<R> {}

/// The completion for any result type: its context is a boxed sender.
unsafe extern "C" fn done<R: Copy>(ctx: *mut c_void, err: *mut FfiError, result: R) {
    let tx = unsafe { &*ctx.cast::<mpsc::Sender<Sent<R>>>() }.clone();
    let code = if err.is_null() {
        0
    } else {
        let code = unsafe { (*err).code };
        unsafe { abi6_error_free(err) };
        code
    };
    tx.send(Sent(code, result)).unwrap();
}

/// Launch an async call through `launch` and wait for its one completion.
fn complete<R: Copy>(launch: impl FnOnce(Option<Done<R>>, *mut c_void)) -> R {
    let (tx, rx) = mpsc::channel::<Sent<R>>();
    let ctx: *mut c_void = Box::into_raw(Box::new(tx)).cast();
    launch(Some(done::<R>), ctx);
    let Sent(code, result) = rx.recv_timeout(WAIT).unwrap();
    assert_eq!(code, 0);
    assert!(
        rx.recv_timeout(Duration::from_millis(20)).is_err(),
        "exactly once"
    );
    drop(unsafe { Box::from_raw(ctx.cast::<mpsc::Sender<Sent<R>>>()) });
    result
}

/// Drain an iterator through its `_next`, then destroy it.
fn drain<T: Copy>(
    it: *mut IterHandle<T>,
    next: unsafe extern "C" fn(*mut IterHandle<T>, *mut T, *mut FfiError) -> i32,
    destroy: unsafe extern "C" fn(*mut IterHandle<T>),
    zero: T,
) -> Vec<T> {
    assert!(!it.is_null());
    let mut err = FfiError::default();
    let mut out = Vec::new();
    loop {
        let mut item = zero;
        match unsafe { next(it, &mut item, &mut err) } {
            1 => out.push(item),
            0 => break,
            other => panic!("_next returned {other}"),
        }
        assert_eq!(err.code, 0);
    }
    assert_eq!(err.code, 0);
    unsafe { destroy(it) };
    out
}

// ── The consumer's `Mirror` ─────────────────────────────────────────────

/// The `Mirror` vtable, as the generated header lays it out.
#[repr(C)]
pub struct MirrorVtable {
    header: VtableHeader,
    scalar: Option<unsafe extern "C" fn(*mut c_void, i64, *mut FfiError) -> i64>,
    enumeration: Option<unsafe extern "C" fn(*mut c_void, i32, *mut FfiError) -> i32>,
    opt_scalar: Option<unsafe extern "C" fn(*mut c_void, Opt<f64>, *mut FfiError) -> Opt<f64>>,
    opt_enum: Option<unsafe extern "C" fn(*mut c_void, Opt<i32>, *mut FfiError) -> Opt<i32>>,
    text: Option<unsafe extern "C" fn(*mut c_void, Run, *mut FfiError) -> Run>,
    bytes: Option<unsafe extern "C" fn(*mut c_void, Run, *mut FfiError) -> Run>,
    typed: Option<unsafe extern "C" fn(*mut c_void, Run, *mut FfiError) -> Run>,
    buffer: Option<unsafe extern "C" fn(*mut c_void, Run, *mut FfiError) -> Run>,
    object: Option<unsafe extern "C" fn(*mut c_void, *const Cell, *mut FfiError) -> *const Cell>,
    opt_object:
        Option<unsafe extern "C" fn(*mut c_void, *const Cell, *mut FfiError) -> *const Cell>,
}

/// Each method transforms its argument, so the test sees that the consumer
/// read the argument's C value and the producer read the return's.
mod mirror {
    use super::*;

    /// The enum's next variant, by discriminant.
    pub fn next_mode(m: i32) -> i32 {
        match m {
            0 => 1,
            1 => 5,
            5 => 0,
            other => panic!("not a Mode: {other}"),
        }
    }

    unsafe extern "C" fn scalar(_: *mut c_void, x: i64, _: *mut FfiError) -> i64 {
        x * 2
    }

    unsafe extern "C" fn enumeration(_: *mut c_void, x: i32, _: *mut FfiError) -> i32 {
        next_mode(x)
    }

    unsafe extern "C" fn opt_scalar(_: *mut c_void, x: Opt<f64>, _: *mut FfiError) -> Opt<f64> {
        match x.has {
            1 => Opt::some(x.value * 2.0),
            0 => {
                assert_eq!(x.value, 0.0, "an absent value is 0");
                Opt { has: 0, value: 0.0 }
            }
            other => panic!("bad has {other}"),
        }
    }

    unsafe extern "C" fn opt_enum(_: *mut c_void, x: Opt<i32>, _: *mut FfiError) -> Opt<i32> {
        match x.has {
            1 => Opt::some(next_mode(x.value)),
            _ => Opt { has: 0, value: 0 },
        }
    }

    unsafe extern "C" fn text(_: *mut c_void, x: Run, _: *mut FfiError) -> Run {
        // Arguments are lent for the call: read, never free.
        give(unsafe { x.text() }.unwrap().to_uppercase().as_bytes())
    }

    unsafe extern "C" fn bytes(_: *mut c_void, x: Run, _: *mut FfiError) -> Run {
        let mut b = unsafe { x.bytes() }.unwrap().to_vec();
        b.reverse();
        give(&b)
    }

    unsafe extern "C" fn typed(_: *mut c_void, x: Run, _: *mut FfiError) -> Run {
        assert_eq!(x.len % 4, 0, "a typed array's length counts bytes");
        let scaled: Vec<f32> = unsafe { x.slice::<f32>() }
            .unwrap()
            .iter()
            .map(|v| v * 10.0)
            .collect();
        give(unsafe { std::slice::from_raw_parts(scaled.as_ptr().cast(), scaled.len() * 4) })
    }

    unsafe extern "C" fn buffer(_: *mut c_void, x: Run, _: *mut FfiError) -> Run {
        let (label, value) = decode_reading(unsafe { x.bytes() }.unwrap());
        give(&encode_reading(&format!("{label}!"), value.map(|v| v + 1)))
    }

    unsafe extern "C" fn object(_: *mut c_void, x: *const Cell, _: *mut FfiError) -> *const Cell {
        // The argument moved a reference to the consumer; the return moves
        // a fresh one back.
        cell(take_cell(x) + 1)
    }

    unsafe extern "C" fn opt_object(
        _: *mut c_void,
        x: *const Cell,
        _: *mut FfiError,
    ) -> *const Cell {
        if x.is_null() {
            std::ptr::null()
        } else {
            cell(take_cell(x) + 1)
        }
    }

    pub static VTABLE: MirrorVtable = MirrorVtable {
        header: VtableHeader {
            size: std::mem::size_of::<MirrorVtable>() as u32,
            flags: 0,
            free: None,
        },
        scalar: Some(scalar),
        enumeration: Some(enumeration),
        opt_scalar: Some(opt_scalar),
        opt_enum: Some(opt_enum),
        text: Some(text),
        bytes: Some(bytes),
        typed: Some(typed),
        buffer: Some(buffer),
        object: Some(object),
        opt_object: Some(opt_object),
    };

    pub fn arg() -> CallbackArg<MirrorVtable> {
        CallbackArg {
            ctx: std::ptr::null_mut(),
            vtable: &VTABLE,
        }
    }
}

// ── The matrix ──────────────────────────────────────────────────────────

#[test]
fn scalars_are_the_scalar_in_every_position() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { abi6_echo_scalar(-7, &mut err) }, -7);
    assert_eq!(
        complete(|cb, ctx| unsafe { abi6_later_scalar(i64::MIN, cb, ctx) }),
        i64::MIN
    );
    let it = unsafe { abi6_iter_scalar(9, &mut err) };
    assert_eq!(
        drain(
            it,
            abi6_IterScalarIterator_next,
            abi6_IterScalarIterator_destroy,
            0
        ),
        [9, 9]
    );
    assert_eq!(
        unsafe { abi6_bounce_scalar(mirror::arg(), 21, &mut err) },
        42
    );
    assert_eq!(err.code, 0);
}

#[test]
fn enums_are_their_discriminant_in_every_position() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { abi6_echo_enum(5, &mut err) }, 5);
    assert_eq!(
        complete(|cb, ctx| unsafe { abi6_later_enum(1, cb, ctx) }),
        1
    );
    let it = unsafe { abi6_iter_enum(5, &mut err) };
    assert_eq!(
        drain(
            it,
            abi6_IterEnumIterator_next,
            abi6_IterEnumIterator_destroy,
            -1
        ),
        [5, 5]
    );
    assert_eq!(unsafe { abi6_bounce_enum(mirror::arg(), 1, &mut err) }, 5);
    assert_eq!(err.code, 0);
    // A discriminant in the gap isn't a variant.
    assert_eq!(unsafe { abi6_echo_enum(3, &mut err) }, 0);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn optional_scalars_are_opt_structs_in_every_position() {
    let mut err = FfiError::default();
    let some = Opt::some(1.25f64);
    let none = Opt { has: 0, value: 0.0 };
    let r = unsafe { abi6_echo_opt_scalar(some, &mut err) };
    assert_eq!((r.has, r.value), (1, 1.25));
    let r = unsafe { abi6_echo_opt_scalar(none, &mut err) };
    assert_eq!((r.has, r.value), (0, 0.0));
    // An absent value's `value` is ignored on the way in.
    let r = unsafe { abi6_echo_opt_scalar(Opt { has: 0, value: 9.0 }, &mut err) };
    assert_eq!((r.has, r.value), (0, 0.0), "and 0 on the way out");

    let r = complete(|cb, ctx| unsafe { abi6_later_opt_scalar(some, cb, ctx) });
    assert_eq!((r.has, r.value), (1, 1.25));
    let r = complete(|cb, ctx| unsafe { abi6_later_opt_scalar(none, cb, ctx) });
    assert_eq!(r.has, 0);

    let it = unsafe { abi6_iter_opt_scalar(some, &mut err) };
    let items = drain(
        it,
        abi6_IterOptScalarIterator_next,
        abi6_IterOptScalarIterator_destroy,
        none,
    );
    assert_eq!(items, [some, some]);
    let it = unsafe { abi6_iter_opt_scalar(none, &mut err) };
    let items = drain(
        it,
        abi6_IterOptScalarIterator_next,
        abi6_IterOptScalarIterator_destroy,
        some,
    );
    assert_eq!(items, [none, none]);

    let r = unsafe { abi6_bounce_opt_scalar(mirror::arg(), some, &mut err) };
    assert_eq!((r.has, r.value), (1, 2.5));
    let r = unsafe { abi6_bounce_opt_scalar(mirror::arg(), none, &mut err) };
    assert_eq!(r.has, 0);
    assert_eq!(err.code, 0);
}

#[test]
fn optional_enums_are_opt_i32_in_every_position() {
    let mut err = FfiError::default();
    let some = Opt::some(1i32);
    let none = Opt { has: 0, value: 0 };
    assert_eq!(unsafe { abi6_echo_opt_enum(some, &mut err) }, some);
    assert_eq!(unsafe { abi6_echo_opt_enum(none, &mut err) }, none);
    assert_eq!(
        complete(|cb, ctx| unsafe { abi6_later_opt_enum(some, cb, ctx) }),
        some
    );
    let it = unsafe { abi6_iter_opt_enum(some, &mut err) };
    assert_eq!(
        drain(
            it,
            abi6_IterOptEnumIterator_next,
            abi6_IterOptEnumIterator_destroy,
            none
        ),
        [some, some]
    );
    assert_eq!(
        unsafe { abi6_bounce_opt_enum(mirror::arg(), some, &mut err) },
        Opt::some(5)
    );
    assert_eq!(
        unsafe { abi6_bounce_opt_enum(mirror::arg(), none, &mut err) },
        none
    );
    assert_eq!(err.code, 0);
    // A present value must be a variant.
    assert_eq!(unsafe { abi6_echo_opt_enum(Opt::some(2), &mut err) }, none);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn strings_are_one_run_in_every_position() {
    let mut err = FfiError::default();
    let s = "héllo\0wörld";
    assert_eq!(take_string(unsafe { abi6_echo_text(text(s), &mut err) }), s);
    assert_eq!(unsafe { abi6_echo_text(Run::EMPTY, &mut err) }, Run::EMPTY);
    assert_eq!(
        take_string(complete(|cb, ctx| unsafe {
            abi6_later_text(text(s), cb, ctx)
        })),
        s
    );
    let it = unsafe { abi6_iter_text(text(s), &mut err) };
    let items = drain(
        it,
        abi6_IterTextIterator_next,
        abi6_IterTextIterator_destroy,
        Run::EMPTY,
    );
    assert_eq!(
        items.into_iter().map(take_string).collect::<Vec<_>>(),
        [s, s]
    );
    assert_eq!(
        take_string(unsafe { abi6_bounce_text(mirror::arg(), text(s), &mut err) }),
        "HÉLLO\0WÖRLD"
    );
    assert_eq!(err.code, 0);
    assert_no_leaks();
}

#[test]
fn bytes_are_one_run_in_every_position() {
    let mut err = FfiError::default();
    let b = [0u8, 1, 2, 255];
    assert_eq!(
        take_bytes(unsafe { abi6_echo_bytes(Run::borrowing(&b), &mut err) }),
        b
    );
    assert_eq!(
        take_bytes(complete(|cb, ctx| unsafe {
            abi6_later_bytes(Run::borrowing(&b), cb, ctx)
        })),
        b
    );
    let it = unsafe { abi6_iter_bytes(Run::borrowing(&b), &mut err) };
    let items = drain(
        it,
        abi6_IterBytesIterator_next,
        abi6_IterBytesIterator_destroy,
        Run::EMPTY,
    );
    assert_eq!(
        items.into_iter().map(take_bytes).collect::<Vec<_>>(),
        [b, b]
    );
    assert_eq!(
        take_bytes(unsafe { abi6_bounce_bytes(mirror::arg(), Run::borrowing(&b), &mut err) }),
        [255, 2, 1, 0]
    );
    // Empty bytes are the empty run in both directions.
    assert_eq!(
        unsafe { abi6_bounce_bytes(mirror::arg(), Run::EMPTY, &mut err) },
        Run::EMPTY
    );
    assert_eq!(err.code, 0);
    assert_no_leaks();
}

#[test]
fn typed_arrays_are_one_run_of_packed_elements_in_every_position() {
    let mut err = FfiError::default();
    let xs = [1.5f32, -2.0, 0.25];
    let r = unsafe { abi6_echo_typed(array(&xs), &mut err) };
    assert_eq!(r.len, 12, "the length counts bytes");
    assert_eq!(take_array::<f32>(r), xs);
    assert_eq!(
        take_array::<f32>(complete(|cb, ctx| unsafe {
            abi6_later_typed(array(&xs), cb, ctx)
        })),
        xs
    );
    let it = unsafe { abi6_iter_typed(array(&xs), &mut err) };
    let items = drain(
        it,
        abi6_IterTypedIterator_next,
        abi6_IterTypedIterator_destroy,
        Run::EMPTY,
    );
    assert_eq!(
        items.into_iter().map(take_array::<f32>).collect::<Vec<_>>(),
        [xs, xs]
    );
    assert_eq!(
        take_array::<f32>(unsafe { abi6_bounce_typed(mirror::arg(), array(&xs), &mut err) }),
        [15.0, -20.0, 2.5]
    );
    assert_eq!(err.code, 0);
    assert_no_leaks();
}

#[test]
fn value_buffers_are_one_run_in_every_position() {
    let mut err = FfiError::default();
    let some = encode_reading("t", Some(-3));
    let none = encode_reading("", None);
    for bytes in [&some, &none] {
        assert_eq!(
            &take_bytes(unsafe { abi6_echo_buffer(Run::borrowing(bytes), &mut err) }),
            bytes
        );
    }
    assert_eq!(
        take_bytes(complete(|cb, ctx| unsafe {
            abi6_later_buffer(Run::borrowing(&some), cb, ctx)
        })),
        some
    );
    let it = unsafe { abi6_iter_buffer(Run::borrowing(&some), &mut err) };
    let items = drain(
        it,
        abi6_IterBufferIterator_next,
        abi6_IterBufferIterator_destroy,
        Run::EMPTY,
    );
    assert_eq!(
        items.into_iter().map(take_bytes).collect::<Vec<_>>(),
        [some.clone(), some.clone()]
    );
    let back =
        take_bytes(unsafe { abi6_bounce_buffer(mirror::arg(), Run::borrowing(&some), &mut err) });
    assert_eq!(decode_reading(&back), ("t!".to_string(), Some(-2)));
    assert_eq!(err.code, 0);
    assert_no_leaks();
}

#[test]
fn objects_are_one_pointer_in_every_position() {
    let mut err = FfiError::default();
    let c = cell(10);

    // A parameter borrows; a return moves one reference to the consumer.
    let r = unsafe { abi6_echo_object(c, &mut err) };
    assert_eq!(r, c, "the same object");
    assert_eq!(take_cell(r), 10);
    let r = complete(|cb, ctx| unsafe { abi6_later_object(c, cb, ctx) });
    assert_eq!(take_cell(r), 10);
    let it = unsafe { abi6_iter_object(c, &mut err) };
    let items = drain(
        it,
        abi6_IterObjectIterator_next,
        abi6_IterObjectIterator_destroy,
        std::ptr::null(),
    );
    assert_eq!(
        items.into_iter().map(take_cell).collect::<Vec<_>>(),
        [10, 10]
    );
    // The consumer's method gets a reference and returns a fresh object.
    let r = unsafe { abi6_bounce_object(mirror::arg(), c, &mut err) };
    assert_ne!(r, c);
    assert_eq!(take_cell(r), 11);
    assert_eq!(err.code, 0);

    // A null required object is a marshalling failure.
    assert!(unsafe { abi6_echo_object(std::ptr::null(), &mut err) }.is_null());
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);

    assert_eq!(take_cell(c), 10);
    assert_no_leaks();
}

#[test]
fn optional_objects_are_a_nullable_pointer_in_every_position() {
    let mut err = FfiError::default();
    let null = std::ptr::null::<Cell>();
    let c = cell(3);
    assert_eq!(take_cell(unsafe { abi6_echo_opt_object(c, &mut err) }), 3);
    assert!(unsafe { abi6_echo_opt_object(null, &mut err) }.is_null());
    assert_eq!(err.code, 0, "null is none, not an error");
    assert_eq!(
        take_cell(complete(|cb, ctx| unsafe {
            abi6_later_opt_object(c, cb, ctx)
        })),
        3
    );
    assert!(complete(|cb, ctx| unsafe { abi6_later_opt_object(null, cb, ctx) }).is_null());
    let it = unsafe { abi6_iter_opt_object(null, &mut err) };
    let items = drain(
        it,
        abi6_IterOptObjectIterator_next,
        abi6_IterOptObjectIterator_destroy,
        c,
    );
    assert_eq!(items, [null, null]);
    let it = unsafe { abi6_iter_opt_object(c, &mut err) };
    let items = drain(
        it,
        abi6_IterOptObjectIterator_next,
        abi6_IterOptObjectIterator_destroy,
        null,
    );
    assert_eq!(items.into_iter().map(take_cell).collect::<Vec<_>>(), [3, 3]);
    assert_eq!(
        take_cell(unsafe { abi6_bounce_opt_object(mirror::arg(), c, &mut err) }),
        4
    );
    assert!(unsafe { abi6_bounce_opt_object(mirror::arg(), null, &mut err) }.is_null());
    assert_eq!(err.code, 0);
    assert_eq!(take_cell(c), 3);
    assert_no_leaks();
}

// ── The edges ───────────────────────────────────────────────────────────

/// The consumer's `Probe` vtable.
#[repr(C)]
pub struct ProbeVtable {
    header: VtableHeader,
    reading: Option<unsafe extern "C" fn(*mut c_void, Opt<f64>, *mut FfiError) -> Opt<f64>>,
    samples: Option<unsafe extern "C" fn(*mut c_void, Run, *mut FfiError) -> Run>,
    tag: Option<unsafe extern "C" fn(*mut c_void, *mut FfiError) -> Run>,
    ping: Option<unsafe extern "C" fn(*mut c_void, u64, *mut FfiError)>,
    note: Option<unsafe extern "C" fn(*mut c_void, Run, Run, *mut FfiError) -> Run>,
}

unsafe extern "C" {
    fn abi6_doubled(sizes: Run, err: *mut FfiError) -> Run;
    fn abi6_count(n: u64, err: *mut FfiError) -> u64;
    fn abi6_offset(n: i64, err: *mut FfiError) -> i64;
    fn abi6_initial(name: Run, err: *mut FfiError) -> Run;
    fn abi6_repeat(c: Run, n: u8, err: *mut FfiError) -> Run;
    fn abi6_bump(h: Run, err: *mut FfiError) -> Run;
    fn abi6_largest(hs: Run, err: *mut FfiError) -> Run;
    fn abi6_tagged(id: Run, label: Run, err: *mut FfiError) -> Run;
    fn abi6_total(t: Run, err: *mut FfiError) -> u32;
    fn abi6_next_addr(a: Run, err: *mut FfiError) -> Run;
    fn abi6_ramps(n: u16, err: *mut FfiError) -> *mut IterHandle<Run>;
    fn abi6_RampsIterator_next(it: *mut IterHandle<Run>, out: *mut Run, err: *mut FfiError) -> i32;
    fn abi6_RampsIterator_destroy(it: *mut IterHandle<Run>);
    fn abi6_flags(n: u8, err: *mut FfiError) -> *mut IterHandle<Opt<u8>>;
    fn abi6_FlagsIterator_next(
        it: *mut IterHandle<Opt<u8>>,
        out: *mut Opt<u8>,
        err: *mut FfiError,
    ) -> i32;
    fn abi6_FlagsIterator_destroy(it: *mut IterHandle<Opt<u8>>);
    #[allow(clippy::too_many_arguments)]
    fn abi6_borrowed(
        a: Run,
        b: Run,
        c: Run,
        d: Run,
        e: Run,
        f: *const Cell,
        g: *const Cell,
        h: Run,
        err: *mut FfiError,
    ) -> Run;
    fn abi6_borrowed_later(
        a: Run,
        c: Run,
        d: Run,
        f: *const Cell,
        callback: Option<Done<Run>>,
        context: *mut c_void,
    );
    fn abi6_survey(probe: CallbackArg<ProbeVtable>, err: *mut FfiError) -> Run;
    fn abi6_read_elsewhere(probe: CallbackArg<ProbeVtable>, err: *mut FfiError) -> Run;

    fn abi6_gen_len(x: Run, err: *mut FfiError) -> u64;
    fn abi6_gen_first(x: Run, err: *mut FfiError) -> Opt<u8>;
    fn abi6_gen_iter(x: u8, err: *mut FfiError) -> *mut IterHandle<u8>;
    fn abi6_GenIterIterator_next(it: *mut IterHandle<u8>, out: *mut u8, err: *mut FfiError) -> i32;
    fn abi6_GenIterIterator_destroy(it: *mut IterHandle<u8>);
    fn abi6_gen_later(
        x: i32,
        cancel_token: *const rt::FfiCancelToken,
        callback: Option<Done<i32>>,
        context: *mut c_void,
    );
}

/// A string encoded in a value buffer: a `u32` byte length, then UTF-8.
fn buf_string(s: &str) -> Vec<u8> {
    let mut out = (s.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(s.as_bytes());
    out
}

/// A list of strings encoded in a value buffer: a `u32` count, then each.
fn buf_strings(items: &[&str]) -> Vec<u8> {
    let mut out = (items.len() as u32).to_le_bytes().to_vec();
    for s in items {
        out.extend(buf_string(s));
    }
    out
}

#[test]
fn sizes_cross_as_64_bit_integers() {
    let mut err = FfiError::default();
    assert_eq!(unsafe { abi6_count(41, &mut err) }, 42u64);
    assert_eq!(unsafe { abi6_offset(-41, &mut err) }, -42i64);
    let sizes = [1u64, 20, 300];
    let r = unsafe { abi6_doubled(array(&sizes), &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(
        take_array::<u64>(r),
        [2, 40, 600],
        "a `Vec<usize>` is a `[u64]`"
    );
    if usize::BITS < 64 {
        assert_eq!(unsafe { abi6_count(u64::MAX, &mut err) }, 0);
        assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
    }
}

#[test]
fn chars_are_one_character_strings() {
    let mut err = FfiError::default();
    assert_eq!(
        take_string(unsafe { abi6_initial(text("\u{1F980}rab"), &mut err) }),
        "\u{1F980}"
    );
    assert_eq!(
        take_string(unsafe { abi6_repeat(text("é"), 3, &mut err) }),
        "ééé"
    );
    assert_eq!(unsafe { abi6_repeat(text(""), 3, &mut err) }, Run::EMPTY);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn custom_types_cross_as_their_repr() {
    let mut err = FfiError::default();
    assert_eq!(
        take_string(unsafe { abi6_bump(text("ff"), &mut err) }),
        "100"
    );
    // A foreign type works as a custom type, too.
    assert_eq!(
        take_string(unsafe { abi6_next_addr(text("10.0.0.255"), &mut err) }),
        "10.0.1.0"
    );

    // In a value buffer, a custom value is its repr's encoding.
    let hs = buf_strings(&["a", "1f"]);
    let r = take_bytes(unsafe { abi6_largest(Run::borrowing(&hs), &mut err) });
    let mut expected = vec![1u8];
    expected.extend(buf_string("1f"));
    assert_eq!(r, expected, "a present `Hex?`: a flag, then the string");

    let bytes = take_bytes(unsafe { abi6_tagged(text("ff"), text("x"), &mut err) });
    let mut expected = buf_string("ff");
    expected.extend(buf_strings(&["100", "101"]));
    expected.extend(buf_string("x"));
    assert_eq!(bytes, expected);
    assert_eq!(
        unsafe { abi6_total(Run::borrowing(&bytes), &mut err) },
        255 + 256 + 257,
        "and decodes back through `lift`"
    );
    // A buffer whose custom field doesn't lift is malformed.
    let mut bad = buf_string("zz");
    bad.extend(buf_strings(&[]));
    bad.extend(buf_string("x"));
    assert_eq!(unsafe { abi6_total(Run::borrowing(&bad), &mut err) }, 0);
    assert_eq!(take_failure(&mut err).0, rt::MARSHAL_ERROR_CODE);
}

#[test]
fn marshalling_failures_name_the_parameter_and_quote_the_value() {
    let mut err = FfiError::default();
    // A char that isn't one character.
    assert_eq!(unsafe { abi6_repeat(text("ab"), 3, &mut err) }, Run::EMPTY);
    assert_eq!(
        take_failure(&mut err),
        (
            rt::MARSHAL_ERROR_CODE,
            "c: \"ab\" is not a valid char".to_string()
        )
    );
    // A custom type's `lift` failure.
    assert_eq!(unsafe { abi6_bump(text("zz"), &mut err) }, Run::EMPTY);
    assert_eq!(
        take_failure(&mut err),
        (
            rt::MARSHAL_ERROR_CODE,
            "h: invalid digit found in string".to_string()
        )
    );
    // A typed array that isn't aligned for its element type.
    let words = [1u64, 2];
    let misaligned = Run {
        ptr: unsafe { array(&words).ptr.add(4) },
        len: 8,
    };
    assert_eq!(unsafe { abi6_doubled(misaligned, &mut err) }, Run::EMPTY);
    assert_eq!(
        take_failure(&mut err),
        (
            rt::MARSHAL_ERROR_CODE,
            "sizes: the array isn't aligned for its element type".to_string()
        )
    );
    // An enum value that names no variant.
    assert_eq!(unsafe { abi6_echo_enum(3, &mut err) }, 0);
    assert_eq!(
        take_failure(&mut err),
        (
            rt::MARSHAL_ERROR_CODE,
            "x: 3 is not a valid Mode".to_string()
        )
    );
    // A null receiver.
    assert_eq!(unsafe { abi6_Cell_get(std::ptr::null(), &mut err) }, 0);
    assert_eq!(
        take_failure(&mut err),
        (
            rt::MARSHAL_ERROR_CODE,
            "self: an object is null".to_string()
        )
    );
}

#[test]
fn iterator_items_of_lists_and_optional_bools() {
    let mut err = FfiError::default();
    let it = unsafe { abi6_ramps(3, &mut err) };
    let items = drain(
        it,
        abi6_RampsIterator_next,
        abi6_RampsIterator_destroy,
        Run::EMPTY,
    );
    let ramps: Vec<Vec<u16>> = items.into_iter().map(take_array::<u16>).collect();
    assert_eq!(ramps, [vec![], vec![0], vec![0, 1]]);

    let it = unsafe { abi6_flags(4, &mut err) };
    let none = Opt { has: 0, value: 0 };
    let items = drain(
        it,
        abi6_FlagsIterator_next,
        abi6_FlagsIterator_destroy,
        none,
    );
    assert_eq!(items, [Opt::some(1), Opt::some(0), none, Opt::some(0)]);
    assert_no_leaks();
}

#[test]
fn borrowed_parameters_of_every_shape() {
    let mut err = FfiError::default();
    let c = cell(7);
    let reading = encode_reading("r", Some(2));
    let mut some_label = vec![1u8];
    some_label.extend(buf_string("e"));
    let mut some_reading = vec![1u8];
    some_reading.extend(encode_reading("h", None));
    let out = unsafe {
        abi6_borrowed(
            text("a"),
            Run::borrowing(&[1, 2]),
            array(&[0.5f32]),
            Run::borrowing(&reading),
            Run::borrowing(&some_label),
            c,
            c,
            Run::borrowing(&some_reading),
            &mut err,
        )
    };
    assert_eq!(err.code, 0, "{}", message(&err));
    assert_eq!(
        take_string(out),
        r#"a [1, 2] [0.5] r:Some(2) Some("e") 7 Some(7) Some("h")"#
    );

    // The optional borrows' none: a buffer holding a zero flag, and null.
    let out = unsafe {
        abi6_borrowed(
            Run::EMPTY,
            Run::EMPTY,
            Run::EMPTY,
            Run::borrowing(&reading),
            Run::borrowing(&[0]),
            c,
            std::ptr::null(),
            Run::borrowing(&[0]),
            &mut err,
        )
    };
    assert_eq!(take_string(out), " [] [] r:Some(2) None 7 None None");

    // An async call owns copies, so the consumer may release its arguments
    // (and its object reference) as soon as the launcher returns.
    let later = complete(|cb, ctx| unsafe {
        let a = "x".to_string();
        let d = encode_reading("d", None);
        let f = cell(9);
        abi6_borrowed_later(text(&a), array(&[1.0f32]), Run::borrowing(&d), f, cb, ctx);
        abi6_Cell_destroy(f.cast_mut());
    });
    assert_eq!(take_string(later), "x [1.0] d 9");
    assert_eq!(take_cell(c), 7);
    assert_no_leaks();
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
        last: Opt<f64>,
        out_err: *mut FfiError,
    ) -> Opt<f64> {
        let state = unsafe { &*ctx.cast::<State>() };
        if state.fail_reading {
            // A typed failure of the method's own domain, with its payload.
            let msg = "bad";
            let fields = 7i32.to_le_bytes();
            unsafe {
                abi6_error_set(out_err, 1, msg.as_ptr(), msg.len());
                abi6_error_set_payload(out_err, fields.as_ptr(), fields.len());
            }
            return Opt { has: 0, value: 0.0 };
        }
        match last.has {
            0 => Opt::some(0.5),
            _ if last.value > 1.0 => Opt { has: 0, value: 0.0 },
            _ => Opt::some(last.value + 1.0),
        }
    }

    unsafe extern "C" fn samples(_ctx: *mut c_void, scale: Run, _err: *mut FfiError) -> Run {
        let scale = unsafe { scale.slice::<f32>() }.unwrap();
        let out: Vec<f32> = scale.iter().map(|s| s * 10.0).collect();
        give(unsafe { std::slice::from_raw_parts(out.as_ptr().cast(), out.len() * 4) })
    }

    unsafe extern "C" fn tag(_ctx: *mut c_void, _err: *mut FfiError) -> Run {
        give("λ".as_bytes())
    }

    unsafe extern "C" fn ping(ctx: *mut c_void, count: u64, _err: *mut FfiError) {
        let state = unsafe { &*ctx.cast::<State>() };
        state.pings.fetch_add(count as usize, Ordering::SeqCst);
    }

    unsafe extern "C" fn note(
        _ctx: *mut c_void,
        label: Run,
        reading: Run,
        _err: *mut FfiError,
    ) -> Run {
        // `string?` is a value buffer: a presence byte, then the string.
        let label = unsafe { label.bytes() }.unwrap();
        let label = match label[0] {
            0 => "none".to_string(),
            _ => String::from_utf8(label[5..].to_vec()).unwrap(),
        };
        let (r, _) = decode_reading(unsafe { reading.bytes() }.unwrap());
        give(format!("{label}:{r}").as_bytes())
    }

    unsafe extern "C" fn free(ctx: *mut c_void) {
        drop(unsafe { Box::from_raw(ctx.cast::<State>()) });
    }

    pub fn vtable(flags: u32) -> &'static ProbeVtable {
        Box::leak(Box::new(ProbeVtable {
            header: VtableHeader {
                size: std::mem::size_of::<ProbeVtable>() as u32,
                flags,
                free: Some(free),
            },
            reading: Some(reading),
            samples: Some(samples),
            tag: Some(tag),
            ping: Some(ping),
            note: Some(note),
        }))
    }

    pub fn arg(vtable: &'static ProbeVtable, fail_reading: bool) -> CallbackArg<ProbeVtable> {
        CallbackArg {
            ctx: Box::into_raw(Box::new(State {
                fail_reading,
                pings: AtomicUsize::new(0),
            }))
            .cast(),
            vtable,
        }
    }
}

#[test]
fn callback_methods_at_the_edges() {
    let vtable = consumer_probe::vtable(0);
    let mut err = FfiError::default();
    let out = unsafe { abi6_survey(consumer_probe::arg(vtable, false), &mut err) };
    assert_eq!(err.code, 0, "{}", message(&err));
    assert_eq!(
        take_string(out),
        "Some(0.5) Some(1.5) [10.0, 20.0] λ lbl:r none:r"
    );

    // The consumer's typed failure arrives typed and propagates with its
    // code, message, and payload.
    let out = unsafe { abi6_survey(consumer_probe::arg(vtable, true), &mut err) };
    assert_eq!(out, Run::EMPTY);
    assert_eq!(err.code, 1);
    assert_eq!(message(&err), "bad reading 7");
    assert_eq!(err.payload(), 7i32.to_le_bytes());
    unsafe { abi6_error_clear(&mut err) };
    assert_no_leaks();
}

#[test]
fn thread_affine_vtables_reject_value_methods_off_thread() {
    let mut err = FfiError::default();
    let affine = consumer_probe::vtable(rt::VTABLE_THREAD_AFFINE);
    let out = unsafe { abi6_read_elsewhere(consumer_probe::arg(affine, false), &mut err) };
    // `ping` (no return value) still ran on the other thread; `reading`
    // failed without being called, reaching the producer as `-4`.
    assert_eq!(
        take_string(out),
        "Err(Failed { code: -4, message: \"callback called off its thread\" })"
    );
    let free_threaded = consumer_probe::vtable(0);
    let out = unsafe { abi6_read_elsewhere(consumer_probe::arg(free_threaded, false), &mut err) };
    assert_eq!(take_string(out), "Ok(Some(0.5))");
    // On the adopting thread, an affine vtable works normally.
    let out = unsafe { abi6_survey(consumer_probe::arg(affine, false), &mut err) };
    assert_eq!(err.code, 0);
    assert_eq!(
        take_string(out),
        "Some(0.5) Some(1.5) [10.0, 20.0] λ lbl:r none:r"
    );
}

#[test]
fn exports_generated_by_macro_rules_keep_their_shapes() {
    let mut err = FfiError::default();
    // `&str` arrived as a `$t:ty` group: still a borrowed string.
    assert_eq!(unsafe { abi6_gen_len(text("four"), &mut err) }, 4);
    // `Option<&[u8]>`: a borrowed `bytes?`, a value buffer.
    let mut some = vec![1u8];
    some.extend(4u32.to_le_bytes());
    some.extend([9, 8, 7, 6]);
    assert_eq!(
        unsafe { abi6_gen_first(Run::borrowing(&some), &mut err) },
        Opt::some(9)
    );
    assert_eq!(
        unsafe { abi6_gen_first(Run::borrowing(&[0]), &mut err) }.has,
        0
    );
    assert_eq!(err.code, 0);
    // `Iter<u8>`: an iterator.
    let it = unsafe { abi6_gen_iter(3, &mut err) };
    assert_eq!(
        drain(
            it,
            abi6_GenIterIterator_next,
            abi6_GenIterIterator_destroy,
            0
        ),
        [0, 1, 2]
    );
    // `CancelToken`: the cancel token slot.
    assert_eq!(
        complete(|cb, ctx| unsafe { abi6_gen_later(5, std::ptr::null(), cb, ctx) }),
        5
    );
}
