// Conformance consumer: inventory sample, C target (ABI revision 2).
//
// The only multi-module sample: the `products` module (the `Catalog`
// interface, the `Product` record with an optional and a list field, the
// `Category` enum, the `ProductsError` domain) and the `orders` module (free
// functions over `OrderItem`/`Order` records, the `OrdersError` domain, and
// `add_product_to_order`, which takes a `products::Product` across the
// module boundary). Both error domains reuse the small codes 1 and 2, so the
// consumer checks the doc-comment messages as well as the codes to prove
// each function reports its own domain. Also covers `Catalog` reference
// counting (`_clone`/`_destroy`). Exits 0 on success; aborts on any mismatch.

#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "weaveffi.h"
#include "wvbuf.h"

// ── products::Product ──────────────────────────────────────────────────────
// id, name, description?, price, category, tags.

typedef struct {
    int64_t id;
    char* name;
    char* description;  // NULL when absent
    double price;
    int32_t category;
    char** tags;
    uint32_t tags_n;
} product_t;

static void read_product(wv_reader* r, product_t* p) {
    memset(p, 0, sizeof *p);
    p->id = wv_get_i64(r);
    p->name = wv_get_str(r);
    p->description = wv_get_bool(r) ? wv_get_str(r) : NULL;
    p->price = wv_get_f64(r);
    p->category = wv_get_i32(r);
    p->tags_n = wv_get_u32(r);
    p->tags = (char**)calloc(p->tags_n ? p->tags_n : 1, sizeof(char*));
    assert(p->tags != NULL);
    for (uint32_t i = 0; i < p->tags_n; i++) p->tags[i] = wv_get_str(r);
}

static void write_product(wv_writer* w, const product_t* p) {
    wv_put_i64(w, p->id);
    wv_put_str(w, p->name);
    wv_put_bool(w, p->description != NULL);
    if (p->description) wv_put_str(w, p->description);
    wv_put_f64(w, p->price);
    wv_put_i32(w, p->category);
    wv_put_u32(w, p->tags_n);
    for (uint32_t i = 0; i < p->tags_n; i++) wv_put_str(w, p->tags[i]);
}

static void product_free(product_t* p) {
    free(p->name);
    free(p->description);
    for (uint32_t i = 0; i < p->tags_n; i++) free(p->tags[i]);
    free(p->tags);
    memset(p, 0, sizeof *p);
}

// Decode one Product from an owned return buffer, then release the buffer.
static void take_product(const uint8_t* ptr, size_t len, product_t* p) {
    assert(ptr != NULL);
    wv_reader r;
    wv_r_init(&r, ptr, len);
    read_product(&r, p);
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)ptr, len);
}

// ── orders::OrderItem / orders::Order ──────────────────────────────────────
// OrderItem: product_id, quantity (i32), unit_price. Order: id, items, total,
// status.

typedef struct {
    int64_t product_id;
    int32_t quantity;
    double unit_price;
} order_item_t;

typedef struct {
    int64_t id;
    order_item_t* items;
    uint32_t items_n;
    double total;
    char* status;
} order_t;

static void write_items(wv_writer* w, const order_item_t* items, uint32_t n) {
    wv_put_u32(w, n);
    for (uint32_t i = 0; i < n; i++) {
        wv_put_i64(w, items[i].product_id);
        wv_put_i32(w, items[i].quantity);
        wv_put_f64(w, items[i].unit_price);
    }
}

static void take_order(const uint8_t* ptr, size_t len, order_t* o) {
    assert(ptr != NULL);
    wv_reader r;
    wv_r_init(&r, ptr, len);
    o->id = wv_get_i64(&r);
    o->items_n = wv_get_u32(&r);
    o->items = (order_item_t*)calloc(o->items_n ? o->items_n : 1, sizeof(order_item_t));
    assert(o->items != NULL);
    for (uint32_t i = 0; i < o->items_n; i++) {
        o->items[i].product_id = wv_get_i64(&r);
        o->items[i].quantity = wv_get_i32(&r);
        o->items[i].unit_price = wv_get_f64(&r);
    }
    o->total = wv_get_f64(&r);
    o->status = wv_get_str(&r);
    wv_r_expect_end(&r);
    weaveffi_free_bytes((uint8_t*)ptr, len);
}

static void order_free(order_t* o) {
    free(o->items);
    free(o->status);
    memset(o, 0, sizeof *o);
}

static int64_t create_order(const order_item_t* items, uint32_t n, weaveffi_error* err) {
    wv_writer w;
    wv_w_init(&w);
    write_items(&w, items, n);
    int64_t id = weaveffi_orders_create_order(w.buf, w.len, err);
    wv_w_free(&w);
    return id;
}

static void get_order(int64_t id, order_t* o) {
    weaveffi_error err = {0};
    size_t len = 0;
    const uint8_t* p = weaveffi_orders_get_order(id, &len, &err);
    assert(err.code == 0);
    take_order(p, len, o);
}

static int approx(double a, double b) { return fabs(a - b) < 1e-9; }

int main(void) {
    weaveffi_error err = {0};
    size_t out_len = 0;

    assert(weaveffi_abi_version() == 2u && WEAVEFFI_ABI_VERSION == 2u);

    // ── products ─────────────────────────────────────────────────────────
    weaveffi_products_Catalog* catalog = weaveffi_products_Catalog_new(&err);
    assert(err.code == 0 && catalog != NULL);

    // add_product -> Product buffer with a fresh id, absent description, and
    // no tags.
    const uint8_t* buf = weaveffi_products_Catalog_add_product(
        catalog, "Widget", 9.99, weaveffi_products_Category_Electronics, &out_len, &err);
    assert(err.code == 0);
    product_t widget;
    take_product(buf, out_len, &widget);
    assert(widget.id == 1 && "each catalog hands out ids from 1");
    assert(strcmp(widget.name, "Widget") == 0);
    assert(widget.description == NULL);
    assert(widget.price == 9.99);
    assert(widget.category == weaveffi_products_Category_Electronics);
    assert(widget.tags_n == 0);

    // ProductsError.InvalidPrice (1) with its doc-comment message.
    assert(weaveffi_products_Catalog_add_product(catalog, "Free", 0.0,
                                                 weaveffi_products_Category_Food, &out_len,
                                                 &err) == NULL);
    assert(err.code == weaveffi_products_ProductsError_InvalidPrice);
    assert(err.message != NULL && strcmp(err.message, "price must be positive") == 0);
    assert(err.payload_ptr == NULL && err.payload_len == 0);
    weaveffi_error_clear(&err);
    assert(err.code == 0 && err.message == NULL);

    // ProductsError.ProductNotFound (2).
    assert(weaveffi_products_Catalog_get_product(catalog, 999, &out_len, &err) == NULL);
    assert(err.code == weaveffi_products_ProductsError_ProductNotFound);
    assert(strcmp(err.message, "product not found") == 0);
    weaveffi_error_clear(&err);

    // An undeclared Category discriminant is a marshalling failure (-3).
    assert(weaveffi_products_Catalog_add_product(catalog, "Odd", 1.0,
                                                 (weaveffi_products_Category)42, &out_len,
                                                 &err) == NULL);
    assert(err.code == -3);
    weaveffi_error_clear(&err);

    // Two more products, then search by shelf: a buffered list of records.
    buf = weaveffi_products_Catalog_add_product(
        catalog, "Shirt", 29.99, weaveffi_products_Category_Clothing, &out_len, &err);
    assert(err.code == 0);
    product_t shirt;
    take_product(buf, out_len, &shirt);
    assert(shirt.id == 2 && shirt.category == weaveffi_products_Category_Clothing);
    buf = weaveffi_products_Catalog_add_product(
        catalog, "Phone", 499.99, weaveffi_products_Category_Electronics, &out_len, &err);
    assert(err.code == 0);
    product_t phone;
    take_product(buf, out_len, &phone);
    assert(phone.id == 3);

    buf = weaveffi_products_Catalog_search(catalog, weaveffi_products_Category_Electronics,
                                           &out_len, &err);
    assert(err.code == 0 && buf != NULL);
    {
        wv_reader r;
        wv_r_init(&r, buf, out_len);
        assert(wv_get_u32(&r) == 2);
        product_t a, b;
        read_product(&r, &a);
        read_product(&r, &b);
        wv_r_expect_end(&r);
        assert(a.id == 1 && strcmp(a.name, "Widget") == 0);
        assert(b.id == 3 && strcmp(b.name, "Phone") == 0 && b.price == 499.99);
        product_free(&a);
        product_free(&b);
    }
    weaveffi_free_bytes((uint8_t*)buf, out_len);
    buf = weaveffi_products_Catalog_search(catalog, weaveffi_products_Category_Books, &out_len,
                                           &err);
    assert(err.code == 0 && buf != NULL);
    {
        wv_reader r;
        wv_r_init(&r, buf, out_len);
        assert(wv_get_u32(&r) == 0 && "empty list is just a zero count");
        wv_r_expect_end(&r);
    }
    weaveffi_free_bytes((uint8_t*)buf, out_len);

    // update_price: bool return, typed error path.
    assert(weaveffi_products_Catalog_update_price(catalog, widget.id, 20.0, &err));
    assert(err.code == 0);
    buf = weaveffi_products_Catalog_get_product(catalog, widget.id, &out_len, &err);
    assert(err.code == 0);
    product_t fetched;
    take_product(buf, out_len, &fetched);
    assert(fetched.price == 20.0 && strcmp(fetched.name, "Widget") == 0);
    product_free(&fetched);
    assert(!weaveffi_products_Catalog_update_price(catalog, 999, 1.0, &err));
    assert(err.code == 0);
    assert(!weaveffi_products_Catalog_update_price(catalog, widget.id, -5.0, &err));
    assert(err.code == weaveffi_products_ProductsError_InvalidPrice);
    weaveffi_error_clear(&err);

    // remove: true once, then false.
    assert(weaveffi_products_Catalog_remove(catalog, shirt.id, &err) && err.code == 0);
    assert(!weaveffi_products_Catalog_remove(catalog, shirt.id, &err) && err.code == 0);
    assert(weaveffi_products_Catalog_get_product(catalog, shirt.id, &out_len, &err) == NULL);
    assert(err.code == weaveffi_products_ProductsError_ProductNotFound);
    weaveffi_error_clear(&err);

    // Reference counting: a clone is the same pointer and outlives the
    // original reference.
    weaveffi_products_Catalog* again = weaveffi_products_Catalog_clone(catalog);
    assert(again == catalog);
    weaveffi_products_Catalog_destroy(catalog);
    buf = weaveffi_products_Catalog_get_product(again, phone.id, &out_len, &err);
    assert(err.code == 0);
    take_product(buf, out_len, &fetched);
    assert(strcmp(fetched.name, "Phone") == 0);
    product_free(&fetched);
    catalog = again;

    // A second catalog is independent (its own ids and products).
    weaveffi_products_Catalog* other = weaveffi_products_Catalog_new(&err);
    buf = weaveffi_products_Catalog_add_product(other, "Novel", 12.5,
                                                weaveffi_products_Category_Books, &out_len,
                                                &err);
    assert(err.code == 0);
    take_product(buf, out_len, &fetched);
    assert(fetched.id == 1 && fetched.category == weaveffi_products_Category_Books);
    product_free(&fetched);
    assert(weaveffi_products_Catalog_get_product(other, phone.id, &out_len, &err) == NULL);
    assert(err.code == weaveffi_products_ProductsError_ProductNotFound);
    weaveffi_error_clear(&err);
    weaveffi_products_Catalog_destroy(other);

    // ── orders ───────────────────────────────────────────────────────────
    // create_order takes a buffered list of records and returns an id.
    order_item_t items[2] = {{1, 2, 10.0}, {2, 1, 25.0}};
    int64_t order_id = create_order(items, 2, &err);
    assert(err.code == 0 && order_id >= 1);

    order_t order;
    get_order(order_id, &order);
    assert(order.id == order_id);
    assert(order.items_n == 2);
    assert(order.items[0].product_id == 1 && order.items[0].quantity == 2 &&
           order.items[0].unit_price == 10.0);
    assert(order.items[1].product_id == 2 && order.items[1].quantity == 1 &&
           order.items[1].unit_price == 25.0);
    assert(order.total == 45.0);
    assert(strcmp(order.status, "pending") == 0);
    order_free(&order);

    // OrdersError.EmptyOrder (2): same numeric code as ProductNotFound but a
    // different domain, distinguishable by message.
    assert(create_order(NULL, 0, &err) == 0);
    assert(err.code == weaveffi_orders_OrdersError_EmptyOrder);
    assert(strcmp(err.message, "order must contain at least one item") == 0);
    weaveffi_error_clear(&err);

    // OrdersError.OrderNotFound (1).
    assert(weaveffi_orders_get_order(999, &out_len, &err) == NULL);
    assert(err.code == weaveffi_orders_OrdersError_OrderNotFound);
    assert(strcmp(err.message, "order not found") == 0);
    weaveffi_error_clear(&err);

    // cancel_order: true on the transition, false afterward and for unknown
    // ids; status is visible through get_order.
    assert(weaveffi_orders_cancel_order(order_id, &err) && err.code == 0);
    assert(!weaveffi_orders_cancel_order(order_id, &err) && err.code == 0);
    assert(!weaveffi_orders_cancel_order(999, &err) && err.code == 0);
    get_order(order_id, &order);
    assert(strcmp(order.status, "cancelled") == 0);
    order_free(&order);

    // ── cross-module: a products::Product parameter in the orders module ─
    // Hand-build a Product with the optional description present and tags.
    order_item_t seed[1] = {{0, 1, 1.0}};
    int64_t order2 = create_order(seed, 1, &err);
    assert(err.code == 0 && order2 == order_id + 1);

    product_t gadget;
    memset(&gadget, 0, sizeof gadget);
    gadget.id = 77;
    gadget.name = strdup("Gadget");
    gadget.description = strdup("shiny \xE2\x9C\x93");
    gadget.price = 49.99;
    gadget.category = weaveffi_products_Category_Electronics;
    gadget.tags_n = 2;
    gadget.tags = (char**)calloc(2, sizeof(char*));
    gadget.tags[0] = strdup("new");
    gadget.tags[1] = strdup("");
    wv_writer pw;
    wv_w_init(&pw);
    write_product(&pw, &gadget);
    assert(weaveffi_orders_add_product_to_order(order2, pw.buf, pw.len, &err));
    assert(err.code == 0);
    assert(!weaveffi_orders_add_product_to_order(999, pw.buf, pw.len, &err));
    assert(err.code == 0);
    wv_w_free(&pw);

    get_order(order2, &order);
    assert(order.items_n == 2);
    assert(order.items[1].product_id == 77 && order.items[1].quantity == 1 &&
           order.items[1].unit_price == 49.99);
    assert(approx(order.total, 50.99));
    assert(strcmp(order.status, "pending") == 0);
    order_free(&order);

    // The producer-made Product round-trips into an order too.
    wv_w_init(&pw);
    write_product(&pw, &phone);
    assert(weaveffi_orders_add_product_to_order(order2, pw.buf, pw.len, &err));
    wv_w_free(&pw);
    get_order(order2, &order);
    assert(order.items_n == 3 && order.items[2].product_id == 3 &&
           order.items[2].unit_price == 499.99);
    assert(approx(order.total, 550.98));
    order_free(&order);

    // A truncated Product buffer is a marshalling failure (-3).
    {
        uint8_t truncated[2] = {1, 0};
        assert(!weaveffi_orders_add_product_to_order(order2, truncated, 2, &err));
        assert(err.code == -3);
        weaveffi_error_clear(&err);
    }

    product_free(&gadget);
    product_free(&widget);
    product_free(&shirt);
    product_free(&phone);
    weaveffi_products_Catalog_destroy(catalog);
    weaveffi_products_Catalog_destroy(NULL);

    printf("c/inventory: OK\n");
    return 0;
}
