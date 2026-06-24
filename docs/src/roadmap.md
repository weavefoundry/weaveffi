# Roadmap

WeaveFFI is in active `0.x` development. The [CHANGELOG][changelog] is the
source of truth for what has shipped, and [Stability and
Versioning](stability.md) explains how releases and schema versioning work.

## Producer macro feature coverage

The `#[weaveffi::module]` macro (see [The Rust Producer
Macro](guides/producer-macro.md)) generates the producer's C ABI glue for the
full IDL feature set. Every sample under `samples/` is now macro-annotated safe
Rust with no hand-written `extern "C"` layer; the features below are first-class
in the IDL, the validator, every generator, and the macro's producer-side
codegen.

| Feature | Macro codegen | Reference sample |
|---------|---------------|------------------|
| Modules, records, C-style enums, sync functions, `Result` errors | Shipped | `calculator`, `contacts`, `inventory` |
| Scalars, strings, bytes, handles, optionals, lists (incl. record lists) | Shipped | `contacts`, `inventory` |
| Cross-module record and enum references | Shipped | `inventory` |
| Async (and cancellable) functions | Shipped | `async-demo`, `kvstore` |
| Callbacks and event listeners | Shipped | `events`, `kvstore` |
| Iterator returns | Shipped | `events`, `kvstore` |
| Rich (data-carrying) enums | Shipped | `shapes` |
| Maps | Shipped | `kvstore` |
| Builder records | Shipped | `kvstore` |

A module that uses a shape the macro can't express (for example an iterator
parameter or a tuple-style rich-enum variant) fails to compile with a clear
message naming the gap, so the macro never emits glue that disagrees with the
header.

[changelog]: https://github.com/weavefoundry/weaveffi/blob/main/CHANGELOG.md
