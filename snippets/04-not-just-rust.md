# 04 — Not Just Rust (Single Post)

The reframe post. Most FFI tools assume Rust — this one assumes a C ABI.
That distinction matters to anyone wrapping a C++ engine, a Zig
prototype, or a 20-year-old C library.

---

## Hook

> WeaveFFI doesn't care what your library is written in.
>
> Rust? ✓ (`--scaffold` writes the FFI stubs)
> C? ✓
> C++? ✓
> Zig? ✓
>
> If you can expose a C ABI, you get 11 idiomatic SDKs.

---

## Image

Four logos arranged in a 2×2 grid, each pointing to a central
`extern "C"` block, which then fans out to a row of 11 small language
icons. Or, simpler:

```
   Rust ──┐
   C   ──┤
   C++ ──┼──▶  extern "C"  ──▶  Swift · Kotlin · TS · Python · C# ·
   Zig ──┘                       Dart · Go · Ruby · C · C++ · WASM
```

Render the diagram in [excalidraw.com](https://excalidraw.com) on a dark
background.

---

## Body (in-post or as a reply)

> Most generators (UniFFI, diplomat) start from annotated Rust.
> WeaveFFI starts from an IDL.
>
> Your backend just has to fulfil the C ABI WeaveFFI emits. If you're on
> Rust, `weaveffi generate --scaffold` writes the stubs for you. If
> you're on anything else, implement the symbols in the header — same
> contract.

---

## Why this works

- **Differentiation in a crowded space.** The first reply on any FFI
  post is always "isn't this just UniFFI?" — this post pre-empts it.
- **Audience expansion.** Reaches the C++/Zig crowd that filters out
  Rust-only tools.
- **Honest.** No fake claims; the comparison table in the docs backs it
  up.

---

## Alt text

"A diagram showing Rust, C, C++, and Zig logos each connected by an
arrow to a single block labelled `extern \"C\"`. From that block, eleven
arrows fan out to language logos for Swift, Kotlin, TypeScript, Python,
C#, Dart, Go, Ruby, C, C++, and WASM."
