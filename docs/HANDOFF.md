# Hand-off: Folio v2 (editing)

Folio is a fast, Typora-style Markdown app for Linux. v1, the viewer, shipped
as `v0.1.0`. v2 adds Typora-style inline editing; its first working version
is on the `v2-editing` branch (see "v2 status" below). Read `README.md` first
for features, keys and the file map; this document covers what the code
doesn't say.

## Goals, in priority order

1. **Speed.** Startup should be near-instant, and every operation (typing
   included) should have imperceptible lag: well under one frame.
2. **Aesthetics.** It should look as good as Typora.
3. Full Markdown support (CommonMark, GFM, math, footnotes, …).

## Decisions made, and why

| Decision | Why |
|---|---|
| Rust + GTK 4, no web view | A web view costs 300–500 ms of startup (WebKitGTK spawns a process) and contenteditable is where Typora's editing lag comes from. GTK gives us IME, clipboard, HiDPI, accessibility and Pango text shaping for free, and it's already installed on Linux, so the binary stays at about 1 MB. |
| Linux only | Chosen over cross-platform options such as winit + Parley. |
| Our own renderer on Pango, not GtkTextView | Full control over layout and looks. Each block maps to a source byte range, which is what inline editing needs. |
| pulldown-cmark | Correct and fast (5 ms for 750 KB), and it gives source offsets for every element. |
| **GPU renderer (GTK default)** | Decided after measuring. In headless tests the Cairo renderer reached first frame in ~140 ms vs ~300–400 ms on GL, but it rasterises every frame on the CPU (8–13 ms per frame at 1000–2000 px), which risks stutter on large or HiDPI screens. Don't force `GSK_RENDERER`. |
| Single-instance GApplication | A second launch hands the file to the running process over D-Bus. |
| Remote images not fetched | No network code. Remote images render as link chips. |
| `folio --update` is a shell script | Keeps network code out of the binary: `src/update.sh` (embedded with `include_str!`) uses curl or wget, checks the release's `.sha256` and runs the release's `install.sh`, which installs to `~/.local/bin`. It depends on the release asset names in `release.yml`. No update check at startup (startup speed, no network). |

## v2 status

Done, tested, measured: editing with a source view of the block under the
caret, incremental re-parse, caret/selection/keyboard navigation, IME via
`GtkIMContext`, undo/redo by word, cut/copy/paste, save/save as/new, dirty
title marker, close prompt, own saves not reloaded, `--bench` typing timings
(0.42 ms median per keystroke on the 750 KB file, 1.7 ms max).

How it works (`editor.rs`, with `Engine::splice*` in `layout.rs`):

- **The buffer is a `String`**, not a rope (deviation from the plan below):
  pulldown-cmark needs a contiguous `&str` to re-parse, and inserting into
  750 KB is a ~20 µs memmove. Measured, not guessed; revisit only if `--bench`
  says so.
- **Caret and selection are byte offsets** into the source (`EditState`);
  `Engine::caret`/`sel` (`Pos`) are derived for painting.
- **Source map:** every block has `map: [part, text offset, source offset]`
  runs (source relative to `src.start`), so `Block::text_to_src` /
  `src_to_text` convert rendered positions both ways (clicks on rendered
  text land on the right source byte).
- **The active block is a real block**, `Kind::Source { rich, style }`,
  spliced into `doc.blocks` in place of the rendered block(s); the rendered
  ones are stashed and put back when the caret leaves. So layout, hit-testing
  and selection need no special cases. Its text is the raw source, styled by
  `doc::source_kind` (syntax gets `st::SYNTAX`, drawn in `theme.syntax_mark`).
  A caret between blocks (after Enter) gets an empty synthetic source block.
- **Every edit:** un-stash → change text → re-parse region → re-activate.
  The region is the touched blocks plus one top-level container run on each
  side, widened until bounded by blank lines (lazy continuation and setext
  underlines cross single newlines). Unchanged blocks keep their layout
  (`splice_reuse`). Full re-parse fallback when the doc has footnotes or link
  reference definitions (`Doc::global`), when the edit text is "risky" (HTML
  comment/pre/script, `]:`, `[^`), when front matter may change, or when the
  last untouched block of the region parses differently (e.g. an unclosed
  fence). The randomized test `editor::fuzz` checks the result against a
  fresh parse after every edit; run it with more seeds after parser changes.
- **Front matter** is detected by Folio itself, only at offset 0: pulldown-
  cmark's metadata option also matched `---` + text … `---` mid-document,
  swallowing everything in between (a v1 bug, now fixed).
- **Reading mode:** with no caret (on open, or after `Esc`) the v1 scroll
  keys work; a click starts editing. While editing, links need Ctrl+click.
- **Tables** stay `Kind::Table` while edited: `activate_table` swaps only the
  caret's cell for its source (`Active::cell`), and the caret is kept inside
  that cell's text (`table_cells_limit`; a cell may extend over spaces typed
  at its end). Cell ranges come from `table.rs`, which also rewrites a whole
  table for row/column operations (`Grid::render`).
- **Smart input** is `Engine::type_text` (typed keys only; paste goes through
  `paste_text`). `EditState::pending_close` remembers an auto-inserted closer
  so typing it steps over and Backspace removes the pair.
- **Lists:** `list_item` finds the item (with its nested blocks) under the
  caret, `list_indent` rewrites its lines with `multi_edit` (several edits,
  one undo step, caret mapped through them).
- **Outline:** `Engine::outline` / `heading_at`; the window keeps a `ListBox`
  in a `Revealer` and rebuilds rows only when heading text changes.

Next steps, roughly in priority order:

1. Show the image under its source while an image block is active.
2. Renumber ordered lists after edits; re-pad a table's columns when the
   caret leaves it (as one undo step); make a selection across table cells
   clear the cells instead of deleting pipes.
3. Typing in documents with footnotes or link definitions re-parses
   everything (~5 ms on 750 KB); make footnotes/refs incremental if that
   matters in practice.
4. The source view of a heading or paragraph appears instantly on click and
   can shift text horizontally (syntax becomes visible); consider a short
   transition or keeping the click point fixed.
5. Rich-text copy (HTML) alongside Markdown; a release (bump `Cargo.toml`).

## Architecture

- `doc.rs`: Markdown → `Vec<Block>`. Each block has its `kind`, a container
  stack `conts` (quotes, lists, footnotes) used for indentation and bars, a list
  `marker`, and **`src: Range<usize>`, its byte range in the source**.
  Inline styling is `Rich { text, spans }`, where the text has the Markdown
  syntax stripped.
- `layout.rs` (`Engine`): lazy layout.
  - Only blocks on screen are shaped. Every block has a height (estimated or
    measured), and `ys` holds prefix sums.
  - `measure_some()` refines estimates in 4 ms idle slices.
  - The view anchors on (block, fraction) so changing heights never makes it jump.
  - `invalidate_block(i)` + `ensure()` re-lays out a single block, which is the
    hook for incremental editing.
  - Selection is `Pos { block, part, idx }`, where `idx` indexes `Rich.text`.
    Table cells are separate parts.
- **Padding joiners:** inline code, kbd and badges get a U+2060 joiner at each
  edge (given width by an `AttrShape`), so the text in a Pango layout is not
  identical to `Rich.text`. Always convert with `Part::layout_idx` /
  `Part::text_idx` (tested in `layout::tests`).
- `view.rs`: `DocView` is a `GtkScrollable`.
  - `relayout(f)` applies an engine change while keeping the scroll anchor.
  - `sync()` lays out the visible range and updates the adjustment.
  - Images decode on `gio::spawn_blocking`; their dimensions come from the file
    header (`image_size`) so layout never jumps.
- `paint.rs`: the `Painter` trait has two backends, the GTK snapshot (the app)
  and Cairo (headless `--render`/`--bench`). Anything new you draw must go
  through it so headless tests keep working.
- `app.rs`: the window and actions.
  - The startup preload parses the file on a worker thread while GTK initialises.
  - Live reload uses a `gio::FileMonitor` with a 60 ms debounce. It handles
    atomic rename-over saves. `set_doc(…, keep_position = true)` keeps the
    scroll position.

## Original v2 plan (from v1; mostly done, see "v2 status")

1. **Buffer:** keep the source in a rope (e.g. `ropey`). The engine owns it, and
   blocks keep `src` ranges into it.
2. **Typora model:** the block containing the cursor switches to a *source
   view*: its raw Markdown is laid out with syntax markers dimmed, and inline
   styles are still applied. All other blocks stay rendered. Moving the cursor
   out re-renders the block.
3. **Incremental parse:** after an edit, re-parse from the start of the edited
   block to the next blank-line boundary (extend if a fence or list continues),
   splice the new blocks into `doc.blocks`, shift the later `src` ranges, then
   `invalidate_block` and `ensure`. Budget: under 2 ms per keystroke for normal
   paragraphs. Full re-parse of a 750 KB file is 5 ms, which is an acceptable
   fallback.
4. **Input:** use `GtkIMContext` (IME, compose keys and dead keys), then a caret
   with blinking and keyboard navigation (by character, word, line and block),
   shift-selection, and undo/redo coalesced by word.
5. **Files:** save, save-as, a dirty indicator, a prompt on close, and stopping
   the file monitor from reloading on our own saves.
6. **Keep the speed honest:** add per-keystroke timing to `--bench` (simulate
   typing into a large file) and keep the scroll and paint numbers in the README.

## Testing without a display

The agent's shell has no display. What works:

- `cargo test`: parser, highlighter, TeX, offset mapping, selection and hit-testing.
- `folio --render out.png [--scroll Y] [--dark] [--select-all] FILE`: renders
  through the Cairo painter, so you can look at the PNG.
- `folio --bench FILE`: engine timings. Generate the big test file with
  `python3 samples/gen_big.py`.
- **Real GTK app:** `scripts/headless-gnome.sh env FOLIO_SCREENSHOT=/tmp/s.png ./target/release/folio FILE`
  runs an isolated headless GNOME Shell (its own D-Bus session) and screenshots
  the live window. `FOLIO_SCROLL=y` and `FOLIO_FIND=text` script it.
- **Typing and clicking in the real app:** `scripts/drive.py` injects key and
  pointer events through Mutter's RemoteDesktop D-Bus API (so the input method
  path is exercised) and screenshots after `DELAY` ms. One D-Bus connection
  must hold the session (separate `gdbus call`s lose it), and a button press
  needs a short pause before its release. Quirks: named keys must be sent as
  keycodes (as keysyms, keys that are also on the keypad arrive wrong or not
  at all), and even then `End` is sometimes dropped, so cover navigation in
  unit tests; `Esc` and `F9` never reach the app even as keycodes, so test the
  outline with its button (or `gdbus call … org.gtk.Actions.Activate outline`
  on `/dev/folio/Folio/window/1`); the pointer must not touch the top-left
  hot corner; and window placement varies between runs, so a click can miss
  (retry). Popovers are separate surfaces and don't appear in screenshots.
- `folio --render out.png --caret OFFSET FILE` renders with the source view of
  the block at OFFSET.
  `FOLIO_STARTUP=1` prints startup phase timings and memory use. GL in this
  harness is software-rendered, so GPU startup numbers are pessimistic.
- Dead ends, don't retry: GTK's Broadway backend plus headless Chromium (new
  headless mode screenshots before Broadway draws), and snap Chromium can't
  write to `/tmp`.

## Release

CI (`.github/workflows/release.yml`) runs on `v*` tags, pull requests and
manual dispatch, not on pushes to `main`. A `v*` tag tests, builds and
publishes a GitHub Release with an x86_64 tarball. Bump `Cargo.toml`'s
version first. The repo (`nikosk/folio`) is public, so Actions runs (~1 min
each with the cache warm) are free. There is no LICENSE yet.

## Known v1 limitations

- Math is TeX converted to Unicode, which is fine inline but not for matrices
  or complex layout.
- No outline sidebar, print or export.
- The default window size of 940×1000 may be too tall for small laptop screens.
