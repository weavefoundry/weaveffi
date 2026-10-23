//! Type aliases resolve through rustc, and an item's `#[cfg]` applies to
//! everything generated for it: the thunks, the metadata, and the contract.

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
#[weaveffi::export]
impl Store {
    pub fn new() -> Self {
        Store
    }
}

#[cfg(any())]
#[weaveffi::export]
impl Store {
    pub fn missing(&self) -> Hidden {
        Hidden { id: 0 }
    }
}

#[weaveffi::export]
impl Store {
    /// A member's own `#[cfg]` works too.
    #[cfg(any())]
    pub fn gone(&self) -> Hidden {
        Hidden { id: 0 }
    }

    pub fn here(&self) -> Id {
        1
    }
}

#[cfg(any())]
mod gone {
    #[weaveffi::export]
    pub fn vanished() {}
}

weaveffi::export_runtime!();

fn main() {
    let _ = Store::new().here();
}
