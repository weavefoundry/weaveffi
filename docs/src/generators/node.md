# Node

The Node generator produces a CommonJS loader and `.d.ts` type definitions
for your functions. The generated addon uses the [N-API](https://nodejs.org/api/n-api.html)
(Node-API) interface to load the C ABI symbols and expose JS-friendly functions.

## Generated artifacts

- `generated/node/index.js` — CommonJS loader that requires `./index.node`
- `generated/node/types.d.ts` — function signatures inferred from your IR
- `generated/node/package.json`

## Generated code examples

Given this IDL definition:

```yaml
version: "0.1.0"
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

### TypeScript interfaces

Structs map to TypeScript interfaces with typed fields:

```typescript
export interface Contact {
  name: string;
  email: string | null;
  tags: string[];
}
```

### Enums

Enums map to TypeScript enums with explicit integer values:

```typescript
export enum Color {
  Red = 0,
  Green = 1,
  Blue = 2,
}
```

### Nullable types

Optional types are expressed as union types with `null`:

```typescript
// Optional parameter
color: Color | null

// Optional return
export function get_contact(id: number): Contact | null
```

### Array types

List types map to TypeScript arrays. Lists of optionals use parenthesized
union types:

```typescript
// Simple array
export function get_tags(contact_id: number): string[]

// Array return of structs
export function list_contacts(): Contact[]

// Array of optionals (if defined)
(number | null)[]
```

### Complete generated `types.d.ts`

For the IDL above, the full generated file looks like:

```typescript
// Generated types for WeaveFFI functions
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
// module contacts
export function get_contact(id: number): Contact | null
export function list_contacts(): Contact[]
export function set_favorite_color(contact_id: number, color: Color | null): void
export function get_tags(contact_id: number): string[]
```

### Type mapping reference

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

## Running the example

### macOS

```bash
cargo build -p calculator
cp target/debug/libindex.dylib generated/node/index.node

cd examples/node
DYLD_LIBRARY_PATH=../../target/debug npm start
```

### Linux

```bash
cargo build -p calculator
cp target/debug/libindex.so generated/node/index.node

cd examples/node
LD_LIBRARY_PATH=../../target/debug npm start
```

## Notes

- The loader expects the compiled N-API addon next to it as `index.node`.
- The N-API addon crate is in `samples/node-addon`.
