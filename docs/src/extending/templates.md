# Custom Templates

Every built-in generator emits the bulk of its output from hand-written Rust
formatters. For languages where the default shape is not quite right, WeaveFFI
also exposes **Tera templates** for a handful of well-defined files. You can
override those templates with your own `.tera` files to tweak comments,
headers, or the whole file layout without forking the generator.

This page documents:

- which templates each generator ships
- the context schema passed to every template
- the filters WeaveFFI registers on top of Tera's standard set
- a worked example that customises the C header to use Doxygen comments

## Enabling overrides

Point the `generate` command at a directory of templates with `--templates`:

```bash
weaveffi generate api.yml -o generated --templates ./my-templates
```

WeaveFFI walks the directory recursively and loads every file whose extension
is `.tera`, keyed by its path relative to `--templates`. When a generator
renders a template, it looks up the entry by that path (for example
`c/header.tera`). If your directory contains a file at the matching path, it
wins over the built-in; otherwise the built-in Rust formatter runs unchanged.

Your override only has to cover the templates you want to customise. Any
generator without an override falls back to the default output, and any
template your directory does not define is emitted by the built-in formatter.

## Built-in templates per generator

Today the following generators expose Tera templates. All other generators
(Android, Node, WASM, .NET, C++, Dart, Go, Ruby) still produce their output
exclusively through Rust code.

| Generator | Template path        | File produced                           |
| --------- | -------------------- | --------------------------------------- |
| `c`       | `c/header.tera`      | `generated/c/{c_prefix}.h`              |
| `swift`   | `swift/wrapper.tera` | `generated/swift/Sources/<mod>/<mod>.swift` |
| `python`  | `python/module.tera` | `generated/python/<pkg>/weaveffi.py`    |
| `python`  | `python/stubs.tera`  | `generated/python/<pkg>/weaveffi.pyi`   |

Other files around these artefacts — `weaveffi.c`, `Package.swift`,
`module.modulemap`, `pyproject.toml`, `setup.py`, `__init__.py`, and the
per-target `README.md` — are still emitted by the built-in formatter and
cannot be retemplated. If you need deeper customisation for a generator that
is not listed above, run the existing generator and post-process its output,
or fall back to an [external generator](./external-generators.md).

The template path is also the override path: put your file at
`<--templates>/c/header.tera` (or `swift/wrapper.tera`, etc.) and it will be
picked up automatically.

## Tera context schema

Every template is rendered against the context produced by
`weaveffi_core::templates::api_to_context`. The context mirrors the validated
[`Api`](../reference/idl.md) but flattens each type reference into a small
object so templates do not need to parse strings.

### Top-level variables

| Variable  | Type   | Description                               |
| --------- | ------ | ----------------------------------------- |
| `version` | string | IR schema version declared in the IDL     |
| `modules` | array  | One entry per top-level module (see below)|

### Module

Each element of `modules` is an object with these keys:

| Key         | Type   | Description                                         |
| ----------- | ------ | --------------------------------------------------- |
| `name`      | string | Module name as declared in the IDL                  |
| `functions` | array  | Exported functions (see [Function](#function))      |
| `structs`   | array  | Value-type struct definitions                       |
| `enums`     | array  | Integer-valued enum definitions                     |
| `callbacks` | array  | Foreign callback signatures                         |
| `listeners` | array  | Listener-style subscription APIs                    |

### Function

| Key       | Type        | Description                                        |
| --------- | ----------- | -------------------------------------------------- |
| `name`    | string      | Function name                                      |
| `params`  | array       | Ordered parameter list (see [Param](#param))       |
| `returns` | type or nil | `null` when the function has no return value       |
| `doc`     | string/nil  | Doc comment if provided, otherwise `null`          |

### Param

| Key    | Type   | Description                                     |
| ------ | ------ | ----------------------------------------------- |
| `name` | string | Parameter name                                  |
| `type` | object | [Type object](#type-object) for the parameter   |

### Struct

| Key      | Type   | Description                                 |
| -------- | ------ | ------------------------------------------- |
| `name`   | string | Struct name                                 |
| `fields` | array  | Ordered field list: `{name, type}` objects  |

### Enum

| Key        | Type   | Description                                 |
| ---------- | ------ | ------------------------------------------- |
| `name`     | string | Enum name                                   |
| `variants` | array  | Ordered variants: `{name, value}` objects   |

### Callback

| Key       | Type        | Description                                  |
| --------- | ----------- | -------------------------------------------- |
| `name`    | string      | Callback type name                           |
| `params`  | array       | Parameters passed to the callback            |
| `returns` | type or nil | Callback return type, or `null` for `void`   |
| `doc`     | string/nil  | Doc comment if provided                      |

### Listener

| Key              | Type       | Description                                  |
| ---------------- | ---------- | -------------------------------------------- |
| `name`           | string     | Listener type name                           |
| `event_callback` | string     | Name of the callback the listener dispatches |
| `doc`            | string/nil | Doc comment if provided                      |

### Type object

Every `type` and `returns` entry is an object rather than a string so templates
can branch without substring matching. The `kind` discriminates the shape:

| `kind`          | Extra keys                               | IDL source              |
| --------------- | ---------------------------------------- | ----------------------- |
| `i32`           | —                                        | `i32`                   |
| `u32`           | —                                        | `u32`                   |
| `i64`           | —                                        | `i64`                   |
| `f64`           | —                                        | `f64`                   |
| `bool`          | —                                        | `bool`                  |
| `string`        | —                                        | `string`                |
| `bytes`         | —                                        | `bytes`                 |
| `borrowed_str`  | —                                        | `&str`                  |
| `borrowed_bytes`| —                                        | `&[u8]`                 |
| `handle`        | `name` (when typed)                      | `handle` / `handle<T>`  |
| `struct`        | `name`                                   | `MyStruct`              |
| `enum`          | `name`                                   | `MyEnum`                |
| `optional`      | `inner` (nested type object)             | `T?`                    |
| `list`          | `inner` (nested type object)             | `[T]`                   |
| `map`           | `key`, `value` (nested type objects)     | `{K:V}`                 |
| `iterator`      | `inner` (nested type object)             | `iter<T>`               |
| `callback`      | `name`                                   | `callback<Name>`        |

## Available filters

WeaveFFI templates have access to every filter [built into Tera](https://keats.github.io/tera/docs/#built-in-filters)
plus a small set of case-conversion helpers registered on top. Use whichever
reads most naturally for the target language's conventions.

### Case conversion (WeaveFFI-registered)

All four filters accept a string and return a string; non-string inputs raise
a template error so typos surface immediately.

| Filter                 | Output style        | Example (`"my_function_name"`)     |
| ---------------------- | ------------------- | ---------------------------------- |
| `to_snake_case`        | `snake_case`        | `my_function_name`                 |
| `to_camel_case`        | `lowerCamelCase`    | `myFunctionName`                   |
| `to_pascal_case`       | `UpperCamelCase`    | `MyFunctionName`                   |
| `to_shouty_snake_case` | `SCREAMING_SNAKE`   | `MY_FUNCTION_NAME`                 |

They round-trip across identifier styles, so `"HelloWorld" | to_snake_case`
yields `hello_world` and `"hello_world" | to_pascal_case` yields `HelloWorld`.

```tera
{% for func in module.functions %}
void {{ func.name | to_snake_case }}(void);      {# snake_case for C     #}
// {{ func.name | to_pascal_case }}               {# PascalCase for docs  #}
{% endfor %}
```

### Commonly useful Tera built-ins

These are not WeaveFFI-specific but turn up in most generators:

| Filter            | Purpose                                         |
| ----------------- | ----------------------------------------------- |
| `upper` / `lower` | ASCII case                                       |
| `trim`            | Strip whitespace                                 |
| `replace(...)`    | Substring replacement                            |
| `default(value=…)`| Fallback for `null`/missing values               |
| `length`          | Array or string length                           |
| `join(sep=…)`     | Collapse arrays into strings                     |
| `first` / `last`  | Array endpoints                                  |

Refer to the Tera docs for the full list. Filters chain left-to-right, so
`{{ func.name | to_snake_case | upper }}` produces `MY_FUNCTION_NAME`.

## Worked example: Doxygen comments in the C header

Suppose you want the generated C header to carry
[Doxygen](https://www.doxygen.nl/) comments (`/** … */`) instead of WeaveFFI's
default plain-text layout, so you can point `doxygen` at `generated/c/` and
produce API reference documentation for free.

The C generator ships a template at `c/header.tera`; overriding it replaces
the **entire header body**. WeaveFFI still prepends its trace-stamp comment on
the first line, but everything from the include guards down is yours. Keep
the include guards, standard headers, and `extern "C"` block so downstream
compilers and the built-in `{prefix}.c` stub keep working.

### Step 1 — lay out the override directory

```text
my-templates/
└── c/
    └── header.tera
```

### Step 2 — write the template

This override emits a Doxygen file header, a `@defgroup` per module, and a
`@brief` / `@param` / `@return` block per function. It uses
`to_pascal_case` to render module names in TitleCase, and the type object's
`kind` field to print human-readable parameter and return types:

```tera
{#-
  Custom C header template — Doxygen style.
  Lives at <--templates>/c/header.tera.
-#}
/**
 * @file weaveffi.h
 * @brief Generated C ABI for WeaveFFI IR {{ version }}.
 *
 * DO NOT EDIT. Regenerate with `weaveffi generate`.
 */
#ifndef WEAVEFFI_H
#define WEAVEFFI_H

#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

{% for module in modules -%}
/**
 * @defgroup {{ module.name | to_snake_case }} {{ module.name | to_pascal_case }}
 * @{
 */
{% for func in module.functions -%}
/**
 * @brief {{ func.doc | default(value=func.name) }}
{%- for p in func.params %}
 * @param {{ p.name }} {{ p.type.kind }}
{%- endfor %}
{%- if func.returns %}
 * @return {{ func.returns.kind }}
{%- endif %}
 */
{% endfor -%}
/** @} */

{% endfor -%}
#ifdef __cplusplus
}
#endif

#endif // WEAVEFFI_H
```

> The template is intentionally focused on the *comment* layer. Function
> prototypes, struct typedefs, and enum declarations are produced by the
> built-in formatter when no override is supplied; if you need them inside the
> override you must emit them yourself from the context. Most projects want
> the comments to be customisable but keep the signatures default — in that
> case, render prototypes from within the loop and mirror the
> `weaveffi_{module}_{function}` convention documented in
> [Naming and Package Conventions](../reference/naming.md).

### Step 3 — regenerate

Pass `--templates ./my-templates` alongside your usual `generate` invocation:

```bash
weaveffi generate api.yml -o generated --templates ./my-templates
```

For an IDL with a single `math` module that exposes
`add(a: i32, b: i32) -> i32`, the header now opens with:

```c
// WeaveFFI 0.2.0 c 0.2.0 - DO NOT EDIT - regenerate with 'weaveffi generate'
/**
 * @file weaveffi.h
 * @brief Generated C ABI for WeaveFFI IR 0.2.0.
 *
 * DO NOT EDIT. Regenerate with `weaveffi generate`.
 */
#ifndef WEAVEFFI_H
#define WEAVEFFI_H
/* ...standard includes... */

/**
 * @defgroup math Math
 * @{
 */
/**
 * @brief Add two numbers
 * @param a i32
 * @param b i32
 * @return i32
 */
/** @} */
```

Point Doxygen at `generated/c/` and you get API reference documentation that
tracks your IDL without any further hand-editing.
