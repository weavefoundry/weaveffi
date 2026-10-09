//! Type aliases in the tree are resolved by substitution, and an item's
//! `#[cfg]` applies to everything generated for it.

#[weaveffi::module]
mod tree {
    pub type Id = u64;
    pub type Ids = Vec<Id>;

    #[weaveffi::export]
    pub fn first(ids: Ids) -> Option<Id> {
        ids.first().copied()
    }

    #[cfg(any())]
    #[weaveffi::export]
    pub fn never(id: Id) -> Id {
        id
    }

    #[cfg(any())]
    #[weaveffi::record]
    pub struct Hidden {
        pub id: Id,
    }

    #[weaveffi::interface]
    pub struct Store;

    #[cfg(all())]
    impl Store {
        pub fn new() -> Self {
            Store
        }
    }

    #[cfg(any())]
    impl Store {
        pub fn missing(&self) -> Hidden {
            Hidden { id: 0 }
        }
    }

    #[cfg(any())]
    #[weaveffi::module]
    pub mod gone {
        #[weaveffi::export]
        pub fn vanished() {}
    }
}

weaveffi::export_runtime!();

fn main() {
    let _ = tree::Store::new();
}
