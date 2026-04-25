#![allow(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::os::raw::c_char;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;
use weaveffi_abi::{self as abi, weaveffi_error, weaveffi_handle_t};

// ── Products module ─────────────────────────────────────────

#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Electronics = 0,
    Clothing = 1,
    Food = 2,
    Books = 3,
}

impl Category {
    fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::Electronics),
            1 => Some(Self::Clothing),
            2 => Some(Self::Food),
            3 => Some(Self::Books),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Product {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub price: f64,
    pub category: Category,
    pub tags: Vec<String>,
}

static PRODUCT_STORE: Mutex<Vec<Product>> = Mutex::new(Vec::new());
static NEXT_PRODUCT_ID: AtomicI64 = AtomicI64::new(1);

// ── Helpers ─────────────────────────────────────────────────

/// Convert a (ptr, len) UTF-8 byte slice to an owned `String`.
/// Returns `None` if the pointer is null or the bytes are not valid UTF-8.
fn slice_to_string(ptr: *const u8, len: usize) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    std::str::from_utf8(slice).ok().map(str::to_owned)
}

// ── Products: module functions ──────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_products_create_product(
    name_ptr: *const u8,
    name_len: usize,
    price: f64,
    category: i32,
    out_err: *mut weaveffi_error,
) -> weaveffi_handle_t {
    let name = match slice_to_string(name_ptr, name_len) {
        Some(s) => s,
        None => {
            abi::error_set(out_err, 1, "name is null or invalid UTF-8");
            return 0;
        }
    };
    let cat = match Category::from_i32(category) {
        Some(c) => c,
        None => {
            abi::error_set(out_err, 1, "invalid category value");
            return 0;
        }
    };

    let id = NEXT_PRODUCT_ID.fetch_add(1, Ordering::Relaxed);
    let product = Product {
        id,
        name,
        description: None,
        price,
        category: cat,
        tags: Vec::new(),
    };
    PRODUCT_STORE.lock().unwrap().push(product);

    abi::error_set_ok(out_err);
    id as weaveffi_handle_t
}

#[no_mangle]
pub extern "C" fn weaveffi_products_get_product(
    id: weaveffi_handle_t,
    out_err: *mut weaveffi_error,
) -> *mut Product {
    let store = PRODUCT_STORE.lock().unwrap();
    match store.iter().find(|p| p.id == id as i64) {
        Some(p) => {
            abi::error_set_ok(out_err);
            Box::into_raw(Box::new(p.clone()))
        }
        None => {
            abi::error_set(out_err, 1, "product not found");
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_search_products(
    category: i32,
    out_len: *mut usize,
    out_err: *mut weaveffi_error,
) -> *mut *mut Product {
    let cat = match Category::from_i32(category) {
        Some(c) => c,
        None => {
            abi::error_set(out_err, 1, "invalid category value");
            if !out_len.is_null() {
                unsafe { *out_len = 0 };
            }
            return std::ptr::null_mut();
        }
    };

    let store = PRODUCT_STORE.lock().unwrap();
    let matches: Vec<&Product> = store.iter().filter(|p| p.category == cat).collect();
    let len = matches.len();

    if !out_len.is_null() {
        unsafe { *out_len = len };
    }

    if len == 0 {
        abi::error_set_ok(out_err);
        return std::ptr::null_mut();
    }

    let mut ptrs: Vec<*mut Product> = matches
        .iter()
        .map(|p| Box::into_raw(Box::new((*p).clone())))
        .collect();
    let ptr = ptrs.as_mut_ptr();
    std::mem::forget(ptrs);

    abi::error_set_ok(out_err);
    ptr
}

#[no_mangle]
pub extern "C" fn weaveffi_products_update_price(
    id: weaveffi_handle_t,
    price: f64,
    out_err: *mut weaveffi_error,
) -> i32 {
    let mut store = PRODUCT_STORE.lock().unwrap();
    match store.iter_mut().find(|p| p.id == id as i64) {
        Some(p) => {
            p.price = price;
            abi::error_set_ok(out_err);
            1
        }
        None => {
            abi::error_set_ok(out_err);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_delete_product(
    id: weaveffi_handle_t,
    out_err: *mut weaveffi_error,
) -> i32 {
    let mut store = PRODUCT_STORE.lock().unwrap();
    let before = store.len();
    store.retain(|p| p.id != id as i64);
    abi::error_set_ok(out_err);
    (store.len() < before) as i32
}

// ── Products: Product getters ───────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_get_id(product: *const Product) -> i64 {
    assert!(!product.is_null());
    unsafe { (*product).id }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_get_name(product: *const Product) -> *const c_char {
    assert!(!product.is_null());
    abi::string_to_c_ptr(&unsafe { &*product }.name)
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_get_description(
    product: *const Product,
) -> *const c_char {
    assert!(!product.is_null());
    match &unsafe { &*product }.description {
        Some(d) => abi::string_to_c_ptr(d),
        None => std::ptr::null(),
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_get_price(product: *const Product) -> f64 {
    assert!(!product.is_null());
    unsafe { (*product).price }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_get_category(product: *const Product) -> i32 {
    assert!(!product.is_null());
    unsafe { (*product).category as i32 }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_get_tags(
    product: *const Product,
    out_len: *mut usize,
) -> *mut *const c_char {
    assert!(!product.is_null());
    let tags = &unsafe { &*product }.tags;
    let len = tags.len();

    if !out_len.is_null() {
        unsafe { *out_len = len };
    }

    if len == 0 {
        return std::ptr::null_mut();
    }

    let mut ptrs: Vec<*const c_char> = tags.iter().map(abi::string_to_c_ptr).collect();
    let ptr = ptrs.as_mut_ptr();
    std::mem::forget(ptrs);
    ptr
}

// ── Products: Product setters ───────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_set_id(product: *mut Product, id: i64) {
    assert!(!product.is_null());
    unsafe { (*product).id = id };
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_set_name(
    product: *mut Product,
    name_ptr: *const u8,
    name_len: usize,
) {
    assert!(!product.is_null());
    if let Some(s) = slice_to_string(name_ptr, name_len) {
        unsafe { (*product).name = s };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_set_description(
    product: *mut Product,
    description_ptr: *const u8,
    description_len: usize,
) {
    assert!(!product.is_null());
    unsafe { (*product).description = slice_to_string(description_ptr, description_len) };
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_set_price(product: *mut Product, price: f64) {
    assert!(!product.is_null());
    unsafe { (*product).price = price };
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_set_category(product: *mut Product, category: i32) {
    assert!(!product.is_null());
    if let Some(cat) = Category::from_i32(category) {
        unsafe { (*product).category = cat };
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_set_tags(
    product: *mut Product,
    tags: *const *const c_char,
    len: usize,
) {
    assert!(!product.is_null());
    let mut new_tags = Vec::new();
    if !tags.is_null() {
        for i in 0..len {
            let ptr = unsafe { *tags.add(i) };
            if let Some(s) = abi::c_ptr_to_string(ptr) {
                new_tags.push(s);
            }
        }
    }
    unsafe { (*product).tags = new_tags };
}

// ── Products: Category enum conversions ─────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_products_Category_from_i32(
    value: i32,
    out_err: *mut weaveffi_error,
) -> i32 {
    match Category::from_i32(value) {
        Some(ct) => {
            abi::error_set_ok(out_err);
            ct as i32
        }
        None => {
            abi::error_set(out_err, 1, "invalid Category value");
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Category_to_i32(ct: i32) -> i32 {
    ct
}

// ── Products: free functions ────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_destroy(product: *mut Product) {
    if product.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(product)) };
}

#[no_mangle]
pub extern "C" fn weaveffi_products_Product_list_free(products: *mut *mut Product, len: usize) {
    if products.is_null() {
        return;
    }
    let ptrs = unsafe { Vec::from_raw_parts(products, len, len) };
    for ptr in ptrs {
        if !ptr.is_null() {
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_products_string_list_free(strings: *mut *const c_char, len: usize) {
    if strings.is_null() {
        return;
    }
    let ptrs = unsafe { Vec::from_raw_parts(strings, len, len) };
    for ptr in ptrs {
        abi::free_string(ptr);
    }
}

// ── Orders module ───────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OrderItem {
    pub product_id: i64,
    pub quantity: i32,
    pub unit_price: f64,
}

#[derive(Debug, Clone)]
pub struct Order {
    pub id: i64,
    pub items: Vec<OrderItem>,
    pub total: f64,
    pub status: String,
}

static ORDER_STORE: Mutex<Vec<Order>> = Mutex::new(Vec::new());
static NEXT_ORDER_ID: AtomicI64 = AtomicI64::new(1);

// ── Orders: module functions ────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_orders_create_order(
    items: *const *const OrderItem,
    items_len: usize,
    out_err: *mut weaveffi_error,
) -> weaveffi_handle_t {
    let mut order_items = Vec::new();
    if !items.is_null() {
        for i in 0..items_len {
            let ptr = unsafe { *items.add(i) };
            if ptr.is_null() {
                abi::error_set(out_err, 1, "null OrderItem pointer in items array");
                return 0;
            }
            order_items.push(unsafe { (*ptr).clone() });
        }
    }

    let total: f64 = order_items
        .iter()
        .map(|it| it.unit_price * it.quantity as f64)
        .sum();
    let id = NEXT_ORDER_ID.fetch_add(1, Ordering::Relaxed);
    let order = Order {
        id,
        items: order_items,
        total,
        status: "pending".to_string(),
    };
    ORDER_STORE.lock().unwrap().push(order);

    abi::error_set_ok(out_err);
    id as weaveffi_handle_t
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_get_order(
    id: weaveffi_handle_t,
    out_err: *mut weaveffi_error,
) -> *mut Order {
    let store = ORDER_STORE.lock().unwrap();
    match store.iter().find(|o| o.id == id as i64) {
        Some(o) => {
            abi::error_set_ok(out_err);
            Box::into_raw(Box::new(o.clone()))
        }
        None => {
            abi::error_set(out_err, 1, "order not found");
            std::ptr::null_mut()
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_cancel_order(
    id: weaveffi_handle_t,
    out_err: *mut weaveffi_error,
) -> i32 {
    let mut store = ORDER_STORE.lock().unwrap();
    match store.iter_mut().find(|o| o.id == id as i64) {
        Some(o) => {
            if o.status == "cancelled" {
                abi::error_set_ok(out_err);
                return 0;
            }
            o.status = "cancelled".to_string();
            abi::error_set_ok(out_err);
            1
        }
        None => {
            abi::error_set_ok(out_err);
            0
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_add_product_to_order(
    order_id: weaveffi_handle_t,
    product: *const Product,
    out_err: *mut weaveffi_error,
) -> i32 {
    if product.is_null() {
        abi::error_set(out_err, 1, "product is null");
        return 0;
    }
    let p = unsafe { &*product };
    let mut store = ORDER_STORE.lock().unwrap();
    match store.iter_mut().find(|o| o.id == order_id as i64) {
        Some(order) => {
            let item = OrderItem {
                product_id: p.id,
                quantity: 1,
                unit_price: p.price,
            };
            order.total += item.unit_price;
            order.items.push(item);
            abi::error_set_ok(out_err);
            1
        }
        None => {
            abi::error_set_ok(out_err);
            0
        }
    }
}

// ── Orders: OrderItem getters ───────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_get_product_id(item: *const OrderItem) -> i64 {
    assert!(!item.is_null());
    unsafe { (*item).product_id }
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_get_quantity(item: *const OrderItem) -> i32 {
    assert!(!item.is_null());
    unsafe { (*item).quantity }
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_get_unit_price(item: *const OrderItem) -> f64 {
    assert!(!item.is_null());
    unsafe { (*item).unit_price }
}

// ── Orders: OrderItem setters ───────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_set_product_id(item: *mut OrderItem, product_id: i64) {
    assert!(!item.is_null());
    unsafe { (*item).product_id = product_id };
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_set_quantity(item: *mut OrderItem, quantity: i32) {
    assert!(!item.is_null());
    unsafe { (*item).quantity = quantity };
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_set_unit_price(item: *mut OrderItem, unit_price: f64) {
    assert!(!item.is_null());
    unsafe { (*item).unit_price = unit_price };
}

// ── Orders: Order getters ───────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_get_id(order: *const Order) -> i64 {
    assert!(!order.is_null());
    unsafe { (*order).id }
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_get_items(
    order: *const Order,
    out_len: *mut usize,
) -> *mut *mut OrderItem {
    assert!(!order.is_null());
    let items = &unsafe { &*order }.items;
    let len = items.len();

    if !out_len.is_null() {
        unsafe { *out_len = len };
    }

    if len == 0 {
        return std::ptr::null_mut();
    }

    let mut ptrs: Vec<*mut OrderItem> = items
        .iter()
        .map(|it| Box::into_raw(Box::new(it.clone())))
        .collect();
    let ptr = ptrs.as_mut_ptr();
    std::mem::forget(ptrs);
    ptr
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_get_total(order: *const Order) -> f64 {
    assert!(!order.is_null());
    unsafe { (*order).total }
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_get_status(order: *const Order) -> *const c_char {
    assert!(!order.is_null());
    abi::string_to_c_ptr(&unsafe { &*order }.status)
}

// ── Orders: Order setters ───────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_set_id(order: *mut Order, id: i64) {
    assert!(!order.is_null());
    unsafe { (*order).id = id };
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_set_items(
    order: *mut Order,
    items: *const *const OrderItem,
    len: usize,
) {
    assert!(!order.is_null());
    let mut new_items = Vec::new();
    if !items.is_null() {
        for i in 0..len {
            let ptr = unsafe { *items.add(i) };
            if !ptr.is_null() {
                new_items.push(unsafe { (*ptr).clone() });
            }
        }
    }
    unsafe { (*order).items = new_items };
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_set_total(order: *mut Order, total: f64) {
    assert!(!order.is_null());
    unsafe { (*order).total = total };
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_set_status(
    order: *mut Order,
    status_ptr: *const u8,
    status_len: usize,
) {
    assert!(!order.is_null());
    if let Some(s) = slice_to_string(status_ptr, status_len) {
        unsafe { (*order).status = s };
    }
}

// ── Orders: free functions ──────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_destroy(item: *mut OrderItem) {
    if item.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(item)) };
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_OrderItem_list_free(items: *mut *mut OrderItem, len: usize) {
    if items.is_null() {
        return;
    }
    let ptrs = unsafe { Vec::from_raw_parts(items, len, len) };
    for ptr in ptrs {
        if !ptr.is_null() {
            unsafe { drop(Box::from_raw(ptr)) };
        }
    }
}

#[no_mangle]
pub extern "C" fn weaveffi_orders_Order_destroy(order: *mut Order) {
    if order.is_null() {
        return;
    }
    unsafe { drop(Box::from_raw(order)) };
}

// ── Shared free functions ───────────────────────────────────

#[no_mangle]
pub extern "C" fn weaveffi_free_string(ptr: *const c_char) {
    abi::free_string(ptr)
}

#[no_mangle]
pub extern "C" fn weaveffi_error_clear(err: *mut weaveffi_error) {
    abi::error_clear(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn setup() -> std::sync::MutexGuard<'static, ()> {
        let guard = TEST_MUTEX.lock().unwrap();
        PRODUCT_STORE.lock().unwrap().clear();
        NEXT_PRODUCT_ID.store(1, Ordering::Relaxed);
        ORDER_STORE.lock().unwrap().clear();
        NEXT_ORDER_ID.store(1, Ordering::Relaxed);
        guard
    }

    fn new_err() -> weaveffi_error {
        weaveffi_error::default()
    }

    // ── Product tests ───────────────────────────────────────

    #[test]
    fn create_and_get_product() {
        let _g = setup();
        let mut err = new_err();
        let name = "Widget";

        let handle = weaveffi_products_create_product(
            name.as_ptr(),
            name.len(),
            9.99,
            Category::Electronics as i32,
            &mut err,
        );
        assert_eq!(err.code, 0);
        assert!(handle > 0);

        let product = weaveffi_products_get_product(handle, &mut err);
        assert_eq!(err.code, 0);
        assert!(!product.is_null());

        let p = unsafe { &*product };
        assert_eq!(p.name, "Widget");
        assert_eq!(p.price, 9.99);
        assert_eq!(p.category, Category::Electronics);
        assert_eq!(p.description, None);
        assert!(p.tags.is_empty());

        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn create_product_null_name() {
        let _g = setup();
        let mut err = new_err();

        let handle = weaveffi_products_create_product(std::ptr::null(), 0, 9.99, 0, &mut err);
        assert_eq!(handle, 0);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn create_product_invalid_category() {
        let _g = setup();
        let mut err = new_err();
        let name = "Bad";

        let handle =
            weaveffi_products_create_product(name.as_ptr(), name.len(), 9.99, 99, &mut err);
        assert_eq!(handle, 0);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn get_product_not_found() {
        let _g = setup();
        let mut err = new_err();
        let product = weaveffi_products_get_product(999, &mut err);
        assert!(product.is_null());
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn search_products_by_category() {
        let _g = setup();
        let mut err = new_err();
        let n1 = "Laptop";
        let n2 = "Shirt";
        let n3 = "Phone";

        weaveffi_products_create_product(
            n1.as_ptr(),
            n1.len(),
            999.99,
            Category::Electronics as i32,
            &mut err,
        );
        weaveffi_products_create_product(
            n2.as_ptr(),
            n2.len(),
            29.99,
            Category::Clothing as i32,
            &mut err,
        );
        weaveffi_products_create_product(
            n3.as_ptr(),
            n3.len(),
            499.99,
            Category::Electronics as i32,
            &mut err,
        );

        let mut len: usize = 0;
        let results =
            weaveffi_products_search_products(Category::Electronics as i32, &mut len, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(len, 2);
        assert!(!results.is_null());

        weaveffi_products_Product_list_free(results, len);
    }

    #[test]
    fn search_products_empty() {
        let _g = setup();
        let mut err = new_err();
        let mut len: usize = 999;
        let results = weaveffi_products_search_products(Category::Books as i32, &mut len, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(len, 0);
        assert!(results.is_null());
    }

    #[test]
    fn search_products_invalid_category() {
        let _g = setup();
        let mut err = new_err();
        let mut len: usize = 999;
        let results = weaveffi_products_search_products(99, &mut len, &mut err);
        assert_ne!(err.code, 0);
        assert_eq!(len, 0);
        assert!(results.is_null());
        abi::error_clear(&mut err);
    }

    #[test]
    fn update_product_price() {
        let _g = setup();
        let mut err = new_err();
        let name = "Item";

        let handle = weaveffi_products_create_product(name.as_ptr(), name.len(), 10.0, 0, &mut err);
        assert_eq!(weaveffi_products_update_price(handle, 20.0, &mut err), 1);

        let product = weaveffi_products_get_product(handle, &mut err);
        assert_eq!(unsafe { (*product).price }, 20.0);
        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn update_price_not_found() {
        let _g = setup();
        let mut err = new_err();
        assert_eq!(weaveffi_products_update_price(999, 20.0, &mut err), 0);
    }

    #[test]
    fn delete_product() {
        let _g = setup();
        let mut err = new_err();
        let name = "Del";

        let handle = weaveffi_products_create_product(name.as_ptr(), name.len(), 10.0, 0, &mut err);
        assert_eq!(weaveffi_products_delete_product(handle, &mut err), 1);
        assert_eq!(weaveffi_products_delete_product(handle, &mut err), 0);
    }

    #[test]
    fn product_getters_and_setters() {
        let _g = setup();
        let mut err = new_err();
        let name = "Test";

        let handle = weaveffi_products_create_product(
            name.as_ptr(),
            name.len(),
            5.0,
            Category::Food as i32,
            &mut err,
        );
        let product = weaveffi_products_get_product(handle, &mut err);
        assert!(!product.is_null());

        assert_eq!(weaveffi_products_Product_get_id(product), handle as i64);
        assert_eq!(weaveffi_products_Product_get_price(product), 5.0);
        assert_eq!(
            weaveffi_products_Product_get_category(product),
            Category::Food as i32
        );

        let pname = weaveffi_products_Product_get_name(product);
        assert_eq!(abi::c_ptr_to_string(pname).unwrap(), "Test");
        abi::free_string(pname);

        assert!(weaveffi_products_Product_get_description(product).is_null());

        let mut tag_len: usize = 0;
        let tags = weaveffi_products_Product_get_tags(product, &mut tag_len);
        assert_eq!(tag_len, 0);
        assert!(tags.is_null());

        // Setters
        weaveffi_products_Product_set_id(product, 42);
        assert_eq!(weaveffi_products_Product_get_id(product), 42);

        let new_name = "Updated";
        weaveffi_products_Product_set_name(product, new_name.as_ptr(), new_name.len());
        let n = weaveffi_products_Product_get_name(product);
        assert_eq!(abi::c_ptr_to_string(n).unwrap(), "Updated");
        abi::free_string(n);

        let desc = "A description";
        weaveffi_products_Product_set_description(product, desc.as_ptr(), desc.len());
        let d = weaveffi_products_Product_get_description(product);
        assert_eq!(abi::c_ptr_to_string(d).unwrap(), "A description");
        abi::free_string(d);

        weaveffi_products_Product_set_description(product, std::ptr::null(), 0);
        assert!(weaveffi_products_Product_get_description(product).is_null());

        weaveffi_products_Product_set_price(product, 99.99);
        assert_eq!(weaveffi_products_Product_get_price(product), 99.99);

        weaveffi_products_Product_set_category(product, Category::Books as i32);
        assert_eq!(
            weaveffi_products_Product_get_category(product),
            Category::Books as i32
        );

        let tag1 = CString::new("sale").unwrap();
        let tag2 = CString::new("new").unwrap();
        let tag_ptrs: [*const c_char; 2] = [tag1.as_ptr(), tag2.as_ptr()];
        weaveffi_products_Product_set_tags(product, tag_ptrs.as_ptr(), 2);

        let mut tlen: usize = 0;
        let tags_out = weaveffi_products_Product_get_tags(product, &mut tlen);
        assert_eq!(tlen, 2);
        assert!(!tags_out.is_null());

        let t0 = unsafe { *tags_out };
        assert_eq!(abi::c_ptr_to_string(t0).unwrap(), "sale");
        let t1 = unsafe { *tags_out.add(1) };
        assert_eq!(abi::c_ptr_to_string(t1).unwrap(), "new");

        weaveffi_products_string_list_free(tags_out, tlen);
        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn category_conversions() {
        let mut err = new_err();
        assert_eq!(weaveffi_products_Category_from_i32(0, &mut err), 0);
        assert_eq!(err.code, 0);
        assert_eq!(weaveffi_products_Category_from_i32(1, &mut err), 1);
        assert_eq!(weaveffi_products_Category_from_i32(2, &mut err), 2);
        assert_eq!(weaveffi_products_Category_from_i32(3, &mut err), 3);

        assert_eq!(weaveffi_products_Category_from_i32(99, &mut err), -1);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);

        assert_eq!(weaveffi_products_Category_to_i32(0), 0);
        assert_eq!(weaveffi_products_Category_to_i32(3), 3);
    }

    #[test]
    fn free_null_product_is_safe() {
        weaveffi_products_Product_destroy(std::ptr::null_mut());
    }

    #[test]
    fn free_null_product_list_is_safe() {
        weaveffi_products_Product_list_free(std::ptr::null_mut(), 0);
    }

    #[test]
    fn free_null_string_list_is_safe() {
        weaveffi_products_string_list_free(std::ptr::null_mut(), 0);
    }

    // ── Order tests ─────────────────────────────────────────

    #[test]
    fn create_and_get_order() {
        let _g = setup();
        let mut err = new_err();

        let item1 = Box::into_raw(Box::new(OrderItem {
            product_id: 1,
            quantity: 2,
            unit_price: 10.0,
        }));
        let item2 = Box::into_raw(Box::new(OrderItem {
            product_id: 2,
            quantity: 1,
            unit_price: 25.0,
        }));
        let items: [*const OrderItem; 2] = [item1, item2];

        let handle = weaveffi_orders_create_order(items.as_ptr(), 2, &mut err);
        assert_eq!(err.code, 0);
        assert!(handle > 0);

        let order = weaveffi_orders_get_order(handle, &mut err);
        assert_eq!(err.code, 0);
        assert!(!order.is_null());

        let o = unsafe { &*order };
        assert_eq!(o.items.len(), 2);
        assert_eq!(o.total, 45.0);
        assert_eq!(o.status, "pending");

        weaveffi_orders_Order_destroy(order);
        unsafe {
            drop(Box::from_raw(item1));
            drop(Box::from_raw(item2));
        }
    }

    #[test]
    fn create_order_empty_items() {
        let _g = setup();
        let mut err = new_err();

        let handle = weaveffi_orders_create_order(std::ptr::null(), 0, &mut err);
        assert_eq!(err.code, 0);
        assert!(handle > 0);

        let order = weaveffi_orders_get_order(handle, &mut err);
        let o = unsafe { &*order };
        assert!(o.items.is_empty());
        assert_eq!(o.total, 0.0);

        weaveffi_orders_Order_destroy(order);
    }

    #[test]
    fn create_order_null_item_pointer() {
        let _g = setup();
        let mut err = new_err();

        let items: [*const OrderItem; 1] = [std::ptr::null()];
        let handle = weaveffi_orders_create_order(items.as_ptr(), 1, &mut err);
        assert_eq!(handle, 0);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn get_order_not_found() {
        let _g = setup();
        let mut err = new_err();
        let order = weaveffi_orders_get_order(999, &mut err);
        assert!(order.is_null());
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn cancel_order() {
        let _g = setup();
        let mut err = new_err();

        let handle = weaveffi_orders_create_order(std::ptr::null(), 0, &mut err);
        assert_eq!(err.code, 0);

        assert_eq!(weaveffi_orders_cancel_order(handle, &mut err), 1);
        assert_eq!(weaveffi_orders_cancel_order(handle, &mut err), 0);

        let order = weaveffi_orders_get_order(handle, &mut err);
        assert_eq!(unsafe { &*order }.status, "cancelled");
        weaveffi_orders_Order_destroy(order);
    }

    #[test]
    fn cancel_order_not_found() {
        let _g = setup();
        let mut err = new_err();
        assert_eq!(weaveffi_orders_cancel_order(999, &mut err), 0);
    }

    #[test]
    fn order_getters_and_setters() {
        let _g = setup();
        let mut err = new_err();

        let item = Box::into_raw(Box::new(OrderItem {
            product_id: 1,
            quantity: 3,
            unit_price: 10.0,
        }));
        let items: [*const OrderItem; 1] = [item];

        let handle = weaveffi_orders_create_order(items.as_ptr(), 1, &mut err);
        let order = weaveffi_orders_get_order(handle, &mut err);
        assert!(!order.is_null());

        assert_eq!(weaveffi_orders_Order_get_id(order), handle as i64);
        assert_eq!(weaveffi_orders_Order_get_total(order), 30.0);

        let status = weaveffi_orders_Order_get_status(order);
        assert_eq!(abi::c_ptr_to_string(status).unwrap(), "pending");
        abi::free_string(status);

        let mut items_len: usize = 0;
        let items_out = weaveffi_orders_Order_get_items(order, &mut items_len);
        assert_eq!(items_len, 1);
        assert!(!items_out.is_null());
        weaveffi_orders_OrderItem_list_free(items_out, items_len);

        weaveffi_orders_Order_set_id(order, 42);
        assert_eq!(weaveffi_orders_Order_get_id(order), 42);

        weaveffi_orders_Order_set_total(order, 99.99);
        assert_eq!(weaveffi_orders_Order_get_total(order), 99.99);

        let new_status = "shipped";
        weaveffi_orders_Order_set_status(order, new_status.as_ptr(), new_status.len());
        let s = weaveffi_orders_Order_get_status(order);
        assert_eq!(abi::c_ptr_to_string(s).unwrap(), "shipped");
        abi::free_string(s);

        weaveffi_orders_Order_destroy(order);
        unsafe { drop(Box::from_raw(item)) };
    }

    #[test]
    fn order_item_getters() {
        let item = Box::into_raw(Box::new(OrderItem {
            product_id: 5,
            quantity: 2,
            unit_price: 15.0,
        }));

        assert_eq!(weaveffi_orders_OrderItem_get_product_id(item), 5);
        assert_eq!(weaveffi_orders_OrderItem_get_quantity(item), 2);
        assert_eq!(weaveffi_orders_OrderItem_get_unit_price(item), 15.0);

        weaveffi_orders_OrderItem_set_product_id(item, 10);
        assert_eq!(weaveffi_orders_OrderItem_get_product_id(item), 10);

        weaveffi_orders_OrderItem_set_quantity(item, 5);
        assert_eq!(weaveffi_orders_OrderItem_get_quantity(item), 5);

        weaveffi_orders_OrderItem_set_unit_price(item, 20.0);
        assert_eq!(weaveffi_orders_OrderItem_get_unit_price(item), 20.0);

        weaveffi_orders_OrderItem_destroy(item);
    }

    #[test]
    fn free_null_order_is_safe() {
        weaveffi_orders_Order_destroy(std::ptr::null_mut());
    }

    #[test]
    fn free_null_order_item_is_safe() {
        weaveffi_orders_OrderItem_destroy(std::ptr::null_mut());
    }

    #[test]
    fn free_null_order_item_list_is_safe() {
        weaveffi_orders_OrderItem_list_free(std::ptr::null_mut(), 0);
    }

    // ── Cross-module tests ──────────────────────────────────

    #[test]
    fn add_product_to_order_success() {
        let _g = setup();
        let mut err = new_err();
        let name = "Gadget";

        let product_handle = weaveffi_products_create_product(
            name.as_ptr(),
            name.len(),
            49.99,
            Category::Electronics as i32,
            &mut err,
        );
        assert_eq!(err.code, 0);

        let order_handle = weaveffi_orders_create_order(std::ptr::null(), 0, &mut err);
        assert_eq!(err.code, 0);

        let product = weaveffi_products_get_product(product_handle, &mut err);
        assert!(!product.is_null());

        let result = weaveffi_orders_add_product_to_order(order_handle, product, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(result, 1);

        let order = weaveffi_orders_get_order(order_handle, &mut err);
        let o = unsafe { &*order };
        assert_eq!(o.items.len(), 1);
        assert_eq!(o.items[0].product_id, product_handle as i64);
        assert_eq!(o.items[0].unit_price, 49.99);
        assert_eq!(o.items[0].quantity, 1);
        assert_eq!(o.total, 49.99);

        weaveffi_orders_Order_destroy(order);
        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn add_product_to_order_not_found() {
        let _g = setup();
        let mut err = new_err();

        let product = Box::into_raw(Box::new(Product {
            id: 1,
            name: "Test".to_string(),
            description: None,
            price: 10.0,
            category: Category::Electronics,
            tags: Vec::new(),
        }));

        let result = weaveffi_orders_add_product_to_order(999, product, &mut err);
        assert_eq!(err.code, 0);
        assert_eq!(result, 0);

        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn add_product_to_order_null_product() {
        let _g = setup();
        let mut err = new_err();

        let order_handle = weaveffi_orders_create_order(std::ptr::null(), 0, &mut err);
        assert_eq!(err.code, 0);

        let result = weaveffi_orders_add_product_to_order(order_handle, std::ptr::null(), &mut err);
        assert_ne!(err.code, 0);
        assert_eq!(result, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn create_product_accepts_multibyte_utf8() {
        let _g = setup();
        let mut err = new_err();
        let name = "café au lait ☕";

        let handle = weaveffi_products_create_product(
            name.as_ptr(),
            name.len(),
            3.50,
            Category::Food as i32,
            &mut err,
        );
        assert_eq!(err.code, 0);

        let product = weaveffi_products_get_product(handle, &mut err);
        assert_eq!(unsafe { &*product }.name, "café au lait ☕");
        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn create_product_does_not_read_past_len() {
        let _g = setup();
        let mut err = new_err();
        let buf = "WidgetIGNORED";

        let handle = weaveffi_products_create_product(
            buf.as_ptr(),
            6,
            10.0,
            Category::Electronics as i32,
            &mut err,
        );
        assert_eq!(err.code, 0);

        let product = weaveffi_products_get_product(handle, &mut err);
        assert_eq!(unsafe { &*product }.name, "Widget");
        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn create_product_invalid_utf8_name() {
        let _g = setup();
        let mut err = new_err();
        let bad: [u8; 3] = [0xFF, 0xFE, 0xFD];

        let handle = weaveffi_products_create_product(bad.as_ptr(), bad.len(), 1.0, 0, &mut err);
        assert_eq!(handle, 0);
        assert_ne!(err.code, 0);
        abi::error_clear(&mut err);
    }

    #[test]
    fn set_description_uses_byteslice() {
        let _g = setup();
        let mut err = new_err();
        let name = "Item";
        let handle = weaveffi_products_create_product(name.as_ptr(), name.len(), 1.0, 0, &mut err);
        let product = weaveffi_products_get_product(handle, &mut err);

        let desc = "café";
        weaveffi_products_Product_set_description(product, desc.as_ptr(), desc.len());
        assert_eq!(unsafe { &*product }.description, Some("café".to_string()));

        weaveffi_products_Product_destroy(product);
    }

    #[test]
    fn set_status_uses_byteslice() {
        let _g = setup();
        let mut err = new_err();
        let handle = weaveffi_orders_create_order(std::ptr::null(), 0, &mut err);
        let order = weaveffi_orders_get_order(handle, &mut err);

        let status = "expédié";
        weaveffi_orders_Order_set_status(order, status.as_ptr(), status.len());
        assert_eq!(unsafe { &*order }.status, "expédié");

        weaveffi_orders_Order_destroy(order);
    }

    #[test]
    fn add_multiple_products_to_order() {
        let _g = setup();
        let mut err = new_err();

        let n1 = "Widget";
        let n2 = "Gizmo";

        let h1 = weaveffi_products_create_product(n1.as_ptr(), n1.len(), 10.0, 0, &mut err);
        let h2 = weaveffi_products_create_product(n2.as_ptr(), n2.len(), 25.0, 1, &mut err);

        let order_handle = weaveffi_orders_create_order(std::ptr::null(), 0, &mut err);

        let p1 = weaveffi_products_get_product(h1, &mut err);
        let p2 = weaveffi_products_get_product(h2, &mut err);

        assert_eq!(
            weaveffi_orders_add_product_to_order(order_handle, p1, &mut err),
            1
        );
        assert_eq!(
            weaveffi_orders_add_product_to_order(order_handle, p2, &mut err),
            1
        );

        let order = weaveffi_orders_get_order(order_handle, &mut err);
        let o = unsafe { &*order };
        assert_eq!(o.items.len(), 2);
        assert_eq!(o.total, 35.0);

        weaveffi_orders_Order_destroy(order);
        weaveffi_products_Product_destroy(p1);
        weaveffi_products_Product_destroy(p2);
    }
}
