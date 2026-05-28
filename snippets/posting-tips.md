# Posting Tips

Mechanics behind the snippets. Read this once before your first post; you
won't need to come back to it.

---

## Screenshot tooling

| Tool | Use for | Notes |
|------|---------|-------|
| [carbon.now.sh](https://carbon.now.sh) | One-shot code panels | "VSCode Dark+" or "Night Owl". No window chrome. Padding 32 px. |
| [ray.so](https://ray.so) | Hero shots | Cleaner branding, rounded corners. Slight gradient background. |
| [Polacode](https://marketplace.visualstudio.com/items?itemName=pnp.polacode) | If you live in VSCode | Captures from your real editor with your real theme. |
| [Screen Studio](https://screen.studio) | Video/GIF | Cinematic zoom-and-pan; perfect for the watch-mode demo. |
| `asciinema` + `agg` | Terminal recordings | Tiny file size, sharp text. |

**Standard for this repo's snippets:**

- Font: JetBrains Mono or Berkeley Mono, ≥ 16 pt.
- Theme: one dark theme across the whole thread (consistency >
  aesthetics).
- Padding: at least 32 px on each side.
- Resolution: 2× export for retina.
- Aspect: 1:1 or 4:3 for feed; 16:9 only for video.

---

## Image counts per post

X allows up to 4 images per post. Use them.

- **Hero post:** 1 image. The eye needs one place to land.
- **Thread reply:** 1 image per post. Multi-image carousels read as
  cluttered on mobile.
- **Side-by-side comparison:** 1 *composed* image with both panels in
  it. Don't make the reader swipe to see the punchline.

---

## Alt text — write it every time

Every post here ships with alt text. Paste it into the "Description"
field when you attach the image. This is the #1 reach lever X applies
in 2026: posts with alt text get prioritised in the For You algorithm
and are accessible to screen readers.

---

## Hashtags

Use **at most one** hashtag. None is also fine.

Good candidates depending on the post:

- `#Rust` — when the post mentions `--scaffold` or `cargo install`.
- `#FFI` — niche but the right people are listening.
- `#WebDev` — for the TypeScript/Node-leaning posts.
- `#iOSDev` / `#AndroidDev` — for the Swift / Kotlin posts.

Never use `#Programming`, `#Coding`, or `#Tech` — they pull bots, not
humans.

---

## Timing

X engagement on dev content peaks around:

- **Tue–Thu, 09:00–11:00 PT** (afternoon for US East, evening for
  Europe). Best window for the flagship thread.
- **Tue–Thu, 15:00–17:00 PT** for shorter posts.

Avoid Fridays after 12:00 PT and the entire weekend unless you're
piggybacking on a specific event.

---

## Threading mechanics

- Queue every reply *before* hitting post on the first. X autopublishes
  the chain.
- First reply: the link out (`https://weaveffi.com`). The algorithm
  treats first-reply links more kindly than in-post ones.
- Last reply: a clear call to action. "Try it: `cargo install
  weaveffi-cli`" is enough.
- Keep each post < 250 chars where possible — leaves room for quote-tweets.

---

## Engagement playbook

- **Reply within the first 15 minutes** to comments. Algorithm
  amplification is steepest in this window.
- **Pin the post** that lands hardest until the next launch beat.
- When someone replies with a real war story (especially on the
  "Stop Writing JNI" post), reply with a single concrete answer +
  link to the relevant doc page. Don't pitch.

---

## Don't

- Don't post the same snippet twice in the same week.
- Don't drop a thread without a link out. People will quote-share it
  without context.
- Don't lead with crates.io / docs.rs links — they read as "API
  reference," which scrolls past. Always lead with a code panel or a
  diagram.
- Don't auto-cross-post to LinkedIn unedited. The tone here doesn't
  travel.

---

## Sequencing — a four-week sample plan

| Week | Post |
|------|------|
| 1 | `01` flagship thread (Tue AM) + `04` not-just-rust (Thu PM) |
| 2 | `02` async-one-line (Tue AM) + `09` typed-errors (Thu PM) |
| 3 | `08` idiomatic-side-by-side thread (Tue AM) + `05` 7ms (Thu PM) |
| 4 | `06` kitchen-sink thread (Tue AM) + `10` watch-mode video (Thu PM) |

After four weeks, recycle. The audience has rolled over and the posts
hit fresh eyes.
