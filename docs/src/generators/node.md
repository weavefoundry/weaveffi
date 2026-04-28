# Node.js

## Overview

The Node.js target produces a CommonJS loader plus TypeScript type
definitions. The actual native bridging happens in an N-API addon
(`samples/node-addon` for the in-tree examples) which the loader picks
up as `index.node`. The generator focuses on the consumer-facing surface
so that downstream projects can ship the same `.node` file with typed JS
bindings.

## What gets generated

| File | Purpose |
|------|---------|
| `generated/node/index.js` | CommonJS loader that requires `./index.node` |
| `generated/node/types.d.ts` | TypeScript declarations for the public surface |
| `generated/node/package.json` | npm package metadata (`main`, `types`) |

## Type mapping

| IDL type      | TypeScript type      |
|---------------|----------------------|
| `i32`         | `number`             |
| `u32`         | `number`             |
| `i64`         | `number`             |
| `f64`         | `number`             |
| `bool`        | `boolean`            |
| `string`      | `string`             |
| `bytes`       | `Buffer`             |
| `handle`      | `bigint`             |
| `StructName`  | `StructName`         |
| `EnumName`    | `EnumName`           |
| `T?`          | `T \| null`          |
| `[T]`         | `T[]`                |

## Example IDL → generated code

```yaml
version: "0.3.0"
modules:
  - name: contacts
    enums:
      - name: Color
        variants:
          - { name: Red, value: 0 }
          - { name: Green, value: 1 }
          - { name: Blue, value: 2 }

    structs:
      - name: Contact
        fields:
          - { name: name, type: string }
          - { name: email, type: "string?" }
          - { name: tags, type: "[string]" }

    functions:
      - name: get_contact
        params:
          - { name: id, type: i32 }
        return: "Contact?"

      - name: list_contacts
        params: []
        return: "[Contact]"

      - name: set_favorite_color
        params:
          - { name: contact_id, type: i32 }
          - { name: color, type: "Color?" }

      - name: get_tags
        params:
          - { name: contact_id, type: i32 }
        return: "[string]"
```

Structs become TypeScript interfaces and enums become explicit numeric
TypeScript enums:

```typescript
export interface Contact {
  name: string;
  email: string | null;
  tags: string[];
}

export enum Color {
  Red = 0,
  Green = 1,
  Blue = 2,
}
```

Optional return and parameter types use `| null`; arrays use `T[]`:

```typescript
export function get_contact(id: number): Contact | null
export function list_contacts(): Contact[]
export function set_favorite_color(contact_id: number, color: Color | null): void
export function get_tags(contact_id: number): string[]
```

## Build instructions

The runnable example uses the `calculator` sample.

macOS:

```bash
cargo build -p calculator
cp target/debug/libindex.dylib generated/node/index.node

cd examples/node
DYLD_LIBRARY_PATH=../../target/debug npm start
```

Linux:

```bash
cargo build -p calculator
cp target/debug/libindex.so generated/node/index.node

cd examples/node
LD_LIBRARY_PATH=../../target/debug npm start
```

Windows: copy `target\debug\index.dll` to `generated\node\index.node`
and run `npm start` from `examples\node`.

For your own project, build an N-API addon (see `samples/node-addon`),
copy the resulting platform-specific binary in as `index.node`, and
publish the generated directory as a private npm package or ship it
inside your app.

## Memory and ownership

- The N-API addon is responsible for all conversions between JS values
  and C ABI types. Strings and byte buffers are copied into JS-managed
  storage, so consumers never need to think about freeing memory.
- Struct handles surface as opaque numeric IDs (`bigint`). The addon
  exposes `_destroy` helpers that tear down the underlying Rust state;
  use them in `try`/`finally` blocks for deterministic cleanup.
- Errors from the C ABI are converted into JavaScript `Error` instances
  by the addon before bubbling up to the caller.

## Async support

Async IDL functions are exposed as JS functions that return a Promise.
The N-API addon implements them with `napi_create_async_work` so that
the JS event loop stays responsive while the Rust function runs:

```typescript
export function fetch_contact(id: number): Promise<Contact>;
```

When the IDL marks the function `cancel: true`, the addon also accepts
an `AbortSignal` parameter and forwards aborts to the underlying
`weaveffi_cancel_token`.

## Troubleshooting

- **`Error: Cannot find module 'index.node'`** — the addon binary is
  missing. Build the N-API addon for your platform and copy it into
  `generated/node/` as `index.node`.
- **`dlopen: ... image not found`** — the addon links against the
  Rust cdylib at runtime; set `DYLD_LIBRARY_PATH` /
  `LD_LIBRARY_PATH` or copy the cdylib next to `index.node`.
- **`BigInt` errors with `handle`** — handles are 64-bit; pass them as
  `bigint`, not `number`.
- **TypeScript complains about missing types** — point `tsconfig`'s
  `paths` at `generated/node/types.d.ts` or include the generated
  package in `compilerOptions.types`.
