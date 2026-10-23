# Folio

A fast, good-looking Markdown viewer for Linux, in the spirit of Typora.
Native GTK 4 and Rust, no web engine. v1 opens and displays files; v2 will
add Typora-style inline editing on the same engine.

## Build & run

```sh
sudo apt install build-essential libgtk-4-dev   # once
cargo build --release
./target/release/folio README.md
./install.sh                                    # optional: ~/.local/bin + desktop entry
```

## Features (v1)

- CommonMark + GitHub Flavored Markdown: tables (with alignment), task lists,
  strikethrough, footnotes, autolinks, GitHub alerts (`> [!NOTE]` …),
  front matter, heading anchors.
- Syntax highlighting for ~30 languages (built-in, zero startup cost).
- Inline and display math rendered as Unicode (`$e^{i\pi}$` → e<sup>iπ</sup>);
  not a full TeX typesetter.
- Local images (PNG, JPEG, GIF, WebP, SVG via the system loaders), decoded off
  the main thread; their size is read from the file header so layout never jumps.
  Remote images are shown as link chips (Folio makes no network requests).
- Light and dark themes, following the GNOME colour-scheme setting live.
- Text selection (drag, double-click word, triple-click block), copy,
  primary selection, find with match highlighting.
- Live reload when the file changes on disk, keeping the scroll position.
- Relative links to other Markdown files open in place; `Alt+←` goes back.

## Keys

| Action | Keys |
|---|---|
| Open | `Ctrl+O` (or drop a file on the window) |
| Find / next / previous | `Ctrl+F` / `Enter`, `Ctrl+G` / `Shift+Enter`, `Ctrl+Shift+G` |
| Zoom in / out / reset | `Ctrl+=` / `Ctrl+-` / `Ctrl+0`, or `Ctrl`+scroll |
| Toggle dark mode | `Ctrl+D` |
| Reload | `Ctrl+R`, `F5` |
| Back (after following a link) | `Alt+←` |
| Scroll | arrows, `j`/`k`, `Space`/`Shift+Space`, `PgUp`/`PgDn`, `Home`/`End`, `g`/`G` |
| Select all / copy | `Ctrl+A` / `Ctrl+C` |
| Close window / quit | `Ctrl+W` / `Ctrl+Q` |

## How it stays fast

- **No web engine.** The document is drawn directly with Pango and GTK's
  renderer.
- **Lazy layout.** Only blocks on screen are shaped. The rest have estimated
  heights that an idle-time pass refines in 4 ms slices, with the view
  anchored so nothing jumps. Layouts far off screen are dropped, so open time
  and memory don't grow with file size.
- **Parallel startup.** The file is read and parsed on a worker thread while
  GTK initialises.
- **Single instance.** A second `folio file.md` hands the file to the running
  process over D-Bus and opens a new window there.

Measured in a headless GNOME Shell session (software GL, so GPU numbers are
pessimistic), 750 KB / 2,700-block file:

| Measure | Result |
|---|---|
| Parse | 5 ms |
| Lay out one screen (fonts warm) | ~3 ms |
| Scroll step (new blocks entering view) | 0.9 ms median, 2.5 ms max |
| Find across the whole document | 1.8 ms |
| Cold start → first frame, Cairo renderer | ~135–150 ms, 59 MB RSS |
| Cold start → first frame, GL renderer | ~300–400 ms (dominated by GL init) |

File size doesn't change startup time: the 750 KB file reaches the screen as
fast as a 3 KB one. Most of the cold start is GTK itself (library loading,
renderer and theme initialisation). Pick the renderer with `GSK_RENDERER=cairo`
or `GSK_RENDERER=ngl` / `vulkan`.

## Development

```sh
cargo test
folio --render out.png [--width 1000] [--height 1300] [--scroll Y] [--dark] [--select-all] FILE
folio --bench FILE                     # engine timings, no display needed
python3 samples/gen_big.py             # generate samples/big.md for benchmarking
FOLIO_STARTUP=1 folio FILE             # print startup phase timings, exit after first frame
FOLIO_SCREENSHOT=out.png folio FILE    # screenshot the live window and exit (FOLIO_SCROLL, FOLIO_FIND)
```

| File | Role |
|---|---|
| `src/doc.rs` | Markdown (pulldown-cmark) → flat list of blocks with container stacks and source ranges |
| `src/layout.rs` | Layout engine: lazy per-block Pango layout, virtual heights, painting, hit-testing, selection, find |
| `src/view.rs` | GTK widget (`GtkScrollable`): scrolling, input, async image loading |
| `src/app.rs` | Windows, actions, file loading, live reload, theme |
| `src/paint.rs` | Drawing backends: GTK snapshot (GPU) and Cairo (headless) |
| `src/highlight.rs`, `src/tex.rs` | Code highlighting, TeX → Unicode |

## Roadmap: v2 editing

Every block already carries its source byte range, and the engine re-lays out
individual blocks cheaply. Editing will go like this:

- The block under the cursor shows its Markdown source with syntax dimmed, as
  in Typora. All other blocks stay rendered.
- Each keystroke edits a rope, re-parses only the affected blocks and
  re-shapes one paragraph, so the cost per keystroke stays well under a frame.
- Input goes through `GtkIMContext`, so IME and compose keys work.
