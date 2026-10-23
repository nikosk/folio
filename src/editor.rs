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
        }
    }
}

/// The block under the caret, replaced by its source view.
struct Active {
    idx: usize,
    /// The rendered blocks it replaced.
    stash: Vec<Block>,
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
                return;
            }
        }
        self.deactivate();
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
        self.ed.active = Some(Active { idx: at, stash });
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
        let before = (self.ed.anchor, self.ed.caret.unwrap_or(r.start));
        let del = self.src[r.clone()].to_string();
        let at = r.start;
        self.apply(r, text);
        let end = at + text.len();
        let after = (end, end);
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
        self.ed.caret = Some(end);
        self.ed.anchor = end;
        self.ed.goal_x = None;
        self.refresh_active();
        self.sync_view();
        self.ed.group = group;
    }

    fn sel_or_caret(&self) -> Option<Range<usize>> {
        let c = self.ed.caret?;
        Some(self.selection().unwrap_or(c..c))
    }

    /// Typed or pasted text replaces the selection.
    pub fn insert(&mut self, text: &str) {
        let Some(r) = self.sel_or_caret() else { return };
        let text = text.replace("\r\n", "\n");
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
        let Some(i) = self.active_block() else { return };
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

    pub fn backspace(&mut self, word: bool) {
        let Some(c) = self.ed.caret else { return };
        if self.delete_selection() {
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
                let op = match next(10) {
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
