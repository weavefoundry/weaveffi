//! Shapes sample cdylib: exercises WeaveFFI's rich (algebraic) enums and the
//! expanded numeric type set over the stable C ABI.
//!
//! `Shape` is a sum type whose variants carry associated data, so the
//! `#[weaveffi::module]` expansion crosses it as an opaque object: a tag
//! reader, per-variant constructors and field getters, and a destructor,
//! exactly the surface a struct gets. `Channel` is a plain C-style enum that
//! crosses as its `i32` discriminant. The producer writes only safe Rust; the
//! macro emits the `weaveffi_shapes_*` thunks that line up 1:1 with the
//! generated header (see `weaveffi generate shapes.yml --target c`).

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
mod tests {
    use crate::shapes::*;
    use weaveffi::abi::{self, weaveffi_error};

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    #[test]
    fn circle_roundtrips_radius_and_tag() {
        let mut err = new_err();
        let s = weaveffi_shapes_Shape_Circle_new(2.5, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(weaveffi_shapes_Shape_tag(s), 1);
        assert!((weaveffi_shapes_Shape_Circle_get_radius(s) - 2.5).abs() < 1e-9);
        weaveffi_shapes_Shape_destroy(s);
    }

    #[test]
    fn rectangle_uses_f32_fields() {
        let mut err = new_err();
        let s = weaveffi_shapes_Shape_Rectangle_new(3.0, 4.0, &mut err);
        assert_eq!(weaveffi_shapes_Shape_tag(s), 2);
        assert!((weaveffi_shapes_Shape_Rectangle_get_width(s) - 3.0).abs() < 1e-6);
        assert!((weaveffi_shapes_Shape_Rectangle_get_height(s) - 4.0).abs() < 1e-6);
        weaveffi_shapes_Shape_destroy(s);
    }

    #[test]
    fn labeled_roundtrips_string_and_u8() {
        let mut err = new_err();
        let label = std::ffi::CString::new("hex").unwrap();
        let s = weaveffi_shapes_Shape_Labeled_new(label.as_ptr(), 6, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(weaveffi_shapes_Shape_tag(s), 3);
        let got = weaveffi_shapes_Shape_Labeled_get_label(s);
        assert_eq!(abi::c_ptr_to_string(got).unwrap(), "hex");
        abi::free_string(got);
        assert_eq!(weaveffi_shapes_Shape_Labeled_get_count(s), 6);
        weaveffi_shapes_Shape_destroy(s);
    }

    #[test]
    fn empty_has_tag_zero() {
        let mut err = new_err();
        let s = weaveffi_shapes_Shape_Empty_new(&mut err);
        assert_eq!(weaveffi_shapes_Shape_tag(s), 0);
        weaveffi_shapes_Shape_destroy(s);
    }

    #[test]
    fn describe_and_scale() {
        let mut err = new_err();
        let s = weaveffi_shapes_Shape_Circle_new(2.0, &mut err);
        let d = weaveffi_shapes_describe(s, &mut err);
        assert_eq!(abi::c_ptr_to_string(d).unwrap(), "circle(r=2)");
        abi::free_string(d);

        let scaled = weaveffi_shapes_scale(s, 3.0, &mut err);
        assert!((weaveffi_shapes_Shape_Circle_get_radius(scaled) - 6.0).abs() < 1e-9);
        weaveffi_shapes_Shape_destroy(scaled);
        weaveffi_shapes_Shape_destroy(s);
    }

    #[test]
    fn sum_bytes_widens_to_u64() {
        let mut err = new_err();
        let data: [u8; 4] = [250, 250, 250, 250];
        let total = weaveffi_shapes_sum_bytes(data.as_ptr(), data.len(), &mut err);
        assert_eq!(total, 1000);
        assert_eq!(err.code, 0);
    }

    #[test]
    fn destroy_null_is_safe() {
        weaveffi_shapes_Shape_destroy(std::ptr::null_mut());
    }
}
