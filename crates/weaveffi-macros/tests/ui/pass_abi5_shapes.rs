//! The revision 5 shapes: optional scalars and numeric lists in every
//! position, `usize`/`isize`/`char`, custom types, several error domains in
//! one module (one opting out of the generated `Display`), `throws any`
//! functions, typed callback errors, and `#[weaveffi::skip]`.
#![deny(unsafe_code)]

use std::sync::Arc;

fn parse_celsius(text: String) -> Result<Celsius, std::num::ParseFloatError> {
    text.parse().map(Celsius)
}

fn format_celsius(c: &Celsius) -> String {
    c.0.to_string()
}

/// A temperature, a newtype declared outside the module.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub struct Celsius(pub f64);

#[weaveffi::module]
mod shapes {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use weaveffi::ForeignError;

    #[weaveffi::custom(repr = String, lift = super::parse_celsius, lower = super::format_celsius)]
    pub type Temp = super::Celsius;

    #[weaveffi::error]
    #[repr(i32)]
    #[derive(Debug)]
    pub enum SensorError {
        /// sensor offline
        Offline = 1,
        #[weaveffi(message = "reading {value:?} out of range")]
        Range { value: Temp } = 2,
        #[weaveffi(message = "consumer failed: {message}")]
        Consumer { message: String } = 3,
    }

    impl From<ForeignError> for SensorError {
        fn from(e: ForeignError) -> Self {
            Self::Consumer { message: e.message }
        }
    }

    /// A second domain in the same module, with a hand-written `Display`.
    #[weaveffi::error(no_display)]
    #[derive(Debug)]
    pub enum CalibrationError {
        /// drifted
        Drifted = 10,
    }

    impl std::fmt::Display for CalibrationError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("drifted")
        }
    }

    impl std::error::Error for CalibrationError {}

    #[weaveffi::enumeration]
    #[repr(i32)]
    pub enum Unit {
        C = 0,
        F = 1,
    }

    #[weaveffi::record]
    pub struct Reading {
        pub at: Temp,
        pub history: Vec<Temp>,
        pub by_name: BTreeMap<String, Option<Temp>>,
        pub count: usize,
        pub grade: char,
    }

    #[weaveffi::callback_interface]
    pub trait Sensor: Send + Sync {
        fn read(&self, unit: Option<Unit>) -> Result<Option<f64>, SensorError>;
        fn burst(&self, sizes: &[u32], since: Option<i64>) -> Result<Vec<f64>, ForeignError>;
        fn temp(&self, hint: Temp) -> Result<Temp, ForeignError>;
        fn grade(&self) -> Result<char, anyhow_like::Error>;
        fn reset(&self) -> Result<(), ForeignError>;
    }

    /// An error type that converts from `ForeignError` but isn't a domain.
    pub mod anyhow_like {
        #[derive(Debug)]
        pub struct Error(pub String);
        impl From<weaveffi::ForeignError> for Error {
            fn from(e: weaveffi::ForeignError) -> Self {
                Self(e.message)
            }
        }
        impl std::fmt::Display for Error {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    }

    #[weaveffi::interface]
    pub struct Station {
        size: usize,
    }

    impl Station {
        pub fn new(size: usize) -> Self {
            Self { size }
        }
        pub fn size(&self) -> usize {
            self.size
        }
        pub fn window(&self, xs: &[f64]) -> Option<f64> {
            xs.iter().copied().reduce(f64::max)
        }
        /// Not exported.
        #[weaveffi::skip]
        pub fn internal(&self) -> Box<dyn Fn()> {
            Box::new(|| {})
        }
    }

    // Constants named like the C slots and like error fields: every thunk
    // parameter and binding is `__wv_*`, so none of them collide.
    #[allow(non_upper_case_globals, dead_code)]
    const out_err: i32 = 1;
    #[allow(non_upper_case_globals, dead_code)]
    const out_len: i32 = 2;
    #[allow(non_upper_case_globals, dead_code)]
    const callback: i32 = 3;
    #[allow(non_upper_case_globals, dead_code)]
    const context: i32 = 4;
    #[allow(non_upper_case_globals, dead_code)]
    const message: i32 = 5;

    #[weaveffi::export]
    pub fn shadowing(base: i32, count: Option<i32>) -> String {
        (base + count.unwrap_or(0)).to_string()
    }

    #[weaveffi::export]
    pub fn hottest(temps: Vec<Temp>) -> Result<Temp, SensorError> {
        temps
            .into_iter()
            .reduce(|a, b| if b > a { b } else { a })
            .ok_or(SensorError::Offline)
    }

    #[weaveffi::export]
    pub fn calibrate(by: Option<Temp>) -> Result<Option<Temp>, CalibrationError> {
        Ok(by)
    }

    #[weaveffi::export]
    pub fn parse(text: &str) -> Result<isize, std::num::ParseIntError> {
        text.parse()
    }

    #[weaveffi::export]
    pub fn describe(text: String) -> Result<char, String> {
        text.chars().next().ok_or_else(|| "empty".to_string())
    }

    #[weaveffi::export]
    pub fn sizes(xs: &[u64], extra: Vec<usize>) -> Vec<usize> {
        xs.iter().map(|x| *x as usize).chain(extra).collect()
    }

    #[weaveffi::export]
    pub async fn later(x: Option<u8>, ys: Vec<i16>) -> Option<i16> {
        ys.into_iter().nth(usize::from(x.unwrap_or(0)))
    }

    #[weaveffi::export]
    pub async fn spread(n: u32) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    #[weaveffi::export]
    pub async fn temp_later(t: Temp) -> Temp {
        t
    }

    #[weaveffi::export]
    pub fn ladders(n: u32) -> weaveffi::Iter<Vec<u32>> {
        weaveffi::Iter::new((0..n).map(|i| (0..i).collect()))
    }

    #[weaveffi::export]
    pub fn maybes(n: u32) -> weaveffi::Iter<Option<Unit>> {
        weaveffi::Iter::new((0..n).map(|i| (i % 2 == 0).then_some(Unit::C)))
    }

    #[weaveffi::export]
    pub fn temps(n: u32) -> weaveffi::Iter<Temp> {
        weaveffi::Iter::new((0..n).map(|i| super::Celsius(f64::from(i))))
    }

    #[weaveffi::export]
    pub fn poll(sensor: Arc<dyn Sensor>) -> Result<String, SensorError> {
        let a = sensor.read(Some(Unit::F))?;
        let b = sensor.burst(&[1, 2], None)?;
        let t = sensor.temp(super::Celsius(1.0))?;
        let g = sensor.grade().map_err(|e| SensorError::Consumer { message: e.0 })?;
        sensor.reset()?;
        Ok(format!("{a:?} {b:?} {t:?} {g}"))
    }

    #[weaveffi::module]
    pub mod nested {
        #[weaveffi::export]
        pub fn warmer(t: super::Temp, by: Option<super::Temp>) -> super::Temp {
            super::super::Celsius(t.0 + by.map_or(0.0, |b| b.0))
        }
    }
}

weaveffi::export_runtime!();

fn main() {
    let _ = Arc::new(shapes::Station::new(1)).size();
}
