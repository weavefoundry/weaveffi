# Inventory sample

A multi-module WeaveFFI sample that ships two modules (`products` and
`orders`) so the generators exercise cross-module struct references,
nested struct lists, and list-of-string fields.

## What this sample demonstrates

- **Multiple modules in a single IDL** — `products` and `orders` both
  appear in one `inventory.yml` and become sibling namespaces in every
  generator (e.g. `weaveffi_products_*` and `weaveffi_orders_*` in C).
- A **`Category` enum** (`Electronics`, `Clothing`, `Food`, `Books`) used as
  both a function parameter and a struct field.
- A **`Product` struct** with a full range of field kinds:
  - `i64`, `f64`, `string`
  - `string?` (optional)
  - enum-typed field (`category: Category`)
  - list-of-string field (`tags: [string]`)
- A **`OrderItem` struct** and an **`Order` struct** whose `items` field is
  `[OrderItem]`, exercising **nested struct lists**.
- **Cross-module struct passing** — `orders.add_product_to_order` takes a
  `Product` (defined in the `products` module) as a parameter.
- A **list-returning search function** — `search_products(category)` returns
  `[Product]` filtered by category.
- A small CRUD surface (`create_product`, `get_product`, `update_price`,
  `delete_product`, `create_order`, `get_order`, `cancel_order`).

## IDL highlights

From [`inventory.yml`](inventory.yml):

```yaml
modules:
  - name: products
    enums:
      - name: Category
        variants: [Electronics, Clothing, Food, Books]
    structs:
      - name: Product
        fields:
          - { name: name,        type: string }
          - { name: description, type: "string?" }
          - { name: price,       type: f64 }
          - { name: category,    type: Category }
          - { name: tags,        type: "[string]" }
    functions:
      - { name: search_products, return: "[Product]", params: [...] }

  - name: orders
    structs:
      - name: OrderItem
        fields: [product_id, quantity, unit_price]
      - name: Order
        fields:
          - { name: items, type: "[OrderItem]" }
          - { name: total, type: f64 }
    functions:
      - name: add_product_to_order
        params:
          - { name: order_id, type: handle }
          - { name: product,  type: Product }   # cross-module reference
        return: bool
```

Key IDL features exercised:

- `type: "[string]"` — list of primitives as a struct field.
- `type: "[OrderItem]"` — list of structs as a struct field.
- `type: Product` inside `orders` — a struct reference resolved across
  modules.
- `return: "[Product]"` with a filter parameter.

## Generate bindings

Run the following from the repo root. Omit `--target` to generate bindings
for **all** supported targets.

```bash
# All targets
cargo run -p weaveffi-cli -- generate samples/inventory/inventory.yml -o generated

# A single target
cargo run -p weaveffi-cli -- generate samples/inventory/inventory.yml -o generated --target c

# A comma-separated subset
cargo run -p weaveffi-cli -- generate samples/inventory/inventory.yml -o generated --target c,cpp,dotnet
```

Supported `--target` values: `c`, `cpp`, `swift`, `android`, `node`, `wasm`,
`python`, `dotnet`, `dart`, `go`, `ruby`.

## What to look for in the generated output

- **Two module namespaces** — every output file carries symbols for both
  modules: C prototypes split into `weaveffi_products_*` and
  `weaveffi_orders_*`, Swift/Python/Kotlin types grouped in the same file
  but with distinct class/module prefixes.
- **`generated/c/weaveffi.h`** — opaque typedefs
  `weaveffi_products_Product` and `weaveffi_orders_Order`, the
  `weaveffi_products_Category` enum, and prototypes like
  `weaveffi_products_search_products(int32_t category, size_t* out_len, weaveffi_error* err)`
  and `weaveffi_orders_add_product_to_order(weaveffi_handle_t order_id, const weaveffi_products_Product* product, weaveffi_error* err)`.
- **Tag-list accessors on `Product`** — generators emit a pair of
  `weaveffi_products_Product_get_tags(product, out_len, err)` and
  `weaveffi_products_Product_set_tags(product, tags_ptr, tags_len)` so the
  `[string]` field round-trips across the boundary.
- **Nested list accessors on `Order`** — `Order.items` is materialised as a
  `[OrderItem]` getter returning `*mut *mut OrderItem` + `out_len`, paired
  with a `weaveffi_orders_OrderItem_list_free` helper.
- **`generated/swift/Sources/WeaveFFI/WeaveFFI.swift`** — two groups of
  wrappers: `public class Product` / `public enum Category` /
  `public func searchProducts(category:) -> [Product]` and
  `public class Order` / `public class OrderItem` /
  `public func addProductToOrder(orderId:product:) -> Bool`.
- **`generated/python/weaveffi/__init__.py`** — a single package with
  `Product`, `Order`, `OrderItem` classes, `Category` enum, and
  module-level functions (`search_products`, `create_order`,
  `add_product_to_order`) that type-check lists of structs.
- **`generated/node/types.d.ts`** — `export declare class Product`,
  `export declare class Order`, `readonly items: OrderItem[]`, and
  `function products_search_products(category: Category): Product[]`.

## Build the cdylib

From the repo root:

```bash
cargo build -p inventory
```

This produces the shared library under `target/debug/`:

- macOS: `target/debug/libinventory.dylib`
- Linux: `target/debug/libinventory.so`
- Windows: `target\debug\inventory.dll`
