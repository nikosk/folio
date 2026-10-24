//! Editing: caret and selection as byte offsets into the Markdown source,
//! the Typora-style source view of the block under the caret, incremental
//! re-parsing after each edit, and undo.
//!
//! Every edit follows the same steps: put the rendered blocks back in place
//! of the source-view block, change the text, re-parse the few top-level
//! blocks around the change, then turn the block under the caret into its
//! source view again. Only the blocks that changed lose their layout.

use crate::doc::{self, Block, Cont, Kind, Marker, SrcStyle};
use crate::layout::{Engine, Pos};
use crate::table::{self, Grid};
use std::ops::Range;

pub struct EditState {
    /// Caret byte offset; `None` while just reading.
    pub caret: Option<usize>,
    /// The other end of the selection (equal to the caret when there is none).
    pub anchor: usize,
    /// Horizontal position kept across up/down moves.
    pub goal_x: Option<f64>,
    active: Option<Active>,
    undo: Vec<Change>,
    redo: Vec<Change>,
    /// Undo depth that matches the file on disk; `None` if unreachable.
    saved: Option<usize>,
    group: Option<Group>,
    /// Incremented on every change to the text.
    pub version: u64,
    /// Number of edits that re-parsed the whole document (for `--bench`).
    pub full_parses: usize,
    /// Offset of a closing bracket or quote that typing inserted: typing it
    /// steps over it, and Backspace right after the opener removes both.
    pending_close: Option<usize>,
}

impl Default for EditState {
    fn default() -> Self {
        EditState {
            caret: None,
            anchor: 0,
            goal_x: None,
            active: None,
            undo: Vec::new(),
            redo: Vec::new(),
            saved: Some(0),
            group: None,
            version: 0,
            full_parses: 0,
            pending_close: None,
        }
    }
}

/// The block under the caret, replaced by its source view.
struct Active {
    idx: usize,
    /// The rendered blocks it replaced.
    stash: Vec<Block>,
    /// For a table, which stays rendered: the cell (part) shown as source.
    cell: Option<usize>,
}

/// Row and column operations on the table under the caret.
#[derive(Clone, Copy, Debug)]
pub enum TableOp {
    RowAbove,
    RowBelow,
    ColLeft,
    ColRight,
    DelRow,
    DelCol,
    Align(table::Align),
}

pub struct Heading {
    pub level: u8,
    pub text: String,
    /// Source offset of the heading.
    pub off: usize,
}

struct Change {
    at: usize,
    del: String,
    ins: String,
    before: (usize, usize),
    after: (usize, usize),
}

/// Kinds of consecutive edits that undo as one step.
#[derive(Clone, Copy, PartialEq)]
enum Group {
    Type,
    Back,
    Del,
}

fn line_start(s: &str, off: usize) -> usize {
    s[..off].rfind('\n').map_or(0, |k| k + 1)
}

/// Whether the line before line start `ls` is blank.
fn blank_before(s: &str, ls: usize) -> bool {
    let prev = &s[..ls.saturating_sub(1)];
    prev[line_start(prev, prev.len())..].trim().is_empty()
}

fn prev_char(s: &str, off: usize) -> usize {
    s[..off].chars().next_back().map_or(off, |c| off - c.len_utf8())
}

fn next_char(s: &str, off: usize) -> usize {
    s[off..].chars().next().map_or(off, |c| off + c.len_utf8())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Offset of the previous word start (skipping whitespace and punctuation first).
fn prev_word(s: &str, off: usize) -> usize {
    let mut i = off;
    let mut it = s[..off].char_indices().rev().peekable();
    while let Some(&(k, c)) = it.peek() {
        if is_word(c) {
            break;
        }
        i = k;
        it.next();
    }
    for (k, c) in it {
        if !is_word(c) {
            break;
        }
        i = k;
    }
    i
}

fn next_word(s: &str, off: usize) -> usize {
    let mut i = off;
    let mut it = s[off..].char_indices().peekable();
    while let Some(&(_, c)) = it.peek() {
        if is_word(c) {
            break;
        }
        i += c.len_utf8();
        it.next();
    }
    for (_, c) in it {
        if !is_word(c) {
            break;
        }
        i += c.len_utf8();
    }
    i
}

/// Byte offset of the list marker token (and the end of it, including a task
/// box and the spaces after) within a line prefix like `"> 1. [ ] "`.
fn list_marker(prefix: &str) -> Option<(usize, usize, bool)> {
    let mut found = None;
    let mut i = 0;
    let b = prefix.as_bytes();
    while i < b.len() {
        if b[i] == b' ' || b[i] == b'\t' || b[i] == b'>' {
            i += 1;
            continue;
        }
        let start = i;
        if b[i] == b'[' && b.get(i + 2) == Some(&b']') {
            i += 3;
        }
        while i < b.len() && b[i] != b' ' && b[i] != b'\t' {
            i += 1;
        }
        let tok = &prefix[start..i];
        let is_num = tok.len() > 1
            && tok[..tok.len() - 1].bytes().all(|c| c.is_ascii_digit())
            && (tok.ends_with('.') || tok.ends_with(')'));
        if matches!(tok, "-" | "*" | "+") || is_num {
            found = Some((start, prefix.len(), false));
        } else if matches!(tok, "[ ]" | "[x]" | "[X]") {
            if let Some(f) = &mut found {
                f.2 = true;
            }
        } else {
            return None;
        }
    }
    found
}

/// The prefix for the item after one whose line starts with `prefix`.
fn next_item_prefix(prefix: &str) -> String {
    let Some((m, _, _)) = list_marker(prefix) else { return prefix.to_string() };
    let rest = &prefix[m..];
    let tok_end = rest.find([' ', '\t']).unwrap_or(rest.len());
    let tok = &rest[..tok_end];
    let mut out = prefix[..m].to_string();
    if let Some(n) = tok[..tok.len() - 1].parse::<u64>().ok().filter(|_| tok.len() > 1) {
        out.push_str(&format!("{}{}", n + 1, &tok[tok.len() - 1..]));
    } else {
        out.push_str(tok);
    }
    out.push_str(&rest[tok_end..].replace("[x]", "[ ]").replace("[X]", "[ ]"));
    out
}

/// Length of the blockquote markers (`> > `) at the start of a line.
fn quote_len(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut i, mut last) = (0, 0);
    loop {
        let mut j = i;
        while j < b.len() && j - i < 3 && b[j] == b' ' {
            j += 1;
        }
        if b.get(j) != Some(&b'>') {
            return last;
        }
        j += 1;
        if b.get(j) == Some(&b' ') {
            j += 1;
        }
        (i, last) = (j, j);
    }
}

/// The prefix for continuation lines of a block whose first line starts
/// with `prefix`: quote markers stay, list markers become spaces.
fn cont_prefix(prefix: &str) -> String {
    prefix.chars().map(|c| if c == '>' { '>' } else { ' ' }).collect()
}

/// A cell's range while edited: its text, extended over trailing spaces up
/// to the caret (but not past `limit`).
fn cell_reach(r: Range<usize>, limit: usize, caret: Option<usize>) -> Range<usize> {
    match caret {
        Some(c) if c > r.end && c <= limit => r.start..c,
        _ => r,
    }
}

fn is_url(s: &str) -> bool {
    ["http://", "https://", "mailto:"].iter().any(|p| s.starts_with(p) && s.len() > p.len()) && !s.contains(char::is_whitespace)
}

fn closer(open: char) -> Option<char> {
    Some(match open {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '"' | '`' | '*' | '_' | '~' => open,
        _ => return None,
    })
}

/// Text that could make a local re-parse wrong (HTML blocks that end at
/// a closing tag rather than a blank line, footnotes, link definitions).
fn risky(s: &str) -> bool {
    s.contains("]:") || s.contains("[^") || {
        let l = s.to_ascii_lowercase();
        ["<!--", "-->", "<pre", "<script", "<style", "<textarea", "<?", "<!["].iter().any(|t| l.contains(t))
    }
}

impl Engine {
    // ---- queries ------------------------------------------------------

    pub fn editing(&self) -> bool {
        self.ed.caret.is_some()
    }

    pub fn is_dirty(&self) -> bool {
        self.ed.saved != Some(self.ed.undo.len())
    }

    pub fn mark_saved(&mut self) {
        self.ed.saved = Some(self.ed.undo.len());
        self.ed.group = None;
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        let c = self.ed.caret?;
        let a = self.ed.anchor;
        (a != c).then(|| a.min(c)..a.max(c))
    }

    /// Index of the block whose source range contains `off`, preferring text.
    fn block_of(&self, off: usize) -> Option<usize> {
        let mut found = None;
        for (i, b) in self.doc.blocks.iter().enumerate() {
            if b.src.start <= off && off <= b.src.end {
                if b.part_count() > 0 {
                    return Some(i);
                }
                found.get_or_insert(i);
            }
        }
        found
    }

    /// Map a text position to a source offset.
    pub fn pos_to_off(&self, p: Pos) -> usize {
        match self.doc.blocks.get(p.block) {
            Some(b) => b.text_to_src(p.part, p.idx),
            None => self.src.len(),
        }
    }

    /// Map a source offset to a text position (in the nearest block).
    pub fn off_to_pos(&self, off: usize) -> Pos {
        if let Some(i) = self.block_of(off) {
            let (part, idx) = self.doc.blocks[i].src_to_text(off);
            return Pos { block: i, part, idx };
        }
        let i = self.doc.blocks.partition_point(|b| b.src.end < off);
        Pos { block: i.min(self.len().saturating_sub(1)), part: 0, idx: 0 }
    }

    /// The block before or after block `i` in source order (footnotes are
    /// moved to the end of the block list, so index order can differ).
    fn neighbour(&self, i: usize, after: bool) -> Option<usize> {
        if !self.doc.global {
            return if after { Some(i + 1).filter(|&j| j < self.len()) } else { i.checked_sub(1) };
        }
        let r = self.doc.blocks[i].src.clone();
        // The footnote separator has an empty range and no source of its own.
        let cands = self.doc.blocks.iter().enumerate().filter(|(j, b)| *j != i && !(b.src.is_empty() && matches!(b.kind, Kind::Rule)));
        if after {
            cands.filter(|(_, b)| b.src.start >= r.end).min_by_key(|(_, b)| b.src.start).map(|(j, _)| j)
        } else {
            cands.filter(|(_, b)| b.src.end <= r.start).max_by_key(|(_, b)| b.src.end).map(|(j, _)| j)
        }
    }

    pub fn active_block(&self) -> Option<usize> {
        self.ed.active.as_ref().map(|a| a.idx)
    }

    /// Source ranges of a table block's cells, in part order. A missing
    /// cell (short row) sits at the end of its line.
    fn table_cells(&self, i: usize) -> Vec<Range<usize>> {
        self.table_cells_limit(i).into_iter().map(|(r, _)| r).collect()
    }

    /// Cell ranges with how far each may extend while edited (see
    /// `table::row_cells_limit`): spaces typed at the end of a cell stay in it.
    fn table_cells_limit(&self, i: usize) -> Vec<(Range<usize>, usize)> {
        let b = &self.doc.blocks[i];
        let Kind::Table { aligns, rows } = &b.kind else { return Vec::new() };
        let cols = aligns.len();
        let s0 = b.src.start;
        let prefix = s0 - line_start(&self.src, s0);
        let mut out = Vec::with_capacity(rows.len() * cols);
        for (k, (at, line)) in table::lines(&self.src[b.src.clone()], prefix).into_iter().enumerate() {
            if k == 1 {
                continue;
            }
            let at = s0 + at;
            let mut cells: Vec<(Range<usize>, usize)> =
                table::row_cells_limit(line).into_iter().take(cols).map(|(r, l)| (at + r.start..at + r.end, at + l)).collect();
            let end = at + line.trim_end().len();
            cells.resize(cols, (end..end, end));
            out.extend(cells);
        }
        out.truncate(rows.len() * cols);
        out
    }

    /// The table cell being edited: (block, part, source range). The range
    /// reaches the caret if it sits in spaces after the text.
    fn active_cell(&self) -> Option<(usize, usize, Range<usize>)> {
        let a = self.ed.active.as_ref()?;
        let k = a.cell?;
        let (r, limit) = self.table_cells_limit(a.idx).get(k)?.clone();
        Some((a.idx, k, cell_reach(r, limit, self.ed.caret)))
    }

    fn table_cols(&self, i: usize) -> usize {
        match &self.doc.blocks[i].kind {
            Kind::Table { aligns, .. } => aligns.len(),
            _ => 0,
        }
    }

    pub fn in_table(&self) -> bool {
        self.active_cell().is_some()
    }

    /// Headings in document order, for the outline.
    pub fn outline(&self) -> Vec<Heading> {
        self.doc
            .blocks
            .iter()
            .filter_map(|b| {
                let (level, text) = match &b.kind {
                    Kind::Heading { level, text, .. } => (*level, text.text.clone()),
                    Kind::Source { rich, style: SrcStyle::Heading(level) } => {
                        let first = rich.text.lines().next().unwrap_or("").trim();
                        (*level, first.trim_start_matches('#').trim_end_matches('#').trim().to_string())
                    }
                    _ => return None,
                };
                Some(Heading { level, text, off: b.src.start })
            })
            .collect()
    }

    /// Index (into `outline()`) of the heading of the section at document y.
    pub fn heading_at(&mut self, y: f64) -> Option<usize> {
        let mut cur = None;
        let mut n = 0;
        for i in 0..self.len() {
            if !matches!(self.doc.blocks[i].kind, Kind::Heading { .. } | Kind::Source { style: SrcStyle::Heading(_), .. }) {
                continue;
            }
            if self.block_y(i) > y {
                break;
            }
            cur = Some(n);
            n += 1;
        }
        cur
    }

    /// Document y of the block containing source offset `off`.
    pub fn off_y(&mut self, off: usize) -> Option<f64> {
        let i = self.block_of(off)?;
        Some(self.block_y(i))
    }

    // ---- the source view ---------------------------------------------

    fn deactivate(&mut self) {
        if let Some(a) = self.ed.active.take() {
            self.splice(a.idx..a.idx + 1, a.stash);
        }
    }

    /// Show the block under the caret as source (and the previous one rendered).
    fn refresh_active(&mut self) {
        let Some(c) = self.ed.caret else {
            self.deactivate();
            return;
        };
        if let Some(a) = &self.ed.active {
            let b = &self.doc.blocks[a.idx];
            if b.src.start <= c && c <= b.src.end {
                match a.cell {
                    None => return,
                    // Same cell, and its shown text still reaches exactly as far.
                    Some(k)
                        if self.table_cells_limit(a.idx).get(k).is_some_and(|(r, l)| {
                            r.start <= c && c <= r.end.max(*l) && cell_reach(r.clone(), *l, Some(c)).len() == b.part_text(k).len()
                        }) =>
                    {
                        return;
                    }
                    Some(_) => {}
                }
            }
        }
        self.deactivate();
        if let Some(i) = self.block_of(c)
            && matches!(self.doc.blocks[i].kind, Kind::Table { .. })
            && self.activate_table(i, c)
        {
            return;
        }
        let (range, src, like) = match self.block_of(c) {
            Some(i) => {
                // Blocks sharing source (e.g. from one HTML block) switch together.
                let r = self.doc.blocks[i].src.clone();
                let (mut lo, mut hi) = (i, i + 1);
                let overlaps = |b: &Block| b.src.start < r.end && b.src.end > r.start;
                while lo > 0 && overlaps(&self.doc.blocks[lo - 1]) {
                    lo -= 1;
                }
                while hi < self.len() && overlaps(&self.doc.blocks[hi]) {
                    hi += 1;
                }
                let s = self.doc.blocks[lo..hi].iter().map(|b| b.src.start).min().unwrap();
                let e = self.doc.blocks[lo..hi].iter().map(|b| b.src.end).max().unwrap();
                (lo..hi, s..e, Some(i))
            }
            None => {
                // Between blocks (after Enter): an empty paragraph at the caret.
                let at = self.doc.blocks.iter().position(|b| b.src.start > c).unwrap_or(self.len());
                (at..at, c..c, None)
            }
        };
        let (conts, marker, center, kind) = match like {
            Some(i) => {
                let b = &self.doc.blocks[range.start];
                let kind = doc::source_kind(&self.src[src.clone()], &self.doc.blocks[i].kind);
                (b.conts.clone(), b.marker, b.center, kind)
            }
            None => (Vec::new(), None, false, doc::source_kind("", &Kind::Rule)),
        };
        let block = Block { kind, conts, marker, center, src, map: vec![[0, 0, 0]] };
        let at = range.start;
        let stash: Vec<Block> = self.doc.blocks.splice(range.clone(), [block]).collect();
        self.splice_state(range, 1);
        self.ed.active = Some(Active { idx: at, stash, cell: None });
    }

    /// A table keeps its grid while edited: only the cell nearest the caret
    /// shows its source, and the caret is moved into that cell.
    fn activate_table(&mut self, i: usize, c: usize) -> bool {
        let cells = self.table_cells_limit(i);
        let dist = |(r, l): &(Range<usize>, usize)| if c < r.start { r.start - c } else { c.saturating_sub(r.end.max(*l)) };
        let Some(k) = (0..cells.len()).min_by_key(|&k| dist(&cells[k])) else { return false };
        let (r, limit) = cells[k].clone();
        let cell = cell_reach(r, limit, Some(c));
        let snapped = c.clamp(cell.start, cell.end);
        if self.ed.anchor == c {
            self.ed.anchor = snapped;
        }
        self.ed.caret = Some(snapped);
        let b = &self.doc.blocks[i];
        let Kind::Table { aligns, rows } = &b.kind else { return false };
        let cols = aligns.len();
        let mut rows = rows.clone();
        if let Kind::Source { rich, .. } = doc::source_kind(&self.src[cell.clone()], &Kind::Para(Default::default())) {
            rows[k / cols][k % cols] = rich;
        }
        let mut map: Vec<[u32; 3]> = b.map.iter().filter(|m| m[0] as usize != k).copied().collect();
        map.push([k as u32, 0, (cell.start - b.src.start) as u32]);
        map.sort_unstable();
        let block = Block {
            kind: Kind::Table { aligns: aligns.clone(), rows },
            conts: b.conts.clone(),
            marker: b.marker,
            center: b.center,
            src: b.src.clone(),
            map,
        };
        let stash: Vec<Block> = self.doc.blocks.splice(i..i + 1, [block]).collect();
        self.splice_state(i..i + 1, 1);
        self.ed.active = Some(Active { idx: i, stash, cell: Some(k) });
        true
    }

    /// Recompute the painted caret and selection from the offsets.
    fn sync_view(&mut self) {
        let Some(c) = self.ed.caret else {
            self.caret = None;
            return;
        };
        let cp = self.off_to_pos(c);
        self.caret = Some(cp);
        self.sel = (self.ed.anchor != c).then(|| (self.off_to_pos(self.ed.anchor), cp));
    }

    // ---- caret --------------------------------------------------------

    /// Move the caret; with `extend`, keep the selection anchor. `activate`
    /// switches the source view to the caret's block (not while dragging).
    pub fn set_caret(&mut self, off: usize, extend: bool, activate: bool) {
        let mut off = off.min(self.src.len());
        while !self.src.is_char_boundary(off) {
            off -= 1;
        }
        if !extend || self.ed.caret.is_none() {
            self.ed.anchor = off;
        }
        self.ed.caret = Some(off);
        self.ed.group = None;
        self.ed.pending_close = None;
        if activate {
            self.refresh_active();
        }
        self.sync_view();
    }

    pub fn clear_caret(&mut self) {
        self.ed.caret = None;
        self.ed.goal_x = None;
        self.refresh_active();
        self.sync_view();
        self.sel = None;
    }

    /// Re-attach the source view after a drag selection ends.
    pub fn settle(&mut self) {
        if self.selection().is_none() {
            self.refresh_active();
            self.sync_view();
        }
    }

    pub fn caret_pos(&self) -> Option<Pos> {
        self.caret
    }

    pub fn select_range(&mut self, r: Range<usize>) {
        self.set_caret(r.start, false, true);
        self.set_caret(r.end, true, true);
    }

    pub fn select_word_at(&mut self, off: usize) {
        let s = &self.src;
        let c = s[off..].chars().next();
        let (a, b) = if c.is_some_and(is_word) {
            let a = s[..off].char_indices().rev().take_while(|(_, c)| is_word(*c)).last().map_or(off, |(k, _)| k);
            let b = off + s[off..].chars().take_while(|c| is_word(*c)).map(char::len_utf8).sum::<usize>();
            (a, b)
        } else {
            (off, next_char(s, off))
        };
        self.select_range(a..b);
    }

    pub fn select_block_at(&mut self, off: usize) {
        if let Some(i) = self.block_of(off) {
            let r = self.doc.blocks[i].src.clone();
            self.select_range(r);
        }
    }

    pub fn select_all_text(&mut self) {
        self.set_caret(0, false, true);
        self.set_caret(self.src.len(), true, true);
    }

    /// Step left or right by character or word, crossing to the neighbouring
    /// block at block edges.
    pub fn move_h(&mut self, forward: bool, word: bool, extend: bool) {
        let Some(c) = self.ed.caret else { return };
        self.ed.goal_x = None;
        if let (Some(r), false) = (self.selection(), extend) {
            self.set_caret(if forward { r.end } else { r.start }, false, true);
            return;
        }
        if let Some((i, k, cell)) = self.active_cell() {
            // Within a cell by character or word; at its edges, to the next cell.
            let cells = self.table_cells(i);
            let target = if forward && c >= cell.end {
                match cells.get(k + 1) {
                    Some(n) => n.start,
                    None => self.neighbour(i, true).map_or(c, |n| self.doc.blocks[n].src.start),
                }
            } else if !forward && c <= cell.start {
                match k.checked_sub(1) {
                    Some(p) => cells[p].end,
                    None => self.neighbour(i, false).map_or(c, |p| self.doc.blocks[p].src.end),
                }
            } else if forward {
                (if word { next_word(&self.src, c) } else { next_char(&self.src, c) }).min(cell.end)
            } else {
                (if word { prev_word(&self.src, c) } else { prev_char(&self.src, c) }).max(cell.start)
            };
            return self.set_caret(target, extend, true);
        }
        let target = match self.active_block() {
            Some(i) => {
                let src = self.doc.blocks[i].src.clone();
                if forward && c >= src.end {
                    self.neighbour(i, true).map_or(c, |n| self.doc.blocks[n].src.start)
                } else if !forward && c <= src.start {
                    self.neighbour(i, false).map_or(c, |p| self.doc.blocks[p].src.end)
                } else if word {
                    if forward { next_word(&self.src, c).min(src.end) } else { prev_word(&self.src, c).max(src.start) }
                } else if forward {
                    next_char(&self.src, c)
                } else {
                    prev_char(&self.src, c)
                }
            }
            None if forward => next_char(&self.src, c),
            None => prev_char(&self.src, c),
        };
        self.set_caret(target, extend, true);
    }

    /// Move one visual line up or down.
    pub fn move_v(&mut self, down: bool, extend: bool) {
        let Some(p) = self.caret else { return };
        let goal = match self.ed.goal_x {
            Some(g) => g,
            None => self.caret_rect(p).map_or(0.0, |r| r.0),
        };
        if let Some(t) = self.vmove(p, down, goal) {
            let off = self.pos_to_off(t);
            self.set_caret(off, extend, true);
        }
        self.ed.goal_x = Some(goal);
    }

    pub fn move_line_edge(&mut self, end: bool, extend: bool) {
        let Some(p) = self.caret else { return };
        let t = self.line_edge(p, end);
        let off = self.pos_to_off(t);
        self.ed.goal_x = None;
        self.set_caret(off, extend, true);
    }

    // ---- editing ------------------------------------------------------

    /// Replace `r` with `text` and re-parse; does not record undo.
    fn apply(&mut self, r: Range<usize>, text: &str) {
        self.deactivate();
        let risky = risky(&self.src[r.clone()]) || risky(text);
        self.src.replace_range(r.clone(), text);
        self.reparse(r.start, r.end, r.start + text.len(), risky);
        self.ed.version += 1;
    }

    /// Replace `r` with `text`, recording undo and leaving the caret after it.
    fn edit(&mut self, r: Range<usize>, text: &str, group: Option<Group>) {
        self.edit_sel(r, text, group, None)
    }

    /// Like `edit`, but leaves the selection at `sel` (anchor, caret; offsets
    /// in the new text) when given.
    fn edit_sel(&mut self, r: Range<usize>, text: &str, group: Option<Group>, sel: Option<(usize, usize)>) {
        let before = (self.ed.anchor, self.ed.caret.unwrap_or(r.start));
        let del = self.src[r.clone()].to_string();
        let at = r.start;
        self.ed.pending_close = match self.ed.pending_close {
            Some(p) if r.start <= p && r.end <= p => Some(p - r.len() + text.len()),
            _ => None,
        };
        self.apply(r, text);
        let end = at + text.len();
        let after = sel.unwrap_or((end, end));
        self.ed.redo.clear();
        if self.ed.saved.is_some_and(|s| s > self.ed.undo.len()) {
            self.ed.saved = None;
        }
        let can_merge = group.is_some() && group == self.ed.group && self.ed.saved != Some(self.ed.undo.len());
        let merged = can_merge
            && match (self.ed.undo.last_mut(), group) {
                (Some(last), Some(Group::Type)) if last.at + last.ins.len() == at && del.is_empty() => {
                    // A space after a word starts a new undo step.
                    let starts_word = text.starts_with(char::is_whitespace) && !last.ins.ends_with(char::is_whitespace);
                    !starts_word && {
                        last.ins.push_str(text);
                        last.after = after;
                        true
                    }
                }
                (Some(last), Some(Group::Back)) if last.at == at + del.len() && text.is_empty() => {
                    last.del.insert_str(0, &del);
                    last.at = at;
                    last.after = after;
                    true
                }
                (Some(last), Some(Group::Del)) if last.at == at && text.is_empty() => {
                    last.del.push_str(&del);
                    true
                }
                _ => false,
            };
        if !merged {
            self.ed.undo.push(Change { at, del, ins: text.to_string(), before, after });
        }
        self.ed.anchor = after.0;
        self.ed.caret = Some(after.1);
        self.ed.goal_x = None;
        self.refresh_active();
        self.sync_view();
        self.ed.group = group;
    }

    fn sel_or_caret(&self) -> Option<Range<usize>> {
        let c = self.ed.caret?;
        Some(self.selection().unwrap_or(c..c))
    }

    /// Several disjoint replacements `(offset, removed length, text)`, sorted,
    /// as one undo step; caret and anchor stay on the same text.
    fn multi_edit(&mut self, edits: Vec<(usize, usize, String)>) {
        let (Some(first), Some(last)) = (edits.first(), edits.last()) else { return };
        let (start, end) = (first.0, last.0 + last.1);
        let mut out = String::new();
        let mut at = start;
        for (p, n, t) in &edits {
            out.push_str(&self.src[at..*p]);
            out.push_str(t);
            at = p + n;
        }
        let map = |o: usize| {
            let mut d = 0isize;
            for (p, n, t) in &edits {
                if o >= p + n {
                    d += t.len() as isize - *n as isize;
                } else if o > *p {
                    return (*p as isize + d) as usize + t.len();
                }
            }
            (o as isize + d) as usize
        };
        let c = self.ed.caret.unwrap_or(start);
        let sel = (map(self.ed.anchor), map(c));
        self.edit_sel(start..end, &out, None, Some(sel));
    }

    /// Whether brackets and quotes pair up here (not in code).
    fn smart_ok(&self) -> bool {
        self.active_block().is_some_and(|i| !matches!(self.doc.blocks[i].kind, Kind::Source { style: SrcStyle::Code { .. } | SrcStyle::Mono, .. }))
    }

    /// Typed text: brackets and quotes pair up, a typed closer steps over
    /// the one that was inserted, and with a selection `*`, `_`, `~`, `` ` ``,
    /// quotes and brackets wrap it.
    pub fn type_text(&mut self, text: &str) {
        let mut it = text.chars();
        let (Some(ch), None) = (it.next(), it.next()) else { return self.insert(text) };
        let Some(c) = self.ed.caret else { return };
        if !self.smart_ok() {
            return self.insert(text);
        }
        if self.selection().is_none() && self.ed.pending_close == Some(c) && self.src[c..].starts_with(ch) && ")]}\"`".contains(ch) {
            return self.set_caret(c + ch.len_utf8(), false, true);
        }
        let Some(close) = closer(ch) else { return self.insert(text) };
        if let Some(r) = self.selection() {
            let inner = self.src[r.clone()].to_string();
            let s = r.start + ch.len_utf8();
            return self.edit_sel(r, &format!("{ch}{inner}{close}"), None, Some((s, s + inner.len())));
        }
        let next = self.src[c..].chars().next();
        let prev = self.src[..c].chars().next_back();
        let pairs = match ch {
            '*' | '_' | '~' => false,
            '"' => prev.is_none_or(|p| p.is_whitespace() || "([{".contains(p)),
            '`' => prev != Some('`') && next != Some('`'),
            _ => true,
        } && next.is_none_or(|n| n.is_whitespace() || ")]}.,;:!?".contains(n));
        if !pairs {
            return self.insert(text);
        }
        let after = c + ch.len_utf8();
        self.edit_sel(c..c, &format!("{ch}{close}"), None, Some((after, after)));
        self.ed.pending_close = Some(after);
    }

    /// Pasted text: a URL pasted over a selection links it.
    pub fn paste_text(&mut self, text: &str) {
        let url = text.trim();
        if let Some(r) = self.selection() {
            let sel = self.src[r].to_string();
            if is_url(url) && !is_url(&sel) && !sel.contains('\n') {
                return self.insert(&format!("[{sel}]({url})"));
            }
        }
        self.insert(text)
    }

    /// Ctrl+K: wrap the selection in a link, ready to type the target.
    pub fn insert_link(&mut self) {
        let Some(r) = self.sel_or_caret() else { return };
        let sel = self.src[r.clone()].to_string();
        let (text, at) = if is_url(&sel) {
            (format!("[]({sel})"), r.start + 1)
        } else {
            (format!("[{sel}]()"), r.start + sel.len() + 3)
        };
        self.edit_sel(r, &text, None, Some((at, at)));
    }

    /// Typed or pasted text replaces the selection.
    pub fn insert(&mut self, text: &str) {
        let Some(r) = self.sel_or_caret() else { return };
        let mut text = text.replace("\r\n", "\n");
        if self.active_cell().is_some() {
            // A cell is one line, and a bare pipe would split it.
            text = text.replace('\n', " ").replace('|', "\\|");
        }
        let group = (r.is_empty() && text.chars().count() == 1).then_some(Group::Type);
        self.edit(r, &text, group);
    }

    pub fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some(r) => {
                self.edit(r, "", None);
                true
            }
            None => false,
        }
    }

    /// Line prefix of the active block (container markers before its text).
    fn active_prefix(&self) -> Option<(usize, String)> {
        let b = &self.doc.blocks[self.active_block()?];
        let ls = line_start(&self.src, b.src.start);
        Some((ls, self.src[ls..b.src.start].to_string()))
    }

    /// Enter: a new paragraph, list item or code line, Typora-style.
    pub fn newline(&mut self, soft: bool) {
        let Some(c) = self.ed.caret else { return };
        if self.delete_selection() {
            return self.newline(soft);
        }
        if let Some((i, k, _)) = self.active_cell() {
            // Shift+Enter breaks the line inside the cell; Enter goes down a
            // row, adding one at the bottom.
            if soft {
                return self.edit(c..c, "<br>", None);
            }
            let cols = self.table_cols(i);
            if let Some(r) = self.table_cells(i).get(k + cols) {
                return self.set_caret(r.end, false, true);
            }
            // A new row starts a new record: its first cell.
            if self.table_op(TableOp::RowBelow)
                && let Some(a) = self.active_block()
                && let Some(r) = self.table_cells(a).get((k / cols + 1) * cols)
            {
                self.set_caret(r.start, false, true);
            }
            return;
        }
        let Some(i) = self.active_block() else { return };
        if !soft && (self.complete_fence(i, c) || self.row_to_table(i, c)) {
            return;
        }
        let b = &self.doc.blocks[i];
        let (ls, prefix) = self.active_prefix().unwrap_or_default();
        let in_code = matches!(&b.kind, Kind::Source { style: SrcStyle::Code { .. } | SrcStyle::Mono, .. });
        let text = if soft || in_code {
            // Keep the indentation of the current line.
            let cur = line_start(&self.src, c);
            let indent: String = self.src[cur..c].chars().take_while(|c| *c == ' ' || *c == '\t' || *c == '>').collect();
            format!("\n{indent}")
        } else if matches!(b.marker, Some(Marker::Bullet(_) | Marker::Number(_) | Marker::Task(_))) {
            if b.src.is_empty() {
                // Enter on an empty item ends the list.
                // A blank line separates what follows from the list.
                let r = ls..b.src.start;
                let outer = &prefix[..list_marker(&prefix).map_or(prefix.len(), |m| m.0)];
                let text = format!("{}\n{outer}", outer.trim_end());
                self.edit(r, &text, None);
                return;
            }
            let tight = b.conts.iter().rev().find_map(|c| match c {
                Cont::List { id } => Some(self.doc.is_tight(*id)),
                _ => None,
            });
            let next = next_item_prefix(&prefix);
            let quote: String = prefix.chars().take_while(|c| *c == ' ' || *c == '>').collect();
            if tight == Some(false) { format!("\n{}\n{next}", quote.trim_end()) } else { format!("\n{next}") }
        } else {
            let keep: String = prefix.chars().take_while(|c| *c == ' ' || *c == '>' || *c == '\t').collect();
            format!("\n{}\n{keep}", keep.trim_end())
        };
        self.edit(c..c, &text, None);
    }

    /// Enter at the end of a fence opener (```` ```lang ````) that has no
    /// closing fence yet: add the closing fence, caret on the line between.
    fn complete_fence(&mut self, i: usize, c: usize) -> bool {
        let b = &self.doc.blocks[i];
        if !matches!(&b.kind, Kind::Source { style: SrcStyle::Code { .. }, .. }) {
            return false;
        }
        let ls = line_start(&self.src, c);
        let line_end = self.src[c..].find('\n').map_or(self.src.len(), |k| c + k);
        let body = &self.src[ls..c];
        let q = quote_len(body);
        let opener = body[q..].trim_start();
        let fence: String = opener.chars().take_while(|ch| *ch == '`' || *ch == '~').collect();
        if ls > b.src.start || fence.len() < 3 || !self.src[c..line_end].trim().is_empty() || opener[fence.len()..].contains(['`', '~']) {
            return false;
        }
        let text = &self.src[b.src.clone()];
        let closed = text.lines().count() > 1 && text.lines().last().is_some_and(|l| l[quote_len(l)..].trim_start().starts_with(&fence[..3]));
        if closed {
            return false;
        }
        let indent = cont_prefix(&body[..body.len() - opener.len()]);
        let at = c + 1 + indent.len();
        self.edit_sel(c..c, &format!("\n{indent}\n{indent}{fence}"), None, Some((at, at)));
        true
    }

    /// Enter after a one-line paragraph like `| a | b |`: make it a table.
    fn row_to_table(&mut self, i: usize, c: usize) -> bool {
        let b = &self.doc.blocks[i];
        let line = &self.src[b.src.clone()];
        if !matches!(&b.kind, Kind::Source { style: SrcStyle::Text, .. }) || c != b.src.end || line.contains('\n') {
            return false;
        }
        let t = line.trim();
        if t.len() < 3 || !t.starts_with('|') || !t.ends_with('|') {
            return false;
        }
        let head: Vec<String> = table::row_cells(t).into_iter().map(|r| t[r].to_string()).collect();
        let g = Grid { aligns: vec![table::Align::None; head.len()], rows: vec![head.clone(), vec![String::new(); head.len()]] };
        let ls = line_start(&self.src, b.src.start);
        let (out, cells) = g.render(&cont_prefix(&self.src[ls..b.src.start]));
        let at = b.src.start + cells[head.len()].start;
        let r = b.src.clone();
        self.edit_sel(r, &out, None, Some((at, at)));
        true
    }

    pub fn backspace(&mut self, word: bool) {
        let Some(c) = self.ed.caret else { return };
        if self.delete_selection() {
            return;
        }
        // Right after typing an opening bracket or quote: remove the pair.
        if !word && self.ed.pending_close == Some(c) {
            let open = self.src[..c].chars().next_back();
            if let Some(o) = open
                && closer(o).is_some_and(|cl| self.src[c..].starts_with(cl))
            {
                let r = c - o.len_utf8()..c + o.len_utf8();
                return self.edit(r, "", None);
            }
        }
        if let Some((_, _, cell)) = self.active_cell() {
            if c > cell.start {
                let to = if word { prev_word(&self.src, c).max(cell.start) } else { prev_char(&self.src, c) };
                self.edit(to..c, "", Some(Group::Back));
            }
            return;
        }
        let Some(i) = self.active_block() else { return };
        let b = &self.doc.blocks[i];
        if c > b.src.start {
            let to = if word { prev_word(&self.src, c).max(b.src.start) } else { prev_char(&self.src, c) };
            return self.edit(to..c, "", Some(Group::Back));
        }
        // At the start of the block.
        if matches!(b.marker, Some(Marker::Bullet(_) | Marker::Number(_) | Marker::Task(_))) {
            let (ls, prefix) = self.active_prefix().unwrap_or_default();
            if let Some((m, end, task)) = list_marker(&prefix) {
                let from = if task { ls + prefix[..end].rfind('[').unwrap_or(m) } else { ls + m };
                return self.edit(from..b.src.start, "", None);
            }
        }
        let Some(p) = self.neighbour(i, false) else { return };
        let prev = &self.doc.blocks[p];
        match prev.kind {
            Kind::Para(_) | Kind::Heading { .. } | Kind::Math(_) => {
                let r = prev.src.end..c;
                self.edit(r, "", None)
            }
            Kind::Rule | Kind::Image { .. } => {
                let r = prev.src.start..c;
                self.edit(r, "", None)
            }
            _ => {
                let to = prev.src.end;
                self.set_caret(to, false, true)
            }
        }
    }

    pub fn delete_forward(&mut self, word: bool) {
        let Some(c) = self.ed.caret else { return };
        if self.delete_selection() {
            return;
        }
        if let Some((_, _, cell)) = self.active_cell() {
            if c < cell.end {
                let to = if word { next_word(&self.src, c).min(cell.end) } else { next_char(&self.src, c) };
                self.edit(c..to, "", Some(Group::Del));
            }
            return;
        }
        let Some(i) = self.active_block() else { return };
        let b = &self.doc.blocks[i];
        if c < b.src.end {
            let to = if word { next_word(&self.src, c).min(b.src.end) } else { next_char(&self.src, c) };
            return self.edit(c..to, "", Some(Group::Del));
        }
        let Some(next) = self.neighbour(i, true).map(|n| &self.doc.blocks[n]) else { return };
        match next.kind {
            Kind::Para(_) | Kind::Heading { .. } | Kind::Math(_) => {
                let r = c..next.src.start;
                self.edit(r, "", None)
            }
            Kind::Rule | Kind::Image { .. } => {
                let r = c..next.src.end;
                self.edit(r, "", None)
            }
            _ => {}
        }
    }

    /// Tab: next table cell (a new row after the last), list indent, or
    /// spaces. Shift+Tab: previous cell or list outdent.
    pub fn tab(&mut self, back: bool) {
        if self.ed.caret.is_none() {
            return;
        }
        if let Some((i, k, _)) = self.active_cell() {
            let cells = self.table_cells(i);
            if back {
                if k > 0 {
                    self.set_caret(cells[k - 1].end, false, true);
                }
            } else if k + 1 < cells.len() {
                self.set_caret(cells[k + 1].end, false, true);
            } else if self.table_op(TableOp::RowBelow)
                && let Some(a) = self.active_block()
            {
                // Into the first cell of the new row.
                let cols = self.table_cols(a);
                if let Some(r) = self.table_cells(a).get((k / cols + 1) * cols) {
                    self.set_caret(r.start, false, true);
                }
            }
            return;
        }
        if !self.list_indent(back) && !back {
            self.insert("    ");
        }
    }

    /// The list item under the caret: (first block, last block, depth index
    /// of its list in `conts`). The item includes its nested blocks.
    fn list_item(&self) -> Option<(usize, usize, usize)> {
        let i = self.active_block()?;
        let bl = &self.doc.blocks;
        let k = bl[i].conts.iter().rposition(|c| matches!(c, Cont::List { .. }))?;
        let base = &bl[i].conts[..=k];
        let within = |j: usize| bl[j].conts.len() > k && &bl[j].conts[..=k] == base;
        let is_item = |j: usize| {
            matches!(bl[j].marker, Some(Marker::Bullet(_) | Marker::Number(_) | Marker::Task(_)))
                && bl[j].conts.len() == k + 1
                && within(j)
        };
        let mut h = i;
        while !is_item(h) {
            h = h.checked_sub(1).filter(|&j| within(j))?;
        }
        let mut e = h;
        while e + 1 < bl.len() && within(e + 1) && !is_item(e + 1) {
            e += 1;
        }
        Some((h, e, k))
    }

    /// Indent the item under the caret into the previous item's list, or
    /// outdent it to its parent's level. Returns false outside lists.
    fn list_indent(&mut self, back: bool) -> bool {
        let Some((h, e, k)) = self.list_item() else { return false };
        let bl = &self.doc.blocks;
        let prefix_of = |j: usize| &self.src[line_start(&self.src, bl[j].src.start)..bl[j].src.start];
        let indent_of = |p: &str| {
            let q = quote_len(p);
            p[q..].len() - p[q..].trim_start_matches(' ').len()
        };
        let item_at = |j: usize, depth: usize| {
            matches!(bl[j].marker, Some(Marker::Bullet(_) | Marker::Number(_) | Marker::Task(_)))
                && bl[j].conts.len() == depth + 1
                && bl[j].conts[..=depth] == bl[h].conts[..=depth]
        };
        let ls = line_start(&self.src, bl[h].src.start);
        let mut lines = vec![ls];
        while let Some(n) = self.src[*lines.last().unwrap()..bl[e].src.end].find('\n') {
            lines.push(lines.last().unwrap() + n + 1);
        }
        let blank = |p: usize| {
            let l = &self.src[p..self.src[p..].find('\n').map_or(self.src.len(), |n| p + n)];
            l[quote_len(l)..].trim().is_empty()
        };
        let mut edits = Vec::new();
        if !back {
            // Nest under the previous item at its content column.
            let Some(sib) = (0..h).rev().take_while(|&j| bl[j].conts.len() > k && bl[j].conts[..=k] == bl[h].conts[..=k]).find(|&j| item_at(j, k)) else {
                return true;
            };
            let sp = prefix_of(sib);
            let Some((m, _, _)) = list_marker(sp) else { return true };
            let tok = sp[m..].find([' ', '\t']).unwrap_or(sp.len() - m);
            let gap = sp[m + tok..].len() - sp[m + tok..].trim_start().len();
            let width = tok + gap.clamp(1, 4);
            for &p in lines.iter().filter(|&&p| !blank(p)) {
                let q = quote_len(&self.src[p..]);
                edits.push((p + q, 0, " ".repeat(width)));
            }
            // A new nested ordered list starts at 1.
            let hp = prefix_of(h);
            if let Some((hm, _, _)) = list_marker(hp) {
                let digits = hp[hm..].bytes().take_while(u8::is_ascii_digit).count();
                if digits > 0 {
                    edits.push((ls + hm, digits, "1".into()));
                }
            }
        } else {
            let Some(kp) = bl[h].conts[..k].iter().rposition(|c| matches!(c, Cont::List { .. })) else { return true };
            let Some(parent) = (0..h).rev().find(|&j| item_at(j, kp)) else { return true };
            let w = indent_of(prefix_of(h)).saturating_sub(indent_of(prefix_of(parent)));
            for &p in &lines {
                let q = quote_len(&self.src[p..]);
                let n = self.src[p + q..].bytes().take(w).take_while(|b| *b == b' ').count();
                if n > 0 {
                    edits.push((p + q, n, String::new()));
                }
            }
        }
        edits.sort_by_key(|e| e.0);
        self.multi_edit(edits);
        true
    }

    /// Insert or delete rows and columns, or align a column, rewriting the
    /// table with padded columns. Returns false if not in a table.
    pub fn table_op(&mut self, op: TableOp) -> bool {
        let Some((i, k, _)) = self.active_cell() else { return false };
        let b = &self.doc.blocks[i];
        let ls = line_start(&self.src, b.src.start);
        let prefix = cont_prefix(&self.src[ls..b.src.start]);
        let mut g = Grid::parse(&self.src[b.src.clone()], b.src.start - ls);
        let cols = g.cols();
        if cols == 0 {
            return false;
        }
        let (mut r, mut col) = (k / cols, k % cols);
        match op {
            TableOp::RowAbove => {
                r = r.max(1);
                g.rows.insert(r, vec![String::new(); cols]);
            }
            TableOp::RowBelow => {
                r += 1;
                g.rows.insert(r, vec![String::new(); cols]);
            }
            TableOp::ColLeft | TableOp::ColRight => {
                col += matches!(op, TableOp::ColRight) as usize;
                g.rows.iter_mut().for_each(|row| row.insert(col, String::new()));
                g.aligns.insert(col, table::Align::None);
            }
            TableOp::DelRow => {
                if r == 0 {
                    return false;
                }
                g.rows.remove(r);
                r = r.min(g.rows.len() - 1);
            }
            TableOp::DelCol => {
                if cols == 1 {
                    return false;
                }
                g.rows.iter_mut().for_each(|row| {
                    row.remove(col);
                });
                g.aligns.remove(col);
                col = col.min(cols - 2);
            }
            TableOp::Align(a) => g.aligns[col] = a,
        }
        let (out, cells) = g.render(&prefix);
        let at = b.src.start + cells[r * g.cols() + col].end;
        let range = b.src.clone();
        self.edit_sel(range, &out, None, Some((at, at)));
        true
    }

    /// Ctrl+T: a 3-column table after the current block (or in place of an
    /// empty paragraph), with the first header cell selected.
    pub fn insert_table(&mut self) {
        let Some(c) = self.ed.caret else { return };
        let head: Vec<String> = (1..=3).map(|n| format!("Column {n}")).collect();
        let g = Grid { aligns: vec![table::Align::None; 3], rows: vec![head, vec![String::new(); 3]] };
        let (out, cells) = g.render("");
        let (at, before) = match self.active_block().map(|i| self.doc.blocks[i].src.clone()) {
            Some(r) if !r.is_empty() => (r.end, "\n\n"),
            _ => (c, ""),
        };
        let base = at + before.len();
        self.edit_sel(at..at, &format!("{before}{out}"), None, Some((base + cells[0].start, base + cells[0].end)));
    }

    pub fn selected_source(&self) -> String {
        self.selection().map(|r| self.src[r].to_string()).unwrap_or_default()
    }

    pub fn undo(&mut self) -> bool {
        let Some(ch) = self.ed.undo.pop() else { return false };
        self.apply(ch.at..ch.at + ch.ins.len(), &ch.del);
        self.ed.group = None;
        let (a, c) = ch.before;
        self.ed.redo.push(ch);
        self.set_caret(a, false, true);
        self.set_caret(c, true, true);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(ch) = self.ed.redo.pop() else { return false };
        self.apply(ch.at..ch.at + ch.del.len(), &ch.ins);
        self.ed.group = None;
        let (a, c) = ch.after;
        self.ed.undo.push(ch);
        self.set_caret(a, false, true);
        self.set_caret(c, true, true);
        true
    }

    // ---- incremental parsing -----------------------------------------

    /// Index range of the top-level container run containing block `i`.
    fn run(&self, i: usize) -> Range<usize> {
        let bl = &self.doc.blocks;
        let top = bl[i].conts.first();
        if top.is_none() {
            return i..i + 1;
        }
        let (mut a, mut b) = (i, i + 1);
        while a > 0 && bl[a - 1].conts.first() == top {
            a -= 1;
        }
        while b < bl.len() && bl[b].conts.first() == top {
            b += 1;
        }
        a..b
    }

    /// After `src[at..old_end]` became `src[at..new_end]`, re-parse the
    /// top-level blocks around it (falling back to the whole document).
    fn reparse(&mut self, at: usize, old_end: usize, new_end: usize, risky: bool) {
        let delta = new_end as isize - old_end as isize;
        let n = self.len();
        if self.doc.global || risky || n == 0 {
            return self.full_reparse(at, old_end, delta, risky);
        }
        let bl = &self.doc.blocks;
        let lo = bl.partition_point(|b| b.src.end < at);
        let hi = bl.partition_point(|b| b.src.start <= old_end);
        // One untouched block (and its whole container) on each side, then
        // out to blank lines: without one, a line can join its neighbour
        // (lazy continuation, setext underline).
        let mut a = if lo == 0 { 0 } else { self.run(lo - 1).start };
        let mut b = self.run(hi.min(n - 1)).end;
        let start = loop {
            if a == 0 {
                break 0;
            }
            let s = line_start(&self.src, bl[a].src.start.min(at));
            if s >= bl[a - 1].src.end && blank_before(&self.src, s) {
                break s.min(at);
            }
            a = self.run(a - 1).start;
        };
        // Blocks from `hi` on are past the edit: shift them to the new text.
        let shift = |o: usize| (o as isize + delta) as usize;
        let end = loop {
            if b >= n {
                break self.src.len();
            }
            let ls = line_start(&self.src, shift(bl[b].src.start));
            if ls >= shift(bl[b - 1].src.end) && blank_before(&self.src, ls) {
                break ls;
            }
            b = self.run(b).end;
        };
        if end < start || end < new_end || start > at {
            return self.full_reparse(at, old_end, delta, false);
        }
        // Front matter runs to its closing line, which may lie past the region.
        if start == 0 && (self.src.starts_with("---") || self.src.starts_with("+++")) {
            let fm_end = match bl.first() {
                Some(b) if matches!(b.kind, Kind::FrontMatter(_)) => b.src.end,
                _ => self.src.find('\n').unwrap_or(self.src.len()),
            };
            if at <= fm_end {
                return self.full_reparse(at, old_end, delta, false);
            }
        }
        let mut new = doc::parse_region(&self.src, start..end, &self.doc);
        if new.global {
            return self.full_reparse(at, old_end, delta, true);
        }
        // If the last block of the region (which the edit did not touch)
        // now parses differently, the change may reach further: re-parse all.
        if b < n {
            let old = &self.doc.blocks[b - 1];
            let same = new.blocks.last().is_some_and(|l| {
                let s = (old.src.start as isize + delta) as usize..(old.src.end as isize + delta) as usize;
                l.src == s && l.conts.len() == old.conts.len() && std::mem::discriminant(&l.kind) == std::mem::discriminant(&old.kind)
            });
            if !same {
                return self.full_reparse(at, old_end, delta, false);
            }
        }
        // Anchors only need recomputing when heading text changed; otherwise
        // the new headings take the old (document-wide unique) slugs.
        let heads = |bs: &[Block]| -> Vec<String> {
            bs.iter()
                .filter_map(|b| match &b.kind {
                    Kind::Heading { text, .. } => Some(text.text.clone()),
                    _ => None,
                })
                .collect()
        };
        let reslug = heads(&self.doc.blocks[a..b]) != heads(&new.blocks);
        if !reslug {
            let old: Vec<String> = self.doc.blocks[a..b]
                .iter()
                .filter_map(|b| match &b.kind {
                    Kind::Heading { slug, .. } => Some(slug.clone()),
                    _ => None,
                })
                .collect();
            let slots = new.blocks.iter_mut().filter_map(|b| match &mut b.kind {
                Kind::Heading { slug, .. } => Some(slug),
                _ => None,
            });
            for (slot, s) in slots.zip(old) {
                *slot = s;
            }
        }
        let k = new.blocks.len();
        self.doc.links.append(&mut new.links);
        self.doc.loose.append(&mut new.loose);
        // Unchanged neighbours keep their layout.
        let moved = |o: usize| if o > old_end { (o as isize + delta) as usize } else { o };
        self.splice_reuse(a..b, new.blocks, |o, n| {
            moved(o.src.start) == n.src.start
                && moved(o.src.end) == n.src.end
                && (o.src.end < at || o.src.start > old_end)
                && std::mem::discriminant(&o.kind) == std::mem::discriminant(&n.kind)
                && o.conts.len() == n.conts.len()
                && o.marker == n.marker
                && o.center == n.center
        });
        for blk in &mut self.doc.blocks[a + k..] {
            blk.src = (blk.src.start as isize + delta) as usize..(blk.src.end as isize + delta) as usize;
        }
        if reslug {
            self.doc.reslug();
        }
    }

    /// Re-parse everything, keeping the layout of blocks before and after
    /// the change. With `relayout`, all layouts are dropped (the edit can
    /// change how earlier text renders, e.g. a new link definition).
    fn full_reparse(&mut self, _at: usize, _old_end: usize, delta: isize, relayout: bool) {
        self.ed.full_parses += 1;
        let new = doc::parse(&self.src);
        let (n, m) = (self.len(), new.blocks.len());
        let same = |a: &Block, b: &Block, d: isize| {
            a.src.start as isize + d == b.src.start as isize
                && a.src.end as isize + d == b.src.end as isize
                && std::mem::discriminant(&a.kind) == std::mem::discriminant(&b.kind)
        };
        let mut p = 0;
        while p < n.min(m) && same(&self.doc.blocks[p], &new.blocks[p], 0) {
            p += 1;
        }
        let mut s = 0;
        while s < (n - p).min(m - p) && same(&self.doc.blocks[n - 1 - s], &new.blocks[m - 1 - s], delta) {
            s += 1;
        }
        self.doc = new;
        self.splice_state(p..n - s, m - p - s);
        if relayout {
            self.invalidate(false);
        }
    }
}

#[cfg(test)]
mod tests_support {
    pub use super::*;
    pub use crate::theme::Theme;
    pub use gtk::pango::prelude::FontMapExt;
    pub use std::path::PathBuf;

    pub fn engine(src: &str) -> Engine {
        let ctx = pangocairo::FontMap::new().create_context();
        let mut e = Engine::new(ctx, src.to_string(), doc::parse(src), PathBuf::new(), Theme::light(), 1.0);
        e.set_width(800.0);
        e
    }

    /// The incremental result must match a fresh parse of the same text.
    pub fn check(e: &Engine) {
        let mut e2 = engine(&e.src);
        e2.ed.caret = e.ed.caret;
        e2.refresh_active();
        let key = |b: &Block| (b.src.clone(), std::mem::discriminant(&b.kind), b.conts.len(), b.marker, b.part_text(0).to_string());
        let a: Vec<_> = e.doc.blocks.iter().map(key).collect();
        let b: Vec<_> = e2.doc.blocks.iter().map(key).collect();
        if a != b {
            let k = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
            panic!("block {k} differs\n  incremental: {:?}\n  fresh:       {:?}\nsource: {:?}", a.get(k), b.get(k), e.src);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::*;

    fn typed(src: &str, at: usize, s: &str) -> Engine {
        let mut e = engine(src);
        e.set_caret(at, false, true);
        for ch in s.chars() {
            match ch {
                '\n' => e.newline(false),
                '\u{8}' => e.backspace(false),
                c => e.insert(&c.to_string()),
            }
            check(&e);
        }
        e
    }

    #[test]
    fn typing_into_paragraph() {
        let e = typed("# Title\n\nHello\n\n- a\n- b\n", 14, " world");
        assert_eq!(e.src, "# Title\n\nHello world\n\n- a\n- b\n");
        assert_eq!(e.ed.full_parses, 0);
        assert!(matches!(e.doc.blocks[1].kind, Kind::Source { .. }));
    }

    #[test]
    fn enter_makes_paragraphs_and_items() {
        let e = typed("Hello\n", 5, "\nnext");
        assert_eq!(e.src, "Hello\n\nnext\n");
        let e = typed("- one\n", 5, "\ntwo\n\nafter");
        assert_eq!(e.src, "- one\n- two\n\nafter\n");
        let e = typed("1. one\n", 6, "\ntwo");
        assert_eq!(e.src, "1. one\n2. two\n");
        let e = typed("- [x] done\n", 10, "\nnew");
        assert_eq!(e.src, "- [x] done\n- [ ] new\n");
    }

    #[test]
    fn markup_changes_block_kind() {
        let e = typed("abc\n\nnext\n", 0, "## ");
        assert_eq!(e.src, "## abc\n\nnext\n");
        e.doc.blocks.iter().for_each(|b| assert!(!matches!(b.kind, Kind::Para(_)) || b.part_text(0) == "next"));
        let e = typed("a\n\nb\n\nc\n\nd\n\ne\n", 3, "```\n");
        assert!(e.ed.full_parses > 0, "an unclosed fence swallows the rest of the document");
    }

    #[test]
    fn backspace_joins_and_unlists() {
        let e = typed("one\n\ntwo\n", 5, "\u{8}");
        assert_eq!(e.src, "onetwo\n");
        let e = typed("- a\n- b\n", 6, "\u{8}");
        assert_eq!(e.src, "- a\nb\n");
        let e = typed("- [ ] a\n", 6, "\u{8}");
        assert_eq!(e.src, "- a\n");
    }

    #[test]
    fn undo_redo_groups_words() {
        let mut e = typed("x\n", 1, " hello world");
        assert!(e.is_dirty());
        e.undo();
        assert_eq!(e.src, "x hello\n");
        e.undo();
        assert_eq!(e.src, "x\n");
        assert!(!e.is_dirty());
        e.redo();
        e.redo();
        assert_eq!(e.src, "x hello world\n");
        check(&e);
    }

    #[test]
    fn rendered_positions_map_to_source() {
        let e = engine("Some **bold** and `code` here\n");
        let b = &e.doc.blocks[0];
        assert_eq!(b.part_text(0), "Some bold and code here");
        assert_eq!(b.text_to_src(0, 5), 7); // 'b' after "**"
        assert_eq!(b.text_to_src(0, 14), 19); // 'c' after '`'
        assert_eq!(b.src_to_text(7), (0, 5));
        assert_eq!(b.src_to_text(6), (0, 5)); // inside "**" clamps to the run
    }

    /// Run keys: chars are typed (smart input), `\n` Enter, `\t` Tab,
    /// `\u{1}` Shift+Tab, `\u{8}` Backspace.
    fn keys(src: &str, at: usize, keys: &str) -> Engine {
        let mut e = engine(src);
        e.set_caret(at, false, true);
        for ch in keys.chars() {
            match ch {
                '\n' => e.newline(false),
                '\t' => e.tab(false),
                '\u{1}' => e.tab(true),
                '\u{8}' => e.backspace(false),
                c => e.type_text(&c.to_string()),
            }
            check(&e);
        }
        e
    }

    fn caret_at(e: &Engine) -> String {
        let c = e.ed.caret.unwrap();
        format!("{}|{}", &e.src[..c], &e.src[c..])
    }

    #[test]
    fn list_indent_and_outdent() {
        let e = keys("- a\n- b\n  - c\n", 7, "\t");
        assert_eq!(caret_at(&e), "- a\n  - b|\n    - c\n");
        let e = keys("- a\n- b\n  - c\n", 7, "\t\u{1}");
        assert_eq!(caret_at(&e), "- a\n- b|\n  - c\n");
        // Ordered items nest at their content column and restart at 1.
        let e = keys("1. a\n2. b\n", 9, "\t");
        assert_eq!(e.src, "1. a\n   1. b\n");
        let e = keys("> - a\n> - b\n", 11, "\t");
        assert_eq!(e.src, "> - a\n>   - b\n");
        // The first item has nothing to nest under; outside lists Tab is spaces.
        assert_eq!(keys("- a\n", 3, "\t").src, "- a\n");
        assert_eq!(keys("text\n", 4, "\t").src, "text    \n");
    }

    #[test]
    fn brackets_and_quotes_pair() {
        let e = keys("x\n", 1, " (a)");
        assert_eq!(caret_at(&e), "x (a)|\n");
        let e = keys("x\n", 1, " (\u{8}");
        assert_eq!(caret_at(&e), "x |\n");
        // No pairing before a word, or for an apostrophe-like quote mid-word.
        assert_eq!(keys("word\n", 0, "(").src, "(word\n");
        assert_eq!(keys("ab\n", 1, "\"").src, "a\"b\n");
        // A selection is wrapped and stays selected.
        let mut e = engine("make bold\n");
        e.select_range(5..9);
        e.type_text("*");
        e.type_text("*");
        assert_eq!(e.src, "make **bold**\n");
        assert_eq!(e.selection(), Some(7..11));
    }

    #[test]
    fn fences_and_tables_from_enter() {
        let e = keys("x\n\n", 3, "```rs\nfn");
        assert_eq!(caret_at(&e), "x\n\n```rs\nfn|\n```");
        let e = keys("x\n\n", 3, "| a | b |\nq");
        assert_eq!(e.src, "x\n\n| a   | b   |\n| --- | --- |\n| q    |     |");
    }

    #[test]
    fn table_cells_edit_in_place() {
        let src = "| a | b |\n|---|---|\n| c |  |\n";
        // Clicking into the delimiter row lands in the nearest cell.
        let mut e = keys(src, 12, "");
        assert_eq!(e.active_cell().map(|c| c.1), Some(1));
        // Typing goes into the cell; a pipe is escaped; Tab moves on.
        let e2 = keys(src, 4, "x|\ty");
        assert_eq!(e2.src, "| ax\\| | by |\n|---|---|\n| c |  |\n");
        assert!(matches!(e2.doc.blocks[0].kind, Kind::Table { .. }));
        // Tab from the last cell adds a row; Enter in the last row too.
        e.set_caret(src.find("|  |").unwrap() + 2, false, true);
        e.tab(false);
        e.type_text("z");
        assert_eq!(e.src, "| a   | b   |\n| --- | --- |\n| c   |     |\n| z    |     |\n");
        check(&e);
        assert!(e.table_op(TableOp::ColRight));
        assert!(e.table_op(TableOp::Align(table::Align::Center)));
        // The caret is in column 0: the new column goes right of it.
        assert_eq!(e.src, "| a   |     | b   |\n| --- | :-: | --- |\n| c   |     |     |\n| z   |     |     |\n");
        assert!(!e.table_op(TableOp::DelRow) || e.src.lines().count() == 3);
        check(&e);
    }

    #[test]
    fn table_cell_keeps_typed_spaces_and_moves_by_row() {
        let src = "| a | b |\n|---|---|\n| c | d |\n";
        let mut e = keys(src, 3, " x");
        assert_eq!(e.src, "| a x | b |\n|---|---|\n| c | d |\n");
        e.move_v(true, false);
        assert_eq!(e.active_cell().map(|c| c.1), Some(2));
        e.move_v(false, false);
        assert_eq!(e.active_cell().map(|c| c.1), Some(0));
    }

    #[test]
    fn outline_lists_headings() {
        let mut e = engine("# One\n\ntext\n\n## Two *b*\n");
        e.set_caret(2, false, true);
        let o: Vec<(u8, String)> = e.outline().into_iter().map(|h| (h.level, h.text)).collect();
        assert_eq!(o, [(1, "One".into()), (2, "Two b".into())]);
        assert_eq!(e.heading_at(0.0), None);
        assert_eq!(e.heading_at(1e6), Some(1));
    }

    #[test]
    fn empty_document() {
        let e = typed("", 0, "# Title\nbody\n- a\nb");
        assert_eq!(e.src, "# Title\n\nbody\n\n- a\n- b");
        let mut e = typed("x", 1, "\u{8}\u{8}");
        assert_eq!(e.src, "");
        e.undo();
        assert_eq!(e.src, "x");
    }

    #[test]
    fn caret_moves_between_blocks() {
        let mut e = engine("one\n\ntwo\n");
        e.set_caret(3, false, true);
        e.move_h(true, false, false);
        assert_eq!(e.ed.caret, Some(5));
        e.move_h(false, false, false);
        assert_eq!(e.ed.caret, Some(3));
        let p = e.caret_pos().unwrap();
        eprintln!("caret {:?} rect {:?}", p, e.caret_rect(p));
        e.move_v(true, false);
        eprintln!("after {:?} {:?}", e.ed.caret, e.caret_pos());
        assert_eq!(e.active_block(), Some(1));
    }
}

#[cfg(test)]
mod fuzz {
    use super::tests_support::*;

    /// Random edits on the showcase document; after each, the spliced block
    /// list must equal a fresh parse.
    #[test]
    fn random_edits_match_full_parse() {
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/samples/showcase.md")).unwrap();
        let pieces = ["a", "b", " ", "\n", "\n\n", "#", "# ", "- ", "* ", "1. ", "> ", "`", "**", "_", "|", "---", "```", "    ", "[x](y)", "é"];
        // Without footnotes the document can be re-parsed incrementally.
        let local = src[..src.find("That's all").unwrap()].to_string();
        let (mut edits, mut full) = (0, 0);
        for seed in 0..40u64 {
            let mut e = engine(if seed % 4 == 0 { &src } else { &local });
            let mut r = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let mut next = |n: usize| {
                r = r.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                ((r >> 33) as usize) % n.max(1)
            };
            for _ in 0..300 {
                let before = (e.src.clone(), e.ed.caret);
                let op = match next(15) {
                    10 => {
                        e.tab(false);
                        "tab".into()
                    }
                    11 => {
                        e.tab(true);
                        "shift-tab".into()
                    }
                    12 | 13 => {
                        let c = "()[]\"`*|x ".chars().nth(next(10)).unwrap().to_string();
                        e.type_text(&c);
                        format!("type {c:?}")
                    }
                    14 => {
                        use table::Align::*;
                        let ops = [TableOp::RowAbove, TableOp::RowBelow, TableOp::ColLeft, TableOp::ColRight, TableOp::DelRow, TableOp::DelCol, TableOp::Align(Center), TableOp::Align(None)];
                        let o = ops[next(ops.len())];
                        e.table_op(o);
                        format!("{o:?}")
                    }
                    0 => {
                        let at = next(e.src.len() + 1);
                        e.set_caret(at, false, true);
                        format!("caret {at}")
                    }
                    1 => {
                        e.backspace(false);
                        "backspace".into()
                    }
                    2 => {
                        e.delete_forward(false);
                        "delete".into()
                    }
                    3 => {
                        e.newline(false);
                        "enter".into()
                    }
                    4 => {
                        e.move_h(next(2) == 0, false, false);
                        "move".into()
                    }
                    _ => {
                        let p = pieces[next(pieces.len())];
                        e.insert(p);
                        format!("insert {p:?}")
                    }
                };
                let ok = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&e)));
                if ok.is_err() {
                    let (src, c) = before;
                    let c = c.unwrap_or(0);
                    let lo = src[..c].char_indices().rev().nth(80).map_or(0, |x| x.0);
                    let hi = c + src[c..].char_indices().nth(80).map_or(src.len() - c, |x| x.0);
                    panic!("seed {seed}: {op} at {c}: {:?}|{:?}", &src[lo..c], &src[c..hi]);
                }
            }
            if seed % 4 != 0 {
                edits += e.ed.version;
                full += e.ed.full_parses;
            }
        }
        eprintln!("{edits} edits, {full} full re-parses");
    }
}
