//! Markdown → flat list of renderable blocks.
//!
//! Nesting (quotes, lists, footnotes) is flattened: every leaf block carries
//! the stack of containers it lives in, which is all the renderer needs for
//! indentation, quote bars and list markers. Each block keeps its source
//! byte range so the editor (v2) can map back to the text.

use crate::tex;
use pulldown_cmark::{Alignment, BlockQuoteKind, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::collections::HashMap;
use std::ops::Range;

/// Inline style bits.
pub mod st {
    pub const BOLD: u16 = 1 << 0;
    pub const ITALIC: u16 = 1 << 1;
    pub const CODE: u16 = 1 << 2;
    pub const STRIKE: u16 = 1 << 3;
    pub const LINK: u16 = 1 << 4;
    pub const SUP: u16 = 1 << 5;
    pub const SUB: u16 = 1 << 6;
    pub const MATH: u16 = 1 << 7;
    pub const KBD: u16 = 1 << 8;
    pub const BADGE: u16 = 1 << 9;
    pub const ALERT: u16 = 1 << 10;
    /// Markdown syntax shown dimmed in the source view of the active block.
    pub const SYNTAX: u16 = 1 << 11;
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub style: u16,
    /// Index into `Doc::links`, valid when `style` has `LINK`.
    pub link: u32,
}

#[derive(Default, Clone, Debug)]
pub struct Rich {
    pub text: String,
    pub spans: Vec<Span>,
}

impl Rich {
    pub fn plain(s: &str) -> Rich {
        Rich { text: s.to_string(), spans: Vec::new() }
    }

    fn push(&mut self, s: &str, style: u16, link: u32) {
        if s.is_empty() {
            return;
        }
        let start = self.text.len() as u32;
        self.text.push_str(s);
        let end = self.text.len() as u32;
        if style == 0 {
            return;
        }
        if let Some(last) = self.spans.last_mut()
            && last.end == start && last.style == style && last.link == link {
                last.end = end;
                return;
            }
        self.spans.push(Span { start, end, style, link });
    }

    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }

    /// Trim surrounding whitespace; returns the number of leading bytes cut.
    fn trim(&mut self) -> usize {
        let end = self.text.trim_end().len();
        self.text.truncate(end);
        let start = self.text.len() - self.text.trim_start().len();
        if start > 0 {
            self.text.drain(..start);
        }
        let (s, e) = (start as u32, end as u32);
        self.spans.retain_mut(|sp| {
            sp.start = sp.start.clamp(s, e) - s;
            sp.end = sp.end.clamp(s, e) - s;
            sp.start < sp.end
        });
        start
    }

    pub fn span_at(&self, idx: usize, style: u16) -> Option<&Span> {
        let i = idx as u32;
        self.spans.iter().find(|s| s.style & style != 0 && s.start <= i && i < s.end)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl Alert {
    fn title(self) -> &'static str {
        match self {
            Alert::Note => "Note",
            Alert::Tip => "Tip",
            Alert::Important => "Important",
            Alert::Warning => "Warning",
            Alert::Caution => "Caution",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cont {
    Quote { id: u32, alert: Option<Alert> },
    List { id: u32 },
    Footnote,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Marker {
    Bullet(u8),
    Number(u64),
    Task(bool),
    Footnote(u32),
}

#[derive(Debug)]
pub enum Kind {
    Para(Rich),
    Heading { level: u8, text: Rich, slug: String },
    Code { lang: String, text: String },
    FrontMatter(String),
    Math(Rich),
    Rule,
    /// Row 0 is the header row; all rows have `aligns.len()` cells.
    Table { aligns: Vec<Align>, rows: Vec<Vec<Rich>> },
    Image { url: String, alt: String, width: Option<f64> },
    /// The block under the caret, shown as its Markdown source.
    Source { rich: Rich, style: SrcStyle },
}

#[derive(Debug, Clone, PartialEq)]
pub enum SrcStyle {
    Text,
    Heading(u8),
    /// Monospace on a code background; `body` is the code inside the fences.
    Code { lang: String, body: Range<usize> },
    Mono,
}

#[derive(Debug)]
pub struct Block {
    pub kind: Kind,
    pub conts: Vec<Cont>,
    pub marker: Option<Marker>,
    pub center: bool,
    pub src: Range<usize>,
    /// Source map, sorted: `[part, text offset, source offset]` at the start
    /// of each run of text that was copied from the source. Source offsets
    /// are relative to `src.start`, so moving a block only moves `src`.
    pub map: Vec<[u32; 3]>,
}

impl Block {
    /// Number of selectable text parts (table cells are separate parts).
    pub fn part_count(&self) -> usize {
        match &self.kind {
            Kind::Rule | Kind::Image { .. } => 0,
            Kind::Table { rows, aligns } => rows.len() * aligns.len(),
            _ => 1,
        }
    }

    pub fn part_text(&self, part: usize) -> &str {
        match &self.kind {
            Kind::Para(r) | Kind::Heading { text: r, .. } | Kind::Math(r) | Kind::Source { rich: r, .. } => &r.text,
            Kind::Code { text, .. } | Kind::FrontMatter(text) => text,
            Kind::Table { rows, aligns } => &rows[part / aligns.len()][part % aligns.len()].text,
            Kind::Rule | Kind::Image { .. } => "",
        }
    }

    pub fn part_rich(&self, part: usize) -> Option<&Rich> {
        match &self.kind {
            Kind::Para(r) | Kind::Heading { text: r, .. } | Kind::Math(r) | Kind::Source { rich: r, .. } => Some(r),
            Kind::Table { rows, aligns } => Some(&rows[part / aligns.len()][part % aligns.len()]),
            _ => None,
        }
    }

    /// Map a text position in a part to a source byte offset.
    pub fn text_to_src(&self, part: usize, idx: usize) -> usize {
        let p = part as u32;
        let s0 = self.src.start;
        let k = self.map.partition_point(|m| m[0] < p || (m[0] == p && m[1] as usize <= idx));
        match k.checked_sub(1).map(|k| (k, self.map[k])) {
            Some((k, m)) if m[0] == p => {
                // Clamp to this run so positions in hidden syntax stay inside it.
                let next = self.map.get(k + 1).filter(|n| n[0] == p).map_or(self.src.end, |n| s0 + n[2] as usize);
                let at = s0 + m[2] as usize;
                (at + (idx - m[1] as usize)).min(next.max(at)).min(self.src.end)
            }
            _ if idx == 0 => self.map.iter().find(|m| m[0] == p).map_or(s0, |m| s0 + m[2] as usize),
            _ => self.src.end,
        }
    }

    /// Map a source byte offset inside this block to (part, text position).
    pub fn src_to_text(&self, off: usize) -> (usize, usize) {
        let o = off.saturating_sub(self.src.start) as u32;
        let Some(k) = self.map.iter().rposition(|m| m[2] <= o) else {
            return match self.map.first() {
                Some(m) => (m[0] as usize, m[1] as usize),
                None => (0, if off > self.src.start { 1 } else { 0 }),
            };
        };
        let m = self.map[k];
        let part = m[0] as usize;
        let len = self.part_text(part).len();
        let next = self.map.get(k + 1).filter(|n| n[0] == m[0]).map_or(len, |n| n[1] as usize);
        let mut idx = (m[1] + (o - m[2])) as usize;
        idx = idx.min(next).min(len);
        let t = self.part_text(part);
        while !t.is_char_boundary(idx) {
            idx -= 1;
        }
        (part, idx)
    }
}

#[derive(Default, Debug)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub links: Vec<String>,
    /// Indexed by list container id: true if the list is loose.
    pub loose: Vec<bool>,
    /// Footnotes or link reference definitions make parsing depend on the
    /// whole document, so edits must re-parse all of it.
    pub global: bool,
}

impl Doc {
    pub fn is_tight(&self, list_id: u32) -> bool {
        !self.loose.get(list_id as usize).copied().unwrap_or(false)
    }

    /// Give headings unique anchors in document order (after splicing).
    pub fn reslug(&mut self) {
        let mut seen: HashMap<String, u32> = HashMap::new();
        for b in &mut self.blocks {
            if let Kind::Heading { text, slug, .. } = &mut b.kind {
                *slug = unique_slug(&mut seen, &text.text);
            }
        }
    }
}

fn opts() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM
}

/// Front matter (`---` YAML or `+++` TOML) at the very start of the file:
/// (end of the block, range of its content). pulldown-cmark would also
/// accept such a block mid-document, swallowing text between two rules.
fn front_matter(src: &str) -> Option<(usize, Range<usize>)> {
    let delim = ["---", "+++"].into_iter().find(|d| src.starts_with(d))?;
    let first = src.find('\n')? + 1;
    if !src[3..first].trim().is_empty() || src[first..].lines().next().is_none_or(|l| l.trim().is_empty()) {
        return None;
    }
    let mut at = first;
    for line in src[first..].split_inclusive('\n') {
        let l = line.trim_end();
        if l == delim || (delim == "---" && l == "...") {
            return Some((at + l.len(), first..at));
        }
        at += line.len();
    }
    None
}

pub fn parse(src: &str) -> Doc {
    let Some((end, body)) = front_matter(src) else { return parse_body(src) };
    let mut d = parse_region(src, end..src.len(), &Doc::default());
    let text = src[body.clone()].trim_end_matches('\n').to_string();
    let block = Block {
        kind: Kind::FrontMatter(text),
        conts: Vec::new(),
        marker: None,
        center: false,
        src: 0..end,
        map: vec![[0, 0, body.start as u32]],
    };
    d.blocks.insert(0, block);
    d
}

fn parse_body(src: &str) -> Doc {
    let mut b = Builder { src, ..Default::default() };
    let mut it = Parser::new_ext(src, opts()).into_offset_iter();
    for (ev, r) in it.by_ref() {
        b.event(ev, r);
    }
    let refs = it.reference_definitions().iter().next().is_some();
    let mut d = b.finish();
    d.global |= refs;
    d
}

/// Parse `src[range]` on its own, with block ranges in `src` coordinates and
/// container/link ids starting after `base`'s.
pub fn parse_region(src: &str, range: std::ops::Range<usize>, base: &Doc) -> Doc {
    let mut d = if range.start == 0 { parse(&src[range.clone()]) } else { parse_body(&src[range.clone()]) };
    let (lo, li) = (base.loose.len() as u32, base.links.len() as u32);
    let off = range.start;
    for b in &mut d.blocks {
        b.src = b.src.start + off..b.src.end + off;
        for c in &mut b.conts {
            match c {
                Cont::Quote { id, .. } | Cont::List { id } => *id += lo,
                Cont::Footnote => {}
            }
        }
        let shift = |r: &mut Rich| {
            for sp in r.spans.iter_mut().filter(|s| s.style & st::LINK != 0) {
                sp.link += li;
            }
        };
        match &mut b.kind {
            Kind::Para(r) | Kind::Heading { text: r, .. } | Kind::Math(r) => shift(r),
            Kind::Table { rows, .. } => rows.iter_mut().flatten().for_each(shift),
            _ => {}
        }
    }
    d
}

/// The source view of a block: its Markdown text with syntax dimmed and
/// inline styles applied. `like` is the rendered kind it replaces.
pub fn source_kind(text: &str, like: &Kind) -> Kind {
    let mut style = match like {
        Kind::Heading { level, .. } => SrcStyle::Heading(*level),
        Kind::Table { .. } => SrcStyle::Mono,
        Kind::Code { lang, .. } => SrcStyle::Code { lang: lang.clone(), body: 0..0 },
        Kind::FrontMatter(_) => SrcStyle::Code { lang: "yaml".into(), body: 0..0 },
        Kind::Source { style, .. } => style.clone(),
        _ => SrcStyle::Text,
    };
    let mut marks = vec![st::SYNTAX; text.len()];
    let mut set = |r: Range<usize>, v: u16| {
        for m in &mut marks[r.start.min(text.len())..r.end.min(text.len())] {
            *m = v;
        }
    };
    let mut stack: Vec<u16> = Vec::new();
    let mut body: Option<Range<usize>> = None;
    let mut in_code = false;
    for (ev, r) in Parser::new_ext(text, opts()).into_offset_iter() {
        let cur = stack.iter().fold(0, |a, s| a | s);
        match ev {
            Event::Start(t) => match t {
                Tag::Emphasis => stack.push(st::ITALIC),
                Tag::Strong => stack.push(st::BOLD),
                Tag::Strikethrough => stack.push(st::STRIKE),
                Tag::Link { .. } => stack.push(st::LINK),
                Tag::Image { .. } | Tag::Superscript | Tag::Subscript => stack.push(0),
                Tag::CodeBlock(_) | Tag::MetadataBlock(_) => in_code = true,
                _ => {}
            },
            Event::End(t) => match t {
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image
                | TagEnd::Superscript
                | TagEnd::Subscript => {
                    stack.pop();
                }
                TagEnd::CodeBlock | TagEnd::MetadataBlock(_) => in_code = false,
                _ => {}
            },
            Event::Text(_) if in_code => {
                body = Some(body.map_or(r.clone(), |b| b.start..r.end));
                set(r, 0);
            }
            Event::Text(_) => set(r, cur),
            Event::Code(t) => {
                let s = text[r.clone()].find(&*t).map_or(r.start, |k| r.start + k);
                set(s..s + t.len(), cur | st::CODE);
            }
            // TeX source keeps the body font so the line height stays even.
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                let s = text[r.clone()].find(&*t).map_or(r.start, |k| r.start + k);
                set(s..s + t.len(), cur);
            }
            Event::SoftBreak | Event::HardBreak => set(r, 0),
            _ => {}
        }
    }
    if let SrcStyle::Code { body: b, .. } = &mut style {
        // Without content events (empty block), the body is between the fence lines.
        *b = body.unwrap_or_else(|| {
            let s = text.find('\n').map_or(text.len(), |k| k + 1);
            let e = text.rfind('\n').map_or(s, |k| k.max(s));
            s..e
        });
        set(b.clone(), 0);
        marks.iter_mut().for_each(|m| *m &= st::SYNTAX);
    }
    let mut rich = Rich { text: text.to_string(), spans: Vec::new() };
    let mut k = 0;
    while k < marks.len() {
        let v = marks[k];
        let mut e = k + 1;
        while e < marks.len() && marks[e] == v {
            e += 1;
        }
        if v != 0 {
            rich.spans.push(Span { start: k as u32, end: e as u32, style: v, link: 0 });
        }
        k = e;
    }
    Kind::Source { rich, style }
}

#[derive(Clone, Copy, PartialEq)]
enum Leaf {
    Para,
    Heading(u8),
    Cell,
}

struct Inline {
    leaf: Leaf,
    rich: Rich,
    start: usize,
}

struct CodeB {
    lang: String,
    text: String,
    front: bool,
}

struct ImgB {
    url: String,
    alt: String,
}

#[derive(Default)]
struct Builder<'a> {
    src: &'a str,
    /// Source map of the leaf being built (see `Block::map`).
    map: Vec<[u32; 3]>,
    part: u32,
    blocks: Vec<Block>,
    links: Vec<String>,
    loose: Vec<bool>,
    conts: Vec<Cont>,
    counters: Vec<Option<u64>>,
    marker: Option<Marker>,
    center: bool,
    inl: Option<Inline>,
    styles: Vec<(u16, u32)>,
    style: u16,
    link: u32,
    code: Option<CodeB>,
    html: Option<String>,
    image: Option<ImgB>,
    table: Option<(Vec<Align>, Vec<Vec<Rich>>)>,
    last_end: usize,
    fn_nums: HashMap<String, u32>,
    fn_open: Vec<(u32, usize)>,
    fn_defs: Vec<(u32, Vec<Block>)>,
    slugs: HashMap<String, u32>,
}

impl Builder<'_> {
    fn event(&mut self, ev: Event, r: Range<usize>) {
        match ev {
            Event::Start(tag) => self.start(tag, r),
            Event::End(tag) => self.end(tag, r),
            Event::Text(t) => {
                if let Some(c) = &mut self.code {
                    self.map.push([0, c.text.len() as u32, r.start as u32]);
                    c.text.push_str(&t);
                } else if let Some(h) = &mut self.html {
                    h.push_str(&t);
                } else if let Some(img) = &mut self.image {
                    img.alt.push_str(&t);
                } else {
                    self.text(&t, 0, r);
                }
            }
            Event::Code(t) => {
                let at = self.src[r.clone()].find(&*t).map_or(r.start, |k| r.start + k);
                self.text(&t, st::CODE, at..r.end);
            }
            Event::InlineMath(t) => {
                for (s, extra) in tex::render(&t) {
                    self.text(&s, st::MATH | extra, r.start + 1..r.end);
                }
            }
            Event::DisplayMath(t) => {
                self.break_para(r.end);
                let mut rich = Rich::default();
                for (s, extra) in tex::render(&t) {
                    rich.push(&s, st::MATH | extra, 0);
                }
                rich.trim();
                self.map = vec![[0, 0, r.start as u32 + 2]];
                self.push(Kind::Math(rich), r);
            }
            Event::Html(t) => {
                if let Some(h) = &mut self.html {
                    h.push_str(&t);
                }
            }
            Event::InlineHtml(t) => self.inline_html(&t, r),
            Event::FootnoteReference(label) => {
                let n = self.fn_num(&label);
                let li = self.add_link(format!("#fn:{n}"));
                let link = std::mem::replace(&mut self.link, li);
                self.text(&format!("[{n}]"), st::SUP | st::LINK, r);
                self.link = link;
            }
            Event::SoftBreak => self.text(" ", 0, r),
            Event::HardBreak => self.text("\n", 0, r),
            Event::Rule => {
                self.flush();
                self.push(Kind::Rule, r);
            }
            Event::TaskListMarker(done) => self.marker = Some(Marker::Task(done)),
        }
    }

    fn start(&mut self, tag: Tag, r: Range<usize>) {
        match tag {
            Tag::Paragraph => {
                self.flush();
                if let Some(Cont::List { id }) = self.conts.last() {
                    self.loose[*id as usize] = true;
                }
                self.inl = Some(Inline { leaf: Leaf::Para, rich: Rich::default(), start: r.start });
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.inl = Some(Inline { leaf: Leaf::Heading(level as u8), rich: Rich::default(), start: r.start });
            }
            Tag::BlockQuote(kind) => {
                self.flush();
                let id = self.new_id();
                let alert = kind.map(|k| match k {
                    BlockQuoteKind::Note => Alert::Note,
                    BlockQuoteKind::Tip => Alert::Tip,
                    BlockQuoteKind::Important => Alert::Important,
                    BlockQuoteKind::Warning => Alert::Warning,
                    BlockQuoteKind::Caution => Alert::Caution,
                });
                self.conts.push(Cont::Quote { id, alert });
                if let Some(a) = alert {
                    let mut rich = Rich::default();
                    rich.push(a.title(), st::ALERT | st::BOLD, 0);
                    let line = self.src[r.clone()].find('\n').map_or(r.end, |k| r.start + k);
                    self.push(Kind::Para(rich), r.start..line);
                }
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info
                        .split(|c: char| c.is_whitespace() || c == ',' || c == '{')
                        .next()
                        .unwrap_or("")
                        .to_ascii_lowercase(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some(CodeB { lang, text: String::new(), front: false });
            }
            Tag::MetadataBlock(_) => {
                self.flush();
                self.code = Some(CodeB { lang: "yaml".into(), text: String::new(), front: true });
            }
            Tag::HtmlBlock => {
                self.flush();
                self.html = Some(String::new());
            }
            Tag::List(start) => {
                self.flush();
                let id = self.new_id();
                self.conts.push(Cont::List { id });
                self.counters.push(start);
            }
            Tag::Item => {
                self.flush();
                let depth = self.conts.iter().filter(|c| matches!(c, Cont::List { .. })).count();
                self.marker = Some(match self.counters.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        Marker::Number(*n - 1)
                    }
                    _ => Marker::Bullet(depth.saturating_sub(1) as u8),
                });
            }
            Tag::FootnoteDefinition(label) => {
                self.flush();
                let n = self.fn_num(&label);
                self.conts.push(Cont::Footnote);
                self.marker = Some(Marker::Footnote(n));
                self.fn_open.push((n, self.blocks.len()));
            }
            Tag::Table(aligns) => {
                self.flush();
                let aligns = aligns
                    .iter()
                    .map(|a| match a {
                        Alignment::Center => Align::Center,
                        Alignment::Right => Align::Right,
                        _ => Align::Left,
                    })
                    .collect();
                self.table = Some((aligns, Vec::new()));
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some((_, rows)) = &mut self.table {
                    rows.push(Vec::new());
                }
            }
            Tag::TableCell => {
                if let Some((aligns, rows)) = &self.table {
                    self.part = ((rows.len().max(1) - 1) * aligns.len() + rows.last().map_or(0, Vec::len)) as u32;
                }
                self.inl = Some(Inline { leaf: Leaf::Cell, rich: Rich::default(), start: r.start });
            }
            Tag::Emphasis => self.push_style(st::ITALIC, 0),
            Tag::Strong => self.push_style(st::BOLD, 0),
            Tag::Strikethrough => self.push_style(st::STRIKE, 0),
            Tag::Superscript => self.push_style(st::SUP, 0),
            Tag::Subscript => self.push_style(st::SUB, 0),
            Tag::Link { dest_url, .. } => {
                let li = self.add_link(dest_url.to_string());
                self.push_style(st::LINK, li);
            }
            Tag::Image { dest_url, .. } => {
                self.image = Some(ImgB { url: dest_url.to_string(), alt: String::new() });
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd, r: Range<usize>) {
        self.last_end = self.last_end.max(r.end);
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.flush(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.conts.pop();
            }
            TagEnd::CodeBlock | TagEnd::MetadataBlock(_) => {
                if let Some(c) = self.code.take() {
                    let mut text = c.text;
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    let kind = if c.front { Kind::FrontMatter(text) } else { Kind::Code { lang: c.lang, text } };
                    self.push(kind, r);
                }
            }
            TagEnd::HtmlBlock => {
                if let Some(h) = self.html.take() {
                    self.html_block(&h, r);
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.conts.pop();
                self.counters.pop();
            }
            TagEnd::Item => {
                self.flush();
                if self.marker.is_some() {
                    // Empty item: the caret goes after the marker.
                    let line = &self.src[r.clone()];
                    let at = r.start + line.find('\n').unwrap_or(line.len());
                    self.map.clear();
                    self.blocks.push(Block {
                        kind: Kind::Para(Rich::default()),
                        conts: self.conts.clone(),
                        marker: self.marker.take(),
                        center: self.center,
                        src: at..at,
                        map: vec![[0, 0, 0]],
                    });
                }
            }
            TagEnd::FootnoteDefinition => {
                self.flush();
                self.conts.pop();
                if let Some((n, start)) = self.fn_open.pop() {
                    let blocks = self.blocks.split_off(start);
                    self.fn_defs.push((n, blocks));
                }
            }
            TagEnd::Table => {
                if let Some((aligns, mut rows)) = self.table.take() {
                    for row in &mut rows {
                        row.resize_with(aligns.len(), Rich::default);
                    }
                    if !aligns.is_empty() && !rows.is_empty() {
                        self.push(Kind::Table { aligns, rows }, r);
                    }
                    self.part = 0;
                }
            }
            TagEnd::TableCell => {
                if let Some(mut inl) = self.inl.take() {
                    let cut = inl.rich.trim();
                    self.shift_map(cut, inl.rich.text.len());
                    if let Some((_, rows)) = &mut self.table
                        && let Some(row) = rows.last_mut() {
                            row.push(inl.rich);
                        }
                }
            }
            TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link => self.pop_style(),
            TagEnd::Image => {
                if let Some(img) = self.image.take() {
                    self.image(img.url, img.alt, None, r);
                }
            }
            _ => {}
        }
    }

    fn new_id(&mut self) -> u32 {
        self.loose.push(false);
        (self.loose.len() - 1) as u32
    }

    fn add_link(&mut self, url: String) -> u32 {
        self.links.push(url);
        (self.links.len() - 1) as u32
    }

    fn fn_num(&mut self, label: &str) -> u32 {
        let next = self.fn_nums.len() as u32 + 1;
        *self.fn_nums.entry(label.to_string()).or_insert(next)
    }

    fn push_style(&mut self, style: u16, link: u32) {
        self.styles.push((style, link));
        self.restyle();
    }

    fn pop_style(&mut self) {
        self.styles.pop();
        self.restyle();
    }

    fn pop_style_if(&mut self, style: u16) {
        if self.styles.last().is_some_and(|s| s.0 == style) {
            self.pop_style();
        }
    }

    fn restyle(&mut self) {
        self.style = self.styles.iter().fold(0, |a, s| a | s.0);
        self.link = self.styles.iter().rev().find(|s| s.0 == st::LINK).map_or(0, |s| s.1);
    }

    fn ensure_inline(&mut self, at: usize) {
        if self.inl.is_none() {
            self.inl = Some(Inline { leaf: Leaf::Para, rich: Rich::default(), start: at });
        }
    }

    fn text(&mut self, s: &str, extra: u16, r: Range<usize>) {
        self.ensure_inline(r.start);
        let (style, link) = (self.style | extra, self.link);
        let rich = &mut self.inl.as_mut().unwrap().rich;
        if !s.is_empty() {
            self.map.push([self.part, rich.text.len() as u32, r.start as u32]);
        }
        rich.push(s, style, link);
        self.last_end = r.end;
    }

    /// Adjust the current part's map after its text was trimmed.
    fn shift_map(&mut self, cut: usize, len: usize) {
        let p = self.part;
        for m in self.map.iter_mut().filter(|m| m[0] == p) {
            let t = (m[1] as usize).saturating_sub(cut).min(len);
            m[2] += (cut as u32).saturating_sub(m[1]);
            m[1] = t as u32;
        }
    }

    fn push(&mut self, kind: Kind, src: Range<usize>) {
        // Trailing whitespace belongs to the gap, not the block.
        let end = src.start + self.src[src.clone()].trim_end().len();
        let mut map = if matches!(kind, Kind::Rule | Kind::Image { .. }) { Vec::new() } else { std::mem::take(&mut self.map) };
        for m in &mut map {
            m[2] = m[2].saturating_sub(src.start as u32);
        }
        self.blocks.push(Block {
            kind,
            conts: self.conts.clone(),
            marker: self.marker.take(),
            center: self.center,
            src: src.start..end,
            map,
        });
    }

    /// Finish any open inline leaf (explicit or implicit paragraph, heading).
    fn flush(&mut self) {
        let Some(mut inl) = self.inl.take() else { return };
        let src = inl.start..self.last_end.max(inl.start);
        match inl.leaf {
            Leaf::Para => {
                let cut = inl.rich.trim();
                self.shift_map(cut, inl.rich.text.len());
                if !inl.rich.is_blank() {
                    self.push(Kind::Para(inl.rich), src);
                } else {
                    self.map.clear();
                }
            }
            Leaf::Heading(level) => {
                let cut = inl.rich.trim();
                self.shift_map(cut, inl.rich.text.len());
                let slug = self.slug(&inl.rich.text);
                self.push(Kind::Heading { level, text: inl.rich, slug }, src);
            }
            Leaf::Cell => self.inl = Some(inl),
        }
    }

    /// Split the current paragraph so a block-level item can be inserted
    /// mid-text; the rest of the paragraph starts at `next`.
    fn break_para(&mut self, next: usize) {
        if self.inl.as_ref().is_some_and(|i| i.leaf == Leaf::Para) {
            self.flush();
            self.inl = Some(Inline { leaf: Leaf::Para, rich: Rich::default(), start: next });
        }
    }

    fn image(&mut self, url: String, alt: String, width: Option<f64>, r: Range<usize>) {
        let remote = url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:");
        let in_para = self.inl.as_ref().is_none_or(|i| i.leaf == Leaf::Para);
        if remote || !in_para {
            // No network access: show remote images as a compact chip that links to them.
            let label = if alt.trim().is_empty() { "image".to_string() } else { alt };
            let li = if self.link != 0 || self.style & st::LINK != 0 { self.link } else { self.add_link(url) };
            let (style, link) = (self.style, self.link);
            self.style |= st::BADGE | st::LINK;
            self.link = li;
            self.text(&label, 0, r);
            (self.style, self.link) = (style, link);
            return;
        }
        self.break_para(r.end);
        self.push(Kind::Image { url, alt, width }, r);
    }

    fn inline_html(&mut self, t: &str, r: Range<usize>) {
        let lower = t.trim().to_ascii_lowercase();
        let name: String = lower
            .trim_start_matches('<')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '/')
            .collect();
        let style = match name.trim_start_matches('/') {
            "kbd" => st::KBD,
            "sup" => st::SUP,
            "sub" => st::SUB,
            "b" | "strong" => st::BOLD,
            "i" | "em" => st::ITALIC,
            "s" | "del" | "strike" => st::STRIKE,
            "code" => st::CODE,
            "a" => st::LINK,
            "br" => return self.text("\n", 0, r),
            "img" => {
                let url = attr(t, "src").unwrap_or_default();
                let alt = attr(t, "alt").unwrap_or_default();
                let w = attr(t, "width").and_then(|w| w.trim_end_matches("px").parse().ok());
                return self.image(url, alt, w, r);
            }
            _ => return,
        };
        if name.starts_with('/') {
            self.pop_style_if(style);
        } else if style == st::LINK {
            let li = self.add_link(attr(t, "href").unwrap_or_default());
            self.push_style(st::LINK, li);
        } else {
            self.push_style(style, 0);
        }
    }

    /// Best-effort rendering of raw HTML blocks: keep text, images and headings.
    fn html_block(&mut self, h: &str, r: Range<usize>) {
        let t = h.trim();
        if t.starts_with("<!--") && t.ends_with("-->") {
            return;
        }
        let lower = t.to_ascii_lowercase();
        let saved = self.center;
        self.center = lower.contains("align=\"center\"") || lower.contains("align=center") || lower.contains("text-align: center");
        let mut text = String::new();
        let mut heading = None;
        let mut rest = t;
        while let Some(lt) = rest.find('<') {
            text.push_str(&decode_entities(&rest[..lt]));
            let Some(gt) = rest[lt..].find('>') else {
                rest = &rest[lt..];
                break;
            };
            let tag = &rest[lt..lt + gt + 1];
            rest = &rest[lt + gt + 1..];
            if tag.starts_with("<!--") {
                if let Some(end) = rest.find("-->") {
                    rest = &rest[end + 3..];
                }
                continue;
            }
            let name: String = tag[1..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '/')
                .collect::<String>()
                .to_ascii_lowercase();
            match name.as_str() {
                "img" => {
                    self.html_text(&mut text, heading, r.clone());
                    let url = attr(tag, "src").unwrap_or_default();
                    let alt = attr(tag, "alt").unwrap_or_default();
                    let w = attr(tag, "width").and_then(|w| w.trim_end_matches("px").parse().ok());
                    if url.starts_with("http://") || url.starts_with("https://") {
                        let li = self.add_link(url);
                        let mut rich = Rich::default();
                        rich.push(if alt.is_empty() { "image" } else { &alt }, st::BADGE | st::LINK, li);
                        self.push(Kind::Para(rich), r.clone());
                    } else {
                        self.push(Kind::Image { url, alt, width: w }, r.clone());
                    }
                }
                "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                    self.html_text(&mut text, heading, r.clone());
                    heading = Some(name.as_bytes()[1] - b'0');
                }
                "/h1" | "/h2" | "/h3" | "/h4" | "/h5" | "/h6" => {
                    self.html_text(&mut text, heading, r.clone());
                    heading = None;
                }
                "br" | "p" | "/p" | "div" | "/div" | "li" | "/li" | "tr" | "/tr" | "summary" | "/summary" => {
                    text.push('\n')
                }
                _ => {}
            }
        }
        text.push_str(&decode_entities(rest));
        self.html_text(&mut text, heading, r);
        self.center = saved;
    }

    fn html_text(&mut self, text: &mut String, heading: Option<u8>, r: Range<usize>) {
        let lines: Vec<String> = text
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .collect();
        text.clear();
        if lines.is_empty() {
            return;
        }
        let rich = Rich::plain(&lines.join("\n"));
        self.map = vec![[0, 0, r.start as u32]];
        match heading {
            Some(level) => {
                let slug = self.slug(&rich.text);
                self.push(Kind::Heading { level, text: rich, slug }, r);
            }
            None => self.push(Kind::Para(rich), r),
        }
    }

    fn slug(&mut self, text: &str) -> String {
        unique_slug(&mut self.slugs, text)
    }

    fn finish(mut self) -> Doc {
        self.flush();
        if !self.fn_defs.is_empty() {
            self.fn_defs.sort_by_key(|d| d.0);
            let end = self.last_end;
            self.marker = None;
            self.conts.clear();
            self.push(Kind::Rule, end..end);
            for (_, blocks) in self.fn_defs.drain(..) {
                self.blocks.extend(blocks);
            }
        }
        let global = !self.fn_nums.is_empty();
        Doc { blocks: self.blocks, links: self.links, loose: self.loose, global }
    }
}

fn unique_slug(seen: &mut HashMap<String, u32>, text: &str) -> String {
    let mut s = String::new();
    for c in text.trim().chars() {
        if c.is_alphanumeric() || c == '-' || c == '_' {
            s.extend(c.to_lowercase());
        } else if c == ' ' {
            s.push('-');
        }
    }
    let n = seen.entry(s.clone()).or_insert(0);
    *n += 1;
    if *n > 1 { format!("{s}-{}", *n - 1) } else { s }
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(name) {
        let at = from + i;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(|c| c.is_whitespace()) {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else { continue };
        let rest = rest.trim_start();
        let val = match rest.chars().next()? {
            q @ ('"' | '\'') => rest[1..].split(q).next()?,
            _ => rest.split(|c: char| c.is_whitespace() || c == '>').next()?,
        };
        return Some(decode_entities(val));
    }
    None
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&nbsp;", "\u{a0}")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&copy;", "©")
        .replace("&mdash;", "—")
        .replace("&ndash;", "–")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tight_and_loose_lists() {
        let d = parse("- a\n- b\n\n1. x\n\n2. y\n");
        assert_eq!(d.blocks.len(), 4);
        let Cont::List { id: a } = d.blocks[0].conts[0] else { panic!() };
        let Cont::List { id: b } = d.blocks[2].conts[0] else { panic!() };
        assert!(d.is_tight(a));
        assert!(!d.is_tight(b));
        assert_eq!(d.blocks[3].marker, Some(Marker::Number(2)));
    }

    #[test]
    fn tasks_tables_footnotes() {
        let d = parse("- [x] done\n\n| a | b |\n|:-|-:|\n| 1 | 2 |\n\nref[^n]\n\n[^n]: note\n");
        assert_eq!(d.blocks[0].marker, Some(Marker::Task(true)));
        assert!(matches!(&d.blocks[1].kind, Kind::Table { rows, .. } if rows.len() == 2));
        assert!(matches!(d.blocks.last().unwrap().marker, Some(Marker::Footnote(1))));
    }

    #[test]
    fn local_image_splits_paragraph() {
        let d = parse("before ![pic](a.png) after");
        assert_eq!(d.blocks.len(), 3);
        assert!(matches!(&d.blocks[1].kind, Kind::Image { url, .. } if url == "a.png"));
    }

    #[test]
    fn inline_styles() {
        let d = parse("**bold** and `code` [link](http://x)");
        let Kind::Para(r) = &d.blocks[0].kind else { panic!() };
        assert_eq!(r.text, "bold and code link");
        assert_eq!(r.spans.len(), 3);
        assert_eq!(d.links[r.spans[2].link as usize], "http://x");
    }

    #[test]
    fn front_matter_only_at_start() {
        let d = parse("---\ntitle: x\n---\n\npara\n");
        assert!(matches!(&d.blocks[0].kind, Kind::FrontMatter(t) if t == "title: x"));
        assert_eq!(d.blocks[1].src, 18..22);
        // A rule followed by text is not front matter.
        let d = parse("para\n\n---\ntext\n\n---\n");
        assert!(d.blocks.iter().all(|b| !matches!(b.kind, Kind::FrontMatter(_))));
    }
}
