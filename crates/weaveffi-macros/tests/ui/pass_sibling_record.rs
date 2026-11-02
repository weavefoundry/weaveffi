//! Types declared in one Rust module cross from functions in another, each
//! by its own family: a record and a rich enum as value buffers, a C-style
//! enum as its discriminant, and an interface as a pointer.
#![deny(unsafe_code)]

pub mod shapes {
    #[weaveffi::record]
    pub struct Point {
        pub x: f64,
        pub y: f64,
    }

    #[weaveffi::enumeration]
    pub enum Shape {
        Dot { at: Point },
        Empty,
    }

    #[weaveffi::enumeration]
    pub enum Color {
        Red = 0,
        Green = 1,
    }

    #[weaveffi::interface]
    pub struct Brush;
}

pub mod canvas {
    use super::shapes::{Brush, Color, Point, Shape};

    #[weaveffi::export]
    pub fn origin() -> Point {
        Point { x: 0.0, y: 0.0 }
    }

    #[weaveffi::export]
    pub fn draw(points: Vec<Point>, shape: Option<super::shapes::Shape>) -> i32 {
        points.len() as i32 + i32::from(matches!(shape, Some(Shape::Dot { .. })))
    }

    #[weaveffi::export]
    pub fn mix(color: Color, brush: &Brush) -> i32 {
        let _ = brush;
        color as i32
    }
}

weaveffi::export_runtime!();

fn main() {}
