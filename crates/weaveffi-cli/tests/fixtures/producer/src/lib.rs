//! A small producer exercising every kind of declaration the library
//! metadata carries: a module tree with a nested module, an error domain
//! with a payload, a type alias, a C-style enum, a record, a callback
//! interface, an interface whose members span two `impl` blocks, and a
//! sibling tree that uses the first tree's record. Declarations marked
//! `#[cfg(feature = "extra")]` (an item, an `impl` block, and a nested
//! module) exist only in a build with the `extra` feature, and so only in
//! that build's metadata.

/// The shop.
#[weaveffi::module]
pub mod shop {
    use std::sync::{Arc, Mutex};

    use weaveffi::ForeignError;

    /// The shop's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    #[repr(i32)]
    pub enum ShopError {
        /// sold out
        SoldOut {
            /// The item that ran out.
            item: String,
        } = 1,
    }

    impl std::fmt::Display for ShopError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::SoldOut { item } => write!(f, "{item} is sold out"),
            }
        }
    }

    /// A price in cents.
    pub type Price = u64;

    /// How big an item is.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug)]
    pub enum Size {
        /// Small.
        Small = 0,
        /// Large.
        Large = 1,
    }

    /// Something for sale.
    #[weaveffi::record]
    #[derive(Clone, Debug)]
    pub struct Item {
        /// The item's name.
        pub name: String,
        /// What it costs.
        pub price: Price,
        /// How big it is.
        pub size: Size,
    }

    /// Told about every item added to a cart.
    #[weaveffi::callback_interface]
    pub trait Watcher: Send + Sync {
        /// `item` was added.
        fn added(&self, item: &Item) -> Result<(), ForeignError>;
    }

    /// A shopping cart.
    #[weaveffi::interface]
    #[derive(Default)]
    pub struct Cart {
        items: Mutex<Vec<Item>>,
    }

    impl Cart {
        /// An empty cart.
        pub fn new() -> Cart {
            Cart::default()
        }

        /// Add `item`, telling `watcher`.
        pub fn add(&self, item: Item, watcher: Option<Arc<dyn Watcher>>) {
            if let Some(w) = watcher {
                let _ = w.added(&item);
            }
            self.items.lock().unwrap().push(item);
        }

        /// The sum of every item's price.
        pub fn total(&self) -> Price {
            self.items.lock().unwrap().iter().map(|i| i.price).sum()
        }
    }

    #[cfg(feature = "extra")]
    impl Cart {
        /// Empty the cart.
        pub fn clear(&self) {
            self.items.lock().unwrap().clear();
        }
    }

    /// What `item` costs.
    #[weaveffi::export]
    pub fn price_of(item: Item) -> Price {
        item.price
    }

    /// Buy one `name`. Fails with [`ShopError::SoldOut`] for anything but
    /// `"tea"`.
    #[weaveffi::export]
    pub fn buy(name: String) -> Result<Item, ShopError> {
        if name != "tea" {
            return Err(ShopError::SoldOut { item: name });
        }
        Ok(Item {
            name,
            price: 250,
            size: Size::Small,
        })
    }

    /// The discount in percent.
    #[cfg(feature = "extra")]
    #[weaveffi::export]
    pub fn discount() -> u32 {
        10
    }

    /// The list price of everything.
    #[cfg(not(feature = "extra"))]
    #[weaveffi::export]
    pub fn list_price() -> Price {
        250
    }

    /// Stock levels.
    #[weaveffi::module]
    pub mod stock {
        use std::sync::Arc;

        use super::Cart;

        /// How many items `cart` holds, if there is one.
        #[weaveffi::export]
        pub fn count(cart: Option<Arc<Cart>>) -> u32 {
            cart.map_or(0, |c| c.items.lock().unwrap().len() as u32)
        }
    }

    /// Promotions.
    #[cfg(feature = "extra")]
    #[weaveffi::module]
    pub mod promo {
        /// Today's code.
        #[weaveffi::export]
        pub fn code() -> String {
            "TEA10".to_string()
        }
    }
}

/// Receipts, in a sibling tree that uses the shop's record.
#[weaveffi::module]
pub mod receipt {
    use super::shop::Item;

    /// One line describing `item`.
    #[weaveffi::export]
    pub fn line(item: Item) -> String {
        format!("{}: {}", item.name, item.price)
    }
}

weaveffi::export_runtime!();
