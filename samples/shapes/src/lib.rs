//! Shapes sample cdylib: exercises WeaveFFI's rich (algebraic) enums and the
//! expanded numeric type set over the stable C ABI.
//!
//! `Shape` is a sum type whose variants carry associated data, so it crosses
//! the boundary as an opaque object: a tag reader, per-variant constructors and
//! field getters, and a destructor, exactly the surface a struct gets. The
//! symbol names here line up 1:1 with the generated header
//! (`weaveffi_shapes_Shape_*`); see `weaveffi generate shapes.yml --target c`.

#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use weaveffi_abi::{self as abi, weaveffi_error};

/// The algebraic shape. Discriminants match the IDL (`Empty=0 … Labeled=3`).
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Empty,
    Circle { radius: f64 },
    Rectangle { width: f32, height: f32 },
    Labeled { label: String, count: u8 },
}

impl Shape {
    fn tag(&self) -> i32 {
        match self {
            Shape::Empty => 0,
            Shape::Circle { .. } => 1,
            Shape::Rectangle { .. } => 2,
            Shape::Labeled { .. } => 3,
        }
    }

    fn describe(&self) -> String {
        match self {
            Shape::Empty => "empty".to_string(),
            Shape::Circle { radius } => format!("circle(r={radius})"),
            Shape::Rectangle { width, height } => format!("rectangle({width}x{height})"),
            Shape::Labeled { label, count } => format!("labeled({label} x{count})"),
        }
    }

    fn scaled(&self, factor: f64) -> Shape {
        match self {
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
}

fn boxed(shape: Shape) -> *mut Shape {
    Box::into_raw(Box::new(shape))
}

// --- Rich-enum constructors ---

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Empty_new(out_err: *mut weaveffi_error) -> *mut Shape {
    abi::error_set_ok(out_err);
    boxed(Shape::Empty)
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Circle_new(
    radius: f64,
    out_err: *mut weaveffi_error,
) -> *mut Shape {
    abi::error_set_ok(out_err);
    boxed(Shape::Circle { radius })
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Rectangle_new(
    width: f32,
    height: f32,
    out_err: *mut weaveffi_error,
) -> *mut Shape {
    abi::error_set_ok(out_err);
    boxed(Shape::Rectangle { width, height })
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Labeled_new(
    label: *const c_char,
    count: u8,
    out_err: *mut weaveffi_error,
) -> *mut Shape {
    let label = match abi::c_ptr_to_string(label) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, 1, "label is null or invalid UTF-8");
            return std::ptr::null_mut();
        }
    };
    abi::error_set_ok(out_err);
    boxed(Shape::Labeled { label, count })
}

// --- Tag reader + destructor ---

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_tag(ptr: *const Shape) -> i32 {
    assert!(!ptr.is_null());
    unsafe { &*ptr }.tag()
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_destroy(ptr: *mut Shape) {
    if ptr.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(ptr)) };
}

// --- Per-variant field getters ---

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Circle_get_radius(ptr: *const Shape) -> f64 {
    assert!(!ptr.is_null());
    match unsafe { &*ptr } {
        Shape::Circle { radius } => *radius,
        _ => 0.0,
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Rectangle_get_width(ptr: *const Shape) -> f32 {
    assert!(!ptr.is_null());
    match unsafe { &*ptr } {
        Shape::Rectangle { width, .. } => *width,
        _ => 0.0,
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Rectangle_get_height(ptr: *const Shape) -> f32 {
    assert!(!ptr.is_null());
    match unsafe { &*ptr } {
        Shape::Rectangle { height, .. } => *height,
        _ => 0.0,
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Labeled_get_label(ptr: *const Shape) -> *const c_char {
    assert!(!ptr.is_null());
    match unsafe { &*ptr } {
        Shape::Labeled { label, .. } => abi::string_to_c_ptr(label),
        _ => std::ptr::null(),
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_Shape_Labeled_get_count(ptr: *const Shape) -> u8 {
    assert!(!ptr.is_null());
    match unsafe { &*ptr } {
        Shape::Labeled { count, .. } => *count,
        _ => 0,
    }
}

// --- Module functions ---

#[no_mangle]
pub extern "C" fn weaveffi_shapes_describe(
    shape: *const Shape,
    out_err: *mut weaveffi_error,
) -> *const c_char {
    if shape.is_null() {
        abi::error_set(out_err, 1, "shape is null");
        return std::ptr::null();
    }
    abi::error_set_ok(out_err);
    abi::string_to_c_ptr(unsafe { &*shape }.describe())
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_scale(
    shape: *const Shape,
    factor: f64,
    out_err: *mut weaveffi_error,
) -> *mut Shape {
    if shape.is_null() {
        abi::error_set(out_err, 1, "shape is null");
        return std::ptr::null_mut();
    }
    abi::error_set_ok(out_err);
    boxed(unsafe { &*shape }.scaled(factor))
}

#[no_mangle]
pub extern "C" fn weaveffi_shapes_sum_bytes(
    values: *const u8,
    values_len: usize,
    out_err: *mut weaveffi_error,
) -> u64 {
    abi::error_set_ok(out_err);
    if values.is_null() || values_len == 0 {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(values, values_len) };
    slice.iter().map(|b| u64::from(*b)).sum()
}

abi::export_runtime!();

#[cfg(test)]
mod tests {
    use super::*;

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
