# Folio

A fast, good-looking Markdown editor for Linux, in the spirit of Typora.
Native GTK 4 and Rust, no web engine. The document is always rendered; the
block you are editing shows its Markdown source, with the syntax dimmed.

## Install a release

Download `folio-*-x86_64-linux.tar.gz` from Releases. It needs GTK 4.12+ and
glibc 2.39+ (Ubuntu 24.04, Fedora 40 or newer).

```sh
tar xzf folio-*-x86_64-linux.tar.gz && ./folio-*/install.sh
```

To update later, run `folio --update` (from 0.4.0 on). It fetches the latest
release with `curl` or `wget`, checks its SHA-256 and runs its `install.sh`;
restart Folio afterwards.

## Build & run

```sh
sudo apt install build-essential libgtk-4-dev   # once
cargo build --release
./target/release/folio README.md
./install.sh                                    # optional: ~/.local/bin + desktop entry
```

## Features

- Typora-style editing: click anywhere to place the caret. The block under
  the caret shows its Markdown (syntax dimmed, styles kept); every other
  block stays rendered. `Esc` goes back to reading.
- Enter continues lists (bullets, numbers, task boxes) and quotes; Enter on
  an empty item ends the list. Backspace at the start of an item removes its
  marker, at the start of a paragraph joins it to the one above.
- Tables are edited in place: the grid stays, only the cell you are in shows
  its Markdown. Tab/Shift+Tab and the arrow keys move between cells; Enter
  goes down a row (adding one at the bottom). Right-click for inserting and
  deleting rows and columns and aligning columns (this rewrites the table
  with padded columns). `Ctrl+T` inserts a table; typing `| a | b |` and
  Enter turns the line into one.
- Tab / Shift+Tab indent and outdent list items, with their sub-items.
- Smart input: brackets and quotes close themselves (typing the closer steps
  over it, Backspace removes an empty pair); with text selected, `*`, `_`,
  `~`, `` ` ``, quotes and brackets wrap it. Enter after ```` ```lang ````
  adds the closing fence. `Ctrl+K` makes a link; pasting a URL over selected
  text links it.
- Paste or drop files to link them (images inline, paths relative to the
  document); a pasted image is saved as `images/pasted-<time>.png` next to
  the document.
- Sidebar (`F9` or the header button) with two pages:
  - **Outline:** headings with the current section highlighted; click one
    to jump to it.
  - **Files:** the document's project (its git work tree, or else its
    folder) as a tree of folders and Markdown files, with the open file
    highlighted. Click a file to open it; the up arrow browses the parent
    folder. Hidden files show with `Ctrl+H` (as in Files/Nautilus); a hidden
    folder holding the open file always shows.
  - **Git status** in a repository: **M** modified, **S** staged, **A** added,
    **R** renamed, **N** new (untracked), **!** conflict; ignored files are
    dimmed, and a folder shows a dot in the colour of the most important
    change inside. It updates after `git add`/`commit`/`checkout`, after
    saving, and when the window comes back to the front (git runs in the
    background; without git there are no markings).
- Go to file (`Ctrl+P`): type part of a name to fuzzy-search every Markdown
  file in that project; `↑`/`↓` and `Enter` open one.
- Spell check (`F7` toggles it) through Enchant, with your system's
  Hunspell/Aspell dictionaries for your language. Code, links, math, paths
  and acronyms are skipped. Right-click a marked word for suggestions,
  "Add to Dictionary" or "Ignore". Without libenchant it is simply off.
- Spell checking, hidden files and the sidebar's state are remembered in
  `~/.config/folio/settings`.
- Input goes through the GTK input method, so IME, compose and dead keys work.
- The header menu (left) opens files, copies the current file's path, shows
  it in the file manager, or opens it in another app (the system's "Open
  With" chooser).
- Undo/redo by word, cut/copy/paste (as Markdown), save/save as, a dirty
  marker in the title, and a prompt before closing unsaved work.

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
- Live reload when another program changes the file (your own saves are
  recognised; with unsaved edits you are asked first).
- Relative links to other Markdown files open in place (`Ctrl`+click while
  editing); `Alt+←` goes back.

## Keys

| Action | Keys |
|---|---|
| New / open | `Ctrl+N` / `Ctrl+O` (or drop a file on the window) |
| Save / save as | `Ctrl+S` / `Ctrl+Shift+S` |
| Undo / redo | `Ctrl+Z` / `Ctrl+Shift+Z`, `Ctrl+Y` |
| Cut / copy / paste | `Ctrl+X` / `Ctrl+C` / `Ctrl+V` |
| Bold / italic / link | `Ctrl+B` / `Ctrl+I` / `Ctrl+K` (wraps the selection) |
| Indent / outdent list item | `Tab` / `Shift+Tab` |
| Table: next / previous cell, row below | `Tab` / `Shift+Tab`, `Enter`; `Ctrl+Enter` inserts a row |
| Insert table | `Ctrl+T` |
| Sidebar (outline, files) | `F9` |
| Go to file | `Ctrl+P` |
| Spell check on/off | `F7` |
| Show hidden files (sidebar, go to file) | `Ctrl+H` |
| Copy file path | `Ctrl+Alt+C` |
| Move by word / line edge / document edge | `Ctrl+←→`, `Home`/`End`, `Ctrl+Home`/`Ctrl+End` (add `Shift` to select) |
| Delete word | `Ctrl+Backspace`, `Ctrl+Delete` |
| Line break inside a block | `Shift+Enter` |
| Stop editing (reading mode) | `Esc` |
| Find / next / previous | `Ctrl+F` / `Enter`, `Ctrl+G` / `Shift+Enter`, `Ctrl+Shift+G` |
| Zoom in / out / reset | `Ctrl+=` / `Ctrl+-` / `Ctrl+0`, or `Ctrl`+scroll |
| Toggle dark mode | `Ctrl+D` |
| Reload | `Ctrl+R`, `F5` |
| Back (after following a link) | `Alt+←` |
| Scroll (reading mode) | arrows, `j`/`k`, `Space`/`Shift+Space`, `PgUp`/`PgDn`, `Home`/`End`, `g`/`G` |
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
- **Incremental editing.** A keystroke changes the text, re-parses only the
  top-level blocks around it (out to the nearest blank lines) and re-shapes
  one paragraph; unchanged neighbours keep their layout. Edits that can
  affect the whole document (code fences that lose their end, footnotes,
  link definitions) fall back to a full re-parse, which is still ~5 ms.
- **Spell checking stays off the critical path.** libenchant is opened at
  run time on a worker thread (no link-time dependency), only blocks laid
  out for display are checked, and results are cached per word.
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
| Keystroke: edit + re-parse + layout (typing into a paragraph) | 0.42 ms median, 1.7 ms max |
| Keystroke: Enter / undo | 0.07 ms / 0.06 ms median |
| Keystroke in a 7-row table cell (the table is re-laid out) | 0.42 ms median, 0.7 ms max |
| Spell check: load the dictionary (worker thread) / check one screen, no words cached | 3–4 ms / 1.5–2 ms |
| Scroll step with spell check | 0.9 ms median, 2.7–3.5 ms max |
| Cold start → first frame, Cairo renderer | ~135–150 ms, 59 MB RSS |
| Cold start → first frame, GL renderer | ~300–400 ms (dominated by GL init) |

File size doesn't change startup time: the 750 KB file reaches the screen as
fast as a 3 KB one. Most of the cold start is GTK itself (library loading,
renderer and theme initialisation).

Folio uses GTK's default GPU renderer, which gives smooth scrolling at any
window size. The Cairo software renderer starts faster and uses less memory
but draws every frame on the CPU. Try it with `GSK_RENDERER=cairo folio FILE`.

## Development

```sh
cargo test
folio --render out.png [--width 1000] [--height 1300] [--scroll Y] [--dark] [--select-all] [--caret OFFSET] [--spell] FILE
folio --bench FILE                     # engine and typing timings, no display needed
python3 samples/gen_big.py             # generate samples/big.md for benchmarking
FOLIO_STARTUP=1 folio FILE             # print startup phase timings, exit after first frame
FOLIO_SCREENSHOT=out.png folio FILE    # screenshot the live window and exit (FOLIO_SCROLL, FOLIO_FIND, FOLIO_SIDEBAR, FOLIO_QUICK, FOLIO_OPEN)
scripts/headless-gnome.sh env FOLIO_SCREENSHOT=/tmp/s.png ./target/release/folio FILE   # no display needed
scripts/headless-gnome.sh python3 -I scripts/drive.py /tmp/s.png FILE "click_at(300, 400)" "type('hi')"   # scripted input
```

| File | Role |
|---|---|
| `src/doc.rs` | Markdown (pulldown-cmark) → flat list of blocks with container stacks and source ranges |
| `src/editor.rs` | Editing: caret and selection as source offsets, the source view of the active block, incremental re-parse, undo |
| `src/layout.rs` | Layout engine: lazy per-block Pango layout, virtual heights, painting, hit-testing, selection, find |
| `src/view.rs` | GTK widget (`GtkScrollable`): scrolling, input, async image loading |
| `src/app.rs` | Windows, actions, file loading, live reload, theme, saved settings |
| `src/files.rs` | Files sidebar (lazy GTK tree of the project folder) and go to file (folder walk, fuzzy ranking) |
| `src/git.rs` | Git status for the files sidebar (`git status --porcelain`, parsed) |
| `src/spell.rs` | Spell checking: libenchant via `dlopen`, word splitting, per-word cache |
| `src/update.sh` | `folio --update` (embedded in the binary) |
| `src/paint.rs` | Drawing backends: GTK snapshot (GPU) and Cairo (headless) |
| `src/highlight.rs`, `src/tex.rs` | Code highlighting, TeX → Unicode |

## Not yet

- Images disappear while their block is shown as source.
- Typing in a table doesn't re-pad its columns in the source; row and column
  operations do. Deleting a selection that spans table cells deletes the
  pipes too (undo restores them).
- Ordered lists aren't renumbered in the source after edits (they render
  correctly).
- Copy puts Markdown on the clipboard, not rich text.
