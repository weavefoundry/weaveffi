"""Conformance consumer: inventory sample, Python target.

Drives the only multi-module sample through the generated ctypes wrapper. The
`products` module contributes the `Catalog` interface (constructor, `with`
statement, idempotent `close()`), the `Product` record with an optional
string, an enum, and a list field, and the `ProductsError` domain; the
`orders` module contributes free functions over `Order`/`OrderItem` records
(a list-of-record parameter, a buffered record return) and the `OrdersError`
domain. Both domains reuse the codes 1 and 2, so each throwing callable must
map its code to its own module's classes. `add_product_to_order` takes a
`products.Product` across the module boundary. The generated package is
placed on sys.path via WV_PY; the cdylib is selected with WEAVEFFI_LIBRARY.
"""
import os
import sys

sys.path.insert(0, os.environ["WV_PY"])

import inventory as wv  # noqa: E402


def check(cond: bool, what: str) -> None:
    if not cond:
        print(f"python/inventory: FAIL: {what}", file=sys.stderr)
        sys.exit(1)


def expect(fn, cls, code: int, message: str, what: str) -> None:
    try:
        fn()
    except cls as exc:
        check(exc.code == code and exc.CODE == code, f"{what}: code {exc.code}")
        check(exc.message == message, f"{what}: message {exc.message!r}")
        check(isinstance(exc, wv.WeaveFFIError), f"{what}: hierarchy")
        return
    check(False, f"{what}: expected {cls.__name__}")


def products() -> wv.Product:
    check(wv.Category.Electronics == 0 and wv.Category.Books == 3, "Category discriminants")
    check(wv.InvalidPrice is wv.ProductsError.InvalidPrice
          and wv.ProductNotFound is wv.ProductsError.ProductNotFound, "products aliases")
    check(issubclass(wv.ProductsError, wv.WeaveFFIError)
          and not issubclass(wv.ProductsError, wv.OrdersError), "domains are distinct classes")

    with wv.Catalog() as catalog:
        widget = catalog.add_product("Widget", 9.99, wv.Category.Electronics)
        check(isinstance(widget, wv.Product), "add_product returns a Product")
        check(widget.id > 0 and widget.name == "Widget" and widget.price == 9.99, f"widget {widget}")
        check(widget.category is wv.Category.Electronics, "category enum member")
        check(widget.description is None and widget.tags == [], "absent optional / empty list")
        check(widget == wv.Product(id=widget.id, name="Widget", description=None, price=9.99,
                                   category=wv.Category.Electronics, tags=[]),
              "Product value equality")

        shirt = catalog.add_product("Shirt", 29.5, wv.Category.Clothing)
        phone = catalog.add_product("Phöne 📱", 499.0, wv.Category.Electronics)
        check(shirt.id == widget.id + 1 and phone.id == shirt.id + 1, "ids are monotonic")
        check(phone.name == "Phöne 📱", f"unicode name {phone.name!r}")

        # Typed errors in the products domain (codes 1 and 2).
        expect(lambda: catalog.add_product("Free", 0.0, wv.Category.Food),
               wv.ProductsError.InvalidPrice, 1, "price must be positive", "add_product(0.0)")
        expect(lambda: catalog.add_product("Debt", -5.0, wv.Category.Food),
               wv.ProductsError.InvalidPrice, 1, "price must be positive", "add_product(-5.0)")
        expect(lambda: catalog.get_product(999), wv.ProductsError.ProductNotFound, 2,
               "product not found", "get_product(999)")

        fetched = catalog.get_product(shirt.id)
        check(fetched == shirt, f"get_product round-trips {fetched}")

        # List-of-record return.
        electronics = catalog.search(wv.Category.Electronics)
        check([p.name for p in electronics] == ["Widget", "Phöne 📱"], f"search {electronics}")
        check(catalog.search(wv.Category.Books) == [], "search on an empty shelf")

        check(catalog.update_price(widget.id, 12.25) is True, "update_price existing")
        check(catalog.get_product(widget.id).price == 12.25, "price updated")
        check(catalog.update_price(999, 1.0) is False, "update_price missing is False")
        expect(lambda: catalog.update_price(widget.id, -1.0), wv.InvalidPrice, 1,
               "price must be positive", "update_price(-1.0)")

        check(catalog.remove(shirt.id) is True, "remove existing")
        check(catalog.remove(shirt.id) is False, "remove missing is False")
        expect(lambda: catalog.get_product(shirt.id), wv.ProductNotFound, 2,
               "product not found", "get_product after remove")

        # Catalogs are independent objects.
        other = wv.Catalog()
        check(other.search(wv.Category.Electronics) == [], "second catalog is empty")
        other.close()
        other.close()
        try:
            other.remove(1)
            check(False, "expected use-after-close error")
        except wv.WeaveFFIError as exc:
            check("after close" in exc.message, f"use-after-close message {exc.message!r}")
        closed = catalog
        gadget = catalog.get_product(phone.id)
    closed.close()
    try:
        closed.search(wv.Category.Food)
        check(False, "expected use-after-close error after with")
    except wv.WeaveFFIError:
        pass
    return gadget


def orders(product: wv.Product) -> None:
    check(wv.OrderNotFound is wv.OrdersError.OrderNotFound
          and wv.EmptyOrder is wv.OrdersError.EmptyOrder, "orders aliases")

    # List-of-record parameter in one value buffer; buffered record return.
    items = [wv.OrderItem(product_id=1, quantity=2, unit_price=10.0),
             wv.OrderItem(product_id=2, quantity=1, unit_price=25.0)]
    order_id = wv.create_order(items)
    check(order_id > 0, f"create_order id {order_id}")
    order = wv.get_order(order_id)
    check(isinstance(order, wv.Order), "get_order returns an Order")
    check(order.id == order_id and order.status == "pending", f"order {order}")
    check(order.items == items, f"items round-trip {order.items}")
    check(order.total == 45.0, f"total {order.total}")
    check(order == wv.Order(id=order_id, items=items, total=45.0, status="pending"),
          "Order value equality")

    # Typed errors in the orders domain: the same numeric codes as the
    # products domain resolve to the orders classes.
    expect(lambda: wv.create_order([]), wv.OrdersError.EmptyOrder, 2,
           "order must contain at least one item", "create_order([])")
    expect(lambda: wv.get_order(999), wv.OrdersError.OrderNotFound, 1,
           "order not found", "get_order(999)")
    try:
        wv.get_order(999)
    except wv.ProductsError:
        check(False, "orders code 1 must not map to ProductsError.InvalidPrice")
    except wv.OrdersError:
        pass

    # Cross-module record parameter: a products.Product appended to an order.
    check(wv.add_product_to_order(order_id, product) is True, "add_product_to_order")
    order = wv.get_order(order_id)
    check(len(order.items) == 3, f"items after add {order.items}")
    check(order.items[2] == wv.OrderItem(product_id=product.id, quantity=1, unit_price=499.0),
          f"appended item {order.items[2]}")
    check(order.total == 45.0 + 499.0, f"total after add {order.total}")
    check(wv.add_product_to_order(999, product) is False, "add_product_to_order missing order")
    # A locally built Product with every field populated crosses too.
    local = wv.Product(id=77, name="Local", description="built in Python", price=0.5,
                       category=wv.Category.Books, tags=["x", "yy"])
    check(wv.add_product_to_order(order_id, local) is True, "add local product")
    check(wv.get_order(order_id).items[3].product_id == 77, "local product id recorded")

    check(wv.cancel_order(order_id) is True, "cancel_order")
    check(wv.cancel_order(order_id) is False, "cancel_order twice is False")
    check(wv.get_order(order_id).status == "cancelled", "status after cancel")
    check(wv.cancel_order(999) is False, "cancel_order missing is False")

    second = wv.create_order([wv.OrderItem(product_id=3, quantity=0, unit_price=1e9)])
    check(second == order_id + 1, "order ids are monotonic")
    check(wv.get_order(second).total == 0.0, "zero quantity yields zero total")


def main() -> None:
    gadget = products()
    orders(gadget)
    print("python/inventory: OK")


main()
