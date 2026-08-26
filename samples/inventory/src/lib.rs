//! Inventory sample: a two-module product and order catalog in safe Rust.
//!
//! This sample shows how `#[weaveffi::module]` scales past a single namespace.
//! The `products` module owns the `Product` record, its `Category` enum, and
//! the `Catalog` interface: each catalog object holds its products directly
//! (methods take `&self` and guard the state with a `Mutex`). The `orders`
//! module keeps free functions and an in-memory store behind a `Mutex`, and
//! references `products::Product` across the module boundary. Both modules
//! declare typed error domains, and the macro generates every `extern "C"`
//! thunk and the value-buffer marshalling that carries records and lists
//! across the boundary. No `unsafe` glue is written by hand.

/// Product catalog: the `Product` record, its `Category` enum, and the
/// `Catalog` interface.
#[weaveffi::module]
pub mod products {
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    /// The product catalog's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum ProductsError {
        /// price must be positive
        InvalidPrice = 1,
        /// product not found
        ProductNotFound = 2,
    }

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

    /// A product catalog exported as an interface. Each catalog owns its
    /// products and id counter directly; destroying the object (via the
    /// generated destroy symbol) releases that state.
    #[weaveffi::interface]
    pub struct Catalog {
        products: Mutex<Vec<Product>>,
        next_id: AtomicI64,
    }

    impl Default for Catalog {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Catalog {
        /// Create an empty catalog.
        pub fn new() -> Self {
            Catalog {
                products: Mutex::new(Vec::new()),
                next_id: AtomicI64::new(1),
            }
        }

        /// Add a product, returning the stored record with its assigned id.
        /// A non-positive price is rejected with
        /// [`ProductsError::InvalidPrice`].
        pub fn add_product(
            &self,
            name: String,
            price: f64,
            category: Category,
        ) -> Result<Product, ProductsError> {
            if price <= 0.0 {
                return Err(ProductsError::InvalidPrice);
            }
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let product = Product {
                id,
                name,
                description: None,
                price,
                category,
                tags: Vec::new(),
            };
            self.products.lock().unwrap().push(product.clone());
            Ok(product)
        }

        /// Look up a product by id, failing with [`ProductsError::ProductNotFound`]
        /// when none exists.
        pub fn get_product(&self, id: i64) -> Result<Product, ProductsError> {
            self.products
                .lock()
                .unwrap()
                .iter()
                .find(|p| p.id == id)
                .cloned()
                .ok_or(ProductsError::ProductNotFound)
        }

        /// Return every product on the given shelf.
        pub fn search(&self, category: Category) -> Vec<Product> {
            self.products
                .lock()
                .unwrap()
                .iter()
                .filter(|p| p.category == category)
                .cloned()
                .collect()
        }

        /// Update a product's price, returning whether the product existed.
        /// A non-positive price is rejected with
        /// [`ProductsError::InvalidPrice`].
        pub fn update_price(&self, id: i64, price: f64) -> Result<bool, ProductsError> {
            if price <= 0.0 {
                return Err(ProductsError::InvalidPrice);
            }
            let mut products = self.products.lock().unwrap();
            match products.iter_mut().find(|p| p.id == id) {
                Some(p) => {
                    p.price = price;
                    Ok(true)
                }
                None => Ok(false),
            }
        }

        /// Remove a product by id, returning whether it existed.
        pub fn remove(&self, id: i64) -> bool {
            let mut products = self.products.lock().unwrap();
            let before = products.len();
            products.retain(|p| p.id != id);
            products.len() < before
        }
    }
}

/// Order management: the `Order`/`OrderItem` records and order operations,
/// including one that takes a `products::Product` across the module boundary.
#[weaveffi::module]
pub mod orders {
    use super::products::Product;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    /// The order module's error domain.
    #[weaveffi::error]
    #[derive(Debug)]
    pub enum OrdersError {
        /// order not found
        OrderNotFound = 1,
        /// order must contain at least one item
        EmptyOrder = 2,
    }

    /// A single line in an order.
    #[weaveffi::record]
    #[derive(Clone, Debug)]
    pub struct OrderItem {
        /// Id of the ordered product.
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

    /// Create an order from a list of items, returning its id. An empty item
    /// list is rejected with [`OrdersError::EmptyOrder`].
    #[weaveffi::export]
    pub fn create_order(items: Vec<OrderItem>) -> Result<i64, OrdersError> {
        if items.is_empty() {
            return Err(OrdersError::EmptyOrder);
        }
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
        Ok(id)
    }

    /// Look up an order by id, failing with [`OrdersError::OrderNotFound`] when
    /// none exists.
    #[weaveffi::export]
    pub fn get_order(id: i64) -> Result<Order, OrdersError> {
        STORE
            .lock()
            .unwrap()
            .iter()
            .find(|o| o.id == id)
            .cloned()
            .ok_or(OrdersError::OrderNotFound)
    }

    /// Cancel an order, returning whether this call changed its status (a
    /// missing or already-cancelled order yields `false`).
    #[weaveffi::export]
    pub fn cancel_order(id: i64) -> bool {
        let mut store = STORE.lock().unwrap();
        match store.iter_mut().find(|o| o.id == id) {
            Some(o) if o.status != "cancelled" => {
                o.status = "cancelled".to_string();
                true
            }
            _ => false,
        }
    }

    /// Append a single unit of `product` to an existing order, returning
    /// whether the order existed. Demonstrates a cross-module record
    /// parameter.
    #[weaveffi::export]
    pub fn add_product_to_order(order_id: i64, product: Product) -> bool {
        let mut store = STORE.lock().unwrap();
        match store.iter_mut().find(|o| o.id == order_id) {
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
#[allow(unsafe_code)]
mod tests {
    use super::orders::{
        add_product_to_order, cancel_order, create_order, get_order, OrderItem, OrdersError,
    };
    use super::products::{Catalog, Category, ProductsError};
    use std::sync::Mutex;

    // The orders store is process-global, so serialize the tests that touch it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn orders_guard() -> std::sync::MutexGuard<'static, ()> {
        let g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        super::orders::reset();
        g
    }

    fn item(product_id: i64, quantity: i32, unit_price: f64) -> OrderItem {
        OrderItem {
            product_id,
            quantity,
            unit_price,
        }
    }

    // ── Products ────────────────────────────────────────────

    #[test]
    fn add_and_get_product() {
        let catalog = Catalog::new();
        let added = catalog
            .add_product("Widget".into(), 9.99, Category::Electronics)
            .expect("valid product");
        assert!(added.id > 0);
        let p = catalog.get_product(added.id).expect("product exists");
        assert_eq!(p.name, "Widget");
        assert_eq!(p.price, 9.99);
        assert_eq!(p.category, Category::Electronics);
        assert_eq!(p.description, None);
        assert!(p.tags.is_empty());
    }

    #[test]
    fn add_product_rejects_non_positive_price() {
        let catalog = Catalog::new();
        assert!(matches!(
            catalog.add_product("Free".into(), 0.0, Category::Food),
            Err(ProductsError::InvalidPrice)
        ));
    }

    #[test]
    fn get_missing_product_is_not_found() {
        let catalog = Catalog::new();
        assert!(matches!(
            catalog.get_product(999),
            Err(ProductsError::ProductNotFound)
        ));
    }

    #[test]
    fn search_products_by_category() {
        let catalog = Catalog::new();
        catalog
            .add_product("Laptop".into(), 999.99, Category::Electronics)
            .unwrap();
        catalog
            .add_product("Shirt".into(), 29.99, Category::Clothing)
            .unwrap();
        catalog
            .add_product("Phone".into(), 499.99, Category::Electronics)
            .unwrap();

        assert_eq!(catalog.search(Category::Electronics).len(), 2);
        assert!(catalog.search(Category::Books).is_empty());
    }

    #[test]
    fn update_and_remove_product() {
        let catalog = Catalog::new();
        let added = catalog
            .add_product("Item".into(), 10.0, Category::Food)
            .unwrap();
        assert!(catalog.update_price(added.id, 20.0).unwrap());
        assert_eq!(catalog.get_product(added.id).unwrap().price, 20.0);
        assert!(!catalog.update_price(999, 1.0).unwrap());
        assert!(matches!(
            catalog.update_price(added.id, -5.0),
            Err(ProductsError::InvalidPrice)
        ));

        assert!(catalog.remove(added.id));
        assert!(!catalog.remove(added.id));
    }

    // ── Orders ──────────────────────────────────────────────

    #[test]
    fn create_and_get_order() {
        let _g = orders_guard();
        let id = create_order(vec![item(1, 2, 10.0), item(2, 1, 25.0)]).expect("non-empty order");
        assert!(id > 0);
        let o = get_order(id).expect("order exists");
        assert_eq!(o.items.len(), 2);
        assert_eq!(o.total, 45.0);
        assert_eq!(o.status, "pending");
    }

    #[test]
    fn create_empty_order_is_rejected() {
        let _g = orders_guard();
        assert!(matches!(
            create_order(Vec::new()),
            Err(OrdersError::EmptyOrder)
        ));
    }

    #[test]
    fn get_missing_order_is_not_found() {
        let _g = orders_guard();
        assert!(matches!(get_order(999), Err(OrdersError::OrderNotFound)));
    }

    #[test]
    fn cancel_order_transitions() {
        let _g = orders_guard();
        let id = create_order(vec![item(1, 1, 5.0)]).unwrap();
        assert!(cancel_order(id));
        assert!(!cancel_order(id));
        assert_eq!(get_order(id).unwrap().status, "cancelled");
        assert!(!cancel_order(999));
    }

    // ── Cross-module ────────────────────────────────────────

    #[test]
    fn add_product_to_order_across_modules() {
        let _g = orders_guard();
        let catalog = Catalog::new();
        let product = catalog
            .add_product("Gadget".into(), 49.99, Category::Electronics)
            .unwrap();
        let order_id = create_order(vec![item(0, 1, 1.0)]).unwrap();

        assert!(add_product_to_order(order_id, product.clone()));

        let o = get_order(order_id).unwrap();
        assert_eq!(o.items.len(), 2);
        assert_eq!(o.items[1].product_id, product.id);
        assert_eq!(o.items[1].unit_price, 49.99);
        assert_eq!(o.total, 50.99);

        assert!(!add_product_to_order(999, product));
    }

    // A direct exercise of the generated C ABI thunks. This drives the
    // list-of-record parameter (`create_order`) through the `extern "C"`
    // boundary as one encoded value buffer, and decodes the buffered `Order`
    // return.
    #[test]
    fn ffi_order_surface_smoke() {
        use super::orders::{weaveffi_orders_create_order, weaveffi_orders_get_order, Order};
        use weaveffi::abi::{self, weaveffi_error};

        let _g = orders_guard();
        let mut err = weaveffi_error::default();

        // Encode the item list into the (ptr, len) slot pair the thunk
        // expects; the buffer is borrowed for the duration of the call.
        let items = vec![item(1, 2, 10.0), item(2, 1, 25.0)];
        let items_buf = abi::encode_value(&items);
        let id = weaveffi_orders_create_order(items_buf.as_ptr(), items_buf.len(), &mut err);
        assert_eq!(err.code, 0);
        assert!(id > 0);

        let mut order_len: usize = 0;
        let order_ptr = weaveffi_orders_get_order(id, &mut order_len, &mut err);
        assert_eq!(err.code, 0);
        assert!(!order_ptr.is_null());
        let order =
            abi::decode_value::<Order>(unsafe { std::slice::from_raw_parts(order_ptr, order_len) })
                .expect("well-formed Order buffer");
        abi::free_bytes(order_ptr as *mut u8, order_len);
        assert_eq!(order.total, 45.0);
        assert_eq!(order.items.len(), 2);
        assert_eq!(order.status, "pending");

        // An empty item list reports the EmptyOrder domain code.
        let empty_buf = abi::encode_value(&Vec::<OrderItem>::new());
        let rejected = weaveffi_orders_create_order(empty_buf.as_ptr(), empty_buf.len(), &mut err);
        assert_eq!(rejected, 0);
        assert_eq!(err.code, 2);
        abi::error_clear(&mut err);
    }

    // The interface thunks: construct a catalog, add a product (success and
    // typed-error paths), and read it back, all through the C ABI. The
    // catalog itself still crosses as an opaque pointer; the `Product`
    // results are decoded from value buffers.
    #[test]
    fn ffi_catalog_surface_smoke() {
        use super::products::{
            weaveffi_products_Catalog_add_product, weaveffi_products_Catalog_destroy,
            weaveffi_products_Catalog_get_product, weaveffi_products_Catalog_new,
            weaveffi_products_Catalog_remove, Product,
        };
        use std::ffi::CString;
        use weaveffi::abi::{self, weaveffi_error};

        fn decode_and_free(ptr: *const u8, len: usize) -> Product {
            assert!(!ptr.is_null());
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
            let product = abi::decode_value::<Product>(bytes).expect("well-formed Product buffer");
            abi::free_bytes(ptr as *mut u8, len);
            product
        }

        let mut err = weaveffi_error::default();
        let catalog = weaveffi_products_Catalog_new(&mut err);
        assert_eq!(err.code, 0);
        assert!(!catalog.is_null());

        let name = CString::new("Widget").unwrap();
        let mut added_len: usize = 0;
        let added_ptr = weaveffi_products_Catalog_add_product(
            catalog,
            name.as_ptr(),
            9.99,
            Category::Electronics as i32,
            &mut added_len,
            &mut err,
        );
        assert_eq!(err.code, 0);
        let added = decode_and_free(added_ptr, added_len);
        assert!(added.id > 0);
        assert_eq!(added.name, "Widget");

        // A non-positive price reports the InvalidPrice domain code.
        let mut rejected_len: usize = 0;
        let rejected = weaveffi_products_Catalog_add_product(
            catalog,
            name.as_ptr(),
            0.0,
            Category::Electronics as i32,
            &mut rejected_len,
            &mut err,
        );
        assert!(rejected.is_null());
        assert_eq!(err.code, 1);
        abi::error_clear(&mut err);

        let mut fetched_len: usize = 0;
        let fetched_ptr =
            weaveffi_products_Catalog_get_product(catalog, added.id, &mut fetched_len, &mut err);
        assert_eq!(err.code, 0);
        let fetched = decode_and_free(fetched_ptr, fetched_len);
        assert_eq!(fetched.price, 9.99);

        assert!(weaveffi_products_Catalog_remove(
            catalog, added.id, &mut err
        ));

        weaveffi_products_Catalog_destroy(catalog);
    }
}
