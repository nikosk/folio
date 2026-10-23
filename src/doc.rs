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

    fn trim(&mut self) {
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
}

#[derive(Debug)]
pub struct Block {
    pub kind: Kind,
    pub conts: Vec<Cont>,
    pub marker: Option<Marker>,
    pub center: bool,
    pub src: Range<usize>,
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
            Kind::Para(r) | Kind::Heading { text: r, .. } | Kind::Math(r) => &r.text,
            Kind::Code { text, .. } | Kind::FrontMatter(text) => text,
            Kind::Table { rows, aligns } => &rows[part / aligns.len()][part % aligns.len()].text,
            Kind::Rule | Kind::Image { .. } => "",
        }
    }

    pub fn part_rich(&self, part: usize) -> Option<&Rich> {
        match &self.kind {
            Kind::Para(r) | Kind::Heading { text: r, .. } | Kind::Math(r) => Some(r),
            Kind::Table { rows, aligns } => Some(&rows[part / aligns.len()][part % aligns.len()]),
            _ => None,
        }
    }
}

#[derive(Default, Debug)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub links: Vec<String>,
    /// Indexed by list container id: true if the list is loose.
    pub loose: Vec<bool>,
}

impl Doc {
    pub fn is_tight(&self, list_id: u32) -> bool {
        !self.loose.get(list_id as usize).copied().unwrap_or(false)
    }
}

pub fn parse(src: &str) -> Doc {
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
        | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS
        | Options::ENABLE_MATH
        | Options::ENABLE_GFM;
    let mut b = Builder::default();
    for (ev, r) in Parser::new_ext(src, opts).into_offset_iter() {
        b.event(ev, r);
    }
    b.finish()
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
struct Builder {
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

impl Builder {
    fn event(&mut self, ev: Event, r: Range<usize>) {
        match ev {
            Event::Start(tag) => self.start(tag, r),
            Event::End(tag) => self.end(tag, r),
            Event::Text(t) => {
                if let Some(c) = &mut self.code {
                    c.text.push_str(&t);
                } else if let Some(h) = &mut self.html {
                    h.push_str(&t);
                } else if let Some(img) = &mut self.image {
                    img.alt.push_str(&t);
                } else {
                    self.text(&t, 0, r);
                }
            }
            Event::Code(t) => self.text(&t, st::CODE, r),
            Event::InlineMath(t) => {
                for (s, extra) in tex::render(&t) {
                    self.text(&s, st::MATH | extra, r.clone());
                }
            }
            Event::DisplayMath(t) => {
                self.break_para();
                let mut rich = Rich::default();
                for (s, extra) in tex::render(&t) {
                    rich.push(&s, st::MATH | extra, 0);
                }
                rich.trim();
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
                self.ensure_inline(r.start);
                let style = self.style | st::SUP | st::LINK;
                self.inl.as_mut().unwrap().rich.push(&format!("[{n}]"), style, li);
                self.last_end = r.end;
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
                    self.push(Kind::Para(rich), r.start..r.start);
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
                    self.push(Kind::Para(Rich::default()), r);
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
                }
            }
            TagEnd::TableCell => {
                if let Some(mut inl) = self.inl.take() {
                    inl.rich.trim();
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
        self.inl.as_mut().unwrap().rich.push(s, style, link);
        self.last_end = r.end;
    }

    fn push(&mut self, kind: Kind, src: Range<usize>) {
        self.blocks.push(Block {
            kind,
            conts: self.conts.clone(),
            marker: self.marker.take(),
            center: self.center,
            src,
        });
    }

    /// Finish any open inline leaf (explicit or implicit paragraph, heading).
    fn flush(&mut self) {
        let Some(mut inl) = self.inl.take() else { return };
        let src = inl.start..self.last_end.max(inl.start);
        match inl.leaf {
            Leaf::Para => {
                inl.rich.trim();
                if !inl.rich.is_blank() {
                    self.push(Kind::Para(inl.rich), src);
                }
            }
            Leaf::Heading(level) => {
                inl.rich.trim();
                let slug = self.slug(&inl.rich.text);
                self.push(Kind::Heading { level, text: inl.rich, slug }, src);
            }
            Leaf::Cell => self.inl = Some(inl),
        }
    }

    /// Split the current paragraph so a block-level item can be inserted mid-text.
    fn break_para(&mut self) {
        if self.inl.as_ref().is_some_and(|i| i.leaf == Leaf::Para) {
            let start = self.inl.as_ref().unwrap().start;
            self.flush();
            self.inl = Some(Inline { leaf: Leaf::Para, rich: Rich::default(), start });
        }
    }

    fn image(&mut self, url: String, alt: String, width: Option<f64>, r: Range<usize>) {
        let remote = url.starts_with("http://") || url.starts_with("https://") || url.starts_with("data:");
        let in_para = self.inl.as_ref().is_none_or(|i| i.leaf == Leaf::Para);
        if remote || !in_para {
            // No network access: show remote images as a compact chip that links to them.
            let label = if alt.trim().is_empty() { "image".to_string() } else { alt };
            let li = if self.link != 0 || self.style & st::LINK != 0 { self.link } else { self.add_link(url) };
            self.ensure_inline(r.start);
            let style = self.style | st::BADGE | st::LINK;
            self.inl.as_mut().unwrap().rich.push(&label, style, li);
            return;
        }
        self.break_para();
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
        match heading {
            Some(level) => {
                let slug = self.slug(&rich.text);
                self.push(Kind::Heading { level, text: rich, slug }, r);
            }
            None => self.push(Kind::Para(rich), r),
        }
    }

    fn slug(&mut self, text: &str) -> String {
        let mut s = String::new();
        for c in text.trim().chars() {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                s.extend(c.to_lowercase());
            } else if c == ' ' {
                s.push('-');
            }
        }
        let n = self.slugs.entry(s.clone()).or_insert(0);
        *n += 1;
        if *n > 1 { format!("{s}-{}", *n - 1) } else { s }
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
        Doc { blocks: self.blocks, links: self.links, loose: self.loose }
    }
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
}
