# Doc comment style

This page describes how WeaveFFI's Rust doc comments are written. Follow it
when you add or revise public API so the generated [Rust API docs](rust.md)
read consistently and the doc lints stay green in CI.

## TL;DR

- Every public item carries a doc comment. This is enforced by
  `#![deny(missing_docs)]` on each library crate.
- Use `///` for items, `//!` for modules and crates.
- The first line is a short imperative summary ending in a period.
- Document fallible and panicking behavior with `# Errors`, `# Panics`, and
  `# Safety` sections. These are the Rust analog of "what can go wrong," and
  the matching Clippy lints require them.
- Link other items with intra-doc links: `` [`BindingModel`] `` or
  `` [`Api`](weaveffi_ir::ir::Api) ``.
- Wrap code-like identifiers in backticks. Product and tool names
  (WeaveFFI, SwiftPM, CMake) are allow-listed in `clippy.toml` instead.
- Comments explain why, not what.

## Grammar and punctuation

Prose in doc comments and Markdown follows the *Chicago Manual of Style*
(17th edition), matching the repository's `AGENTS.md`. Highlights:

- **No em dashes (`U+2014`).** Use commas, parentheses, semicolons, colons, or
  separate sentences instead.
- **Use straight ASCII quotes and apostrophes** (`"` and `'`), not curly
  ones, so prose stays copy-pasteable into source and terminals.
- Use the **serial (Oxford) comma** in lists of three or more.
- Use **contractions** where they read naturally ("doesn't," "isn't").
- Use **sentence case** for headings: capitalize only the first word and
  proper nouns.

## Doc comments

WeaveFFI follows the conventions in
[RFC 1574](https://github.com/rust-lang/rfcs/blob/master/text/1574-more-api-documentation-conventions.md)
and the [rustdoc book](https://doc.rust-lang.org/rustdoc/how-to-write-documentation.html).
The standard section headings (`# Examples`, `# Errors`, `# Panics`,
`# Safety`) play the role that `Args`, `Returns`, and `Raises` play in a
Google-style docstring.

### Functions and methods

```rust
/// Generate bindings for every requested target and write them to `out_dir`.
///
/// Targets are rendered from a shared [`BindingModel`] so symbol names and
/// parameter lowering are computed once and reused across languages.
///
/// # Errors
///
/// Returns an error if the IDL fails to validate, a requested target is
/// unknown, or any output file cannot be written.
///
/// # Examples
///
/// ```no_run
/// use weaveffi_core::codegen::generate;
/// # use weaveffi_ir::ir::Api;
/// # fn demo(api: Api) -> anyhow::Result<()> {
/// generate(&api, "./generated", &["c", "swift"])?;
/// # Ok(())
/// # }
/// ```
pub fn generate(api: &Api, out_dir: &str, targets: &[&str]) -> anyhow::Result<()> {
    // ...
}
```

Notes:

- Lead with a one-line imperative summary, then a blank line, then any
  extended description.
- Refer to parameters by name in backticks (`` `out_dir` ``). Don't restate
  their types; the rendered signature already shows them.
- Add a **`# Errors`** section to every public function that returns
  `Result`, describing the conditions that produce an `Err`. Clippy's
  `missing_errors_doc` enforces this.
- Add a **`# Panics`** section to any public function that can panic,
  describing when. Clippy's `missing_panics_doc` enforces this. If a panic
  path is provably unreachable (for example an `expect` on sanitized
  input), suppress it locally with a reason instead of documenting a panic
  that cannot happen:

  ```rust
  // `CString::new` is infallible here: interior NUL bytes are stripped above.
  #[allow(clippy::missing_panics_doc)]
  pub fn string_to_c_ptr(s: impl AsRef<str>) -> *const c_char {
      // ...
  }
  ```

- Prefer ` ```no_run ` or ` ```ignore ` for examples that need a built
  `cdylib`, a file path, or other state the doctest can't set up. Use a
  plain ` ```rust ` block (which `cargo test` compiles and runs) when the
  snippet is self-contained.

### `unsafe` functions

Every public `unsafe fn`, and any function that dereferences raw pointers
across the C ABI, needs a `# Safety` section spelling out the caller's
obligations. Clippy's `missing_safety_doc` enforces this.

```rust
/// Register a handle and its destructor with the given arena.
///
/// # Safety
///
/// `arena` must be a valid pointer returned by `arena_create`. `ptr` and
/// `dtor` must stay valid until `arena_destroy` is called.
pub fn arena_register(arena: *mut HandleArena, ptr: *mut c_void, dtor: Dtor) {
    // ...
}
```

### Structs, enums, and their members

Document the type, then every public field or variant. `missing_docs`
flags undocumented `pub` fields and variants, not just the type itself.

```rust
/// Error struct passed across the C ABI boundary.
#[repr(C)]
pub struct weaveffi_error {
    /// Status code. `0` means success; any non-zero value indicates failure.
    pub code: i32,
    /// Owned, NUL-terminated UTF-8 message, or null when `code` is `0`.
    pub message: *const c_char,
}

/// How a value crosses the ABI boundary.
pub enum Ownership {
    /// The callee owns the value; the caller must not free it.
    Borrowed,
    /// Ownership transfers to the caller, who must free it.
    Owned,
}
```

Field and variant docs can be terse. One clause that says what the field
*means* (not what its type is) is usually enough.

### Modules and crates

Open every crate's `lib.rs` with a `//!` summary, and every module with a
`//!` header describing its role:

```rust
//! C ABI runtime: error struct, memory helpers, and utility functions.
```

Crate-level docs are enforced separately by
`RUSTDOCFLAGS="-D rustdoc::missing_crate_level_docs"` in CI, so a crate
without a `//!` header fails the `rustdoc` job.

### Private items

`missing_docs` only requires docs on the public API, so private helpers
aren't strictly required to have them. Still, write a short `///` line for
non-obvious private items: contributors read them in editors and reviews.

## Comments: explain why

Comments are most useful when they explain things the reader can't learn
from the code itself:

- a non-obvious invariant or ABI constraint,
- a trade-off between two reasonable approaches,
- a reference to an external spec, RFC, or upstream bug.

Don't narrate what the next line does (`// increment the counter`) or
restate a name (`// the generator`). Delete redundant comments when you
find them.

## Intra-doc links

Link to other items so rustdoc can resolve and cross-reference them. This
is the Rust analog of the docs site's autorefs:

```rust
/// Renders from the shared [`BindingModel`], never re-deriving lowering.
///
/// See [`Api`](weaveffi_ir::ir::Api) for the input model and
/// [`LanguageBackend`](crate::backend::LanguageBackend) for the trait every
/// generator implements.
```

Use the short `` [`Type`] `` form when the item is in scope, and the
`` [`Type`](path::to::Type) `` form to link across modules or crates.

## `doc_markdown` and backticks

Clippy's `doc_markdown` lint flags identifiers that look like code but
aren't wrapped in backticks. Wrap real identifiers, types, paths, and
file names in backticks (`` `BindingModel` ``, `` `weaveffi.yml` ``).

Product names, tool names, and naming-convention terms (WeaveFFI, SwiftPM,
CMake, NuGet, `snake_case`, `PascalCase`) read as prose, not code. Rather
than backticking them, they're allow-listed in `clippy.toml` under
`doc-valid-idents`. Add a new entry there when you introduce another such
name.

## Enforcement

The doc lints are configured per library crate (in each crate's `lib.rs`)
and centrally in `clippy.toml`:

| Lint | What it requires |
| --- | --- |
| `missing_docs` (deny) | A doc comment on every public item, field, and variant |
| `clippy::missing_errors_doc` | A `# Errors` section on public `fn`s returning `Result` |
| `clippy::missing_panics_doc` | A `# Panics` section on public `fn`s that can panic |
| `clippy::missing_safety_doc` | A `# Safety` section on public `unsafe fn`s (on by default) |
| `clippy::doc_markdown` | Backticks around code-like identifiers |

CI runs these through the existing gates, so missing or malformed docs
fail the build. Check your changes locally before pushing:

```bash
# Lint everything, including the doc lints (warnings are denied).
cargo clippy --workspace --all-targets -- -D warnings

# Build the API docs the way the rustdoc job does.
RUSTDOCFLAGS="-D rustdoc::all -D rustdoc::missing_crate_level_docs" \
    cargo doc --workspace --no-deps

# Or run both through the shared recipe.
just doc
```

The generated API reference is published under
[`/api/rust/`](rust.md) when the docs site deploys.
