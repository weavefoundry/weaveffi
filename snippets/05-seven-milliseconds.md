# 05 — 7 ms for 11 SDKs (Single Post)

The speed flex. Devs love a benchmark, especially when the number is
absurd in a good way.

---

## Hook

> Full WeaveFFI code-gen pass on the kitchen-sink IDL:
>
>   • 11 SDKs (Swift, Kotlin, TS, Python, C#, Dart, Go, Ruby, C, C++, WASM)
>   • Includes Gradle, SwiftPM, `pyproject.toml`, `.csproj`, `pubspec.yaml`,
>     `package.json`, `go.mod`, `.gemspec`, `CMakeLists.txt`.
>   • Median: **7.27 ms** on an M-series laptop.
>
> Code-gen should disappear in the build. It does.

---

## Image — the table

Use a simple monospace table screenshot from the docs' performance page,
or render this minimal version:

```
Benchmark                       Median     Target
─────────────────────────────────────────────────
validate (kitchen-sink IDL)     7.45 µs    < 5 ms      (670× under)
hash     (kitchen-sink IR)     37.50 µs    < 1 ms      (27×  under)
codegen, all 11 (calculator)    6.92 ms    < 500 ms    (72×  under)
codegen, all 11 (kitchen-sink)  7.27 ms    < 2000 ms   (275× under)
```

---

## Body (in reply)

> Methodology: criterion.rs, 100 samples, fresh temp directory per
> iteration so disk I/O is included. Reference hardware: M-series
> laptop, `--release`, no LTO. Source:
> `crates/weaveffi-core/benches/codegen_bench.rs`.

---

## Why this works

- **Single number.** "7 ms" sticks in your head. "Full SDK matrix" makes
  it absurd.
- **Quantified margin.** "275× under target" reads as "we tried to make
  it slow and failed."
- **Honest methodology.** The reply preempts the inevitable "but did
  you include I/O?" comment.

---

## Alt text

"A four-row benchmark table. Columns: benchmark name, median time,
target time, headroom multiplier. Rows show validation at 7.45
microseconds, hashing at 37.5 microseconds, full code generation for
the calculator IDL at 6.92 milliseconds, and full code generation for
the kitchen-sink IDL at 7.27 milliseconds — each well under its
target."
