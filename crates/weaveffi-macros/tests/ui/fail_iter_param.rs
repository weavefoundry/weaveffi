#[weaveffi::module]
mod bad {
    #[weaveffi::export]
    pub fn drain(items: weaveffi::Iter<i32>) -> i64 {
        items.map(i64::from).sum()
    }
}

weaveffi::export_runtime!();

fn main() {}
