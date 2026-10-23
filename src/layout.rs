//! Document layout engine.
//!
//! Blocks are laid out lazily with Pango: only what is on screen is shaped,
//! everything else carries an estimated height that a background pass
//! refines. Cached layouts far from the viewport are dropped, so memory and
//! open time stay flat regardless of document size.

use crate::doc::{st, Align, Alert, Block, Cont, Doc, Kind, Marker, Rich};
use crate::highlight::{self, Tok};
use crate::paint::Painter;
use crate::theme::{Theme, BODY_FONT, MATH_FONT, MONO_FONT};
use gtk::gdk::{self, RGBA};
use gtk::pango;
use gtk::prelude::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SCALE: f64 = pango::SCALE as f64;
const BASE_PX: f64 = 16.0;
const MAX_COL_EM: f64 = 46.0;

fn pu(px: f64) -> i32 {
    (px * SCALE).round() as i32
}

fn px(u: i32) -> f64 {
    u as f64 / SCALE
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Pos {
    pub block: usize,
    pub part: usize,
    pub idx: usize,
}

pub enum Img {
    /// Not decoded yet; dimensions from the file header when known.
    Pending(Option<(f64, f64)>),
    Ready(gdk::Texture),
    Failed,
}

struct Part {
    l: pango::Layout,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    color: RGBA,
    /// Text offsets where padding joiners were inserted into the layout text.
    ins: Vec<u32>,
}

/// Invisible joiner inserted around inline code; a shape attribute gives it
/// width so the rounded background has room without touching its neighbours.
const PAD_CHAR: char = '\u{2060}';
const PAD_LEN: usize = 3;
const PADDED: u16 = st::CODE | st::KBD | st::BADGE;

/// Map a text offset to a layout offset; `after` places it past joiners
/// inserted at exactly that offset.
fn map_ins(ins: &[u32], i: usize, after: bool) -> usize {
    if i == usize::MAX {
        return i;
    }
    let n = ins.iter().take_while(|&&o| (o as usize) < i || (after && o as usize == i)).count();
    i + PAD_LEN * n
}

impl Part {
    fn layout_idx(&self, i: usize, after: bool) -> usize {
        map_ins(&self.ins, i, after)
    }

    fn text_idx(&self, li: usize) -> usize {
        let mut out = li;
        for (k, &o) in self.ins.iter().enumerate() {
            if li > o as usize + PAD_LEN * k {
                out -= PAD_LEN;
            } else {
                break;
            }
        }
        out
    }
}

enum Deco {
    Text(pango::Layout, f64, f64, RGBA),
    Check(f64, f64, f64, bool),
    /// List bullet drawn as a shape: centre x, centre y, radius, nesting depth.
    Bullet(f64, f64, f64, u8),
}

#[derive(Default)]
struct Laid {
    h: f64,
    parts: Vec<Part>,
    deco: Vec<Deco>,
    table: Option<(Vec<f64>, Vec<f64>)>,
    img: Option<(gdk::Texture, f64, f64, f64, f64)>,
}

struct Fonts {
    em: f64,
}

fn font(family: &str, size: f64) -> pango::FontDescription {
    let mut d = pango::FontDescription::new();
    d.set_family(family);
    d.set_absolute_size(size * SCALE);
    d
}

fn attr(list: &pango::AttrList, a: impl Into<pango::Attribute>, s: u32, e: u32) {
    let mut a = a.into();
    a.set_start_index(s);
    a.set_end_index(e);
    list.insert(a);
}

fn fg(c: &RGBA) -> pango::AttrColor {
    let u = |v: f32| (v.clamp(0.0, 1.0) * 65535.0) as u16;
    pango::AttrColor::new_foreground(u(c.red()), u(c.green()), u(c.blue()))
}

pub fn resolve(base: &Path, url: &str) -> PathBuf {
    let url = url.strip_prefix("file://").unwrap_or(url);
    let url = url.split(['?', '#']).next().unwrap_or(url);
    let decoded = percent_decode(url);
    let p = Path::new(&decoded);
    if p.is_absolute() { p.to_path_buf() } else { base.join(p) }
}

fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_string();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len()
            && let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Read image dimensions from the file header (PNG, GIF, JPEG, WebP) so
/// layout has the right height before the image is decoded.
pub fn image_size(path: &Path) -> Option<(f64, f64)> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut b = vec![0u8; 64 * 1024];
    let n = f.read(&mut b).ok()?;
    let b = &b[..n];
    let be16 = |i: usize| Some(u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as f64);
    let le16 = |i: usize| Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]) as f64);
    if b.starts_with(b"\x89PNG") && n >= 24 {
        let w = u32::from_be_bytes(b[16..20].try_into().ok()?) as f64;
        let h = u32::from_be_bytes(b[20..24].try_into().ok()?) as f64;
        return Some((w, h));
    }
    if b.starts_with(b"GIF8") {
        return Some((le16(6)?, le16(8)?));
    }
    if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        return match b.get(12..16)? {
            b"VP8X" => {
                let w = 1 + (b[24] as u32 | (b[25] as u32) << 8 | (b[26] as u32) << 16);
                let h = 1 + (b[27] as u32 | (b[28] as u32) << 8 | (b[29] as u32) << 16);
                Some((w as f64, h as f64))
            }
            b"VP8 " => Some(((le16(26)? as u32 & 0x3fff) as f64, (le16(28)? as u32 & 0x3fff) as f64)),
            b"VP8L" => {
                let v = u32::from_le_bytes(b.get(21..25)?.try_into().ok()?);
                Some((((v & 0x3fff) + 1) as f64, (((v >> 14) & 0x3fff) + 1) as f64))
            }
            _ => None,
        };
    }
    if b.starts_with(&[0xff, 0xd8]) {
        let mut i = 2;
        while i + 9 < n {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let m = b[i + 1];
            if (0xc0..=0xcf).contains(&m) && m != 0xc4 && m != 0xc8 && m != 0xcc {
                return Some((be16(i + 7)?, be16(i + 5)?));
            }
            i += 2 + be16(i + 2)? as usize;
        }
    }
    None
}

pub struct Engine {
    pub doc: Doc,
    pub base_dir: PathBuf,
    ctx: pango::Context,
    pub theme: Theme,
    zoom: f64,
    fonts: Fonts,
    width: f64,
    cache: Vec<Option<Laid>>,
    n_cached: usize,
    h: Vec<f64>,
    gap: Vec<f64>,
    measured: Vec<bool>,
    unmeasured: usize,
    ys: Vec<f64>,
    ys_dirty: bool,
    bg_next: usize,
    pub images: HashMap<PathBuf, Img>,
    pub img_requests: Vec<PathBuf>,
    pub sel: Option<(Pos, Pos)>,
    pub matches: Vec<(Pos, usize)>,
    pub cur_match: Option<usize>,
}

impl Engine {
    pub fn new(ctx: pango::Context, doc: Doc, base_dir: PathBuf, theme: Theme, zoom: f64) -> Engine {
        let n = doc.blocks.len();
        let mut e = Engine {
            doc,
            base_dir,
            ctx,
            theme,
            zoom,
            fonts: Fonts { em: BASE_PX * zoom },
            width: 800.0,
            cache: (0..n).map(|_| None).collect(),
            n_cached: 0,
            h: vec![0.0; n],
            gap: vec![0.0; n],
            measured: vec![false; n],
            unmeasured: n,
            ys: vec![0.0; n + 1],
            ys_dirty: true,
            bg_next: 0,
            images: HashMap::new(),
            img_requests: Vec::new(),
            sel: None,
            matches: Vec::new(),
            cur_match: None,
        };
        e.compute_gaps();
        for i in 0..n {
            e.h[i] = e.estimate(i);
        }
        e
    }

    pub fn em(&self) -> f64 {
        self.fonts.em
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    fn col_w(&self) -> f64 {
        let side = (0.06 * self.width).max(1.25 * self.em());
        (self.width - 2.0 * side).min(MAX_COL_EM * self.em()).max(10.0 * self.em())
    }

    fn x0(&self) -> f64 {
        ((self.width - self.col_w()) / 2.0).max(0.0)
    }

    fn pad_top(&self) -> f64 {
        2.0 * self.em()
    }

    fn indent(&self, b: &Block) -> f64 {
        b.conts.iter().map(|c| self.cont_w(c)).sum()
    }

    fn cont_w(&self, c: &Cont) -> f64 {
        match c {
            Cont::Quote { .. } => 1.2 * self.em(),
            Cont::List { .. } | Cont::Footnote => 1.85 * self.em(),
        }
    }

    fn block_x(&self, i: usize) -> f64 {
        self.x0() + self.indent(&self.doc.blocks[i])
    }

    fn block_w(&self, i: usize) -> f64 {
        (self.col_w() - self.indent(&self.doc.blocks[i])).max(4.0 * self.em())
    }

    fn compute_gaps(&mut self) {
        let em = self.em();
        for i in 0..self.doc.blocks.len() {
            self.gap[i] = gap_between(&self.doc, i, em);
        }
    }

    fn estimate(&self, i: usize) -> f64 {
        let em = self.em();
        let w = self.block_w(i);
        let lines = |chars: usize, cw: f64| ((chars as f64 * cw) / w).ceil().max(1.0);
        match &self.doc.blocks[i].kind {
            Kind::Para(r) => lines(r.text.len(), 0.5 * em) * 1.65 * em,
            Kind::Heading { level, .. } => {
                let s = heading_scale(*level) * em * 1.3;
                if *level <= 2 { s + 0.45 * em } else { s }
            }
            Kind::Code { text, .. } | Kind::FrontMatter(text) => {
                text.lines().count().max(1) as f64 * 1.55 * 0.875 * em + 1.9 * em
            }
            Kind::Math(_) => 2.6 * em,
            Kind::Rule => 0.6 * em,
            Kind::Table { rows, .. } => rows.len() as f64 * 2.55 * em,
            Kind::Image { .. } => 14.0 * em,
        }
    }

    pub fn len(&self) -> usize {
        self.doc.blocks.len()
    }

    fn update_ys(&mut self) {
        if !self.ys_dirty {
            return;
        }
        self.ys[0] = self.pad_top();
        for i in 0..self.h.len() {
            self.ys[i + 1] = self.ys[i] + self.gap[i] + self.h[i];
        }
        self.ys_dirty = false;
    }

    pub fn total_height(&mut self) -> f64 {
        self.update_ys();
        self.ys[self.len()] + 4.0 * self.em()
    }

    /// Index of the block whose outer box (gap + content) contains `y`.
    fn block_at(&self, y: f64) -> usize {
        let n = self.len();
        if n == 0 {
            return 0;
        }
        self.ys[..n].partition_point(|&v| v <= y).saturating_sub(1)
    }

    /// Scroll anchor: (block, fraction into block) at `top`.
    pub fn anchor(&mut self, top: f64) -> (usize, f64) {
        self.update_ys();
        if self.len() == 0 {
            return (0, 0.0);
        }
        let i = self.block_at(top);
        let bh = self.ys[i + 1] - self.ys[i];
        (i, if bh > 0.0 { ((top - self.ys[i]) / bh).clamp(0.0, 1.0) } else { 0.0 })
    }

    pub fn anchor_top(&mut self, (i, f): (usize, f64)) -> f64 {
        self.update_ys();
        if self.len() == 0 {
            return 0.0;
        }
        let i = i.min(self.len() - 1);
        if f == 0.0 && i == 0 {
            return 0.0;
        }
        self.ys[i] + f * (self.ys[i + 1] - self.ys[i])
    }

    /// Returns true if the content width changed (layouts invalidated).
    pub fn set_width(&mut self, w: f64) -> bool {
        if (w - self.width).abs() < 0.5 {
            return false;
        }
        let old_col = self.col_w();
        self.width = w;
        if (self.col_w() - old_col).abs() < 0.5 {
            return false;
        }
        self.invalidate(true);
        true
    }

    pub fn set_zoom(&mut self, z: f64) {
        let ratio = z / self.zoom;
        self.zoom = z;
        self.fonts = Fonts { em: BASE_PX * z };
        self.compute_gaps();
        for h in &mut self.h {
            *h *= ratio;
        }
        self.invalidate(true);
    }

    pub fn set_theme(&mut self, t: Theme) {
        self.theme = t;
        self.invalidate(false);
    }

    /// Drop cached layouts; with `remeasure`, heights become estimates again
    /// (the previous values are kept as the estimate).
    fn invalidate(&mut self, remeasure: bool) {
        for c in &mut self.cache {
            *c = None;
        }
        self.n_cached = 0;
        if remeasure {
            self.measured.iter_mut().for_each(|m| *m = false);
            self.unmeasured = self.len();
            self.bg_next = 0;
            self.ys_dirty = true;
        }
    }

    fn invalidate_block(&mut self, i: usize) {
        if self.cache[i].take().is_some() {
            self.n_cached -= 1;
        }
        if self.measured[i] {
            self.measured[i] = false;
            self.unmeasured += 1;
            self.bg_next = self.bg_next.min(i);
        }
    }

    pub fn fully_measured(&self) -> bool {
        self.unmeasured == 0
    }

    fn build(&mut self, i: usize) -> Laid {
        let w = self.block_w(i);
        let mut lx = Lx {
            ctx: &self.ctx,
            th: &self.theme,
            em: self.fonts.em,
            base_dir: &self.base_dir,
            images: &mut self.images,
            req: &mut self.img_requests,
        };
        lx.block(&self.doc.blocks[i], w)
    }

    fn set_measured(&mut self, i: usize, h: f64) -> bool {
        let changed = !self.measured[i] || (self.h[i] - h).abs() > 0.01;
        if !self.measured[i] {
            self.measured[i] = true;
            self.unmeasured -= 1;
        }
        if changed {
            self.h[i] = h;
            self.ys_dirty = true;
        }
        changed
    }

    fn ensure_block(&mut self, i: usize) {
        if self.cache[i].is_none() {
            let laid = self.build(i);
            self.set_measured(i, laid.h);
            self.cache[i] = Some(laid);
            self.n_cached += 1;
        }
    }

    /// Lay out every block intersecting [top, bottom).
    pub fn ensure(&mut self, top: f64, bottom: f64) {
        self.update_ys();
        let n = self.len();
        if n == 0 {
            return;
        }
        let first = self.block_at(top);
        let mut i = first;
        let mut y = self.ys[i];
        while i < n && y < bottom {
            self.ensure_block(i);
            y += self.gap[i] + self.h[i];
            i += 1;
        }
        self.update_ys();
        if self.n_cached > 400 {
            let (lo, hi) = (first.saturating_sub(150), i + 150);
            for j in (0..n).filter(|&j| j < lo || j > hi) {
                if self.cache[j].take().is_some() {
                    self.n_cached -= 1;
                }
            }
        }
    }

    /// Measure off-screen blocks for up to `budget`. Returns true when done.
    pub fn measure_some(&mut self, budget: Duration) -> bool {
        let t = Instant::now();
        let n = self.len();
        while self.bg_next < n {
            let i = self.bg_next;
            self.bg_next += 1;
            if self.measured[i] {
                continue;
            }
            let laid = self.build(i);
            self.set_measured(i, laid.h);
            if i.is_multiple_of(8) && t.elapsed() > budget {
                break;
            }
        }
        self.bg_next >= n
    }

    pub fn image_ready(&mut self, path: &Path, img: Img) {
        self.images.insert(path.to_path_buf(), img);
        for i in 0..self.len() {
            if let Kind::Image { url, .. } = &self.doc.blocks[i].kind
                && resolve(&self.base_dir, url) == path {
                    self.invalidate_block(i);
                }
        }
    }

    // ---- painting -------------------------------------------------------

    pub fn paint(&mut self, p: &mut dyn Painter, top: f64, height: f64) {
        self.ensure(top, top + height);
        let n = self.len();
        if n == 0 {
            return;
        }
        let sel = self.sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) }).filter(|(a, b)| a != b);
        let mut i = self.block_at(top);
        while i < n && self.ys[i] < top + height {
            let oy = self.ys[i] + self.gap[i] - top;
            self.paint_block(p, i, oy, sel);
            i += 1;
        }
    }

    fn paint_block(&self, p: &mut dyn Painter, i: usize, oy: f64, sel: Option<(Pos, Pos)>) {
        let Some(laid) = self.cache[i].as_ref() else { return };
        let b = &self.doc.blocks[i];
        let th = &self.theme;
        let em = self.em();
        let x = self.block_x(i);
        let w = self.block_w(i);

        // Container decorations: quote bars run continuously through gaps.
        let mut cx = self.x0();
        for ct in &b.conts {
            if let Cont::Quote { alert, .. } = ct {
                let color = alert.map_or(th.quote_bar, |a| th.alert(a));
                let shared = i > 0 && self.doc.blocks[i - 1].conts.contains(ct);
                let ext = if shared { self.gap[i] } else { 0.0 };
                p.rect(cx, oy - ext, (0.22 * em).round(), laid.h + ext, &color);
            }
            cx += self.cont_w(ct);
        }

        match &b.kind {
            Kind::Code { .. } | Kind::FrontMatter(_) => p.rrect(x, oy, w, laid.h, 6.0, &th.code_bg),
            Kind::Heading { level, .. } if *level <= 2 => p.rect(x, oy + laid.h - 1.0, w, 1.0, &th.border),
            Kind::Rule => p.rect(x, oy + (laid.h / 2.0).round() - 1.0, w, 2.0, &th.border),
            Kind::Table { .. } => {
                if let Some((cols, rows)) = &laid.table {
                    let tw = *cols.last().unwrap();
                    p.rect(x, oy, tw, rows[1], &th.table_head);
                    for r in (2..rows.len() - 1).step_by(2) {
                        p.rect(x, oy + rows[r], tw, rows[r + 1] - rows[r], &th.stripe);
                    }
                    for &ry in &rows[1..rows.len() - 1] {
                        p.rect(x, oy + ry, tw, 1.0, &th.border);
                    }
                    for &cxl in &cols[1..cols.len() - 1] {
                        p.rect(x + cxl, oy, 1.0, laid.h, &th.border);
                    }
                    p.rborder(x, oy, tw + 1.0, laid.h + 1.0, 6.0, 1.0, &th.border);
                }
            }
            _ => {}
        }
        if let Some((t, ix, iy, iw, ih)) = &laid.img {
            p.texture(t, x + ix, oy + iy, *iw, *ih);
            if sel.is_some_and(|(a, z)| a.block < i && i < z.block) {
                p.rect(x + ix, oy + iy, *iw, *ih, &th.sel);
            }
        }

        for (pi, part) in laid.parts.iter().enumerate() {
            let (px0, py0) = (x + part.x, oy + part.y);
            if let Some(r) = b.part_rich(pi) {
                for sp in r.spans.iter().filter(|s| s.style & PADDED != 0) {
                    let size = if sp.style & st::BADGE != 0 { 0.85 } else { 0.875 } * em;
                    let pad = 0.3 * em;
                    let (s, e) = (part.layout_idx(sp.start as usize, true), part.layout_idx(sp.end as usize, false));
                    for (rx, rw, _, _, base) in range_rects(&part.l, s, e, part.w) {
                        let (bx, by, bw, bh) = (px0 + rx - pad, py0 + base - size * 0.98, rw + 2.0 * pad, size * 1.32);
                        if sp.style & st::KBD != 0 {
                            p.rrect(bx, by, bw, bh, 4.0, &th.code_bg);
                            p.rborder(bx, by, bw, bh + 1.0, 4.0, 1.0, &th.border);
                        } else {
                            p.rrect(bx, by, bw, bh, 4.0, &th.inline_code_bg);
                        }
                    }
                }
            }
            let key = Pos { block: i, part: pi, idx: 0 };
            let first = self.matches.partition_point(|(m, _)| (m.block, m.part) < (i, pi));
            for (k, (m, end)) in self.matches[first..].iter().enumerate() {
                if (m.block, m.part) != (key.block, key.part) {
                    break;
                }
                let color = if self.cur_match == Some(first + k) { &th.find_cur } else { &th.find };
                for (rx, rw, y0, y1, _) in range_rects(&part.l, part.layout_idx(m.idx, true), part.layout_idx(*end, false), part.w) {
                    p.rrect(px0 + rx - 1.0, py0 + y0, rw + 2.0, y1 - y0, 3.0, color);
                }
            }
            if let Some((a, z)) = sel {
                let here = (i, pi);
                if (a.block, a.part) <= here && here <= (z.block, z.part) {
                    let s = if (a.block, a.part) == here { a.idx } else { 0 };
                    let e = if (z.block, z.part) == here { z.idx } else { usize::MAX };
                    for (rx, rw, y0, y1, _) in range_rects(&part.l, part.layout_idx(s, false), part.layout_idx(e, true), part.w) {
                        p.rect(px0 + rx, py0 + y0, rw, y1 - y0, &th.sel);
                    }
                }
            }
            p.layout(&part.l, px0, py0, &part.color);
        }

        for d in &laid.deco {
            match d {
                Deco::Text(l, dx, dy, c) => p.layout(l, x + dx, oy + dy, c),
                Deco::Check(dx, dy, s, done) => {
                    let (bx, by) = (x + dx, oy + dy);
                    if *done {
                        p.rrect(bx, by, *s, *s, 4.0, &th.accent);
                        let lw = (s * 0.13).max(1.5);
                        p.line(bx + s * 0.24, by + s * 0.52, bx + s * 0.43, by + s * 0.71, lw, &th.on_accent);
                        p.line(bx + s * 0.43, by + s * 0.71, bx + s * 0.77, by + s * 0.31, lw, &th.on_accent);
                    } else {
                        p.rborder(bx, by, *s, *s, 4.0, 1.5, &th.muted);
                    }
                }
                Deco::Bullet(cx, cy, r, depth) => {
                    let (cx, cy) = (x + cx, oy + cy);
                    match depth % 3 {
                        0 => p.rrect(cx - r, cy - r, 2.0 * r, 2.0 * r, *r, &th.fg),
                        1 => p.rborder(cx - r, cy - r, 2.0 * r, 2.0 * r, *r, (r * 0.45).max(1.0), &th.fg),
                        _ => p.rect(cx - r * 0.85, cy - r * 0.85, 1.7 * r, 1.7 * r, &th.fg),
                    }
                }
            }
        }
    }

    // ---- hit testing & selection ----------------------------------------

    /// Map a point (x in widget coords, y in document coords) to a text position.
    pub fn hit(&mut self, x: f64, y: f64) -> Option<(Pos, bool)> {
        self.update_ys();
        let n = self.len();
        if n == 0 {
            return None;
        }
        let i = self.block_at(y);
        self.ensure_block(i);
        self.update_ys();
        let (bx, by) = (self.block_x(i), self.ys[i] + self.gap[i]);
        let laid = self.cache[i].as_ref()?;
        let (lx, ly) = (x - bx, y - by);
        if laid.parts.is_empty() {
            let idx = if ly > laid.h / 2.0 { 1 } else { 0 };
            return Some((Pos { block: i, part: 0, idx }, false));
        }
        let dist = |p: &Part| {
            let dx = if lx < p.x { p.x - lx } else if lx > p.x + p.w { lx - p.x - p.w } else { 0.0 };
            let dy = if ly < p.y { p.y - ly } else if ly > p.y + p.h { ly - p.y - p.h } else { 0.0 };
            dy * 4.0 + dx
        };
        let (pi, part) = laid
            .parts
            .iter()
            .enumerate()
            .min_by(|a, b| dist(a.1).total_cmp(&dist(b.1)))?;
        let (inside, idx, trailing) = part.l.xy_to_index(pu(lx - part.x), pu(ly - part.y));
        let ltext = part.l.text();
        let mut li = (idx.max(0) as usize).min(ltext.len());
        for _ in 0..trailing {
            li += ltext[li..].chars().next().map_or(0, |c| c.len_utf8());
        }
        let text = self.doc.blocks[i].part_text(pi);
        let idx = part.text_idx(li).min(text.len());
        let inside = inside && dist(part) == 0.0;
        Some((Pos { block: i, part: pi, idx }, inside))
    }

    pub fn link_at(&mut self, x: f64, y: f64) -> Option<String> {
        let (pos, inside) = self.hit(x, y)?;
        if !inside {
            return None;
        }
        let rich = self.doc.blocks[pos.block].part_rich(pos.part)?;
        let sp = rich.span_at(pos.idx, st::LINK)?;
        self.doc.links.get(sp.link as usize).cloned()
    }

    /// Whether the point is over selectable text (for the I-beam cursor).
    pub fn over_text(&mut self, x: f64, y: f64) -> bool {
        self.hit(x, y).is_some_and(|(_, inside)| inside)
    }

    pub fn word_at(&self, p: Pos) -> (Pos, Pos) {
        let b = &self.doc.blocks[p.block];
        if p.part >= b.part_count() {
            return (p, p);
        }
        let t = b.part_text(p.part);
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let at = t[p.idx..].chars().next();
        let class = at.map(word);
        let mut s = p.idx;
        while let Some(c) = t[..s].chars().next_back() {
            if Some(word(c)) != class || c.is_whitespace() != at.is_some_and(char::is_whitespace) {
                break;
            }
            s -= c.len_utf8();
        }
        let mut e = p.idx;
        while let Some(c) = t[e..].chars().next() {
            if Some(word(c)) != class || c.is_whitespace() != at.is_some_and(char::is_whitespace) {
                break;
            }
            e += c.len_utf8();
        }
        (Pos { idx: s, ..p }, Pos { idx: e, ..p })
    }

    pub fn block_range(&self, p: Pos) -> (Pos, Pos) {
        let b = &self.doc.blocks[p.block];
        let n = b.part_count();
        if n == 0 {
            return (Pos { block: p.block, part: 0, idx: 0 }, Pos { block: p.block, part: 0, idx: 1 });
        }
        if matches!(b.kind, Kind::Table { .. }) {
            return (Pos { idx: 0, ..p }, Pos { idx: b.part_text(p.part).len(), ..p });
        }
        (Pos { block: p.block, part: 0, idx: 0 }, Pos { block: p.block, part: n - 1, idx: b.part_text(n - 1).len() })
    }

    pub fn select_all(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }
        let last = &self.doc.blocks[n - 1];
        let lp = last.part_count().saturating_sub(1);
        let end = Pos { block: n - 1, part: lp, idx: last.part_text(lp).len().max(1) };
        self.sel = Some((Pos::default(), end));
    }

    pub fn selected_text(&self) -> String {
        let Some((a, z)) = self.sel.map(|(a, b)| if a <= b { (a, b) } else { (b, a) }) else {
            return String::new();
        };
        let mut out = String::new();
        for bi in a.block..=z.block.min(self.len().saturating_sub(1)) {
            let b = &self.doc.blocks[bi];
            let cols = if let Kind::Table { aligns, .. } = &b.kind { aligns.len() } else { 0 };
            for pi in 0..b.part_count() {
                let here = (bi, pi);
                if here < (a.block, a.part) || here > (z.block, z.part) {
                    continue;
                }
                let t = b.part_text(pi);
                let s = if (a.block, a.part) == here { a.idx.min(t.len()) } else { 0 };
                let e = if (z.block, z.part) == here { z.idx.min(t.len()) } else { t.len() };
                out.push_str(&t[s..e.max(s)]);
                if here != (z.block, z.part) {
                    out.push(if cols > 0 && (pi + 1) % cols != 0 { '\t' } else { '\n' });
                }
            }
        }
        out
    }

    // ---- navigation & find ----------------------------------------------

    /// Document y of a position (lays out its block if needed).
    pub fn pos_y(&mut self, p: Pos) -> f64 {
        if p.block >= self.len() {
            return 0.0;
        }
        self.ensure_block(p.block);
        self.update_ys();
        let by = self.ys[p.block] + self.gap[p.block];
        let laid = self.cache[p.block].as_ref().unwrap();
        match laid.parts.get(p.part) {
            Some(part) => by + part.y + px(part.l.index_to_pos(part.layout_idx(p.idx, false) as i32).y()),
            None => by,
        }
    }

    pub fn anchor_y(&mut self, anchor: &str) -> Option<f64> {
        let i = if let Some(n) = anchor.strip_prefix("fn:") {
            let n: u32 = n.parse().ok()?;
            self.doc.blocks.iter().position(|b| b.marker == Some(Marker::Footnote(n)))?
        } else {
            let want = anchor.to_lowercase();
            self.doc.blocks.iter().position(|b| matches!(&b.kind, Kind::Heading { slug, .. } if *slug == want))?
        };
        Some(self.pos_y(Pos { block: i, part: 0, idx: 0 }) - self.em())
    }

    pub fn find(&mut self, q: &str) -> usize {
        self.matches.clear();
        self.cur_match = None;
        if q.is_empty() {
            return 0;
        }
        let ql = q.to_ascii_lowercase();
        let qb = ql.as_bytes();
        for (bi, b) in self.doc.blocks.iter().enumerate() {
            for pi in 0..b.part_count() {
                let t = b.part_text(pi);
                if q.is_ascii() {
                    let tb = t.as_bytes();
                    let mut k = 0;
                    while k + qb.len() <= tb.len() {
                        if tb[k..k + qb.len()].eq_ignore_ascii_case(qb) {
                            self.matches.push((Pos { block: bi, part: pi, idx: k }, k + qb.len()));
                            k += qb.len();
                        } else {
                            k += 1;
                        }
                    }
                } else {
                    for (k, m) in t.match_indices(q) {
                        self.matches.push((Pos { block: bi, part: pi, idx: k }, k + m.len()));
                    }
                }
            }
        }
        self.matches.len()
    }

    /// Advance to the next/previous match; the first step picks the first
    /// match at or after `top`. Returns the match position.
    pub fn find_step(&mut self, forward: bool, top: f64) -> Option<Pos> {
        let n = self.matches.len();
        if n == 0 {
            return None;
        }
        let next = match self.cur_match {
            Some(c) if forward => (c + 1) % n,
            Some(c) => (c + n - 1) % n,
            None => {
                self.update_ys();
                let b = self.block_at(top + 1.0);
                self.matches.partition_point(|(m, _)| m.block < b) % n
            }
        };
        self.cur_match = Some(next);
        Some(self.matches[next].0)
    }
}

/// Rectangles covering byte range [s, e) of a layout:
/// (x, width, line top, line bottom, baseline), relative to the layout.
fn range_rects(l: &pango::Layout, s: usize, e: usize, w: f64) -> Vec<(f64, f64, f64, f64, f64)> {
    let mut v = Vec::new();
    let mut it = l.iter();
    loop {
        if let Some(line) = it.line_readonly() {
            let ls = line.start_index() as usize;
            let le = ls + line.length() as usize;
            let (ss, ee) = (s.max(ls), e.min(le));
            let continues = e > le && !it.at_last_line();
            if ss < ee || (continues && s <= le) {
                let (y0, y1) = it.line_yrange();
                let base = px(it.baseline());
                let xr = line.x_ranges(ss as i32, ee.max(ss) as i32);
                let mut pairs: Vec<(f64, f64)> = xr.chunks(2).map(|c| (px(c[0]), px(c[1]))).collect();
                if continues {
                    let start = pairs.last().map_or_else(|| px(line.index_to_x(ss as i32, false)), |p| p.1);
                    pairs.push((start, w.max(start)));
                }
                for (a, b) in pairs {
                    if b > a {
                        v.push((a, b - a, px(y0), px(y1), base));
                    }
                }
            }
            if le >= e {
                break;
            }
        }
        if !it.next_line() {
            break;
        }
    }
    v
}

fn heading_scale(level: u8) -> f64 {
    [2.0, 1.55, 1.3, 1.12, 1.0, 0.92][(level.clamp(1, 6) - 1) as usize]
}

fn alert_of(b: &Block) -> Option<Alert> {
    b.conts.iter().rev().find_map(|c| match c {
        Cont::Quote { alert, .. } => Some(*alert),
        _ => None,
    })?
}

fn is_alert_title(b: &Block) -> bool {
    matches!(&b.kind, Kind::Para(r) if r.spans.first().is_some_and(|s| s.style & st::ALERT != 0))
}

fn gap_between(doc: &Doc, i: usize, em: f64) -> f64 {
    if i == 0 {
        return 0.0;
    }
    let (p, c) = (&doc.blocks[i - 1], &doc.blocks[i]);
    if is_alert_title(p) {
        return 0.4 * em;
    }
    let common = c.conts.iter().rev().find_map(|ct| match ct {
        Cont::List { id } if p.conts.contains(ct) => Some(*id),
        _ => None,
    });
    if let Some(id) = common
        && doc.is_tight(id) && !matches!(c.kind, Kind::Heading { .. }) {
            let both_text = matches!(c.kind, Kind::Para(_)) && matches!(p.kind, Kind::Para(_));
            return if both_text { 0.3 * em } else { 0.55 * em };
        }
    let top: f64 = match c.kind {
        Kind::Heading { level, .. } => [1.7, 1.5, 1.35, 1.2, 1.1, 1.1][(level.clamp(1, 6) - 1) as usize],
        _ => 0.9,
    };
    let bottom = match p.kind {
        Kind::Heading { .. } => 0.55,
        _ => 0.9,
    };
    top.max(bottom) * em
}

/// Layout context for building one block.
struct Lx<'a> {
    ctx: &'a pango::Context,
    th: &'a Theme,
    em: f64,
    base_dir: &'a Path,
    images: &'a mut HashMap<PathBuf, Img>,
    req: &'a mut Vec<PathBuf>,
}

impl Lx<'_> {
    fn layout(&self, text: &str, desc: &pango::FontDescription, w: f64, line_h: f64) -> pango::Layout {
        let l = pango::Layout::new(self.ctx);
        l.set_font_description(Some(desc));
        if w > 0.0 {
            l.set_width(pu(w));
        }
        l.set_wrap(pango::WrapMode::WordChar);
        l.set_text(text);
        let attrs = pango::AttrList::new();
        attr(&attrs, pango::AttrInt::new_line_height_absolute(pu(line_h)), 0, u32::MAX);
        l.set_attributes(Some(&attrs));
        l
    }

    fn rich(&self, r: &Rich, desc: &pango::FontDescription, w: f64, line_h: f64, alert: Option<Alert>) -> (pango::Layout, Vec<u32>) {
        let ins: Vec<u32> = r.spans.iter().filter(|s| s.style & PADDED != 0).flat_map(|s| [s.start, s.end]).collect();
        let text = if ins.is_empty() {
            std::borrow::Cow::Borrowed(r.text.as_str())
        } else {
            let mut t = String::with_capacity(r.text.len() + ins.len() * PAD_LEN);
            let mut at = 0;
            for &o in &ins {
                t.push_str(&r.text[at..o as usize]);
                t.push(PAD_CHAR);
                at = o as usize;
            }
            t.push_str(&r.text[at..]);
            std::borrow::Cow::Owned(t)
        };
        let l = self.layout(&text, desc, w, line_h);
        if r.spans.is_empty() {
            return (l, ins);
        }
        let attrs = l.attributes().unwrap_or_default();
        let th = self.th;
        let em = desc.size() as f64 / SCALE;
        let pad = pu(0.3 * em);
        for (k, &o) in ins.iter().enumerate() {
            let at = (o as usize + PAD_LEN * k) as u32;
            let rect = pango::Rectangle::new(0, 0, pad, 0);
            attr(&attrs, pango::AttrShape::new(&rect, &rect), at, at + PAD_LEN as u32);
        }
        for sp in &r.spans {
            let padded = sp.style & PADDED != 0;
            let s = map_ins(&ins, sp.start as usize, padded) as u32;
            let e = map_ins(&ins, sp.end as usize, !padded) as u32;
            let y = sp.style;
            if y & st::BOLD != 0 {
                attr(&attrs, pango::AttrInt::new_weight(pango::Weight::Bold), s, e);
            }
            if y & st::ITALIC != 0 {
                attr(&attrs, pango::AttrInt::new_style(pango::Style::Italic), s, e);
            }
            if y & (st::CODE | st::KBD) != 0 {
                attr(&attrs, pango::AttrString::new_family(MONO_FONT), s, e);
                attr(&attrs, pango::AttrFloat::new_scale(if y & st::KBD != 0 { 0.82 } else { 0.875 }), s, e);
            }
            if y & st::STRIKE != 0 {
                attr(&attrs, pango::AttrInt::new_strikethrough(true), s, e);
                attr(&attrs, fg(&th.muted), s, e);
            }
            if y & st::LINK != 0 {
                attr(&attrs, fg(&th.link), s, e);
            }
            if y & st::BADGE != 0 {
                attr(&attrs, pango::AttrFloat::new_scale(0.85), s, e);
            }
            if y & (st::SUP | st::SUB) != 0 {
                attr(&attrs, pango::AttrFloat::new_scale(0.72), s, e);
                let rise = if y & st::SUP != 0 { 0.36 } else { -0.18 } * em;
                attr(&attrs, pango::AttrInt::new_rise(pu(rise)), s, e);
            }
            if y & st::MATH != 0 {
                attr(&attrs, pango::AttrString::new_family(MATH_FONT), s, e);
                attr(&attrs, pango::AttrFloat::new_scale(1.04), s, e);
            }
            if y & st::ALERT != 0
                && let Some(a) = alert {
                    attr(&attrs, fg(&th.alert(a)), s, e);
                }
        }
        l.set_attributes(Some(&attrs));
        (l, ins)
    }

    fn block(&mut self, b: &Block, w: f64) -> Laid {
        let th = self.th;
        let em = if b.conts.contains(&Cont::Footnote) { self.em * 0.9 } else { self.em };
        let body = font(BODY_FONT, em);
        let line_h = 1.65 * em;
        let align = if b.center { pango::Alignment::Center } else { pango::Alignment::Left };
        let mut laid = Laid::default();
        let mut part = |(l, ins): (pango::Layout, Vec<u32>), x: f64, y: f64, w: f64, color: RGBA| -> f64 {
            let h = l.pixel_size().1 as f64;
            laid.parts.push(Part { l, x, y, w, h, color, ins });
            h
        };
        let mut deco = Vec::new();
        let mut table = None;
        let mut img = None;
        let h = match &b.kind {
            Kind::Para(r) => {
                let l = self.rich(r, &body, w, line_h, alert_of(b));
                l.0.set_alignment(align);
                let color = if b.conts.iter().any(|c| matches!(c, Cont::Quote { alert: None, .. })) {
                    th.muted
                } else {
                    th.fg
                };
                part(l, 0.0, 0.0, w, color)
            }
            Kind::Heading { level, text, .. } => {
                let size = heading_scale(*level) * em;
                let mut desc = font(BODY_FONT, size);
                desc.set_weight(if *level == 1 { pango::Weight::Bold } else { pango::Weight::Semibold });
                let l = self.rich(text, &desc, w, size * 1.3, None);
                l.0.set_alignment(align);
                let color = if *level == 6 { th.muted } else { th.heading };
                let h = part(l, 0.0, 0.0, w, color);
                if *level <= 2 { h + 0.45 * em } else { h }
            }
            Kind::Code { .. } | Kind::FrontMatter(_) => {
                let (lang, text) = match &b.kind {
                    Kind::Code { lang, text } => (lang.as_str(), text),
                    Kind::FrontMatter(text) => ("yaml", text),
                    _ => unreachable!(),
                };
                let size = 0.875 * em;
                let (padx, pady) = (1.0 * em, 0.85 * em);
                let l = self.layout(text, &font(MONO_FONT, size), w - 2.0 * padx, size * 1.55);
                let attrs = l.attributes().unwrap_or_default();
                for (s, e, tok) in highlight::highlight(lang, text) {
                    attr(&attrs, fg(&th.syntax(tok)), s, e);
                    if tok == Tok::Com {
                        attr(&attrs, pango::AttrInt::new_style(pango::Style::Italic), s, e);
                    }
                }
                l.set_attributes(Some(&attrs));
                let color = if matches!(b.kind, Kind::FrontMatter(_)) { th.muted } else { th.fg };
                let h = part((l, Vec::new()), padx, pady, w - 2.0 * padx, color);
                if matches!(b.kind, Kind::Code { .. }) && !lang.is_empty() {
                    let ll = self.layout(lang, &font(BODY_FONT, 0.72 * em), -1.0, 0.72 * em * 1.3);
                    let lw = ll.pixel_size().0 as f64;
                    deco.push(Deco::Text(ll, w - lw - 0.6 * em, 0.35 * em, th.muted));
                }
                h + 2.0 * pady
            }
            Kind::Math(r) => {
                let l = self.rich(r, &font(MATH_FONT, 1.1 * em), w, 1.6 * em, None);
                l.0.set_alignment(pango::Alignment::Center);
                part(l, 0.0, 0.3 * em, w, th.fg) + 0.6 * em
            }
            Kind::Rule => 0.6 * em,
            Kind::Table { aligns, rows } => {
                let cols = aligns.len();
                let (padx, pady) = (0.8 * em, 0.42 * em);
                let mut bold = body.clone();
                bold.set_weight(pango::Weight::Semibold);
                let ls: Vec<(pango::Layout, Vec<u32>)> = rows
                    .iter()
                    .enumerate()
                    .flat_map(|(ri, row)| {
                        let desc = if ri == 0 { &bold } else { &body };
                        row.iter().map(|c| self.rich(c, desc, -1.0, 1.55 * em, None)).collect::<Vec<_>>()
                    })
                    .collect();
                let mut nat = vec![2.0 * padx; cols];
                for (k, (l, _)) in ls.iter().enumerate() {
                    nat[k % cols] = nat[k % cols].max(l.pixel_size().0 as f64 + 2.0 * padx + 1.0);
                }
                let mut colw = nat.clone();
                if nat.iter().sum::<f64>() > w {
                    let mut order: Vec<usize> = (0..cols).collect();
                    order.sort_by(|a, b| nat[*a].total_cmp(&nat[*b]));
                    let mut remaining = w;
                    for (k, &c) in order.iter().enumerate() {
                        colw[c] = nat[c].min(remaining / (cols - k) as f64).max(2.0 * padx + em);
                        remaining -= colw[c];
                    }
                }
                let mut col_x = vec![0.0];
                for cw in &colw {
                    col_x.push(col_x.last().unwrap() + cw);
                }
                let mut row_y = vec![0.0];
                let mut cells = Vec::with_capacity(ls.len());
                for (r, row) in ls.chunks(cols).enumerate() {
                    let mut rh: f64 = 0.0;
                    for (c, (l, ins)) in row.iter().enumerate() {
                        l.set_width(pu(colw[c] - 2.0 * padx));
                        l.set_alignment(match aligns[c] {
                            Align::Left => pango::Alignment::Left,
                            Align::Center => pango::Alignment::Center,
                            Align::Right => pango::Alignment::Right,
                        });
                        rh = rh.max(l.pixel_size().1 as f64);
                        cells.push(((l.clone(), ins.clone()), col_x[c] + padx, row_y[r] + pady, colw[c] - 2.0 * padx));
                    }
                    row_y.push(row_y[r] + rh + 2.0 * pady);
                }
                for (l, x, y, cw) in cells {
                    part(l, x, y, cw, th.fg);
                }
                let h = *row_y.last().unwrap();
                table = Some((col_x, row_y));
                h
            }
            Kind::Image { url, alt, width } => {
                let path = resolve(self.base_dir, url);
                if !self.images.contains_key(&path) {
                    let size = image_size(&path);
                    self.images.insert(path.clone(), Img::Pending(size));
                    self.req.push(path.clone());
                }
                let fit = |nw: f64, nh: f64| {
                    let tw = width.unwrap_or(nw).min(w).max(1.0);
                    (tw, nh * tw / nw.max(1.0))
                };
                match &self.images[&path] {
                    Img::Ready(t) => {
                        let (tw, thh) = fit(t.width() as f64, t.height() as f64);
                        let ix = if b.center { (w - tw) / 2.0 } else { 0.0 };
                        img = Some((t.clone(), ix, 0.0, tw, thh));
                        thh
                    }
                    Img::Pending(Some((nw, nh))) => fit(*nw, *nh).1,
                    Img::Pending(None) => 1.5 * em,
                    Img::Failed => {
                        let label = if alt.is_empty() { url.as_str() } else { alt.as_str() };
                        let mut desc = body.clone();
                        desc.set_style(pango::Style::Italic);
                        let l = self.layout(&format!("🖼  {label}"), &desc, w, line_h);
                        let h = l.pixel_size().1 as f64;
                        deco.push(Deco::Text(l, 0.0, 0.0, th.muted));
                        h
                    }
                }
            }
        };

        if let Some(m) = b.marker {
            let base = laid.parts.first().map_or(em * 1.2, |p| p.y + px(p.l.baseline()));
            let marker_text = |s: &str, size: f64, color: RGBA, gap: f64, deco: &mut Vec<Deco>| {
                let l = self.layout(s, &font(BODY_FONT, size), -1.0, size * 1.3);
                let lw = l.pixel_size().0 as f64;
                let y = base - px(l.baseline());
                deco.push(Deco::Text(l, -lw - gap, y, color));
            };
            match m {
                Marker::Bullet(d) => deco.push(Deco::Bullet(-0.95 * em, base - 0.31 * em, 0.165 * em, d)),
                Marker::Number(n) => marker_text(&format!("{n}."), em, th.fg, 0.45 * em, &mut deco),
                Marker::Footnote(n) => marker_text(&format!("{n}."), em, th.muted, 0.45 * em, &mut deco),
                Marker::Task(done) => {
                    let s = 0.95 * em;
                    deco.push(Deco::Check(-s - 0.55 * em, base - 0.36 * em - s / 2.0, s, done));
                }
            }
        }
        laid.h = h;
        laid.deco = deco;
        laid.table = table;
        laid.img = img;
        laid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_offsets_round_trip() {
        // "a `bc` d": joiners inserted at text offsets 2 and 4.
        let ins = vec![2u32, 4];
        assert_eq!(map_ins(&ins, 1, false), 1);
        assert_eq!(map_ins(&ins, 2, false), 2);
        assert_eq!(map_ins(&ins, 2, true), 2 + PAD_LEN);
        assert_eq!(map_ins(&ins, 4, false), 4 + PAD_LEN);
        assert_eq!(map_ins(&ins, 4, true), 4 + 2 * PAD_LEN);
        assert_eq!(map_ins(&ins, 6, false), 6 + 2 * PAD_LEN);
        let ctx = pangocairo::FontMap::new().create_context();
        let part = Part {
            l: pango::Layout::new(&ctx),
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
            color: RGBA::BLACK,
            ins,
        };
        for i in 0..=6 {
            assert_eq!(part.text_idx(part.layout_idx(i, false)), i);
            assert_eq!(part.text_idx(part.layout_idx(i, true)), i);
        }
    }

    #[test]
    fn copy_selection_as_text() {
        let doc = crate::doc::parse("# Title\n\nSome `code` here.\n\n| a | b |\n|---|---|\n| 1 | 2 |\n");
        let ctx = pangocairo::FontMap::new().create_context();
        let mut e = Engine::new(ctx, doc, PathBuf::new(), Theme::light(), 1.0);
        e.set_width(800.0);
        e.select_all();
        assert_eq!(e.selected_text(), "Title\nSome code here.\na\tb\n1\t2");
        // Partial selection inside the paragraph, across the inline code.
        e.sel = Some((Pos { block: 1, part: 0, idx: 2 }, Pos { block: 1, part: 0, idx: 8 }));
        assert_eq!(e.selected_text(), "me cod");
    }

    #[test]
    fn hit_maps_back_through_padding() {
        let doc = crate::doc::parse("ab `cd` ef");
        let ctx = pangocairo::FontMap::new().create_context();
        let mut e = Engine::new(ctx, doc, PathBuf::new(), Theme::light(), 1.0);
        e.set_width(800.0);
        e.ensure(0.0, 500.0);
        // Walk across the line: text offsets must be monotonic and never
        // land inside the inserted joiners.
        let y = e.pos_y(Pos { block: 0, part: 0, idx: 0 }) + 8.0;
        let mut last = 0;
        for x in (0..400).step_by(2) {
            let (p, _) = e.hit(x as f64, y).unwrap();
            assert!(p.idx >= last && p.idx <= "ab cd ef".len());
            assert!("ab cd ef".is_char_boundary(p.idx));
            last = p.idx;
        }
        assert_eq!(last, "ab cd ef".len());
    }

    #[test]
    fn image_header_sizes() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/gradient.png");
        assert_eq!(image_size(&p), Some((480.0, 200.0)));
    }
}
