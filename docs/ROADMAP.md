# Roadmap

Where Folio goes after v0.4.1. The goals from `HANDOFF.md` still decide
trade-offs: speed first, then looks, then Markdown coverage. Smaller fixes to
existing features stay in `HANDOFF.md` ("Next steps").

## Releases

| Release | Contents |
|---|---|
| **0.5** | Find & replace; images visible while their block is edited; word count |
| **0.6** | Change tracking (see below) |
| **0.7** | Export to HTML and PDF, printing; rich copy and paste |
| **0.8** | Files sidebar and quick open (`Ctrl+P`); spell check |
| Later, maybe | Focus and typewriter modes, wiki links with backlinks, custom themes |

## Typora features Folio lacks

Ranked by how much they are missed in daily use.

1. **Find & replace.** Find exists; replace doesn't. Replace-all should be
   one undo step through `multi_edit`, as list indent is.
2. **Export and print.** HTML comes from pulldown-cmark's HTML output plus
   the theme as CSS. PDF and printing go through `GtkPrintOperation`, which
   hands out a Cairo context per page, so the existing Cairo painter
   (`paint.rs`) can draw pages; the new work is splitting the layout into
   pages.
3. **Rich clipboard.** Paste HTML (from browsers, Google Docs) as Markdown;
   copy HTML alongside Markdown so formatting survives a paste into mail.
4. **Files sidebar and quick open.** Browse the document's folder and jump to
   any Markdown file by name.
5. **Spell check** through Enchant, the library GNOME apps use. Check only the
   visible blocks, and skip code, links and math.

Also missing: focus and typewriter modes, word count (planned for 0.5).

### Left out on purpose

Mermaid diagrams and TeX-quality math. Typora renders both with JavaScript
libraries; doing them properly without a web view means writing a layout
engine for each, which costs the speed and size Folio exists for. Math stays
TeX converted to Unicode, and diagrams stay code blocks.

## Change tracking: the feature Typora doesn't have

Folio and Typora both reload a file when another program changes it, but
neither shows what changed. Reading a `HANDOFF.md` while an agent rewrites
it, or a README after `git pull`, means finding the edits yourself.

- When the file changes on disk, changed blocks get a margin mark and a short
  highlight.
- A key jumps to the next or previous change.
- Optionally, compare against git's `HEAD` instead of the last reload, to see
  what changed since the last commit, rendered rather than as a raw diff.

It fits the existing code: every block knows its source range, and live
reload already re-parses the file, so the new work is a diff over blocks on
reload. Measure its cost on the 750 KB sample with `--bench` before settling
the design.

## Undecided

- **Automatic update checks.** Considered after 0.4.0 and put aside: an
  in-app check plus update-and-restart needs a check on a timer, a restart
  helper (single instance hands files to the old process), and a change to
  the "no network requests" promise. `folio --update` covers it for now.
- **Wiki links with backlinks.** Only useful after the files sidebar, and
  moves Folio towards Obsidian.
