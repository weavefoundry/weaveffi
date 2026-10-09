//! A record (or rich enum) is the one kind of type a module tree may share
//! with a sibling tree: it crosses as a value buffer either way.
#![deny(unsafe_code)]

#[weaveffi::module]
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
}

#[weaveffi::module]
pub mod canvas {
    use super::shapes::{Point, Shape};

    #[weaveffi::export]
    pub fn origin() -> Point {
        Point { x: 0.0, y: 0.0 }
    }

    #[weaveffi::export]
    pub fn draw(points: Vec<Point>, shape: Option<super::shapes::Shape>) -> i32 {
        points.len() as i32 + i32::from(matches!(shape, Some(Shape::Dot { .. })))
    }
}

weaveffi::export_runtime!();

fn main() {}
