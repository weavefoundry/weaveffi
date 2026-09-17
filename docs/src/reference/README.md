# Reference

The reference pages state the contracts the rest of the documentation
assumes. Two of them are normative: the [C ABI Contract](abi.md) and the
[Value Buffer Protocol](value-buffers.md) define what a producer exports and
what a consumer may rely on, and every generator, sample, and conformance
lane is checked against them. The others summarize those contracts from a
particular angle and point back to the normative text rather than
restating it.

| Page | Read it when you need |
|------|-----------------------|
| [IDL Reference](idl.md) | The complete schema `0.9.0` grammar: every field, every type position, every `ValidationError` code with its message and suggestion, and validated examples of each construct |
| [C ABI Contract](abi.md) | The normative ABI revision 2: symbol shapes, ownership by position, object reference counting, callback vtables, error codes, cancellation, and the revision check |
| [Value Buffer Protocol](value-buffers.md) | The byte-level encoding of records, rich enums, optionals, lists, maps, object tokens, and structured error payloads |
| [Memory and Error Model](memory-error.md) | A working summary of who frees what: strings, bytes, buffers, `_clone`/`_destroy`, callback `free`, `weaveffi_error_set`/`_clear`/`_free`, and cancel tokens |
| [Naming and Package Conventions](naming.md) | How the project names itself and how the generators spell C symbols, wrapper types, error classes, and the Kotlin package |

For the pipeline that turns an IDL into these outputs, see
[Architecture](../architecture.md); for per-language usage, see the
generator pages.
