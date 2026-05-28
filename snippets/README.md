# WeaveFFI Snippets for X

A grab-bag of high-virality posts, threads, and demo concepts that show off
WeaveFFI on X. Each file is a self-contained "post kit": hook text,
copy-pasteable code, a screenshotting plan, and (where it applies) a thread
continuation.

## Index

| File | Format | Hook |
|------|--------|------|
| [01-one-idl-eleven-languages.md](01-one-idl-eleven-languages.md) | Thread (7 posts) | "I wrote 30 lines of YAML and got 11 SDKs." |
| [02-async-one-line.md](02-async-one-line.md) | Single post | One IDL flag → `async/await` in 8 languages. |
| [03-stop-writing-jni.md](03-stop-writing-jni.md) | Single post | 230 lines of JNI vs. 10 lines of YAML. |
| [04-not-just-rust.md](04-not-just-rust.md) | Single post | Rust, C, C++, Zig — anything with a C ABI. |
| [05-seven-milliseconds.md](05-seven-milliseconds.md) | Single post | 11 SDKs, kitchen-sink IDL, 7 ms. |
| [06-kitchen-sink-thread.md](06-kitchen-sink-thread.md) | Thread (8 posts) | One file. Every FFI feature you've ever wanted. |
| [07-eleven-registries.md](07-eleven-registries.md) | Single post | npm + PyPI + SwiftPM + Maven + NuGet + pub.dev + RubyGems + Go modules. |
| [08-idiomatic-side-by-side.md](08-idiomatic-side-by-side.md) | Thread (7 posts) | Same struct, five language flavours, idiomatic types in each. |
| [09-typed-errors-everywhere.md](09-typed-errors-everywhere.md) | Single post | One error domain → typed exceptions in every language. |
| [10-watch-mode-demo.md](10-watch-mode-demo.md) | Video / GIF | Edit YAML, watch 11 SDKs regenerate live. |
| [posting-tips.md](posting-tips.md) | Reference | Screenshot tools, timing, alt text, hashtags. |

## How to use

1. Pick a snippet that matches the energy you want to lead with.
2. Copy the hook text exactly — they're sized for X.
3. Render the code with [carbon.now.sh](https://carbon.now.sh) or
   [ray.so](https://ray.so) using the suggested theme. See
   [posting-tips.md](posting-tips.md).
4. Post the hook + image. For threads, queue every reply before publishing.
5. The first reply should always include the URL to <https://weaveffi.com>
   (or the repo) so the algorithm has a clear destination.

## Posting cadence

Spread them out. Three good posts a week beats ten posts in a day. Lead
with the flagship thread (`01`) — it's the moment people screenshot and
share. Use the single posts as midweek follow-ups. Save the video
(`10`) for a launch week or a 1.0 announcement.

## Customization

Every snippet uses a `contacts` or `kvstore` example pulled from this
repo's `samples/`, so you can run the exact code in the screenshots to
prove it works. Swap in your own real-world example whenever it lands
harder for your audience.

## Veracity check before posting

Each snippet was cross-referenced against the WeaveFFI snapshot tests
in `crates/weaveffi-cli/tests/snapshots/` and the actual `generated/`
output in this repo. The function names, error class names, and async
shapes are what the current generators emit.

Two artistic liberties to be aware of:

- The flagship thread (`01`) shows a *partial* terminal listing under
  `weaveffi generate --dry-run`. The actual command also emits paths
  for `weaveffi-config.cmake.in`, `binding.gyp`, `conanfile.py`, and
  a few other build files. Trim or extend the screenshot to taste.
- The watch-mode video (`10`) suggests a generated `.pyi` containing
  `phone: Optional[str]` after a YAML edit. To reproduce verbatim,
  run the demo against `samples/contacts/contacts.yml` (which has a
  `Contact` struct) — not against `samples/calculator/`.

If you change a snippet's IDL or any API call, regenerate that
sample's snapshot (`cargo insta test --accept`) and update the
snippet to match.
