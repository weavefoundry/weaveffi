//! Inventory sample: a two-module product and order catalog in safe Rust.
//!
//! This sample shows how `#[weaveffi::module]` scales past a single namespace.
//! The `products` module owns the `Product` record and its `Category` enum; the
//! `orders` module owns `Order`/`OrderItem` and references `products::Product`
//! across the module boundary. Each module keeps its own in-memory store behind
//! a `Mutex`, hands out opaque `u64` handles, and lets the macro generate every
//! `extern "C"` thunk, getter, and list marshalling. No `unsafe` glue is written
//! by hand.

/// Product catalog: the `Product` record, its `Category`, and CRUD functions.
#[weaveffi::module]
pub mod products {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    /// The shelf a product belongs to.
    #[weaveffi::enumeration]
    #[repr(i32)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Category {
        /// Phones, laptops, and other devices.
        Electronics = 0,
        /// Apparel and accessories.
        Clothing = 1,
        /// Anything edible.
        Food = 2,
        /// Printed and digital books.
        Books = 3,
    }

    /// A catalog item.
    #[weaveffi::record]
    #[derive(Clone, Debug)]
    pub struct Product {
        /// Stable identifier assigned on creation.
        pub id: i64,
        /// Display name.
        pub name: String,
        /// Optional long-form description.
        pub description: Option<String>,
        /// Price in the catalog's base currency.
        pub price: f64,
        /// Which shelf the product sits on.
        pub category: Category,
        /// Free-form search tags.
        pub tags: Vec<String>,
    }

    static STORE: Mutex<Vec<Product>> = Mutex::new(Vec::new());
    static NEXT_ID: AtomicI64 = AtomicI64::new(1);

    /// Create a product, returning its opaque handle.
    #[weaveffi::export]
    pub fn create_product(name: String, price: f64, category: Category) -> u64 {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        STORE.lock().unwrap().push(Product {
            id,
            name,
            description: None,
            price,
            category,
            tags: Vec::new(),
        });
        id as u64
    }

    /// Look up a product by handle, erroring if none exists.
    #[weaveffi::export]
    pub fn get_product(id: u64) -> Result<Product, String> {
        STORE
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id as i64)
            .cloned()
            .ok_or_else(|| format!("product {id} not found"))
    }

    /// Return every product on the given shelf.
    #[weaveffi::export]
    pub fn search_products(category: Category) -> Vec<Product> {
        STORE
            .lock()
            .unwrap()
            .iter()
            .filter(|p| p.category == category)
            .cloned()
            .collect()
    }

    /// Update a product's price, returning whether the product existed.
    #[weaveffi::export]
    pub fn update_price(id: u64, price: f64) -> bool {
        let mut store = STORE.lock().unwrap();
        match store.iter_mut().find(|p| p.id == id as i64) {
            Some(p) => {
                p.price = price;
                true
            }
            None => false,
        }
    }

    /// Delete a product by handle, returning whether it existed.
    #[weaveffi::export]
    pub fn delete_product(id: u64) -> bool {
        let mut store = STORE.lock().unwrap();
        let before = store.len();
        store.retain(|p| p.id != id as i64);
        store.len() < before
    }

    /// Reset the in-memory store. Test-only helper (not part of the ABI).
    #[cfg(test)]
    pub(crate) fn reset() {
        STORE.lock().unwrap().clear();
        NEXT_ID.store(1, Ordering::Relaxed);
    }
}

/// Order management: the `Order`/`OrderItem` records and order operations,
/// including one that takes a `products::Product` across the module boundary.
#[weaveffi::module]
pub mod orders {
    use super::products::Product;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    /// A single line in an order.
    #[weaveffi::record]
    #[derive(Clone, Debug)]
    pub struct OrderItem {
        /// Handle of the ordered product.
        pub product_id: i64,
        /// How many units were ordered.
        pub quantity: i32,
        /// Price charged per unit at order time.
        pub unit_price: f64,
    }

    /// A customer order.
    #[weaveffi::record]
    #[derive(Clone, Debug)]
    pub struct Order {
        /// Stable identifier assigned on creation.
        pub id: i64,
        /// The ordered line items.
        pub items: Vec<OrderItem>,
        /// Sum of `unit_price * quantity` across the items.
        pub total: f64,
        /// Lifecycle status (`pending`, `cancelled`, ...).
        pub status: String,
    }

    static STORE: Mutex<Vec<Order>> = Mutex::new(Vec::new());
    static NEXT_ID: AtomicI64 = AtomicI64::new(1);

    /// Create an order from a list of items, returning its opaque handle.
    #[weaveffi::export]
    pub fn create_order(items: Vec<OrderItem>) -> u64 {
        let total = items
            .iter()
            .map(|it| it.unit_price * it.quantity as f64)
            .sum();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        STORE.lock().unwrap().push(Order {
            id,
            items,
            total,
            status: "pending".to_string(),
        });
        id as u64
    }

    /// Look up an order by handle, erroring if none exists.
    #[weaveffi::export]
    pub fn get_order(id: u64) -> Result<Order, String> {
        STORE
            .lock()
            .unwrap()
            .iter()
            .find(|o| o.id == id as i64)
            .cloned()
            .ok_or_else(|| format!("order {id} not found"))
    }

    /// Cancel an order, returning whether this call changed its status (a
    /// missing or already-cancelled order yields `false`).
    #[weaveffi::export]
    pub fn cancel_order(id: u64) -> bool {
        let mut store = STORE.lock().unwrap();
        match store.iter_mut().find(|o| o.id == id as i64) {
            Some(o) if o.status != "cancelled" => {
                o.status = "cancelled".to_string();
                true
            }
            _ => false,
        }
    }

    /// Append a single unit of `product` to an existing order, returning whether
    /// the order existed. Demonstrates a cross-module record parameter.
    #[weaveffi::export]
    pub fn add_product_to_order(order_id: u64, product: Product) -> bool {
        let mut store = STORE.lock().unwrap();
        match store.iter_mut().find(|o| o.id == order_id as i64) {
            Some(order) => {
                let item = OrderItem {
                    product_id: product.id,
                    quantity: 1,
                    unit_price: product.price,
                };
                order.total += item.unit_price;
                order.items.push(item);
                true
            }
            None => false,
        }
    }

    /// Reset the in-memory store. Test-only helper (not part of the ABI).
    #[cfg(test)]
    pub(crate) fn reset() {
        STORE.lock().unwrap().clear();
        NEXT_ID.store(1, Ordering::Relaxed);
    }
}

weaveffi::export_runtime!();

#[cfg(test)]
mod tests {
    use super::orders::{add_product_to_order, cancel_order, create_order, get_order, OrderItem};
    use super::products::{
        create_product, delete_product, get_product, search_products, update_price, Category,
    };
    use std::sync::Mutex;

    // Both stores are process-global, so serialize the tests that touch them.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::products::reset();
        super::orders::reset();
        g
    }

    // ── Products ────────────────────────────────────────────

    #[test]
    fn create_and_get_product() {
        let _g = guard();
        let id = create_product("Widget".into(), 9.99, Category::Electronics);
        assert!(id > 0);
        let p = get_product(id).expect("product exists");
        assert_eq!(p.name, "Widget");
        assert_eq!(p.price, 9.99);
        assert_eq!(p.category, Category::Electronics);
        assert_eq!(p.description, None);
        assert!(p.tags.is_empty());
    }

    #[test]
    fn get_missing_product_is_err() {
        let _g = guard();
        assert!(get_product(999).is_err());
    }

    #[test]
    fn search_products_by_category() {
        let _g = guard();
        create_product("Laptop".into(), 999.99, Category::Electronics);
        create_product("Shirt".into(), 29.99, Category::Clothing);
        create_product("Phone".into(), 499.99, Category::Electronics);

        let electronics = search_products(Category::Electronics);
        assert_eq!(electronics.len(), 2);
        assert!(search_products(Category::Books).is_empty());
    }

    #[test]
    fn update_and_delete_product() {
        let _g = guard();
        let id = create_product("Item".into(), 10.0, Category::Food);
        assert!(update_price(id, 20.0));
        assert_eq!(get_product(id).unwrap().price, 20.0);
        assert!(!update_price(999, 1.0));

        assert!(delete_product(id));
        assert!(!delete_product(id));
    }

    // ── Orders ──────────────────────────────────────────────

    #[test]
    fn create_and_get_order() {
        let _g = guard();
        let id = create_order(vec![
            OrderItem {
                product_id: 1,
                quantity: 2,
                unit_price: 10.0,
            },
            OrderItem {
                product_id: 2,
                quantity: 1,
                unit_price: 25.0,
            },
        ]);
        assert!(id > 0);
        let o = get_order(id).expect("order exists");
        assert_eq!(o.items.len(), 2);
        assert_eq!(o.total, 45.0);
        assert_eq!(o.status, "pending");
    }

    #[test]
    fn create_empty_order() {
        let _g = guard();
        let id = create_order(Vec::new());
        let o = get_order(id).unwrap();
        assert!(o.items.is_empty());
        assert_eq!(o.total, 0.0);
    }

    #[test]
    fn cancel_order_transitions() {
        let _g = guard();
        let id = create_order(Vec::new());
        assert!(cancel_order(id));
        assert!(!cancel_order(id));
        assert_eq!(get_order(id).unwrap().status, "cancelled");
        assert!(!cancel_order(999));
    }

    // ── Cross-module ────────────────────────────────────────

    #[test]
    fn add_product_to_order_across_modules() {
        let _g = guard();
        let product_id = create_product("Gadget".into(), 49.99, Category::Electronics);
        let order_id = create_order(Vec::new());

        let product = get_product(product_id).unwrap();
        assert!(add_product_to_order(order_id, product));

        let o = get_order(order_id).unwrap();
        assert_eq!(o.items.len(), 1);
        assert_eq!(o.items[0].product_id, product_id as i64);
        assert_eq!(o.items[0].unit_price, 49.99);
        assert_eq!(o.total, 49.99);

        assert!(!add_product_to_order(999, get_product(product_id).unwrap()));
    }

    // A direct exercise of the generated C ABI thunks. This drives the
    // list-of-record parameter (`create_order`) through the `extern "C"`
    // boundary, so it covers the `lift_ptr_vec` marshalling the macro emits.
    #[test]
    fn ffi_surface_smoke() {
        use super::orders::{
            weaveffi_orders_OrderItem_create, weaveffi_orders_OrderItem_destroy,
            weaveffi_orders_Order_destroy, weaveffi_orders_Order_get_total,
            weaveffi_orders_create_order, weaveffi_orders_get_order,
        };
        use weaveffi::abi::weaveffi_error;

        let _g = guard();
        let mut err = weaveffi_error::default();

        // Build two OrderItem objects through their generated constructor.
        let a = weaveffi_orders_OrderItem_create(1, 2, 10.0, &mut err);
        assert_eq!(err.code, 0);
        let b = weaveffi_orders_OrderItem_create(2, 1, 25.0, &mut err);
        assert_eq!(err.code, 0);

        // Pass them as the array-of-object-pointers slot create_order expects.
        let items = [a, b];
        let handle = weaveffi_orders_create_order(items.as_ptr(), items.len(), &mut err);
        assert_eq!(err.code, 0);
        assert!(handle > 0);

        let order = weaveffi_orders_get_order(handle, &mut err);
        assert_eq!(err.code, 0);
        assert!(!order.is_null());
        assert_eq!(weaveffi_orders_Order_get_total(order), 45.0);

        weaveffi_orders_Order_destroy(order);
        weaveffi_orders_OrderItem_destroy(a);
        weaveffi_orders_OrderItem_destroy(b);
    }
}
