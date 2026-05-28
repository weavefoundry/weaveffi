# 10 — Watch-Mode Demo (Video / GIF)

The kinetic post. Static screenshots show the *what*. A short loop shows
the *feel*. Save this for a launch tweet or pinned-post slot.

---

## Hook

> Live demo: edit one YAML file, watch eleven SDKs regenerate.
>
> 9-second loop.

---

## Video / GIF storyboard

**Setup:** A wide terminal tiled into three vertical panes.

| Pane | What's running |
|------|----------------|
| Left | `nvim contacts.yml` (you'll type into this) |
| Middle | `weaveffi watch contacts.yml -o sdk` (will fire on save) |
| Right | `watch -n 0.5 ls sdk/swift sdk/python sdk/node sdk/dart` (just to make the listing flicker visibly) |

**Sequence (≈9 s):**

1. **0–1 s:** Cursor sits on the IDL. Title overlay: *"Add a field."*
2. **1–3 s:** You type `- { name: phone, type: "string?" }` into the
   `Contact` struct.
3. **3–4 s:** Save. The middle pane prints
   `Generated artifacts in sdk` and `Regenerated at 14:02:33`.
4. **4–5 s:** The right pane's listing updates: file mtimes change
   across `swift/`, `python/`, `node/`, `dart/`.
5. **5–7 s:** Cut to a single panel: `grep phone
   sdk/python/weaveffi/weaveffi.pyi` → `phone: Optional[str]`.
6. **7–9 s:** End card: *"weaveffi watch contacts.yml -o sdk · cargo
   install weaveffi-cli · weaveffi.com"*

Export as a looping MP4 or GIF, ≤ 4 MB. X autoplays MP4s, which read as
"premium." GIFs are a fallback.

---

## Recording stack

- [`asciinema`](https://asciinema.org/) for the terminal capture, plus
  [`agg`](https://github.com/asciinema/agg) to convert to GIF/MP4.
- Or [Screen Studio](https://screen.studio) for the cinematic look.
- Font: JetBrains Mono or Berkeley Mono, ≥ 16 pt so it reads on mobile.
- Background: any dark theme. Add a faint vignette so the eye lands on
  the typing pane.

---

## Pinned reply (link out)

> Source code in the video — `weaveffi watch` ships with the CLI.
>
> `cargo install weaveffi-cli`
>
> Docs: https://weaveffi.com

---

## Why this works

- **Motion beats stills.** A 9-second loop pulls the eye on a feed that
  scrolls at 800 px/sec.
- **Concrete delta.** You can *see* the SDKs change. Nothing to imagine.
- **Mobile-friendly.** ≤10s, ≤4 MB, native autoplay.

---

## Alt text

"A 9-second screen recording of a terminal split into three panes. On
the left, a YAML file is edited to add a new field `phone: string?`. In
the middle, a `weaveffi watch` process detects the save and reports
regenerating bindings for 11 targets. On the right, a directory listing
of the generated SDK folders flickers as new files are written. The
recording ends with the Python `.pyi` showing `phone: Optional[str]`."
