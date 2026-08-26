//! Shapes sample cdylib: exercises WeaveFFI's rich (algebraic) enums and the
//! expanded numeric type set over the stable C ABI.
//!
//! `Shape` is a sum type whose variants carry associated data, so the
//! `#[weaveffi::module]` expansion crosses it as a value buffer: an `i32`
//! tag followed by the active variant's fields, moved through one
//! `(const uint8_t*, size_t)` slot pair. The macro implements `BufferValue`
//! for the type instead of emitting per-variant C symbols. `Channel` is a
//! plain C-style enum that crosses as its `i32` discriminant. The producer
//! writes only safe Rust; the macro emits the `weaveffi_shapes_*` thunks that
//! line up 1:1 with the generated header (see `weaveffi generate shapes.yml
//! --target c`).

/// Rich-enum + numerics smoke test
#[weaveffi::module]
pub mod shapes {
    /// An algebraic shape (sum type with associated data)
    #[weaveffi::enumeration]
    #[derive(Debug, Clone, PartialEq)]
    pub enum Shape {
        /// The empty shape
        Empty,
        /// A circle with a radius
        Circle {
            /// Radius in points
            radius: f64,
        },
        /// An axis-aligned rectangle
        Rectangle { width: f32, height: f32 },
        /// A labeled shape with a small count
        Labeled { label: String, count: u8 },
    }

    /// A plain C-style enum (no payloads)
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Channel {
        Red = 0,
        Green = 1,
        Blue = 2,
    }

    /// Render a shape to a string
    #[weaveffi::export]
    pub fn describe(shape: &Shape) -> String {
        match shape {
            Shape::Empty => "empty".to_string(),
            Shape::Circle { radius } => format!("circle(r={radius})"),
            Shape::Rectangle { width, height } => format!("rectangle({width}x{height})"),
            Shape::Labeled { label, count } => format!("labeled({label} x{count})"),
        }
    }

    /// Scale a shape by a factor, returning a new shape
    #[weaveffi::export]
    pub fn scale(shape: &Shape, factor: f64) -> Shape {
        match shape {
            Shape::Empty => Shape::Empty,
            Shape::Circle { radius } => Shape::Circle {
                radius: radius * factor,
            },
            Shape::Rectangle { width, height } => Shape::Rectangle {
                width: (f64::from(*width) * factor) as f32,
                height: (f64::from(*height) * factor) as f32,
            },
            Shape::Labeled { label, count } => Shape::Labeled {
                label: label.clone(),
                count: *count,
            },
        }
    }

    /// Sum a list of bytes into a wide integer (numerics smoke)
    #[weaveffi::export]
    pub fn sum_bytes(values: Vec<u8>) -> u64 {
        values.iter().map(|b| u64::from(*b)).sum()
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
#[allow(unsafe_code)]
mod tests {
    use crate::shapes::*;
    use weaveffi::abi::{self, weaveffi_error};

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    /// Encode a shape into the borrowed value buffer a thunk parameter takes.
    fn encode_shape(shape: &Shape) -> Vec<u8> {
        abi::encode_value(shape)
    }

    #[test]
    fn circle_round_trips_with_tag() {
        // A rich enum crosses as a value buffer: an i32 tag (declaration
        // order, so Circle is 1) followed by the variant's fields.
        let shape = Shape::Circle { radius: 2.5 };
        let bytes = encode_shape(&shape);
        assert_eq!(bytes[..4], 1i32.to_le_bytes());
        assert_eq!(abi::decode_value::<Shape>(&bytes).unwrap(), shape);
    }

    #[test]
    fn rectangle_round_trips_f32_fields() {
        let shape = Shape::Rectangle {
            width: 3.0,
            height: 4.0,
        };
        let bytes = encode_shape(&shape);
        assert_eq!(bytes[..4], 2i32.to_le_bytes());
        assert_eq!(abi::decode_value::<Shape>(&bytes).unwrap(), shape);
    }

    #[test]
    fn labeled_round_trips_string_and_u8() {
        let shape = Shape::Labeled {
            label: "hex".to_string(),
            count: 6,
        };
        let bytes = encode_shape(&shape);
        assert_eq!(bytes[..4], 3i32.to_le_bytes());
        assert_eq!(abi::decode_value::<Shape>(&bytes).unwrap(), shape);
    }

    #[test]
    fn empty_has_tag_zero() {
        let bytes = encode_shape(&Shape::Empty);
        assert_eq!(bytes, 0i32.to_le_bytes());
        assert_eq!(abi::decode_value::<Shape>(&bytes).unwrap(), Shape::Empty);
    }

    #[test]
    fn describe_and_scale() {
        let mut err = new_err();
        let circle = encode_shape(&Shape::Circle { radius: 2.0 });
        let d = weaveffi_shapes_describe(circle.as_ptr(), circle.len(), &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(abi::c_ptr_to_string(d).unwrap(), "circle(r=2)");
        abi::free_string(d);

        // A buffered return: the thunk returns producer-owned bytes plus an
        // out-length; the consumer decodes and frees them.
        let mut out_len: usize = 0;
        let scaled_ptr =
            weaveffi_shapes_scale(circle.as_ptr(), circle.len(), 3.0, &mut out_len, &mut err);
        assert_eq!(err.code, 0);
        assert!(!scaled_ptr.is_null());
        let scaled_bytes = unsafe { std::slice::from_raw_parts(scaled_ptr, out_len) };
        let scaled = abi::decode_value::<Shape>(scaled_bytes).unwrap();
        abi::free_bytes(scaled_ptr as *mut u8, out_len);
        assert_eq!(scaled, Shape::Circle { radius: 6.0 });
    }

    #[test]
    fn sum_bytes_widens_to_u64() {
        let mut err = new_err();
        let data: [u8; 4] = [250, 250, 250, 250];
        let total = weaveffi_shapes_sum_bytes(data.as_ptr(), data.len(), &mut err);
        assert_eq!(total, 1000);
        assert_eq!(err.code, 0);
    }
}
