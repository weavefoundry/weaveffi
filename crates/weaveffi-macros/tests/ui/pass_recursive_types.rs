//! A record or rich enum may contain itself, or a type that contains it,
//! through a list or map (the IDL allows it), and crosses in every position.
#![deny(unsafe_code)]

use std::collections::BTreeMap;

#[weaveffi::record]
pub struct Node {
    pub value: i32,
    pub children: Vec<Node>,
    pub named: BTreeMap<String, Node>,
}

#[weaveffi::enumeration]
pub enum Expr {
    Lit { value: i64 },
    Sum { terms: Vec<Expr> },
}

// Two records that contain each other.
#[weaveffi::record]
pub struct Dir {
    pub name: String,
    pub files: Vec<File>,
}

#[weaveffi::record]
pub struct File {
    pub name: String,
    pub links: Option<Vec<Dir>>,
}

#[weaveffi::export]
pub fn depth(node: Node) -> i32 {
    1 + node.children.into_iter().map(depth).max().unwrap_or(0)
}

#[weaveffi::export]
pub fn eval(expr: Expr) -> i64 {
    match expr {
        Expr::Lit { value } => value,
        Expr::Sum { terms } => terms.into_iter().map(eval).sum(),
    }
}

#[weaveffi::export]
pub fn root(name: String) -> Dir {
    Dir { name, files: Vec::new() }
}

#[weaveffi::export]
pub async fn leaf(value: i32) -> Option<Node> {
    Some(Node { value, children: Vec::new(), named: BTreeMap::new() })
}

weaveffi::export_runtime!();

fn main() {}
